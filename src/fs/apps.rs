//! What macOS knows about files: the apps that open them, and where Finder aliases and bookmarks
//! lead.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use objc2::rc::{Retained, autoreleasepool};
use objc2_app_kit::{NSWorkspace, NSWorkspaceOpenConfiguration};
use objc2_foundation::{NSArray, NSData, NSURL, NSURLBookmarkResolutionOptions};

use super::{file_url, natural_cmp, read_text};

/// The apps that can open all of `paths`, as (name, app), with the first one's default app first.
pub fn apps_to_open(paths: &[PathBuf]) -> Vec<(String, PathBuf)> {
    let workspace = NSWorkspace::sharedWorkspace();
    let app_path = |url: Retained<NSURL>| url.to_file_path();
    // Items of one kind open with the same apps, so each kind is asked about once.
    let mut kinds = HashSet::new();
    let mut apps: Option<Vec<PathBuf>> = None;
    for path in paths {
        if !kinds.insert((path.is_dir(), path.extension().map(|ext| ext.to_ascii_lowercase()))) {
            continue;
        }
        let these: Vec<_> =
            workspace.URLsForApplicationsToOpenURL(&file_url(path)).into_iter().filter_map(app_path).collect();
        apps = Some(match apps {
            Some(apps) => apps.into_iter().filter(|app| these.contains(app)).collect(),
            None => these,
        });
    }
    let default = paths.first().and_then(|path| workspace.URLForApplicationToOpenURL(&file_url(path)));
    let default = default.and_then(app_path);
    let mut apps: Vec<_> = apps
        .unwrap_or_default()
        .into_iter()
        .map(|app| (app.file_stem().unwrap_or_default().to_string_lossy().into_owned(), app))
        .collect();
    apps.sort_by(|(a_name, a), (b_name, b)| {
        (default.as_ref() != Some(a)).cmp(&(default.as_ref() != Some(b))).then_with(|| natural_cmp(a_name, b_name))
    });
    // An app installed twice, like a copy in Downloads, shows once.
    let mut names = HashSet::new();
    apps.retain(|(name, _)| names.insert(name.clone()));
    apps
}

/// Opens `paths` in `app`.
pub fn open_with(paths: &[PathBuf], app: &Path) {
    let urls: Vec<_> = paths.iter().map(|path| file_url(path)).collect();
    NSWorkspace::sharedWorkspace().openURLs_withApplicationAtURL_configuration_completionHandler(
        &NSArray::from_retained_slice(&urls),
        &file_url(app),
        &NSWorkspaceOpenConfiguration::configuration(),
        None,
    );
}

/// Whether `path` is plain text that TextEdit, or no app at all, would open. It's plain text when
/// its first 8000 bytes are UTF-8 without a NUL, and it isn't RTF.
pub fn is_plain_text(path: &Path) -> bool {
    let app =
        autoreleasepool(|_| NSWorkspace::sharedWorkspace().URLForApplicationToOpenURL(&file_url(path))?.to_file_path());
    if app.is_some_and(|app| !app.ends_with("TextEdit.app"))
        || path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("rtf"))
    {
        return false;
    }
    read_text(path, 8000).is_ok_and(|bytes| match std::str::from_utf8(&bytes) {
        Ok(_) => true,
        // The 8000 bytes can end partway through a character.
        Err(error) => bytes.len() == 8000 && error.error_len().is_none(),
    })
}

/// How aliases and bookmarks are followed: without asking anything or mounting a disk.
const QUIETLY: NSURLBookmarkResolutionOptions =
    NSURLBookmarkResolutionOptions::WithoutUI.union(NSURLBookmarkResolutionOptions::WithoutMounting);

/// Where the Finder alias at `path` leads.
pub(super) fn alias_target(path: &Path) -> Option<PathBuf> {
    autoreleasepool(|_| {
        NSURL::URLByResolvingAliasFileAtURL_options_error(&file_url(path), QUIETLY).ok()?.to_file_path()
    })
}

/// Where the bookmark in `data` leads, like one of the favorites in Finder's sidebar.
pub fn bookmark_target(data: &NSData) -> Option<PathBuf> {
    let url = unsafe {
        NSURL::URLByResolvingBookmarkData_options_relativeToURL_bookmarkDataIsStale_error(
            data,
            QUIETLY,
            None,
            std::ptr::null_mut(),
        )
    };
    url.ok()?.to_file_path()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::Scratch;

    #[test]
    fn plain_text_is_utf8_but_not_rtf() {
        let scratch = Scratch::new("plain-text");
        let file = |name: &str, bytes: &[u8]| {
            let path = scratch.0.join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        };
        assert!(is_plain_text(&file("notes.txt", b"hello\n")));
        assert!(is_plain_text(&file("LICENSE", format!("a{}", "é".repeat(4000)).as_bytes())));
        assert!(!is_plain_text(&file("binary", b"\0\x01")));
        assert!(!is_plain_text(&file("latin", b"caf\xe9")));
        assert!(!is_plain_text(&file("styled.rtf", br"{\rtf1 hello}")));
    }
}
