//! What an app's installer put around the Mac, like a driver in /Library or a tool in /usr/local,
//! from the records macOS keeps of each package it installs.

use std::{
    collections::HashSet,
    ffi::OsStr,
    path::{Component, Path, PathBuf},
    process::Command,
};

use plist::Value;

use super::{app::App, leftovers::by_id};
use crate::fs;

const RECEIPTS: &str = "/private/var/db/receipts";

/// What the packages that installed `app` put outside it that's theirs alone, and their records,
/// and whether they're ticked to go: not when the packages installed one of the `others` too, like
/// the apps of a suite.
pub fn installed_by(app: &App, others: &[&App]) -> Vec<(PathBuf, bool)> {
    let packages = packages(app);
    let listed: HashSet<PathBuf> = packages.iter().flat_map(|package| files(package)).collect();
    let apps: Vec<&Path> = [app].iter().chain(others).map(|app| app.path.as_path()).collect();
    let mut found = own(&listed, &apps, |path| listed_by_others(path, &packages));
    for package in &packages {
        found.extend(["bom", "plist"].map(|ext| Path::new(RECEIPTS).join(format!("{package}.{ext}"))));
    }
    found.retain(|path| path.symlink_metadata().is_ok());
    let shared = others.iter().any(|other| listed.contains(&other.path));
    found.into_iter().map(|path| (path, !shared)).collect()
}

/// The packages that installed the app, or go by its ids.
fn packages(app: &App) -> Vec<String> {
    let mut packages = file_info(&app.path);
    packages.extend(
        pkgutil(["--pkgs"])
            .lines()
            .filter(|package| app.ids.iter().any(|id| id.split('.').count() > 2 && by_id(package, id)))
            .map(str::to_owned),
    );
    // macOS's own, which list its standard folders.
    packages.retain(|package| !package.starts_with("com.apple."));
    packages.sort();
    packages.dedup();
    packages
}

/// What `package` put where.
fn files(package: &str) -> Vec<PathBuf> {
    let info = pkgutil(["--pkg-info-plist", package]);
    let info = Value::from_reader_xml(info.as_bytes()).ok().and_then(Value::into_dictionary).unwrap_or_default();
    let string = |key| info.get(key).and_then(Value::as_string).unwrap_or_default();
    let root = Path::new("/").join(string("volume")).join(string("install-location"));
    pkgutil(["--files", package]).lines().map(|file| root.join(file)).collect()
}

/// Whether a package besides `ours` lists `path`, as macOS's own do its standard folders.
fn listed_by_others(path: &Path, ours: &[String]) -> bool {
    file_info(path).iter().any(|package| !ours.contains(package))
}

/// The packages that list `path`.
fn file_info(path: &Path) -> Vec<String> {
    let info = pkgutil([OsStr::new("--file-info"), path.as_os_str()]);
    info.lines().filter_map(|line| line.strip_prefix("pkgid: ")).map(str::to_owned).collect()
}

fn pkgutil(args: impl IntoIterator<Item = impl AsRef<OsStr>>) -> String {
    fs::run(Command::new("/usr/sbin/pkgutil").args(args)).unwrap_or_default()
}

/// The topmost of what's `listed` that's there and is the packages' alone, outside the `apps`
/// installed: files and folders with nothing else in them that no other package lists. Folders
/// that every Mac has, like /Library/Application Support, and those right inside /usr/local or
/// /opt, aren't anyone's.
fn own(listed: &HashSet<PathBuf>, apps: &[&Path], listed_by_others: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    let mut paths: Vec<&PathBuf> = listed.iter().collect();
    paths.sort();
    let mut found: Vec<PathBuf> = Vec::new();
    for path in paths {
        let inside_found = found.last().is_some_and(|last| path.starts_with(last));
        if inside_found || apps.iter().any(|app| path.starts_with(app) || app.starts_with(path)) {
            continue;
        }
        let Ok(meta) = path.symlink_metadata() else { continue };
        let depth = path.components().filter(|part| matches!(part, Component::Normal(_))).count();
        let standard = depth < 3 || ["/usr/local", "/opt"].iter().any(|dir| path.parent() == Some(Path::new(dir)));
        if (!meta.is_dir() || (!standard && only_listed(path, listed))) && !listed_by_others(path) {
            found.push(path.clone());
        }
    }
    found
}

/// Whether everything in `dir` is `listed`, as far as can be seen.
fn only_listed(dir: &Path, listed: &HashSet<PathBuf>) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else { return false };
    entries.flatten().all(|entry| {
        let path = entry.path();
        listed.contains(&path) && (!entry.file_type().is_ok_and(|kind| kind.is_dir()) || only_listed(&path, listed))
    })
}

#[cfg(test)]
mod tests {
    use std::{collections::HashSet, fs, path::PathBuf};

    use super::own;
    use crate::fs::Scratch;

    #[test]
    fn finds_what_is_the_packages_alone() {
        let scratch = Scratch::new("receipts");
        let root = &scratch.0;
        let listed: HashSet<PathBuf> = [
            "Applications",
            "Applications/Example.app",
            "Applications/Example.app/Contents",
            "Library",
            "Library/Support",
            "Library/Support/Example",
            "Library/Support/Example/data",
            "Library/Support/Example/More",
            "Library/Support/Example/More/data",
            "Library/Plug-Ins",
            "Library/Plug-Ins/Example.driver",
            "Library/Plug-Ins/Shared",
            "Library/Plug-Ins/Shared/Example.component",
            "Library/Gone",
        ]
        .iter()
        .map(|path| root.join(path))
        .collect();
        for path in &listed {
            if path.extension().is_none() && !path.ends_with("data") {
                fs::create_dir_all(path).unwrap();
            }
        }
        for path in
            ["Library/Support/Example/data", "Library/Support/Example/More/data", "Library/Plug-Ins/Example.driver"]
        {
            fs::write(root.join(path), "").unwrap();
        }
        fs::create_dir_all(root.join("Library/Plug-Ins/Shared/Example.component")).unwrap();
        // Something of the user's, in a folder of the package's.
        fs::write(root.join("Library/Plug-Ins/Shared/Other.component"), "").unwrap();
        fs::remove_dir(root.join("Library/Gone")).unwrap();
        let app = root.join("Applications/Example.app");
        let found = own(&listed, &[&app], |dir| dir.ends_with("Library/Plug-Ins") || dir.ends_with("Library/Support"));
        let mut expected: Vec<PathBuf> =
            ["Library/Plug-Ins/Example.driver", "Library/Plug-Ins/Shared/Example.component", "Library/Support/Example"]
                .iter()
                .map(|path| root.join(path))
                .collect();
        expected.sort();
        assert_eq!(found, expected);
    }
}
