//! What an app leaves around the Mac besides itself: its settings, caches, containers, logs and
//! the like, found by the ids and names they go by, and the jobs launchd starts for it.
//!
//! A file can go by more than one app's id or name, like `com.example.App.beta.plist`, which
//! `com.example.App` and `com.example.App.beta` both start. It goes with the installed app it goes
//! by most closely, so removing one app doesn't take another's files.

use std::{
    ffi::OsStr,
    os::unix::fs::MetadataExt as _,
    path::{Path, PathBuf},
};

use plist::Value;
use rayon::prelude::*;

use super::{
    app::{self, App, Signing},
    receipts,
};
use crate::{
    fs::{self, home_dir},
    mac::startup::launchd,
};

/// Something an app left, and whether it's ticked to go.
pub struct Leftover {
    pub path: PathBuf,
    pub place: Place,
    pub size: u64,
    pub checked: bool,
}

/// What a leftover is, which it's listed under.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Place {
    App,
    Support,
    Caches,
    Containers,
    Preferences,
    State,
    Web,
    Logs,
    Launch,
    Installed,
    Other,
}

impl Place {
    pub fn name(self) -> &'static str {
        match self {
            Place::App => "App",
            Place::Support => "Application Support",
            Place::Caches => "Caches",
            Place::Containers => "Containers",
            Place::Preferences => "Preferences",
            Place::State => "Saved State",
            Place::Web => "Website Data",
            Place::Logs => "Logs and Crash Reports",
            Place::Launch => "Launch Agents and Daemons",
            Place::Installed => "Installed by Its Installer",
            Place::Other => "Other",
        }
    }
}

/// Where things are looked for: your home and the Mac's Library, which tests point elsewhere.
pub struct Roots {
    pub home: PathBuf,
    /// /Library.
    pub local: PathBuf,
    /// Whether to ask the installer's records what its packages put where.
    pub receipts: bool,
}

impl Roots {
    pub fn mac() -> Self {
        Roots { home: home_dir(), local: "/Library".into(), receipts: true }
    }

    fn libraries(&self) -> [PathBuf; 2] {
        [self.home.join("Library"), self.local.clone()]
    }
}

/// How the items in a folder are told to be an app's.
#[derive(Clone, Copy, PartialEq)]
enum By {
    /// Named exactly after one of its ids, like a container.
    Exact,
    /// Named after one of its ids.
    Id,
    /// Named after one of its ids or its names.
    Name,
}

/// The folders in a Library to look in, how and how deep, and where what's found goes.
const FOLDERS: &[(&str, By, usize, Place)] = &[
    ("", By::Name, 0, Place::Other),
    ("Application Support", By::Name, 0, Place::Support),
    ("Caches", By::Name, 0, Place::Caches),
    ("Containers", By::Exact, 0, Place::Containers),
    ("Application Scripts", By::Exact, 0, Place::Containers),
    ("Preferences", By::Name, 0, Place::Preferences),
    ("Preferences/ByHost", By::Id, 0, Place::Preferences),
    ("SyncedPreferences", By::Id, 0, Place::Preferences),
    ("Saved Application State", By::Id, 0, Place::State),
    ("Cookies", By::Id, 0, Place::Web),
    ("HTTPStorages", By::Id, 0, Place::Web),
    ("WebKit", By::Id, 0, Place::Web),
    ("Logs", By::Name, 2, Place::Logs),
    ("LaunchAgents", By::Id, 0, Place::Launch),
    ("LaunchDaemons", By::Id, 0, Place::Launch),
    ("PrivilegedHelperTools", By::Id, 0, Place::Launch),
    ("Services", By::Name, 0, Place::Other),
    ("PreferencePanes", By::Name, 0, Place::Other),
    ("Internet Plug-Ins", By::Name, 0, Place::Other),
    (
        "Application Support/com.apple.sharedfilelist/com.apple.LSSharedFileList.ApplicationRecentDocuments",
        By::Id,
        0,
        Place::Other,
    ),
];

/// Folders macOS makes in a Library for everyone's use, which no app has to itself.
const SHARED: &[&str] = &[
    "Accounts",
    "Application Scripts",
    "Application Support",
    "Audio",
    "Caches",
    "ColorPickers",
    "ColorSync",
    "Components",
    "Containers",
    "Cookies",
    "Developer",
    "Extensions",
    "Fonts",
    "Frameworks",
    "Group Containers",
    "HTTPStorages",
    "Input Methods",
    "Internet Plug-Ins",
    "Keychains",
    "LaunchAgents",
    "LaunchDaemons",
    "Logs",
    "Mail",
    "Preferences",
    "PreferencePanes",
    "Printers",
    "PrivilegedHelperTools",
    "QuickLook",
    "Saved Application State",
    "Screen Savers",
    "Scripts",
    "Services",
    "Sounds",
    "Spotlight",
    "SyncedPreferences",
    "WebKit",
];

/// Everything `app` left, besides those another of the `installed` apps goes by more closely, with
/// the app itself first.
pub fn find(app: &App, installed: &[App], roots: &Roots) -> Vec<Leftover> {
    let others: Vec<&App> = installed.iter().filter(|other| other.path != app.path).collect();
    let signing = if roots.receipts { app.signing() } else { Signing::default() };
    let family =
        others.iter().flat_map(|other| &other.names).any(|other| app.names.iter().any(|name| by_name(other, name)));
    let mut found = Found { app, others: &others, family, items: Vec::new() };
    found.add(enclosing(app), Place::App, true);
    for library in roots.libraries() {
        for &(folder, by, depth, place) in FOLDERS {
            found.search(&library.join(folder), by, depth, place);
        }
        found.groups(&library, &signing);
        found.crash_reports(&library);
    }
    found.launch_jobs(roots);
    found.config(&roots.home);
    if roots.receipts {
        for (path, checked) in receipts::installed_by(app, &others) {
            found.add(path, Place::Installed, checked);
        }
    }
    let mut found = found.items;
    found.sort_by(|a, b| a.path.cmp(&b.path));
    // Something found twice goes once, ticked if either says so, and something inside something
    // else found goes with it, unless it alone is ticked.
    let mut items: Vec<Leftover> = Vec::new();
    for item in found {
        match items.iter_mut().rev().find(|kept| item.path.starts_with(&kept.path)) {
            Some(kept) if kept.path == item.path => kept.checked |= item.checked,
            Some(kept) if kept.checked || !item.checked => {}
            _ => items.push(item),
        }
    }
    items.par_iter_mut().for_each(|item| item.size = fs::disk_usage(&item.path));
    items.sort_by(|a, b| a.place.cmp(&b.place).then_with(|| a.path.cmp(&b.path)));
    items
}

/// The app, or the folder it came in when that goes by its name and has nothing else in it, like
/// /Applications/Example/Example.app.
fn enclosing(app: &App) -> PathBuf {
    let Some(dir) = app.path.parent() else { return app.path.clone() };
    let standard = [&app::folders()[..], &["/Applications/Utilities".into()]].concat();
    let named = dir.file_name().is_some_and(|name| app.names.iter().any(|app| by_name(&name.to_string_lossy(), app)));
    let others = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| entry.path() != app.path && !entry.file_name().to_string_lossy().starts_with('.'));
    // A volume's own folder, like a disk image's, can't go.
    let mount = std::fs::metadata(dir).ok().map(|meta| meta.dev())
        != dir.parent().and_then(|parent| std::fs::metadata(parent).ok()).map(|meta| meta.dev());
    if named && !others && !mount && !standard.iter().any(|folder| folder == dir) {
        dir.to_path_buf()
    } else {
        app.path.clone()
    }
}

struct Found<'a> {
    app: &'a App,
    others: &'a [&'a App],
    /// Whether another app installed goes by a name that starts with the app's, like Example Beta,
    /// which may keep its files in the same place.
    family: bool,
    items: Vec<Leftover>,
}

impl Found<'_> {
    fn add(&mut self, path: PathBuf, place: Place, checked: bool) {
        // Something macOS protects can't go.
        if !fs::is_restricted(&path) {
            self.items.push(Leftover { path, place, size: 0, checked });
        }
    }

    /// Adds what in `dir` is the app's, going `depth` folders down into those that aren't.
    fn search(&mut self, dir: &Path, by: By, depth: usize, place: Place) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if dir.file_name() == Some(OsStr::new("Library")) && SHARED.contains(&name.as_str()) {
                continue;
            }
            match self.claim(&name, by) {
                Some(shared) => self.add(entry.path(), place, !shared),
                None if depth > 0 && entry.file_type().is_ok_and(|kind| kind.is_dir()) => {
                    self.search(&entry.path(), by, depth - 1, place)
                }
                None => {}
            }
        }
    }

    /// Whether `name` is the app's, and if so, whether it may be another installed app's too: one
    /// that goes by it as closely, like a second copy, or by a name starting with the app's, when it
    /// goes by the app's name alone. None when another goes by it more closely, or the app doesn't
    /// at all.
    fn claim(&self, name: &str, by: By) -> Option<bool> {
        let ours = closeness(self.app, name, by)?;
        let theirs = self.others.iter().filter_map(|other| closeness(other, name, by)).max();
        match theirs {
            Some(theirs) if theirs > ours => None,
            theirs => Some(theirs == Some(ours) || (ours < BY_ID && self.family)),
        }
    }

    /// The app groups it shares files in with other apps by its maker, in Group Containers and
    /// Application Scripts, like TEAMID.com.example.App or group.com.example.App. One named after
    /// the app is its own. Any other may hold what the maker's other apps keep, so it isn't ticked.
    fn groups(&mut self, library: &Path, signing: &Signing) {
        for folder in ["Group Containers", "Application Scripts"] {
            for entry in std::fs::read_dir(library.join(folder)).into_iter().flatten().flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let unprefixed = match &signing.team {
                    Some(team) => name.strip_prefix(team.as_str()).and_then(|rest| rest.strip_prefix('.')),
                    None => None,
                }
                .or_else(|| name.strip_prefix("group."))
                .unwrap_or(&name);
                let own = self.claim(unprefixed, By::Id) == Some(false);
                if own || signing.groups.contains(&name) {
                    self.add(entry.path(), Place::Containers, own);
                }
            }
        }
    }

    /// Crash reports for the app's programs, like `Example-2026-01-02-030405.ips`, or
    /// `ExcUserFault_Example-…` for some kinds of them. Those of a program another installed app
    /// carries too, like a maker's updater, aren't ticked.
    fn crash_reports(&mut self, library: &Path) {
        for dir in ["Logs/DiagnosticReports", "Logs/DiagnosticReports/Retired", "Application Support/CrashReporter"] {
            for entry in std::fs::read_dir(library.join(dir)).into_iter().flatten().flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let unkinded = name.split_once('_').map(|(_, rest)| rest);
                let executable = self.app.executables.iter().find(|executable| {
                    [Some(&*name), unkinded].into_iter().flatten().any(|program| {
                        program.strip_prefix(executable.as_str()).is_some_and(|rest| rest.starts_with(['-', '_', '.']))
                    })
                });
                if let Some(executable) = executable {
                    // A helper's may go by a name as plain as Helper, which other apps' could too.
                    let named = *executable == self.app.executables[0]
                        || self.app.names.iter().any(|name| executable.to_lowercase().contains(&name.to_lowercase()));
                    let shared = self.others.iter().any(|other| other.executables.contains(executable));
                    self.add(entry.path(), Place::Logs, named && !shared);
                }
            }
        }
    }

    /// The jobs launchd starts for the app, whatever they're named: those whose program is in it or
    /// that say they're its, and the programs they run from outside it, like a privileged helper.
    /// So are the helpers it asks macOS to install for it.
    fn launch_jobs(&mut self, roots: &Roots) {
        let libraries = roots.libraries();
        let mut folders: Vec<PathBuf> = libraries.iter().map(|library| library.join("LaunchAgents")).collect();
        folders.push(roots.local.join("LaunchDaemons"));
        let helpers: Vec<String> = app::info(&self.app.path)
            .and_then(|info| {
                info.get("SMPrivilegedExecutables")?.as_dictionary().map(|keys| keys.keys().cloned().collect())
            })
            .unwrap_or_default();
        for helper in &helpers {
            let tool = roots.local.join("PrivilegedHelperTools").join(helper);
            if tool.exists() {
                self.add(tool, Place::Launch, true);
            }
        }
        for plist in folders.iter().flat_map(|folder| std::fs::read_dir(folder).into_iter().flatten().flatten()) {
            let plist = plist.path();
            let Some(job) = Value::from_file(&plist).ok().and_then(Value::into_dictionary) else { continue };
            let label = job.get("Label").and_then(Value::as_string).unwrap_or_default();
            let program = launchd::program(&job);
            let theirs = job
                .get("AssociatedBundleIdentifiers")
                .map(|ids| match ids {
                    Value::Array(ids) => ids.iter().filter_map(Value::as_string).map(str::to_owned).collect(),
                    ids => ids.as_string().map(str::to_owned).into_iter().collect::<Vec<_>>(),
                })
                .unwrap_or_default();
            let ours = program.as_ref().is_some_and(|program| program.starts_with(&self.app.path))
                || theirs.iter().any(|id| self.app.ids.contains(id))
                || helpers.iter().any(|helper| helper == label)
                || self.claim(label, By::Id) == Some(false);
            if !ours {
                continue;
            }
            // One that another app installed runs too, like a maker's updater, may still be needed.
            let shared = self.others.iter().any(|other| {
                theirs.iter().any(|id| other.ids.contains(id))
                    || program.as_ref().is_some_and(|program| program.starts_with(&other.path))
            });
            self.add(plist, Place::Launch, !shared);
            // A program it runs from a Library, which nothing else would find, with the app it's in.
            if let Some(program) = program.filter(|program| {
                !program.starts_with(&self.app.path) && libraries.iter().any(|library| program.starts_with(library))
            }) && program.exists()
            {
                self.add(fs::app_bundle(&program).unwrap_or(program), Place::Launch, !shared);
            }
        }
    }

    /// Settings kept in your home folder rather than your Library, like ~/.config/example, which
    /// a command line tool by the same name may share, so they aren't ticked.
    fn config(&mut self, home: &Path) {
        for dir in [home.to_path_buf(), home.join(".config")] {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let name = if dir == home { name.strip_prefix('.') } else { Some(name.as_str()) };
                if name.is_some_and(|name| {
                    self.app.names.iter().any(|app| !app.contains(' ') && app.eq_ignore_ascii_case(name))
                }) && !(dir == home && name == Some("config"))
                {
                    self.add(entry.path(), Place::Other, false);
                }
            }
        }
    }
}

/// How closely what goes by an id does, past any name.
const BY_ID: usize = 1000;

/// How closely `name` goes by `app`: by one of its ids beats by one of its names, and a longer one
/// beats a shorter one, so a beta's files go with the beta. None when it doesn't.
fn closeness(app: &App, name: &str, by: By) -> Option<usize> {
    let by_ids = app.ids.iter().filter(|id| match by {
        By::Exact => name.eq_ignore_ascii_case(id),
        _ => by_id(name, id),
    });
    let by_names = app.names.iter().filter(|_| by == By::Name).filter(|app| by_name(name, app));
    by_ids.map(|id| BY_ID + id.len()).chain(by_names.map(String::len)).max()
}

/// Whether `name` goes by bundle id `id`, like `com.example.App.plist` or `com.example.App.helper`.
/// An id of only two parts, like `com.example`, could start another maker's ids, so it has to be
/// the whole name, less its extension.
pub(super) fn by_id(name: &str, id: &str) -> bool {
    let Some(rest) = strip_prefix_ignoring_case(name, id) else { return false };
    if id.split('.').count() < 3 {
        return whole(rest);
    }
    rest.is_empty() || rest.starts_with(['.', '-', '_'])
}

/// Whether `name` goes by the app name `app`, like `Example`, `Example.log`, `Example 2` or
/// `Example-backup`. A name of three letters or fewer could start too many others, so it has to be
/// the whole name, less its extension.
fn by_name(name: &str, app: &str) -> bool {
    let Some(rest) = strip_prefix_ignoring_case(name, app) else { return false };
    if app.chars().count() <= 3 {
        return whole(rest);
    }
    rest.is_empty() || rest.starts_with(['.', '-', '_', ' ']) || rest.starts_with(|c: char| c.is_ascii_digit())
}

/// Whether what's left of a name after what it goes by is at most an extension.
fn whole(rest: &str) -> bool {
    rest.is_empty() || rest.strip_prefix('.').is_some_and(|ext| !ext.contains('.'))
}

fn strip_prefix_ignoring_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then(|| &text[prefix.len()..])
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path};

    use super::*;
    use crate::{fs::Scratch, mac::uninstall::app::tests::make_app};

    #[test]
    fn matches_ids_and_names_at_a_boundary() {
        assert!(by_id("com.example.App.plist", "com.example.App"));
        assert!(by_id("com.example.app.helper", "com.example.App"));
        assert!(!by_id("com.example.AppHelper.plist", "com.example.App"));
        assert!(by_id("com.example.plist", "com.example"));
        assert!(!by_id("com.example.other.plist", "com.example"));
        assert!(by_name("Example 2", "Example"));
        assert!(by_name("example.log", "Example"));
        assert!(!by_name("Examples", "Example"));
        assert!(by_name("Zed.log", "Zed"));
        assert!(!by_name("Zed Notes", "Zed"));
    }

    #[test]
    fn finds_what_an_app_left() {
        let scratch = Scratch::new("leftovers");
        let home = scratch.0.join("home");
        let local = scratch.0.join("Library");
        let apps = scratch.0.join("Applications");
        let user = home.join("Library");
        let make = |path: &Path| {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "x").unwrap();
        };
        let id = "io.github.sylophi.mutest";
        // What's the app's, each a file or a folder with a file in it.
        let ours = [
            user.join("Application Support/Mu Test/"),
            user.join(format!("Application Support/{id}/")),
            user.join(format!("Caches/{id}/")),
            user.join(format!("Containers/{id}.helper/")),
            user.join(format!("Preferences/{id}.plist")),
            user.join(format!("Preferences/ByHost/{id}.0000-1111.plist")),
            user.join(format!("Saved Application State/{id}.savedState/")),
            user.join(format!("HTTPStorages/{id}.binarycookies")),
            user.join(format!("WebKit/{id}/")),
            user.join("Logs/Mu Test/"),
            user.join("Logs/DiagnosticReports/MuTest-2026-01-02-030405.ips"),
            user.join("Logs/DiagnosticReports/ExcUserFault_MuTest-2026-01-02-030405.ips"),
            local.join(format!("LaunchDaemons/{id}.daemon.plist")),
            local.join("Application Support/Mu Test/"),
        ];
        let theirs = [
            // Another app's, by a longer id or name, and things that only start like ours.
            user.join(format!("Preferences/{id}.beta.plist")),
            user.join("Application Support/Mu Test Beta/"),
            user.join(format!("Preferences/{id}er.plist")),
            user.join("Caches/Mu Testing/"),
            user.join("Logs/DiagnosticReports/MuTester-2026-01-02-030405.ips"),
        ];
        for path in ours.iter().chain(&theirs) {
            match path.to_string_lossy().ends_with('/') {
                true => make(&path.join("data")),
                false => make(path),
            }
        }
        // A job named after nothing in particular, whose program is in the app.
        let app = make_app(&apps, "Mu Test", id, Some(&format!("{id}.helper")));
        fs::rename(app.join("Contents/Info.plist"), app.join("Contents/Info.xml")).unwrap();
        let info = Value::from_file(app.join("Contents/Info.xml")).unwrap();
        let mut info = info.into_dictionary().unwrap();
        info.insert("CFBundleExecutable".into(), "MuTest".into());
        Value::Dictionary(info).to_file_xml(app.join("Contents/Info.plist")).unwrap();
        let agent = user.join("LaunchAgents/agent.plist");
        fs::create_dir_all(agent.parent().unwrap()).unwrap();
        let program = app.join("Contents/MacOS/agent").to_string_lossy().into_owned();
        let job = plist::Dictionary::from_iter([
            ("Label".to_owned(), Value::from("agent")),
            ("ProgramArguments".to_owned(), Value::Array(vec![program.into()])),
        ]);
        Value::Dictionary(job).to_file_xml(&agent).unwrap();
        // A config folder with the app's name.
        make(&home.join(".config/MuTest/config"));

        let beta = make_app(&apps, "Mu Test Beta", &format!("{id}.beta"), None);
        let installed: Vec<App> = [&app, &beta].iter().filter_map(|path| App::new(path)).collect();
        let roots = Roots { home: home.clone(), local: local.clone(), receipts: false };
        let found = find(&installed[0], &installed, &roots);
        let paths: Vec<&Path> = found.iter().map(|item| item.path.as_path()).collect();
        let mut expected: Vec<PathBuf> = ours.iter().map(|path| path.components().collect()).collect();
        expected.extend([app.clone(), agent, home.join(".config/MuTest")]);
        for path in &expected {
            assert!(paths.contains(&path.as_path()), "missing {}", path.display());
        }
        assert_eq!(paths.len(), expected.len(), "found {paths:#?}");
        // What goes by its name alone may be Mu Test Beta's too, and the config a tool's.
        let unticked = [
            user.join("Application Support/Mu Test"),
            user.join("Logs/Mu Test"),
            local.join("Application Support/Mu Test"),
            home.join(".config/MuTest"),
        ];
        for item in &found {
            assert_eq!(item.checked, !unticked.contains(&item.path), "{}", item.path.display());
        }
        assert_eq!(found[0].place, Place::App);
        assert!(found.iter().all(|item| item.size > 0));
    }
}
