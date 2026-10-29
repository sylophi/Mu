//! The Trash: moving items there through the system, noting where each came from so it can be put
//! back, and emptying it.

use std::{
    ffi::{CStr, OsString},
    fs, io,
    os::{
        macos::fs::MetadataExt as _,
        unix::ffi::{OsStrExt as _, OsStringExt as _},
    },
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};
use objc2::rc::autoreleasepool;
use objc2_foundation::NSFileManager;

use super::{
    Done, Progress, Undo,
    admin::{self, Step},
    c_path, delete_path, disk_name, display_name, each, file_url, same_volume, trash_dir, unique_path,
};

/// What's in the Trash, leaving out the files Finder keeps there.
pub fn trash_items() -> io::Result<Vec<PathBuf>> {
    let entries = fs::read_dir(trash_dir())?.flatten();
    Ok(entries
        .filter(|entry| !matches!(entry.file_name().to_str(), Some(".DS_Store" | ".localized")))
        .map(|entry| entry.path())
        .collect())
}

/// Where an item Mu moved to the Trash came from, kept on the item so it lasts.
const ORIGIN: &CStr = c"io.github.sylophi.mu.origin";

/// Where `path`, in the Trash, was before Mu moved it there.
pub fn origin(path: &Path) -> Option<PathBuf> {
    let path = c_path(path).ok()?;
    let size =
        unsafe { libc::getxattr(path.as_ptr(), ORIGIN.as_ptr(), std::ptr::null_mut(), 0, 0, libc::XATTR_NOFOLLOW) };
    let mut value = vec![0u8; usize::try_from(size).ok().filter(|&size| size > 0)?];
    let read = unsafe {
        libc::getxattr(path.as_ptr(), ORIGIN.as_ptr(), value.as_mut_ptr().cast(), value.len(), 0, libc::XATTR_NOFOLLOW)
    };
    (read == size).then(|| PathBuf::from(OsString::from_vec(value)))
}

/// Notes on `path` where it came from. Should that fail, it just can't be put back later.
pub fn set_origin(path: &Path, origin: &Path) {
    let (Ok(path), value) = (c_path(path), origin.as_os_str().as_bytes()) else { return };
    unsafe {
        libc::setxattr(path.as_ptr(), ORIGIN.as_ptr(), value.as_ptr().cast(), value.len(), 0, libc::XATTR_NOFOLLOW)
    };
}

pub(super) fn clear_origin(path: &Path) {
    if let Ok(path) = c_path(path) {
        unsafe { libc::removexattr(path.as_ptr(), ORIGIN.as_ptr(), libc::XATTR_NOFOLLOW) };
    }
}

/// Moves `paths` to the Trash through the system, which says where each one landed so it can be
/// put back.
pub fn trash(paths: &[PathBuf]) -> Done {
    let (trashed, error) = each(paths, |path| Ok((trash_one(path)?, path.clone())));
    Done { paths: Vec::new(), undo: (!trashed.is_empty()).then_some(Undo::PutBack(trashed)), error }
}

/// Moves `path` to the Trash, noting where it came from, and says where it landed. The Trash won't
/// take a locked item, so it's unlocked first, and locked again if it doesn't go. Callers ask
/// before.
pub(super) fn trash_one(path: &Path) -> Result<PathBuf> {
    let locked = is_locked(path);
    if locked {
        set_locked(path, false)?;
    }
    let landed = autoreleasepool(|_| move_to_trash(path));
    if locked && landed.is_err() {
        let _ = set_locked(path, true);
    }
    landed
}

/// The move itself, through the system, or as an administrator when only one may make it.
fn move_to_trash(path: &Path) -> Result<PathBuf> {
    let mut landed = None;
    match NSFileManager::defaultManager().trashItemAtURL_resultingItemURL_error(&file_url(path), Some(&mut landed)) {
        Ok(()) => {
            let landed = landed.and_then(|url| url.to_file_path()).context("The Trash didn't say where it put it")?;
            set_origin(&landed, path);
            Ok(landed)
        }
        // NSFileWriteNoPermissionError: it goes into the Trash as an administrator, when it's on the
        // Trash's disk.
        Err(error) if error.code() == 513 && path.parent().is_some_and(|dir| same_volume(dir, &trash_dir())) => {
            let to = unique_path(&trash_dir(), &disk_name(path).to_string_lossy(), false);
            admin::run(Step::Trash(path.into(), to.clone()))
                .with_context(|| format!("Couldn't move “{}” to the Trash", display_name(path)))?;
            Ok(to)
        }
        Err(error) => bail!("Couldn't move “{}” to the Trash: {}", display_name(path), error.localizedDescription()),
    }
}

/// Whether `path` is locked in Finder, which keeps it from being changed, moved or trashed.
pub fn is_locked(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.st_flags() & libc::UF_IMMUTABLE != 0)
}

/// Locks or unlocks `path` in Finder.
fn set_locked(path: &Path, locked: bool) -> Result<()> {
    let flags = fs::symlink_metadata(path)?.st_flags();
    let flags = if locked { flags | libc::UF_IMMUTABLE } else { flags & !libc::UF_IMMUTABLE };
    if unsafe { libc::lchflags(c_path(path)?.as_ptr(), flags) } != 0 {
        let error = io::Error::last_os_error();
        bail!("Couldn't {} “{}”: {error}", if locked { "lock" } else { "unlock" }, display_name(path));
    }
    Ok(())
}

/// Moves items in the Trash back to where they were when Mu moved them there.
pub fn put_back(paths: &[PathBuf]) -> Done {
    let (moves, unknown) = each(paths, |path| {
        let origin =
            origin(path).with_context(|| format!("Mu doesn't know where “{}” came from", display_name(path)))?;
        Ok((path.clone(), origin))
    });
    let done = Undo::PutBack(moves).apply(&Progress::default());
    Done { error: unknown.or(done.error), ..done }
}

/// Deletes everything in the Trash for good.
pub fn empty_trash() -> Result<()> {
    let items = trash_items().context("Couldn't open the Trash")?;
    let (_, error) =
        each(&items, |path| delete_path(path).with_context(|| format!("Couldn't delete “{}”", display_name(path))));
    error.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::{Choices, Scratch, copy_into, tests::names};

    #[test]
    fn trash_and_put_back() {
        let scratch = Scratch::new("trash");
        let dir = &scratch.0;
        fs::write(dir.join("a.txt"), "a").unwrap();
        let copied = copy_into(&[dir.join("a.txt")], dir, &Choices::new(), &Progress::default());

        // Undoing a copy moves it to the Trash, and redoing puts it back from there.
        let trashed = copied.undo.unwrap().apply(&Progress::default());
        assert!(trashed.error.is_none());
        assert_eq!(names(dir), ["a.txt"]);
        let Some(Undo::PutBack(moves)) = &trashed.undo else { panic!("expected a put back, got {:?}", trashed.undo) };
        assert!(moves[0].0.exists());
        let restored = trashed.undo.unwrap().apply(&Progress::default());
        assert!(restored.error.is_none());
        assert_eq!(names(dir), ["a copy.txt", "a.txt"]);
        assert_eq!(restored.undo, Some(Undo::Trash(vec![dir.join("a copy.txt")])));

        assert!(trash(&[dir.join("missing")]).error.is_some());

        // A locked item is unlocked on its way, and comes back unlocked.
        let locked = dir.join("locked.txt");
        fs::write(&locked, "").unwrap();
        assert_eq!(unsafe { libc::chflags(c_path(&locked).unwrap().as_ptr(), libc::UF_IMMUTABLE) }, 0);
        assert!(is_locked(&locked));
        let trashed = trash(std::slice::from_ref(&locked));
        assert!(trashed.error.is_none() && !locked.exists());
        assert!(trashed.undo.unwrap().apply(&Progress::default()).error.is_none());
        assert!(locked.exists() && !is_locked(&locked));
    }

    #[test]
    fn put_back_remembers_where_it_came_from() {
        let scratch = Scratch::new("put-back");
        let a = scratch.0.join("a.txt");
        fs::write(&a, "a").unwrap();
        let trashed = trash(std::slice::from_ref(&a));
        let Some(Undo::PutBack(moves)) = &trashed.undo else { panic!("expected a put back, got {:?}", trashed.undo) };
        let landed = moves[0].0.clone();
        assert_eq!(origin(&landed), Some(a.clone()));

        let back = put_back(&[landed]);
        assert!(back.error.is_none());
        assert_eq!(back.paths, std::slice::from_ref(&a));
        assert_eq!(back.undo, Some(Undo::Trash(vec![a.clone()])));
        assert_eq!(origin(&a), None);

        // Mu can't tell where something it didn't move to the Trash goes.
        let unknown = put_back(std::slice::from_ref(&a));
        assert!(unknown.error.is_some() && unknown.undo.is_none());
        assert!(a.exists());
    }
}
