//! Brings over what you've set up in Finder: its sidebar's folders and how it shows them. And
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

/// Makes the items in Mu's sidebar outside any group, hidden files, panes and view match Finder's.
/// A folder that comes over leaves any group it was in. Resolves to how many items came over.
/// Reading Finder's sidebar takes Full Disk Access.
pub fn import(cx: &mut App) -> io::Result<usize> {
    // Finder keeps iCloud Drive in a list of its own, which goes after Favorites.
    let mut favorites = sidebar_list("FavoriteItems")?;
    favorites.extend(sidebar_list("iCloudItems").unwrap_or_default());
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
        settings.sidebar_items.clear();
        settings.place_in_sidebar(None, 0, &favorites);
        settings.show_hidden = show_hidden.unwrap_or(settings.show_hidden);
        settings.show_sidebar = show_sidebar.unwrap_or(settings.show_sidebar);
        settings.show_preview = show_preview.unwrap_or(settings.show_preview);
        settings.grid = grid.unwrap_or(settings.grid);
    });
    Ok(count)
}

/// The folders in one of the lists in Finder's sidebar, like Favorites, in its order. It keeps them
/// as bookmarks in a keyed archive. Recents, AirDrop and the like aren't folders, so those stay out.
fn sidebar_list(name: &str) -> io::Result<Vec<PathBuf>> {
    let list = home_dir()
        .join(format!("Library/Application Support/com.apple.sharedfilelist/com.apple.LSSharedFileList.{name}.sfl4"));
    let bytes = fs::read(list)?;
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
                // iCloud Drive's bookmark doesn't lead to it.
                let icloud = item
                    .objectForKey(ns_string!("CustomItemProperties"))
                    .and_then(|properties| properties.downcast::<NSDictionary>().ok())
                    .and_then(|properties| {
                        properties.objectForKey(ns_string!("com.apple.LSSharedFileList.SpecialItemIdentifier"))
                    })
                    .and_then(|id| id.downcast::<NSString>().ok())
                    .is_some_and(|id| id.to_string() == "com.apple.LSSharedFileList.IsICloudDrive");
                let path = match icloud {
                    true => files::icloud_drive(),
                    false => {
                        let bookmark = item.objectForKey(ns_string!("Bookmark"))?.downcast::<NSData>().ok()?;
                        files::bookmark_target(&bookmark)?
                    }
                };
                (!hidden && path.is_dir() && !is_canned(&path)).then_some(path)
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
