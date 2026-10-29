//! Disks and shares: which are mounted and how full they are, mounting a disk image and ejecting.

use std::{
    ffi::{CStr, CString, OsStr},
    fs,
    os::{macos::fs::MetadataExt as _, unix::ffi::OsStrExt as _},
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context as _, Result, bail};
use gpui_kit::SharedString;

use super::{Cancelled, display_name, natural_cmp, run};

fn statfs(path: &Path) -> Option<libc::statfs> {
    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    (unsafe { libc::statfs(path.as_ptr(), &mut stat) } == 0).then_some(stat)
}

/// How big the disk `path` is on and how much of it is free, in bytes.
pub fn capacity(path: &Path) -> Option<(u64, u64)> {
    statfs(path).map(|stat| (stat.f_blocks * stat.f_bsize as u64, stat.f_bavail * stat.f_bsize as u64))
}

/// Whether `path` is on a share from another computer.
pub fn is_network(path: &Path) -> bool {
    statfs(path).is_some_and(|stat| stat.f_flags & libc::MNT_LOCAL as u32 == 0)
}

/// Where the volume `path` is on is mounted.
pub fn volume_root(path: &Path) -> Option<PathBuf> {
    let stat = statfs(path)?;
    Some(PathBuf::from(OsStr::from_bytes(unsafe { CStr::from_ptr(stat.f_mntonname.as_ptr()) }.to_bytes())))
}

/// What the volume at `path` is mounted from, like a disk's device or a share's address.
pub fn mounted_from(path: &Path) -> Option<String> {
    let stat = statfs(path)?;
    Some(unsafe { CStr::from_ptr(stat.f_mntfromname.as_ptr()) }.to_string_lossy().into_owned())
}

#[derive(Clone)]
pub struct Volume {
    pub name: SharedString,
    /// Where it's mounted.
    pub path: PathBuf,
    /// Whether it's a share from another computer.
    pub network: bool,
}

/// Mounted volumes, the startup disk first. Like Finder, this skips system volumes marked "don't
/// browse", such as Recovery. It takes what macOS already knows about each mount without asking
/// again, so a share whose server has gone quiet can't hold it up.
pub fn volumes() -> Vec<Volume> {
    let root = PathBuf::from("/");
    let mut volumes = vec![Volume { name: display_name(&root).into(), path: root, network: false }];
    let count = unsafe { libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT) };
    let mut mounts = vec![unsafe { std::mem::zeroed::<libc::statfs>() }; count.max(0) as usize];
    let size = (mounts.len() * size_of::<libc::statfs>()) as i32;
    let count = unsafe { libc::getfsstat(mounts.as_mut_ptr(), size, libc::MNT_NOWAIT) };
    mounts.truncate(count.max(0) as usize);
    let mut found: Vec<_> = mounts
        .iter()
        .filter(|mount| mount.f_flags & libc::MNT_DONTBROWSE as u32 == 0)
        .filter_map(|mount| {
            let path =
                PathBuf::from(OsStr::from_bytes(unsafe { CStr::from_ptr(mount.f_mntonname.as_ptr()) }.to_bytes()));
            (path.parent() == Some(Path::new("/Volumes"))).then(|| Volume {
                name: display_name(&path).into(),
                path,
                network: mount.f_flags & libc::MNT_LOCAL as u32 == 0,
            })
        })
        .collect();
    found.sort_by(|a, b| natural_cmp(&a.name, &b.name));
    volumes.extend(found);
    volumes
}

pub fn same_volume(a: &Path, b: &Path) -> bool {
    match (fs::metadata(a), fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.st_dev() == b.st_dev(),
        _ => false,
    }
}

/// Mounts the disk image at `path` like opening it does, but without Finder showing it, and says
/// where it's mounted. macOS asks for an encrypted one's password, which can be cancelled. An image
/// that's mounted already says where it is.
pub fn mount_image(path: &Path) -> Result<PathBuf> {
    let stdout = match run(Command::new("/usr/sbin/diskutil").args(["image", "attach"]).arg(path)) {
        Ok(stdout) => stdout,
        Err(error) if error.to_string().contains("cancelled by the user") => return Err(Cancelled.into()),
        Err(error) => {
            bail!("Couldn't open “{}”: {}", display_name(path), error.to_string().trim_start_matches("Error: "))
        }
    };
    // A line for each part of it: its device, what it holds and, if that mounted, where.
    let mounted = stdout.lines().find_map(|line| line.splitn(3, '\t').nth(2));
    mounted.map(PathBuf::from).with_context(|| format!("“{}” has nothing to show", display_name(path)))
}

pub fn eject(volume: &Path) -> Result<()> {
    // A share from another computer has no disk to eject, only a mount to undo.
    let verb = if is_network(volume) { "unmount" } else { "eject" };
    run(Command::new("/usr/sbin/diskutil").arg(verb).arg(volume))
        .with_context(|| format!("Couldn't eject “{}”", display_name(volume)))?;
    Ok(())
}
