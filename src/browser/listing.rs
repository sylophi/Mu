//! Loading the folder and keeping up with it: its items, their order, and which of them show.

use std::{
    cmp::Ordering,
    collections::HashMap,
    io,
    ops::Index,
    path::{Path, PathBuf},
    time::Duration,
};

use gpui_kit::*;
use objc2_app_kit::NSEvent;

use super::{
    Browser,
    layout::Layout,
    view::{ROW_ICON, TILE_ICON},
};
use crate::{
    fs::{self, Changes, Entry},
    icons,
    permissions::Permissions,
    settings::Settings,
};

/// How long going to a folder waits for its listing. Most folders list in a millisecond or two, so
/// they show up in the very next frame, without a frame of the folder that was there before.
const LISTING_WAIT: Duration = Duration::from_millis(20);

/// What the listing is sorted by, one for each of the list's columns.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum SortKey {
    #[default]
    Name,
    Modified,
    Size,
    Kind,
}

impl SortKey {
    /// In the order of the list's columns.
    pub(super) const ALL: [SortKey; 4] = [SortKey::Name, SortKey::Modified, SortKey::Size, SortKey::Kind];

    /// Its column's heading, which Sort By lists it by too.
    pub(super) fn label(self) -> &'static str {
        match self {
            SortKey::Name => "Name",
            SortKey::Modified => "Date Modified",
            SortKey::Size => "Size",
            SortKey::Kind => "Kind",
        }
    }
}

/// What to do once the next listing arrives.
#[derive(Default)]
pub(super) struct AfterLoad {
    pub(super) select: Vec<PathBuf>,
    pub(super) rename: bool,
}

/// A folder's items, sorted, and how Finder lays it out, if it does.
type Listed = (io::Result<Vec<Entry>>, Option<Layout>);

/// A folder got ready to open, so that it opens with its items, their icons and their pictures
/// there: the folder under the cursor, or one from a press that may be the start of what opens it,
/// a click or a double click. Like the folder shown, it's watched from when it's listed, so what
/// changes until it opens isn't missed.
pub(super) struct Prefetch {
    path: PathBuf,
    /// Got ready for the cursor, and let go of once it moves off, rather than for a press.
    cursor: bool,
    changes: Changes,
    listing: Task<Listed>,
    _draw: Task<()>,
}

/// What's in the folder, in order, and which of it shows.
#[derive(Default)]
pub(super) struct Listing {
    entries: Vec<Entry>,
    /// Indices into `entries`, without hidden files and filtered by name.
    visible: Vec<usize>,
    /// Where each item is in `entries`, by name.
    by_name: HashMap<SharedString, usize>,
    pub(super) sort: SortKey,
    pub(super) descending: bool,
}

impl Listing {
    /// How many items show.
    pub(super) fn len(&self) -> usize {
        self.visible.len()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.visible.is_empty()
    }

    /// The items that show, in order.
    pub(super) fn iter(&self) -> impl DoubleEndedIterator<Item = &Entry> {
        self.visible.iter().map(|&ix| &self.entries[ix])
    }

    /// Where the item named `name` shows, if it does.
    pub(super) fn position(&self, name: &str) -> Option<usize> {
        // `visible` keeps to the order of `entries`.
        self.visible.binary_search(self.by_name.get(name)?).ok()
    }

    /// Puts the items in the order they show in.
    fn sort(&mut self) {
        let (sort, descending) = (self.sort, self.descending);
        self.entries.sort_by(|a, b| compare(a, b, sort, descending));
        self.by_name = self.entries.iter().enumerate().map(|(ix, entry)| (entry.name.clone(), ix)).collect();
    }
}

/// The `ix`th item that shows.
impl Index<usize> for Listing {
    type Output = Entry;

    fn index(&self, ix: usize) -> &Entry {
        &self.entries[self.visible[ix]]
    }
}

impl Browser {
    pub(super) fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Started right away, not when the task that shows it first runs, which can be a while at
        // launch.
        let listing = self.list(self.path.clone(), window, cx);
        self.load_from(listing, Duration::ZERO, window, cx);
    }

    /// Loads the folder again, then selects whichever of `paths` are in it, and with `rename`,
    /// starts renaming the first. With no `paths`, what an earlier call asked for still goes.
    pub(super) fn reload_selecting(
        &mut self,
        paths: Vec<PathBuf>,
        rename: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !paths.is_empty() {
            self.after_load = AfterLoad { select: paths, rename };
        }
        self.load(window, cx);
    }

    /// Shows what `listing` comes to, giving it up to `wait` to come first.
    fn load_from(&mut self, listing: Task<Listed>, wait: Duration, window: &mut Window, cx: &mut Context<Self>) {
        self.loading = true;
        let listing = match cx.foreground_executor().block_with_timeout(wait, listing) {
            Ok(listing) => {
                self._load = Task::ready(());
                return self.apply_listing(listing, window, cx);
            }
            Err(listing) => listing,
        };
        // Replaced by the next load, which drops this one, so it never lands in another folder.
        self._load = cx.spawn_in(window, async move |this, cx| {
            let listing = listing.await;
            this.update_in(cx, |this, window, cx| this.apply_listing(listing, window, cx)).ok();
        });
    }

    /// Lists `path` in the background, sorted as this listing is.
    fn list(&self, path: PathBuf, window: &Window, cx: &App) -> Task<Listed> {
        let (sort, descending, scale) = (self.listing.sort, self.listing.descending, window.scale_factor());
        cx.background_spawn(async move {
            // Sorted here, so sorting it again on the main thread only checks the order.
            let listing = fs::list_dir(&path).map(|mut entries| {
                entries.sort_by(|a, b| compare(a, b, sort, descending));
                entries
            });
            (listing, Layout::read(&path, scale))
        })
    }

    /// Gets the folder at `path` ready to open from a press, in place of this one.
    pub fn prefetch(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.get_ready(path, false, window, cx);
    }

    /// Keeps the folder under the cursor ready to open, so ⌘↓ opens it as soon as a double click
    /// does, and ⌘] goes forward as soon, to the folder ⌘[ came out of and put the cursor on.
    pub(super) fn prefetch_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A press's goes first, until it's let go of.
        if self.prefetch.as_ref().is_some_and(|prefetch| !prefetch.cursor) {
            return;
        }
        // More than one opens in tabs instead.
        let single = self.selection.len() == 1;
        let Some(folder) = self.selected_cursor_entry().filter(|entry| entry.folder && single) else {
            self.prefetch = None;
            return;
        };
        if self.prefetch.as_ref().is_none_or(|prefetch| prefetch.path != folder.target()) {
            self.get_ready(folder.target().to_path_buf(), true, window, cx);
        }
    }

    /// Gets the folder at `path` ready to open, in place of this one: watches and lists it, then
    /// draws the icons and pictures it opens with.
    fn get_ready(&mut self, path: PathBuf, cursor: bool, window: &mut Window, cx: &mut Context<Self>) {
        if path == self.path {
            return;
        }
        let changes = watcher(&path, cx);
        let settings = Settings::get(cx);
        let (show_hidden, first) = (settings.show_hidden, self.screenful(settings.grid));
        let (sender, receiver) = smol::channel::bounded(1);
        let listing = self.list(path.clone(), window, cx);
        let listing = cx.background_spawn(async move {
            let listed = listing.await;
            // What it shows first, unless Finder lays it out.
            if let (Ok(entries), None) = &listed {
                let shown = entries.iter().filter(|entry| show_hidden || !entry.hidden);
                sender.try_send(shown.take(first).cloned().collect::<Vec<_>>()).ok();
            }
            listed
        });
        let draw = cx.spawn_in(window, async move |this, cx| {
            if let Ok(entries) = receiver.recv().await {
                this.update_in(cx, |this, window, cx| this.draw_ahead(&entries, window, cx)).ok();
            }
            if cursor {
                return;
            }
            // Once a double click from the press is past, it's the cursor's, kept only if it's the
            // folder under it, rather than kept watching and going stale until the next folder.
            let double_click = Duration::from_secs_f64(NSEvent::doubleClickInterval());
            cx.background_executor().timer(double_click).await;
            this.update_in(cx, |this, window, cx| {
                if let Some(prefetch) = &mut this.prefetch {
                    prefetch.cursor = true;
                }
                this.prefetch_cursor(window, cx);
            })
            .ok();
        });
        self.prefetch = Some(Prefetch { path, cursor, changes, listing, _draw: draw });
    }

    /// Starts drawing the icons and pictures of `entries`, so they show as soon as their folder does.
    fn draw_ahead(&mut self, entries: &[Entry], window: &mut Window, cx: &mut Context<Self>) {
        let (grid, scale) = (Settings::get(cx).grid, window.scale_factor());
        if grid {
            // Each folder the cursor stops on adds its pictures, which otherwise pile up until the
            // next folder.
            self.tiles.trim(Some(&self.path), Some(window), cx);
        }
        for entry in entries {
            if grid {
                icons::prefetch(entry, TILE_ICON, scale, cx);
                self.tiles.get(entry, window, cx, |this| &mut this.tiles);
            } else {
                icons::prefetch(entry, ROW_ICON, scale, cx);
            }
        }
    }

    /// Keeps up with the folder just gone to and lists it, taking what was got ready for it.
    pub(super) fn arrive(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (changes, listing) = match self.prefetch.take().filter(|prefetch| prefetch.path == self.path) {
            // Listed again if it's changed since, rather than shown as it was.
            Some(prefetch) if prefetch.changes.is_pending() => {
                (prefetch.changes, self.list(self.path.clone(), window, cx))
            }
            Some(prefetch) => (prefetch.changes, prefetch.listing),
            None => (watcher(&self.path, cx), self.list(self.path.clone(), window, cx)),
        };
        self.watch(changes, window, cx);
        self.load_from(listing, LISTING_WAIT, window, cx);
    }

    /// Keeps up with the folder through `changes`, from its watcher.
    pub(super) fn watch(&mut self, changes: Changes, window: &mut Window, cx: &mut Context<Self>) {
        self._watch = cx.spawn_in(window, async move |this, cx| {
            while let Some(changed) = changes.next().await {
                let update = this.update_in(cx, |this, window, cx| {
                    this.load(window, cx);
                    // Like a folder given a new icon in Finder, or an app updated.
                    icons::refresh(&changed, cx);
                });
                if update.is_err() {
                    break;
                }
            }
        });
    }

    fn apply_listing(&mut self, (listed, layout): Listed, window: &mut Window, cx: &mut Context<Self>) {
        self.loading = false;
        // Each load draws the picture again, and the GPU keeps the last one until it's let go of.
        if let Some(old) = std::mem::replace(&mut self.layout, layout) {
            cx.drop_image(old.picture, Some(window));
        }
        match listed {
            Ok(entries) => {
                self.listing.entries = entries;
                self.error = None;
                self.blocked = None;
            }
            Err(error) => {
                self.listing.entries.clear();
                // Privacy protection says EPERM, while plain file permissions say EACCES.
                self.blocked =
                    (error.raw_os_error() == Some(libc::EPERM)).then(|| Permissions::global(cx).read(cx).watch());
                self.error = Some(
                    match error.kind() {
                        io::ErrorKind::PermissionDenied => {
                            format!("Mu doesn't have permission to open “{}”.", fs::display_name(&self.path))
                        }
                        io::ErrorKind::NotFound => format!("“{}” doesn't exist.", self.path.display()),
                        _ => format!("Couldn't open “{}”: {error}", fs::display_name(&self.path)),
                    }
                    .into(),
                );
            }
        }
        let after = std::mem::take(&mut self.after_load);
        let select: Vec<SharedString> = after
            .select
            .iter()
            .filter(|path| path.parent() == Some(&self.path))
            .filter_map(|path| path.file_name())
            .map(|name| SharedString::from(name.to_string_lossy().into_owned()))
            .collect();
        let selecting = !select.is_empty();
        if selecting {
            self.selection.set(select);
        }
        // Keeps the selection to what shows.
        self.sort_entries(cx);
        if selecting && let Some(ix) = self.cursor_ix() {
            self.scroll_to(ix, ScrollStrategy::Center, cx);
            if after.rename {
                self.start_rename(window, cx);
            }
        }
        cx.notify();
    }

    fn sort_entries(&mut self, cx: &mut Context<Self>) {
        self.listing.sort();
        self.update_visible(cx);
    }

    /// Works out which items show, and lets go of the selection, the cursor and a rename on those
    /// that don't, so what's selected is what the operations act on.
    pub(super) fn update_visible(&mut self, cx: &mut Context<Self>) {
        let show_hidden = Settings::get(cx).show_hidden;
        let filter = self.filter.to_lowercase();
        let listing = &mut self.listing;
        listing.visible = (0..listing.entries.len())
            .filter(|&ix| show_hidden || !listing.entries[ix].hidden)
            .filter(|&ix| filter.is_empty() || listing.entries[ix].name.to_lowercase().contains(&filter))
            .collect();
        self.selection.keep_to(&self.listing);
        if self.rename.as_ref().is_some_and(|rename| self.listing.position(&rename.name).is_none()) {
            self.rename = None;
        }
        self.spots = match &self.layout {
            Some(layout) => layout.spots(self.listing.iter().map(|entry| entry.name.as_ref())),
            None => Vec::new(),
        };
        cx.notify();
    }

    pub fn set_filter(&mut self, filter: SharedString, cx: &mut Context<Self>) {
        if self.filter == filter {
            return;
        }
        self.filter = filter;
        self.update_visible(cx);
        if let Some(ix) = self.cursor_ix() {
            self.scroll_to(ix, ScrollStrategy::Nearest, cx);
        } else if !self.filter.is_empty() && !self.listing.is_empty() {
            self.select_ix(0, false, cx);
        }
    }

    pub(super) fn set_sort(&mut self, key: SortKey, cx: &mut Context<Self>) {
        let listing = &mut self.listing;
        if listing.sort == key {
            listing.descending = !listing.descending;
        } else {
            listing.sort = key;
            // Newest and biggest first is what you usually want from those columns.
            listing.descending = matches!(key, SortKey::Modified | SortKey::Size);
        }
        // Got ready sorted as it was, so it's got ready again.
        self.prefetch = None;
        self.sort_entries(cx);
    }
}

/// Watches the folder at `path`, a change at a time once its burst settles.
pub(super) fn watcher(path: &Path, cx: &App) -> Changes {
    fs::watch(path, Duration::from_millis(120), cx.background_executor())
}

/// The order items are shown in: folders first, then by `sort`, ties by name.
fn compare(a: &Entry, b: &Entry, sort: SortKey, descending: bool) -> Ordering {
    let by_key = match sort {
        SortKey::Name => fs::natural_cmp(&a.name, &b.name),
        SortKey::Modified => a.modified.cmp(&b.modified),
        SortKey::Size => a.size.cmp(&b.size),
        SortKey::Kind => fs::natural_cmp(&a.kind, &b.kind),
    }
    .then_with(|| fs::natural_cmp(&a.name, &b.name));
    b.folder.cmp(&a.folder).then(if descending { by_key.reverse() } else { by_key })
}

#[cfg(test)]
pub(super) mod tests {
    use std::{
        path::PathBuf,
        time::{Duration, SystemTime},
    };

    use gpui_kit::SharedString;

    use super::{Listing, SortKey, compare};
    use crate::fs::{Category, Entry};

    /// A file, or a folder with `folder`, named `name`.
    pub fn entry(name: &str, folder: bool) -> Entry {
        Entry {
            name: name.to_string().into(),
            path: PathBuf::from("/").join(name),
            alias: None,
            folder,
            package: false,
            is_link: false,
            custom_icon: false,
            locked: false,
            cloud_only: false,
            hidden: name.starts_with('.'),
            size: 0,
            modified: None,
            category: if folder { Category::Folder } else { Category::Other },
            kind: SharedString::default(),
        }
    }

    /// A listing of files named `names` that shows them all, in that order.
    pub fn listing(names: &[&str]) -> Listing {
        let entries: Vec<Entry> = names.iter().map(|name| entry(name, false)).collect();
        let by_name = names.iter().enumerate().map(|(ix, name)| (SharedString::from(*name), ix)).collect();
        Listing { visible: (0..entries.len()).collect(), by_name, entries, ..Listing::default() }
    }

    /// The names of `entries` in the order `sort` puts them in.
    fn sorted(mut entries: Vec<Entry>, sort: SortKey, descending: bool) -> Vec<String> {
        entries.sort_by(|a, b| compare(a, b, sort, descending));
        entries.into_iter().map(|entry| entry.name.to_string()).collect()
    }

    #[test]
    fn sorts_folders_first_then_names_naturally() {
        let entries =
            vec![entry("b10", false), entry("Zed", true), entry("B2", false), entry("apps", true), entry("a", false)];
        assert_eq!(sorted(entries.clone(), SortKey::Name, false), ["apps", "Zed", "a", "B2", "b10"]);
        // Turned around, folders still come first.
        assert_eq!(sorted(entries, SortKey::Name, true), ["Zed", "apps", "b10", "B2", "a"]);
    }

    #[test]
    fn sorts_by_a_column_with_ties_by_name() {
        let sized = |name: &str, size: u64| Entry { size, ..entry(name, false) };
        let entries = vec![sized("c", 1), sized("b", 2), sized("a", 1)];
        assert_eq!(sorted(entries.clone(), SortKey::Size, false), ["a", "c", "b"]);
        // Ties turn around with the rest.
        assert_eq!(sorted(entries, SortKey::Size, true), ["b", "c", "a"]);

        let changed = |name: &str, at: Option<u64>| Entry {
            modified: at.map(|at| SystemTime::UNIX_EPOCH + Duration::from_secs(at)),
            ..entry(name, false)
        };
        let entries = vec![changed("new", Some(20)), changed("unknown", None), changed("old", Some(10))];
        assert_eq!(sorted(entries, SortKey::Modified, false), ["unknown", "old", "new"]);

        let kind = |name: &str, kind: &str| Entry { kind: kind.to_string().into(), ..entry(name, false) };
        let entries = vec![kind("a", "Text 10"), kind("b", "Image"), kind("c", "text 9")];
        assert_eq!(sorted(entries, SortKey::Kind, false), ["b", "c", "a"]);
    }
}
