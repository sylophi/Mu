//! The sidebar: favorites, iCloud Drive, disks, the Trash and servers on the network, then the shelf.

use std::{
    cell::Cell,
    path::{Path, PathBuf},
    rc::Rc,
};

use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Sizable as _, StyledExt as _, Theme,
    button::{Button, ButtonVariants as _},
    h_flex,
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
    drag::{self, DragFiles as _, DraggedFiles, DropFiles as _, drag_preview},
    fs::{self, home_dir},
    icons,
    network::{self, Servers},
    places::Places,
    session::Tab,
    settings::Settings,
    shelf,
    theme::Toasts as _,
};

/// What a sidebar entry is.
#[derive(Clone, Copy, PartialEq)]
enum Place {
    /// The favorite at this index.
    Favorite(usize),
    ICloudDrive,
    Volume,
    Trash,
}

/// A favorite being dragged, to another spot in Favorites or out of the sidebar to remove it.
#[derive(Clone)]
pub(super) struct DraggedFavorite {
    pub(super) path: PathBuf,
    name: SharedString,
    /// Whether it's out of the sidebar, where letting go removes it.
    removing: Rc<Cell<bool>>,
    grabbed_at: Point<Pixels>,
}

impl Render for DraggedFavorite {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let icon = match self.removing.get() {
            true => Icon::new(IconName::CircleX).size(PLACE_ICON).text_color(cx.theme().danger).into_any_element(),
            false => icons::item(&self.path, true, PLACE_ICON).into_any_element(),
        };
        drag_preview(self.grabbed_at, icon, self.name.clone(), cx)
    }
}

/// Something that can be dragged into Favorites.
trait FavoriteDrag: 'static {
    /// How much of a favorite's row, from its top and from its bottom, drops this in between
    /// favorites instead of on the one there.
    fn edge(&self) -> f32;
}

impl FavoriteDrag for DraggedFavorite {
    fn edge(&self) -> f32 {
        0.5
    }
}

impl FavoriteDrag for DraggedFiles {
    fn edge(&self) -> f32 {
        folders_edge(&self.paths)
    }
}

impl FavoriteDrag for ExternalPaths {
    fn edge(&self) -> f32 {
        folders_edge(self.paths())
    }
}

/// Only folders go in Favorites.
fn folders_edge(paths: &[PathBuf]) -> f32 {
    if paths.iter().all(|path| path.is_dir()) { 0.25 } else { 0. }
}

/// The size of the icons by places in the sidebar.
const PLACE_ICON: Pixels = px(16.);

/// The heading of the Network or the Shelf section, with a button at its end.
fn heading(title: &'static str, button: Option<Button>, theme: &Theme) -> Div {
    h_flex()
        .h(px(20.))
        .pl_2()
        .mb_1()
        .justify_between()
        .text_xs()
        .font_medium()
        .text_color(theme.muted_foreground)
        .child(title)
        .children(button)
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
                Err(error) => window.toast_error(&error, cx),
            })
            .ok();
        })
        .detach();
    }

    /// The folders in the sidebar's sections, from the top: Favorites, iCloud while iCloud Drive is
    /// on, and Locations.
    fn places(&self, cx: &App) -> [Vec<(SharedString, PathBuf, Place)>; 3] {
        let favorites = Settings::get(cx)
            .favorites
            .iter()
            .enumerate()
            .map(|(ix, path)| (SharedString::from(fs::display_name(path)), path.clone(), Place::Favorite(ix)))
            .collect();
        let icloud = fs::icloud_drive();
        let icloud = if icloud.is_dir() { vec![("iCloud Drive".into(), icloud, Place::ICloudDrive)] } else { vec![] };
        let volumes = &cx.global::<Places>().volumes;
        let mut locations: Vec<_> =
            volumes.iter().map(|volume| (volume.name.clone(), volume.path.clone(), Place::Volume)).collect();
        locations.push(("Trash".into(), fs::trash_dir(), Place::Trash));
        [favorites, icloud, locations]
    }

    /// Shows the folder at this position in the sidebar, counting from the top.
    pub(super) fn show_place(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((_, path, _)) = self.places(cx).into_iter().flatten().nth(ix) {
            self.show_folder(path, window, cx);
        }
    }

    pub(super) fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.last_folder.as_ref().map(|browser| browser.read(cx).path().to_path_buf());
        let [favorites, icloud, locations] = self.places(cx);
        let section = |title: &'static str, items: Vec<(SharedString, PathBuf, Place)>, cx: &mut Context<Self>| {
            v_flex()
                .gap_px()
                .child(div().px_2().pb_1().text_xs().font_medium().text_color(cx.theme().muted_foreground).child(title))
                .children(items.into_iter().map(|(name, path, place)| {
                    let active = current.as_ref() == Some(&path);
                    self.render_place(name, path, place, active, cx)
                }))
        };
        let favorites = Self::track_favorite_drop(section("Favorites", favorites, cx), None, cx);
        let places = v_flex()
            .id("sidebar")
            .flex_1()
            .min_h_0()
            .p_2()
            .gap_4()
            .overflow_y_scroll()
            .on_drag_move(|event: &DragMoveEvent<DraggedFavorite>, window, cx| {
                let removing = !event.bounds.contains(&event.event.position);
                if event.drag(cx).removing.replace(removing) != removing {
                    window.refresh();
                }
            })
            .on_drop(cx.listener(|this, favorite: &DraggedFavorite, _, cx| {
                if let Some(ix) = this.favorite_drop.take() {
                    Settings::update(cx, |settings| settings.place_favorites(ix, std::slice::from_ref(&favorite.path)));
                }
            }))
            .on_drop_files(cx.listener(|this, paths: &[PathBuf], _, cx| {
                if let Some(ix) = this.favorite_drop.take() {
                    Settings::update(cx, |settings| settings.place_favorites(ix, paths));
                }
            }))
            .child(favorites)
            .when(!icloud.is_empty(), |el| el.child(section("iCloud", icloud, cx)))
            .child(section("Locations", locations, cx))
            .children(self.render_network(cx));
        v_flex()
            .size_full()
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .child(places)
            .children(self.render_shelf(cx))
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

    /// Shows where letting go of a drag over favorite `ix` would put it in Favorites: in between
    /// near the row's top or bottom edge, and otherwise on the favorite. With no `ix`, clears that
    /// once the drag leaves `element`.
    fn track_favorite_drop<E: InteractiveElement>(element: E, ix: Option<usize>, cx: &Context<Self>) -> E {
        fn track<T: FavoriteDrag, E: InteractiveElement>(element: E, ix: Option<usize>, cx: &Context<Workspace>) -> E {
            element.on_drag_move(cx.listener(move |this, event: &DragMoveEvent<T>, _, cx| {
                let (position, bounds) = (event.event.position, event.bounds);
                let drop = match ix {
                    Some(ix) if bounds.contains(&position) => {
                        let (at, edge) = ((position.y - bounds.top()) / bounds.size.height, event.drag(cx).edge());
                        if at < edge {
                            Some(ix)
                        } else if at >= 1. - edge {
                            Some(ix + 1)
                        } else {
                            None
                        }
                    }
                    None if !bounds.contains(&position) => None,
                    _ => return,
                };
                if this.favorite_drop != drop {
                    this.favorite_drop = drop;
                    cx.notify();
                }
            }))
        }
        let element = track::<DraggedFavorite, _>(element, ix, cx);
        let element = track::<DraggedFiles, _>(element, ix, cx);
        track::<ExternalPaths, _>(element, ix, cx)
    }

    fn render_place(
        &self,
        name: SharedString,
        path: PathBuf,
        place: Place,
        active: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let menu_path = path.clone();
        let ejectable = place == Place::Volume && path != Path::new("/");
        let trash = cx.global::<Places>().trash;
        let count = (place == Place::Trash && trash > 0).then_some(trash);
        let workspace = cx.weak_entity();
        let favorite = match place {
            Place::Favorite(ix) => Some(ix),
            _ => None,
        };
        let drop = self.favorite_drop.filter(|_| cx.has_active_drag());
        // Where a drag would go in between favorites: above this one, or below the last.
        let line = favorite.and_then(|ix| match drop? {
            drop if drop == ix => Some(px(-1.5)),
            drop if drop == ix + 1 && drop == Settings::get(cx).favorites.len() => Some(px(25.5)),
            _ => None,
        });
        let dragged = DraggedFavorite {
            path: path.clone(),
            name: name.clone(),
            removing: Rc::default(),
            grabbed_at: Point::default(),
        };
        row(SharedString::from(path.to_string_lossy().into_owned()), theme)
            .relative()
            .when(active, |el| el.bg(theme.sidebar_accent).text_color(theme.sidebar_accent_foreground))
            .when(drop.is_none(), |el| el.drop_target())
            .children(line.map(|top| div().absolute().top(top).left_1().right_1().h(px(2.)).bg(theme.foreground)))
            .child(icons::item(&path, true, PLACE_ICON))
            .child(truncated(name).flex_1())
            .children(count.map(|count| div().text_xs().text_color(theme.muted_foreground).child(count.to_string())))
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
            .on_drop_files(cx.listener(move |this, paths: &[PathBuf], window, cx| match this.favorite_drop.take() {
                Some(ix) => Settings::update(cx, |settings| settings.place_favorites(ix, paths)),
                None => drag::drop_into(paths, path.clone(), window, cx).detach(),
            }))
            .when_some(favorite, |row, ix| {
                let row = row.on_drag(dragged, |favorite, grabbed_at, _, cx| {
                    cx.new(|_| DraggedFavorite { grabbed_at, ..favorite.clone() })
                });
                Self::track_favorite_drop(row, Some(ix), cx)
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
                let Place::Favorite(_) = place else { return menu };
                let remove_path = menu_path.clone();
                menu.separator().item(PopupMenuItem::new("Remove from Sidebar").on_click(move |_, _, cx| {
                    Settings::update(cx, |settings| settings.favorites.retain(|p| p != &remove_path));
                }))
            })
            .into_any_element()
    }
}
