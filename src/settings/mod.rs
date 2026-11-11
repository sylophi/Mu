//! App-wide preferences, saved as JSON in Application Support.

pub mod page;

use std::path::{Path, PathBuf};

use gpui_kit::{App, BorrowAppContext as _, Global, Pixels, px};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::fs::home_dir;

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub show_hidden: bool,
    /// Whether new windows show the sidebar and the preview, as the last window to change them did.
    pub show_sidebar: bool,
    pub show_preview: bool,
    /// Whether folders show as a grid of icons instead of a list.
    pub grid: bool,
    /// Whether the list shows what's in the folders in it, under them, as far as it has room.
    pub smart_expansion: bool,
    /// The folders added to the sidebar outside any group.
    #[serde(alias = "favorites")]
    pub sidebar_items: Vec<PathBuf>,
    pub sidebar_groups: Vec<SidebarGroup>,
    /// How wide the list's Name, Date Modified, Size and Kind columns are.
    pub column_widths: [Pixels; 4],
    /// How wide the sidebar and the preview are in new windows, as in the last window to change them.
    pub dock_widths: DockWidths,
    /// Whether the greys lean toward a hue instead of being neutral.
    pub tint: bool,
    /// How far their tint is turned around the hue circle from Monokai's, in degrees.
    pub tint_turn: f32,
    /// Whether plain text that would open in TextEdit opens in the editor instead.
    pub edit_text: bool,
    /// Whether opening an archive extracts it here instead of handing it to its app.
    pub extract_archives: bool,
}

impl Global for Settings {}

/// The list's column widths to begin with, which a double click on a column's divider goes back to.
pub const COLUMN_WIDTHS: [Pixels; 4] = [px(280.), px(170.), px(80.), px(150.)];

/// Folders added to the sidebar under a heading of their own.
#[derive(Serialize, Deserialize)]
pub struct SidebarGroup {
    pub name: String,
    pub collapsed: bool,
    pub items: Vec<PathBuf>,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
pub struct DockWidths {
    pub sidebar: Pixels,
    pub preview: Pixels,
}

/// The docks' widths to begin with, which a double click on a dock's edge goes back to.
pub const DOCK_WIDTHS: DockWidths = DockWidths { sidebar: px(170.), preview: px(240.) };

impl Default for Settings {
    fn default() -> Self {
        let home = home_dir();
        Self {
            show_hidden: false,
            show_sidebar: true,
            show_preview: false,
            grid: false,
            smart_expansion: true,
            sidebar_items: vec![
                home.clone(),
                home.join("Desktop"),
                home.join("Documents"),
                home.join("Downloads"),
                "/Applications".into(),
            ],
            sidebar_groups: Vec::new(),
            column_widths: COLUMN_WIDTHS,
            dock_widths: DOCK_WIDTHS,
            tint: false,
            tint_turn: 0.,
            edit_text: true,
            extract_archives: true,
        }
    }
}

/// Where Mu keeps what it remembers.
fn support_dir() -> PathBuf {
    home_dir().join("Library/Application Support/Mu")
}

/// What Mu remembered in file `name`, or the default when there's nothing to go on.
pub fn load<T: DeserializeOwned + Default>(name: &str) -> T {
    std::fs::read(support_dir().join(name))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Remembers `value` in file `name`. It's written aside and then moved over the old one, so
/// quitting halfway leaves the old one whole.
pub fn store(name: &str, value: &impl Serialize) {
    let dir = support_dir();
    let temp = dir.join(format!(".{name}"));
    let _ = std::fs::create_dir_all(&dir);
    // A path that isn't UTF-8 can't be written as JSON.
    let Ok(json) = serde_json::to_vec_pretty(value) else { return };
    let _ = std::fs::write(&temp, json).and_then(|()| std::fs::rename(&temp, dir.join(name)));
}

impl Settings {
    pub fn get(cx: &App) -> &Self {
        cx.global::<Self>()
    }

    pub fn load(cx: &mut App) {
        cx.set_global::<Settings>(load("settings.json"));
    }

    pub fn update(cx: &mut App, f: impl FnOnce(&mut Self)) {
        cx.update_global::<Self, _>(|settings, _| {
            f(settings);
            store("settings.json", settings);
        });
    }

    pub fn in_sidebar(&self, path: &Path) -> bool {
        self.sidebar_items.iter().chain(self.sidebar_groups.iter().flat_map(|group| &group.items)).any(|p| p == path)
    }

    /// The folders in `group`, or outside any group.
    pub fn sidebar_list(&self, group: Option<usize>) -> &[PathBuf] {
        match group {
            Some(group) => &self.sidebar_groups[group].items,
            None => &self.sidebar_items,
        }
    }

    pub fn remove_from_sidebar(&mut self, paths: &[PathBuf]) {
        self.sidebar_items.retain(|item| !paths.contains(item));
        for group in &mut self.sidebar_groups {
            group.items.retain(|item| !paths.contains(item));
        }
    }

    /// Puts `dirs` in `group` before the one at `ix`, moving any that are in the sidebar already.
    pub fn place_in_sidebar(&mut self, group: Option<usize>, ix: usize, dirs: &[PathBuf]) {
        let list = self.sidebar_list(group);
        let ix = ix.min(list.len());
        let ix = ix - list[..ix].iter().filter(|item| dirs.contains(item)).count();
        self.remove_from_sidebar(dirs);
        let list = match group {
            Some(group) => &mut self.sidebar_groups[group].items,
            None => &mut self.sidebar_items,
        };
        list.splice(ix..ix, dirs.iter().cloned());
    }

    /// Takes away the group at `ix`, leaving its folders in the sidebar outside any group.
    pub fn remove_sidebar_group(&mut self, ix: usize) {
        let group = self.sidebar_groups.remove(ix);
        self.sidebar_items.extend(group.items);
    }

    /// Moves the group at `from` to before the one that's at `to`.
    pub fn move_sidebar_group(&mut self, from: usize, to: usize) {
        let group = self.sidebar_groups.remove(from);
        self.sidebar_groups.insert(if to > from { to - 1 } else { to }, group);
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{Settings, SidebarGroup};

    fn paths(names: &str) -> Vec<PathBuf> {
        names.chars().map(|name| PathBuf::from(name.to_string())).collect()
    }

    fn group(name: &str, items: &str) -> SidebarGroup {
        SidebarGroup { name: name.to_string(), collapsed: false, items: paths(items) }
    }

    #[test]
    fn places_in_the_sidebar() {
        let mut settings = Settings { sidebar_items: paths("abcd"), ..Settings::default() };
        settings.place_in_sidebar(None, 3, &paths("a"));
        assert_eq!(settings.sidebar_items, paths("bcad"));
        settings.place_in_sidebar(None, 0, &paths("d"));
        assert_eq!(settings.sidebar_items, paths("dbca"));
        settings.place_in_sidebar(None, 2, &paths("xa"));
        assert_eq!(settings.sidebar_items, paths("dbxac"));
        settings.place_in_sidebar(None, 9, &paths("b"));
        assert_eq!(settings.sidebar_items, paths("dxacb"));
    }

    #[test]
    fn places_between_groups() {
        let mut settings = Settings {
            sidebar_items: paths("ab"),
            sidebar_groups: vec![group("one", "cd"), group("two", "e")],
            ..Settings::default()
        };
        settings.place_in_sidebar(Some(1), 1, &paths("a"));
        assert_eq!(settings.sidebar_items, paths("b"));
        assert_eq!(settings.sidebar_groups[1].items, paths("ea"));
        settings.place_in_sidebar(Some(0), 1, &paths("ed"));
        assert_eq!(settings.sidebar_groups[0].items, paths("ced"));
        assert_eq!(settings.sidebar_groups[1].items, paths("a"));
        settings.place_in_sidebar(None, 0, &paths("c"));
        assert_eq!(settings.sidebar_items, paths("cb"));
        assert_eq!(settings.sidebar_groups[0].items, paths("ed"));
    }

    #[test]
    fn removing_a_group_keeps_its_items() {
        let mut settings = Settings {
            sidebar_items: paths("a"),
            sidebar_groups: vec![group("one", "bc"), group("two", "d")],
            ..Settings::default()
        };
        settings.remove_sidebar_group(0);
        assert_eq!(settings.sidebar_items, paths("abc"));
        assert_eq!(settings.sidebar_groups.len(), 1);
    }

    #[test]
    fn moves_groups() {
        let names = |settings: &Settings| settings.sidebar_groups.iter().map(|g| g.name.clone()).collect::<String>();
        let mut settings =
            Settings { sidebar_groups: vec![group("a", ""), group("b", ""), group("c", "")], ..Settings::default() };
        settings.move_sidebar_group(0, 2);
        assert_eq!(names(&settings), "bac");
        settings.move_sidebar_group(2, 0);
        assert_eq!(names(&settings), "cba");
        settings.move_sidebar_group(0, 3);
        assert_eq!(names(&settings), "bac");
    }
}
