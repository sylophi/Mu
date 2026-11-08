//! What iCloud says about the items it keeps in sync: what hasn't made it up yet or couldn't, and
//! downloading what's only in iCloud or letting go of what's downloaded.

use std::{
    fs, io,
    os::{macos::fs::MetadataExt as _, unix::ffi::OsStrExt as _},
    path::{Path, PathBuf},
};

use anyhow::{Result, anyhow};
use gpui_kit::SharedString;
use objc2::rc::autoreleasepool;
use objc2_foundation::{
    NSArray, NSError, NSFileManager, NSNumber, NSURLUbiquitousItemDownloadingErrorKey,
    NSURLUbiquitousItemIsUploadedKey, NSURLUbiquitousItemUploadingErrorKey,
};

use super::{Entry, display_name, file_url, home_dir, icloud_drive, list_dir};

/// Where iCloud keeps what it syncs: iCloud Drive and each app's own folder.
pub fn root() -> PathBuf {
    home_dir().join("Library/Mobile Documents")
}

/// Whether iCloud keeps `path` in sync.
pub fn contains(path: &Path) -> bool {
    path.starts_with(root())
}

/// What an app keeps in iCloud that shows in iCloud Drive, under the app's name.
pub const APP_FOLDER: &str = "Documents";

/// The apps' folders Finder shows at the top of iCloud Drive. iCloud hides the rest of the apps'.
pub fn app_folders() -> Vec<PathBuf> {
    app_folders_in(&root())
}

fn app_folders_in(root: &Path) -> Vec<PathBuf> {
    let Ok(containers) = fs::read_dir(root) else { return Vec::new() };
    let shown = containers.flatten().filter(|c| c.metadata().is_ok_and(|meta| meta.st_flags() & libc::UF_HIDDEN == 0));
    let mut folders: Vec<PathBuf> = shown
        .map(|container| container.path().join(APP_FOLDER))
        .filter(|folder| app_folder_in(folder, root).is_some() && folder.is_dir())
        .collect();
    folders.sort();
    folders
}

/// The app's folder `path` is or is in, with iCloud keeping them in `root`.
pub fn app_folder_in(path: &Path, root: &Path) -> Option<PathBuf> {
    let mut parts = path.strip_prefix(root).ok()?.components();
    let (container, folder) = (parts.next()?.as_os_str(), parts.next()?.as_os_str());
    // iCloud Drive's own Documents is a folder like any other, and iCloud's Trash isn't an app's.
    let app = folder == APP_FOLDER && container != "com~apple~CloudDocs" && !container.as_bytes().starts_with(b".");
    app.then(|| root.join(container).join(APP_FOLDER))
}

/// The app's folder `path` is or is in.
pub fn app_folder(path: &Path) -> Option<PathBuf> {
    app_folder_in(path, &root())
}

/// Whether `path` is an app's folder, which stays where iCloud keeps it.
pub fn is_app_folder(path: &Path) -> bool {
    app_folder(path).is_some_and(|folder| folder == path)
}

/// The folder `path` shows in, which for an app's folder is iCloud Drive.
pub fn enclosing(path: &Path) -> Option<PathBuf> {
    if is_app_folder(path) { Some(icloud_drive()) } else { path.parent().map(Path::to_path_buf) }
}

/// iCloud's copy of the icon of the app whose folder is at `path`, the biggest it has.
pub fn app_folder_icon(path: &Path) -> Option<PathBuf> {
    let container = is_app_folder(path).then(|| path.parent()?.file_name()?.to_str()).flatten()?;
    let icons =
        home_dir().join("Library/Application Support/CloudDocs/session/containers").join(container.replace('~', "."));
    ["256x256_OSX", "128x128_OSX", "120x120_iOS", "80x80_iOS"]
        .into_iter()
        .map(|icon| icons.join(icon).with_extension("png"))
        .find(|icon| icon.is_file())
}

/// `folder`, an app's, as the item it shows as in iCloud Drive.
pub fn app_folder_item(folder: &Path) -> io::Result<Vec<Entry>> {
    let mut items = list_dir(folder.parent().unwrap_or(folder))?;
    items.retain(|item| item.path == folder);
    for item in &mut items {
        item.name = display_name(folder).into();
    }
    Ok(items)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::{Scratch, c_path};

    #[test]
    fn shows_only_the_apps_folders_icloud_doesnt_hide() {
        let scratch = Scratch::new("app-folders");
        let root = &scratch.0;
        for container in ["com~apple~TextEdit", "com~apple~Pages", "com~apple~CloudDocs", ".Trash"] {
            fs::create_dir_all(root.join(container).join(APP_FOLDER)).unwrap();
        }
        fs::create_dir(root.join("com~apple~NoDocuments")).unwrap();
        fs::create_dir(root.join("com~apple~DocumentsFile")).unwrap();
        fs::write(root.join("com~apple~DocumentsFile").join(APP_FOLDER), "").unwrap();
        let hidden = c_path(&root.join("com~apple~Pages")).unwrap();
        assert_eq!(unsafe { libc::chflags(hidden.as_ptr(), libc::UF_HIDDEN) }, 0);
        assert_eq!(app_folders_in(root), [root.join("com~apple~TextEdit/Documents")]);
        // With iCloud off, there's nowhere to look.
        assert!(app_folders_in(&root.join("missing")).is_empty());
    }

    #[test]
    fn an_apps_folder_is_enclosed_by_icloud_drive() {
        let app = root().join("com~apple~TextEdit/Documents");
        assert_eq!(enclosing(&app), Some(icloud_drive()));
        assert_eq!(enclosing(&app.join("Letters")), Some(app.clone()));
        assert_eq!(enclosing(&icloud_drive().join("Documents")), Some(icloud_drive()));
    }

    #[test]
    fn tells_what_is_in_an_apps_folder() {
        let root = Path::new("/icloud");
        let app = Some(root.join("com~apple~TextEdit/Documents"));
        assert_eq!(app_folder_in(&root.join("com~apple~TextEdit/Documents"), root), app);
        assert_eq!(app_folder_in(&root.join("com~apple~TextEdit/Documents/a/b"), root), app);
        for other in
            ["com~apple~TextEdit", "com~apple~TextEdit/Data", "com~apple~CloudDocs/Documents", ".Trash/Documents"]
        {
            assert_eq!(app_folder_in(&root.join(other), root), None, "{other}");
        }
    }
}
