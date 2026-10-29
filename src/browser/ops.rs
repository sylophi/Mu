//! Opening items, and the file operations started from a folder, like paste, rename and trash.

use std::{collections::HashSet, path::PathBuf};

use gpui_kit::component::input::{InputEvent, InputState};
use gpui_kit::*;

use super::Browser;
use crate::{
    drag,
    fs::{self, Choices, Done, Entry, Progress},
    history::{self, Op},
    operations, pasteboard,
    settings::Settings,
    theme::Toasts as _,
    workspace::TabEvent,
};

/// Files cut with ⌘X, shown dimmed until they're pasted somewhere. They only count as cut while
/// the pasteboard still holds them, which `change_count` tells.
#[derive(Default)]
pub(super) struct CutFiles {
    paths: HashSet<PathBuf>,
    change_count: isize,
}

impl CutFiles {
    pub(super) fn current(cx: &App) -> Option<&HashSet<PathBuf>> {
        let cut = cx.global::<Self>();
        (!cut.paths.is_empty() && cut.change_count == pasteboard::change_count()).then_some(&cut.paths)
    }
}

impl Global for CutFiles {}

pub(super) struct RenameState {
    pub(super) name: SharedString,
    path: PathBuf,
    pub(super) input: Entity<InputState>,
    _subscription: Subscription,
}

impl Browser {
    pub(super) fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let targets: Vec<Entry> = self.selected_entries().into_iter().cloned().collect();
        match targets.as_slice() {
            [] => {}
            [entry] if entry.folder => self.navigate(entry.target().to_path_buf(), window, cx),
            targets => {
                let (folders, files): (Vec<_>, Vec<_>) =
                    targets.iter().map(|entry| (entry.folder, entry.target().to_path_buf())).partition(|(dir, _)| *dir);
                for (_, folder) in folders {
                    cx.emit(TabEvent::OpenTab(folder));
                }
                self.open_files(files.into_iter().map(|(_, file)| file).collect(), targets.len() > 1, window, cx);
            }
        }
    }

    /// Opens `files` here as a double-click does: a disk image shows what's on it, in a tab of its
    /// own with `tabs`. Settings can have an archive extracted and plain text opened in the editor.
    /// Anything else opens in its app.
    pub fn open_files(&mut self, files: Vec<PathBuf>, tabs: bool, window: &mut Window, cx: &mut Context<Self>) {
        let settings = Settings::get(cx);
        let (edit, extract) = (settings.edit_text, settings.extract_archives);
        let (archives, files): (Vec<_>, Vec<_>) = files.into_iter().partition(|file| extract && fs::is_archive(file));
        for file in files {
            if fs::is_disk_image(&file) {
                self.mount(file, tabs, window, cx);
            } else if edit && fs::is_plain_text(&file) {
                cx.emit(TabEvent::Edit { path: file, line: None });
            } else {
                cx.open_with_system(&file);
            }
        }
        if !archives.is_empty() {
            let dir = self.path.clone();
            self.run(Op::Extract, move |_| fs::extract(&archives, &dir), false, window, cx).detach();
        }
    }

    /// Mounts the disk image at `path`, then shows what's on it, in a new tab when `tab`.
    fn mount(&mut self, path: PathBuf, tab: bool, window: &mut Window, cx: &mut Context<Self>) {
        let mount = cx.background_spawn(async move { fs::mount_image(&path) });
        cx.spawn_in(window, async move |this, cx| {
            let volume = mount.await;
            this.update_in(cx, |this, window, cx| match volume {
                Ok(volume) if tab => cx.emit(TabEvent::OpenTab(volume)),
                Ok(volume) => this.navigate(volume, window, cx),
                Err(error) => window.toast_error(&error, cx),
            })
            .ok();
        })
        .detach();
    }

    pub(super) fn open_in_new_tab(&mut self, cx: &mut Context<Self>) {
        for entry in self.selected_entries() {
            if entry.folder {
                cx.emit(TabEvent::OpenTab(entry.target().to_path_buf()));
            }
        }
    }

    pub(super) fn open_in_new_window(&mut self, cx: &mut Context<Self>) {
        for entry in self.selected_entries() {
            if entry.folder {
                cx.emit(TabEvent::OpenWindow(entry.target().to_path_buf()));
            }
        }
    }

    pub(super) fn show_package_contents(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(entry) = self.cursor_entry().filter(|e| e.package) {
            self.navigate(entry.target().to_path_buf(), window, cx);
        }
    }

    pub(super) fn open_in_editor(&mut self, cx: &mut Context<Self>) {
        for entry in self.selected_entries() {
            if !entry.folder && !entry.package {
                cx.emit(TabEvent::Edit { path: entry.target().to_path_buf(), line: None });
            }
        }
    }

    pub(super) fn open_in_terminal(&mut self, cx: &mut Context<Self>) {
        let dir = match self.cursor_entry().filter(|e| e.folder && self.selection.contains(&e.name)) {
            Some(entry) => entry.target().to_path_buf(),
            None => self.path.clone(),
        };
        cx.emit(TabEvent::OpenTerminal(dir));
    }

    // Operations

    /// Runs file operation `op` (see `operations::run`), then reloads and selects what it made or
    /// moved. The task resolves to whether all of it went through.
    fn run(
        &mut self,
        op: Op,
        work: impl FnOnce(&Progress) -> Done + Send + 'static,
        rename: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        let task = operations::run(op, work, window, cx);
        let task = cx.spawn(async move |_, _| Some(task.await));
        self.settle(task, rename, window, cx)
    }

    /// Copies or moves `paths` into folder `dir`, or moves them to the Trash (see
    /// `operations::transfer`), then selects them where they went.
    fn transfer(
        &mut self,
        op: Op,
        paths: Vec<PathBuf>,
        dir: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        let task = operations::transfer(op, paths, dir, window, cx);
        self.settle(task, false, window, cx)
    }

    /// Waits for a file operation, then reloads and selects what it made or moved, and with
    /// `rename`, starts renaming it. Resolves to whether all of it went through.
    fn settle(
        &mut self,
        task: Task<Option<Done>>,
        rename: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Task<bool> {
        cx.spawn_in(window, async move |this, cx| {
            let Some(done) = task.await else { return false };
            this.update_in(cx, |this, window, cx| {
                this.reload_selecting(done.paths, rename, window, cx);
                done.error.is_none()
            })
            .unwrap_or(false)
        })
    }

    /// Undoes (or redoes) the latest file operation, then selects what came back here.
    pub(super) fn take_back(&mut self, redo: bool, window: &mut Window, cx: &mut Context<Self>) {
        let task = history::step(redo, window, cx);
        cx.spawn_in(window, async move |this, cx| {
            let paths = task.await;
            this.update_in(cx, |this, window, cx| this.reload_selecting(paths, false, window, cx)).ok();
        })
        .detach();
    }

    pub(super) fn drop_paths(
        &mut self,
        paths: &[PathBuf],
        target: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let task = drag::drop_into(paths, target, window, cx);
        self.settle(task, false, window, cx).detach();
    }

    pub fn in_trash(&self) -> bool {
        self.path == fs::trash_dir()
    }

    /// Moves the selection to the Trash, or in the Trash, puts it back like Finder.
    pub fn trash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        if self.in_trash() {
            return self.put_back(window, cx);
        }
        // Like Explorer, the cursor lands on whatever follows the trashed items.
        let after = self
            .listing
            .iter()
            .skip_while(|e| !self.selection.contains(&e.name))
            .find(|e| !self.selection.contains(&e.name))
            .or_else(|| self.listing.iter().rev().find(|e| !self.selection.contains(&e.name)))
            .map(|e| e.path.clone());
        let task = operations::trash(paths, window, cx);
        let task = cx.spawn(async move |_, _| Some(Done { paths: after.into_iter().collect(), ..task.await? }));
        self.settle(task, false, window, cx).detach();
    }

    pub(super) fn put_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = self.selected_paths();
        if !paths.is_empty() {
            self.run(Op::PutBack, move |_| fs::put_back(&paths), false, window, cx).detach();
        }
    }

    pub(super) fn copy(&mut self, cut: bool, cx: &mut Context<Self>) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        let change_count = pasteboard::write_files(&paths);
        *cx.global_mut::<CutFiles>() =
            if cut { CutFiles { paths: paths.into_iter().collect(), change_count } } else { CutFiles::default() };
        cx.notify();
    }

    pub(super) fn paste(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = pasteboard::files();
        if paths.is_empty() {
            return self.paste_contents(window, cx);
        }
        let is_cut = CutFiles::current(cx).is_some_and(|cut| paths.iter().all(|path| cut.contains(path)));
        let op = if is_cut { Op::Move } else { Op::Copy };
        let task = self.transfer(op, paths, self.path.clone(), window, cx);
        cx.spawn(async move |_, cx| {
            if task.await && is_cut {
                cx.update(|cx| *cx.global_mut::<CutFiles>() = CutFiles::default());
            }
        })
        .detach();
    }

    /// Pastes a picture or text copied anywhere as a new file, to name right away.
    fn paste_contents(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(contents) = pasteboard::contents() else { return };
        let dir = self.path.clone();
        let paste = move |_: &Progress| {
            let (name, bytes) = contents.into_file();
            fs::new_file(&dir, name, &bytes)
        };
        self.run(Op::Paste, paste, true, window, cx).detach();
    }

    pub(super) fn duplicate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = self.selected_paths();
        if !paths.is_empty() {
            let dir = self.path.clone();
            let duplicate = move |progress: &Progress| fs::copy_into(&paths, &dir, &Choices::new(), progress);
            self.run(Op::Duplicate, duplicate, false, window, cx).detach();
        }
    }

    pub(super) fn compress(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = self.selected_paths();
        if !paths.is_empty() {
            let dir = self.path.clone();
            self.run(Op::Compress, move |_| fs::compress(&paths, &dir), false, window, cx).detach();
        }
    }

    pub(super) fn new_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let dir = self.path.clone();
        self.run(Op::NewFolder, move |_| fs::new_folder(&dir), true, window, cx).detach();
    }

    pub(super) fn copy_path(&mut self, cx: &mut Context<Self>) {
        let paths = self.selected_paths();
        let paths = if paths.is_empty() { vec![self.path.clone()] } else { paths };
        let text = paths.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>().join("\n");
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    pub(super) fn add_to_sidebar(&mut self, cx: &mut Context<Self>) {
        let dirs: Vec<PathBuf> = match self.selected_entries().as_slice() {
            [] => vec![self.path.clone()],
            entries => entries.iter().filter(|e| e.folder).map(|e| e.target().to_path_buf()).collect(),
        };
        Settings::update(cx, |settings| {
            for dir in dirs {
                if !settings.is_favorite(&dir) {
                    settings.favorites.push(dir);
                }
            }
        });
    }

    // Renaming

    pub(super) fn start_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.cursor_entry() else { return };
        let (name, path) = (entry.name.clone(), entry.path.clone());
        let stem_len = match name.rsplit_once('.') {
            Some((stem, _)) if !stem.is_empty() && !entry.folder => stem.len(),
            _ => name.len(),
        };
        self.selection.names = HashSet::from([name.clone()]);
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name.clone()));
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.set_selected_range(0..stem_len, cx);
        });
        let subscription = cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
            InputEvent::PressEnter { .. } | InputEvent::Blur => this.commit_rename(window, cx),
            _ => {}
        });
        self.rename = Some(RenameState { name, path, input, _subscription: subscription });
        if let Some(ix) = self.cursor_ix() {
            self.scroll_to(ix, ScrollStrategy::Nearest, cx);
        }
        cx.notify();
    }

    pub(super) fn commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rename) = self.rename.take() else { return };
        let new_name = rename.input.read(cx).value().trim().to_string();
        // After Enter the list takes focus back. After a click elsewhere, the click decides.
        if rename.input.focus_handle(cx).is_focused(window) {
            self.focus_handle.focus(window, cx);
        }
        if new_name != rename.name.as_ref() {
            let path = rename.path;
            self.run(Op::Rename, move |_| fs::rename(&path, &new_name), false, window, cx).detach();
        }
        cx.notify();
    }

    pub(super) fn cancel_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.rename = None;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }
}
