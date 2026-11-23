//! The steps file operations are made of, like copying, moving and deleting an item, as an
//! administrator if need be, with how far they've got. And the simple operations: renaming, and
//! making a folder or a file.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    ffi::{CStr, OsStr, c_void},
    fs,
    io::{self, Write as _},
    os::{macos::fs::MetadataExt as _, unix::ffi::OsStrExt as _},
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{
            AtomicBool, AtomicU64,
            Ordering::{Acquire, Relaxed, Release},
        },
    },
};

use anyhow::{Context as _, Result, anyhow, bail};

use super::{
    Done,
    admin::{self, Step},
    c_path, display_name, list_usage,
    underway::Published,
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
    /// Bytes to copy, counted while copying goes on.
    total: AtomicU64,
    /// Whether `total` is still being counted.
    counting: AtomicBool,
    /// How many bytes are in each folder counted so far, for its own progress.
    folders: Mutex<HashMap<PathBuf, u64>>,
    copied: AtomicU64,
    cancelled: AtomicBool,
}

impl Progress {
    /// Bytes copied so far, and of how many once they're all counted.
    pub fn bytes(&self) -> (u64, Option<u64>) {
        (self.copied.load(Relaxed), (!self.counting.load(Acquire)).then(|| self.total.load(Relaxed)))
    }

    /// Stops the operation at the next chance. What's been copied stays, except a partial item.
    pub fn cancel(&self) {
        self.cancelled.store(true, Relaxed);
    }

    pub(super) fn check(&self) -> Result<()> {
        if self.cancelled.load(Relaxed) { Err(Cancelled.into()) } else { Ok(()) }
    }

    /// Runs `work`, counting what copying `paths` will write alongside it, so copying starts right
    /// away rather than once a slow disk has told the size of everything.
    pub(super) fn expecting<'a, R>(
        &self,
        paths: impl IntoIterator<Item = &'a PathBuf> + Send,
        work: impl FnOnce() -> R,
    ) -> R {
        self.counting.store(true, Relaxed);
        let finished = &AtomicBool::new(false);
        let result = std::thread::scope(|scope| {
            scope.spawn(move || {
                for path in paths {
                    let size = self.measure(path, finished);
                    self.total.fetch_add(size, Relaxed);
                }
                if !finished.load(Relaxed) {
                    self.counting.store(false, Release);
                }
            });
            let result = work();
            finished.store(true, Relaxed);
            result
        });
        // Done before the count was, it's at least what was copied.
        self.total.fetch_max(self.copied.load(Relaxed), Relaxed);
        self.counting.store(false, Release);
        result
    }

    /// How many bytes are in `path`, all the way down, keeping each folder's for later, until the
    /// work's `finished` or cancelled.
    fn measure(&self, path: &Path, finished: &AtomicBool) -> u64 {
        match fs::symlink_metadata(path) {
            Ok(meta) if meta.is_dir() => self.measure_folder(path, finished),
            Ok(meta) => meta.len(),
            Err(_) => 0,
        }
    }

    /// Like `measure`, for the folder at `dir`, its items listed in bulk, as each call costs a round
    /// trip on a network disk. Only a folder counted all the way is kept.
    fn measure_folder(&self, dir: &Path, finished: &AtomicBool) -> u64 {
        let stopped = || finished.load(Relaxed) || self.check().is_err();
        if stopped() {
            return 0;
        }
        let items = list_usage(dir).unwrap_or_default();
        let size = items
            .iter()
            .map(|item| match item.folder {
                true => self.measure_folder(&dir.join(&*item.name), finished),
                false => item.length,
            })
            .sum();
        if !stopped() {
            self.folders.lock().unwrap().insert(dir.to_path_buf(), size);
        }
        size
    }

    /// How many bytes are in the folder at `path`, once that's been counted.
    fn folder(&self, path: &Path) -> Option<u64> {
        self.folders.lock().unwrap().get(path).copied()
    }

    /// Counts `path`, which was expected but won't be copied after all, as done.
    pub(super) fn skip(&self, path: &Path) {
        self.copied.fetch_add(self.measure(path, &AtomicBool::new(false)), Relaxed);
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
    let Err(error) = copyfile(from, to, progress, true) else { return Ok(()) };
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
/// counts what it writes in `progress`. With `publish`, it tells how far it's got with each item
/// it's copying, all the way down, as it gets to them.
pub fn copyfile(from: &Path, to: &Path, progress: &Progress, publish: bool) -> io::Result<()> {
    let (from_c, to_c) = (c_path(from)?, c_path(to)?);
    let flags = libc::COPYFILE_METADATA
        | libc::COPYFILE_DATA
        | libc::COPYFILE_RECURSIVE
        | libc::COPYFILE_CLONE
        | libc::COPYFILE_NOFOLLOW_SRC;
    let tracker = Tracker { progress, file: Default::default(), items: publish.then(Default::default) };
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

/// What `copy_status` needs: where to count, and the file it's copying. And when publishing, the
/// items it's in the middle of copying, from the outermost in.
struct Tracker<'a> {
    progress: &'a Progress,
    file: Cell<File>,
    items: Option<RefCell<Vec<Item>>>,
}

/// The file being copied: how big it is, how much of it has been counted, and whether it's told
/// how far it's got.
#[derive(Clone, Copy, Default)]
struct File {
    size: u64,
    counted: u64,
    published: bool,
}

/// An item being copied, and how far that's got, counting from what had been copied before it.
struct Item {
    published: Published,
    before: u64,
    /// Where a folder whose size is still being counted is copied from, to look it up by.
    pending: Option<PathBuf>,
}

impl Tracker<'_> {
    /// Starts telling how far copying a folder from `from` to `to` has got.
    fn start_folder(&self, from: &Path, to: &Path) {
        let Some(items) = &self.items else { return };
        let size = self.progress.folder(from);
        let item = Item {
            published: Published::copying(to, size),
            before: self.progress.copied.load(Relaxed),
            pending: size.is_none().then(|| from.to_path_buf()),
        };
        items.borrow_mut().push(item);
    }

    /// Starts on a file copied from `from` to `to`. The outermost item tells of itself right away,
    /// so the copy shows however it goes.
    fn start_file(&self, from: &Path, to: &Path) {
        let size = fs::symlink_metadata(from).map_or(0, |meta| meta.len());
        let mut file = File { size, ..File::default() };
        if let Some(items) = &self.items {
            let mut items = items.borrow_mut();
            if items.is_empty() {
                let before = self.progress.copied.load(Relaxed);
                items.push(Item { published: Published::copying(to, Some(size)), before, pending: None });
                file.published = true;
            }
        }
        self.file.set(file);
    }

    /// Stops telling of the innermost item, which is done: all there, whatever it was counted to be.
    fn finish(&self) {
        let Some(items) = &self.items else { return };
        let Some(item) = items.borrow_mut().pop() else { return };
        let done = self.progress.copied.load(Relaxed) - item.before;
        item.published.set_total(done);
        item.published.set_copied(done);
    }

    /// Counts `bytes` of the current file copied to `to` so far, in all and in each item it's in. A
    /// file inside a folder starts telling how far it's got once it's partway, while one copied in
    /// one go never needs to.
    fn count(&self, bytes: u64, to: Option<&Path>) {
        let mut file = self.file.get();
        let added = bytes.saturating_sub(file.counted);
        let copied = self.progress.copied.fetch_add(added, Relaxed) + added;
        // What had been copied before this file.
        let before = copied - added - file.counted;
        file.counted = bytes;
        let Some(items) = &self.items else { return self.file.set(file) };
        let mut items = items.borrow_mut();
        if let Some(to) = to
            && !file.published
            && bytes < file.size
        {
            let published = Published::copying(to, Some(file.size));
            items.push(Item { published, before, pending: None });
            file.published = true;
        }
        self.file.set(file);
        for item in items.iter_mut() {
            // Folders are counted alongside, so their size may have come in since they started.
            if let Some(from) = &item.pending
                && let Some(size) = self.progress.folder(from)
            {
                item.published.set_total(size);
                item.pending = None;
            }
            item.published.set_copied(copied - item.before);
        }
    }

    /// Counts the current file as done, as a cloned one writes nothing, and stops telling of it.
    fn finish_file(&self) {
        let file = self.file.get();
        self.count(file.size, None);
        if file.published {
            self.finish();
        }
    }
}

/// Hears from `copyfile` as it goes: counts the bytes it writes and tells of the items it copies,
/// and stops it at the first error, as it would by itself, or when cancelled.
extern "C" fn copy_status(
    what: i32,
    stage: i32,
    state: libc::copyfile_state_t,
    from: *const i8,
    to: *const i8,
    context: *mut c_void,
) -> i32 {
    let tracker = unsafe { &*context.cast::<Tracker>() };
    let path = |path: *const i8| {
        (!path.is_null()).then(|| Path::new(OsStr::from_bytes(unsafe { CStr::from_ptr(path) }.to_bytes())))
    };
    match (what, stage) {
        (_, libc::COPYFILE_ERR) => return libc::COPYFILE_QUIT,
        (libc::COPYFILE_RECURSE_DIR, libc::COPYFILE_START) => {
            if let (Some(from), Some(to)) = (path(from), path(to)) {
                tracker.start_folder(from, to);
            }
        }
        (libc::COPYFILE_RECURSE_DIR_CLEANUP, libc::COPYFILE_FINISH) => tracker.finish(),
        (libc::COPYFILE_RECURSE_FILE, libc::COPYFILE_START) => {
            if let (Some(from), Some(to)) = (path(from), path(to)) {
                tracker.start_file(from, to);
            }
        }
        (libc::COPYFILE_COPY_DATA, libc::COPYFILE_PROGRESS) => {
            let mut copied: libc::off_t = 0;
            unsafe { libc::copyfile_state_get(state, libc::COPYFILE_STATE_COPIED as u32, (&raw mut copied).cast()) };
            tracker.count(copied as u64, path(to));
        }
        (libc::COPYFILE_RECURSE_FILE, libc::COPYFILE_FINISH) => tracker.finish_file(),
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

/// Saves `bytes` to the file at `path`, as an administrator if need be.
pub fn save(path: &Path, bytes: &[u8]) -> Result<()> {
    admin::session(|| {
        fs::write(path, bytes)
            .or_else(|error| admin::retry(error, Step::Save(path.into(), bytes.to_vec())))
            .with_context(|| format!("Couldn't save “{}”", display_name(path)))
    })
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
    fn counts_while_copying() {
        let scratch = Scratch::new("counts");
        let dir = &scratch.0;
        let (from, to) = (dir.join("from"), dir.join("to"));
        fs::create_dir_all(from.join("inner")).unwrap();
        fs::create_dir(from.join("empty")).unwrap();
        fs::write(from.join("a.bin"), vec![1; 300_000]).unwrap();
        fs::write(from.join("inner/b.bin"), vec![2; 200_000]).unwrap();
        std::os::unix::fs::symlink("a.bin", from.join("link")).unwrap();

        // Each folder's size is kept, for its own progress.
        let counting = Progress::default();
        assert_eq!(counting.measure(&from, &AtomicBool::new(false)), 500_005);
        assert_eq!(counting.folder(&from.join("inner")), Some(200_000));
        assert_eq!(counting.folder(&from.join("empty")), Some(0));

        // Whichever's done first, copying or counting, it all adds up.
        let progress = Progress::default();
        progress.expecting([&from], || copy_path(&from, &to, &progress)).unwrap();
        assert_eq!(progress.bytes(), (500_005, Some(500_005)));
        assert_eq!(names(&to), ["a.bin", "empty", "inner", "link"]);
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
