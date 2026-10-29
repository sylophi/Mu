//! Quick Look through the system preview panel, the same one Finder uses.
//!
//! The panel only shows items from a controller it finds in the key window's responder chain, so
//! GPUI's view is taught to be one.

use std::{
    cell::{Cell, OnceCell, RefCell},
    ffi::{c_char, c_void},
    path::PathBuf,
};

use block2::RcBlock;
use gpui_kit::{Pixels, Point, point, px};
use objc2::{
    DefinedClass as _, MainThreadMarker, MainThreadOnly, define_class, ffi, msg_send,
    rc::Retained,
    runtime::{AnyClass, AnyObject, Bool, Imp, NSObject, NSObjectProtocol, ProtocolObject, Sel},
    sel,
};
use objc2_app_kit::{NSApplication, NSEvent, NSEventType, NSView, NSWindowDelegate};
use objc2_core_foundation::{CFRunLoop, kCFRunLoopCommonModes};
use objc2_foundation::{NSInteger, NSURL};
use objc2_quick_look_ui::{QLPreviewItem, QLPreviewPanel, QLPreviewPanelDataSource, QLPreviewPanelDelegate};

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = RefCell<Vec<Retained<NSURL>>>]
    struct Items;

    unsafe impl NSObjectProtocol for Items {}

    unsafe impl QLPreviewPanelDataSource for Items {
        #[unsafe(method(numberOfPreviewItemsInPreviewPanel:))]
        fn count(&self, _panel: Option<&QLPreviewPanel>) -> NSInteger {
            self.ivars().borrow().len() as NSInteger
        }

        #[unsafe(method_id(previewPanel:previewItemAtIndex:))]
        fn item(
            &self,
            _panel: Option<&QLPreviewPanel>,
            index: NSInteger,
        ) -> Option<Retained<ProtocolObject<dyn QLPreviewItem>>> {
            let url = self.ivars().borrow().get(index as usize).cloned();
            url.map(ProtocolObject::from_retained)
        }
    }

    unsafe impl NSWindowDelegate for Items {}

    unsafe impl QLPreviewPanelDelegate for Items {
        /// The panel takes the keyboard, so keys it doesn't use (arrows, shortcuts) go on to the list.
        #[unsafe(method(previewPanel:handleEvent:))]
        fn handle_event(&self, _panel: Option<&QLPreviewPanel>, event: Option<&NSEvent>) -> bool {
            let event = event.filter(|event| event.r#type() == NSEventType::KeyDown);
            let window = NSApplication::sharedApplication(self.mtm()).mainWindow();
            match (event, window) {
                (Some(event), Some(window)) => {
                    window.sendEvent(event);
                    true
                }
                _ => false,
            }
        }
    }
);

/// What a tap with three fingers on a window does, given its view and where in it the tap was.
type OnTap = Box<dyn Fn(*const c_void, Point<Pixels>)>;

thread_local! {
    static ITEMS: Retained<Items> = {
        let items = Items::alloc(mtm()).set_ivars(RefCell::default());
        unsafe { msg_send![super(items), init] }
    };
    static ON_TAP: OnceCell<OnTap> = const { OnceCell::new() };
}

fn mtm() -> MainThreadMarker {
    MainThreadMarker::new().expect("Quick Look runs on the main thread")
}

/// Teaches GPUI's view, the one in every window, to answer Quick Look. It becomes a controller
/// for the panel. (The window itself won't do: NSWindow answers `respondsToSelector:` for these
/// with NO.) And it passes a tap with three fingers, or a force click if that's what looks things
/// up, on to `on_tap` with where in the window it was. That's what Finder shows Quick Look for.
pub fn init(on_tap: impl Fn(*const c_void, Point<Pixels>) + 'static) {
    extern "C-unwind" fn accepts(_: &AnyObject, _: Sel, _: &QLPreviewPanel) -> Bool {
        Bool::YES
    }
    extern "C-unwind" fn begin(_: &AnyObject, _: Sel, panel: &QLPreviewPanel) {
        ITEMS.with(|items| unsafe {
            panel.setDataSource(Some(ProtocolObject::from_ref(&**items)));
            panel.setDelegate(Some(items));
        });
    }
    extern "C-unwind" fn end(_: &AnyObject, _: Sel, panel: &QLPreviewPanel) {
        unsafe {
            panel.setDataSource(None);
            panel.setDelegate(None);
        }
    }
    extern "C-unwind" fn tap(view: &NSView, _: Sel, event: &NSEvent) {
        let location = event.locationInWindow();
        // From the top left, as GPUI has it.
        let at = point(px(location.x as f32), px((view.frame().size.height - location.y) as f32));
        ON_TAP.with(|on_tap| on_tap.get().map(|on_tap| on_tap(view as *const NSView as *const c_void, at)));
    }
    ON_TAP.with(|cell| cell.set(Box::new(on_tap)).ok());
    let class = AnyClass::get(c"GPUIView").expect("GPUI makes its view class at launch");
    let class = class as *const AnyClass as *mut AnyClass;
    type Accepts = extern "C-unwind" fn(&AnyObject, Sel, &QLPreviewPanel) -> Bool;
    type Control = extern "C-unwind" fn(&AnyObject, Sel, &QLPreviewPanel);
    type Tap = extern "C-unwind" fn(&NSView, Sel, &NSEvent);
    let methods: [(Sel, Imp, &[u8]); 4] = unsafe {
        [
            (sel!(acceptsPreviewPanelControl:), std::mem::transmute::<Accepts, Imp>(accepts), b"B@:@\0"),
            (sel!(beginPreviewPanelControl:), std::mem::transmute::<Control, Imp>(begin), b"v@:@\0"),
            (sel!(endPreviewPanelControl:), std::mem::transmute::<Control, Imp>(end), b"v@:@\0"),
            (sel!(quickLookWithEvent:), std::mem::transmute::<Tap, Imp>(tap), b"v@:@\0"),
        ]
    };
    for (sel, imp, types) in methods {
        unsafe { ffi::class_addMethod(class, sel, imp, types.as_ptr() as *const c_char) };
    }
}

fn panel() -> Option<Retained<QLPreviewPanel>> {
    let mtm = mtm();
    unsafe { QLPreviewPanel::sharedPreviewPanelExists(mtm).then(|| QLPreviewPanel::sharedPreviewPanel(mtm)).flatten() }
}

/// Opens the panel on `paths`, or closes it if it's already open.
pub fn toggle(paths: Vec<PathBuf>) {
    soon(move || {
        if !close() {
            open(paths);
        }
    });
}

/// Shows `paths` in the panel, opening it if it isn't.
pub fn show(paths: Vec<PathBuf>) {
    soon(move || open(paths));
}

fn open(paths: Vec<PathBuf>) {
    if paths.is_empty() || NSApplication::sharedApplication(mtm()).keyWindow().is_none() {
        return;
    }
    let Some(panel) = (unsafe { QLPreviewPanel::sharedPreviewPanel(mtm()) }) else { return };
    set_items(paths);
    unsafe {
        panel.updateController();
        panel.reloadData();
    }
    panel.orderFront(None);
}

/// Shows `paths` in the panel if it's open, as the selection changes.
pub fn follow(paths: impl FnOnce() -> Vec<PathBuf>) {
    let Some(panel) = panel().filter(|panel| panel.isVisible()) else { return };
    let paths = paths();
    soon(move || {
        if paths.is_empty() {
            return panel.orderOut(None);
        }
        if ITEMS.with(|items| {
            items.ivars().borrow().iter().map(|url| url.to_file_path()).eq(paths.iter().cloned().map(Some))
        }) {
            return;
        }
        set_items(paths);
        unsafe { panel.reloadData() };
    });
}

/// Runs `f` after the GPUI update that asks for it. A panel call can run the main loop while Quick
/// Look reads an app, and any GPUI task that runs in there would find `App` still borrowed by the
/// update and panic. It runs from the run loop, not the main queue: run from a block on that queue,
/// the panel would wait half a second for answers that come on it before it gave up and showed.
fn soon(f: impl FnOnce() + 'static) {
    let f = Cell::new(Some(f));
    let block = RcBlock::new(move || f.take().map_or((), |f| f()));
    let main = CFRunLoop::main().expect("the main thread has a run loop");
    unsafe { main.perform_block(kCFRunLoopCommonModes.map(|mode| &**mode), Some(&block)) };
    main.wake_up();
}

/// Closes the panel. Returns whether it was open.
pub fn close() -> bool {
    let Some(panel) = panel().filter(|panel| panel.isVisible()) else { return false };
    panel.orderOut(None);
    true
}

fn set_items(paths: Vec<PathBuf>) {
    let urls = paths.iter().map(|path| crate::fs::file_url(path)).collect();
    ITEMS.with(|items| *items.ivars().borrow_mut() = urls);
}
