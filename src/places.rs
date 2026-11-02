//! The disks mounted and how many items are in the Trash, which every window's sidebar shows, kept
//! up to date as they change.

use std::{path::Path, time::Duration};

use gpui_kit::*;

use crate::{
    fs::{self, Volume},
    icons,
    permissions::Permissions,
};

pub struct Places {
    /// Mounted volumes, the startup disk first.
    pub volumes: Vec<Volume>,
    /// How many items are in the Trash.
    pub trash: usize,
    _trash_watch: Task<()>,
}

impl Global for Places {}

pub fn init(cx: &mut App) {
    cx.set_global(Places { volumes: fs::volumes(), trash: 0, _trash_watch: Task::ready(()) });
    watch(Path::new("/Volumes"), fs::volumes, |places, volumes| places.volumes = volumes, cx).detach();
    watch_trash(cx);
    // The Trash can't be read without Full Disk Access, so it's read again once that's granted.
    cx.observe(&Permissions::global(cx), |_, cx| watch_trash(cx)).detach();
}

/// Counts what's in the Trash, and again whenever that changes.
fn watch_trash(cx: &mut App) {
    let watch = watch(&fs::trash_dir(), trash_count, |places, count| places.trash = count, cx);
    cx.update_global::<Places, _>(|places, _| {
        places.trash = trash_count();
        places._trash_watch = watch;
    });
    // Its icon shows whether it's empty, which can't be told before Full Disk Access.
    icons::refresh(&[fs::trash_dir()], cx);
}

/// Once `dir` settles after a change to its items, reads it again with `read` in the background and
/// hands that to `apply`, until the task is dropped.
fn watch<T: Send + 'static>(dir: &Path, read: fn() -> T, apply: fn(&mut Places, T), cx: &mut App) -> Task<()> {
    let changes = fs::watch(vec![dir.to_path_buf()], false, Duration::from_millis(300), cx.background_executor());
    cx.spawn(async move |cx| {
        while let Some(changed) = changes.next().await {
            cx.update(|cx| icons::refresh(&changed.paths, cx));
            if changed.items {
                let value = cx.background_spawn(async move { read() }).await;
                cx.update_global::<Places, _>(|places, _| apply(places, value));
            }
        }
    })
}

fn trash_count() -> usize {
    fs::trash_items().map_or(0, |items| items.len())
}
