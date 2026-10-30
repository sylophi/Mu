//! Everything that touches the disk: listing folders, describing files and the file operations.
//! All of it is blocking and meant to run on a background thread.

pub mod admin;
mod apps;
mod archives;
mod kinds;
mod listing;
mod merge;
mod names;
mod ops;
mod trash;
mod undo;
mod volumes;

use std::{ffi::CString, io, os::unix::ffi::OsStrExt as _, path::Path, process::Command};

use anyhow::{Result, bail};

use objc2::rc::Retained;
use objc2_foundation::{NSString, NSURL};

pub use apps::{apps_to_open, bookmark_target, is_plain_text, open_with};
pub use archives::{compress, extract, replacement_folder};
pub use kinds::{Category, extension, is_archive, is_disk_image, is_package, read_text};
pub use listing::{Changes, Entry, SF_DATALESS, Shared, Usage, list_dir, list_usage, watch};
pub use merge::{Choice, Choices, Clash, clashes, copy_into, move_into};
pub use names::{
    count, display_name, format_date, format_size, home_dir, icloud_drive, natural_cmp, shown_path, trash_dir,
    typed_path,
};
pub use ops::{Cancelled, Progress, copyfile, new_file, new_folder, remove_all, rename};
pub use trash::{empty_trash, is_locked, origin, put_back, set_origin, trash, trash_items};
pub use undo::{Done, Undo};
pub use volumes::{Volume, capacity, eject, mount_image, mounted_from, same_volume, volume_root, volumes};

use apps::alias_target;
use kinds::describe;
use ops::{copy_path, delete_path, disk_name, each, move_path, unique_path};
use trash::{clear_origin, trash_one};

pub fn c_path(path: &Path) -> io::Result<CString> {
    Ok(CString::new(path.as_os_str().as_bytes())?)
}

pub fn file_url(path: &Path) -> Retained<NSURL> {
    NSURL::fileURLWithPath(&NSString::from_str(&path.to_string_lossy()))
}

/// Runs `command` and says what it printed. Should it fail, this fails with what it said went
/// wrong, less the program's name that tools like ditto and tar put first.
pub fn run(command: &mut Command) -> Result<String> {
    let output = command.output()?;
    if !output.status.success() {
        let program = Path::new(command.get_program()).file_name().unwrap_or_default().to_string_lossy();
        let error = String::from_utf8_lossy(&output.stderr);
        let error = error.trim();
        bail!("{}", error.strip_prefix(&format!("{program}: ")).unwrap_or(error));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
pub use tests::Scratch;

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    /// A folder of its own in the temp dir for a test, removed afterwards.
    pub struct Scratch(pub PathBuf);

    impl Scratch {
        pub fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("mu-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// The names of what's in `dir`, sorted.
    pub fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> =
            fs::read_dir(dir).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }
}
