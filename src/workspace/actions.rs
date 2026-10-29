//! The window's commands: their actions, the keys bound to them, the menu bar that lists them, and
//! what they do.

use std::{io, path::PathBuf};

use gpui_kit::component::notification::Notification;
use gpui_kit::{prelude::FluentBuilder as _, *};
use mu_terminal::ShowInFiles;

use super::{
    Item, Workspace,
    sidebar::empty_trash,
    windows::{open_window, quit},
};
use crate::{
    browser::{
        Compress, Copy, CopyPath, Cut, Duplicate, NewFolder, Open, OpenInEditor, OpenInNewTab, OpenInNewWindow,
        OpenInTerminal, Paste, QuickLook, Redo, Refresh, Rename, SelectAll, Trash, Undo,
    },
    editor, finder,
    fs::{self, home_dir},
    history::{self, History},
    palette::{Palette, PaletteEvent},
    session::Tab,
    settings::Settings,
    theme::Toasts as _,
    update,
};

gpui_kit::actions!(
    mu,
    [
        GoBack,
        GoForward,
        GoUp,
        EditPath,
        ConnectToServer,
        CancelPathEdit,
        CompletePath,
        NextSuggestion,
        PreviousSuggestion,
        FocusFilter,
        ClearFilter,
        NewTab,
        CloseTab,
        ReopenClosedTab,
        NextTab,
        PrevTab,
        NewTerminal,
        ToggleHidden,
        ViewAsGrid,
        ViewAsList,
        ToggleView,
        ToggleSidebar,
        TogglePreview,
        ToggleZoom,
        ShowActivity,
        ShowStorage,
        ShowHistory,
        ShowSettings,
        ImportFromFinder,
        EmptyTrash,
        AddToShelf,
        FindInFolder,
        CommandPalette,
        NewWindow,
        CloseWindow,
        Minimize,
        Quit,
    ]
);

/// Shows this folder, one of those the Go menu has.
#[derive(Clone, PartialEq, Action)]
#[action(namespace = mu, no_json)]
struct GoTo(PathBuf);

/// The folders the Go menu has, with their names there and their keys.
fn go_to() -> [(&'static str, &'static str, PathBuf); 6] {
    let home = home_dir();
    [
        ("Home", "cmd-shift-h", home.clone()),
        ("Desktop", "cmd-shift-d", home.join("Desktop")),
        ("Documents", "cmd-shift-o", home.join("Documents")),
        ("Downloads", "alt-cmd-l", home.join("Downloads")),
        ("Applications", "cmd-shift-a", "/Applications".into()),
        ("iCloud Drive", "cmd-shift-i", fs::icloud_drive()),
    ]
}

/// Shows the tab at this position, or the last one when there aren't that many.
#[derive(Clone, PartialEq, Action)]
#[action(namespace = mu, no_json)]
struct ActivateTab(usize);

/// Shows the folder at this position in the sidebar, counting from the top.
#[derive(Clone, PartialEq, Action)]
#[action(namespace = mu, no_json)]
pub struct ShowPlace(pub usize);

/// What ⇧1 to ⇧9 type on a US keyboard, which is how macOS reports them.
pub const SHIFTED_DIGITS: [&str; 9] = ["!", "@", "#", "$", "%", "^", "&", "*", "("];

pub(super) fn init(cx: &mut App) {
    let ws = Some("Workspace");
    cx.bind_keys([
        KeyBinding::new("cmd-[", GoBack, ws),
        KeyBinding::new("cmd-]", GoForward, ws),
        KeyBinding::new("cmd-up", GoUp, ws),
        KeyBinding::new("cmd-l", EditPath, ws),
        KeyBinding::new("cmd-shift-g", EditPath, ws),
        KeyBinding::new("cmd-k", ConnectToServer, ws),
        KeyBinding::new("cmd-f", FocusFilter, ws),
        KeyBinding::new("cmd-t", NewTab, ws),
        KeyBinding::new("cmd-w", CloseTab, ws),
        KeyBinding::new("cmd-shift-t", ReopenClosedTab, ws),
        KeyBinding::new("ctrl-tab", NextTab, ws),
        KeyBinding::new("ctrl-shift-tab", PrevTab, ws),
        // macOS reports shift plus a symbol as the shifted symbol: ⇧⌘] is cmd-}.
        KeyBinding::new("cmd-}", NextTab, ws),
        KeyBinding::new("cmd-{", PrevTab, ws),
        KeyBinding::new("ctrl-`", NewTerminal, ws),
        KeyBinding::new("cmd->", ToggleHidden, ws),
        KeyBinding::new("tab", ToggleView, Some("FileList && !renaming")),
        KeyBinding::new("backspace", GoBack, Some("FileList && !renaming")),
        KeyBinding::new("cmd-b", ToggleSidebar, ws),
        KeyBinding::new("cmd-p", TogglePreview, ws),
        KeyBinding::new("shift-escape", ToggleZoom, ws),
        KeyBinding::new("alt-cmd-a", ShowActivity, ws),
        KeyBinding::new("alt-cmd-s", ShowStorage, ws),
        KeyBinding::new("alt-cmd-z", ShowHistory, ws),
        KeyBinding::new("cmd-shift-backspace", EmptyTrash, ws),
        KeyBinding::new("cmd-,", ShowSettings, ws),
        KeyBinding::new("cmd-shift-f", FindInFolder, ws),
        KeyBinding::new("cmd-shift-p", CommandPalette, ws),
        KeyBinding::new("cmd-shift-w", CloseWindow, ws),
        KeyBinding::new("cmd-m", Minimize, ws),
        KeyBinding::new("escape", CancelPathEdit, Some("PathEditor")),
        // Over the input's own, which do nothing on one line.
        KeyBinding::new("tab", CompletePath, Some("PathEditor > Input")),
        KeyBinding::new("down", NextSuggestion, Some("PathEditor > Input")),
        KeyBinding::new("up", PreviousSuggestion, Some("PathEditor > Input")),
        KeyBinding::new("escape", ClearFilter, Some("Filter")),
        KeyBinding::new("cmd-n", NewWindow, None),
        KeyBinding::new("cmd-q", Quit, None),
    ]);
    cx.bind_keys(go_to().map(|(_, key, path)| KeyBinding::new(key, GoTo(path), ws)));
    // ⌘9 is the last tab, as in a browser.
    cx.bind_keys((1..=8).map(|n| KeyBinding::new(&format!("cmd-{n}"), ActivateTab(n - 1), ws)));
    cx.bind_keys([KeyBinding::new("cmd-9", ActivateTab(usize::MAX), ws)]);
    // In the folder view alone, so the symbols still type everywhere else.
    cx.bind_keys(
        SHIFTED_DIGITS
            .iter()
            .enumerate()
            .map(|(ix, key)| KeyBinding::new(key, ShowPlace(ix), Some("FileList && !renaming"))),
    );
    cx.on_action(|_: &Quit, cx| quit(cx, || {}));
    cx.on_action(|_: &ToggleHidden, cx| Settings::update(cx, |s| s.show_hidden = !s.show_hidden));
    cx.on_action(|_: &ViewAsGrid, cx| Settings::update(cx, |s| s.grid = true));
    cx.on_action(|_: &ViewAsList, cx| Settings::update(cx, |s| s.grid = false));
    cx.on_action(|_: &ToggleView, cx| Settings::update(cx, |s| s.grid = !s.grid));
    // Undo and Redo name what they'd do, which changes with the history.
    cx.observe_global::<History>(|cx| cx.set_menus(menus(cx))).detach();
}

/// The menu bar, which the command palette lists too. Undo and Redo name what they'd do.
pub fn menus(cx: &App) -> Vec<Menu> {
    let (undo, redo) = history::next(cx);
    vec![
        Menu::new("Mu").items([
            MenuItem::action("Settings…", ShowSettings),
            MenuItem::action("Check for Updates", update::CheckForUpdates),
            MenuItem::action("Import from Finder", ImportFromFinder),
            MenuItem::separator(),
            MenuItem::action("Quit Mu", Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("New Window", NewWindow),
            MenuItem::action("New Tab", NewTab),
            MenuItem::action("New Folder", NewFolder),
            MenuItem::action("New Terminal", NewTerminal),
            MenuItem::separator(),
            MenuItem::action("Open", Open),
            MenuItem::action("Open in New Tab", OpenInNewTab),
            MenuItem::action("Open in New Window", OpenInNewWindow),
            MenuItem::action("Open in Editor", OpenInEditor),
            MenuItem::action("Open in Terminal", OpenInTerminal),
            MenuItem::action("Quick Look", QuickLook),
            MenuItem::separator(),
            MenuItem::action("Rename", Rename),
            MenuItem::action("Duplicate", Duplicate),
            MenuItem::action("Compress", Compress),
            MenuItem::action("Add to Shelf", AddToShelf),
            MenuItem::action("Move to Trash", Trash),
            MenuItem::action("Empty Trash…", EmptyTrash),
            MenuItem::separator(),
            MenuItem::action("Save", editor::Save),
            MenuItem::action("Close Tab", CloseTab),
            MenuItem::action("Reopen Closed Tab", ReopenClosedTab),
            MenuItem::action("Close Window", CloseWindow),
        ]),
        Menu::new("Edit").items([
            MenuItem::action(undo.map_or("Undo".into(), |name| format!("Undo {name}")), Undo),
            MenuItem::action(redo.map_or("Redo".into(), |name| format!("Redo {name}")), Redo),
            MenuItem::action("Undo History", ShowHistory),
            MenuItem::separator(),
            MenuItem::action("Cut", Cut),
            MenuItem::action("Copy", Copy),
            MenuItem::action("Paste", Paste),
            MenuItem::action("Copy Path", CopyPath),
            MenuItem::separator(),
            MenuItem::action("Select All", SelectAll),
            MenuItem::action("Filter", FocusFilter),
            MenuItem::action("Find in Folder", FindInFolder),
        ]),
        Menu::new("View").items([
            MenuItem::action("Command Palette", CommandPalette),
            MenuItem::action("Activity", ShowActivity),
            MenuItem::action("Storage", ShowStorage),
            MenuItem::separator(),
            MenuItem::action("Toggle Sidebar", ToggleSidebar),
            MenuItem::action("Toggle Preview", TogglePreview),
            MenuItem::action("Zoom", ToggleZoom),
            MenuItem::separator(),
            MenuItem::action("as Grid", ViewAsGrid),
            MenuItem::action("as List", ViewAsList),
            MenuItem::separator(),
            MenuItem::action("Show Hidden Files", ToggleHidden),
            MenuItem::action("Refresh", Refresh),
        ]),
        Menu::new("Go").items(
            [
                MenuItem::action("Back", GoBack),
                MenuItem::action("Forward", GoForward),
                MenuItem::action("Enclosing Folder", GoUp),
                MenuItem::separator(),
            ]
            .into_iter()
            .chain(go_to().map(|(name, _, path)| MenuItem::action(name, GoTo(path))))
            .chain([
                MenuItem::separator(),
                MenuItem::action("Go to Folder…", EditPath),
                MenuItem::action("Connect to Server…", ConnectToServer),
                MenuItem::action("Show Terminal Folder", ShowInFiles),
            ]),
        ),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Next Tab", NextTab),
            MenuItem::action("Previous Tab", PrevTab),
        ]),
    ]
}

impl Workspace {
    /// What the window's commands do, from wherever focus is in it.
    pub(super) fn register_actions(&self, div: Div, cx: &mut Context<Self>) -> Div {
        let terminal = match self.active_item() {
            Some(Item::Terminal(terminal)) => Some(terminal.clone()),
            _ => None,
        };
        div.on_action(
            cx.listener(|this, _: &GoBack, window, cx| this.with_folder(cx, |b, cx| b.travel(false, window, cx))),
        )
        .on_action(
            cx.listener(|this, _: &GoForward, window, cx| this.with_folder(cx, |b, cx| b.travel(true, window, cx))),
        )
        .on_action(cx.listener(|this, _: &GoUp, window, cx| this.with_folder(cx, |b, cx| b.go_up(window, cx))))
        .on_action(cx.listener(|this, GoTo(path): &GoTo, window, cx| this.show_folder(path.clone(), window, cx)))
        .on_action(cx.listener(|this, _: &EditPath, window, cx| this.edit_path(false, window, cx)))
        .on_action(cx.listener(|this, _: &ConnectToServer, window, cx| this.edit_path(true, window, cx)))
        .on_action(cx.listener(|this, _: &FocusFilter, window, cx| {
            if this.active_folder().is_some() {
                this.filter.update(cx, |filter, cx| {
                    filter.focus(window, cx);
                    filter.select_all(window, cx);
                })
            }
        }))
        .on_action(cx.listener(|this, _: &NewTab, window, cx| {
            let dir = this.current_dir(cx);
            this.open_tab(Tab::Folder(dir), window, cx);
        }))
        .on_action(cx.listener(|this, _: &CloseTab, window, cx| this.close(window, cx)))
        .on_action(cx.listener(|this, _: &ReopenClosedTab, window, cx| this.reopen_closed_tab(window, cx)))
        .on_action(cx.listener(|this, _: &NextTab, window, cx| this.cycle(1, window, cx)))
        .on_action(cx.listener(|this, _: &PrevTab, window, cx| this.cycle(-1, window, cx)))
        .on_action(cx.listener(|this, &ActivateTab(ix): &ActivateTab, window, cx| {
            this.activate(ix.min(this.items.len() - 1), window, cx)
        }))
        .on_action(cx.listener(|this, &ShowPlace(ix): &ShowPlace, window, cx| this.show_place(ix, window, cx)))
        .on_action(cx.listener(|this, _: &NewTerminal, window, cx| {
            let dir = this.current_dir(cx);
            this.open_tab(Tab::Terminal(dir), window, cx);
        }))
        .on_action(cx.listener(|this, _: &ToggleZoom, _, cx| this.toggle_zoom(cx)))
        .on_action(cx.listener(|this, _: &ShowActivity, window, cx| this.open_tab(Tab::Activity, window, cx)))
        .on_action(cx.listener(|this, _: &ShowStorage, window, cx| {
            let dir = this.current_dir(cx);
            this.open_tab(Tab::Storage(dir), window, cx);
        }))
        .on_action(cx.listener(|this, _: &ShowHistory, window, cx| this.open_tab(Tab::History, window, cx)))
        .on_action(|_: &EmptyTrash, window, cx| empty_trash(window, cx))
        .on_action(cx.listener(|this, _: &ShowSettings, window, cx| this.open_tab(Tab::Settings, window, cx)))
        .on_action(cx.listener(|this, _: &ImportFromFinder, window, cx| this.import_from_finder(window, cx)))
        .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.show_sidebar(!this.docks.sidebar, cx)))
        .on_action(cx.listener(|this, _: &TogglePreview, _, cx| this.show_preview(!this.docks.preview, cx)))
        .on_action(cx.listener(|this, _: &FindInFolder, window, cx| {
            let root = this.current_dir(cx);
            this.open_tab(Tab::Search(root), window, cx);
        }))
        .on_action(cx.listener(|this, _: &CommandPalette, window, cx| this.open_palette(window, cx)))
        .on_action(cx.listener(|this, _: &CloseWindow, window, cx| this.close_window(window, cx)))
        .on_action(cx.listener(|this, _: &NewWindow, _, cx| open_window(this.current_dir(cx), cx)))
        .on_action(|_: &Minimize, window, _| window.minimize_window())
        .on_action(cx.listener(|this, _: &AddToShelf, _, cx| this.add_to_shelf(cx)))
        // The terminal's own menu has Show in Files, for the folder its shell is in.
        .when_some(terminal, |div, terminal| {
            div.on_action(
                cx.listener(move |this, _: &ShowInFiles, window, cx| this.show_terminal_folder(&terminal, window, cx)),
            )
        })
    }

    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() {
            return self.close_palette(window, cx);
        }
        // The path editor goes away when it loses focus, so it can't take focus back afterwards.
        if self.path_editor.is_some() {
            self.cancel_path_edit(window, cx);
        }
        let Some(previous) = window.focused(cx) else { return };
        let palette = cx.new(|cx| Palette::new(previous, window, cx));
        let subscription = cx.subscribe_in(&palette, window, |this, _, event, window, cx| {
            this.close_palette(window, cx);
            if let PaletteEvent::Confirmed(action) = event {
                window.dispatch_action(action.boxed_clone(), cx);
            }
        });
        self.palette = Some((palette, subscription));
        cx.notify();
    }

    /// Closes the command palette, handing focus back to where it was.
    pub(super) fn close_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((palette, _)) = self.palette.take() {
            palette.read(cx).previous_focus().clone().focus(window, cx);
        }
        cx.notify();
    }

    fn import_from_finder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let note = match finder::import(cx) {
            Ok(count) => {
                // Finder's panes show in this window too, not only in the windows to come.
                let settings = Settings::get(cx);
                let (sidebar, preview) = (settings.show_sidebar, settings.show_preview);
                self.show_sidebar(sidebar, cx);
                self.show_preview(preview, cx);
                Notification::success(format!(
                    "Brought over {} and how Finder shows folders.",
                    fs::count(count, "favorite", "favorites")
                ))
            }
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                Notification::error("Mu needs Full Disk Access to read Finder's sidebar.")
            }
            Err(_) => Notification::error("Couldn't read Finder's sidebar."),
        };
        window.toast(note, cx);
    }
}
