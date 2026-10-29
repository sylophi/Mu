//! The steps file operations are made of, like copying, moving and deleting an item, as an
//! administrator if need be, with how far they've got. And the simple operations: renaming, and
//! making a folder or a file.

use std::{
    ffi::{CStr, OsStr, c_void},
    fs,
    io::{self, Write as _},
    os::{macos::fs::MetadataExt as _, unix::ffi::OsStrExt as _},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed},
};

use anyhow::{Context as _, Result, anyhow, bail};

use super::{
    Done,
    admin::{self, Step},
    c_path, display_name,
};

/// `name` in `dir`, or when that's taken, a free name in "name copy.ext" style when `copy`,
/// otherwise "name 2.ext".
pub(super) fn unique_path(dir: &Path, name: &str, copy: bool) -> PathBuf {
    let candidate = dir.join(name);
    if fs::symlink_metadata(&candidate).is_err() {
        return candidate;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !candidate.is_dir() => (stem, format!(".{ext}")),
        _ => (name, String::new()),
    };
    (if copy { 1 } else { 2 }..)
        .map(|n| match (copy, n) {
            (true, 1) => dir.join(format!("{stem} copy{ext}")),
            (true, n) => dir.join(format!("{stem} copy {n}{ext}")),
            (false, n) => dir.join(format!("{stem} {n}{ext}")),
        })
        .find(|path| fs::symlink_metadata(path).is_err())
        .unwrap()
}

/// Runs `op` on every item, keeping what it returns and the first error, until one's cancelled.
pub(super) fn each<T, R>(items: &[T], mut op: impl FnMut(&T) -> Result<R>) -> (Vec<R>, Option<anyhow::Error>) {
    let mut done = Vec::new();
    let mut error = None;
    for item in items {
        match op(item) {
            Ok(result) => done.push(result),
            Err(failure) => {
                let cancelled = failure.is::<Cancelled>();
                error.get_or_insert(failure);
                if cancelled {
                    break;
                }
            }
        }
    }
    (done, error)
}

/// How far a file operation's copying has got, and a way to stop it.
#[derive(Default)]
pub struct Progress {
    /// Bytes to copy, counted before copying starts.
    total: AtomicU64,
    copied: AtomicU64,
    cancelled: AtomicBool,
}

impl Progress {
    /// Bytes copied so far, of how many.
    pub fn bytes(&self) -> (u64, u64) {
        (self.copied.load(Relaxed), self.total.load(Relaxed))
    }

    /// Stops the operation at the next chance. What's been copied stays, except a partial item.
    pub fn cancel(&self) {
        self.cancelled.store(true, Relaxed);
    }

    pub(super) fn check(&self) -> Result<()> {
        if self.cancelled.load(Relaxed) { Err(Cancelled.into()) } else { Ok(()) }
    }

    /// Adds what copying `paths` will write to the total.
    pub(super) fn expect<'a>(&self, paths: impl IntoIterator<Item = &'a PathBuf>) {
        self.total.fetch_add(paths.into_iter().map(|path| size(path)).sum(), Relaxed);
    }

    /// Takes `path`, which was expected but won't be copied after all, off the total.
    pub(super) fn skip(&self, path: &Path) {
        let size = size(path);
        let _ = self.total.fetch_update(Relaxed, Relaxed, |total| Some(total.saturating_sub(size)));
    }
}

/// How many bytes are in `path`, all the way down.
fn size(path: &Path) -> u64 {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => {
            fs::read_dir(path).map(|items| items.flatten().map(|item| size(&item.path())).sum()).unwrap_or(0)
        }
        Ok(meta) => meta.len(),
        Err(_) => 0,
    }
}

/// What a cancelled operation fails with, which isn't worth telling you about.
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("Cancelled")
    }
}

impl std::error::Error for Cancelled {}

/// Copies `from` to `to`, as an administrator if need be (see `copyfile`). Cancelled, it removes
/// what it made of `to`.
pub(super) fn copy_path(from: &Path, to: &Path, progress: &Progress) -> Result<()> {
    progress.check()?;
    let Err(error) = copyfile(from, to, progress) else { return Ok(()) };
    if progress.check().is_err() {
        let _ = remove_all(to);
        return Err(Cancelled.into());
    }
    if error.raw_os_error() == Some(libc::EACCES) {
        // What it got through goes, as copying again onto it would copy into it.
        let _ = delete_path(to);
    }
    admin::retry(error, Step::Copy(from.into(), to.into()))
        .with_context(|| format!("Couldn't copy “{}”", display_name(from)))?;
    // Copying as an administrator can't be stopped partway, so a cancel takes it back once it's done.
    if progress.check().is_err() {
        let _ = delete_path(to);
        return Err(Cancelled.into());
    }
    Ok(())
}

/// Copies with the system's `copyfile`, which clones on APFS and keeps metadata, like Finder, and
/// counts what it writes in `progress`.
pub fn copyfile(from: &Path, to: &Path, progress: &Progress) -> io::Result<()> {
    let (from_c, to_c) = (c_path(from)?, c_path(to)?);
    let flags = libc::COPYFILE_METADATA
        | libc::COPYFILE_DATA
        | libc::COPYFILE_RECURSIVE
        | libc::COPYFILE_CLONE
        | libc::COPYFILE_NOFOLLOW_SRC;
    let tracker = Tracker { progress, file: Default::default() };
    let state = unsafe { libc::copyfile_state_alloc() };
    let callback: CopyCallback = copy_status;
    let (result, error) = unsafe {
        libc::copyfile_state_set(state, libc::COPYFILE_STATE_STATUS_CB as u32, callback as *const c_void);
        libc::copyfile_state_set(state, libc::COPYFILE_STATE_STATUS_CTX as u32, (&raw const tracker).cast());
        let result = libc::copyfile(from_c.as_ptr(), to_c.as_ptr(), state, flags);
        let error = io::Error::last_os_error();
        libc::copyfile_state_free(state);
        (result, error)
    };
    if result == 0 { Ok(()) } else { Err(error) }
}

/// Deletes `path`, all the way down, with only your own rights (see `delete_path`).
pub fn remove_all(path: &Path) -> io::Result<()> {
    if fs::symlink_metadata(path)?.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) }
}

/// Deletes `path`, all the way down, as an administrator if need be, like `copy_path` and
/// `move_path` do their steps.
pub(super) fn delete_path(path: &Path) -> Result<()> {
    remove_all(path).or_else(|error| admin::retry(error, Step::Delete(path.into())))
}

type CopyCallback = extern "C" fn(i32, i32, libc::copyfile_state_t, *const i8, *const i8, *mut c_void) -> i32;

/// What `copy_status` needs: where to count, and how much of the current file it has counted.
struct Tracker<'a> {
    progress: &'a Progress,
    file: std::cell::Cell<u64>,
}

/// Hears from `copyfile` as it goes: counts the bytes it writes, and stops it at the first error, as
/// it would by itself, or when cancelled.
extern "C" fn copy_status(
    what: i32,
    stage: i32,
    state: libc::copyfile_state_t,
    from: *const i8,
    _to: *const i8,
    context: *mut c_void,
) -> i32 {
    let tracker = unsafe { &*context.cast::<Tracker>() };
    let count = |bytes: u64| {
        tracker.progress.copied.fetch_add(bytes.saturating_sub(tracker.file.replace(bytes)), Relaxed);
    };
    match (what, stage) {
        (_, libc::COPYFILE_ERR) => return libc::COPYFILE_QUIT,
        (libc::COPYFILE_RECURSE_FILE, libc::COPYFILE_START) => tracker.file.set(0),
        (libc::COPYFILE_COPY_DATA, libc::COPYFILE_PROGRESS) => {
            let mut copied: libc::off_t = 0;
            unsafe { libc::copyfile_state_get(state, libc::COPYFILE_STATE_COPIED as u32, (&raw mut copied).cast()) };
            count(copied as u64);
        }
        // A cloned file writes nothing, so it counts in one go.
        (libc::COPYFILE_RECURSE_FILE, libc::COPYFILE_FINISH) if !from.is_null() => {
            let from = Path::new(OsStr::from_bytes(unsafe { CStr::from_ptr(from) }.to_bytes()));
            count(fs::symlink_metadata(from).map_or(0, |meta| meta.len()));
        }
        _ => {}
    }
    if tracker.progress.check().is_err() { libc::COPYFILE_QUIT } else { libc::COPYFILE_CONTINUE }
}

/// Moves `from` to `to`, or next to it under a free name if something else has that one, and
/// returns where it went. Remakes `to`'s folder if it's gone, and copies then deletes across disks.
pub(super) fn move_path(from: &Path, to: &Path, progress: &Progress) -> Result<PathBuf> {
    progress.check()?;
    let Ok(current) = fs::symlink_metadata(from) else { bail!("“{}” isn't there anymore", display_name(from)) };
    let dir = to.parent().context("Can't move to the top of the disk")?;
    // A name that only differs in case finds the same file on a case-insensitive disk.
    let taken = fs::symlink_metadata(to).is_ok_and(|existing| existing.st_ino() != current.st_ino());
    let target = if taken { unique_path(dir, &disk_name(to).to_string_lossy(), false) } else { to.to_path_buf() };
    fs::create_dir_all(dir)
        .or_else(|error| admin::retry(error, Step::MakeFolder(dir.into())))
        .with_context(|| format!("Couldn't make “{}”", display_name(dir)))?;
    match fs::rename(from, &target) {
        Ok(()) => {}
        Err(error) if error.raw_os_error() == Some(libc::EXDEV) => {
            copy_path(from, &target, progress)?;
            delete_path(from)
                .with_context(|| format!("Couldn't delete “{}” once it was copied", display_name(from)))?;
        }
        Err(error) => admin::retry(error, Step::Rename(from.into(), target.clone()))
            .with_context(|| format!("Couldn't move “{}”", display_name(from)))?,
    }
    Ok(target)
}

/// The name `path` has on disk, which unlike its display name is what to give its copies.
pub(super) fn disk_name(path: &Path) -> &OsStr {
    path.file_name().unwrap_or_default()
}

pub fn rename(path: &Path, new_name: &str) -> Done {
    if new_name.is_empty() || new_name.contains('/') {
        return Done::failed(anyhow!("“{new_name}” isn't a valid name"));
    }
    let target = path.with_file_name(new_name);
    // On a case-insensitive disk "a" → "A" finds the same file, which is fine.
    if let (Ok(existing), Ok(current)) = (fs::symlink_metadata(&target), fs::symlink_metadata(path))
        && existing.st_ino() != current.st_ino()
    {
        return Done::failed(anyhow!("“{new_name}” already exists"));
    }
    match fs::rename(path, &target).or_else(|error| admin::retry(error, Step::Rename(path.into(), target.clone()))) {
        Ok(()) => Done::moved(vec![(path.to_path_buf(), target)], None),
        Err(error) => Done::failed(error.context(format!("Couldn't rename “{}”", display_name(path)))),
    }
}

pub fn new_folder(dir: &Path) -> Done {
    let path = unique_path(dir, "New Folder", false);
    match fs::create_dir(&path).or_else(|error| admin::retry(error, Step::MakeFolder(path.clone()))) {
        Ok(()) => Done::created(vec![path], None),
        Err(error) => Done::failed(error.context("Couldn't create the folder")),
    }
}

/// Makes a file in `dir` with `bytes` in it, named `name`, numbered if that's taken.
pub fn new_file(dir: &Path, name: &str, bytes: &[u8]) -> Done {
    let path = unique_path(dir, name, false);
    let made = fs::File::create_new(&path).and_then(|mut file| file.write_all(bytes));
    match made.or_else(|error| admin::retry(error, Step::Write(path.clone(), bytes.to_vec()))) {
        Ok(()) => Done::created(vec![path], None),
        Err(error) => Done::failed(error.context("Couldn't create the file")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::{Scratch, tests::names};

    #[test]
    fn unique_names() {
        let scratch = Scratch::new("unique");
        let dir = &scratch.0;
        fs::write(dir.join("a.txt"), "").unwrap();
        fs::write(dir.join(".env"), "").unwrap();
        fs::create_dir(dir.join("v1.2")).unwrap();
        assert_eq!(unique_path(dir, "a.txt", false), dir.join("a 2.txt"));
        assert_eq!(unique_path(dir, "a.txt", true), dir.join("a copy.txt"));
        assert_eq!(unique_path(dir, "b.txt", false), dir.join("b.txt"));
        assert_eq!(unique_path(dir, ".env", false), dir.join(".env 2"));
        // A folder's dots aren't an extension.
        assert_eq!(unique_path(dir, "v1.2", false), dir.join("v1.2 2"));
        fs::write(dir.join("a copy.txt"), "").unwrap();
        assert_eq!(unique_path(dir, "a.txt", true), dir.join("a copy 2.txt"));
    }

    #[test]
    fn renames() {
        let scratch = Scratch::new("rename");
        let dir = &scratch.0;
        fs::write(dir.join("a.txt"), "").unwrap();
        fs::write(dir.join("b.txt"), "").unwrap();
        assert!(rename(&dir.join("a.txt"), "b.txt").error.is_some());
        assert!(rename(&dir.join("a.txt"), "").error.is_some());
        assert!(rename(&dir.join("a.txt"), "x/y").error.is_some());
        // Only the case changes, which a case-insensitive disk sees as the same file.
        let renamed = rename(&dir.join("a.txt"), "A.txt");
        assert_eq!(renamed.paths, [dir.join("A.txt")]);
        assert_eq!(names(dir), ["A.txt", "b.txt"]);
        // And back, without taking it for a different file that has the name.
        let undone = renamed.undo.unwrap().apply(&Progress::default());
        assert!(undone.error.is_none());
        assert_eq!(names(dir), ["a.txt", "b.txt"]);
        undone.undo.unwrap().apply(&Progress::default());
        assert_eq!(names(dir), ["A.txt", "b.txt"]);
    }
}
