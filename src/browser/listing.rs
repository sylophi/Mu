//! Loading the folder and keeping up with it: its items, their order, and which of them show.

use std::{
    cmp::Ordering,
    collections::HashMap,
    io,
    ops::{Index, Range},
    os::unix::ffi::OsStrExt as _,
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
    permissions::{Permission, Permissions},
    places::Places,
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

/// A folder's items, sorted, with those of the folders expanded in it and the rooms in which they'd
/// be the same, and how Finder lays it out, if it does.
type Listed = (io::Result<(Vec<Item>, Range<usize>)>, Option<Layout>);

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

/// An item in the listing: one of the folder's, or one in a folder expanded under it.
pub(super) struct Item {
    pub(super) entry: Entry,
    /// Its path from the folder, which tells it apart from items of the same name in other folders.
    pub(super) key: SharedString,
    /// How many expanded folders it's in.
    pub(super) depth: usize,
    /// A folder whose items show under it.
    expanded: bool,
}

/// What's in the folder, in order, and which of it shows.
#[derive(Default)]
pub(super) struct Listing {
    /// Each expanded folder's items follow it.
    items: Vec<Item>,
    /// Indices into `items`, without hidden files and filtered by name.
    visible: Vec<usize>,
    /// Where each item is in `items`, by key.
    by_key: HashMap<SharedString, usize>,
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
    pub(super) fn iter(&self) -> impl DoubleEndedIterator<Item = &Item> {
        self.visible.iter().map(|&ix| &self.items[ix])
    }

    /// The key of the `ix`th item that shows.
    pub(super) fn key(&self, ix: usize) -> &SharedString {
        &self.items[self.visible[ix]].key
    }

    /// How many expanded folders the `ix`th item that shows is in.
    pub(super) fn depth(&self, ix: usize) -> usize {
        self.items[self.visible[ix]].depth
    }

    /// Where the item with `key` shows, if it does.
    pub(super) fn position(&self, key: &str) -> Option<usize> {
        // `visible` keeps to the order of `items`.
        self.visible.binary_search(self.by_key.get(key)?).ok()
    }

    /// The folders expanded in the listing.
    fn expanded(&self) -> Vec<PathBuf> {
        self.items.iter().filter(|item| item.expanded).map(|item| item.entry.path.clone()).collect()
    }

    /// Puts the items in the order they show in.
    fn sort(&mut self) {
        self.items = sort_tree(std::mem::take(&mut self.items), self.sort, self.descending);
        self.by_key = self.items.iter().enumerate().map(|(ix, item)| (item.key.clone(), ix)).collect();
    }
}

/// The `ix`th item that shows.
impl Index<usize> for Listing {
    type Output = Entry;

    fn index(&self, ix: usize) -> &Entry {
        &self.items[self.visible[ix]].entry
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

    /// Lists `path` in the background, sorted as this listing is, with the folders in it that fit
    /// in the list's room expanded.
    fn list(&self, path: PathBuf, window: &Window, cx: &App) -> Task<Listed> {
        let (sort, descending, scale) = (self.listing.sort, self.listing.descending, window.scale_factor());
        let (room, show_hidden) = (self.room(cx), Settings::get(cx).show_hidden);
        let dirs = shown_dirs(&path, cx);
        cx.background_spawn(async move {
            let layout = Layout::read(&path, scale);
            // Sorted here, so sorting it again on the main thread only checks the order.
            let listing = fs::read_dirs(&dirs, fs::list_dir).map(|entries| match layout {
                // A folder Finder lays out shows as Finder has it, whatever the room.
                Some(_) => (expand(entries, &path, 0, show_hidden, sort, descending).0, 0..usize::MAX),
                None => expand(entries, &path, room, show_hidden, sort, descending),
            });
            (listing, layout)
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
        let changes = watcher(shown_dirs(&path, cx), cx);
        let settings = Settings::get(cx);
        let (show_hidden, first) = (settings.show_hidden, self.screenful(settings.grid));
        let (sender, receiver) = smol::channel::bounded(1);
        let listing = self.list(path.clone(), window, cx);
        let listing = cx.background_spawn(async move {
            let listed = listing.await;
            // What it shows first, unless Finder lays it out.
            if let (Ok((items, _)), None) = &listed {
                let shown = items.iter().map(|item| &item.entry).filter(|entry| show_hidden || !entry.hidden);
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
            None => (watcher(shown_dirs(&self.path, cx), cx), self.list(self.path.clone(), window, cx)),
        };
        self.watch(changes, window, cx);
        self.load_from(listing, LISTING_WAIT, window, cx);
    }

    /// Keeps up with the folder through `changes`, from its watcher.
    pub(super) fn watch(&mut self, changes: Changes, window: &mut Window, cx: &mut Context<Self>) {
        self._watch = Self::keep_up(changes, window, cx);
    }

    /// Keeps up with the folders expanded in the listing too, whose changes count as its own.
    pub(super) fn watch_expanded(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let expanded = self.listing.expanded();
        self._watch_expanded = match expanded.is_empty() {
            true => Task::ready(()),
            false => Self::keep_up(watcher(expanded, cx), window, cx),
        };
    }

    /// Loads the folder again whenever `changes` has one, until the task is dropped.
    fn keep_up(changes: Changes, window: &mut Window, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn_in(window, async move |this, cx| {
            while let Some(changed) = changes.next().await {
                let update = this.update_in(cx, |this, window, cx| {
                    if changed.items {
                        this.load(window, cx);
                    }
                    // Like a folder given a new icon in Finder, or an app updated.
                    icons::refresh(&changed.paths, cx);
                });
                if update.is_err() {
                    break;
                }
            }
        })
    }

    fn apply_listing(&mut self, (listed, layout): Listed, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(feature = "bench")]
        crate::bench::landed();
        self.loading = false;
        // Each load draws the picture again, and the GPU keeps the last one until it's let go of.
        if let Some(old) = std::mem::replace(&mut self.layout, layout) {
            cx.drop_image(old.picture, Some(window));
        }
        let expanded = self.listing.expanded();
        match listed {
            Ok((items, fits)) => {
                self.listing.items = items;
                self.fits = fits;
                self.error = None;
                self.blocked = None;
            }
            Err(error) => {
                self.listing.items.clear();
                self.fits = 0..usize::MAX;
                // Privacy protection says EPERM, while plain file permissions say EACCES.
                self.blocked = (error.raw_os_error() == Some(libc::EPERM))
                    .then(|| (Permission::for_path(&self.path), Permissions::global(cx).read(cx).watch()));
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
        if self.listing.expanded() != expanded {
            self.watch_expanded(window, cx);
        }
        let after = std::mem::take(&mut self.after_load);
        let select: Vec<SharedString> = after
            .select
            .iter()
            .filter(|path| **path != self.path)
            .map(|path| path.strip_prefix(&self.path).unwrap_or(path))
            .map(|key| SharedString::from(key.to_string_lossy().into_owned()))
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
        // What's in a folder that doesn't show doesn't either. The filter goes by the folder's own
        // items, and what's in those that match shows with them.
        let mut hidden_below = usize::MAX;
        listing.visible = (0..listing.items.len())
            .filter(|&ix| {
                let Item { entry, depth, .. } = &listing.items[ix];
                if *depth > hidden_below {
                    return false;
                }
                let shows = (show_hidden || !entry.hidden)
                    && (*depth > 0 || filter.is_empty() || entry.name.to_lowercase().contains(&filter));
                hidden_below = if shows { usize::MAX } else { *depth };
                shows
            })
            .collect();
        self.selection.keep_to(&self.listing);
        if self.rename.as_ref().is_some_and(|rename| self.listing.position(&rename.key).is_none()) {
            self.rename = None;
        }
        self.spots = match &self.layout {
            Some(layout) => layout.spots(self.listing.iter().map(|item| item.entry.name.as_ref())),
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

/// Watches the folders at `paths`, a change at a time once its burst settles.
pub(super) fn watcher(paths: Vec<PathBuf>, cx: &App) -> Changes {
    fs::watch(paths, true, Duration::from_millis(120), cx.background_executor())
}

/// The folders whose items show at `path`: each disk's Trash in the Trash, or else just it.
pub(super) fn shown_dirs(path: &Path, cx: &App) -> Vec<PathBuf> {
    match *path == fs::trash_dir() {
        true => cx.global::<Places>().trash_dirs.clone(),
        false => vec![path.to_path_buf()],
    }
}

/// The `entries` of `folder`, sorted, with the items of the folders in them under them: a level at a
/// time, each folder whose items fit in the `room` left, so as much as fits shows without a click.
/// Links, aliases, other disks and what's only in iCloud stay as they are, as reading them can take
/// a while or download them, like empty folders, which have nothing to show. Also gives
/// the rooms it comes out the same in, so it's only done again once the room leaves them.
fn expand(
    mut entries: Vec<Entry>,
    folder: &Path,
    room: usize,
    show_hidden: bool,
    sort: SortKey,
    descending: bool,
) -> (Vec<Item>, Range<usize>) {
    // Sorted first, so folders expand in the order they show in.
    let sort = |entries: &mut Vec<Entry>| entries.sort_by(|a, b| compare(a, b, sort, descending));
    sort(&mut entries);
    let shown = |entries: &[Entry]| entries.iter().filter(|entry| show_hidden || !entry.hidden).count();
    let openable = |entries: &[Entry]| -> Vec<PathBuf> {
        entries
            .iter()
            .filter(|e| e.folder && !e.is_link && !e.mounted && !e.cloud_only && (show_hidden || !e.hidden))
            .map(|e| e.path.clone())
            .collect()
    };
    // The rows taken so far, and the least room another folder would fit in.
    let (mut used, mut more) = (shown(&entries), usize::MAX);
    let mut inside = HashMap::new();
    let mut level = openable(&entries);
    while !level.is_empty() {
        let mut next = Vec::new();
        for dir in level {
            let left = room.saturating_sub(used);
            if left == 0 {
                // What's in the rest isn't known, but it takes a row at least.
                more = more.min(used + 1);
                break;
            }
            // Its names counted first, a few at most, so a big folder isn't read in full to find
            // it doesn't fit.
            let names = std::fs::read_dir(&dir).map(|names| {
                let names = names.filter_map(Result::ok);
                names
                    .filter(|name| show_hidden || !name.file_name().as_bytes().starts_with(b"."))
                    .take(left + 1)
                    .count()
            });
            if let Ok(names) = names
                && names > left
            {
                more = more.min(used + names);
                continue;
            }
            let Ok(mut entries) = fs::list_dir(&dir) else { continue };
            let count = shown(&entries);
            if count == 0 {
                continue;
            }
            if count > left {
                more = more.min(used + count);
                continue;
            }
            used += count;
            sort(&mut entries);
            next.extend(openable(&entries));
            inside.insert(dir, entries);
        }
        level = next;
    }
    // With less room than it takes, a folder in it closes.
    let fewer = if inside.is_empty() { 0 } else { used };
    let mut items = Vec::new();
    flatten(entries, folder, None, 0, &mut inside, &mut items);
    (items, fewer..more)
}

/// Adds `entries`, in folder `parent` (by key) `depth` folders into `folder`, to `items`, each
/// followed by what's `inside` it.
fn flatten(
    entries: Vec<Entry>,
    folder: &Path,
    parent: Option<&str>,
    depth: usize,
    inside: &mut HashMap<PathBuf, Vec<Entry>>,
    items: &mut Vec<Item>,
) {
    for entry in entries {
        let key: SharedString = match parent {
            Some(parent) => format!("{parent}/{}", entry.name).into(),
            None if entry.path.parent() == Some(folder) => entry.name.clone(),
            // What's elsewhere, like another disk's Trash in the Trash, goes by its whole path.
            None => entry.path.to_string_lossy().into_owned().into(),
        };
        let children = inside.remove(&entry.path);
        items.push(Item { entry, key: key.clone(), depth, expanded: children.is_some() });
        if let Some(children) = children {
            flatten(children, folder, Some(&key), depth + 1, inside, items);
        }
    }
}

/// `items` in order, each expanded folder's items sorted among themselves after it.
fn sort_tree(mut items: Vec<Item>, sort: SortKey, descending: bool) -> Vec<Item> {
    let Some(depth) = items.first().map(|item| item.depth) else { return items };
    if items.iter().all(|item| item.depth == depth) {
        items.sort_by(|a, b| compare(&a.entry, &b.entry, sort, descending));
        return items;
    }
    // Each item with what's under it.
    let mut groups: Vec<Vec<Item>> = Vec::new();
    for item in items {
        match groups.last_mut() {
            Some(group) if item.depth > depth => group.push(item),
            _ => groups.push(vec![item]),
        }
    }
    groups.sort_by(|a, b| compare(&a[0].entry, &b[0].entry, sort, descending));
    groups
        .into_iter()
        .flat_map(|mut group| {
            let inside = group.split_off(1);
            group.extend(sort_tree(inside, sort, descending));
            group
        })
        .collect()
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

    use super::{Item, Listing, SortKey, compare, expand, sort_tree};
    use crate::fs::{self, Category, Entry, Scratch};

    /// A file, or a folder with `folder`, named `name`.
    pub fn entry(name: &str, folder: bool) -> Entry {
        Entry {
            name: name.to_string().into(),
            path: PathBuf::from("/").join(name),
            alias: None,
            folder,
            package: false,
            is_link: false,
            mounted: false,
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
        let items: Vec<Item> = names
            .iter()
            .map(|name| Item { entry: entry(name, false), key: (*name).into(), depth: 0, expanded: false })
            .collect();
        let by_key = names.iter().enumerate().map(|(ix, name)| (SharedString::from(*name), ix)).collect();
        Listing { visible: (0..items.len()).collect(), by_key, items, ..Listing::default() }
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

    /// The keys of `items` that aren't hidden, each indented by its depth.
    fn keys(items: &[Item]) -> Vec<String> {
        let shown = items.iter().filter(|item| !item.entry.hidden);
        shown.map(|item| format!("{}{}", "  ".repeat(item.depth), item.key)).collect()
    }

    #[test]
    fn expands_folders_a_level_at_a_time_while_they_fit() {
        let scratch = Scratch::new("expand");
        let dir = &scratch.0;
        for path in ["a/x", "a/y", "b/c", "big/1", "big/2", "big/3", "empty", "b/c/z"] {
            std::fs::create_dir_all(dir.join(path)).unwrap();
        }
        std::fs::write(dir.join("a/.hidden"), "").unwrap();
        std::fs::write(dir.join("file"), "").unwrap();
        let expand = |room| expand(fs::list_dir(dir).unwrap(), dir, room, false, SortKey::Name, false).0;
        // Five rows, then a's two (its hidden file doesn't count), and b's one. big's three don't fit
        // in the two left, and empty has nothing to show. c's goes in the next level.
        let items = expand(9);
        assert_eq!(keys(&items), ["a", "  a/x", "  a/y", "b", "  b/c", "    b/c/z", "big", "empty", "file"]);
        // Too little room expands nothing.
        assert_eq!(keys(&expand(5)), ["a", "b", "big", "empty", "file"]);
        // It comes out the same until c closes, a row less, or what's in z might fit, a row more.
        // With nothing expanded, it does until something might fit.
        let fits = |room| super::expand(fs::list_dir(dir).unwrap(), dir, room, false, SortKey::Name, false).1;
        assert_eq!(fits(9), 9..10);
        assert_eq!(fits(5), 0..6);
        // Sorted the other way, what's in each folder stays under it.
        assert_eq!(
            keys(&sort_tree(items, SortKey::Name, true)),
            ["empty", "big", "b", "  b/c", "    b/c/z", "a", "  a/y", "  a/x", "file"]
        );
    }

    #[test]
    fn tells_apart_items_of_one_name_from_another_folder() {
        // Like the Trash, which shows each disk's.
        let scratch = Scratch::new("elsewhere");
        let (home, other) = (scratch.0.join("home"), scratch.0.join("other"));
        for dir in [&home, &other] {
            std::fs::create_dir_all(dir.join("a")).unwrap();
            std::fs::write(dir.join("a/x"), "").unwrap();
        }
        let entries = fs::read_dirs(&[home.clone(), other.clone()], fs::list_dir).unwrap();
        let elsewhere = other.join("a").display().to_string();
        assert_eq!(
            keys(&expand(entries, &home, 9, false, SortKey::Name, false).0),
            ["a", "  a/x", &elsewhere, &format!("  {elsewhere}/x")]
        );
    }
}
