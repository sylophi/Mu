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
    pictures: HashMap<PathBuf, (Option<SystemTime>, Option<Arc<RenderImage>>)>,
}

impl Cache {
    pub fn new(size: Pixels) -> Self {
        Self { size, framed: false, pictures: HashMap::new() }
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
        if let Some((made, picture)) = self.pictures.get(path)
            && *made == modified
        {
            return picture.clone();
        }
        let path = path.to_path_buf();
        let picture = make(&path, self.size, window.scale_factor(), self.framed);
        if let Some((_, Some(stale))) = self.pictures.insert(path.clone(), (modified, None)) {
            cx.drop_image(stale, Some(window));
        }
        cx.spawn(async move |view, cx| {
            let picture = picture.await;
            view.update(cx, |view, cx| match cache(view).pictures.get_mut(&path) {
                Some(slot) if slot.0 == modified => {
                    slot.1 = picture;
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

    /// Lets go of the pictures, which the GPU holds on to until it's told.
    pub fn clear(&mut self, mut window: Option<&mut Window>, cx: &mut App) {
        for picture in self.pictures.drain().filter_map(|(_, (_, picture))| picture) {
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
