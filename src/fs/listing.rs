//! Listing folders with `getattrlistbulk`, for the folder view and for Storage, and watching them
//! for changes.

use std::{
    ffi::OsStr,
    fs, io,
    os::{
        fd::AsRawFd as _,
        unix::{ffi::OsStrExt as _, fs::OpenOptionsExt as _, fs::PermissionsExt as _},
    },
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use gpui_kit::{BackgroundExecutor, SharedString};
use notify::Watcher as _;

use super::{Category, alias_target, describe, extension, is_package};

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: SharedString,
    pub path: PathBuf,
    /// Where a Finder alias leads. Unlike a symlink, an alias can't be gone through as part of a
    /// path, so opening it goes here instead.
    pub alias: Option<PathBuf>,
    /// A folder Mu navigates into (packages like `.app` open instead).
    pub folder: bool,
    /// A folder that opens like a file, such as an `.app`.
    pub package: bool,
    /// A symlink or a Finder alias, which shows what it leads to.
    pub is_link: bool,
    /// It wears an icon of its own, pasted on in Finder.
    pub custom_icon: bool,
    /// Locked in Finder, so it can't be changed, renamed or trashed.
    pub locked: bool,
    /// What's in it is in iCloud and not downloaded, and reading it downloads it.
    pub cloud_only: bool,
    pub hidden: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub category: Category,
    pub kind: SharedString,
}

impl Entry {
    /// Where opening it goes: what an alias leads to, or else the item itself.
    pub fn target(&self) -> &Path {
        self.alias.as_deref().unwrap_or(&self.path)
    }
}

/// Lists `dir` with `getattrlistbulk`, which hands over every item's details in a few calls instead
/// of a `stat` each.
pub fn list_dir(dir: &Path) -> io::Result<Vec<Entry>> {
    let request = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_RETURNED_ATTRS
            | ATTR_CMN_ERROR
            | libc::ATTR_CMN_NAME
            | libc::ATTR_CMN_OBJTYPE
            | libc::ATTR_CMN_MODTIME
            | libc::ATTR_CMN_FNDRINFO
            | libc::ATTR_CMN_ACCESSMASK
            | libc::ATTR_CMN_FLAGS,
        volattr: 0,
        dirattr: libc::ATTR_DIR_MOUNTSTATUS,
        fileattr: libc::ATTR_FILE_DATALENGTH,
        forkattr: 0,
    };
    let mut entries = Vec::new();
    bulk(dir, request, 0, |record| entries.extend(entry(dir, record)))?;
    Ok(entries)
}

/// What an item takes up on disk.
pub struct Usage {
    pub name: Box<OsStr>,
    /// A folder on the same disk, whose size is what's in it. Links and other disks aren't.
    pub folder: bool,
    /// The bytes only this file has.
    pub size: u64,
    /// The bytes it shares with its clones or its other hard links, which count once among them.
    pub shared: Option<(Shared, u64)>,
}

/// What files sharing their bytes have in common.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Shared {
    /// Copies made by cloning, until they're changed.
    Clone(u64),
    /// Hard links to one file.
    Links(u64),
}

/// Lists what's in `dir` and the room each file takes, like `list_dir` but with only that.
pub fn list_usage(dir: &Path) -> io::Result<Vec<Usage>> {
    let request = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_RETURNED_ATTRS
            | ATTR_CMN_ERROR
            | libc::ATTR_CMN_NAME
            | libc::ATTR_CMN_OBJTYPE
            | libc::ATTR_CMN_FILEID,
        volattr: 0,
        dirattr: libc::ATTR_DIR_MOUNTSTATUS,
        fileattr: libc::ATTR_FILE_LINKCOUNT | libc::ATTR_FILE_ALLOCSIZE,
        forkattr: libc::ATTR_CMNEXT_PRIVATESIZE | libc::ATTR_CMNEXT_CLONEID,
    };
    let mut items = Vec::new();
    bulk(dir, request, libc::FSOPT_ATTR_CMN_EXTENDED, |bytes| {
        let mut record = Record { bytes, at: size_of::<u32>() };
        let Some(returned) = record.next::<libc::attribute_set_t>(true) else { return };
        let has = |attr| returned.commonattr & attr != 0;
        if has(ATTR_CMN_ERROR) {
            return;
        }
        let Some(name) = record.name(has(libc::ATTR_CMN_NAME)) else { return };
        let object = record.next::<u32>(has(libc::ATTR_CMN_OBJTYPE)).unwrap_or(0);
        let file = record.next::<u64>(has(libc::ATTR_CMN_FILEID)).unwrap_or(0);
        let mount = record.next::<u32>(returned.dirattr & libc::ATTR_DIR_MOUNTSTATUS != 0).unwrap_or(0);
        let links = record.next::<u32>(returned.fileattr & libc::ATTR_FILE_LINKCOUNT != 0).unwrap_or(1);
        let size = record.next::<i64>(returned.fileattr & libc::ATTR_FILE_ALLOCSIZE != 0).unwrap_or(0).max(0) as u64;
        // Disks that can't clone don't say, and then nothing's shared.
        let private = record.next::<i64>(returned.forkattr & libc::ATTR_CMNEXT_PRIVATESIZE != 0);
        let clone = record.next::<u64>(returned.forkattr & libc::ATTR_CMNEXT_CLONEID != 0);
        let private = private.map_or(size, |private| (private.max(0) as u64).min(size));
        let (size, shared) = match clone {
            _ if links > 1 => (0, Some((Shared::Links(file), size))),
            Some(clone) if private < size => (private, Some((Shared::Clone(clone), size - private))),
            _ => (size, None),
        };
        items.push(Usage {
            name: OsStr::from_bytes(name).into(),
            folder: object == VDIR && mount & libc::DIR_MNTSTATUS_MNTPOINT == 0,
            size,
            shared,
        });
    })?;
    Ok(items)
}

/// Asks for `request` about everything in `dir`, a buffer at a time, handing each item's record to
/// `each`.
fn bulk(dir: &Path, mut request: libc::attrlist, options: u32, mut each: impl FnMut(&[u8])) -> io::Result<()> {
    let folder = fs::OpenOptions::new().read(true).custom_flags(libc::O_DIRECTORY).open(dir)?;
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let count = unsafe {
            libc::getattrlistbulk(
                folder.as_raw_fd(),
                (&raw mut request).cast(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                options.into(),
            )
        };
        match count {
            ..0 => return Err(io::Error::last_os_error()),
            0 => return Ok(()),
            _ => {}
        }
        let mut rest = buffer.as_slice();
        for _ in 0..count {
            let length = Record { bytes: rest, at: 0 }.next::<u32>(true).unwrap_or(0) as usize;
            let (record, after) = rest.split_at(length.min(rest.len()));
            each(record);
            rest = after;
        }
    }
}

/// An item's error code, which `libc` doesn't name.
const ATTR_CMN_ERROR: libc::attrgroup_t = 0x2000_0000;
/// A folder that's really elsewhere, like `/Users` on the startup disk. Also not in `libc`.
const SF_FIRMLINK: u32 = 0x0080_0000;
/// A file whose contents are only in the cloud, like iCloud Drive's that aren't downloaded.
pub const SF_DATALESS: u32 = 0x4000_0000;

/// Kinds of file system object, from `sys/vnode.h`.
const VREG: u32 = 1;
const VDIR: u32 = 2;
const VLNK: u32 = 5;

/// One item's record from `getattrlistbulk`: its length, which details it has, then those details
/// in the order of their flags. An item it couldn't read is left out.
fn entry(dir: &Path, bytes: &[u8]) -> Option<Entry> {
    let mut record = Record { bytes, at: size_of::<u32>() };
    let returned: libc::attribute_set_t = record.next(true)?;
    let has = |attr| returned.commonattr & attr != 0;
    if has(ATTR_CMN_ERROR) {
        return None;
    }
    let raw_name = record.name(has(libc::ATTR_CMN_NAME))?;
    let name = String::from_utf8_lossy(raw_name).into_owned();
    let object = record.next::<u32>(has(libc::ATTR_CMN_OBJTYPE))?;
    let mut modified = record.next::<libc::timespec>(has(libc::ATTR_CMN_MODTIME)).and_then(|time| {
        SystemTime::UNIX_EPOCH.checked_add(Duration::new(time.tv_sec.try_into().ok()?, time.tv_nsec as u32))
    });
    let finder_info = record.next::<[u8; 32]>(has(libc::ATTR_CMN_FNDRINFO)).unwrap_or([0; 32]);
    let mode = record.next::<u32>(has(libc::ATTR_CMN_ACCESSMASK)).unwrap_or(0);
    let flags = record.next::<u32>(has(libc::ATTR_CMN_FLAGS)).unwrap_or(0);
    let mount = record.next::<u32>(returned.dirattr & libc::ATTR_DIR_MOUNTSTATUS != 0).unwrap_or(0);
    let mut size = record.next::<u64>(returned.fileattr & libc::ATTR_FILE_DATALENGTH != 0).unwrap_or(0);

    // From the name's bytes, which are the file's even when they aren't valid UTF-8.
    let path = dir.join(OsStr::from_bytes(raw_name));
    // An alias is a file its Finder info marks as one (kIsAlias, in the flags after the type and
    // creator).
    let alias = (object == VREG && finder_info[8] & 0x80 != 0).then(|| alias_target(&path));
    let is_link = object == VLNK || alias.is_some();
    let alias = alias.flatten();
    // Where a link leads, whose name says what it is.
    let resolved = if object == VLNK { fs::canonicalize(&path).ok() } else { alias.clone() };
    let target = resolved.as_deref().unwrap_or(&path);
    let (mut is_dir, mut executable) = (object == VDIR, object == VREG && mode & 0o111 != 0);
    // A link shows what it points to, and a firmlink or mount point what's there, not the folder
    // it covers.
    let elsewhere = is_link || flags & SF_FIRMLINK != 0 || mount & libc::DIR_MNTSTATUS_MNTPOINT != 0;
    if elsewhere && let Ok(target) = fs::metadata(target) {
        let target_executable = target.is_file() && target.permissions().mode() & 0o111 != 0;
        (is_dir, executable, size, modified) =
            (target.is_dir(), target_executable, target.len(), target.modified().ok());
    }
    let target_name = target.file_name().unwrap_or_default().to_string_lossy();
    // Asking takes a moment, so only folders with an extension or the bundle bit (kHasBundle) ask.
    let package = is_dir && (extension(&target_name).is_some() || finder_info[8] & 0x20 != 0) && is_package(target);
    let folder = is_dir && !package;
    let (category, kind) = describe(&target_name, folder, package, executable);
    Some(Entry {
        hidden: name.starts_with('.') || flags & libc::UF_HIDDEN != 0,
        name: name.into(),
        path,
        alias,
        folder,
        package,
        is_link,
        // A custom icon (kHasCustomIcon). A link shows the icon of what it leads to instead.
        custom_icon: !is_link && finder_info[8] & 0x04 != 0,
        locked: flags & libc::UF_IMMUTABLE != 0,
        cloud_only: flags & SF_DATALESS != 0,
        // Apps are folders too, and a folder's own size is just its list of names.
        size: if is_dir { 0 } else { size },
        modified,
        category,
        kind,
    })
}

/// Reads the details out of a record one after another. Records only keep them 4-byte aligned.
struct Record<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Record<'a> {
    /// The next detail, a plain C value, when the record has it.
    fn next<T: Copy>(&mut self, present: bool) -> Option<T> {
        if !present {
            return None;
        }
        let field = self.bytes.get(self.at..self.at + size_of::<T>())?;
        self.at += size_of::<T>();
        Some(unsafe { field.as_ptr().cast::<T>().read_unaligned() })
    }

    /// The name's bytes, which sit further on. Its offset counts from where its reference sits,
    /// and its length counts the final NUL.
    fn name(&mut self, present: bool) -> Option<&'a [u8]> {
        let at = self.at;
        let name: libc::attrreference_t = self.next(present)?;
        let start = at.checked_add_signed(name.attr_dataoffset as isize)?;
        self.bytes.get(start..start + (name.attr_length as usize).saturating_sub(1))
    }
}

/// Watches `dir` (not its subfolders) from now until the changes drop. A change waits `settle` for
/// the rest of its burst, like a download writing or a big copy landing, so they come together. A
/// watcher takes milliseconds to start, so it starts and stops in the background.
pub fn watch(dir: &Path, settle: Duration, executor: &BackgroundExecutor) -> Changes {
    let since = SystemTime::now();
    let (tx, rx) = smol::channel::unbounded();
    let dir = dir.to_owned();
    executor
        .spawn(async move {
            let started = tx.clone();
            let _watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                if let Ok(event) = event
                    && !event.kind.is_access()
                {
                    tx.send_blocking(event.paths).ok();
                }
            })
            .and_then(|mut watcher| {
                watcher.watch(&dir, notify::RecursiveMode::NonRecursive)?;
                Ok(watcher)
            });
            // Items added, removed or renamed while the watcher started went unseen, so they count as
            // a change. Some file systems keep times to the second, or to two.
            if fs::metadata(&dir)
                .and_then(|metadata| metadata.modified())
                .is_ok_and(|modified| modified + Duration::from_secs(2) >= since)
            {
                started.send(Vec::new()).await.ok();
            }
            // Until the changes drop.
            started.closed().await;
        })
        .detach();
    Changes { rx, settle }
}

/// What changes in a folder `watch`es.
pub struct Changes {
    rx: smol::channel::Receiver<Vec<PathBuf>>,
    settle: Duration,
}

impl Changes {
    /// What changed next, once its burst has settled. None once the watcher's gone.
    pub async fn next(&self) -> Option<Vec<PathBuf>> {
        let mut changed = self.rx.recv().await.ok()?;
        smol::Timer::after(self.settle).await;
        while let Ok(more) = self.rx.try_recv() {
            changed.extend(more);
        }
        Some(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{ffi::CString, os::unix::ffi::OsStringExt as _};

    use objc2_foundation::{NSURL, NSURLBookmarkCreationOptions};

    use crate::fs::{Scratch, file_url};

    #[test]
    fn lists_what_stat_sees() {
        let scratch = Scratch::new("list");
        let dir = &scratch.0;
        fs::write(dir.join("notes.txt"), "hello").unwrap();
        fs::write(dir.join(".hidden"), "").unwrap();
        fs::write(dir.join("ünïcödé 日本.md"), "x").unwrap();
        fs::write(dir.join("run"), "#!/bin/sh").unwrap();
        fs::set_permissions(dir.join("run"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::create_dir(dir.join("folder")).unwrap();
        fs::create_dir(dir.join("Tool.app")).unwrap();
        fs::create_dir(dir.join("Bundle.zzz")).unwrap();
        fs::create_dir(dir.join("folder.d")).unwrap();
        fs::write(dir.join("custom.txt"), "").unwrap();
        fs::write(dir.join("locked.txt"), "").unwrap();
        // Finder's flags: the bundle bit and the custom icon one.
        let c_path = |name: &str| CString::new(dir.join(name).into_os_string().into_vec()).unwrap();
        for (name, flags) in [("Bundle.zzz", 0x20), ("custom.txt", 0x04)] {
            let mut info = [0u8; 32];
            info[8] = flags;
            let set = unsafe {
                libc::setxattr(c_path(name).as_ptr(), c"com.apple.FinderInfo".as_ptr(), info.as_ptr().cast(), 32, 0, 0)
            };
            assert_eq!(set, 0);
        }
        std::os::unix::fs::symlink("Tool.app", dir.join("to-tool")).unwrap();
        std::os::unix::fs::symlink("notes.txt", dir.join("to-notes")).unwrap();
        std::os::unix::fs::symlink("folder", dir.join("to-folder")).unwrap();
        std::os::unix::fs::symlink("nowhere", dir.join("broken")).unwrap();
        // Finder aliases, written the way Finder writes them.
        for (target, alias) in [("notes.txt", "notes alias"), ("folder", "folder alias")] {
            let bookmark = file_url(&dir.join(target))
                .bookmarkDataWithOptions_includingResourceValuesForKeys_relativeToURL_error(
                    NSURLBookmarkCreationOptions::SuitableForBookmarkFile,
                    None,
                    None,
                )
                .unwrap();
            NSURL::writeBookmarkData_toURL_options_error(&bookmark, &file_url(&dir.join(alias)), 0).unwrap();
        }
        // Unlocked again right away, or it couldn't be cleaned up.
        assert_eq!(unsafe { libc::chflags(c_path("locked.txt").as_ptr(), libc::UF_IMMUTABLE) }, 0);
        let entries = list_dir(dir);
        assert_eq!(unsafe { libc::chflags(c_path("locked.txt").as_ptr(), 0) }, 0);
        let mut entries = entries.unwrap();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_ref()).collect();
        assert_eq!(
            names,
            [
                ".hidden",
                "Bundle.zzz",
                "Tool.app",
                "broken",
                "custom.txt",
                "folder",
                "folder alias",
                "folder.d",
                "locked.txt",
                "notes alias",
                "notes.txt",
                "run",
                "to-folder",
                "to-notes",
                "to-tool",
                "ünïcödé 日本.md"
            ]
        );
        let get = |name: &str| entries.iter().find(|e| e.name == name).unwrap();
        for entry in &entries {
            let meta = fs::metadata(entry.target()).or_else(|_| fs::symlink_metadata(&entry.path)).unwrap();
            assert_eq!(entry.modified, meta.modified().ok(), "{}", entry.name);
            assert_eq!(entry.path, dir.join(entry.name.as_ref()));
        }
        assert!(get(".hidden").hidden && !get("notes.txt").hidden);
        assert_eq!(get("notes.txt").size, 5);
        assert!(get("folder").folder && !get("folder").package);
        assert!(get("Tool.app").package && !get("Tool.app").folder);
        // What's a package is up to macOS, not the extension alone.
        assert!(get("Bundle.zzz").package && get("folder.d").folder);
        assert!(get("to-tool").package && get("to-tool").category == Category::App);
        assert!(get("custom.txt").custom_icon && !get("notes.txt").custom_icon);
        assert!(get("locked.txt").locked && !get("notes.txt").locked);
        assert_eq!(get("run").category, Category::Executable);
        assert_eq!(get("notes.txt").category, Category::Text);
        let link = get("to-notes");
        assert!(link.is_link && !link.folder && link.size == 5);
        assert!(get("to-folder").is_link && get("to-folder").folder);
        assert!(get("broken").is_link && !get("broken").folder);
        // An alias is what it leads to, and opens there.
        let alias = get("notes alias");
        assert!(alias.is_link && alias.size == 5 && alias.category == Category::Text);
        assert_eq!(alias.target(), fs::canonicalize(dir.join("notes.txt")).unwrap());
        let alias = get("folder alias");
        assert!(alias.is_link && alias.folder);
        assert_eq!(alias.target(), fs::canonicalize(dir.join("folder")).unwrap());
        assert!(list_dir(&dir.join("missing")).is_err_and(|error| error.kind() == io::ErrorKind::NotFound));
    }
}
