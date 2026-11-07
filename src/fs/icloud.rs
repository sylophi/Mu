//! What iCloud says about the items it keeps in sync: what hasn't made it up yet or couldn't, and
//! downloading what's only in iCloud or letting go of what's downloaded.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};
use gpui_kit::SharedString;
use objc2::rc::autoreleasepool;
use objc2_foundation::{
    NSArray, NSError, NSFileManager, NSNumber, NSURLUbiquitousItemDownloadingErrorKey,
    NSURLUbiquitousItemIsUploadedKey, NSURLUbiquitousItemUploadingErrorKey,
};

use super::{display_name, file_url, home_dir};

/// Where iCloud keeps what it syncs: iCloud Drive and each app's own folder.
pub fn root() -> PathBuf {
    home_dir().join("Library/Mobile Documents")
}

/// Whether iCloud keeps `path` in sync.
pub fn contains(path: &Path) -> bool {
    path.starts_with(root())
}

/// What keeps an item from being in sync with iCloud.
#[derive(Clone, Debug, PartialEq)]
pub enum Unsynced {
    /// Changed here, and not uploaded yet.
    Waiting,
    /// It couldn't upload, for this reason.
    UploadFailed(SharedString),
    /// It couldn't download, for this reason.
    DownloadFailed(SharedString),
}

impl Unsynced {
    /// What it is, like "Couldn't upload: iCloud is full".
    pub fn describe(&self) -> String {
        match self {
            Self::Waiting => "Waiting to upload".into(),
            Self::UploadFailed(why) => format!("Couldn't upload: {why}"),
            Self::DownloadFailed(why) => format!("Couldn't download: {why}"),
        }
    }
}

/// What keeps `path`, which iCloud keeps in sync, from being in sync, if anything does.
pub fn unsynced(path: &Path) -> Option<Unsynced> {
    autoreleasepool(|_| {
        let keys = unsafe {
            NSArray::from_slice(&[
                NSURLUbiquitousItemIsUploadedKey,
                NSURLUbiquitousItemUploadingErrorKey,
                NSURLUbiquitousItemDownloadingErrorKey,
            ])
        };
        let values = file_url(path).resourceValuesForKeys_error(&keys).ok()?;
        let error = |key| {
            let error = values.objectForKey(key)?.downcast::<NSError>().ok()?;
            Some(SharedString::from(error.localizedDescription().to_string()))
        };
        let uploaded = values
            .objectForKey(unsafe { NSURLUbiquitousItemIsUploadedKey })
            .and_then(|uploaded| uploaded.downcast::<NSNumber>().ok())
            .is_none_or(|uploaded| uploaded.as_bool());
        if let Some(error) = error(unsafe { NSURLUbiquitousItemUploadingErrorKey }) {
            Some(Unsynced::UploadFailed(error))
        } else if let Some(error) = error(unsafe { NSURLUbiquitousItemDownloadingErrorKey }) {
            Some(Unsynced::DownloadFailed(error))
        } else {
            (!uploaded).then_some(Unsynced::Waiting)
        }
    })
}

/// Has iCloud download each of `paths`, which are only in iCloud.
pub fn download(paths: &[PathBuf]) -> Result<()> {
    each(paths, "download", |manager, url| manager.startDownloadingUbiquitousItemAtURL_error(url))
}

/// Has iCloud let go of what's downloaded of each of `paths`, which stay in iCloud.
pub fn remove_download(paths: &[PathBuf]) -> Result<()> {
    each(paths, "remove the download of", |manager, url| manager.evictUbiquitousItemAtURL_error(url))
}

/// Asks `ask` of iCloud for each of `paths`, failing with the first that couldn't, which it
/// couldn't `verb`.
fn each(
    paths: &[PathBuf],
    verb: &str,
    ask: impl Fn(&NSFileManager, &objc2_foundation::NSURL) -> Result<(), objc2::rc::Retained<NSError>>,
) -> Result<()> {
    autoreleasepool(|_| {
        let manager = NSFileManager::defaultManager();
        let failed: Vec<_> =
            paths.iter().filter_map(|path| ask(&manager, &file_url(path)).err().map(|error| (path, error))).collect();
        match failed.first() {
            None => Ok(()),
            Some((path, error)) => {
                Err(anyhow!("Couldn't {verb} “{}”: {}", display_name(path), error.localizedDescription()))
            }
        }
    })
}
