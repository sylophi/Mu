//! The sidebar: the folders added to it and their groups, iCloud Drive, disks, the Trash and servers on the network, then the shelf.

use std::{
    cell::Cell,
    collections::HashMap,
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, StyledExt as _, Theme,
    button::{Button, ButtonVariants as _},
    h_flex,
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt as _, PopupMenuItem},
    notification::Notification,
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use super::{
    Item, Workspace,
    actions::ConnectToServer,
    parts::{action_button, close_button, menu_item, opens_folder, row, truncated},
    windows::open_window,
};
use crate::{
    activity::monitor,
    browser::CancelRename,
    drag::{self, DragFiles as _, DraggedFiles, DropFiles as _, drag_preview},
    fs::{self, home_dir},
    icons,
    network::{self, Servers},
    places::Places,
    session::Tab,
    settings::{Settings, SidebarGroup},
    shelf,
    theme::Toasts as _,
    underway::{self, Underway},
};

/// The sidebar, drawn again when the workspace changes, rather than on every frame the folder view
/// draws.
pub(super) struct Sidebar {
    workspace: WeakEntity<Workspace>,
    _observe: Subscription,
}

impl Sidebar {
    pub(super) fn new(workspace: &Entity<Workspace>, cx: &mut Context<Self>) -> Self {
        Self { workspace: workspace.downgrade(), _observe: cx.observe(workspace, |_, _, cx| cx.notify()) }
    }
}

impl Render for Sidebar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.workspace
            .update(cx, |workspace, cx| workspace.render_sidebar(cx).into_any_element())
            .unwrap_or_else(|_| Empty.into_any_element())
    }
}

/// A spot among the items added to the sidebar: before the one at `ix` in `group`, or outside any
/// group.
#[derive(Clone, Copy, PartialEq)]
pub(super) struct Slot {
    group: Option<usize>,
    ix: usize,
}

/// What a sidebar entry is.
#[derive(Clone, Copy, PartialEq)]
enum Place {
    /// An item added to the sidebar.
    Added(Slot),
    ICloudDrive,
    Volume,
    Trash,
}

type Entry = (SharedString, PathBuf, Place);

/// What's in each of the sidebar's sections.
struct Sections {
    items: Vec<Entry>,
    /// What each group shows, which is nothing while it's collapsed.
    groups: Vec<Vec<Entry>>,
    icloud: Vec<Entry>,
    locations: Vec<Entry>,
}

impl Sections {
    /// Every entry, from the top.
    fn all(&self) -> impl Iterator<Item = &Entry> {
        self.items.iter().chain(self.groups.iter().flatten()).chain(&self.icloud).chain(&self.locations)
    }
}

/// An item added to the sidebar being dragged, to another spot in it or out of it to remove it.
#[derive(Clone)]
pub(super) struct DraggedSidebarItem {
    pub(super) path: PathBuf,
    name: SharedString,
    /// Whether it's out of the sidebar, where letting go removes it.
    removing: Rc<Cell<bool>>,
    grabbed_at: Point<Pixels>,
}

impl Render for DraggedSidebarItem {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let icon = match self.removing.get() {
            true => Icon::new(IconName::CircleX).size(PLACE_ICON).text_color(cx.theme().danger).into_any_element(),
            false => icons::item(&self.path, true, PLACE_ICON).into_any_element(),
        };
        drag_preview(self.grabbed_at, icon, self.name.clone(), cx)
    }
}

/// A group being dragged to another spot among the groups.
#[derive(Clone)]
struct DraggedGroup {
    ix: usize,
    name: SharedString,
    grabbed_at: Point<Pixels>,
}

impl Render for DraggedGroup {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let icon = Icon::new(IconName::ChevronDown).xsmall().text_color(cx.theme().muted_foreground);
        drag_preview(self.grabbed_at, icon, self.name.clone(), cx)
    }
}

/// Something that can be dragged in among the items added to the sidebar.
trait SidebarDrag: 'static {
    /// How much of an item's row, from its top and from its bottom, drops this in between items
    /// instead of on the one there.
    fn edge(&self) -> f32;
}

impl SidebarDrag for DraggedSidebarItem {
    fn edge(&self) -> f32 {
        0.5
    }
}

impl SidebarDrag for DraggedFiles {
    fn edge(&self) -> f32 {
        folders_edge(&self.paths)
    }
}

impl SidebarDrag for ExternalPaths {
    fn edge(&self) -> f32 {
        folders_edge(self.paths())
    }
}

/// Only folders can be added to the sidebar.
fn folders_edge(paths: &[PathBuf]) -> f32 {
    if paths.iter().all(|path| path.is_dir()) { 0.25 } else { 0. }
}

/// What a drag among the added items is over.
#[derive(Clone, Copy)]
enum DropArea {
    Row(Slot),
    /// A group's heading, or the space kept for items outside any group while there are none.
    Top(Option<usize>),
    /// Everything added to the sidebar, leaving which clears the drop.
    Added,
}

/// A sidebar group's name while it's being edited.
pub(super) struct GroupRename {
    ix: usize,
    input: Entity<InputState>,
    _subscription: Subscription,
}

/// The size of the icons by places in the sidebar.
const PLACE_ICON: Pixels = px(16.);

/// The heading of a group, or the Network or the Shelf section, with a button at its end.
fn heading(title: impl Into<SharedString>, button: Option<Button>, theme: &Theme) -> Div {
    h_flex()
        .h(px(20.))
        .pl_2()
        .mb_1()
        .justify_between()
        .text_xs()
        .font_medium()
        .text_color(theme.muted_foreground)
        .child(truncated(title))
        .children(button)
}

/// The line showing that a drag would go above the entry at `ix`, or below it if it's the last of
/// `len`, `gap` outside the entry.
fn drop_line(drop: usize, ix: usize, len: usize, gap: Pixels, theme: &Theme) -> Option<Div> {
    let line = div().absolute().left_1().right_1().h(px(2.)).bg(theme.foreground);
    if drop == ix {
        Some(line.top(-gap))
    } else if drop == ix + 1 && drop == len {
        Some(line.bottom(-gap))
    } else {
        None
    }
}

/// How far what's underway in each place has got, all together, while that's known. What's
/// underway shows on the innermost place it's in, like Downloads rather than the disk it's on.
fn underway_by_place(sections: &Sections, cx: &App) -> HashMap<PathBuf, Option<f64>> {
    let mut bytes: HashMap<&Path, Option<(u64, u64)>> = HashMap::new();
    for transfer in Underway::global(cx).read(cx).all() {
        let place = sections
            .all()
            .filter(|(_, path, place)| *place != Place::Trash && transfer.path.starts_with(path))
            .max_by_key(|(_, path, _)| path.components().count());
        let Some((_, path, _)) = place else { continue };
        let sum = bytes.entry(path).or_insert(Some((0, 0)));
        *sum = sum.zip(transfer.bytes()).map(|((a, b), (c, d))| (a + c, b + d));
    }
    bytes
        .into_iter()
        .map(|(path, bytes)| (path.to_path_buf(), bytes.map(|(done, total)| done as f64 / total as f64)))
        .collect()
}

/// Asks, then deletes everything in the Trash for good.
pub(super) fn empty_trash(window: &mut Window, cx: &mut App) {
    let count = match fs::trash_items() {
        Ok(items) => items.len(),
        Err(error) => {
            let message = format!("Couldn't open the Trash: {error}");
            return window.toast(Notification::error(message), cx);
        }
    };
    if count == 0 {
        return;
    }
    let answer = window.prompt(
        PromptLevel::Warning,
        &format!("Delete the {} in the Trash for good?", fs::count(count, "item", "items")),
        Some("You can't undo this."),
        &["Empty Trash", "Cancel"],
        cx,
    );
    window
        .spawn(cx, async move |cx| {
            if answer.await != Ok(0) {
                return;
            }
            if let Err(error) = cx.background_executor().spawn(async { fs::admin::session(fs::empty_trash) }).await {
                cx.update(|window, cx| window.toast_error(&error, cx)).ok();
            }
        })
        .detach();
}

impl Workspace {
    fn eject(&mut self, volume: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let task = cx.background_spawn({
            let volume = volume.clone();
            async move { fs::eject(&volume) }
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(()) => {
                    for item in this.items.iter() {
                        if let Item::Folder(tab) = item
                            && tab.read(cx).path().starts_with(&volume)
                        {
                            tab.update(cx, |tab, cx| tab.navigate(home_dir(), window, cx));
                        }
                    }
                }
                Err(error) => match error.downcast_ref::<fs::InUse>() {
                    Some(fs::InUse(holders)) => {
                        let holders = holders.clone();
                        let workspace = cx.weak_entity();
                        let note = Notification::error(format!("{error:#}")).action(move |_, _, cx| {
                            let (workspace, holders, volume) = (workspace.clone(), holders.clone(), volume.clone());
                            Button::new("quit-and-eject").label("Quit and Eject").on_click(cx.listener(
                                move |note, _, window, cx| {
                                    note.dismiss(window, cx);
                                    workspace
                                        .update(cx, |this, cx| {
                                            this.quit_and_eject(holders.clone(), volume.clone(), window, cx)
                                        })
                                        .ok();
                                },
                            ))
                        });
                        window.toast(note, cx);
                    }
                    None => window.toast_error(&error, cx),
                },
            })
            .ok();
        })
        .detach();
    }

    /// Quits `holders` and ejects `volume` once they've gone or had a few seconds to.
    fn quit_and_eject(
        &mut self,
        holders: Vec<fs::Holder>,
        volume: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(error) = monitor::quit(&holders, libc::SIGHUP) {
            window.toast_error(&error, cx);
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let running = |holder: &fs::Holder| monitor::signal(holder.pid, 0).is_ok();
            for _ in 0..50 {
                if !holders.iter().any(running) {
                    break;
                }
                cx.background_executor().timer(Duration::from_millis(100)).await;
            }
            this.update_in(cx, |this, window, cx| this.eject(volume, window, cx)).ok();
        })
        .detach();
    }

    /// What's in the sidebar's sections. There's iCloud while iCloud Drive is on.
    fn sections(&self, cx: &App) -> Sections {
        let settings = Settings::get(cx);
        let entries = |group: Option<usize>, items: &[PathBuf]| -> Vec<Entry> {
            items
                .iter()
                .enumerate()
                .map(|(ix, path)| {
                    (SharedString::from(fs::display_name(path)), path.clone(), Place::Added(Slot { group, ix }))
                })
                .collect()
        };
        let groups = settings
            .sidebar_groups
            .iter()
            .enumerate()
            .map(|(ix, group)| if group.collapsed { Vec::new() } else { entries(Some(ix), &group.items) })
            .collect();
        let icloud = fs::icloud_drive();
        let icloud = if icloud.is_dir() { vec![("iCloud Drive".into(), icloud, Place::ICloudDrive)] } else { vec![] };
        let volumes = &cx.global::<Places>().volumes;
        let mut locations: Vec<_> =
            volumes.iter().map(|volume| (volume.name.clone(), volume.path.clone(), Place::Volume)).collect();
        locations.push(("Trash".into(), fs::trash_dir(), Place::Trash));
        Sections { items: entries(None, &settings.sidebar_items), groups, icloud, locations }
    }

    /// Shows the folder at this position in the sidebar, counting from the top.
    pub(super) fn show_place(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((_, path, _)) = self.sections(cx).all().nth(ix).cloned() {
            self.show_folder(path, window, cx);
        }
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.last_folder.as_ref().map(|browser| browser.read(cx).path().to_path_buf());
        let sections = self.sections(cx);
        let underway = underway_by_place(&sections, cx);
        let Sections { items, groups, icloud, locations } = sections;
        let rows = |items: Vec<Entry>, cx: &mut Context<Self>| -> Vec<AnyElement> {
            items
                .into_iter()
                .map(|(name, path, place)| {
                    let active = current.as_ref() == Some(&path);
                    let underway = underway.get(&path).copied();
                    self.render_place(name, path, place, active, underway, cx)
                })
                .collect()
        };
        let section = |title: &'static str, items: Vec<Entry>, cx: &mut Context<Self>| {
            let rows = rows(items, cx);
            v_flex()
                .gap_px()
                .child(div().px_2().pb_1().text_xs().font_medium().text_color(cx.theme().muted_foreground).child(title))
                .children(rows)
        };
        let items = match items.is_empty() {
            true => self.render_no_items(cx).into_any_element(),
            false => v_flex().gap_px().children(rows(items, cx)).into_any_element(),
        };
        let groups: Vec<_> = groups
            .into_iter()
            .enumerate()
            .map(|(ix, items)| {
                let rows = rows(items, cx);
                self.render_group(ix, rows, cx)
            })
            .collect();
        let added = v_flex().gap_4().child(items).children(groups);
        let added = Self::track_group_drop(Self::track_item_drop(added, DropArea::Added, cx), None, cx);
        // Its own element, since a menu on the whole sidebar would open along with a row's.
        let workspace = cx.weak_entity();
        let blank = div().id("sidebar-blank").flex_1().min_h(px(40.)).context_menu(move |menu, _, _| {
            menu.item(menu_item("New Group", &workspace, |this, window, cx| this.new_group(Vec::new(), window, cx)))
        });
        let places = v_flex()
            .id("sidebar")
            .flex_1()
            .min_h_0()
            .p_2()
            .gap_4()
            .overflow_y_scroll()
            .on_drag_move(|event: &DragMoveEvent<DraggedSidebarItem>, window, cx| {
                let removing = !event.bounds.contains(&event.event.position);
                if event.drag(cx).removing.replace(removing) != removing {
                    window.refresh();
                }
            })
            .on_drop(cx.listener(|this, item: &DraggedSidebarItem, _, cx| {
                if let Some(Slot { group, ix }) = this.sidebar_drop.take() {
                    Settings::update(cx, |settings| {
                        settings.place_in_sidebar(group, ix, std::slice::from_ref(&item.path))
                    });
                }
            }))
            .on_drop(cx.listener(|this, group: &DraggedGroup, _, cx| {
                if let Some(to) = this.group_drop.take() {
                    Settings::update(cx, |settings| settings.move_sidebar_group(group.ix, to));
                }
            }))
            .on_drop_files(cx.listener(|this, paths: &[PathBuf], _, cx| {
                if let Some(Slot { group, ix }) = this.sidebar_drop.take() {
                    Settings::update(cx, |settings| settings.place_in_sidebar(group, ix, paths));
                }
            }))
            .child(added)
            .when(!icloud.is_empty(), |el| el.child(section("iCloud", icloud, cx)))
            .child(section("Locations", locations, cx))
            .children(self.render_network(cx))
            .child(blank);
        v_flex()
            .size_full()
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .child(places)
            .children(self.render_shelf(cx))
    }

    /// The space to drag items to, while none are outside a group.
    fn render_no_items(&self, cx: &Context<Self>) -> Div {
        let theme = cx.theme();
        let dropping = cx.has_active_drag() && self.sidebar_drop == Some(Slot { group: None, ix: 0 });
        let space = div().h(px(26.)).rounded(theme.radius).when(dropping, |el| el.bg(theme.drop_target));
        Self::track_item_drop(space, DropArea::Top(None), cx)
    }

    /// The group at `ix`: a heading that collapses it, which drags to move it, and its `rows`.
    fn render_group(&self, ix: usize, rows: Vec<AnyElement>, cx: &Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let groups = &Settings::get(cx).sidebar_groups;
        let group = &groups[ix];
        let collapsed = group.collapsed;
        let name = SharedString::from(group.name.clone());
        let renaming = self.group_rename.as_ref().filter(|rename| rename.ix == ix);
        let dragging = cx.has_active_drag();
        let dropping_in = rows.is_empty() && dragging && self.sidebar_drop == Some(Slot { group: Some(ix), ix: 0 });
        // Where a dragged group would go: above this one, or below the last.
        let line =
            self.group_drop.filter(|_| dragging).and_then(|drop| drop_line(drop, ix, groups.len(), px(9.), theme));
        let chevron = if collapsed { IconName::ChevronRight } else { IconName::ChevronDown };
        let dragged = DraggedGroup { ix, name: name.clone(), grabbed_at: Point::default() };
        let workspace = cx.weak_entity();
        let heading = match renaming {
            Some(rename) => heading("", None, theme).child(
                div()
                    .flex_1()
                    .key_context("Rename")
                    .on_action(cx.listener(|this, _: &CancelRename, window, cx| this.cancel_group_rename(window, cx)))
                    // Not everything in the sidebar takes focus, so a click there wouldn't blur the field.
                    .on_mouse_down_out(cx.listener(|this, _, window, cx| this.commit_group_rename(window, cx)))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(Input::new(&rename.input).xsmall()),
            ),
            None => heading(name, None, theme).child(
                div()
                    .flex_none()
                    .pr_1()
                    .when(!collapsed, |el| el.invisible().group_hover("sidebar-group", |s| s.visible()))
                    .child(Icon::new(chevron).xsmall()),
            ),
        };
        let heading = heading
            .id(("sidebar-group", ix))
            .group("sidebar-group")
            .rounded(theme.radius)
            .when(dropping_in, |el| el.bg(theme.drop_target))
            .when(renaming.is_none(), |el| {
                el.on_click(move |_, _, cx| {
                    Settings::update(cx, |settings| settings.sidebar_groups[ix].collapsed = !collapsed);
                })
                .on_drag(dragged, |group, grabbed_at, _, cx| cx.new(|_| DraggedGroup { grabbed_at, ..group.clone() }))
            });
        let heading = Self::track_item_drop(heading, DropArea::Top(Some(ix)), cx).context_menu(move |menu, _, _| {
            menu.item(menu_item("Rename", &workspace, move |this, window, cx| this.rename_group(ix, window, cx)))
                .separator()
                .item(PopupMenuItem::new("Remove Group").on_click(move |_, _, cx| {
                    Settings::update(cx, |settings| settings.remove_sidebar_group(ix));
                }))
        });
        let section = v_flex().relative().gap_px().children(line).child(heading).children(rows);
        Self::track_group_drop(section, Some(ix), cx)
    }

    /// Makes a new group of `items` and starts naming it.
    fn new_group(&mut self, items: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        Settings::update(cx, |settings| {
            settings.remove_from_sidebar(&items);
            settings.sidebar_groups.push(SidebarGroup { name: "New Group".to_string(), collapsed: false, items });
        });
        self.rename_group(Settings::get(cx).sidebar_groups.len() - 1, window, cx);
    }

    fn rename_group(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let name = Settings::get(cx).sidebar_groups[ix].name.clone();
        let len = name.len();
        let input = cx.new(|cx| InputState::new(window, cx).default_value(name));
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.set_selected_range(0..len, cx);
        });
        let subscription = cx.subscribe_in(&input, window, |this, _, event, window, cx| match event {
            InputEvent::PressEnter { .. } | InputEvent::Blur => this.commit_group_rename(window, cx),
            _ => {}
        });
        self.group_rename = Some(GroupRename { ix, input, _subscription: subscription });
        cx.notify();
    }

    fn commit_group_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(rename) = self.group_rename.take() else { return };
        let name = rename.input.read(cx).value().trim().to_string();
        // After Return the folder takes focus back. After a click elsewhere, the click decides.
        if rename.input.focus_handle(cx).is_focused(window) {
            self.focus_active(window, cx);
        }
        if !name.is_empty() {
            Settings::update(cx, |settings| {
                // Another window may have removed it since.
                if let Some(group) = settings.sidebar_groups.get_mut(rename.ix) {
                    group.name = name;
                }
            });
        }
        cx.notify();
    }

    fn cancel_group_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.group_rename = None;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// The file servers found on the network, while there are any. Clicking one connects to it.
    fn render_network(&self, cx: &mut Context<Self>) -> Option<impl IntoElement + use<>> {
        let servers = &cx.global::<Servers>().0;
        if servers.is_empty() {
            return None;
        }
        let theme = cx.theme();
        let rows: Vec<_> = servers
            .iter()
            .map(|name| {
                let address = network::address(name);
                row(SharedString::from(format!("server:{name}")), theme)
                    .child(icons::of_type("com.apple.file-server", PLACE_ICON))
                    .child(truncated(name.clone()).flex_1())
                    .on_click(cx.listener(move |this, _, window, cx| this.connect(address.clone(), window, cx)))
            })
            .collect();
        let connect = action_button("connect-to-server", IconName::Plus, "Connect to Server", ConnectToServer);
        let header = heading("Network", Some(connect.xsmall()), theme);
        Some(v_flex().gap_px().child(header).children(rows))
    }

    /// The shelf, under the places: there while something's on it, or files are dragged that could
    /// go on it. Its files drag off one at a time, or all together.
    fn render_shelf(&self, cx: &Context<Self>) -> Option<impl IntoElement + use<>> {
        let items = shelf::items(cx);
        if items.is_empty() && !self.dragging_files {
            return None;
        }
        let all = Rc::new(items.iter().map(|item| item.path.clone()).collect::<Vec<_>>());
        let rows: Vec<_> = items.iter().map(|item| self.render_shelf_item(item, cx)).collect();
        let theme = cx.theme();
        let clear = (!rows.is_empty()).then(|| {
            Button::new("clear-shelf")
                .ghost()
                .xsmall()
                .icon(IconName::X)
                .tooltip("Clear")
                .on_click(|_, _, cx| shelf::clear(cx))
        });
        let header = heading("Shelf", clear, theme);
        let everything = (rows.len() > 1).then(|| {
            row("shelf-all", theme)
                .child(Icon::new(IconName::Files).small().flex_none().text_color(theme.muted_foreground))
                .child(truncated(format!("All {} items", rows.len())).flex_1())
                .drag_files(all)
        });
        Some(
            v_flex()
                .id("shelf")
                .flex_none()
                .max_h(relative(0.5))
                .p_2()
                .border_t_1()
                .border_color(theme.border)
                .drop_target()
                .on_drop_files(|paths, _, cx| shelf::add(paths, cx))
                .child(header)
                .children(everything)
                .child(v_flex().id("shelf-items").min_h_0().gap_px().overflow_y_scroll().children(rows))
                .when(items.is_empty(), |el| {
                    el.child(
                        div()
                            .h(px(52.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(Icon::new(IconName::Inbox).text_color(theme.muted_foreground)),
                    )
                }),
        )
    }

    fn render_shelf_item(&self, item: &shelf::Item, cx: &Context<Self>) -> impl IntoElement + use<> {
        let name = fs::display_name(&item.path);
        let icon = icons::item(&item.path, item.folder, px(14.));
        let theme = cx.theme();
        let path = item.path.clone();
        let remove = close_button(
            {
                let path = path.clone();
                move |_, _, cx| shelf::remove(&path, cx)
            },
            theme,
        )
        .invisible()
        .group_hover("shelf-item", |s| s.visible())
        .child(Icon::new(IconName::X).xsmall());
        row(SharedString::from(format!("shelf:{}", path.display())), theme)
            .group("shelf-item")
            .pr_1()
            .child(div().flex_none().child(icon))
            .child(truncated(name).flex_1())
            .child(remove)
            .on_click(cx.listener({
                let path = path.clone();
                move |this, _, window, cx| this.reveal(vec![path.clone()], window, cx)
            }))
            .drag_files(Rc::new(vec![path]))
    }

    /// Brings out the shelf, as files start being dragged.
    pub(super) fn files_dragged(&mut self, cx: &mut Context<Self>) {
        if !self.dragging_files {
            self.dragging_files = true;
            cx.notify();
        }
    }

    /// Puts the selection on the shelf, bringing out the sidebar it's in.
    pub(super) fn add_to_shelf(&mut self, cx: &mut Context<Self>) {
        let Some(browser) = self.active_folder() else { return };
        shelf::add(&browser.read(cx).selected_paths(), cx);
        if !self.docks.sidebar {
            self.show_sidebar(true, cx);
        }
    }

    /// Shows where letting go of a drag over `area` would put it: in between near a row's top or
    /// bottom edge and otherwise on the item, or at the top of the list over its top. Over
    /// everything added, clears that once the drag leaves.
    fn track_item_drop<E: InteractiveElement>(element: E, area: DropArea, cx: &Context<Self>) -> E {
        fn track<T: SidebarDrag, E: InteractiveElement>(element: E, area: DropArea, cx: &Context<Workspace>) -> E {
            element.on_drag_move(cx.listener(move |this, event: &DragMoveEvent<T>, _, cx| {
                let (position, bounds) = (event.event.position, event.bounds);
                let drop = match area {
                    DropArea::Row(Slot { group, ix }) if bounds.contains(&position) => {
                        let (at, edge) = ((position.y - bounds.top()) / bounds.size.height, event.drag(cx).edge());
                        if at < edge {
                            Some(Slot { group, ix })
                        } else if at >= 1. - edge {
                            Some(Slot { group, ix: ix + 1 })
                        } else {
                            None
                        }
                    }
                    DropArea::Top(group) if bounds.contains(&position) => {
                        (event.drag(cx).edge() > 0.).then_some(Slot { group, ix: 0 })
                    }
                    DropArea::Added if !bounds.contains(&position) => None,
                    _ => return,
                };
                if this.sidebar_drop != drop {
                    this.sidebar_drop = drop;
                    cx.notify();
                }
            }))
        }
        let element = track::<DraggedSidebarItem, _>(element, area, cx);
        let element = track::<DraggedFiles, _>(element, area, cx);
        track::<ExternalPaths, _>(element, area, cx)
    }

    /// Shows where letting go of a group over the group at `ix` would put it, above or below that
    /// one. With no `ix`, clears that once the drag leaves `element`.
    fn track_group_drop<E: InteractiveElement>(element: E, ix: Option<usize>, cx: &Context<Self>) -> E {
        element.on_drag_move(cx.listener(move |this, event: &DragMoveEvent<DraggedGroup>, _, cx| {
            let (position, bounds) = (event.event.position, event.bounds);
            let drop = match ix {
                Some(ix) if bounds.contains(&position) => {
                    let above = position.y < bounds.center().y;
                    Some(if above { ix } else { ix + 1 })
                }
                None if !bounds.contains(&position) => None,
                _ => return,
            };
            if this.group_drop != drop {
                this.group_drop = drop;
                cx.notify();
            }
        }))
    }

    fn render_place(
        &self,
        name: SharedString,
        path: PathBuf,
        place: Place,
        active: bool,
        underway: Option<Option<f64>>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let menu_path = path.clone();
        let ejectable = place == Place::Volume && path != Path::new("/");
        let trash = cx.global::<Places>().trash;
        let count = (place == Place::Trash && trash > 0).then_some(trash);
        let workspace = cx.weak_entity();
        let added = match place {
            Place::Added(slot) => Some(slot),
            _ => None,
        };
        let drop = self.sidebar_drop.filter(|_| cx.has_active_drag());
        // Where a drag would go in between items: above this one, or below the last in its list.
        let line = added.and_then(|slot| {
            let drop = drop.filter(|drop| drop.group == slot.group)?;
            drop_line(drop.ix, slot.ix, Settings::get(cx).sidebar_list(slot.group).len(), px(1.5), theme)
        });
        let dragged = DraggedSidebarItem {
            path: path.clone(),
            name: name.clone(),
            removing: Rc::default(),
            grabbed_at: Point::default(),
        };
        row(SharedString::from(path.to_string_lossy().into_owned()), theme)
            .relative()
            .when(active, |el| el.bg(theme.sidebar_accent).text_color(theme.sidebar_accent_foreground))
            .when(drop.is_none(), |el| el.drop_target())
            .children(line)
            .child(icons::item(&path, true, PLACE_ICON))
            .child(truncated(name).flex_1())
            .children(count.map(|count| div().text_xs().text_color(theme.muted_foreground).child(count.to_string())))
            .children(underway.map(|done| underway::pie(done, theme.muted_foreground)))
            .when(ejectable, |el| {
                el.child(Button::new("eject").ghost().xsmall().icon(IconName::Eject).tooltip("Eject").on_click(
                    cx.listener({
                        let path = path.clone();
                        move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.eject(path.clone(), window, cx)
                        }
                    }),
                ))
            })
            .map(|el| opens_folder(el, path.clone(), cx))
            .on_drop_files(cx.listener(move |this, paths: &[PathBuf], window, cx| match this.sidebar_drop.take() {
                Some(Slot { group, ix }) => {
                    Settings::update(cx, |settings| settings.place_in_sidebar(group, ix, paths))
                }
                None => drag::drop_into(paths, path.clone(), window, cx).detach(),
            }))
            .when_some(added, |row, slot| {
                let row = row.on_drag(dragged, |item, grabbed_at, _, cx| {
                    cx.new(|_| DraggedSidebarItem { grabbed_at, ..item.clone() })
                });
                Self::track_item_drop(row, DropArea::Row(slot), cx)
            })
            .context_menu(move |menu, _, _| {
                let open = |label, tab: Tab| {
                    menu_item(label, &workspace, move |this, window, cx| {
                        this.open_tab(tab.clone(), window, cx);
                    })
                };
                let window_path = menu_path.clone();
                let menu = menu
                    .item(open("Open in New Tab", Tab::Folder(menu_path.clone())))
                    .item(
                        PopupMenuItem::new("Open in New Window")
                            .on_click(move |_, _, cx| open_window(window_path.clone(), cx)),
                    )
                    .item(open("Open in Terminal", Tab::Terminal(menu_path.clone())));
                if ejectable {
                    let volume = menu_path.clone();
                    return menu.separator().item(menu_item("Eject", &workspace, move |this, window, cx| {
                        this.eject(volume.clone(), window, cx)
                    }));
                }
                if place == Place::Trash {
                    return menu
                        .separator()
                        .item(PopupMenuItem::new("Empty Trash…").on_click(|_, window, cx| empty_trash(window, cx)));
                }
                let Place::Added(_) = place else { return menu };
                let path = menu_path.clone();
                let grouped = path.clone();
                menu.separator()
                    .item(menu_item("New Group", &workspace, move |this, window, cx| {
                        this.new_group(vec![grouped.clone()], window, cx)
                    }))
                    .item(PopupMenuItem::new("Remove from Sidebar").on_click(move |_, _, cx| {
                        Settings::update(cx, |settings| settings.remove_from_sidebar(std::slice::from_ref(&path)));
                    }))
            })
            .into_any_element()
    }
}
