//! The shelf: files and folders set aside to be dragged somewhere else later, so moving them takes
//! no second tab or window. It's the same in every window, and lasts until Mu quits.

use std::path::{Path, PathBuf};

use gpui_kit::{App, BorrowAppContext as _, Global};

use crate::{fs, history::History};

#[derive(Default)]
pub struct Shelf(Vec<Item>);

impl Global for Shelf {}

pub struct Item {
    pub path: PathBuf,
    /// Whether it's a folder, and not a package like an app.
    pub folder: bool,
}

pub fn init(cx: &mut App) {
    cx.set_global(Shelf::default());
    // What a file operation moves off the shelf, or undoing one, is no longer where the shelf has it.
    cx.observe_global::<History>(|cx| {
        if items(cx).iter().any(|item| !exists(&item.path)) {
            cx.update_global::<Shelf, _>(|shelf, _| shelf.0.retain(|item| exists(&item.path)));
        }
    })
    .detach();
}

pub fn items(cx: &App) -> &[Item] {
    &cx.global::<Shelf>().0
}

pub fn add(paths: &[PathBuf], cx: &mut App) {
    cx.update_global::<Shelf, _>(|shelf, _| {
        for path in paths {
            if !shelf.0.iter().any(|item| &item.path == path) {
                let folder = path.is_dir() && !fs::is_package(path);
                shelf.0.push(Item { path: path.clone(), folder });
            }
        }
    });
}

pub fn remove(path: &Path, cx: &mut App) {
    cx.update_global::<Shelf, _>(|shelf, _| shelf.0.retain(|item| item.path != path));
}

pub fn clear(cx: &mut App) {
    cx.update_global::<Shelf, _>(|shelf, _| shelf.0.clear());
}

/// Whether there's anything at `path`, even a link to nothing.
fn exists(path: &Path) -> bool {
    path.symlink_metadata().is_ok()
}
