//! An app as far as removing it goes: what it's called, the bundle ids it and the helpers inside it
//! go by, which name the files they keep, and the apps installed on this Mac.

use std::{
    path::{Path, PathBuf},
    process::Command,
};

use plist::{Dictionary, Value};
use rayon::prelude::*;

use crate::fs::{self, home_dir};

#[derive(Clone, Debug)]
pub struct App {
    pub path: PathBuf,
    /// What it goes by, as Finder shows it.
    pub name: String,
    pub version: Option<String>,
    /// The ids of it and the helpers, extensions and login items inside it.
    pub ids: Vec<String>,
    /// The names of its program and theirs, which their crash reports go by.
    pub executables: Vec<String>,
    /// What its files may be named after: its name, the name in its Info.plist and its display
    /// name, each also without spaces or a number at its end.
    pub names: Vec<String>,
}

impl App {
    /// The app at `path`, when it is one.
    pub fn new(path: &Path) -> Option<App> {
        let info = info(path)?;
        let string = |key| info.get(key).and_then(Value::as_string).map(str::to_owned);
        let name = path.file_stem()?.to_string_lossy().into_owned();
        let version = string("CFBundleShortVersionString").or_else(|| string("CFBundleVersion"));
        let mut ids: Vec<String> = string("CFBundleIdentifier").into_iter().collect();
        let mut executables: Vec<String> = string("CFBundleExecutable").into_iter().collect();
        nested(&path.join("Contents"), 0, &mut ids, &mut executables);
        let mut names = Vec::new();
        for name in [Some(name.clone()), string("CFBundleName"), string("CFBundleDisplayName")].into_iter().flatten() {
            let name = name.trim();
            let without_number = name.trim_end_matches(|c: char| c.is_ascii_digit() || c == ' ' || c == '.');
            for name in [name, &name.replace(' ', ""), without_number] {
                if !name.is_empty() && !names.iter().any(|known: &String| known.eq_ignore_ascii_case(name)) {
                    names.push(name.to_owned());
                }
            }
        }
        Some(App { path: path.into(), name, version, ids, executables, names })
    }

    /// Whether macOS keeps it, so it can't be removed, like the links in /Applications to its own.
    pub fn is_system(&self) -> bool {
        let real = self.path.canonicalize().unwrap_or_else(|_| self.path.clone());
        real.starts_with("/System") || fs::is_restricted(&self.path)
    }

    /// Whether it's Mu, which can't remove itself while it runs.
    pub fn is_mu(&self) -> bool {
        std::env::current_exe().ok().and_then(|exe| fs::app_bundle(&exe)).is_some_and(|bundle| bundle == self.path)
    }

    /// Who signed it and the app groups it may share files in, from its signature.
    pub fn signing(&self) -> Signing {
        let output = Command::new("/usr/bin/codesign")
            .args(["--display", "--verbose", "--entitlements", "-", "--xml"])
            .arg(&self.path)
            .output();
        let Ok(output) = output else { return Signing::default() };
        let team = String::from_utf8_lossy(&output.stderr)
            .lines()
            .find_map(|line| line.strip_prefix("TeamIdentifier="))
            .filter(|team| *team != "not set")
            .map(str::to_owned);
        let entitlements = Value::from_reader_xml(output.stdout.as_slice()).ok();
        let groups = entitlements
            .as_ref()
            .and_then(|entitlements| entitlements.as_dictionary()?.get("com.apple.security.application-groups"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_string)
            .map(str::to_owned)
            .collect();
        Signing { team, groups }
    }
}

#[derive(Default)]
pub struct Signing {
    pub team: Option<String>,
    pub groups: Vec<String>,
}

pub(super) fn info(bundle: &Path) -> Option<Dictionary> {
    // An iPhone or iPad app keeps its in the app it wraps.
    let info = Value::from_file(bundle.join("Contents/Info.plist"));
    info.or_else(|_| Value::from_file(bundle.join("WrappedBundle/Info.plist"))).ok()?.into_dictionary()
}

/// Adds the ids and programs of the apps, extensions and services in `dir`, an app's Contents or a
/// folder in it, to `ids` and `executables`, including those that come in its frameworks, like a
/// browser's updater. The updaters apps carry go by their makers' ids rather than the app's.
fn nested(dir: &Path, depth: usize, ids: &mut Vec<String>, executables: &mut Vec<String>) {
    if depth > 6 {
        return;
    }
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        // Nothing runs from its translations or signature.
        let plain = path.extension().is_some_and(|ext| ext == "lproj") || entry.file_name() == "_CodeSignature";
        if plain || !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let part = path
            .extension()
            .is_some_and(|ext| ["app", "appex", "xpc", "systemextension"].contains(&&*ext.to_string_lossy()));
        let info = if part { info(&path) } else { None };
        let string = |key| info.as_ref().and_then(|info| info.get(key)?.as_string().map(str::to_owned));
        match string("CFBundleIdentifier") {
            Some(id) if id.starts_with("org.sparkle-project") || id.starts_with("com.apple.") => {}
            Some(id) => {
                push_new(ids, id);
                push_new(executables, string("CFBundleExecutable"));
                nested(&path.join("Contents"), depth + 1, ids, executables);
            }
            None => nested(&path, depth + 1, ids, executables),
        }
    }
}

fn push_new(values: &mut Vec<String>, value: impl Into<Option<String>>) {
    if let Some(value) = value.into().filter(|value| !values.contains(value)) {
        values.push(value);
    }
}

/// Where apps are installed for everyone and for you, besides the folders in these.
pub fn folders() -> [PathBuf; 2] {
    ["/Applications".into(), home_dir().join("Applications")]
}

/// The apps installed for everyone and for you.
pub fn installed() -> Vec<App> {
    let mut paths = Vec::new();
    for folder in folders() {
        find(&folder, 0, &mut paths);
    }
    paths.par_iter().filter_map(|path| App::new(path)).collect()
}

/// Adds the apps in `dir` and the folders in it to `paths`.
fn find(dir: &Path, depth: usize, paths: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "app") {
            paths.push(path);
        } else if depth < 2 && entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            find(&path, depth + 1, paths);
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use std::path::{Path, PathBuf};

    use plist::{Dictionary, Value};

    use super::App;
    use crate::fs::Scratch;

    /// Makes an app called `name` in `dir`, with `id`, and a helper inside it with `helper`.
    pub fn make_app(dir: &Path, name: &str, id: &str, helper: Option<&str>) -> PathBuf {
        let app = dir.join(format!("{name}.app"));
        let write = |bundle: &Path, id: &str, name: &str| {
            std::fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
            let info = Dictionary::from_iter([
                ("CFBundleIdentifier".to_owned(), Value::from(id)),
                ("CFBundleName".to_owned(), Value::from(name)),
                ("CFBundleExecutable".to_owned(), Value::from(name)),
                ("CFBundleShortVersionString".to_owned(), Value::from("1.2")),
            ]);
            Value::Dictionary(info).to_file_xml(bundle.join("Contents/Info.plist")).unwrap();
        };
        write(&app, id, name);
        if let Some(helper) = helper {
            write(&app.join("Contents/Library/LoginItems/Helper.app"), helper, "Helper");
        }
        app
    }

    #[test]
    fn reads_what_its_files_go_by() {
        let scratch = Scratch::new("app");
        let path =
            make_app(&scratch.0, "Mu Test 2", "io.github.sylophi.mutest", Some("io.github.sylophi.mutest.helper"));
        let app = App::new(&path).unwrap();
        assert_eq!(app.name, "Mu Test 2");
        assert_eq!(app.version.as_deref(), Some("1.2"));
        assert_eq!(app.ids, ["io.github.sylophi.mutest", "io.github.sylophi.mutest.helper"]);
        assert_eq!(app.executables, ["Mu Test 2", "Helper"]);
        assert_eq!(app.names, ["Mu Test 2", "MuTest2", "Mu Test"]);
        assert!(App::new(&scratch.0).is_none());
    }
}
