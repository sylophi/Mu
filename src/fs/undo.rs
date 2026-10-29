//! What a file operation did and how to take it back. Taking it back says how to take that back in
//! turn, which is how redo works.

use std::path::PathBuf;

use super::{Progress, clear_origin, each, move_path, same_volume, trash};

/// What a file operation did: the items it made or moved (to select), how to take it back, and
/// the first error, if any item failed. It carries on past failures, so whatever did go through
/// can still be undone.
#[derive(Default)]
pub struct Done {
    pub paths: Vec<PathBuf>,
    pub undo: Option<Undo>,
    pub error: Option<anyhow::Error>,
}

impl Done {
    pub(super) fn failed(error: anyhow::Error) -> Self {
        Self { error: Some(error), ..Self::default() }
    }

    /// Made `paths`, which undoing moves to the Trash.
    pub(super) fn created(paths: Vec<PathBuf>, error: Option<anyhow::Error>) -> Self {
        Self { undo: (!paths.is_empty()).then(|| Undo::Trash(paths.clone())), paths, error }
    }

    /// Moved items (from, to), which undoing moves back.
    pub(super) fn moved(moves: Vec<(PathBuf, PathBuf)>, error: Option<anyhow::Error>) -> Self {
        let back: Vec<_> =
            moves.iter().filter(|(from, to)| from != to).map(|(from, to)| (to.clone(), from.clone())).collect();
        Self {
            paths: moves.into_iter().map(|(_, to)| to).collect(),
            undo: (!back.is_empty()).then_some(Undo::Move(back)),
            error,
        }
    }
}

/// How to take a file operation back. Doing that returns what takes the undo back in turn, which
/// is how redo works.
#[derive(Debug, PartialEq)]
pub enum Undo {
    /// Moves each item (from, to), keeping both if `to`'s name has been taken since.
    Move(Vec<(PathBuf, PathBuf)>),
    /// Moves what an operation made to the Trash, rather than deleting it.
    Trash(Vec<PathBuf>),
    /// Takes items back out of the Trash (from, to).
    PutBack(Vec<(PathBuf, PathBuf)>),
    /// Takes back each in turn, for an operation that did more than one kind of thing.
    All(Vec<Undo>),
    /// Takes back what an operation cleared out of its way, like the items it replaced, which
    /// isn't what it was about.
    Aside(Box<Undo>),
}

impl Undo {
    /// One undo for all of `steps`, which run in turn, or None when there's nothing to take back.
    pub(super) fn all(mut steps: Vec<Undo>) -> Option<Undo> {
        match steps.len() {
            0 | 1 => steps.pop(),
            _ => Some(Undo::All(steps)),
        }
    }
}

impl Undo {
    pub fn apply(&self, progress: &Progress) -> Done {
        match self {
            Undo::Move(moves) => {
                progress.expect(
                    moves
                        .iter()
                        .filter(|(from, to)| !same_volume(from, to.parent().unwrap_or(to)))
                        .map(|(from, _)| from),
                );
                let (moved, error) = each(moves, |(from, to)| Ok((from.clone(), move_path(from, to, progress)?)));
                Done::moved(moved, error)
            }
            Undo::Trash(paths) => trash(paths),
            Undo::PutBack(moves) => {
                let (restored, error) = each(moves, |(from, to)| {
                    let restored = move_path(from, to, progress)?;
                    clear_origin(&restored);
                    Ok(restored)
                });
                Done::created(restored, error)
            }
            Undo::All(steps) => {
                let mut done = Done::default();
                let mut back = Vec::new();
                for step in steps {
                    let step = step.apply(progress);
                    done.paths.extend(step.paths);
                    back.extend(step.undo);
                    if let Some(error) = step.error {
                        done.error.get_or_insert(error);
                    }
                }
                // What took the steps back is taken back in the opposite order.
                back.reverse();
                Done { undo: Undo::all(back), ..done }
            }
            Undo::Aside(step) => {
                let done = step.apply(progress);
                Done { undo: done.undo.map(|undo| Undo::Aside(Box::new(undo))), ..done }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    use crate::fs::{Choices, Scratch, move_into, tests::names};

    #[test]
    fn undoes_and_redoes_moves() {
        let scratch = Scratch::new("undo-move");
        let dir = &scratch.0;
        let sub = dir.join("sub");
        fs::create_dir(&sub).unwrap();
        fs::write(dir.join("a.txt"), "a").unwrap();
        fs::write(dir.join("b.txt"), "b").unwrap();
        let moved = move_into(&[dir.join("a.txt"), dir.join("b.txt")], &sub, &Choices::new(), &Progress::default());
        assert_eq!(names(&sub), ["a.txt", "b.txt"]);

        // Something new took a's old name, and b went missing: a comes back beside it, and the
        // error says what happened to b.
        fs::write(dir.join("a.txt"), "new").unwrap();
        fs::remove_file(sub.join("b.txt")).unwrap();
        let undone = moved.undo.unwrap().apply(&Progress::default());
        assert!(undone.error.unwrap().to_string().contains("isn't there anymore"));
        assert_eq!(undone.paths, [dir.join("a 2.txt")]);
        assert_eq!(fs::read_to_string(dir.join("a 2.txt")).unwrap(), "a");
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "new");

        // Redo moves what came back, from where it came back to.
        let redone = undone.undo.unwrap().apply(&Progress::default());
        assert_eq!(redone.paths, [sub.join("a.txt")]);
        assert_eq!(names(&sub), ["a.txt"]);

        // A folder that's gone is made again to move back into.
        fs::rename(&sub, dir.join("elsewhere")).unwrap();
        let back = Undo::Move(vec![(dir.join("elsewhere/a.txt"), sub.join("a.txt"))]).apply(&Progress::default());
        assert!(back.error.is_none());
        assert!(sub.join("a.txt").exists());
    }
}
