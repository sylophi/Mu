//! The tab strip, and dragging tabs as in Zed: onto another tab to take its place, or past the last
//! one. Unlike in Zed, and as in a browser, also onto another window's tabs, or out of any window
//! into one of their own.

use std::{cell::Cell, path::PathBuf, rc::Rc};

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, Theme,
    button::Button,
    h_flex,
    menu::{ContextMenuExt as _, PopupMenuItem},
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSEvent, NSWindow};
use objc2_foundation::NSPoint;

use super::{
    Item, Workspace,
    actions::{FindInFolder, NewTab, NewTerminal, ShowHistory, ShowSettings, ShowStorage},
    pane::paste_paths,
    parts::{action_button, close_button, menu_item, truncated},
    windows::{ns_window, open, workspaces},
};
use crate::{
    drag::{self, DropFiles as _},
    session::Tab,
};

/// A tab being dragged. It stays where it is, and a line shows where it would go.
pub(super) struct DraggedTab {
    pub(super) id: EntityId,
    /// Where the pointer is on it, known once the drag starts.
    pub(super) grabbed_at: Rc<Cell<Point<Pixels>>>,
}

/// Where a tab dragged from a window would go if let go now.
pub(super) struct TabDrag {
    pub(super) id: EntityId,
    grabbed_at: Point<Pixels>,
    /// Where the pointer is on the screen, as AppKit has it.
    at: NSPoint,
    /// Mu's window under the pointer, and where among its tabs, when over them.
    over: Option<(AnyWindowHandle, WeakEntity<Workspace>, Option<usize>)>,
}

impl Workspace {
    /// The tab at `at` in the window, or past the last one, how many there are. None off the strip.
    fn tab_at(&self, at: Point<Pixels>) -> Option<usize> {
        if !self.strip_bounds.get().contains(&at) {
            return None;
        }
        // Where each was laid out, before the strip scrolled.
        let x = at.x - self.tabs_scroll.offset().x;
        let tabs = (0..self.items.len()).map_while(|ix| self.tabs_scroll.bounds_for_item(ix));
        Some(tabs.take_while(|tab| x >= tab.right()).count())
    }

    /// Where what's dragged over the tabs would go, as the gap before the tab at that index, or after
    /// the last at how many there are. None for a tab that would stay where it is.
    fn drop_gap(&self) -> Option<usize> {
        let from = self.tab_drag.as_ref().and_then(|drag| self.position(drag.id));
        self.items.drop_gap(from, self.tab_drop?)
    }

    /// Follows a tab dragged from this window, showing where it would go in the window it's over.
    pub(super) fn tab_dragged(
        &mut self,
        id: EntityId,
        grabbed_at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let at = NSEvent::mouseLocation();
        let over = window_at(at, window, cx).map(|(handle, workspace, local)| {
            let drop = match workspace.entity_id() == cx.entity_id() {
                true => self.tab_at(local),
                false => workspace.read(cx).tab_at(local),
            };
            (handle, workspace.downgrade(), drop)
        });
        let before = self.tab_drag.take().and_then(|drag| drag.over).map(|(_, workspace, _)| workspace);
        if let Some(before) = before
            && over.as_ref().is_none_or(|(_, workspace, _)| workspace.entity_id() != before.entity_id())
        {
            self.show_tab_drop(&before, None, cx);
        }
        if let Some((_, workspace, drop)) = &over {
            self.show_tab_drop(workspace, *drop, cx);
        }
        self.tab_drag = Some(TabDrag { id, grabbed_at, at, over });
    }

    /// Puts a tab dragged from this window where it was let go: elsewhere among the tabs, among
    /// another window's, or anywhere else in a window of its own.
    pub(super) fn tab_dropped(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = self.tab_drag.take() else { return };
        if let Some((_, workspace, _)) = &drag.over {
            self.show_tab_drop(workspace, None, cx);
        }
        let Some(from) = self.position(drag.id) else { return };
        match drag.over {
            Some((_, workspace, drop)) if workspace.entity_id() == cx.entity_id() => {
                if let Some(to) = drop {
                    self.move_item(from, to, cx);
                }
            }
            Some((handle, workspace, Some(to))) => {
                let item = self.take_item(from, window, cx);
                handle
                    .update(cx, |_, window, cx| workspace.update(cx, |this, cx| this.receive(to, item, window, cx)))
                    .ok();
            }
            _ => self.tear_off(from, drag.at, drag.grabbed_at, window, cx),
        }
    }

    fn show_tab_drop(&mut self, workspace: &WeakEntity<Workspace>, drop: Option<usize>, cx: &mut Context<Self>) {
        match workspace.entity_id() == cx.entity_id() {
            true => self.set_tab_drop(drop, cx),
            false => _ = workspace.update(cx, |this, cx| this.set_tab_drop(drop, cx)),
        }
    }

    pub(super) fn set_tab_drop(&mut self, drop: Option<usize>, cx: &mut Context<Self>) {
        if self.tab_drop != drop {
            self.tab_drop = drop;
            cx.notify();
        }
    }

    /// Moves tab `from` to `to`, where it takes the place of the one there.
    pub(super) fn move_item(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        self.items.move_item(from, to);
        cx.notify();
    }

    /// Takes in a tab from another window at `ix`, and shows it, with the window in front.
    fn receive(&mut self, ix: usize, item: Item, window: &mut Window, cx: &mut Context<Self>) {
        if let Item::Folder(browser) = &item {
            browser.update(cx, |browser, cx| browser.moved(window, cx));
        }
        self.insert_item(ix, item, window, cx);
        self.activate(ix, window, cx);
        window.activate_window();
    }

    /// Moves tab `from` into a window of its own, with the tab under the pointer at `at`, where it
    /// was grabbed. Alone in this one, it already has one.
    fn tear_off(
        &mut self,
        from: usize,
        at: NSPoint,
        grabbed_at: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.items.len() == 1 {
            return;
        }
        // It's the first tab there, where the first one is here.
        let pointer = self.tabs_scroll.bounds_for_item(0).map(|tab| tab.origin).unwrap_or_default() + grabbed_at;
        let size = window.bounds().size;
        let item = self.take_item(from, window, cx);
        cx.defer(move |cx| {
            let bounds = WindowBounds::Windowed(Bounds::new(Point::default(), size));
            open(bounds, None, cx, move |this, window, cx| {
                this.receive(0, item, window, cx);
                if let Some(ns_window) = ns_window(window) {
                    let x = at.x - f64::from(f32::from(pointer.x));
                    let y = at.y + f64::from(f32::from(pointer.y));
                    ns_window.setFrameTopLeftPoint(NSPoint::new(x, y));
                    // Kept clear of the menu bar, which would hide the part it's dragged by.
                    let frame = ns_window.constrainFrameRect_toScreen(ns_window.frame(), ns_window.screen().as_deref());
                    ns_window.setFrame_display(frame, false);
                }
            });
        });
    }

    /// Where files dragged to `at` would go among the tabs: past the last one, if they're all folders.
    pub(super) fn folder_drop(&self, at: Point<Pixels>, paths: &[PathBuf]) -> Option<usize> {
        let end = self.items.len();
        (self.tab_at(at) == Some(end) && paths.iter().all(|path| path.is_dir())).then_some(end)
    }

    /// Opens folders dropped past the last tab in tabs there, showing the first.
    fn folders_dropped(&mut self, paths: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) {
        let first = self.items.len();
        if self.tab_drop != Some(first) {
            return;
        }
        for path in paths {
            let item = Item::new(Tab::Folder(path.clone()), window, cx);
            self.insert_item(self.items.len(), item, window, cx);
        }
        self.activate(first, window, cx);
    }
}

/// Mu's window at `at` on the screen, if it's the one in front there, its workspace, and where `at`
/// is in it. `own` is the window this is asked from.
fn window_at(
    at: NSPoint,
    own: &Window,
    cx: &mut Context<Workspace>,
) -> Option<(AnyWindowHandle, Entity<Workspace>, Point<Pixels>)> {
    let mtm = MainThreadMarker::new().expect("drags are on the main thread");
    let number = NSWindow::windowNumberAtPoint_belowWindowWithWindowNumber(at, 0, mtm);
    // In the window, measured from its top left corner. Its content fills it, title bar and all.
    let local = |ns_window: &NSWindow| {
        let frame = ns_window.frame();
        point(px((at.x - frame.origin.x) as f32), px((frame.origin.y + frame.size.height - at.y) as f32))
    };
    if let Some(ns_window) = ns_window(own).filter(|ns_window| ns_window.windowNumber() == number) {
        return Some((own.window_handle(), cx.entity(), local(&ns_window)));
    }
    // The others, as `own` is busy with this.
    workspaces(cx).into_iter().find_map(|(handle, workspace)| {
        let ns_window = handle.update(cx, |_, window, _| ns_window(window)).ok().flatten()?;
        (ns_window.windowNumber() == number).then(|| (handle, workspace, local(&ns_window)))
    })
}

impl Workspace {
    pub(super) fn render_tab_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let tabs = self.items.iter().enumerate().map(|(ix, item)| self.render_tab(ix, item, cx)).collect();
        let new_tab = action_button("new-tab", IconName::Plus, "New Tab", NewTab);
        let buttons = [
            action_button("new-terminal", IconName::SquareTerminal, "New Terminal", NewTerminal),
            action_button("find-in-folder", IconName::FolderSearch, "Find in Folder", FindInFolder),
            action_button("storage", IconName::HardDrive, "Storage", ShowStorage),
            action_button("undo-history", IconName::RotateCcwClock, "Undo History", ShowHistory),
            action_button("settings", IconName::Settings, "Settings", ShowSettings),
        ];
        tab_strip(tabs, &self.tabs_scroll, &self.strip_bounds, new_tab, buttons, cx.theme())
            .on_drop_files(cx.listener(|this, paths: &[PathBuf], window, cx| this.folders_dropped(paths, window, cx)))
    }

    fn render_tab(&self, ix: usize, item: &Item, cx: &mut Context<Self>) -> AnyElement {
        let id = item.id();
        let dirty = matches!(item, Item::Editor(editor) if editor.read(cx).is_dirty());
        let close = cx.listener(move |this, _: &ClickEvent, window, cx| {
            this.close_item(id, window, cx);
        });
        let workspace = cx.weak_entity();
        let tab_item = item.clone();
        // A line where what's dragged over the tabs would go, like a caret: before this tab, or
        // after it when it's the last.
        let line = || div().absolute().top_0().bottom_0().w(px(2.)).bg(cx.theme().foreground);
        let line = match self.drop_gap() {
            Some(gap) if gap == ix => Some(line().left_0()),
            Some(gap) if gap == ix + 1 && gap == self.items.len() => Some(line().right_0()),
            _ => None,
        };
        let dragged = DraggedTab { id, grabbed_at: Rc::default() };
        tab(("tab", id), ix == self.items.active(), dirty, item.icon(cx), item.title(cx), close, cx.theme())
            .relative()
            .when(self.tab_drag.as_ref().is_some_and(|drag| drag.id == id), |tab| tab.opacity(0.5))
            .children(line)
            .on_drag(dragged, |tab, grabbed_at, _, cx| {
                tab.grabbed_at.set(grabbed_at);
                cx.new(|_| EmptyView)
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    if let Some(ix) = this.position(id) {
                        this.activate(ix, window, cx);
                    }
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this, _, window, cx| {
                    this.close_item(id, window, cx);
                }),
            )
            .map(|tab| match item {
                Item::Folder(browser) => {
                    let path = browser.read(cx).path().to_path_buf();
                    tab.drop_target().on_drop_files(move |paths, window, cx| {
                        drag::drop_into(paths, path.clone(), window, cx).detach()
                    })
                }
                Item::Terminal(terminal) => {
                    let terminal = terminal.clone();
                    tab.drop_target().on_drop_files(move |paths, window, cx| paste_paths(&terminal, paths, window, cx))
                }
                _ => tab,
            })
            .context_menu(move |menu, _, cx| {
                let menu = menu
                    .item(menu_item("Close", &workspace, move |this, window, cx| this.close_item(id, window, cx)))
                    .item(menu_item("Close Others", &workspace, move |this, window, cx| {
                        this.close_others(id, window, cx)
                    }))
                    .separator();
                let copy = |path: PathBuf| {
                    PopupMenuItem::new("Copy Path").on_click(move |_, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(path.to_string_lossy().into_owned()))
                    })
                };
                match &tab_item {
                    Item::Folder(browser) => {
                        let path = browser.read(cx).path().to_path_buf();
                        let terminal = Tab::Terminal(path.clone());
                        menu.item(menu_item("Open in Terminal", &workspace, move |this, window, cx| {
                            this.open_tab(terminal.clone(), window, cx);
                        }))
                        .item(copy(path))
                    }
                    Item::Terminal(terminal) => {
                        let terminal = terminal.clone();
                        menu.item(menu_item("Show in Files", &workspace, move |this, window, cx| {
                            this.show_terminal_folder(&terminal, window, cx)
                        }))
                    }
                    Item::Editor(editor) => {
                        let path = editor.read(cx).path().to_path_buf();
                        let reveal = vec![path.clone()];
                        menu.item(menu_item("Show in Files", &workspace, move |this, window, cx| {
                            this.reveal(reveal.clone(), window, cx)
                        }))
                        .item(copy(path))
                    }
                    Item::Search(_) | Item::Activity(_) | Item::Storage(_) | Item::History(_) | Item::Settings(_) => {
                        menu
                    }
                }
            })
            .into_any_element()
    }
}

/// A strip of tabs, the button that adds one, which stays in view however far the tabs scroll,
/// and buttons at the end.
fn tab_strip(
    tabs: Vec<AnyElement>,
    scroll: &ScrollHandle,
    bounds: &Rc<Cell<Bounds<Pixels>>>,
    add: Button,
    buttons: impl IntoIterator<Item = Button>,
    theme: &Theme,
) -> Div {
    let edge = || h_flex().h_full().border_b_1().border_color(theme.border);
    let bounds = bounds.clone();
    h_flex()
        .h(px(32.))
        .flex_none()
        .bg(theme.tab_bar)
        .on_children_prepainted(move |parts, _, _| {
            bounds.set(parts.into_iter().reduce(|a, b| a.union(&b)).unwrap_or_default());
        })
        .child(
            h_flex()
                .id("tabs")
                .h_full()
                .flex_initial()
                .min_w_0()
                .overflow_x_scroll()
                .track_scroll(scroll)
                .children(tabs),
        )
        .child(edge().flex_none().px_1().child(add.xsmall()))
        .child(edge().flex_1())
        .child(edge().flex_none().px_1().gap_0p5().children(buttons.into_iter().map(|button| button.xsmall())))
}

/// A tab: its icon, title and a close button, which shows while it's active or hovered. An unsaved
/// editor's shows a dot until you reach for it.
fn tab(
    id: impl Into<ElementId>,
    active: bool,
    dirty: bool,
    icon: AnyElement,
    title: SharedString,
    on_close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    theme: &Theme,
) -> Stateful<Div> {
    let close = close_button(on_close, theme).relative().map(|el| match dirty {
        true => el
            .child(div().size(px(8.)).rounded_full().bg(theme.foreground).group_hover("tab", |s| s.invisible()))
            .child(
                div().absolute().invisible().group_hover("tab", |s| s.visible()).child(Icon::new(IconName::X).xsmall()),
            ),
        false => el
            .when(!active, |el| el.invisible().group_hover("tab", |s| s.visible()))
            .child(Icon::new(IconName::X).xsmall()),
    });
    h_flex()
        .id(id)
        .group("tab")
        .h_full()
        // Short of room, tabs narrow down to this, and only then does the strip scroll.
        .min_w(px(96.))
        .max_w(px(240.))
        .pl_3()
        .pr_1p5()
        .gap_1p5()
        .text_sm()
        .border_r_1()
        .border_color(theme.border)
        .when(active, |tab| tab.bg(theme.tab_active).text_color(theme.tab_active_foreground))
        .when(!active, |tab| {
            tab.border_b_1()
                .text_color(theme.tab_foreground)
                .hover(|s| s.bg(theme.accent.opacity(0.5)).text_color(theme.tab_active_foreground))
        })
        .child(icon)
        .child(truncated(title).text_ellipsis_middle())
        .child(close)
}
