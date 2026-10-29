//! The Storage tab (⌥⌘S): how full each disk is, then what takes up the room in a folder, biggest
//! first, to dig into a folder at a time.

mod scan;

use std::{
    ffi::OsStr,
    ops::Range,
    path::PathBuf,
    sync::{Arc, atomic::Ordering::Relaxed},
    time::Duration,
};

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    menu::{ContextMenuExt as _, PopupMenu, PopupMenuItem},
    progress::Progress,
    spinner::Spinner,
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::{
    browser::{Redo, Refresh, Undo},
    fs::{self, Volume, format_size, shown_path},
    history::{self, History},
    icons, operations,
    places::Places,
    theme,
    workspace::{TabEvent, actions::GoUp},
};
use scan::{Counted, Kind, Node, add, remove, sort};

pub fn init(cx: &mut App) {
    let context = Some("Storage");
    cx.bind_keys([
        KeyBinding::new("cmd-r", Refresh, context),
        KeyBinding::new("cmd-z", Undo, context),
        KeyBinding::new("cmd-shift-z", Redo, context),
    ]);
}

/// A disk and how full it is.
struct Disk {
    name: SharedString,
    path: PathBuf,
    total: u64,
    available: u64,
}

const ROW_HEIGHT: Pixels = px(40.);

struct Scan {
    counted: Arc<Counted>,
    _task: Task<()>,
}

impl Drop for Scan {
    fn drop(&mut self) {
        self.counted.stopped.store(true, Relaxed);
    }
}

pub struct Storage {
    disks: Vec<Disk>,
    /// Where the scan started.
    root: PathBuf,
    /// What's in `root`.
    nodes: Vec<Node>,
    /// The folders opened inside `root`, each inside the one before.
    at: Vec<Box<OsStr>>,
    scan: Option<Scan>,
    /// How many of the history's undos and redos it has looked at.
    toggles_seen: usize,
    scroll: UniformListScrollHandle,
    focus_handle: FocusHandle,
    _history: Subscription,
}

impl EventEmitter<TabEvent> for Storage {}

impl Storage {
    pub fn new(root: PathBuf, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            disks: Vec::new(),
            root: PathBuf::new(),
            nodes: Vec::new(),
            at: Vec::new(),
            scan: None,
            toggles_seen: history::toggled(cx).len(),
            scroll: UniformListScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            _history: cx.observe_global::<History>(|this, cx| {
                // An undo or redo can bring back what went to the Trash from here, or take it away
                // again.
                let (toggled, changes) = (history::toggled(cx), history::changes(cx));
                let moved_here = toggled[this.toggles_seen..].iter().any(|&id| changes[id].moved_in(&this.root));
                this.toggles_seen = toggled.len();
                if moved_here {
                    this.rescan(cx);
                }
            }),
        };
        this.scan(root, Vec::new(), cx);
        this
    }

    /// Adds up everything in `root` from scratch, handing over each thing in it as it's done, and
    /// checks the disks again. Opens the folder `at` leads to in it, once that's been added up.
    fn scan(&mut self, root: PathBuf, at: Vec<Box<OsStr>>, cx: &mut Context<Self>) {
        self.root = root.clone();
        self.nodes.clear();
        let counted = Arc::new(Counted::default());
        let nodes = scan::start(root, counted.clone());
        // The disks on this Mac, not shares from other computers.
        let volumes = cx.global::<Places>().volumes.iter().filter(|volume| !volume.network).cloned().collect();
        let disks = cx.background_spawn(async { disks(volumes) });
        let task = cx.spawn(async move |this, cx| {
            let disks = disks.await;
            this.update(cx, |this, _| this.disks = disks).ok();
            loop {
                // What's been found is taken in every so often, not as each thing is, or a folder of
                // thousands of files would sort the list and draw it again for every one of them.
                cx.background_executor().timer(Duration::from_millis(150)).await;
                // Whatever arrives before it's closed is still to be taken.
                let done = nodes.is_closed();
                let updated = this.update(cx, |this, cx| {
                    while let Ok(node) = nodes.try_recv() {
                        add(&mut this.nodes, node);
                    }
                    sort(&mut this.nodes);
                    if done {
                        this.scan = None;
                        this.settle();
                    }
                    cx.notify();
                });
                if done || updated.is_err() {
                    break;
                }
            }
        });
        self.scan = Some(Scan { counted, _task: task });
        self.go(at, cx);
    }

    /// Opens `dir`: from what's been added up if it's in there, or with a new scan if not.
    pub fn show(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        match dir.strip_prefix(&self.root) {
            Ok(rest) => self.go(rest.iter().map(Box::from).collect(), cx),
            Err(_) => self.scan(dir, Vec::new(), cx),
        }
    }

    /// The open folder.
    pub fn dir(&self) -> PathBuf {
        self.at.iter().fold(self.root.clone(), |path, name| path.join(&**name))
    }

    fn go(&mut self, at: Vec<Box<OsStr>>, cx: &mut Context<Self>) {
        self.at = at;
        self.settle();
        self.scroll.scroll_to_item(0, ScrollStrategy::Top);
        cx.notify();
    }

    /// Once there's nothing more to add up, ends `at` at the last of its folders that's there. The
    /// rest may be gone since, or never have been listed, like a folder too small to be.
    fn settle(&mut self) {
        if self.scan.is_none() {
            let (_, _, depth) = self.folder();
            self.at.truncate(depth);
        }
    }

    /// What's in the open folder, how big it is, and how many of `at` lead to it, which falls short
    /// while those are still being added up.
    fn folder(&self) -> (&[Node], u64, usize) {
        let mut nodes = &self.nodes[..];
        let mut size = nodes.iter().map(|node| node.size).sum();
        for (depth, name) in self.at.iter().enumerate() {
            match nodes.iter().find(|node| node.name == *name) {
                Some(Node { size: folder_size, kind: Kind::Folder(children), .. }) => {
                    (nodes, size) = (children, *folder_size);
                }
                _ => return (nodes, size, depth),
            }
        }
        (nodes, size, self.at.len())
    }

    fn trash(&mut self, name: Box<OsStr>, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.dir().join(&*name);
        let mut trail = self.at.clone();
        trail.push(name);
        let task = operations::trash(vec![path], window, cx);
        cx.spawn(async move |this, cx| {
            if task.await.is_some_and(|done| done.error.is_none()) {
                this.update(cx, |this, cx| {
                    remove(&mut this.nodes, &trail);
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn render_disks(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        h_flex().flex_none().flex_wrap().gap_3().children(self.disks.iter().enumerate().map(|(ix, disk)| {
            let used = disk.total.saturating_sub(disk.available);
            let path = disk.path.clone();
            theme::card(theme)
                .v_flex()
                .id(("disk", ix))
                .flex_1()
                .min_w(px(200.))
                .p_3()
                .gap_2()
                .cursor_pointer()
                .hover(|style| style.bg(theme.list_hover))
                .child(
                    h_flex()
                        .flex_wrap()
                        .gap_x_3()
                        .text_sm()
                        .child(
                            h_flex()
                                .min_w_0()
                                .gap_2()
                                .child(icons::item(&disk.path, true, px(16.)))
                                .child(div().min_w_0().truncate().font_medium().child(disk.name.clone())),
                        )
                        .child(div().flex_none().ml_auto().text_color(theme.muted_foreground).child(format!(
                            "{} free of {}",
                            format_size(disk.available),
                            format_size(disk.total)
                        ))),
                )
                .child(Progress::new(("disk-usage", ix)).value(used as f32 / disk.total as f32 * 100.))
                .on_click(cx.listener(move |this, _, _, cx| this.scan(path.clone(), Vec::new(), cx)))
        }))
    }

    /// Where the open folder is, from where the scan started, and how big it is.
    fn render_bar(&self, size: u64, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let names =
            std::iter::once(shown_path(&self.root)).chain(self.at.iter().map(|n| n.to_string_lossy().into_owned()));
        let crumbs = names.enumerate().flat_map(|(level, name)| {
            let open = level == self.at.len();
            let separator = (level > 0).then(|| {
                Icon::new(IconName::ChevronRight).xsmall().text_color(theme.muted_foreground).into_any_element()
            });
            let crumb = div()
                .id(("crumb", level))
                .min_w_0()
                .max_w(px(240.))
                .truncate()
                .when(open, |crumb| crumb.font_medium())
                .when(!open, |crumb| {
                    crumb
                        .text_color(theme.muted_foreground)
                        .cursor_pointer()
                        .hover(|style| style.text_color(theme.foreground))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let at = this.at[..level].to_vec();
                            this.go(at, cx)
                        }))
                })
                .child(name);
            separator.into_iter().chain([crumb.into_any_element()])
        });
        let total = match &self.scan {
            Some(scan) => {
                h_flex().gap_2().child(Spinner::new().xsmall()).child(format_size(scan.counted.bytes.load(Relaxed)))
            }
            None => h_flex().child(format_size(size)),
        };
        h_flex()
            .flex_none()
            .flex_wrap()
            .gap_2()
            .px_3()
            .child(h_flex().flex_1().min_w(px(160.)).flex_wrap().gap_1().text_sm().children(crumbs))
            .child(
                h_flex().flex_none().ml_auto().gap_2().text_sm().text_color(theme.muted_foreground).child(total).child(
                    Button::new("rescan")
                        .ghost()
                        .small()
                        .icon(IconName::RefreshCw)
                        .tooltip("Rescan")
                        .on_click(cx.listener(|this, _, _, cx| this.rescan(cx))),
                ),
            )
    }

    /// Scans again where this one started, staying in the open folder.
    fn rescan(&mut self, cx: &mut Context<Self>) {
        self.scan(self.root.clone(), self.at.clone(), cx);
    }

    fn render_rows(&mut self, range: Range<usize>, _: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let (nodes, total, _) = self.folder();
        let dir = self.dir();
        let entity = cx.entity();
        let theme = cx.theme();
        let icon_size = px(18.);
        let range = range.start.min(nodes.len())..range.end.min(nodes.len());
        nodes[range.clone()]
            .iter()
            .zip(range)
            .map(|(node, ix)| {
                let folder = matches!(node.kind, Kind::Folder(_));
                let path = dir.join(&*node.name);
                // The smaller items are in no one place, so their folder is what shows, and they
                // don't go to the Trash.
                let (icon, label, show, trash) = match node.kind {
                    Kind::Smaller(count) => (
                        Icon::new(IconName::Files)
                            .size(icon_size)
                            .text_color(theme.muted_foreground)
                            .into_any_element(),
                        fs::count(count, "smaller item", "smaller items"),
                        TabEvent::ShowFolder(dir.clone()),
                        None,
                    ),
                    _ => (
                        icons::item(&path, folder, icon_size).into_any_element(),
                        node.name.to_string_lossy().into_owned(),
                        TabEvent::Reveal(vec![path]),
                        Some(node.name.clone()),
                    ),
                };
                let menu = {
                    let entity = entity.clone();
                    move |menu: PopupMenu, _: &mut Window, _: &mut Context<PopupMenu>| {
                        let (this, show) = (entity.clone(), show.clone());
                        let menu = menu.item(
                            PopupMenuItem::new("Show in Files")
                                .on_click(move |_, _, cx| this.update(cx, |_, cx| cx.emit(show.clone()))),
                        );
                        let Some(name) = trash.clone() else { return menu };
                        let this = entity.clone();
                        menu.separator().item(PopupMenuItem::new("Move to Trash").on_click(move |_, window, cx| {
                            this.update(cx, |this, cx| this.trash(name.clone(), window, cx))
                        }))
                    }
                };
                let name = node.name.clone();
                h_flex()
                    .id(("item", ix))
                    .w_full()
                    .h(ROW_HEIGHT)
                    .px_3()
                    .gap_3()
                    .rounded(theme.radius)
                    .hover(|style| style.bg(theme.list_hover))
                    .child(div().flex_none().child(icon))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                h_flex()
                                    .gap_3()
                                    .text_sm()
                                    .child(div().flex_1().min_w_0().truncate().text_ellipsis_middle().child(label))
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_color(theme.muted_foreground)
                                            .child(format_size(node.size)),
                                    ),
                            )
                            .child(
                                Progress::new(("share", ix))
                                    .xsmall()
                                    .value(node.size as f32 / total.max(1) as f32 * 100.),
                            ),
                    )
                    .child(div().flex_none().size(px(14.)).when(folder, |el| {
                        el.child(Icon::new(IconName::ChevronRight).xsmall().text_color(theme.muted_foreground))
                    }))
                    .when(folder, |row| {
                        row.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| {
                            let mut at = this.at.clone();
                            at.push(name.clone());
                            this.go(at, cx)
                        }))
                    })
                    .context_menu(menu)
                    .into_any_element()
            })
            .collect()
    }
}

impl Focusable for Storage {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Storage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (nodes, size, depth) = self.folder();
        let (count, waiting) = (nodes.len(), depth < self.at.len() || nodes.is_empty() && self.scan.is_some());
        let theme = cx.theme();
        let message = |el: Div| el.size_full().flex().items_center().justify_center();
        v_flex()
            .id("storage")
            .key_context("Storage")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &Refresh, _, cx| this.rescan(cx)))
            .on_action(|_: &Undo, window, cx| history::step(false, window, cx).detach())
            .on_action(|_: &Redo, window, cx| history::step(true, window, cx).detach())
            .on_action(cx.listener(|this, _: &GoUp, _, cx| {
                if let Some((_, at)) = this.at.split_last() {
                    let at = at.to_vec();
                    this.go(at, cx);
                }
            }))
            .size_full()
            .p_4()
            .gap_4()
            .bg(theme.background)
            .child(self.render_disks(cx))
            .child(self.render_bar(size, cx))
            .child(div().flex_1().min_h_0().map(|list| {
                match (waiting, count) {
                    (true, _) => list.child(message(div()).child(Spinner::new())),
                    (false, 0) => list.child(
                        message(div()).text_sm().text_color(cx.theme().muted_foreground).child("This folder is empty"),
                    ),
                    (false, _) => list.child(
                        uniform_list("storage-items", count, cx.processor(Self::render_rows))
                            .size_full()
                            .track_scroll(&self.scroll),
                    ),
                }
            }))
    }
}

/// How full each of `volumes` is, leaving out any that says it holds nothing.
fn disks(volumes: Vec<Volume>) -> Vec<Disk> {
    volumes
        .into_iter()
        .filter_map(|volume| {
            let (total, available) = fs::capacity(&volume.path).filter(|(total, _)| *total > 0)?;
            Some(Disk { name: volume.name, path: volume.path, total, available })
        })
        .collect()
}
