//! Undo and redo for file operations: one history for every window, like Finder's. What an
//! operation made goes to the Trash when it's undone, and redoing puts it back from there. On a
//! volume without a Trash, undoing warns and deletes it instead, which can't be redone.
//! Besides the latest change, any change can be taken back on its own, from the Undo History tab.
//! The latest changes are saved as JSON in Application Support after every one, so quitting doesn't
//! lose them.

pub mod page;

use std::{
    path::{Path, PathBuf},
    time::SystemTime,
};

use gpui_kit::assets::IconName;
use gpui_kit::component::{WindowExt as _, button::Button, notification::Notification};
use gpui_kit::*;
use serde::{Deserialize, Serialize};

use crate::{
    fs::{self, Cancelled, Undo, shown_path},
    operations, settings,
    theme::Toasts as _,
};

/// How many changes are kept, the latest ones.
const REMEMBERED: usize = 500;

/// A file operation that can be taken back.
#[derive(Clone, Copy, Serialize, Deserialize)]
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
#[derive(Serialize, Deserialize)]
pub struct Change {
    pub id: usize,
    pub op: Op,
    /// The items it made or moved, by name.
    pub detail: String,
    /// The folder it happened in.
    pub place: String,
    pub time: SystemTime,
    pub undone: bool,
    /// Takes it back, or once it's undone, does it again.
    action: Undo,
    /// Every path the last time it was done, undone or redone moved anything from or to. Only asked
    /// about right after an undo or redo, so it isn't saved.
    #[serde(skip)]
    touched: Vec<PathBuf>,
}

impl Change {
    fn new(id: usize, op: Op, undo: Undo) -> Self {
        let (detail, place) = describe(op, &undo);
        let touched = footprint(&undo);
        Self { id, op, detail, place, time: SystemTime::now(), undone: false, action: undo, touched }
    }

    /// Whether undoing it deleted its items for good, so there's nothing to redo.
    pub fn gone(&self) -> bool {
        self.action == Undo::Gone
    }

    /// Where its items are now.
    pub fn current(&self) -> Vec<PathBuf> {
        items(&self.action).into_iter().map(|(now, _)| now.to_path_buf()).collect()
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
        Undo::Gone => Vec::new(),
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
        Undo::Aside(_) | Undo::Gone => Vec::new(),
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

#[derive(Default, Serialize, Deserialize)]
pub struct History {
    /// The id of the oldest change kept. Ids count on from there, and aren't used again once their
    /// change is let go, so a toast's button can't take back some other change.
    first: usize,
    /// Oldest first.
    changes: Vec<Change>,
    /// Changes undone since the last new one, latest last: what Redo does again, in turn.
    redo: Vec<usize>,
    /// Each change undone or redone since launch, once for every time, in turn.
    #[serde(skip)]
    toggled: Vec<usize>,
    /// The change being taken back or done again, so that happens one at a time.
    #[serde(skip)]
    running: Option<usize>,
}

impl Global for History {}

impl History {
    /// The latest change that isn't undone, which Undo takes back.
    fn latest(&self) -> Option<&Change> {
        self.changes.iter().rev().find(|change| !change.undone)
    }

    fn get(&self, id: usize) -> Option<&Change> {
        self.changes.get(id.checked_sub(self.first)?)
    }

    fn get_mut(&mut self, id: usize) -> Option<&mut Change> {
        self.changes.get_mut(id.checked_sub(self.first)?)
    }

    /// The id the next change gets.
    fn next_id(&self) -> usize {
        self.first + self.changes.len()
    }
}

/// The toast for the latest change, which the next one replaces.
struct Toast;

pub fn init(cx: &mut App) {
    cx.set_global::<History>(settings::load("history.json"));
    page::init(cx);
}

fn save(cx: &App) {
    settings::store("history.json", cx.global::<History>());
}

/// Forgets every change, leaving what they did as it is.
pub fn clear(cx: &mut App) {
    let history = cx.global_mut::<History>();
    history.first = history.next_id();
    history.changes.clear();
    history.redo.clear();
    save(cx);
}

pub fn changes(cx: &App) -> &[Change] {
    &cx.global::<History>().changes
}

/// Change `id`, unless it's been let go.
pub fn change(id: usize, cx: &App) -> Option<&Change> {
    cx.global::<History>().get(id)
}

/// Each change undone or redone, by id, once for every time, in turn.
pub fn toggled(cx: &App) -> &[usize] {
    &cx.global::<History>().toggled
}

/// The change being taken back or done again.
pub fn running(cx: &App) -> Option<usize> {
    cx.global::<History>().running
}

/// What Undo and Redo would take back or do again, for the menu.
pub fn next(cx: &App) -> (Option<&'static str>, Option<&'static str>) {
    let history = cx.global::<History>();
    let undo = history.latest().map(|change| change.op.name());
    let redo = history.redo.last().and_then(|&id| history.get(id)).map(|change| change.op.name());
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
    let id = history.next_id();
    history.changes.push(Change::new(id, op, undo));
    history.redo.clear();
    if history.changes.len() > REMEMBERED {
        history.changes.remove(0);
        history.first += 1;
    }
    save(cx);
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
    let next = history.next_id();
    if history.running.is_some() {
        return Task::ready(Vec::new());
    }
    let Some(change) = history.get(id).filter(|change| !change.gone()) else { return Task::ready(Vec::new()) };
    let (op, redo, action) = (change.op, change.undone, change.action.clone());
    history.running = Some(id);
    let verb = if redo { "Redoing" } else { "Undoing" };
    let window = window.window_handle();
    cx.spawn(async move |cx| {
        // Finding out can mean waiting for a share.
        let doomed = cx.background_executor().spawn({
            let action = action.clone();
            async move { action.doomed() }
        });
        let doomed = doomed.await;
        if !doomed.is_empty() && !operations::confirm_delete(&doomed, window, cx).await {
            cx.update(|cx| cx.global_mut::<History>().running = None);
            return Vec::new();
        }
        let task = cx.update(|cx| operations::track(verb, move |progress| action.apply(progress), cx));
        let mut done = task.await;
        let toggled = cx.update(|cx| {
            let history = cx.global_mut::<History>();
            history.running = None;
            // Unless nothing went through, so it's as it was and can be tried again, or it was
            // cleared while it ran, or let go of as the oldest.
            let toggled = match (history.get_mut(id), done.undo.take()) {
                (Some(change), Some(back)) => {
                    change.touched = [footprint(&change.action), footprint(&back)].concat();
                    change.action = back;
                    change.undone = !redo;
                    true
                }
                _ => false,
            };
            if toggled {
                history.toggled.push(id);
            }
            if toggled && redo {
                history.redo.retain(|&undone| undone != id);
            } else if toggled && history.next_id() == next && !history.get(id).is_some_and(Change::gone) {
                // A change made in the meantime would have left nothing to redo, this included.
                history.redo.push(id);
            }
            save(cx);
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
    let note = Notification::new().message(message).id::<Toast>().autohide(true);
    let note = if change(id, cx).is_some_and(Change::gone) {
        note
    } else {
        note.action(move |_, _, _| {
            Button::new("take-back").label(if redo { "Redo" } else { "Undo" }).on_click(move |_, window, cx| {
                if change(id, cx).is_some_and(|change| change.undone == redo) {
                    toggle(id, window, cx).detach();
                }
            })
        })
    };
    window.toast(note, cx);
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
