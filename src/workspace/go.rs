//! Going to a folder by typing it in the path bar: Tab completes a folder's name like a shell does,
//! and what's typed brings up folders to pick, from where it points and from the folders you go to
//! most.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use gpui_kit::{App, AppContext as _, Global, Task};
use serde::{Deserialize, Serialize};

use crate::{
    fs::{self, home_dir},
    settings,
};

/// How many folders the path bar suggests at most.
const SUGGESTIONS: usize = 8;

/// How many folders are remembered, the ones gone to most.
const REMEMBERED: usize = 500;

/// The folders you've gone to, how often and when last.
#[derive(Default, Serialize, Deserialize)]
struct Visits(HashMap<PathBuf, Visit>);

#[derive(Clone, Copy, Serialize, Deserialize)]
struct Visit {
    count: u32,
    /// Seconds since 1970.
    last: u64,
}

impl Global for Visits {}

impl Visit {
    /// How likely you're after it again: how often you've gone there, weighed by how lately, the
    /// way zoxide ranks folders.
    fn score(&self, now: u64) -> f64 {
        let weight = match now.saturating_sub(self.last) {
            age if age < 60 * 60 => 4.,
            age if age < 24 * 60 * 60 => 2.,
            age if age < 7 * 24 * 60 * 60 => 0.5,
            _ => 0.25,
        };
        self.count as f64 * weight
    }
}

impl Visits {
    /// The folders gone to, the likeliest to be gone to again first.
    fn ranked(&self) -> Vec<(&PathBuf, &Visit)> {
        let now = now();
        let mut visits: Vec<_> = self.0.iter().collect();
        visits.sort_by(|(_, a), (_, b)| b.score(now).total_cmp(&a.score(now)));
        visits
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs())
}

pub fn init(cx: &mut App) {
    cx.set_global::<Visits>(settings::load("visits.json"));
    cx.on_app_quit(|cx| {
        save(cx);
        async {}
    })
    .detach();
}

/// Counts going to `dir`.
pub fn visit(dir: &Path, cx: &mut App) {
    let visit = cx.global_mut::<Visits>().0.entry(dir.to_path_buf()).or_insert(Visit { count: 0, last: 0 });
    visit.count += 1;
    visit.last = now();
}

/// Keeps the folders gone to most.
fn save(cx: &App) {
    let visits = cx.global::<Visits>().ranked().into_iter().take(REMEMBERED).map(|(dir, visit)| (dir.clone(), *visit));
    settings::store("visits.json", &Visits(visits.collect()));
}

/// What's typed, split where its last name starts: the folder it's in, and the start of the name.
fn split(text: &str) -> (&str, &str) {
    match text.rfind('/') {
        Some(ix) => (&text[..=ix], &text[ix + 1..]),
        None => ("", text),
    }
}

/// The folder `text` names: from the home folder with `~`, from `cwd` unless it starts at `/`.
pub fn resolve(text: &str, cwd: &Path) -> PathBuf {
    let path = match text.strip_prefix('~') {
        Some(rest) => home_dir().join(rest.trim_start_matches('/')),
        None => PathBuf::from(text),
    };
    if path.is_relative() { cwd.join(path) } else { path }
}

/// The folders in `dir` whose names start with `start`, ignoring case, in order. Hidden ones show
/// when `hidden`, or when `start` asks for them. Packages like apps are left out, as they open
/// instead, and so are aliases, as a typed path can't go through one.
fn folders(dir: &Path, start: &str, hidden: bool) -> Vec<String> {
    let hidden = hidden || start.starts_with('.');
    let start = start.to_lowercase();
    let mut names: Vec<String> = fs::list_dir(dir)
        .unwrap_or_default()
        .into_iter()
        .filter(|entry| entry.folder && entry.alias.is_none() && (hidden || !entry.name.starts_with('.')))
        .filter(|entry| entry.name.to_lowercase().starts_with(&start))
        .map(|entry| entry.name.into())
        .collect();
    names.sort_by(|a, b| fs::natural_cmp(a, b));
    names
}

/// `text` with the name it ends in completed, as far as the folders it could be agree, and past
/// the `/` when only one can. None when there's nothing to add.
pub fn complete(text: &str, cwd: &Path, hidden: bool) -> Option<String> {
    let (dir, start) = split(text);
    let names = folders(&resolve(dir, cwd), start, hidden);
    let completed = match names.as_slice() {
        [] => return None,
        [name] => format!("{name}/"),
        [first, rest @ ..] => {
            let mut common: Vec<char> = first.chars().collect();
            for name in rest {
                let same = common.iter().zip(name.chars()).take_while(|(a, b)| a.to_lowercase().eq(b.to_lowercase()));
                common.truncate(same.count());
            }
            common.into_iter().collect()
        }
    };
    (completed.chars().count() > start.chars().count()).then(|| format!("{dir}{completed}"))
}

/// The folders to suggest for `text`: those in the folder it points to whose names start the way
/// it ends, then, when it's a bare name, folders you go to whose names have it in them. With
/// nothing typed, the folders you go to most. They're found in the background, as a folder on a
/// server can take a while to list.
pub fn suggestions(text: &str, cwd: &Path, hidden: bool, cx: &App) -> Task<Vec<PathBuf>> {
    let (dir, start) = split(text);
    let (dir, start) = (resolve(dir, cwd), start.to_string());
    let visited: Vec<PathBuf> = if text.contains('/') || text.starts_with('~') {
        Vec::new()
    } else {
        let needle = text.to_lowercase();
        cx.global::<Visits>()
            .ranked()
            .into_iter()
            .map(|(path, _)| path)
            .filter(|path| path.file_name().is_some_and(|name| name.to_string_lossy().to_lowercase().contains(&needle)))
            .cloned()
            .collect()
    };
    cx.background_spawn(async move {
        let mut found: Vec<PathBuf> =
            folders(&dir, &start, hidden).into_iter().take(SUGGESTIONS).map(|name| dir.join(name)).collect();
        // Checked last, as it asks the disk, and only until there are enough.
        let visited: Vec<PathBuf> = visited
            .into_iter()
            .filter(|path| !found.contains(path) && path.is_dir())
            .take(SUGGESTIONS - found.len())
            .collect();
        found.extend(visited);
        found
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs::Scratch;

    #[test]
    fn completes_like_a_shell() {
        let scratch = Scratch::new("go");
        let root = &scratch.0;
        for dir in ["Documents", "Downloads", "Desktop", ".hidden", "Music.app"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::write(root.join("Doodle.txt"), "").unwrap();
        let complete = |text: &str| complete(text, root, false);
        // Only folders count, matched without case, and one match goes on into it.
        assert_eq!(complete("doc").as_deref(), Some("Documents/"));
        assert_eq!(complete("Do").as_deref(), None);
        assert_eq!(complete("Dow").as_deref(), Some("Downloads/"));
        assert_eq!(complete("./De").as_deref(), Some("./Desktop/"));
        // Hidden folders only when asked for, and packages never.
        assert_eq!(complete(".h").as_deref(), Some(".hidden/"));
        assert_eq!(complete("Mus"), None);
        assert_eq!(complete(&format!("{}/Docu", root.display())), Some(format!("{}/Documents/", root.display())));
    }

    #[test]
    fn completes_as_far_as_names_agree() {
        let scratch = Scratch::new("go-agree");
        for dir in ["Project One", "project two", "Other"] {
            std::fs::create_dir_all(scratch.0.join(dir)).unwrap();
        }
        assert_eq!(complete("p", &scratch.0, false).as_deref(), Some("Project "));
    }
}
