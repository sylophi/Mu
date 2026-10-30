//! One folder view (a tab): its listing, selection, history and the file operations started from it.

mod layout;
mod listing;
mod marquee;
mod menu;
mod ops;
mod selection;
mod view;

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use gpui_kit::component::{
    ActiveTheme as _, Sizable as _,
    button::{Button, ButtonVariants as _},
    menu::ContextMenuExt as _,
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::{
    drag::DropFiles as _,
    fs::{self, Entry},
    permissions::{self, Permission, Permissions, Status},
    quicklook,
    settings::Settings,
    thumbnails::Cache,
    workspace::{
        TabEvent,
        actions::{SHIFTED_DIGITS, ShowPlace},
    },
};

use layout::Layout;
use listing::{AfterLoad, Listing};
use marquee::{Marquee, SelectArea};
use ops::{CutFiles, RenameState};
use selection::Selection;
use view::{HEADER_HEIGHT, Mode, TILE_ICON};

gpui_kit::actions!(
    mu,
    [
        SelectPrev,
        SelectNext,
        ExtendPrev,
        ExtendNext,
        SelectLeft,
        SelectRight,
        ExtendLeft,
        ExtendRight,
        SelectFirst,
        SelectLast,
        ExtendFirst,
        ExtendLast,
        PageUp,
        PageDown,
        SelectAll,
        ClearSelection,
        Open,
        OpenInNewTab,
        OpenInNewWindow,
        ShowPackageContents,
        Rename,
        CancelRename,
        Trash,
        PutBack,
        Copy,
        Cut,
        Paste,
        Duplicate,
        Compress,
        NewFolder,
        CopyPath,
        QuickLook,
        OpenInTerminal,
        OpenInEditor,
        AddToSidebar,
        Refresh,
        Undo,
        Redo,
    ]
);

pub fn init(cx: &mut App) {
    cx.set_global(CutFiles::default());
    // While a name is being edited, keys belong to the text field.
    let list = Some("FileList && !renaming");
    let grid = Some("FileList && grid && !renaming");
    cx.bind_keys([
        KeyBinding::new("up", SelectPrev, list),
        KeyBinding::new("down", SelectNext, list),
        KeyBinding::new("shift-up", ExtendPrev, list),
        KeyBinding::new("shift-down", ExtendNext, list),
        KeyBinding::new("left", SelectLeft, grid),
        KeyBinding::new("right", SelectRight, grid),
        KeyBinding::new("shift-left", ExtendLeft, grid),
        KeyBinding::new("shift-right", ExtendRight, grid),
        KeyBinding::new("home", SelectFirst, list),
        KeyBinding::new("end", SelectLast, list),
        KeyBinding::new("alt-up", SelectFirst, list),
        KeyBinding::new("alt-down", SelectLast, list),
        KeyBinding::new("shift-home", ExtendFirst, list),
        KeyBinding::new("shift-end", ExtendLast, list),
        KeyBinding::new("pageup", PageUp, list),
        KeyBinding::new("pagedown", PageDown, list),
        KeyBinding::new("cmd-a", SelectAll, list),
        KeyBinding::new("escape", ClearSelection, list),
        KeyBinding::new("enter", Open, list),
        KeyBinding::new("cmd-down", Open, list),
        KeyBinding::new("cmd-o", Open, list),
        KeyBinding::new("cmd-enter", OpenInNewTab, list),
        KeyBinding::new("cmd-e", OpenInEditor, list),
        // The later one is what menus show.
        KeyBinding::new("f2", Rename, list),
        KeyBinding::new("shift-enter", Rename, list),
        KeyBinding::new("delete", Trash, list),
        KeyBinding::new("cmd-backspace", Trash, list),
        KeyBinding::new("cmd-c", Copy, list),
        KeyBinding::new("cmd-x", Cut, list),
        KeyBinding::new("cmd-v", Paste, list),
        KeyBinding::new("cmd-d", Duplicate, list),
        KeyBinding::new("cmd-shift-n", NewFolder, list),
        KeyBinding::new("alt-cmd-c", CopyPath, list),
        KeyBinding::new("space", QuickLook, list),
        KeyBinding::new("cmd-r", Refresh, list),
        KeyBinding::new("cmd-z", Undo, list),
        KeyBinding::new("cmd-shift-z", Redo, list),
        // In the Trash, like Finder.
        KeyBinding::new("cmd-backspace", PutBack, Some("FileList && trash && !renaming")),
        KeyBinding::new("escape", CancelRename, Some("Rename")),
    ]);
}

/// How long going to a folder waits for its listing. Most folders list in a millisecond or two, so
/// they show up in the very next frame, without a frame of the folder that was there before.
const LISTING_WAIT: Duration = Duration::from_millis(20);

pub struct Browser {
    path: PathBuf,
    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
    listing: Listing,
    error: Option<SharedString>,
    /// Set while macOS's privacy protection keeps this folder closed, which Full Disk Access opens.
    blocked: Option<permissions::Watch>,
    filter: SharedString,
    selection: Selection,
    rename: Option<RenameState>,
    after_load: AfterLoad,
    type_ahead: String,
    type_ahead_at: Instant,
    loading: bool,
    /// The divider being dragged and the column widths it has made so far, saved once it's let go.
    resizing: Option<(usize, [Pixels; 4])>,
    marquee: Option<Marquee>,
    focus_handle: FocusHandle,
    scroll: UniformListScrollHandle,
    /// How far the list's header and rows are scrolled sideways, together.
    columns_scroll: ScrollHandle,
    /// The listing's width as of the last frame, which the grid's columns and the list's rows follow.
    width: Pixels,
    /// Quick Look's pictures for the grid's tiles.
    tiles: Cache,
    /// How Finder lays out this folder, when it gives it a picture, as with a disk image.
    layout: Option<Layout>,
    /// Where each visible item's icon is centered in the layout.
    spots: Vec<Point<Pixels>>,
    _load: Task<()>,
    _watch: Task<()>,
    _settings: Subscription,
    _in_window: [Subscription; 2],
}

impl EventEmitter<TabEvent> for Browser {}

impl Focusable for Browser {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Browser {
    pub fn new(path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            path,
            back: Vec::new(),
            forward: Vec::new(),
            listing: Listing::default(),
            error: None,
            blocked: None,
            filter: SharedString::default(),
            selection: Selection::default(),
            rename: None,
            after_load: AfterLoad::default(),
            type_ahead: String::new(),
            type_ahead_at: Instant::now(),
            loading: false,
            resizing: None,
            marquee: None,
            focus_handle: cx.focus_handle(),
            scroll: UniformListScrollHandle::new(),
            columns_scroll: ScrollHandle::new(),
            width: px(0.),
            tiles: Cache::framed(TILE_ICON),
            layout: None,
            spots: Vec::new(),
            _load: Task::ready(()),
            _watch: Task::ready(()),
            _settings: cx.observe_global::<Settings>(|this, cx| this.update_visible(cx)),
            _in_window: Self::observe_in_window(window, cx),
        };
        cx.on_release(|this, cx| {
            this.tiles.clear(None, cx);
            if let Some(layout) = this.layout.take() {
                cx.drop_image(layout.picture, None);
            }
        })
        .detach();
        this.open_current(Vec::new(), window, cx);
        this
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn title(&self) -> SharedString {
        fs::display_name(&self.path).into()
    }

    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    /// The selected item under the cursor, for the preview pane.
    pub fn preview_target(&self) -> Option<Entry> {
        self.cursor_entry().filter(|e| self.selection.contains(&e.name)).cloned()
    }

    /// (items shown, items selected, bytes in the selected files)
    pub fn status(&self) -> (usize, usize, u64) {
        let selected = self.selection.names.iter().filter_map(|name| self.listing.position(name));
        let size = selected.map(|ix| self.listing[ix].size).sum();
        (self.listing.len(), self.selection.len(), size)
    }

    // Navigation

    pub fn navigate(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let came_from = self.path.clone();
        self.go(path, vec![came_from], window, cx);
    }

    /// Shows `paths`, which are all in one folder, selected there.
    pub fn reveal(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(first) = paths.first() else { return };
        let Some(parent) = first.parent().map(Path::to_path_buf) else {
            return self.navigate(first.clone(), window, cx);
        };
        if parent == self.path {
            // Already there: another look selects them.
            self.reload_selecting(paths, false, window, cx);
        } else {
            self.go(parent, paths, window, cx);
        }
    }

    /// Goes to `path`, selecting whichever of `select` are in it.
    fn go(&mut self, path: PathBuf, select: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        if path == self.path {
            return;
        }
        let previous = std::mem::replace(&mut self.path, path);
        self.back.push(previous);
        self.forward.clear();
        self.open_current(select, window, cx);
    }

    /// Goes back, or forward, through the folders this tab has shown.
    pub fn travel(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (from, to) =
            if forward { (&mut self.forward, &mut self.back) } else { (&mut self.back, &mut self.forward) };
        if let Some(path) = from.pop() {
            let previous = std::mem::replace(&mut self.path, path);
            to.push(previous.clone());
            self.open_current(vec![previous], window, cx);
        }
    }

    pub fn go_up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(parent) = self.path.parent() {
            self.navigate(parent.to_path_buf(), window, cx);
        }
    }

    /// Shows `self.path`, selecting whichever of `select` are in it, like the folder we came out of.
    fn open_current(&mut self, select: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        self.selection = Selection::default();
        // Leaving with a name half-typed keeps it, as a click elsewhere would.
        self.commit_rename(window, cx);
        self.filter = SharedString::default();
        self.error = None;
        self.tiles.trim(Some(window), cx);
        self.after_load = AfterLoad { select, rename: false };
        // Through the handle a layout tracks too, which scrolling the list to its first row misses.
        self.scroll_handle().set_offset(Point::default());
        self.watch(window, cx);
        self.load_within(LISTING_WAIT, window, cx);
        cx.emit(TabEvent::Navigated(self.path.clone()));
        cx.notify();
    }

    /// What it keeps up with in the window it's in.
    fn observe_in_window(window: &mut Window, cx: &mut Context<Self>) -> [Subscription; 2] {
        [
            // Opens the folder as soon as Full Disk Access is switched on.
            cx.observe_in(&Permissions::global(cx), window, |this, permissions, window, cx| {
                if this.blocked.is_some() && permissions.read(cx).status(Permission::FullDiskAccess) == Status::Granted
                {
                    this.load(window, cx);
                }
            }),
            cx.observe_in(&cx.entity(), window, |this, _, window, cx| {
                if this.focus_handle.contains_focused(window, cx) {
                    quicklook::follow(|| this.selected_paths());
                }
            }),
        ]
    }

    /// Follows its tab into another window, which it keeps up with and loads into from then on.
    pub fn moved(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self._in_window = Self::observe_in_window(window, cx);
        self.watch(window, cx);
        self.load(window, cx);
    }
}

impl Render for Browser {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let empty_message = match (&self.error, self.listing.is_empty(), self.filter.is_empty()) {
            (Some(error), _, _) => Some(error.clone()),
            (None, true, true) if self.loading => None,
            (None, true, true) => Some("This folder is empty".into()),
            (None, true, false) => Some(format!("Nothing here matches “{}”", self.filter).into()),
            _ => None,
        };
        let entity = cx.entity();
        let current_dir = self.path.clone();
        let mode = self.mode(cx);
        let mut key_context = KeyContext::new_with_defaults();
        key_context.add("FileList");
        // A layout's arrows go through its items in order, as in the grid.
        if !matches!(mode, Mode::List) {
            key_context.add("grid");
        }
        if self.in_trash() {
            key_context.add("trash");
        }
        if self.rename.is_some() {
            key_context.add("renaming");
        }
        div()
            .id("file-list")
            .relative()
            .size_full()
            .min_h_0()
            .bg(cx.theme().background)
            .track_focus(&self.focus_handle)
            .key_context(key_context)
            .on_action(cx.listener(|this, _: &SelectPrev, _, cx| this.move_rows(-1, false, cx)))
            .on_action(cx.listener(|this, _: &SelectNext, _, cx| this.move_rows(1, false, cx)))
            .on_action(cx.listener(|this, _: &ExtendPrev, _, cx| this.move_rows(-1, true, cx)))
            .on_action(cx.listener(|this, _: &ExtendNext, _, cx| this.move_rows(1, true, cx)))
            .on_action(cx.listener(|this, _: &SelectLeft, _, cx| this.move_cursor(-1, false, cx)))
            .on_action(cx.listener(|this, _: &SelectRight, _, cx| this.move_cursor(1, false, cx)))
            .on_action(cx.listener(|this, _: &ExtendLeft, _, cx| this.move_cursor(-1, true, cx)))
            .on_action(cx.listener(|this, _: &ExtendRight, _, cx| this.move_cursor(1, true, cx)))
            .on_action(cx.listener(|this, _: &SelectFirst, _, cx| this.select_end(false, false, cx)))
            .on_action(cx.listener(|this, _: &SelectLast, _, cx| this.select_end(true, false, cx)))
            .on_action(cx.listener(|this, _: &ExtendFirst, _, cx| this.select_end(false, true, cx)))
            .on_action(cx.listener(|this, _: &ExtendLast, _, cx| this.select_end(true, true, cx)))
            .on_action(cx.listener(|this, _: &PageUp, _, cx| this.move_page(false, cx)))
            .on_action(cx.listener(|this, _: &PageDown, _, cx| this.move_page(true, cx)))
            .on_action(cx.listener(|this, _: &SelectAll, _, cx| this.select_all(cx)))
            .on_action(cx.listener(|this, _: &ClearSelection, _, cx| this.clear_selection(cx)))
            .on_action(cx.listener(|this, _: &Open, window, cx| this.open(window, cx)))
            .on_action(cx.listener(|this, _: &OpenInNewTab, _, cx| this.open_in_new_tab(cx)))
            .on_action(cx.listener(|this, _: &OpenInNewWindow, _, cx| this.open_in_new_window(cx)))
            .on_action(cx.listener(|this, _: &ShowPackageContents, window, cx| this.show_package_contents(window, cx)))
            .on_action(cx.listener(|this, _: &OpenInTerminal, _, cx| this.open_in_terminal(cx)))
            .on_action(cx.listener(|this, _: &OpenInEditor, _, cx| this.open_in_editor(cx)))
            .on_action(cx.listener(|this, _: &QuickLook, _, cx| {
                if this.typing_ahead() {
                    this.extend_type_ahead(" ", cx);
                } else {
                    quicklook::toggle(this.selected_paths());
                }
            }))
            // Like Space, a symbol typed with ⇧ and a digit goes on with a name being typed.
            .on_action(cx.listener(|this, &ShowPlace(ix): &ShowPlace, _, cx| {
                if this.typing_ahead() {
                    this.extend_type_ahead(SHIFTED_DIGITS[ix], cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &Rename, window, cx| this.start_rename(window, cx)))
            .on_action(cx.listener(|this, _: &Trash, window, cx| this.trash(window, cx)))
            .on_action(cx.listener(|this, _: &PutBack, window, cx| this.put_back(window, cx)))
            .on_action(cx.listener(|this, _: &Undo, window, cx| this.take_back(false, window, cx)))
            .on_action(cx.listener(|this, _: &Redo, window, cx| this.take_back(true, window, cx)))
            .on_action(cx.listener(|this, _: &Copy, _, cx| this.copy(false, cx)))
            .on_action(cx.listener(|this, _: &Cut, _, cx| this.copy(true, cx)))
            .on_action(cx.listener(|this, _: &Paste, window, cx| this.paste(window, cx)))
            .on_action(cx.listener(|this, _: &Duplicate, window, cx| this.duplicate(window, cx)))
            .on_action(cx.listener(|this, _: &Compress, window, cx| this.compress(window, cx)))
            .on_action(cx.listener(|this, _: &NewFolder, window, cx| this.new_folder(window, cx)))
            .on_action(cx.listener(|this, _: &CopyPath, _, cx| this.copy_path(cx)))
            .on_action(cx.listener(|this, _: &AddToSidebar, _, cx| this.add_to_sidebar(cx)))
            .on_action(cx.listener(|this, _: &Refresh, window, cx| this.load(window, cx)))
            .on_key_down(cx.listener(Self::type_ahead))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::press_listing))
            .on_drag(SelectArea, |_, _, _, cx| cx.new(|_| SelectArea))
            .on_drag_move(cx.listener(Self::drag_marquee))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::end_marquee))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::end_marquee))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, window, cx| {
                    if !this.focus_handle.contains_focused(window, cx) {
                        this.focus_handle.focus(window, cx);
                    }
                }),
            )
            .on_drop_files(cx.listener(move |this, paths: &[PathBuf], window, cx| {
                this.drop_paths(paths, current_dir.clone(), window, cx)
            }))
            .child({
                let entity = entity.clone();
                canvas(
                    move |bounds, _, cx| {
                        entity.update(cx, |this, cx| {
                            if this.width != bounds.size.width {
                                this.width = bounds.size.width;
                                cx.notify();
                            }
                        })
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full()
            })
            .child(match mode {
                Mode::Layout(layout) => self.render_layout(layout, window, cx),
                Mode::List | Mode::Grid => self.render_listing(cx),
            })
            .children(empty_message.map(|message| {
                let fix = self.blocked.is_some()
                    && Permissions::global(cx).read(cx).status(Permission::FullDiskAccess) != Status::Granted;
                v_flex()
                    .absolute()
                    .inset_0()
                    .when(matches!(mode, Mode::List), |el| el.top(HEADER_HEIGHT))
                    .gap_3()
                    .items_center()
                    .justify_center()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(message)
                    .when(fix, |el| {
                        el.child(
                            Button::new("allow-access").primary().small().label("Allow Full Disk Access").on_click(
                                |_, _, cx| {
                                    Permissions::global(cx)
                                        .update(cx, |p, cx| p.allow(&[Permission::FullDiskAccess], cx))
                                },
                            ),
                        )
                    })
            }))
            .context_menu(move |menu, window, cx| Browser::context_menu(&entity, menu, window, cx))
    }
}
