//! Pictures of what files hold, made by Quick Look the way it makes them for Finder, like an
//! image, a PDF's first page or a frame of a video.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use block2::RcBlock;
use gpui_kit::{App, Context, Pixels, RenderImage, Window};
use image::{Frame, RgbaImage};
use objc2::AnyThread as _;
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGColorSpace, CGContext, CGImage, CGImageAlphaInfo, CGImageByteOrderInfo,
};
use objc2_foundation::NSError;
use objc2_quick_look_thumbnailing::{
    QLThumbnailGenerationRequest, QLThumbnailGenerationRequestRepresentationTypes as Types, QLThumbnailGenerator,
    QLThumbnailRepresentation,
};

use crate::{fs::Entry, icons};
/// The pictures a view has asked for at one size. Each is made once, and again when its file changes.
pub struct Cache {
    size: Pixels,
    framed: bool,
    pictures: HashMap<PathBuf, Picture>,
    /// How many pictures `trim` keeps.
    keep: usize,
    /// How many times it's been asked for a picture, which dates when each was last asked for.
    asked: u64,
}

/// `trim` keeps as many pictures as tile a square this many points across: hundreds of the grid's,
/// a few of the preview's.
const ROOM: f32 = 1024.;

struct Picture {
    /// When its file was last modified as of making it.
    made: Option<SystemTime>,
    image: Option<Arc<RenderImage>>,
    asked: u64,
}

impl Cache {
    pub fn new(size: Pixels) -> Self {
        let keep = (ROOM / f32::from(size)).powi(2) as usize;
        Self { size, framed: false, pictures: HashMap::new(), keep, asked: 0 }
    }

    /// Pictures framed the way Finder's icon view shows them: a document as a page, a photo with
    /// rounded corners.
    pub fn framed(size: Pixels) -> Self {
        Self { framed: true, ..Self::new(size) }
    }

    /// The picture of what `entry` holds, or None until Quick Look has made it. Then the view is
    /// notified, finding this cache again with `cache`. What macOS draws an icon of its own for,
    /// like a folder or an app, has none, as its icon shows instead.
    pub fn get<V: 'static>(
        &mut self,
        entry: &Entry,
        window: &mut Window,
        cx: &mut Context<V>,
        cache: fn(&mut V) -> &mut Cache,
    ) -> Option<Arc<RenderImage>> {
        if icons::draws_own(entry) {
            return None;
        }
        // An alias shows what it leads to.
        let (path, modified) = (entry.target(), entry.modified);
        self.asked += 1;
        if let Some(picture) = self.pictures.get_mut(path)
            && picture.made == modified
        {
            picture.asked = self.asked;
            return picture.image.clone();
        }
        let path = path.to_path_buf();
        let picture = make(&path, self.size, window.scale_factor(), self.framed);
        let slot = Picture { made: modified, image: None, asked: self.asked };
        if let Some(Picture { image: Some(stale), .. }) = self.pictures.insert(path.clone(), slot) {
            cx.drop_image(stale, Some(window));
        }
        cx.spawn(async move |view, cx| {
            let picture = picture.await;
            view.update(cx, |view, cx| match cache(view).pictures.get_mut(&path) {
                Some(slot) if slot.made == modified => {
                    slot.image = picture;
                    cx.notify();
                }
                _ => {
                    if let Some(picture) = picture {
                        cx.drop_image(picture, None);
                    }
                }
            })
            .ok();
        })
        .detach();
        None
    }

    /// Lets go of all but the pictures asked for last, as the view moves on from the files it
    /// showed, so going back to them shows them right away.
    pub fn trim(&mut self, mut window: Option<&mut Window>, cx: &mut App) {
        let over = self.pictures.len().saturating_sub(self.keep);
        if over == 0 {
            return;
        }
        let mut asked: Vec<u64> = self.pictures.values().map(|picture| picture.asked).collect();
        // All but the `over` asked for longest ago.
        let kept = *asked.select_nth_unstable(over).1;
        self.pictures.retain(|_, picture| {
            if picture.asked < kept
                && let Some(image) = picture.image.take()
            {
                cx.drop_image(image, window.as_deref_mut());
            }
            picture.asked >= kept
        });
    }

    /// Lets go of the pictures, which the GPU holds on to until it's told.
    pub fn clear(&mut self, mut window: Option<&mut Window>, cx: &mut App) {
        for picture in self.pictures.drain().filter_map(|(_, picture)| picture.image) {
            cx.drop_image(picture, window.as_deref_mut());
        }
    }
}

/// Asks Quick Look for a picture of what the file at `path` holds that fits in `size` points at
/// `scale`, `framed` like an icon. Resolves to None when there's no such picture.
fn make(path: &Path, size: Pixels, scale: f32, framed: bool) -> impl Future<Output = Option<Arc<RenderImage>>> + use<> {
    let (sender, receiver) = smol::channel::bounded(1);
    let url = crate::fs::file_url(path);
    let side = f32::from(size) as f64;
    let request = unsafe {
        QLThumbnailGenerationRequest::initWithFileAtURL_size_scale_representationTypes(
            QLThumbnailGenerationRequest::alloc(),
            &url,
            CGSize::new(side, side),
            scale as f64,
            Types::Thumbnail,
        )
    };
    unsafe { request.setIconMode(framed) };
    let done = RcBlock::new(move |representation: *mut QLThumbnailRepresentation, _: *mut NSError| {
        let image =
            unsafe { representation.as_ref() }.and_then(|representation| pixels(&*unsafe { representation.CGImage() }));
        sender.try_send(image.map(|image| Arc::new(RenderImage::new([Frame::new(image)])))).ok();
    });
    unsafe {
        QLThumbnailGenerator::sharedGenerator().generateBestRepresentationForRequest_completionHandler(&request, &done)
    };
    async move { receiver.recv().await.ok().flatten() }
}

/// `image`'s pixels, as `bitmap` has them.
fn pixels(image: &CGImage) -> Option<RgbaImage> {
    let (width, height) = (CGImage::width(Some(image)), CGImage::height(Some(image)));
    let bounds = CGRect::new(CGPoint::ZERO, CGSize::new(width as f64, height as f64));
    bitmap(width, height, |context| CGContext::draw_image(Some(context), bounds, Some(image)))
}

/// What `draw` draws in a `width` by `height` context, in the order GPUI takes pixels: blue,
/// green, red, then alpha, not premultiplied.
pub fn bitmap(width: usize, height: usize, draw: impl FnOnce(&CGContext)) -> Option<RgbaImage> {
    let mut pixels = vec![0u8; width * height * 4];
    let space = CGColorSpace::new_device_rgb()?;
    // Bitmap contexts only draw premultiplied, so this is undone below.
    let info = CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0;
    let context =
        unsafe { CGBitmapContextCreate(pixels.as_mut_ptr().cast(), width, height, 8, width * 4, Some(&space), info) }?;
    draw(&context);
    drop(context);
    for [blue, green, red, alpha] in pixels.as_chunks_mut::<4>().0 {
        let alpha = *alpha as u16;
        if alpha > 0 && alpha < 255 {
            for channel in [blue, green, red] {
                *channel = (*channel as u16 * 255 / alpha) as u8;
            }
        }
    }
    RgbaImage::from_raw(width as u32, height as u32, pixels)
}
