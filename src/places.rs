//! The disks mounted and how many items are in the Trash, which every window's sidebar shows, kept
//! up to date as they change.

use std::{path::PathBuf, time::Duration};

use gpui_kit::*;

use crate::{
    fs::{self, Volume},
    icons,
    permissions::Permissions,
};

pub struct Places {
    /// Mounted volumes, the startup disk first.
    pub volumes: Vec<Volume>,
    /// Each disk's Trash, which Mu's shows as one (see `fs::trash_dirs`).
    pub trash_dirs: Vec<PathBuf>,
    /// How many items are in them.
    pub trash: usize,
    _trash_watch: Task<()>,
}

impl Global for Places {}

pub fn init(cx: &mut App) {
    let places = Places { volumes: fs::volumes(), trash_dirs: Vec::new(), trash: 0, _trash_watch: Task::ready(()) };
    cx.set_global(places);
    let apply = |volumes, cx: &mut App| {
        cx.update_global::<Places, _>(|places, _| places.volumes = volumes);
        find_trashes(cx);
    };
    watch(&[PathBuf::from("/Volumes")], fs::volumes, apply, cx).detach();
    watch_trash(fs::trash_dirs(), cx);
    // The Trash can't be read without Full Disk Access, so it's read again once that's granted.
    cx.observe(&Permissions::global(cx), |_, cx| watch_trash(fs::trash_dirs(), cx)).detach();
}

/// Watches the Trashes again if they've changed: a disk came or went, or got a Trash of its own.
pub fn find_trashes(cx: &mut App) {
    let dirs = fs::trash_dirs();
    if dirs != cx.global::<Places>().trash_dirs {
        watch_trash(dirs, cx);
    }
}

/// Counts what's in the Trashes `dirs`, and again whenever that changes.
fn watch_trash(dirs: Vec<PathBuf>, cx: &mut App) {
    let apply = |count, cx: &mut App| {
        cx.update_global::<Places, _>(|places, _| places.trash = count);
        // Its icon shows whether any has anything in it, which can't be told before Full Disk
        // Access.
        icons::refresh(&[fs::trash_dir()], cx);
    };
    let watch = watch(&dirs, trash_count, apply, cx);
    cx.update_global::<Places, _>(|places, _| {
        places.trash_dirs = dirs;
        places._trash_watch = watch;
    });
}

/// Reads `dirs` with `read` in the background and hands that to `apply`, then again each time they
/// settle after a change to their items, until the task is dropped.
fn watch<T: Send + 'static>(dirs: &[PathBuf], read: fn() -> T, apply: fn(T, &mut App), cx: &mut App) -> Task<()> {
    let changes = fs::watch(dirs.to_vec(), false, Duration::from_millis(300), cx.background_executor());
    cx.spawn(async move |cx| {
        let value = cx.background_spawn(async move { read() }).await;
        cx.update(|cx| apply(value, cx));
        while let Some(changed) = changes.next().await {
            cx.update(|cx| icons::refresh(&changed.paths, cx));
            if changed.items {
                let value = cx.background_spawn(async move { read() }).await;
                cx.update(|cx| apply(value, cx));
            }
        }
    })
}

fn trash_count() -> usize {
    fs::trash_items().map_or(0, |items| items.len())
}
