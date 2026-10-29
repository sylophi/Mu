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
    pub favorites: Vec<PathBuf>,
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
            favorites: vec![
                home.clone(),
                home.join("Desktop"),
                home.join("Documents"),
                home.join("Downloads"),
                "/Applications".into(),
            ],
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

/// Remembers `value` in file `name`.
pub fn store(name: &str, value: &impl Serialize) {
    let _ = std::fs::create_dir_all(support_dir());
    let _ = std::fs::write(support_dir().join(name), serde_json::to_vec_pretty(value).unwrap());
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

    pub fn is_favorite(&self, path: &Path) -> bool {
        self.favorites.iter().any(|favorite| favorite == path)
    }

    /// Puts `dirs` in Favorites before the one at `ix`, moving any that are there already.
    pub fn place_favorites(&mut self, ix: usize, dirs: &[PathBuf]) {
        let ix = ix.min(self.favorites.len());
        let ix = ix - self.favorites[..ix].iter().filter(|favorite| dirs.contains(favorite)).count();
        self.favorites.retain(|favorite| !dirs.contains(favorite));
        self.favorites.splice(ix..ix, dirs.iter().cloned());
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::Settings;

    #[test]
    fn places_favorites() {
        let paths = |names: &str| names.chars().map(|name| PathBuf::from(name.to_string())).collect::<Vec<_>>();
        let mut settings = Settings { favorites: paths("abcd"), ..Settings::default() };
        settings.place_favorites(3, &paths("a"));
        assert_eq!(settings.favorites, paths("bcad"));
        settings.place_favorites(0, &paths("d"));
        assert_eq!(settings.favorites, paths("dbca"));
        settings.place_favorites(2, &paths("xa"));
        assert_eq!(settings.favorites, paths("dbxac"));
        settings.place_favorites(9, &paths("b"));
        assert_eq!(settings.favorites, paths("dxacb"));
    }
}
