//! Brings over what you've set up in Finder: its sidebar's favorites and how it shows folders. And
//! stands in for it when other apps show files in Finder.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use gpui_kit::App;
use objc2::{AnyThread as _, rc::autoreleasepool};
use objc2_core_foundation::{
    CFPreferencesCopyValue, CFPreferencesSetValue, CFPreferencesSynchronize, CFString, kCFPreferencesAnyApplication,
    kCFPreferencesAnyHost, kCFPreferencesCurrentUser,
};
use objc2_foundation::{
    NSArray, NSBundle, NSData, NSDictionary, NSKeyedArchiveRootObjectKey, NSKeyedUnarchiver, NSNumber, NSString,
    NSUserDefaults, ns_string,
};

use crate::{
    fs::{self as files, home_dir},
    settings::Settings,
};

/// Makes Mu's favorites, hidden files, panes and view match Finder's. Resolves to how many
/// favorites came over. Reading Finder's sidebar takes Full Disk Access.
pub fn import(cx: &mut App) -> io::Result<usize> {
    let favorites = favorites()?;
    let finder = NSUserDefaults::initWithSuiteName(NSUserDefaults::alloc(), Some(ns_string!("com.apple.finder")))
        .ok_or(io::ErrorKind::NotFound)?;
    // Only what Finder has a setting for. The rest stays as it is.
    let flag = |key: &NSString| finder.objectForKey(key).map(|_| finder.boolForKey(key));
    let show_hidden = flag(ns_string!("AppleShowAllFiles"));
    let show_sidebar = flag(ns_string!("ShowSidebar"));
    let show_preview = flag(ns_string!("ShowPreviewPane"));
    let grid = finder.stringForKey(ns_string!("FXPreferredViewStyle")).map(|style| style.to_string() == "icnv");
    let count = favorites.len();
    Settings::update(cx, |settings| {
        settings.favorites = favorites;
        settings.show_hidden = show_hidden.unwrap_or(settings.show_hidden);
        settings.show_sidebar = show_sidebar.unwrap_or(settings.show_sidebar);
        settings.show_preview = show_preview.unwrap_or(settings.show_preview);
        settings.grid = grid.unwrap_or(settings.grid);
    });
    Ok(count)
}

/// The folders in Finder's sidebar under Favorites, in its order. It keeps them as bookmarks in a
/// keyed archive. Recents, AirDrop and the like aren't folders, and iCloud Drive has its own place
/// in Mu's sidebar, so those stay out.
fn favorites() -> io::Result<Vec<PathBuf>> {
    let list = home_dir()
        .join("Library/Application Support/com.apple.sharedfilelist/com.apple.LSSharedFileList.FavoriteItems.sfl4");
    let bytes = fs::read(list)?;
    let icloud = files::icloud_drive();
    autoreleasepool(|_| {
        let data = NSData::with_bytes(&bytes);
        // Not secure coding: the archive holds plain dictionaries, arrays, data and numbers.
        let root = unsafe {
            let archive = NSKeyedUnarchiver::initForReadingFromData_error(NSKeyedUnarchiver::alloc(), &data).ok()?;
            archive.setRequiresSecureCoding(false);
            archive.decodeObjectForKey(NSKeyedArchiveRootObjectKey)
        }?;
        let root = root.downcast::<NSDictionary>().ok()?;
        let items = root.objectForKey(ns_string!("items"))?.downcast::<NSArray>().ok()?;
        let paths = items
            .iter()
            .filter_map(|item| {
                let item = item.downcast::<NSDictionary>().ok()?;
                let hidden = item
                    .objectForKey(ns_string!("visibility"))
                    .and_then(|visibility| visibility.downcast::<NSNumber>().ok())
                    .is_some_and(|visibility| visibility.integerValue() != 0);
                let bookmark = item.objectForKey(ns_string!("Bookmark"))?.downcast::<NSData>().ok()?;
                let path = files::bookmark_target(&bookmark)?;
                (!hidden && path.is_dir() && !is_canned(&path) && path != icloud).then_some(path)
            })
            .collect();
        Some(paths)
    })
    .ok_or(io::ErrorKind::InvalidData.into())
}

/// Whether other apps' Show in Finder shows files in Mu, or None when this copy of Mu isn't an app
/// they could ask. macOS keeps which app it is in NSFileViewer, set for every app at once.
pub fn shows_for_finder() -> Option<bool> {
    let id = NSBundle::mainBundle().bundleIdentifier()?.to_string();
    let viewer = unsafe {
        CFPreferencesCopyValue(
            &CFString::from_static_str("NSFileViewer"),
            kCFPreferencesAnyApplication,
            kCFPreferencesCurrentUser,
            kCFPreferencesAnyHost,
        )
    };
    Some(viewer.and_then(|viewer| viewer.downcast::<CFString>().ok()).is_some_and(|viewer| viewer.to_string() == id))
}

/// Has other apps' Show in Finder show files in Mu, or hands it back to Finder by taking the setting
/// away.
pub fn set_shows_for_finder(on: bool) {
    let Some(id) = NSBundle::mainBundle().bundleIdentifier() else { return };
    let id = on.then(|| CFString::from_str(&id.to_string()));
    unsafe {
        CFPreferencesSetValue(
            &CFString::from_static_str("NSFileViewer"),
            id.as_deref().map(|id| id.as_ref()),
            kCFPreferencesAnyApplication,
            kCFPreferencesCurrentUser,
            kCFPreferencesAnyHost,
        );
        CFPreferencesSynchronize(kCFPreferencesAnyApplication, kCFPreferencesCurrentUser, kCFPreferencesAnyHost);
    }
}

/// Finder's own searches, like Recents, which are folders inside Finder.
fn is_canned(path: &Path) -> bool {
    path.extension().is_some_and(|ext| ext == "cannedSearch") || path.starts_with("/System/Library/CoreServices")
}
