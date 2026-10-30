//! The pane of tabs: opening, showing and closing them, and doing what they ask.

use std::path::{Path, PathBuf};

use gpui_kit::component::{ActiveTheme as _, v_flex};
use gpui_kit::{prelude::FluentBuilder as _, *};
use mu_terminal::{TerminalEvent, TerminalView};

use super::{
    Item, TabEvent, Workspace, go,
    windows::{confirm_discard, open_window},
};
use crate::{
    browser::Browser,
    drag::{DraggedFiles, DropFiles as _},
    fs::home_dir,
    quicklook,
    session::{self, Tab},
};

impl Workspace {
    /// Shows `tab`: in the tab that shows it already, if there's one, or else in a new one.
    pub(super) fn open_tab(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        match self.items.iter().position(|item| item.shows(&tab, cx)) {
            Some(ix) => {
                self.activate(ix, window, cx);
                // Storage shows one folder at a time, so it goes to this one.
                if let (Item::Storage(storage), Tab::Storage(dir)) = (&self.items[ix], tab) {
                    storage.update(cx, |storage, cx| storage.show(dir, cx));
                }
            }
            None => {
                let item = Item::new(tab, window, cx);
                self.add_item(item, true, window, cx);
            }
        }
        match &self.items[self.items.active()] {
            Item::Search(search) => search.update(cx, |search, cx| search.focus_query(window, cx)),
            // Settings lists the permissions the sheet asks for.
            Item::Settings(_) => self.onboarding = None,
            _ => {}
        }
    }

    /// Shows `tab` as `open_tab` does, unless what it shows is gone, as a tab opened again can be.
    /// Says whether it did.
    pub(super) fn open_tab_again(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let gone = tab.path().is_some_and(|path| !path.exists());
        if !gone {
            self.open_tab(tab, window, cx);
        }
        !gone
    }

    /// Opens a folder in a tab behind the active one, after any others opened from it.
    pub(super) fn open_behind(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let item = Item::new(Tab::Folder(path), window, cx);
        self.add_item(item, false, window, cx);
    }

    /// Opens the last tab closed here where it was, or the one before if that's gone. If another
    /// tab shows it already, that one comes forward where it is.
    pub(super) fn reopen_closed_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        while let Some((ix, tab)) = self.closed.pop() {
            let open = self.items.len();
            if !self.open_tab_again(tab, window, cx) {
                continue;
            }
            if self.items.len() > open {
                self.move_item(self.items.active(), ix, cx);
                self.activate(self.items.active(), window, cx);
            }
            return;
        }
    }

    pub(super) fn active_item(&self) -> Option<&Item> {
        self.items.get(self.items.active())
    }

    pub(super) fn active_folder(&self) -> Option<&Entity<Browser>> {
        match self.active_item() {
            Some(Item::Folder(browser)) => Some(browser),
            _ => None,
        }
    }

    pub(super) fn position(&self, id: EntityId) -> Option<usize> {
        self.items.iter().position(|item| item.id() == id)
    }

    pub(super) fn focus_active(&mut self, window: &mut Window, cx: &mut App) {
        if let Some(item) = self.active_item() {
            item.focus_handle(cx).focus(window, cx);
        }
    }

    /// The folder new tabs, terminals and windows start in: wherever the focused thing is.
    pub(super) fn current_dir(&self, cx: &App) -> PathBuf {
        match self.active_item().map(|item| item.saved(cx)) {
            Some(Tab::Editor(path)) => path.parent().map(Path::to_path_buf).unwrap_or_else(home_dir),
            Some(ref tab) if let Some(path) = tab.path() => path.clone(),
            _ => self.last_folder.as_ref().map(|b| b.read(cx).path().to_path_buf()).unwrap_or_else(home_dir),
        }
    }

    fn add_item(&mut self, item: Item, activate: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.subscribe(&item, window, cx);
        let ix = self.items.insert_behind(item);
        if self.items.len() == 1 || activate {
            self.activate(ix, window, cx);
        }
        cx.notify();
    }

    /// Puts a tab at `ix`, keeping the active one active.
    pub(super) fn insert_item(&mut self, ix: usize, item: Item, window: &mut Window, cx: &mut Context<Self>) {
        self.subscribe(&item, window, cx);
        self.items.insert(ix, item);
        cx.notify();
    }

    /// Listens to what `item` asks of the window, while it's in this one.
    fn subscribe(&mut self, item: &Item, window: &mut Window, cx: &mut Context<Self>) {
        let subscription = match item {
            Item::Folder(browser) => cx.subscribe_in(browser, window, Self::tab_event),
            Item::Terminal(terminal) => cx.subscribe_in(terminal, window, Self::terminal_event),
            Item::Editor(editor) => cx.subscribe_in(editor, window, Self::tab_event),
            Item::Search(search) => cx.subscribe_in(search, window, Self::tab_event),
            Item::Activity(activity) => cx.subscribe_in(activity, window, Self::tab_event),
            Item::Storage(storage) => cx.subscribe_in(storage, window, Self::tab_event),
            Item::History(page) => cx.subscribe_in(page, window, Self::tab_event),
            Item::Settings(page) => cx.subscribe_in(page, window, Self::tab_event),
        };
        self.tab_subscriptions.insert(item.id(), subscription);
    }

    /// Does what a tab asks.
    fn tab_event<T: 'static>(
        &mut self,
        tab: &Entity<T>,
        event: &TabEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TabEvent::Navigated(dir) => {
                go::visit(dir, cx);
                if self.active_item().is_some_and(|item| item.id() == tab.entity_id()) {
                    self.path_editor = None;
                    self.reset_filter(window, cx);
                    self.update_title(window, cx);
                }
                cx.notify();
            }
            TabEvent::Changed => {
                if self.active_item().is_some_and(|item| item.id() == tab.entity_id()) {
                    self.update_title(window, cx);
                }
                cx.notify();
            }
            TabEvent::OpenTab(path) => self.open_behind(path.clone(), window, cx),
            TabEvent::OpenWindow(path) => open_window(path.clone(), cx),
            TabEvent::OpenTerminal(dir) => self.open_tab(Tab::Terminal(dir.clone()), window, cx),
            TabEvent::Edit { path, line } => {
                self.open_tab(Tab::Editor(path.clone()), window, cx);
                if let (Some(line), Some(Item::Editor(editor))) = (line, self.active_item()) {
                    editor.update(cx, |editor, cx| editor.go_to_line(*line, window, cx));
                }
            }
            TabEvent::Reveal(paths) => self.reveal(paths.clone(), window, cx),
            TabEvent::ShowFolder(dir) => self.show_folder(dir.clone(), window, cx),
        }
    }

    pub(super) fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.select(ix, window, cx);
        self.focus_active(window, cx);
    }

    /// Makes a tab the active one without moving focus to it.
    fn select(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.items.select(ix);
        if let Some(browser) = self.active_folder().cloned() {
            self.preview.update(cx, |preview, cx| preview.follow(&browser, cx));
            self.last_folder = Some(browser);
        }
        for (ix, item) in self.items.iter().enumerate() {
            if let Item::Activity(activity) = item {
                activity.update(cx, |activity, cx| activity.set_shown(ix == self.items.active(), cx));
            }
        }
        self.path_editor = None;
        self.reset_filter(window, cx);
        self.update_title(window, cx);
        cx.notify();
    }

    pub(super) fn cycle(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let len = self.items.len() as isize;
        self.activate((self.items.active() as isize + delta).rem_euclid(len) as usize, window, cx);
    }

    /// Closes a tab, asking first when it's an editor with unsaved changes.
    pub(super) fn close_item(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let editor = match self.position(id).map(|ix| &self.items[ix]) {
            Some(Item::Editor(editor)) if editor.read(cx).is_dirty() => editor.clone(),
            _ => return self.remove_item(id, window, cx),
        };
        let confirm = confirm_discard(vec![editor], window, cx);
        cx.spawn_in(window, async move |this, cx| {
            if confirm.await {
                this.update_in(cx, |this, window, cx| this.remove_item(id, window, cx)).ok();
            }
        })
        .detach();
    }

    fn remove_item(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.position(id) else { return };
        // Its last tab closed, the window has nothing to open again.
        if self.items.len() == 1 && cx.windows().len() == 1 {
            session::save(&[]);
        }
        self.closed.push((ix, self.items[ix].saved(cx)));
        self.take_item(ix, window, cx);
    }

    /// Takes a tab out, as it closes or moves to another window. The window closes with its last.
    pub(super) fn take_item(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) -> Item {
        // A tab can close in the background, like a terminal tab whose shell exited.
        let (was_active, had_focus) =
            (ix == self.items.active(), self.items[ix].focus_handle(cx).contains_focused(window, cx));
        let item = self.items.remove(ix);
        self.tab_subscriptions.remove(&item.id());
        if self.items.is_empty() {
            window.remove_window();
            return item;
        }
        if let Item::Folder(browser) = &item
            && self.last_folder.as_ref() == Some(browser)
        {
            self.last_folder = None;
        }
        if was_active {
            self.select(self.items.active(), window, cx);
        }
        if had_focus {
            self.focus_active(window, cx);
        }
        cx.notify();
        item
    }

    pub(super) fn close_others(&mut self, keep: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let others: Vec<EntityId> = self.items.iter().map(Item::id).filter(|id| *id != keep).collect();
        for id in others {
            self.close_item(id, window, cx);
        }
    }

    /// Puts the active tab's title on the window, and brings the tab into view, as its width
    /// follows its title.
    fn update_title(&self, window: &mut Window, cx: &App) {
        let title = self.active_item().map(|item| item.title(cx)).unwrap_or_else(|| "Mu".into());
        // AppKit takes a millisecond or more to set a title, so it's set the frame after the change
        // draws, rather than before.
        window.on_next_frame(move |window, _| window.on_next_frame(move |window, _| window.set_window_title(&title)));
        self.tabs_scroll.scroll_to_item(self.items.active());
    }

    /// The folder tab to show a place in: the active one, else the last one shown.
    pub(super) fn folder_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<Browser>> {
        if let Some(browser) = self.active_folder() {
            return Some(browser.clone());
        }
        let browser = self.last_folder.clone()?;
        let ix = self.position(browser.entity_id())?;
        self.activate(ix, window, cx);
        Some(browser)
    }

    /// Gets the folder at `path` ready for `show_folder`.
    pub(super) fn prefetch(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(browser) = self.active_folder().or(self.last_folder.as_ref()) {
            browser.clone().update(cx, |browser, cx| browser.prefetch(path, window, cx));
        }
    }

    /// Shows a folder's contents.
    pub(super) fn show_folder(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        match self.folder_tab(window, cx) {
            Some(browser) => {
                browser.update(cx, |browser, cx| browser.navigate(path, window, cx));
                self.focus_active(window, cx);
            }
            None => self.open_tab(Tab::Folder(path), window, cx),
        }
    }

    /// Shows files or folders, all in one folder, selected there.
    pub(super) fn reveal(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        if self.folder_tab(window, cx).is_none() {
            let dir = paths.first().and_then(|path| path.parent()).map(Path::to_path_buf).unwrap_or_else(home_dir);
            self.open_tab(Tab::Folder(dir), window, cx);
        }
        let Some(browser) = self.active_folder().cloned() else { return };
        browser.update(cx, |browser, cx| browser.reveal(paths, window, cx));
        self.focus_active(window, cx);
    }

    /// Does what a terminal asks, which as a crate of its own has events of its own.
    fn terminal_event(
        &mut self,
        terminal: &Entity<TerminalView>,
        event: &TerminalEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TerminalEvent::TitleChanged => self.tab_event(terminal, &TabEvent::Changed, window, cx),
            TerminalEvent::Exited => {
                self.remove_item(terminal.entity_id(), window, cx);
            }
        }
    }

    /// Shows the folder a terminal's shell is in.
    pub(super) fn show_terminal_folder(
        &mut self,
        terminal: &Entity<TerminalView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let cwd = terminal.read(cx).cwd();
        self.show_folder(cwd, window, cx);
    }

    pub(super) fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !quicklook::close() {
            self.close_item(self.items[self.items.active()].id(), window, cx);
        }
    }

    /// A terminal, which takes dropped files as their paths.
    fn render_terminal(&self, terminal: &Entity<TerminalView>, cx: &mut Context<Self>) -> impl IntoElement {
        let dropped = terminal.clone();
        div()
            .id(("terminal-drop", terminal.entity_id()))
            .size_full()
            .pl_2()
            .pt_1()
            .bg(cx.theme().background)
            .border_1()
            .border_color(transparent_black())
            .drag_over::<DraggedFiles>(|style, _, _, cx| style.border_color(cx.theme().ring))
            .drag_over::<ExternalPaths>(|style, _, _, cx| style.border_color(cx.theme().ring))
            .on_drop_files(move |paths, window, cx| paste_paths(&dropped, paths, window, cx))
            .child(terminal.clone())
    }

    pub(super) fn render_center(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let content = match &self.items[self.items.active()] {
            Item::Terminal(terminal) => self.render_terminal(terminal, cx).into_any_element(),
            // Cached: a frame for something else, like the status bar's meters or a blinking cursor
            // in the filter, reuses what the tab drew last.
            item => item.view().cached(StyleRefinement::default().size_full()).into_any_element(),
        };
        v_flex()
            .size_full()
            .child(self.render_tab_bar(cx))
            .when_some(self.active_folder().cloned(), |el, browser| el.child(self.render_toolbar(&browser, window, cx)))
            .child(div().flex_1().min_h_0().child(content))
            .into_any_element()
    }

    pub(super) fn with_folder(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Browser, &mut Context<Browser>)) {
        if let Some(browser) = self.active_folder().cloned() {
            browser.update(cx, f);
        }
    }
}

/// Pastes paths into a terminal, quoted for the shell.
pub(super) fn paste_paths(terminal: &Entity<TerminalView>, paths: &[PathBuf], window: &mut Window, cx: &mut App) {
    let text = paths.iter().map(|path| shell_quote(path)).collect::<Vec<_>>().join(" ") + " ";
    terminal.update(cx, |terminal, cx| terminal.send_text(&text, cx));
    terminal.read(cx).focus_handle(cx).focus(window, cx);
}

fn shell_quote(path: &Path) -> String {
    let text = path.to_string_lossy();
    if text.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+@%:,=~".contains(c)) {
        text.into_owned()
    } else {
        format!("'{}'", text.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::shell_quote;

    #[test]
    fn shell_quote_only_when_needed() {
        assert_eq!(shell_quote(Path::new("/tmp/plain-name_1.txt")), "/tmp/plain-name_1.txt");
        assert_eq!(shell_quote(Path::new("/tmp/Tax Documents")), "'/tmp/Tax Documents'");
        assert_eq!(shell_quote(Path::new("/tmp/it's")), r"'/tmp/it'\''s'");
    }
}
