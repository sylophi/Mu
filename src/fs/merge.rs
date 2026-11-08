//! Copying and moving items into a folder like Windows does: folders merge into folders with their
//! names, and each file whose name is taken goes as you chose.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};

use super::{
    Done, Progress, Undo, copy_path, delete_one, disk_name, display_name, each, is_package, lacks_trash, move_path,
    natural_cmp, same_volume, trash_one, trash_or_delete, unique_path,
};

/// What to do about an item copied or moved in whose name is taken there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Choice {
    /// What's there goes to the Trash, or for good on a volume without one, and the item takes
    /// its place.
    Replace,
    /// The item stays where it was, and so does what's there.
    Skip,
    /// The item goes in beside what's there, under a free name.
    KeepBoth,
}

/// What to do about each clash, by where its item would go. A clash nobody chose for keeps both.
pub type Choices = HashMap<PathBuf, Choice>;

/// An item copied or moved in, and what already has its name there.
#[derive(Clone, Debug, PartialEq)]
pub struct Clash {
    pub from: PathBuf,
    pub to: PathBuf,
}

/// Whether `from` merges into `to`, like Windows: both are folders. Packages like apps don't, as
/// their insides go together, and neither do links.
fn merges(from: &Path, to: &Path) -> bool {
    let folder = |path: &Path| fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir()) && !is_package(path);
    folder(from) && folder(to)
}

/// What's in `dir`, in order by name.
fn children(dir: &Path) -> Result<Vec<PathBuf>> {
    let entries = fs::read_dir(dir).with_context(|| format!("Couldn't open “{}”", display_name(dir)))?;
    let mut children: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
    children.sort_by(|a, b| natural_cmp(&a.to_string_lossy(), &b.to_string_lossy()));
    Ok(children)
}

/// The clashes copying or moving `sources` into `dir` would run into, inside folders that merge
/// too. What's in `dir` already doesn't clash: copying it there duplicates it, and moving it there
/// leaves it be.
pub fn clashes(sources: &[PathBuf], dir: &Path) -> Vec<Clash> {
    fn find(from: &Path, to: &Path, found: &mut Vec<Clash>) {
        if fs::symlink_metadata(to).is_err() {
            return;
        }
        if !merges(from, to) {
            // What's in the way holds what's coming, so it can't make way. It goes beside it.
            if !from.starts_with(to) {
                found.push(Clash { from: from.to_path_buf(), to: to.to_path_buf() });
            }
            return;
        }
        for child in children(from).unwrap_or_default() {
            find(&child, &to.join(disk_name(&child)), found);
        }
    }
    let mut found = Vec::new();
    for source in sources.iter().filter(|source| source.parent() != Some(dir) && !dir.starts_with(source)) {
        find(source, &dir.join(disk_name(source)), &mut found);
    }
    found
}

/// What's left of an item after moving it in.
enum Left {
    /// Nothing, as it went here.
    Gone(PathBuf),
    /// Only the folders it held, emptied, since it merged.
    Shell,
    /// Items that stayed, as they were skipped or couldn't move.
    Items,
}

/// A copy or move into a folder, merging folders, and what it's done so far.
struct Merge<'a> {
    choices: &'a Choices,
    progress: &'a Progress,
    /// Whether what's copied or moved counts in `progress`, as moves within a disk don't.
    counted: bool,
    made: Vec<PathBuf>,
    /// Where moved items went and where they were.
    moved: Vec<(PathBuf, PathBuf)>,
    /// What was replaced, in the Trash, and where it was.
    replaced: Vec<(PathBuf, PathBuf)>,
    /// Folders a move emptied, which go to the Trash once it's done, so their own details come
    /// back with them.
    shells: Vec<PathBuf>,
    /// What's making way for the item on its way (where it's waiting, and where it was), on a
    /// volume without a Trash. It's only deleted once the item is there.
    aside: Option<(PathBuf, PathBuf)>,
}

impl<'a> Merge<'a> {
    fn new(choices: &'a Choices, progress: &'a Progress) -> Self {
        let (made, moved, replaced, shells) = Default::default();
        Self { choices, progress, counted: true, made, moved, replaced, shells, aside: None }
    }

    /// Where `from` goes for `to`: `to` itself when it's free or once what's there has made way, a
    /// free name beside it, or nowhere when it's skipped.
    fn target(&mut self, from: &Path, to: &Path) -> Result<Option<PathBuf>> {
        if fs::symlink_metadata(to).is_err() {
            return Ok(Some(to.to_path_buf()));
        }
        let dir = to.parent().context("Can't replace the top of the disk")?;
        match self.choices.get(to).copied().unwrap_or(Choice::KeepBoth) {
            Choice::Skip => {
                if self.counted {
                    self.progress.skip(from);
                }
                Ok(None)
            }
            Choice::KeepBoth => Ok(Some(unique_path(dir, &disk_name(to).to_string_lossy(), false))),
            Choice::Replace => {
                if lacks_trash(dir) {
                    let aside = unique_path(dir, ".mu-replaced", false);
                    fs::rename(to, &aside).with_context(|| format!("Couldn't replace “{}”", display_name(to)))?;
                    self.aside = Some((aside, to.to_path_buf()));
                } else {
                    self.replaced.push((trash_one(to)?, to.to_path_buf()));
                }
                Ok(Some(to.to_path_buf()))
            }
        }
    }

    /// Deletes what made way for the item now it's `landed`, or puts it back if it didn't.
    fn settle(&mut self, landed: bool) -> Result<()> {
        let Some((aside, to)) = self.aside.take() else { return Ok(()) };
        if landed {
            return delete_one(&aside);
        }
        fs::rename(aside, &to).with_context(|| format!("Couldn't put “{}” back", display_name(&to)))
    }

    /// Copies `from` to `to`, merging folders. Says where it went, unless it was skipped.
    fn copy(&mut self, from: &Path, to: &Path) -> Result<Option<PathBuf>> {
        self.progress.check()?;
        if merges(from, to) {
            let (_, error) = each(&children(from)?, |child| self.copy(child, &to.join(disk_name(child))));
            return error.map_or(Ok(Some(to.to_path_buf())), Err);
        }
        let Some(target) = self.target(from, to)? else { return Ok(None) };
        let copied = copy_path(from, &target, self.progress);
        self.settle(copied.is_ok())?;
        copied?;
        self.made.push(target.clone());
        Ok(Some(target))
    }

    /// Moves `from` to `to`, merging folders, and says what it left behind.
    fn r#move(&mut self, from: &Path, to: &Path) -> Result<Left> {
        self.progress.check()?;
        if !merges(from, to) {
            let Some(target) = self.target(from, to)? else { return Ok(Left::Items) };
            let landed = move_path(from, &target, self.progress);
            self.settle(landed.is_ok())?;
            let landed = landed?;
            self.moved.push((landed.clone(), from.to_path_buf()));
            return Ok(Left::Gone(landed));
        }
        let (left, error) =
            each(&children(from)?, |child| Ok((child.clone(), self.r#move(child, &to.join(disk_name(child)))?)));
        let whole = error.is_none() && !left.iter().any(|(_, left)| matches!(left, Left::Items));
        // Emptied folders in one that stays go on their own. Otherwise this one takes them along.
        if !whole {
            self.shells
                .extend(left.into_iter().filter(|(_, left)| matches!(left, Left::Shell)).map(|(child, _)| child));
        }
        error.map_or(Ok(if whole { Left::Shell } else { Left::Items }), Err)
    }

    /// What it did, and how to take that back: in the opposite order, emptied folders come back
    /// first to move items back into, and what was replaced comes back last, once its place is
    /// free again.
    fn done(self, paths: Vec<PathBuf>, error: Option<anyhow::Error>) -> Done {
        let emptied = trash_or_delete(&self.shells);
        let steps = [
            emptied.undo.map(|undo| Undo::Aside(Box::new(undo))),
            (!self.made.is_empty()).then_some(Undo::Trash(self.made)),
            (!self.moved.is_empty()).then_some(Undo::Move(self.moved)),
            (!self.replaced.is_empty()).then_some(Undo::Aside(Box::new(Undo::PutBack(self.replaced)))),
        ];
        Done { paths, undo: Undo::all(steps.into_iter().flatten().collect()), error: error.or(emptied.error) }
    }
}

/// Copies `sources` into `dir`. Folders merge into folders with their names there, like Windows,
/// and each clash goes as `choices` says. A name that's taken in the same folder gets a " copy"
/// suffix, so pasting there duplicates.
pub fn copy_into(sources: &[PathBuf], dir: &Path, choices: &Choices, progress: &Progress) -> Done {
    progress.expect(sources);
    let mut merge = Merge::new(choices, progress);
    let (landed, error) = each(sources, |source| {
        if dir.starts_with(source) {
            bail!("Can't copy “{}” into itself", display_name(source));
        }
        if source.parent() == Some(dir) {
            let target = unique_path(dir, &disk_name(source).to_string_lossy(), true);
            copy_path(source, &target, progress)?;
            merge.made.push(target.clone());
            return Ok(Some(target));
        }
        merge.copy(source, &dir.join(disk_name(source)))
    });
    merge.done(landed.into_iter().flatten().collect(), error)
}

/// Copies each of `sources` next to itself, under a name of its own, like "notes copy.txt".
pub fn duplicate(sources: &[PathBuf], progress: &Progress) -> Done {
    progress.expect(sources);
    let (made, error) = each(sources, |source| {
        let dir = source.parent().context("Can't duplicate the top of a disk")?;
        let target = unique_path(dir, &disk_name(source).to_string_lossy(), true);
        copy_path(source, &target, progress)?;
        Ok(target)
    });
    Done::created(made, error)
}

/// Moves `sources` into `dir`. Folders merge into folders with their names there, like Windows,
/// and each clash goes as `choices` says. A folder that's emptied goes to the Trash, or for good
/// on a volume without one.
pub fn move_into(sources: &[PathBuf], dir: &Path, choices: &Choices, progress: &Progress) -> Done {
    progress.expect(sources.iter().filter(|source| !same_volume(source, dir)));
    let mut merge = Merge::new(choices, progress);
    let (landed, error) = each(sources, |source| {
        if source.parent() == Some(dir) {
            return Ok(source.clone());
        }
        if dir.starts_with(source) {
            bail!("Can't move “{}” into itself", display_name(source));
        }
        merge.counted = !same_volume(source, dir);
        let to = dir.join(disk_name(source));
        Ok(match merge.r#move(source, &to)? {
            Left::Gone(landed) => landed,
            Left::Shell => {
                merge.shells.push(source.clone());
                to
            }
            Left::Items => to,
        })
    });
    merge.done(landed, error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::{Cancelled, Scratch, tests::names};

    #[test]
    fn what_makes_way_goes_only_once_the_item_is_there() {
        let scratch = Scratch::new("settle");
        let (aside, to) = (scratch.0.join(".aside"), scratch.0.join("a.txt"));
        let (choices, progress) = (Choices::new(), Progress::default());
        let mut merge = Merge::new(&choices, &progress);

        fs::write(&aside, "old").unwrap();
        merge.aside = Some((aside.clone(), to.clone()));
        merge.settle(false).unwrap();
        assert_eq!(fs::read_to_string(&to).unwrap(), "old");
        assert!(!aside.exists());

        fs::write(&aside, "old").unwrap();
        merge.aside = Some((aside.clone(), to.clone()));
        merge.settle(true).unwrap();
        assert!(!aside.exists() && to.exists());
    }

    #[test]
    fn duplicates_each_next_to_itself() {
        let scratch = Scratch::new("duplicate");
        let dir = &scratch.0;
        fs::create_dir(dir.join("inner")).unwrap();
        fs::write(dir.join("a.txt"), "a").unwrap();
        fs::write(dir.join("inner/b.txt"), "b").unwrap();
        let done = duplicate(&[dir.join("a.txt"), dir.join("inner/b.txt")], &Progress::default());
        assert_eq!(done.paths, [dir.join("a copy.txt"), dir.join("inner/b copy.txt")]);
        assert_eq!(fs::read_to_string(dir.join("inner/b copy.txt")).unwrap(), "b");
        // Undoing takes the copies away.
        assert_eq!(done.undo, Some(Undo::Trash(done.paths.clone())));
    }

    #[test]
    fn counts_and_cancels_copies() {
        let scratch = Scratch::new("progress");
        let dir = &scratch.0;
        let (from, to) = (dir.join("from"), dir.join("to"));
        fs::create_dir_all(from.join("inner")).unwrap();
        fs::create_dir(&to).unwrap();
        fs::write(from.join("a.bin"), vec![1; 300_000]).unwrap();
        fs::write(from.join("inner/b.bin"), vec![2; 200_000]).unwrap();

        let progress = Progress::default();
        let copied = copy_into(&[from.clone(), from.join("a.bin")], &to, &Choices::new(), &progress);
        assert!(copied.error.is_none());
        assert_eq!(progress.bytes(), (800_000, 800_000));

        let cancelled = Progress::default();
        cancelled.cancel();
        let stopped = copy_into(std::slice::from_ref(&from), &to, &Choices::new(), &cancelled);
        assert!(stopped.error.is_some_and(|error| error.is::<Cancelled>()) && stopped.paths.is_empty());
        assert!(!to.join("from copy").exists());
    }

    #[test]
    fn copies_and_moves() {
        let scratch = Scratch::new("ops");
        let dir = &scratch.0;
        let sub = dir.join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(dir.join("a.txt"), "top").unwrap();
        fs::write(sub.join("a.txt"), "sub").unwrap();

        let none = Choices::new();
        // Pasting into the same folder duplicates.
        let copied = copy_into(&[dir.join("a.txt")], dir, &none, &Progress::default());
        assert!(copied.error.is_none());
        assert_eq!(copied.paths, [dir.join("a copy.txt")]);
        assert_eq!(copied.undo, Some(Undo::Trash(vec![dir.join("a copy.txt")])));
        assert_eq!(fs::read_to_string(dir.join("a copy.txt")).unwrap(), "top");
        assert!(copy_into(std::slice::from_ref(&sub), &sub, &none, &Progress::default()).error.is_some());
        // One item failing doesn't stop the rest, and a name that's taken keeps both unless told.
        let partly = copy_into(&[dir.clone(), dir.join("a.txt")], &sub, &none, &Progress::default());
        assert!(partly.error.is_some());
        assert_eq!(partly.paths, [sub.join("a 2.txt")]);

        // Moving where it already is does nothing.
        let stayed = move_into(&[dir.join("a.txt")], dir, &none, &Progress::default());
        assert_eq!((stayed.paths, stayed.undo), (vec![dir.join("a.txt")], None));
        let moved = move_into(&[sub.join("a.txt")], dir, &none, &Progress::default());
        assert_eq!(moved.paths, [dir.join("a 2.txt")]);
        assert_eq!(moved.undo, Some(Undo::Move(vec![(dir.join("a 2.txt"), sub.join("a.txt"))])));
        assert!(!sub.join("a.txt").exists());
        assert_eq!(fs::read_to_string(dir.join("a 2.txt")).unwrap(), "sub");
        assert!(move_into(std::slice::from_ref(&sub), &sub, &none, &Progress::default()).error.is_some());
    }

    /// A Photos folder to copy or move into `dir`/to, where one with some of the same names is.
    /// Returns both.
    fn photos(dir: &Path) -> (PathBuf, PathBuf) {
        let (from, to) = (dir.join("from/Photos"), dir.join("to/Photos"));
        fs::create_dir_all(from.join("Inner")).unwrap();
        fs::create_dir_all(to.join("Inner")).unwrap();
        fs::write(from.join("a.txt"), "new").unwrap();
        fs::write(from.join("b.txt"), "b").unwrap();
        fs::write(from.join("Inner/c.txt"), "c").unwrap();
        fs::write(to.join("a.txt"), "old").unwrap();
        fs::write(to.join("d.txt"), "d").unwrap();
        (from, to)
    }

    #[test]
    fn finds_clashes_inside_merging_folders() {
        let scratch = Scratch::new("clashes");
        let dir = &scratch.0;
        let (from, to) = photos(dir);
        let clash = Clash { from: from.join("a.txt"), to: to.join("a.txt") };
        assert_eq!(clashes(std::slice::from_ref(&from), &dir.join("to")), [clash]);
        // What's in the folder already isn't in the way of itself.
        assert!(clashes(std::slice::from_ref(&to), &dir.join("to")).is_empty());
        // An app's insides go together, so apps don't merge.
        fs::create_dir_all(dir.join("from/Tool.app/Contents")).unwrap();
        fs::create_dir_all(dir.join("to/Tool.app/Contents")).unwrap();
        let app = Clash { from: dir.join("from/Tool.app"), to: dir.join("to/Tool.app") };
        assert_eq!(clashes(&[dir.join("from/Tool.app")], &dir.join("to")), [app]);
        // A file can't replace the folder it's in, so it goes beside it without asking.
        fs::write(dir.join("from/Photos/Photos"), "").unwrap();
        assert!(clashes(&[dir.join("from/Photos/Photos")], &dir.join("from")).is_empty());
    }

    #[test]
    fn copies_into_merging_folders() {
        let scratch = Scratch::new("merge-copy");
        let dir = &scratch.0;
        let (from, to) = photos(dir);
        let into = dir.join("to");

        // Skipped, the old one stays and the rest comes in, and only what's copied counts.
        let skip = Choices::from([(to.join("a.txt"), Choice::Skip)]);
        let progress = Progress::default();
        let copied = copy_into(std::slice::from_ref(&from), &into, &skip, &progress);
        assert!(copied.error.is_none());
        assert_eq!(copied.paths, std::slice::from_ref(&to));
        assert_eq!(progress.bytes(), (2, 2));
        assert_eq!(names(&to), ["Inner", "a.txt", "b.txt", "d.txt"]);
        assert_eq!(names(&to.join("Inner")), ["c.txt"]);
        assert_eq!(fs::read_to_string(to.join("a.txt")).unwrap(), "old");

        // Undoing takes away only what came in, leaving the folders that were there.
        let undone = copied.undo.unwrap().apply(&Progress::default());
        assert!(undone.error.is_none());
        assert_eq!(names(&to), ["Inner", "a.txt", "d.txt"]);
        assert!(names(&to.join("Inner")).is_empty());
        let redone = undone.undo.unwrap().apply(&Progress::default());
        assert!(redone.error.is_none());
        assert_eq!(names(&to), ["Inner", "a.txt", "b.txt", "d.txt"]);

        // Keeping both, the one coming in gets a number.
        let both = Choices::from([(to.join("a.txt"), Choice::KeepBoth)]);
        copy_into(std::slice::from_ref(&from), &into, &both, &Progress::default());
        assert_eq!(fs::read_to_string(to.join("a 2.txt")).unwrap(), "new");
        assert_eq!(fs::read_to_string(to.join("a.txt")).unwrap(), "old");
    }

    #[test]
    fn moves_into_merging_folders() {
        let scratch = Scratch::new("merge-move");
        let dir = &scratch.0;
        let (from, to) = photos(dir);
        let into = dir.join("to");

        // Replacing sends the old one to the Trash, and the emptied folder goes too.
        let replace = Choices::from([(to.join("a.txt"), Choice::Replace)]);
        let moved = move_into(std::slice::from_ref(&from), &into, &replace, &Progress::default());
        assert!(moved.error.is_none());
        assert_eq!(moved.paths, std::slice::from_ref(&to));
        assert_eq!(names(&to), ["Inner", "a.txt", "b.txt", "d.txt"]);
        assert_eq!(fs::read_to_string(to.join("a.txt")).unwrap(), "new");
        assert!(!from.exists());

        // Undoing puts everything back as it was, and redoing does it again.
        let undone = moved.undo.unwrap().apply(&Progress::default());
        assert!(undone.error.is_none());
        assert_eq!(names(&from), ["Inner", "a.txt", "b.txt"]);
        assert_eq!(names(&from.join("Inner")), ["c.txt"]);
        assert_eq!(names(&to), ["Inner", "a.txt", "d.txt"]);
        assert_eq!(fs::read_to_string(to.join("a.txt")).unwrap(), "old");
        let redone = undone.undo.unwrap().apply(&Progress::default());
        assert!(redone.error.is_none());
        assert_eq!(fs::read_to_string(to.join("a.txt")).unwrap(), "new");
        assert!(!from.exists());
        // Back again, so nothing's left in the Trash.
        assert!(redone.undo.unwrap().apply(&Progress::default()).error.is_none());

        // Skipped, the file stays behind in its folder, and an emptied folder in there goes alone.
        let skip = Choices::from([(to.join("a.txt"), Choice::Skip)]);
        let moved = move_into(std::slice::from_ref(&from), &into, &skip, &Progress::default());
        assert!(moved.error.is_none());
        assert_eq!(names(&from), ["a.txt"]);
        assert_eq!(names(&to), ["Inner", "a.txt", "b.txt", "d.txt"]);
        assert!(moved.undo.unwrap().apply(&Progress::default()).error.is_none());
        assert_eq!(names(&from), ["Inner", "a.txt", "b.txt"]);
        assert_eq!(names(&from.join("Inner")), ["c.txt"]);
    }
}
