//! Undo and redo for file operations: one history for every window, like Finder's. Undoing never
//! deletes anything: what an operation made goes to the Trash, and redoing puts it back from there.
//! Besides the latest change, any change can be taken back on its own, from the Undo History tab.

pub mod page;

use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};

use gpui_kit::assets::IconName;
use gpui_kit::component::{WindowExt as _, button::Button, notification::Notification};
use gpui_kit::*;

use crate::{
    fs::{self, Cancelled, Undo, shown_path},
    operations,
    theme::Toasts as _,
};

/// A file operation that can be taken back.
#[derive(Clone, Copy)]
pub enum Op {
    Trash,
    PutBack,
    Rename,
    Move,
    Copy,
    Duplicate,
    Compress,
    Extract,
    NewFolder,
    Paste,
}

impl Op {
    /// What the menu calls it, as in "Undo Rename".
    pub fn name(self) -> &'static str {
        match self {
            Op::Trash => "Move to Trash",
            Op::PutBack => "Put Back",
            Op::Rename => "Rename",
            Op::Move => "Move",
            Op::Copy => "Copy",
            Op::Duplicate => "Duplicate",
            Op::Compress => "Compress",
            Op::Extract => "Extract",
            Op::NewFolder => "New Folder",
            Op::Paste => "Paste",
        }
    }

    /// What the status bar says while it copies.
    pub fn verb(self) -> &'static str {
        match self {
            Op::Move => "Moving",
            _ => "Copying",
        }
    }

    pub fn icon(self) -> IconName {
        match self {
            Op::Trash => IconName::Trash,
            Op::PutBack => IconName::Undo2,
            Op::Rename => IconName::Pencil,
            Op::Move => IconName::FolderInput,
            Op::Copy => IconName::Copy,
            Op::Duplicate => IconName::CopyPlus,
            Op::Compress => IconName::FileArchive,
            Op::Extract => IconName::ArchiveRestore,
            Op::NewFolder => IconName::FolderPlus,
            Op::Paste => IconName::ClipboardPaste,
        }
    }
}

/// A file operation that went through.
pub struct Change {
    pub id: usize,
    pub op: Op,
    /// The items it made or moved, by name.
    pub detail: String,
    /// The folder it happened in.
    pub place: String,
    pub time: SystemTime,
    pub undone: bool,
    /// Takes it back, or once it's undone, does it again. Out while that runs.
    action: Option<Undo>,
    /// Every path the last time it was done, undone or redone moved anything from or to.
    touched: Vec<PathBuf>,
}

impl Change {
    fn new(id: usize, op: Op, undo: Undo) -> Self {
        let (detail, place) = describe(op, &undo);
        let touched = footprint(&undo);
        Self { id, op, detail, place, time: SystemTime::now(), undone: false, action: Some(undo), touched }
    }

    pub fn running(&self) -> bool {
        self.action.is_none()
    }

    /// Where its items are now.
    pub fn current(&self) -> Vec<PathBuf> {
        self.action.iter().flat_map(items).map(|(now, _)| now.to_path_buf()).collect()
    }

    /// Whether the last time it was done, undone or redone, it moved anything in `dir`, into it or out
    /// of it, or moved `dir` itself along with a folder it's in.
    pub fn moved_in(&self, dir: &Path) -> bool {
        self.touched.iter().any(|path| path.starts_with(dir) || dir.starts_with(path))
    }
}

/// Every path `undo` moves anything from or to, what it puts aside included.
fn footprint(undo: &Undo) -> Vec<PathBuf> {
    match undo {
        Undo::Move(moves) | Undo::PutBack(moves) => {
            moves.iter().flat_map(|(from, to)| [from.clone(), to.clone()]).collect()
        }
        Undo::Trash(made) => made.clone(),
        Undo::All(steps) => steps.iter().flat_map(footprint).collect(),
        Undo::Aside(step) => footprint(step),
    }
}

/// Each item `undo` moves: where it is now and where it goes back to. Items an operation made go
/// back to where they are.
fn items(undo: &Undo) -> Vec<(&Path, &Path)> {
    match undo {
        Undo::Move(moves) | Undo::PutBack(moves) => {
            moves.iter().map(|(now, was)| (now.as_path(), was.as_path())).collect()
        }
        Undo::Trash(made) => made.iter().map(|path| (path.as_path(), path.as_path())).collect(),
        Undo::All(steps) => steps.iter().flat_map(items).collect(),
        Undo::Aside(_) => Vec::new(),
    }
}

/// Names what `op` made or moved, and where, from how to take it back.
fn describe(op: Op, undo: &Undo) -> (String, String) {
    let items = items(undo);
    let names = items.iter().map(|(_, was)| fs::display_name(was)).collect::<Vec<_>>().join(", ");
    let folder = |path: &Path| shown_path(path.parent().unwrap_or(path));
    let Some(&(now, was)) = items.first() else { return (names, String::new()) };
    match op {
        Op::Rename => (format!("{names} → {}", fs::display_name(now)), folder(was)),
        Op::Move => (names, format!("{} → {}", folder(was), folder(now))),
        _ => (names, folder(was)),
    }
}

#[derive(Default)]
pub struct History {
    /// Oldest first, each at its id.
    changes: Vec<Change>,
    /// Changes undone since the last new one, latest last: what Redo does again, in turn.
    redo: Vec<usize>,
    /// Each change undone or redone, once for every time, in turn.
    toggled: Vec<usize>,
    /// Set while a change is taken back or done again, so that happens one at a time.
    busy: bool,
}

impl Global for History {}

impl History {
    /// The latest change that isn't undone, which Undo takes back.
    fn latest(&self) -> Option<&Change> {
        self.changes.iter().rev().find(|change| !change.undone)
    }
}

/// The toast for the latest change, which the next one replaces.
struct Toast;

pub fn init(cx: &mut App) {
    cx.set_global(History::default());
    page::init(cx);
}

pub fn changes(cx: &App) -> &[Change] {
    &cx.global::<History>().changes
}

/// Each change undone or redone, by id, once for every time, in turn.
pub fn toggled(cx: &App) -> &[usize] {
    &cx.global::<History>().toggled
}

pub fn busy(cx: &App) -> bool {
    cx.global::<History>().busy
}

/// What Undo and Redo would take back or do again, for the menu.
pub fn next(cx: &App) -> (Option<&'static str>, Option<&'static str>) {
    let history = cx.global::<History>();
    let undo = history.latest().map(|change| change.op.name());
    let redo = history.redo.last().map(|&id| history.changes[id].op.name());
    (undo, redo)
}

/// Remembers how to take back `op`, which was just done. Moving to the Trash says so, with a way
/// back.
pub fn record(op: Op, undo: Option<Undo>, window: &mut Window, cx: &mut App) {
    let Some(undo) = undo else { return };
    let message = match &undo {
        Undo::PutBack(moves) => Some(match moves.as_slice() {
            [(_, original)] => format!("Moved “{}” to the Trash", fs::display_name(original)),
            moves => format!("Moved {} items to the Trash", moves.len()),
        }),
        _ => None,
    };
    let history = cx.global_mut::<History>();
    let id = history.changes.len();
    history.changes.push(Change::new(id, op, undo));
    history.redo.clear();
    match message {
        Some(message) => toast(message, false, id, window, cx),
        None => window.remove_notification::<Toast>(cx),
    }
}

/// Undoes the latest change, or redoes the latest undone one. Resolves to the items it moved
/// back, to select.
pub fn step(redo: bool, window: &mut Window, cx: &mut App) -> Task<Vec<PathBuf>> {
    let history = cx.global::<History>();
    let id = if redo { history.redo.last().copied() } else { history.latest().map(|change| change.id) };
    match id {
        Some(id) => toggle(id, window, cx),
        None => Task::ready(Vec::new()),
    }
}

/// Undoes change `id`, or redoes it if it's undone, unless a change is already being taken back.
/// Resolves to the items it moved back, to select.
pub fn toggle(id: usize, window: &mut Window, cx: &mut App) -> Task<Vec<PathBuf>> {
    let history = cx.global_mut::<History>();
    let count = history.changes.len();
    if history.busy {
        return Task::ready(Vec::new());
    }
    let Some(action) = history.changes.get_mut(id).and_then(|change| change.action.take()) else {
        return Task::ready(Vec::new());
    };
    let change = &history.changes[id];
    let (op, redo) = (change.op, change.undone);
    history.busy = true;
    let verb = if redo { "Redoing" } else { "Undoing" };
    let task = operations::track(
        verb,
        move |progress| {
            let done = action.apply(progress);
            (action, done)
        },
        cx,
    );
    let window = window.window_handle();
    cx.spawn(async move |cx| {
        let (action, mut done) = task.await;
        let toggled = cx.update(|cx| {
            let history = cx.global_mut::<History>();
            history.busy = false;
            let change = &mut history.changes[id];
            let toggled = match done.undo.take() {
                Some(back) => {
                    change.touched = [footprint(&action), footprint(&back)].concat();
                    change.action = Some(back);
                    change.undone = !redo;
                    true
                }
                // Nothing went through, so it's as it was and can be tried again.
                None => {
                    change.action = Some(action);
                    false
                }
            };
            if toggled {
                history.toggled.push(id);
            }
            if toggled && redo {
                history.redo.retain(|&undone| undone != id);
            } else if toggled && history.changes.len() == count {
                // A change made in the meantime would have left nothing to redo, this included.
                history.redo.push(id);
            }
            toggled
        });
        window
            .update(cx, |_, window, cx| match (&done.error, toggled) {
                (Some(error), _) if !error.is::<Cancelled>() => window.toast_error(error, cx),
                (_, true) => {
                    let verb = if redo { "Redid" } else { "Undid" };
                    toast(format!("{verb} {}", op.name()), !redo, id, window, cx)
                }
                _ => {}
            })
            .ok();
        done.paths
    })
}

/// Shows `message` with a button that undoes change `id`, or with `redo`, redoes it. The button
/// does nothing once the change has been taken back (or done again) some other way.
fn toast(message: String, redo: bool, id: usize, window: &mut Window, cx: &mut App) {
    let note = Notification::new().message(message).id::<Toast>().action(move |_, _, _| {
        Button::new("take-back").label(if redo { "Redo" } else { "Undo" }).on_click(move |_, window, cx| {
            if cx.global::<History>().changes[id].undone == redo {
                toggle(id, window, cx).detach();
            }
        })
    });
    window.toast(note.autohide(true), cx);
}

#[cfg(test)]
mod tests {
    use super::{Change, Op, Undo, describe};

    #[test]
    fn describes_what_changed_and_where() {
        let home = crate::fs::home_dir();
        let docs = home.join("Documents");
        let (a, b) = (docs.join("a.txt"), docs.join("b.txt"));
        let trash = home.join(".Trash");

        let renamed = Undo::Move(vec![(b.clone(), a.clone())]);
        assert_eq!(describe(Op::Rename, &renamed), ("a.txt → b.txt".into(), "~/Documents".into()));

        let moved = Undo::Move(vec![(home.join("Desktop/a.txt"), a.clone()), (home.join("Desktop/b.txt"), b.clone())]);
        assert_eq!(describe(Op::Move, &moved), ("a.txt, b.txt".into(), "~/Documents → ~/Desktop".into()));

        let trashed = Undo::PutBack(vec![(trash.join("a.txt"), a.clone())]);
        assert_eq!(describe(Op::Trash, &trashed), ("a.txt".into(), "~/Documents".into()));

        let duplicated = Undo::Trash(vec![docs.join("a copy.txt")]);
        assert_eq!(describe(Op::Duplicate, &duplicated), ("a copy.txt".into(), "~/Documents".into()));

        // What it replaced, put aside in the Trash, isn't what it's about, before or after an undo.
        let replaced = |step| Undo::All(vec![step, Undo::Aside(Box::new(Undo::Trash(vec![b.clone()])))]);
        let copied = replaced(Undo::Trash(vec![a.clone()]));
        assert_eq!(describe(Op::Copy, &copied), ("a.txt".into(), "~/Documents".into()));
        let undone = replaced(Undo::PutBack(vec![(trash.join("a.txt"), a.clone())]));
        assert_eq!(describe(Op::Copy, &undone), ("a.txt".into(), "~/Documents".into()));
    }

    #[test]
    fn knows_which_folders_it_moved_items_in_and_out_of() {
        let home = crate::fs::home_dir();
        let (docs, desktop) = (home.join("Documents"), home.join("Desktop"));
        let moved = Change::new(0, Op::Move, Undo::Move(vec![(desktop.join("a.txt"), docs.join("a.txt"))]));
        assert!(moved.moved_in(&docs) && moved.moved_in(&desktop) && moved.moved_in(&home));
        assert!(moved.moved_in(&docs.join("a.txt/inside")));
        assert!(!moved.moved_in(&home.join("Downloads")) && !moved.moved_in(&home.join("Doc")));
    }
}
