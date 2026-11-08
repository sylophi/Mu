//! The Trash: moving items there through the system, noting where each came from so it can be put
//! back, and emptying it.

use std::{
    collections::HashMap,
    ffi::{CStr, OsString},
    fs, io, iter,
    os::{
        macos::fs::MetadataExt as _,
        unix::ffi::{OsStrExt as _, OsStringExt as _},
    },
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result, bail};
use objc2::rc::autoreleasepool;
use objc2_foundation::{NSFileManager, NSSearchPathDirectory, NSSearchPathDomainMask};

use super::{
    Cancelled, Done, Progress, Undo,
    admin::{self, Step},
    c_path, delete_path, disk_name, display_name,
    ds_store::{records, text},
    each, file_url, holds_volume, is_read_only, read_dirs, same_volume, trash_dir, unique_path, volume_root, volumes,
    xattr,
};

/// The Trashes Finder shows as one: the startup disk's, then each other disk's. A disk's holds what
/// was trashed from it, and only comes to be when something first is.
pub fn trash_dirs() -> Vec<PathBuf> {
    // Things on a share from another computer are deleted right away instead.
    let disks = volumes().into_iter().skip(1).filter(|volume| !volume.network);
    iter::once(trash_dir()).chain(disks.map(|disk| disk_trash(&disk.path)).filter(|dir| dir.is_dir())).collect()
}

/// Where the Trash is on the disk mounted at `root`, if it's not the startup disk.
fn disk_trash(root: &Path) -> PathBuf {
    root.join(".Trashes").join(unsafe { libc::getuid() }.to_string())
}

/// What's in the Trashes, leaving out the files Finder keeps there. Only the startup disk's not
/// opening fails it.
pub fn trash_items() -> io::Result<Vec<PathBuf>> {
    read_dirs(&trash_dirs(), items_in)
}

fn items_in(dir: &Path) -> io::Result<Vec<PathBuf>> {
    let entries = fs::read_dir(dir)?.flatten();
    Ok(entries
        .filter(|entry| !matches!(entry.file_name().to_str(), Some(".DS_Store" | ".localized")))
        .map(|entry| entry.path())
        .collect())
}

/// Where an item Mu moved to the Trash came from, kept on the item so it lasts.
const ORIGIN: &CStr = c"io.github.sylophi.mu.origin";

/// Where `path`, in the Trash, was before Mu moved it there.
fn origin(path: &Path) -> Option<PathBuf> {
    xattr(path, ORIGIN, libc::XATTR_NOFOLLOW).map(|value| PathBuf::from(OsString::from_vec(value)))
}

/// Where items in the Trashes came from, whether Mu or Finder moved them there. What Finder noted is
/// read once for each Trash, so make one of these for all the items at hand.
#[derive(Default)]
pub struct Origins(HashMap<PathBuf, HashMap<String, FinderNote>>);

impl Origins {
    pub fn of(&mut self, path: &Path) -> Option<PathBuf> {
        origin(path).or_else(|| self.noted_by_finder(path))
    }

    fn noted_by_finder(&mut self, path: &Path) -> Option<PathBuf> {
        let trash = path.parent()?;
        let notes = self.0.entry(trash.to_path_buf()).or_insert_with(|| finder_notes(trash));
        let note = notes.get(path.file_name()?.to_str()?)?;
        Some(finder_folder(trash, note.folder.as_ref()?)?.join(note.name.as_ref()?))
    }
}

/// What a Trash's `.DS_Store` says about an item: the folder it came from, from the top of its disk,
/// and the name it had there.
#[derive(Default)]
struct FinderNote {
    folder: Option<String>,
    name: Option<String>,
}

/// What Finder noted about the items in the Trash at `trash`, by their names there.
fn finder_notes(trash: &Path) -> HashMap<String, FinderNote> {
    let mut notes = HashMap::<String, FinderNote>::new();
    let file = fs::read(trash.join(".DS_Store")).unwrap_or_default();
    for (item, code, value) in records(&file).unwrap_or_default() {
        match &code {
            b"ptbL" => notes.entry(item).or_default().folder = Some(text(value)),
            b"ptbN" => notes.entry(item).or_default().name = Some(text(value)),
            _ => {}
        }
    }
    notes
}

/// The folder that Finder noted as `folder` for an item in the Trash at `trash`. On the startup disk,
/// it can go by way of the volume that really holds the folder.
fn finder_folder(trash: &Path, folder: &str) -> Option<PathBuf> {
    let folder = folder.trim_start_matches('/');
    if trash == trash_dir() {
        return Some(Path::new("/").join(folder.strip_prefix("System/Volumes/Data/").unwrap_or(folder)));
    }
    Some(trash.parent()?.parent()?.join(folder))
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

/// Splits `paths` into those that can go in the Trash and those on a volume without one, which can
/// only be deleted, as on a share from another computer.
pub fn split_by_trash(paths: Vec<PathBuf>) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut lacking = HashMap::new();
    paths.into_iter().partition(|path| {
        // Its folder, as asking about a symlink would ask about what it points to.
        let dir = path.parent().unwrap_or(path).to_path_buf();
        !*lacking.entry(dir).or_insert_with_key(|dir| lacks_trash(dir))
    })
}

/// Whether the volume `dir` is on has no Trash. macOS only says so of a volume's top: anywhere else
/// on a disk, it says the same as for a share. It makes the disk's Trash if it's yet to have one,
/// or it wouldn't tell them apart either. A read-only volume has none, but nothing goes from it
/// anyway.
pub(super) fn lacks_trash(dir: &Path) -> bool {
    const NS_FEATURE_UNSUPPORTED_ERROR: isize = 3328;
    let Some(root) = volume_root(dir).filter(|_| !is_read_only(dir)) else { return false };
    autoreleasepool(|_| {
        NSFileManager::defaultManager()
            .URLForDirectory_inDomain_appropriateForURL_create_error(
                NSSearchPathDirectory::TrashDirectory,
                NSSearchPathDomainMask::UserDomainMask,
                Some(&file_url(&root)),
                true,
            )
            .is_err_and(|error| error.code() == NS_FEATURE_UNSUPPORTED_ERROR)
    })
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

/// Whether macOS protects `path` from changes, even by an administrator, as it does its own apps.
pub fn is_restricted(path: &Path) -> bool {
    const SF_RESTRICTED: u32 = 0x0008_0000;
    fs::symlink_metadata(path).is_ok_and(|meta| meta.st_flags() & SF_RESTRICTED != 0)
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

/// Moves `paths` to the Trash, and deletes for good those on a volume without one. Callers warn
/// about those first.
pub fn trash_or_delete(paths: &[PathBuf]) -> Done {
    let (trashable, doomed) = split_by_trash(paths.to_vec());
    let trashed = trash(&trashable);
    // Turned down an administrator's password: nothing more goes, least of all for good.
    if trashed.error.as_ref().is_some_and(|error| error.is::<Cancelled>()) {
        return trashed;
    }
    let deleted = delete(&doomed);
    Done { error: trashed.error.or(deleted.error), ..trashed }
}

/// Deletes `paths` for good, even what's locked, but never a whole disk.
pub fn delete(paths: &[PathBuf]) -> Done {
    let (_, error) =
        each(paths, |path| delete_one(path).with_context(|| format!("Couldn't delete “{}”", display_name(path))));
    Done { error, ..Done::default() }
}

pub(super) fn delete_one(path: &Path) -> Result<()> {
    if holds_volume(path) {
        bail!("it's a disk, or has one in it");
    }
    let Err(error) = delete_path(path) else { return Ok(()) };
    // What a locked item fails with.
    if error.downcast_ref::<io::Error>().and_then(io::Error::raw_os_error) != Some(libc::EPERM) {
        return Err(error);
    }
    unlock_all(path)?;
    delete_path(path)
}

/// Unlocks `path` and everything in it.
fn unlock_all(path: &Path) -> Result<()> {
    if is_locked(path) {
        set_locked(path, false)?;
    }
    if fs::symlink_metadata(path)?.is_dir() {
        for entry in fs::read_dir(path)? {
            unlock_all(&entry?.path())?;
        }
    }
    Ok(())
}

/// Moves items in the Trash back to where they came from.
pub fn put_back(paths: &[PathBuf]) -> Done {
    let mut origins = Origins::default();
    let (moves, unknown) = each(paths, |path| {
        let origin =
            origins.of(path).with_context(|| format!("Mu doesn't know where “{}” came from", display_name(path)))?;
        // Finder leaves a locked item locked in the Trash, where it can't be moved.
        if is_locked(path) {
            set_locked(path, false)?;
        }
        Ok((path.clone(), origin))
    });
    let done = Undo::PutBack(moves).apply(&Progress::default());
    Done { error: unknown.or(done.error), ..done }
}

/// Deletes everything in the Trashes for good.
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
    fn delete_goes_through_locks() {
        let scratch = Scratch::new("delete");
        let dir = &scratch.0;
        let lock = |path: &Path| set_locked(path, true).unwrap();

        fs::write(dir.join("a.txt"), "a").unwrap();
        let locked = dir.join("locked.txt");
        fs::write(&locked, "").unwrap();
        lock(&locked);
        let folder = dir.join("folder");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("inside.txt"), "").unwrap();
        lock(&folder.join("inside.txt"));
        lock(&folder);

        let done = delete(&[dir.join("a.txt"), locked, folder]);
        assert!(done.error.is_none() && done.undo.is_none());
        assert!(names(dir).is_empty());

        assert!(delete(&[dir.join("missing")]).error.is_some());
    }

    #[test]
    fn disks_are_told_from_folders() {
        assert!(holds_volume(Path::new("/")) && holds_volume(Path::new("/System/Volumes")));
        assert!(!holds_volume(&Scratch::new("holds-volume").0));
    }

    #[test]
    fn a_folder_on_a_disk_can_use_the_trash() {
        let scratch = Scratch::new("split");
        let a = scratch.0.join("a.txt");
        assert_eq!(split_by_trash(vec![a.clone()]), (vec![a], Vec::new()));
    }

    #[test]
    fn puts_back_what_finder_trashed() {
        let scratch = Scratch::new("put-back-finder");
        let trash = scratch.0.join(".Trashes/501");
        fs::create_dir_all(&trash).unwrap();
        fs::write(trash.join(".DS_Store"), include_bytes!("fixtures/finder-trash.ds_store")).unwrap();

        // Finder renamed this one because of another `same.txt` in the Trash.
        let renamed = trash.join("same 9.04.38\u{202f}AM.txt");
        fs::write(&renamed, "two").unwrap();
        let back = put_back(&[renamed]);
        assert!(back.error.is_none());
        let to = scratch.0.join("two/same.txt");
        assert_eq!(back.paths, std::slice::from_ref(&to));
        assert_eq!(back.undo, Some(Undo::Trash(vec![to.clone()])));
        assert_eq!(fs::read_to_string(&to).unwrap(), "two");

        // Finder leaves a locked item locked in the Trash. Like one Mu trashed, it comes back unlocked.
        let locked = trash.join("Case.TXT");
        fs::write(&locked, "").unwrap();
        set_locked(&locked, true).unwrap();
        let back = put_back(std::slice::from_ref(&locked));
        assert!(back.error.is_none(), "{:?}", back.error);
        assert!(!is_locked(&scratch.0.join("one/Case.TXT")));

        // Finder didn't note this one.
        let unknown = trash.join("unknown.txt");
        fs::write(&unknown, "").unwrap();
        assert_eq!(Origins::default().of(&unknown), None);

        // What Mu noted comes first: a `.DS_Store` can keep a record after its item has gone.
        let same = trash.join("same.txt");
        fs::write(&same, "").unwrap();
        assert_eq!(Origins::default().of(&same), Some(scratch.0.join("one/same.txt")));
        set_origin(&same, Path::new("/elsewhere/same.txt"));
        assert_eq!(Origins::default().of(&same), Some(PathBuf::from("/elsewhere/same.txt")));
    }

    #[test]
    fn finder_folders_on_each_kind_of_disk() {
        let folder = |trash: &Path, folder| finder_folder(trash, folder).unwrap();
        let (startup, disk) = (trash_dir(), Path::new("/Volumes/Disk/.Trashes/501"));
        assert_eq!(folder(&startup, "Applications/"), Path::new("/Applications/"));
        assert_eq!(folder(&startup, "System/Volumes/Data/Users/a/"), Path::new("/Users/a/"));
        assert_eq!(folder(disk, "/one-offs/nest/"), Path::new("/Volumes/Disk/one-offs/nest/"));
        assert_eq!(folder(disk, "/System/Volumes/Data/a/"), Path::new("/Volumes/Disk/System/Volumes/Data/a/"));
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

        // Mu can't tell where something goes that neither it nor Finder noted.
        let unknown = put_back(std::slice::from_ref(&a));
        assert!(unknown.error.is_some() && unknown.undo.is_none());
        assert!(a.exists());
    }
}
