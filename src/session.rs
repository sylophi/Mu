//! The windows and tabs open when Mu last closed, saved as JSON in Application Support, so it
//! opens them again at launch.

use std::path::PathBuf;

use gpui_kit::{Bounds, Pixels, WindowBounds};
use serde::{Deserialize, Serialize};

use crate::settings;

/// What a tab shows, enough to open it again.
#[derive(Clone, Serialize, Deserialize)]
pub enum Tab {
    Folder(PathBuf),
    /// A shell, started again in the folder it was last in.
    Terminal(PathBuf),
    Editor(PathBuf),
    /// Find in folder, from this folder.
    Search(PathBuf),
    Activity,
    Storage(PathBuf),
    History,
    Settings,
}

impl Tab {
    /// Where it opens, unless it opens anywhere.
    pub fn path(&self) -> Option<&PathBuf> {
        match self {
            Tab::Folder(path) | Tab::Terminal(path) | Tab::Editor(path) | Tab::Search(path) | Tab::Storage(path) => {
                Some(path)
            }
            Tab::Activity | Tab::History | Tab::Settings => None,
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Window {
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub bounds: Bounds<Pixels>,
    pub shown: Shown,
    /// The display it was on, by its UUID, which unlike its ID lasts across restarts.
    pub display: Option<String>,
}

/// How a window was shown, as in `WindowBounds`.
#[derive(Clone, Copy, Serialize, Deserialize)]
pub enum Shown {
    Windowed,
    Maximized,
    Fullscreen,
}

impl Window {
    pub fn new(tabs: Vec<Tab>, active: usize, bounds: WindowBounds, display: Option<String>) -> Self {
        let (shown, bounds) = match bounds {
            WindowBounds::Windowed(bounds) => (Shown::Windowed, bounds),
            WindowBounds::Maximized(bounds) => (Shown::Maximized, bounds),
            WindowBounds::Fullscreen(bounds) => (Shown::Fullscreen, bounds),
        };
        Self { tabs, active, bounds, shown, display }
    }

    /// Where it goes, with `bounds` in place of its own.
    pub fn window_bounds(&self, bounds: Bounds<Pixels>) -> WindowBounds {
        match self.shown {
            Shown::Windowed => WindowBounds::Windowed(bounds),
            Shown::Maximized => WindowBounds::Maximized(bounds),
            Shown::Fullscreen => WindowBounds::Fullscreen(bounds),
        }
    }
}

/// The windows open when Mu last closed, the front one first.
pub fn load() -> Vec<Window> {
    settings::load("session.json")
}

pub fn save(windows: &[Window]) {
    settings::store("session.json", &windows);
}
