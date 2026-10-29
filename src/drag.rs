//! Dragging files around Mu and to and from other apps, and what follows the pointer meanwhile.

use std::{path::PathBuf, rc::Rc};

use block2::RcBlock;
use gpui_kit::component::{ActiveTheme as _, Theme, h_flex};
use gpui_kit::*;
use objc2::{
    AnyThread as _, MainThreadMarker, msg_send,
    rc::Retained,
    runtime::{AnyObject, Bool, ProtocolObject},
};
use objc2_app_kit::{
    NSApplication, NSAttributedStringNSStringDrawing as _, NSBezierPath, NSColor, NSDraggingItem, NSDraggingSession,
    NSEvent, NSEventModifierFlags, NSFont, NSFontAttributeName, NSForegroundColorAttributeName, NSGraphicsContext,
    NSImage, NSShadow,
};
use objc2_foundation::{NSArray, NSAttributedString, NSDictionary, NSPoint, NSRect, NSSize, NSString};

use crate::{
    fs::{self, Done},
    history::Op,
    icons, operations,
};

/// Files being dragged inside Mu.
pub struct DraggedFiles {
    pub paths: Rc<Vec<PathBuf>>,
}

/// Elements that take files dropped on them, dragged inside Mu or in from other apps.
pub trait DropFiles: InteractiveElement + Sized {
    fn on_drop_files(self, on_drop: impl Fn(&[PathBuf], &mut Window, &mut App) + 'static) -> Self {
        let on_drop = Rc::new(on_drop);
        let external = on_drop.clone();
        self.on_drop(move |files: &DraggedFiles, window, cx| on_drop(&files.paths, window, cx))
            .on_drop(move |files: &ExternalPaths, window, cx| external(files.paths(), window, cx))
    }

    /// Highlights the element while files are over it.
    fn drop_target(self) -> Self {
        self.drag_over::<DraggedFiles>(|style, _, _, cx| style.bg(cx.theme().drop_target))
            .drag_over::<ExternalPaths>(|style, _, _, cx| style.bg(cx.theme().drop_target))
    }
}

impl<E: InteractiveElement> DropFiles for E {}

/// The files being dragged, by Mu or in from another app.
pub fn dragged(value: &dyn std::any::Any) -> Option<&[PathBuf]> {
    value
        .downcast_ref::<DraggedFiles>()
        .map(|files| files.paths.as_slice())
        .or_else(|| value.downcast_ref::<ExternalPaths>().map(ExternalPaths::paths))
}

/// Drops `paths` on folder `target`: moves them there, or copies them when they're on another disk
/// or ⌥ is down, like Finder. Dropped on the Trash, they're moved to the Trash. Resolves to what it
/// did, or None when it's cancelled (see `operations::transfer`).
pub fn drop_into(paths: &[PathBuf], target: PathBuf, window: &mut Window, cx: &mut App) -> Task<Option<Done>> {
    // Asked of macOS, as GPUI doesn't hear about the keys during a drag macOS carries.
    let option = NSEvent::modifierFlags_class().contains(NSEventModifierFlags::Option);
    let op = if target == fs::trash_dir() {
        Op::Trash
    } else if option || !paths.first().is_some_and(|first| fs::same_volume(first, &target)) {
        Op::Copy
    } else {
        Op::Move
    };
    operations::transfer(op, paths.to_vec(), target, window, cx)
}

/// Elements that files are dragged from, to drop in Mu or in other apps.
pub trait DragFiles: StatefulInteractiveElement + Sized {
    fn drag_files(self, paths: Rc<Vec<PathBuf>>) -> Self {
        self.on_drag(DraggedFiles { paths }, |files, _, _, cx| {
            hand_to_macos(&files.paths, cx.theme());
            cx.new(|_| EmptyView)
        })
    }
}

impl<E: StatefulInteractiveElement> DragFiles for E {}

/// Has macOS carry the drag of `paths` on from the event being handled, as it does Finder's: it
/// shows them by the pointer and takes them to other apps. What it shows keeps up with the pointer,
/// where a picture Mu drew would trail it by a frame or two. Over the window the drag started in,
/// GPUI still has it as Mu's own.
fn hand_to_macos(paths: &[PathBuf], theme: &Theme) {
    let mtm = MainThreadMarker::new().expect("drags start on the main thread");
    let Some(event) = NSApplication::sharedApplication(mtm).currentEvent() else { return };
    let Some(window) = event.window(mtm) else { return };
    let Some(view) = window.contentView() else { return };
    let chip = chip(paths, theme);
    let size = chip.size();
    // Its top left corner on the pointer, in a view whose coordinates go up from its bottom. macOS
    // pulls a picture that leaves the pointer out up against it.
    let at = view.convertPoint_fromView(event.locationInWindow(), None);
    let frame = NSRect::new(NSPoint::new(at.x, at.y - size.height), size);
    let chip: &AnyObject = &chip;
    let items: Vec<_> = paths
        .iter()
        .enumerate()
        .map(|(ix, path)| {
            let url = fs::file_url(path);
            let item =
                NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), ProtocolObject::from_ref(&*url));
            // The first shows them all.
            unsafe { item.setDraggingFrame_contents(frame, (ix == 0).then_some(chip)) };
            item
        })
        .collect();
    let items = NSArray::from_retained_slice(&items);
    // GPUI's window is where the drag comes from. It lets other apps copy the files, and hands the
    // drag back to GPUI over its windows.
    let _: Retained<NSDraggingSession> =
        unsafe { msg_send![&view, beginDraggingSessionWithItems: &*items, event: &*event, source: &*window] };
}

/// From the pointer to what's dragged, across and down.
const GAP: f64 = 12.;
/// Room to the right of a chip and below it for its shadow.
const SHADOW: f64 = 8.;
const ICON: f64 = 16.;
/// How far each icon in a stack of them sits past the one on top of it.
const STEP: f64 = 6.;
/// Room at the left end of a chip for the badge macOS puts by the pointer with how many things are
/// dragged, when there's more than one.
const BADGE: f64 = 20.;

/// `paths` drawn the way `drag_preview` draws them, for macOS to show from the pointer on: the icons
/// of the first few stacked with the first on top, and the name of the one or how many there are.
fn chip(paths: &[PathBuf], theme: &Theme) -> Retained<NSImage> {
    let label = match paths {
        [one] => fs::display_name(one),
        many => format!("{} items", many.len()),
    };
    // Small text, as `text_sm` has it.
    let font = NSFont::systemFontOfSize(f64::from(f32::from(theme.font_size) * 0.875));
    let color = ns_color(theme.popover_foreground);
    let keys = unsafe { [NSFontAttributeName, NSForegroundColorAttributeName] };
    let attributes = NSDictionary::from_slices(&keys, &[&*font as &AnyObject, &*color]);
    let label = unsafe { NSAttributedString::new_with_attributes(&NSString::from_str(&label), &attributes) };
    let icons: Vec<_> = paths.iter().take(3).filter_map(|path| icons::native(path)).collect();
    let stack = ICON + STEP * icons.len().saturating_sub(1) as f64;
    let text = label.size();
    let lead = if paths.len() > 1 { BADGE } else { 8. };
    let (width, height) = (lead + stack + 8. + text.width.ceil() + 8., 26.);
    let (fill, border, radius) = (ns_color(theme.popover), ns_color(theme.border), f64::from(f32::from(theme.radius)));
    let draw = RcBlock::new(move |_: NSRect| {
        let outline = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
            NSRect::new(NSPoint::new(GAP + 0.5, SHADOW + 0.5), NSSize::new(width - 1., height - 1.)),
            radius,
            radius,
        );
        NSGraphicsContext::saveGraphicsState_class();
        let shadow = NSShadow::new();
        shadow.setShadowOffset(NSSize::new(0., -2.));
        shadow.setShadowBlurRadius(6.);
        shadow.setShadowColor(Some(&NSColor::colorWithWhite_alpha(0., 0.2)));
        shadow.set();
        fill.setFill();
        outline.fill();
        NSGraphicsContext::restoreGraphicsState_class();
        border.setStroke();
        outline.stroke();
        let y = SHADOW + (height - ICON) / 2.;
        for (ix, icon) in icons.iter().enumerate().rev() {
            let x = GAP + lead + STEP * ix as f64;
            icon.drawInRect(NSRect::new(NSPoint::new(x, y), NSSize::new(ICON, ICON)));
        }
        label.drawAtPoint(NSPoint::new(GAP + lead + stack + 8., SHADOW + (height - text.height) / 2.));
        Bool::YES
    });
    NSImage::imageWithSize_flipped_drawingHandler(
        NSSize::new(GAP + width + SHADOW, GAP + height + SHADOW),
        false,
        &draw,
    )
}

fn ns_color(color: Hsla) -> Retained<NSColor> {
    let Rgba { r, g, b, a } = color.into();
    NSColor::colorWithSRGBRed_green_blue_alpha(r.into(), g.into(), b.into(), a.into())
}

/// What follows the pointer while something's dragged inside Mu: `icon` and `label`, just past the
/// pointer. GPUI keeps the pointer where it was in what's dragged, which for a wide row can be far
/// from a preview this small.
pub fn drag_preview(grabbed_at: Point<Pixels>, icon: impl IntoElement, label: SharedString, cx: &App) -> Div {
    let theme = cx.theme();
    let preview = h_flex()
        .h(px(26.))
        .px_2()
        .gap_2()
        .rounded(theme.radius)
        .bg(theme.popover)
        .border_1()
        .border_color(theme.border)
        .shadow_md()
        .text_sm()
        .text_color(theme.popover_foreground)
        .child(icon)
        .child(label);
    div().pl(grabbed_at.x + px(GAP as f32)).pt(grabbed_at.y + px(GAP as f32)).child(preview)
}
