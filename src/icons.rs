//! Icons say what something is, and each kind of thing gets its icons from one place, so a thing
//! looks the same wherever it shows:
//!
//! - Files, folders, apps and disks: the icons macOS draws for them, the ones Finder shows.
//! - What Mu does, and its own tools: line icons (`IconName`) in the color of the text by them.
//! - Mu itself: its logo, the μ of its app icon.
//!
//! Where there's room, the grid and the preview show what a file holds instead of its icon.

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    hash::{DefaultHasher, Hash as _, Hasher as _},
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex},
};

use gpui_kit::component::{ActiveTheme as _, Theme};
use gpui_kit::{prelude::FluentBuilder as _, *};
use image::Frame;
use objc2::rc::{Retained, autoreleasepool};
use objc2_app_kit::{NSGraphicsContext, NSImage, NSImageNameTrashEmpty, NSWorkspace};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSString, NSURLEffectiveIconKey};
use objc2_uniform_type_identifiers::{UTType, UTTypeData, UTTypeFolder, UTTypeUnixExecutable};

use crate::{
    fs::{self, Category, Entry},
    thumbnails,
};

/// What an icon is drawn for.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Of {
    Folder,
    /// A file without an extension.
    Document,
    /// A program without an extension.
    Program,
    /// A file with this extension, lowercased, or a package like an app.
    Extension(String),
    /// Just what's at this path.
    Path(PathBuf),
    /// What macOS draws for the type with this identifier, like the badge on an alias.
    Type(&'static str),
}

/// The icon for what's at `path`, `size` big. It's a folder, or a package like an app, when
/// `is_dir`.
pub fn item(path: &Path, is_dir: bool, size: Pixels) -> Img {
    let generic = generic(path, is_dir, false);
    // Files with an extension share their kind's icon, as drawing each one's own takes a while.
    let own = !matches!(generic, Of::Extension(_));
    icon(own.then(|| Of::Path(path.to_path_buf())), generic, size)
}

/// The icon for `entry`, `size` big, with the badge Finder puts on it.
pub fn entry(entry: &Entry, size: Pixels) -> Div {
    let (own, generic) = icons_of(entry);
    badged(icon(own, generic, size), entry, size)
}

/// Starts drawing the icon `entry` shows, `size` big at `scale`, before it shows, like in a folder
/// that's about to open.
pub fn prefetch(entry: &Entry, size: Pixels, scale: f32, cx: &mut App) {
    let (own, generic) = icons_of(entry);
    get((own.unwrap_or(generic), side(size, scale)), None, cx);
}

/// The icon of its own `entry` shows, if macOS draws it one, and the one for its kind.
fn icons_of(entry: &Entry) -> (Option<Of>, Of) {
    let (path, program) = (entry.target(), entry.category == Category::Executable);
    (draws_own(entry).then(|| Of::Path(path.to_path_buf())), generic(path, entry.folder, program))
}

/// `picture` of `entry`, with the badge for a link or a locked item on its bottom left corner,
/// sized the way macOS sizes it for an icon `size` square.
pub fn badged(picture: impl IntoElement, entry: &Entry, size: Pixels) -> Div {
    let badge = match (entry.is_link, entry.locked) {
        (true, _) => Some("com.apple.icon-decoration.system.alias"),
        (_, true) => Some("com.apple.icon-decoration.badge.locked"),
        _ => None,
    };
    div()
        .relative()
        .flex_none()
        .child(picture)
        .when_some(badge, |el, badge| el.child(of_type(badge, (size * 0.3).max(px(6.))).absolute().left_0().bottom_0()))
}

/// The icon macOS draws for the type `identifier`, like "com.apple.file-server", `size` big.
pub fn of_type(identifier: &'static str, size: Pixels) -> Img {
    icon(None, Of::Type(identifier), size)
}

/// Whether macOS draws `entry` an icon of its own, not just one for its kind: a folder, a package
/// like an app, or a file with a custom icon. It shows instead of a picture of what's in it.
pub fn draws_own(entry: &Entry) -> bool {
    entry.folder || entry.package || entry.custom_icon
}

/// The icon all things like what's at `path` share, which shows until its own is drawn.
fn generic(path: &Path, is_dir: bool, program: bool) -> Of {
    match path.file_name().and_then(|name| fs::extension(&name.to_string_lossy())) {
        _ if is_dir => Of::Folder,
        Some(extension) => Of::Extension(extension),
        None if program => Of::Program,
        None => Of::Document,
    }
}

/// The icon for `generic`, `size` big, until the one for `own` is drawn, if there is an `own`.
fn icon(own: Option<Of>, generic: Of, size: Pixels) -> Img {
    let draw = move |window: &mut Window, cx: &mut App| {
        let (side, view) = (side(size, window.scale_factor()), window.current_view());
        let mut icon = |of: &Of| get((of.clone(), side), Some(view), cx);
        own.as_ref().and_then(&mut icon).or_else(|| icon(&generic)).map(Ok)
    };
    img(ImageSource::Custom(Arc::new(draw))).size(size).flex_none()
}

/// An icon's side in device pixels, for `size` at `scale`.
fn side(size: Pixels, scale: f32) -> u32 {
    (f32::from(size) * scale).round() as u32
}

/// Mu's logo, `size` big: the μ of its app icon, in the theme's colors.
pub fn logo(size: Pixels, cx: &App) -> Div {
    let layers = crate::assets::LOGO
        .into_iter()
        .zip(logo_colors(cx.theme()))
        .map(|((path, _), color)| svg().path(path).absolute().size(size).text_color(color));
    div().relative().flex_none().size(size).children(layers)
}

/// The colors of the logo's strokes, from the outside in.
pub fn logo_colors(theme: &Theme) -> [Hsla; 4] {
    [theme.cyan, theme.green, theme.yellow, theme.red]
}

/// macOS's icons drawn so far, by what they're for and their side in device pixels, kept for as
/// long as Mu runs.
#[derive(Default)]
struct Icons {
    drawn: HashMap<Of, BTreeMap<u32, Option<Arc<RenderImage>>>>,
    /// The icons being drawn, and the views waiting for them.
    drawing: HashMap<(Of, u32), HashSet<EntityId>>,
}

impl Global for Icons {}

/// The icon for `key`. Until it's drawn that size, it's the one drawn nearest that size, so a thing
/// already showing somewhere doesn't show as something else first. None when it's not drawn at all.
/// `view` is notified once it's drawn.
fn get(key: (Of, u32), view: Option<EntityId>, cx: &mut App) -> Option<Arc<RenderImage>> {
    let icons = cx.default_global::<Icons>();
    let (of, side) = &key;
    let sides = icons.drawn.get(of);
    if let Some(icon) = sides.and_then(|sides| sides.get(side)) {
        return icon.clone();
    }
    // A bigger one shrinks better than a smaller one grows.
    let nearest =
        sides.and_then(|sides| sides.range(side..).chain(sides.range(..side).rev()).find_map(|(_, icon)| icon.clone()));
    start_drawing(key, view, cx);
    nearest
}

/// Draws the icons for what's at `paths` again, as they may have changed. Each shows the one it
/// had until then.
pub fn refresh(paths: &[PathBuf], cx: &mut App) {
    let Some(icons) = cx.try_global::<Icons>() else { return };
    let paths: HashSet<&PathBuf> = paths.iter().collect();
    let stale: Vec<_> = icons
        .drawn
        .iter()
        .filter(|(of, _)| matches!(of, Of::Path(path) if paths.contains(path)))
        .flat_map(|(of, sides)| sides.keys().map(|side| (of.clone(), *side)))
        .collect();
    for key in stale {
        start_drawing(key, None, cx);
    }
}

/// Draws `key`'s icon, unless it's being drawn already, and notifies `view` once it is.
fn start_drawing(key: (Of, u32), view: Option<EntityId>, cx: &mut App) {
    let drawing = &mut cx.default_global::<Icons>().drawing;
    let first = !drawing.contains_key(&key);
    drawing.entry(key.clone()).or_default().extend(view);
    if first {
        draw_later(key, cx);
    }
}

/// Draws `key`'s icon in the background, then shows it.
fn draw_later(key: (Of, u32), cx: &mut App) {
    let (of, side) = key.clone();
    let icon = cx.background_spawn(async move { autoreleasepool(|_| draw(&of, side)) });
    cx.spawn(async move |cx| {
        let icon = icon.await;
        cx.update(|cx| {
            let icons = cx.global_mut::<Icons>();
            let waiting = icons.drawing.remove(&key).unwrap_or_default();
            let (of, side) = key;
            let before = icons.drawn.entry(of).or_default().insert(side, icon.clone());
            for view in waiting {
                cx.notify(view);
            }
            // Wherever the old one shows.
            let id = |icon: &Option<Arc<RenderImage>>| icon.as_ref().map(Arc::as_ptr);
            if before.is_some_and(|before| id(&before) != id(&icon)) {
                cx.refresh_windows();
            }
        })
    })
    .detach();
}

/// The icon macOS has for what's at `path`, for drawing with AppKit.
pub fn native(path: &Path) -> Option<Retained<NSImage>> {
    ns_image(&Of::Path(path.to_path_buf()))
}

/// The icon macOS draws for `of`, `side` pixels square.
fn draw(of: &Of, side: u32) -> Option<Arc<RenderImage>> {
    let image = ns_image(of)?;
    // Drawn in a context its size, an icon is drawn the way it's made for that size.
    let pixels = thumbnails::bitmap(side as usize, side as usize, |context| {
        NSGraphicsContext::setCurrentContext(Some(&NSGraphicsContext::graphicsContextWithCGContext_flipped(
            context, false,
        )));
        image.drawInRect(CGRect::new(CGPoint::ZERO, CGSize::new(side as f64, side as f64)));
        NSGraphicsContext::setCurrentContext(None);
    })?;
    // Most folders look the same, and so do many kinds of file. They share one picture, so the GPU
    // keeps one copy.
    static DRAWN: LazyLock<Mutex<HashMap<u64, Arc<RenderImage>>>> = LazyLock::new(Default::default);
    let mut hasher = DefaultHasher::new();
    pixels.as_raw().hash(&mut hasher);
    let mut drawn = DRAWN.lock().unwrap();
    Some(drawn.entry(hasher.finish()).or_insert_with(|| Arc::new(RenderImage::new([Frame::new(pixels)]))).clone())
}

/// The icon macOS has for `of`.
fn ns_image(of: &Of) -> Option<Retained<NSImage>> {
    let workspace = NSWorkspace::sharedWorkspace();
    Some(match of {
        Of::Folder => workspace.iconForContentType(unsafe { UTTypeFolder }),
        Of::Document => workspace.iconForContentType(unsafe { UTTypeData }),
        Of::Program => workspace.iconForContentType(unsafe { UTTypeUnixExecutable }),
        Of::Extension(extension) => {
            let kind = UTType::typeWithFilenameExtension(&NSString::from_str(extension));
            workspace.iconForContentType(kind.as_deref().unwrap_or(unsafe { UTTypeData }))
        }
        // On disk, it's just a hidden folder.
        Of::Path(path) if *path == fs::trash_dir() => NSImage::imageNamed(unsafe { NSImageNameTrashEmpty })?,
        Of::Type(identifier) => {
            workspace.iconForContentType(&*UTType::typeWithIdentifier(&NSString::from_str(identifier))?)
        }
        // A link shows what it leads to. Its badge goes on separately, so it can go on pictures too.
        Of::Path(path) => {
            let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
            // Asked through a URL, as `iconForFile` keeps giving a path the icon it first had.
            let url = crate::fs::file_url(&path);
            let mut icon = None;
            unsafe { url.getResourceValue_forKey_error(&mut icon, NSURLEffectiveIconKey) }.ok()?;
            icon?.downcast::<NSImage>().ok()?
        }
    })
}
