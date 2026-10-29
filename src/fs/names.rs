//! How files and folders read: their names and paths, natural order, sizes and dates. And the
//! folders Mu knows by name, like the home folder and the Trash.

use std::{
    cmp::Ordering,
    path::{Path, PathBuf},
    sync::LazyLock,
    time::SystemTime,
};

use objc2::rc::autoreleasepool;
use objc2_foundation::{NSFileManager, ns_string};

/// Case-insensitive comparison that orders digit runs by value, so "file2" sorts before "file10".
pub fn natural_cmp(mut a: &str, mut b: &str) -> Ordering {
    loop {
        // Running out first sorts first.
        let (Some(x), Some(y)) = (a.chars().next(), b.chars().next()) else { return a.len().cmp(&b.len()) };
        let order = if x.is_ascii_digit() && y.is_ascii_digit() {
            let digits = |s: &str| s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
            let (da, db) = (digits(a), digits(b));
            let (na, nb) = (a[..da].trim_start_matches('0'), b[..db].trim_start_matches('0'));
            (a, b) = (&a[da..], &b[db..]);
            na.len().cmp(&nb.len()).then_with(|| na.cmp(nb))
        } else {
            (a, b) = (&a[x.len_utf8()..], &b[y.len_utf8()..]);
            x.to_lowercase().cmp(y.to_lowercase())
        };
        if order != Ordering::Equal {
            return order;
        }
    }
}

/// "1 item", "2 items".
pub fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if bytes < 1000 {
        return format!("{bytes} bytes");
    }
    let mut value = bytes as f64 / 1000.;
    let mut unit = 0;
    while value >= 1000. && unit < UNITS.len() - 1 {
        value /= 1000.;
        unit += 1;
    }
    if unit == 0 || value >= 100. {
        format!("{value:.0} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub fn format_date(time: SystemTime) -> String {
    use chrono::{DateTime, Local};
    let time: DateTime<Local> = time.into();
    let today = Local::now().date_naive();
    let date = time.date_naive();
    if date == today {
        time.format("Today at %H:%M").to_string()
    } else if today.pred_opt() == Some(date) {
        time.format("Yesterday at %H:%M").to_string()
    } else {
        time.format("%b %-d, %Y at %H:%M").to_string()
    }
}

/// The name Finder shows for `path`: its file name, the startup disk's for `/`, or the Trash's.
pub fn display_name(path: &Path) -> String {
    match path.file_name() {
        Some(name) if name == ".Trash" && path == trash_dir() => "Trash".into(),
        Some(name) if name == "com~apple~CloudDocs" && path == icloud_drive() => "iCloud Drive".into(),
        Some(name) => name.to_string_lossy().into_owned(),
        None => STARTUP_DISK.clone(),
    }
}

/// The startup disk's name, which you can change in Finder.
static STARTUP_DISK: LazyLock<String> = LazyLock::new(|| {
    autoreleasepool(|_| NSFileManager::defaultManager().displayNameAtPath(ns_string!("/")).to_string())
});

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/"))
}

/// A path as it's shown: as it's typed, but with iCloud Drive by name.
pub fn shown_path(path: &Path) -> String {
    match path.strip_prefix(icloud_drive()) {
        Ok(rest) => Path::new("iCloud Drive").join(rest).display().to_string().trim_end_matches('/').into(),
        Err(_) => typed_path(path),
    }
}

/// A path as it's typed, with the home folder written as `~`.
pub fn typed_path(path: &Path) -> String {
    match path.strip_prefix(home_dir()) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

/// The Trash on the startup disk.
pub fn trash_dir() -> PathBuf {
    home_dir().join(".Trash")
}

pub fn icloud_drive() -> PathBuf {
    home_dir().join("Library/Mobile Documents/com~apple~CloudDocs")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order() {
        let mut names = vec!["file10", "File2", "file1", "a", "B", "file02b"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, ["a", "B", "file1", "File2", "file02b", "file10"]);
    }

    #[test]
    fn names_the_startup_disk() {
        assert!(!display_name(Path::new("/")).is_empty());
        assert_eq!(display_name(&home_dir().join(".Trash")), "Trash");
    }

    #[test]
    fn sizes() {
        assert_eq!(format_size(999), "999 bytes");
        assert_eq!(format_size(12_345), "12 KB");
        assert_eq!(format_size(1_234_567), "1.2 MB");
        assert_eq!(format_size(345_000_000_000), "345 GB");
        assert_eq!(format_size(u64::MAX), "18447 PB");
    }
}
