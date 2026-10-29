//! Walks you through System Settings the way Codex does: it opens the page a permission is on and
//! floats a panel on the window with Mu to drag into the list there, then moves on to the next
//! page as soon as that one's allowed.

use std::{cell::Cell, time::Duration};

use gpui_kit::*;
use objc2::{
    AnyThread as _, DefinedClass as _, MainThreadMarker, MainThreadOnly, Message as _, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, NSObjectProtocol, ProtocolObject},
};
use objc2_app_kit::{
    NSAutoresizingMaskOptions, NSBackingStoreType, NSBox, NSBoxType, NSColor, NSDragOperation, NSDraggingContext,
    NSDraggingItem, NSDraggingSession, NSDraggingSource, NSEvent, NSFont, NSFontWeight, NSFontWeightBold,
    NSFontWeightRegular, NSFontWeightSemibold, NSGlassEffectView, NSImage, NSImageSymbolConfiguration, NSImageView,
    NSLineBreakMode, NSPanel, NSResponder, NSRunningApplication, NSScreen, NSStatusWindowLevel, NSTextField,
    NSTitlePosition, NSView, NSWindowCollectionBehavior, NSWindowStyleMask, NSWorkspace,
};
use objc2_core_foundation::{CFArray, CFDictionary, CGRect};
use objc2_core_graphics::{CGRectMakeWithDictionaryRepresentation, CGWindowListCopyWindowInfo, CGWindowListOption};
use objc2_foundation::{
    NSArray, NSBundle, NSDictionary, NSMouseInRect, NSNumber, NSObject, NSPoint, NSRect, NSSize, NSString, NSURL,
    ns_string,
};

use super::{Permission, Permissions, Watch};

const WIDTH: f64 = 460.;
const HEIGHT: f64 = 100.;
/// Without Mu to drag, for Local Network, whose list Mu adds itself to.
const SHORT_HEIGHT: f64 = 48.;
/// From the window's right edge to the middle of the list, which sits right of the sidebar.
const LIST_CENTER: f64 = 254.;
const ARROW_SIZE: f64 = 17.;

pub(super) struct Guide {
    steps: Vec<Permission>,
    panel: Panel,
    _watch: Watch,
    _track: Task<()>,
}

impl Guide {
    /// Starts on the first of `steps`, if there are any.
    pub(super) fn new(steps: Vec<Permission>, watch: Watch, cx: &mut Context<Permissions>) -> Option<Self> {
        let first = *steps.first()?;
        let track = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(100)).await;
                let tracking = this.update(cx, |this, _| {
                    let tracking = this.guide.as_mut().is_some_and(|guide| guide.panel.track());
                    if !tracking {
                        this.guide = None;
                    }
                    tracking
                });
                if !matches!(tracking, Ok(true)) {
                    break;
                }
            }
        });
        let guide = Self { steps, panel: Panel::new(), _watch: watch, _track: track };
        guide.show(first, cx);
        Some(guide)
    }

    /// Drops the steps that are allowed now and goes on to the next. False once none are left.
    pub(super) fn advance(&mut self, granted: impl Fn(Permission) -> bool, cx: &mut App) -> bool {
        let current = self.steps.first().copied();
        self.steps.retain(|&permission| !granted(permission));
        let Some(&next) = self.steps.first() else { return false };
        if Some(next) != current {
            self.show(next, cx);
        }
        true
    }

    fn show(&self, permission: Permission, cx: &mut App) {
        cx.open_url(permission.settings_url());
        self.panel.show(permission);
    }
}

struct Panel {
    window: Retained<NSPanel>,
    view: Retained<GuideView>,
    /// Whether System Settings has come up, so its quitting can end the guide.
    opened: bool,
}

impl Panel {
    fn new() -> Self {
        let mtm = mtm();
        let view = GuideView::new(mtm);
        let glass = NSGlassEffectView::new(mtm);
        glass.setCornerRadius(18.);
        glass.setContentView(Some(&view));
        let window = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            rect(0., 0., WIDTH, HEIGHT),
            NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
            NSBackingStoreType::Buffered,
            false,
        );
        unsafe { window.setReleasedWhenClosed(false) };
        window.setOpaque(false);
        window.setBackgroundColor(Some(&NSColor::clearColor()));
        window.setHasShadow(true);
        window.setLevel(NSStatusWindowLevel);
        window.setHidesOnDeactivate(false);
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::FullScreenAuxiliary,
        );
        window.setContentView(Some(&glass));
        Self { window, view, opened: false }
    }

    fn show(&self, permission: Permission) {
        let views = self.view.ivars();
        let drag = permission != Permission::LocalNetwork;
        let (arrow, text) = if drag {
            ("arrow.up", format!("Drag Mu to the list above to allow {}", permission.name()))
        } else {
            ("arrow.down", "Scroll down to Local Network, then turn on Mu".into())
        };
        views.arrow.setImage(Some(&symbol_image(&NSString::from_str(arrow), ARROW_SIZE, unsafe { NSFontWeightBold })));
        views.title.setStringValue(&NSString::from_str(&text));
        views.row.setHidden(!drag);
        let mut frame = self.window.frame();
        frame.size.height = if drag { HEIGHT } else { SHORT_HEIGHT };
        self.window.setFrame_display(frame, true);
    }

    /// Keeps the panel on System Settings' window while that's in front. False once you've closed
    /// the panel or quit System Settings.
    fn track(&mut self) -> bool {
        if self.view.ivars().closed.get() {
            return false;
        }
        match settings_window() {
            SettingsWindow::Closed => return !self.opened,
            SettingsWindow::Behind => self.window.orderOut(None),
            SettingsWindow::Front(frame) => {
                self.opened = true;
                let width = self.window.frame().size.width;
                let x = frame.origin.x + frame.size.width - LIST_CENTER - width / 2.;
                self.window.setFrameOrigin(NSPoint::new(x, frame.origin.y + 14.));
                if !self.window.isVisible() {
                    self.window.orderFrontRegardless();
                }
            }
        }
        true
    }
}

impl Drop for Panel {
    fn drop(&mut self) {
        self.window.close();
    }
}

enum SettingsWindow {
    Closed,
    Behind,
    Front(NSRect),
}

/// System Settings' window in AppKit's coordinates, if it's the app in front.
fn settings_window() -> SettingsWindow {
    let id = ns_string!("com.apple.systempreferences");
    let front = NSWorkspace::sharedWorkspace().frontmostApplication();
    let Some(front) = front.filter(|app| app.bundleIdentifier().is_some_and(|bundle| &*bundle == id)) else {
        let running = NSRunningApplication::runningApplicationsWithBundleIdentifier(id).count() > 0;
        return if running { SettingsWindow::Behind } else { SettingsWindow::Closed };
    };
    let options = CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements;
    let Some(windows) = CGWindowListCopyWindowInfo(options, 0) else { return SettingsWindow::Behind };
    // Toll-free bridged: a CFArray of CFDictionaries is an NSArray of NSDictionaries.
    let windows: &NSArray<NSDictionary<NSString, AnyObject>> = unsafe { &*(&*windows as *const CFArray).cast() };
    let number = |window: &NSDictionary<NSString, AnyObject>, key: &NSString| {
        window.objectForKey(key).and_then(|value| value.downcast::<NSNumber>().ok()).map(|value| value.doubleValue())
    };
    let frame = windows
        .iter()
        .filter(|window| {
            number(window, ns_string!("kCGWindowOwnerPID")) == Some(front.processIdentifier() as f64)
                && number(window, ns_string!("kCGWindowLayer")) == Some(0.)
        })
        .filter_map(|window| {
            let bounds = window.objectForKey(ns_string!("kCGWindowBounds"))?;
            let mut frame = CGRect::default();
            let bounds = unsafe { &*(&*bounds as *const AnyObject).cast::<CFDictionary>() };
            unsafe { CGRectMakeWithDictionaryRepresentation(Some(bounds), &mut frame) }.then_some(frame)
        })
        .max_by(|a, b| (a.size.width * a.size.height).total_cmp(&(b.size.width * b.size.height)));
    let Some(frame) = frame else { return SettingsWindow::Behind };
    // Window bounds count down from the main screen's top, but AppKit counts up from its bottom.
    let height = NSScreen::screens(mtm()).firstObject().map_or(0., |screen| screen.frame().size.height);
    SettingsWindow::Front(rect(
        frame.origin.x,
        height - frame.origin.y - frame.size.height,
        frame.size.width,
        frame.size.height,
    ))
}

struct Views {
    arrow: Retained<NSImageView>,
    title: Retained<NSTextField>,
    close: Retained<NSImageView>,
    row: Retained<NSBox>,
    icon: Retained<NSImageView>,
    app: Retained<NSURL>,
    closed: Cell<bool>,
}

define_class!(
    /// What to do, a close button, and Mu to drag into System Settings' list.
    #[unsafe(super(NSView, NSResponder, NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = Views]
    struct GuideView;

    impl GuideView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }

        /// The panel never takes focus from System Settings, so every click is a first one.
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _event: Option<&NSEvent>) -> bool {
            true
        }

        /// Takes every click itself, since the views in it are only pictures.
        #[unsafe(method_id(hitTest:))]
        fn hit_test(&self, point: NSPoint) -> Option<Retained<NSView>> {
            let hit: Option<Retained<NSView>> = unsafe { msg_send![super(self), hitTest: point] };
            hit.map(|_| self.retain().into_super())
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            let views = self.ivars();
            let point = self.convertPoint_fromView(event.locationInWindow(), None);
            if NSMouseInRect(point, views.close.frame(), true) {
                views.closed.set(true);
            } else if !views.row.isHidden() && NSMouseInRect(point, views.row.frame(), true) {
                let item = NSDraggingItem::initWithPasteboardWriter(
                    NSDraggingItem::alloc(),
                    ProtocolObject::from_ref(&*views.app),
                );
                let frame = self.convertRect_fromView(views.icon.frame(), Some(&views.row));
                if let Some(image) = views.icon.image() {
                    let image: &AnyObject = &image;
                    unsafe { item.setDraggingFrame_contents(frame, Some(image)) };
                }
                let items = NSArray::from_retained_slice(&[item]);
                self.beginDraggingSessionWithItems_event_source(&items, event, ProtocolObject::from_ref(self));
            }
        }
    }

    unsafe impl NSObjectProtocol for GuideView {}

    unsafe impl NSDraggingSource for GuideView {
        #[unsafe(method(draggingSession:sourceOperationMaskForDraggingContext:))]
        fn source_operation_mask(&self, _session: &NSDraggingSession, _context: NSDraggingContext) -> NSDragOperation {
            NSDragOperation::Copy
        }

        /// Lifts Mu out of its row while it's dragged.
        #[unsafe(method(draggingSession:willBeginAtPoint:))]
        fn will_begin(&self, _session: &NSDraggingSession, _point: NSPoint) {
            self.ivars().icon.setHidden(true);
        }

        #[unsafe(method(draggingSession:endedAtPoint:operation:))]
        fn ended(&self, _session: &NSDraggingSession, _point: NSPoint, _operation: NSDragOperation) {
            self.ivars().icon.setHidden(false);
        }
    }
);

impl GuideView {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let arrow = symbol(
            ns_string!("arrow.up"),
            ARROW_SIZE,
            unsafe { NSFontWeightBold },
            &NSColor::controlAccentColor(),
            mtm,
        );
        arrow.setFrame(rect(16., 13., 24., 22.));

        let title = NSTextField::labelWithString(ns_string!(""), mtm);
        title.setFont(Some(&NSFont::systemFontOfSize_weight(13., unsafe { NSFontWeightSemibold })));
        title.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        title.setFrame(rect(48., 15., WIDTH - 48. - 44., 18.));

        let close = symbol(
            ns_string!("xmark.circle.fill"),
            15.,
            unsafe { NSFontWeightRegular },
            &NSColor::tertiaryLabelColor(),
            mtm,
        );
        close.setFrame(rect(WIDTH - 36., 13., 22., 22.));

        let bundle = NSBundle::mainBundle();
        let icon =
            NSImageView::imageViewWithImage(&NSWorkspace::sharedWorkspace().iconForFile(&bundle.bundlePath()), mtm);
        icon.setFrame(rect(8., 6., 28., 28.));
        let name = NSTextField::labelWithString(ns_string!("Mu"), mtm);
        name.setFont(Some(&NSFont::systemFontOfSize_weight(14., unsafe { NSFontWeightSemibold })));
        name.sizeToFit();
        name.setFrameOrigin(NSPoint::new(44., (40. - name.frame().size.height) / 2.));
        let row = NSBox::new(mtm);
        row.setBoxType(NSBoxType::Custom);
        row.setTitlePosition(NSTitlePosition::NoTitle);
        row.setBorderWidth(0.);
        row.setCornerRadius(10.);
        row.setFillColor(&NSColor::quaternarySystemFillColor());
        row.setFrame(rect(48., 46., WIDTH - 48. - 16., 40.));
        row.addSubview(&icon);
        row.addSubview(&name);

        let views = Views { arrow, title, close, row, icon, app: bundle.bundleURL(), closed: Cell::new(false) };
        let this = Self::alloc(mtm).set_ivars(views);
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: rect(0., 0., WIDTH, HEIGHT)] };
        this.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        let views = this.ivars();
        for view in [&*views.arrow as &NSView, &views.title, &views.close, &views.row] {
            this.addSubview(view);
        }
        this
    }
}

fn symbol(
    name: &NSString,
    size: f64,
    weight: NSFontWeight,
    color: &NSColor,
    mtm: MainThreadMarker,
) -> Retained<NSImageView> {
    let view = NSImageView::imageViewWithImage(&symbol_image(name, size, weight), mtm);
    view.setContentTintColor(Some(color));
    view
}

fn symbol_image(name: &NSString, size: f64, weight: NSFontWeight) -> Retained<NSImage> {
    let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(name, None).expect("an SF Symbol");
    image
        .imageWithSymbolConfiguration(&NSImageSymbolConfiguration::configurationWithPointSize_weight(size, weight))
        .unwrap_or(image)
}

fn rect(x: f64, y: f64, width: f64, height: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
}

fn mtm() -> MainThreadMarker {
    MainThreadMarker::new().expect("the guide runs on the main thread")
}
