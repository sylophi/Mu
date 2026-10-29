//! Drawing the listing: the list's rows and header, the grid's tiles, and a folder Finder laid out.

use std::{cell::OnceCell, collections::HashSet, ops::Range, path::PathBuf, rc::Rc, sync::Arc};

use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex, input::Input, scroll::Scrollbar, v_flex};
use gpui_kit::{prelude::FluentBuilder as _, *};

use super::{
    Browser, CancelRename,
    layout::Layout,
    listing::SortKey,
    ops::{CutFiles, RenameState},
};
use crate::{
    drag::{self, DragFiles as _, DropFiles as _},
    fs::{self, Entry},
    icons,
    settings::{COLUMN_WIDTHS, Settings},
    workspace::TabEvent,
};

pub(super) const ROW_HEIGHT: Pixels = px(26.);
/// The least room a tile in the grid gets, its cell. Rows share out what's left over.
pub(super) const CELL_WIDTH: Pixels = px(100.);
/// How far apart the grid's rows are.
pub(super) const CELL_HEIGHT: Pixels = px(116.);
/// A tile in the grid, centered in its cell `TILE_TOP` from its top. It's about as tall as its icon
/// and two lines of name.
pub(super) const TILE: Size<Pixels> = size(px(92.), px(100.));
pub(super) const TILE_TOP: Pixels = px(8.);
pub(super) const TILE_ICON: Pixels = px(56.);
/// How far a tile's icon and name are in from its edges.
const TILE_PADDING: Pixels = px(4.);
/// The cloud mark after a name, and the gap before it.
const CLOUD_MARK: Pixels = px(14.);
const ROW_ICON: Pixels = px(16.);
pub(super) const HEADER_HEIGHT: Pixels = px(24.);
/// The least a list column can be dragged down to.
const MIN_COLUMN_WIDTH: Pixels = px(40.);

/// How the listing shows its items.
#[derive(Clone, Copy)]
pub(super) enum Mode<'a> {
    /// A row each, under the columns' header.
    List,
    /// Tiles, as many to a row as fit.
    Grid,
    /// Where Finder put them, over its picture.
    Layout(&'a Layout),
}

/// The divider after the list's column `.0` being dragged.
struct ResizeColumn(usize);

impl Render for ResizeColumn {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// What the rows or tiles drawn in one go share.
struct Batch<'a> {
    focused: bool,
    /// What selected items are tinted with.
    selected_tint: Hsla,
    /// What the item under the pointer is tinted with, unless it's selected.
    hover_tint: Hsla,
    widths: [Pixels; 4],
    row_width: Pixels,
    /// How big a tile's icon is, and how wide the tile.
    tile_icon: Pixels,
    tile_width: Pixels,
    cut: Option<&'a HashSet<PathBuf>>,
    /// The selection's paths, for dragging, worked out the first time a selected item needs them.
    selected_paths: OnceCell<Rc<Vec<PathBuf>>>,
}

impl Batch<'_> {
    /// Hidden files and cut ones show faded.
    fn dimmed(&self, entry: &Entry) -> bool {
        entry.hidden || self.cut.is_some_and(|cut| cut.contains(&entry.path))
    }
}

impl Browser {
    /// How the listing shows its items: a folder Finder laid out as Finder does, and any other as
    /// a list or a grid, whichever the setting says.
    pub(super) fn mode(&self, cx: &App) -> Mode<'_> {
        match &self.layout {
            Some(layout) => Mode::Layout(layout),
            None if Settings::get(cx).grid => Mode::Grid,
            None => Mode::List,
        }
    }

    fn column_widths(&self, cx: &App) -> [Pixels; 4] {
        self.resizing.map_or(Settings::get(cx).column_widths, |(_, widths)| widths)
    }

    /// How wide the list's rows are: their columns and borders, or the whole listing when that's
    /// wider. Past the listing's width, it scrolls sideways.
    pub(super) fn row_width(&self, cx: &App) -> Pixels {
        let columns: Pixels = self.column_widths(cx).into_iter().sum();
        (columns + px(2.)).max(self.width)
    }

    /// Items per row: as many tiles as fit in the grid, one in the list and a layout.
    pub(super) fn columns(&self, cx: &App) -> usize {
        match self.mode(cx) {
            Mode::Grid => (self.width / CELL_WIDTH).floor().max(1.) as usize,
            Mode::List | Mode::Layout(_) => 1,
        }
    }

    /// What scrolls the listing down, and where it is in the window. The list and the grid keep it
    /// in their `uniform_list`'s handle, and a layout, which isn't a list, tracks the same one, so
    /// the box and the keyboard find either there.
    pub(super) fn scroll_handle(&self) -> ScrollHandle {
        self.scroll.0.borrow().base_handle.clone()
    }

    pub(super) fn scroll_to(&self, ix: usize, strategy: ScrollStrategy, cx: &App) {
        match self.mode(cx) {
            Mode::List | Mode::Grid => self.scroll.scroll_to_item(ix / self.columns(cx), strategy),
            Mode::Layout(_) => self.scroll_layout_to(ix, strategy, cx),
        }
    }

    /// Scrolls a layout to item `ix` the way `uniform_list` scrolls to a row, and sideways only as
    /// far as it takes to show it. The layout keeps the offset in bounds as it's drawn.
    fn scroll_layout_to(&self, ix: usize, strategy: ScrollStrategy, cx: &App) {
        let handle = self.scroll_handle();
        let (item, view, offset) = (self.item_bounds(ix, cx), handle.bounds().size, handle.offset());
        // The offset that shows `length` from `start` along an axis `view` long, moving as little
        // as it can from `offset`.
        let nearest = |start: Pixels, length: Pixels, view: Pixels, offset: Pixels| {
            if start + offset < Pixels::ZERO {
                -start
            } else if start + length + offset > view {
                view - length - start
            } else {
                offset
            }
        };
        let y = match strategy {
            ScrollStrategy::Top => -item.top(),
            ScrollStrategy::Center => (view.height - item.size.height) / 2. - item.top(),
            ScrollStrategy::Bottom => view.height - item.bottom(),
            ScrollStrategy::Nearest => nearest(item.top(), item.size.height, view.height, offset.y),
        };
        handle.set_offset(point(nearest(item.left(), item.size.width, view.width, offset.x), y));
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let widths = self.column_widths(cx);
        let cells: Vec<_> = SortKey::ALL
            .into_iter()
            .zip(widths)
            .map(|(key, width)| {
                let label = key.label();
                let active = self.listing.sort == key;
                h_flex()
                    .id(label)
                    .w(width)
                    .flex_none()
                    .h_full()
                    .px_2()
                    .gap_1()
                    .text_color(if active { cx.theme().foreground } else { cx.theme().muted_foreground })
                    .hover(|style| style.text_color(cx.theme().foreground))
                    // Room for a row's icon, so the label lines up with the names.
                    .when(key == SortKey::Name, |el| el.child(div().w(ROW_ICON + px(4.)).flex_none()))
                    .when(key == SortKey::Size, |el| el.justify_end())
                    .child(div().min_w_0().truncate().child(label))
                    .when(active, |el| {
                        let chevron = if self.listing.descending { IconName::ChevronDown } else { IconName::ChevronUp };
                        el.child(Icon::new(chevron).xsmall().flex_none())
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.set_sort(key, cx)))
            })
            .collect();
        let mut right = Pixels::ZERO;
        let dividers: Vec<_> = widths
            .into_iter()
            .enumerate()
            .map(|(ix, width)| {
                right += width;
                self.render_divider(ix, right, cx)
            })
            .collect();
        let row = h_flex()
            .relative()
            .flex_none()
            .w(self.row_width(cx))
            .h_full()
            .border_x_1()
            .border_color(transparent_black())
            .children(cells)
            .children(dividers)
            .on_drag_move(cx.listener(|this, event: &DragMoveEvent<ResizeColumn>, _, cx| {
                let ix = event.drag(cx).0;
                let mut widths = this.column_widths(cx);
                // Past the row's border and the columns before this one.
                let left = event.bounds.left() + px(1.) + widths[..ix].iter().copied().sum::<Pixels>();
                widths[ix] = (event.event.position.x - left).max(MIN_COLUMN_WIDTH);
                this.resizing = Some((ix, widths));
                cx.notify();
            }))
            // Saved once the drag is let go, not on every move.
            .on_mouse_up(MouseButton::Left, cx.listener(Self::finish_resize))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::finish_resize));
        div()
            .h(HEADER_HEIGHT)
            .flex_none()
            .text_xs()
            .border_b_1()
            .border_color(cx.theme().border)
            // Presses here are the header's, not the listing's to select with.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .child(row)
    }

    /// The rows or tiles, which scroll down. In the list, the rows and their header scroll sideways
    /// together, as one, with the scrollbars kept to the edges of what's in view.
    pub(super) fn render_listing(&self, cx: &mut Context<Self>) -> Div {
        let grid = matches!(self.mode(cx), Mode::Grid);
        let rows = self.listing.len().div_ceil(self.columns(cx));
        let mut list =
            uniform_list("entries", rows, cx.processor(Self::render_rows)).size_full().track_scroll(&self.scroll);
        // Only down. Sideways is for the columns around it, which a sideways swipe mustn't turn into.
        list.style().restrict_scroll_to_axis = Some(true);
        let marquee = self.render_marquee(cx);
        if grid {
            return div().size_full().child(list).children(marquee).child(Scrollbar::vertical(&self.scroll));
        }
        let columns = v_flex()
            .w(self.row_width(cx))
            .h_full()
            .child(self.render_header(cx))
            .child(div().relative().flex_1().min_h_0().child(list).children(marquee));
        div()
            .size_full()
            .child(
                div()
                    .id("columns")
                    .size_full()
                    .overflow_x_scroll()
                    .restrict_scroll_to_axis()
                    .track_scroll(&self.columns_scroll)
                    .child(columns),
            )
            .child(
                div()
                    .absolute()
                    .top(HEADER_HEIGHT)
                    .bottom_0()
                    .left_0()
                    .right_0()
                    .child(Scrollbar::vertical(&self.scroll).viewport_from_layout()),
            )
            .child(Scrollbar::horizontal(&self.columns_scroll))
    }

    /// The divider at the right edge of column `ix`, `right` into the row, that drags the column's
    /// width. A double click puts the column back to its first width.
    fn render_divider(&self, ix: usize, right: Pixels, cx: &Context<Self>) -> Stateful<Div> {
        let theme = cx.theme();
        let dragged = self.resizing.is_some_and(|(dragged, _)| dragged == ix);
        h_flex()
            .id(("divider", ix))
            .absolute()
            .top_0()
            .left(right - px(4.))
            .w(px(8.))
            .h_full()
            .justify_center()
            .items_center()
            .cursor_col_resize()
            .group("divider")
            .child(
                div()
                    .w(px(1.))
                    .h(px(12.))
                    .bg(theme.border)
                    .when(dragged, |el| el.h_full().bg(theme.muted_foreground))
                    .group_hover("divider", |style| style.h_full().bg(theme.muted_foreground)),
            )
            .on_click(cx.listener(move |_, event: &ClickEvent, _, cx| {
                if event.click_count() > 1 {
                    Settings::update(cx, |settings| settings.column_widths[ix] = COLUMN_WIDTHS[ix]);
                }
                // It isn't a click on the column, which sorts by it.
                cx.stop_propagation();
            }))
            .on_drag(ResizeColumn(ix), |drag, _, _, cx| cx.new(|_| ResizeColumn(drag.0)))
    }

    fn finish_resize(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((_, widths)) = self.resizing.take() {
            Settings::update(cx, |settings| settings.column_widths = widths);
        }
    }

    /// The rows the listing shows in `range`: one item each in the list, a row of tiles in the grid.
    fn render_rows(&mut self, range: Range<usize>, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let columns = self.columns(cx);
        let grid = matches!(self.mode(cx), Mode::Grid);
        let items = range.start * columns..(range.end * columns).min(self.listing.len());
        // Quick Look's pictures of what the files hold, once they're made.
        let pictures: Vec<_> = match grid {
            true => {
                items.clone().map(|ix| self.tiles.get(&self.listing[ix], window, cx, |this| &mut this.tiles)).collect()
            }
            false => Vec::new(),
        };
        let cx: &Context<Self> = cx;
        let batch = self.batch(window, cx);
        if !grid {
            return items.map(|ix| self.render_row(ix, &batch, cx).into_any_element()).collect();
        }
        // Where each tile's name wraps, found with the tile's font.
        let font_size = rems(0.75).to_pixels(window.rem_size());
        let mut wrapper = window.text_system().line_wrapper(window.text_style().font(), font_size);
        range
            .map(|row| {
                let tiles = (row * columns..((row + 1) * columns).min(self.listing.len())).map(|ix| {
                    let tile = self.render_tile(ix, pictures[ix - items.start].clone(), &mut wrapper, &batch, cx);
                    div().w(relative(1. / columns as f32)).flex().justify_center().pt(TILE_TOP).child(tile)
                });
                h_flex().w_full().h(CELL_HEIGHT).items_start().children(tiles).into_any_element()
            })
            .collect()
    }

    /// What the rows, tiles or the layout's items all go by.
    fn batch<'a>(&self, window: &Window, cx: &'a App) -> Batch<'a> {
        let (focused, theme) = (self.focus_handle.contains_focused(window, cx), cx.theme());
        let (tile_icon, tile_width, light) = match self.mode(cx) {
            Mode::Layout(layout) => (layout.icon_size, layout.tile_width(), layout.light == Some(true)),
            Mode::List | Mode::Grid => (TILE_ICON, TILE.width, false),
        };
        // The theme's tints suit its own background, so on a light picture they go dark.
        let (selected_tint, hover_tint) = match light {
            true => (black().opacity(if focused { 0.15 } else { 0.08 }), black().opacity(0.05)),
            false => (if focused { theme.list_active } else { theme.secondary_active }, theme.list_hover),
        };
        Batch {
            focused,
            selected_tint,
            hover_tint,
            widths: self.column_widths(cx),
            row_width: self.row_width(cx),
            tile_icon,
            tile_width,
            cut: CutFiles::current(cx),
            selected_paths: OnceCell::new(),
        }
    }

    /// A folder Finder laid out, like a disk image's window: its picture, with each item where
    /// Finder put it, centered in the listing while it fits.
    pub(super) fn render_layout(&self, layout: &Layout, window: &mut Window, cx: &Context<Self>) -> Div {
        let batch = self.batch(window, cx);
        let mut wrapper = window.text_system().line_wrapper(window.text_style().font(), layout.text_size);
        // Off the picture, as items Finder hasn't placed can be, names are as usual.
        let on_picture = Bounds::new(Point::default(), layout.size);
        let tiles = self.spots.iter().enumerate().map(|(ix, &at)| {
            let tile = layout.tile(at);
            let names = match layout.light.filter(|_| on_picture.contains(&at)) {
                Some(true) => black(),
                Some(false) => white(),
                None => cx.theme().foreground,
            };
            self.render_tile(ix, None, &mut wrapper, &batch, cx)
                .absolute()
                .left(tile.origin.x)
                .top(tile.origin.y)
                .text_size(layout.text_size)
                .text_color(names)
        });
        let (extent, origin) = (self.layout_extent(layout), self.layout_origin(layout));
        let picture = img(layout.picture.clone()).absolute().w(layout.size.width).h(layout.size.height);
        div()
            .relative()
            .size_full()
            .child(
                div().id("layout").size_full().overflow_scroll().track_scroll(&self.scroll_handle()).child(
                    div()
                        .relative()
                        .ml(origin.x)
                        .mt(origin.y)
                        .w(extent.width)
                        .h(extent.height)
                        .child(picture)
                        .children(tiles),
                ),
            )
            .children(self.render_marquee(cx))
    }

    /// How much room the layout takes: its picture, and any items past it.
    fn layout_extent(&self, layout: &Layout) -> Size<Pixels> {
        self.spots
            .iter()
            .map(|&at| layout.tile(at))
            .fold(layout.size, |extent, tile| size(extent.width.max(tile.right()), extent.height.max(tile.bottom())))
    }

    /// Where the layout's top left corner is in the listing's content: centered along each side it
    /// fits along, and at the start of any it doesn't, so all of it can be scrolled to.
    pub(super) fn layout_origin(&self, layout: &Layout) -> Point<Pixels> {
        let (viewport, extent) = (self.viewport().0.size, self.layout_extent(layout));
        point(
            ((viewport.width - extent.width) / 2.).max(Pixels::ZERO),
            ((viewport.height - extent.height) / 2.).max(Pixels::ZERO),
        )
    }

    fn render_row(&self, ix: usize, batch: &Batch, cx: &Context<Self>) -> Stateful<Div> {
        let entry = &self.listing[ix];
        let icon = icons::entry(entry, ROW_ICON);
        let dimmed = batch.dimmed(entry);
        let muted = cx.theme().muted_foreground;
        let [name_width, modified_width, size_width, kind_width] = batch.widths;
        let cell = |width: Pixels| div().w(width).flex_none().px_2().truncate().text_color(muted);
        let rename = self.rename.as_ref().filter(|rename| rename.name == entry.name);
        let name = h_flex().min_w_0().gap_1().when(dimmed, |el| el.opacity(0.6)).map(|el| match rename {
            Some(rename) => el.flex_1().child(self.render_rename(rename, cx).flex_1()),
            // Cut in the middle, so a long name keeps its extension.
            None => el
                .child(div().min_w_0().truncate().text_ellipsis_middle().child(one_line(&entry.name)))
                .children(cloud_mark(entry, cx)),
        });
        self.item(h_flex().id(ElementId::Name(entry.name.clone())), ix, batch, cx)
            .h(ROW_HEIGHT)
            .w(batch.row_width)
            .text_sm()
            .child(
                h_flex().w(name_width).flex_none().px_2().child(
                    self.grip(h_flex().id("grip").min_w_0().gap_2(), ix, batch, cx)
                        .when(rename.is_some(), |el| el.flex_1())
                        .child(div().flex_none().when(dimmed, |el| el.opacity(0.5)).child(icon))
                        .child(name),
                ),
            )
            .child(cell(modified_width).children(entry.modified.map(fs::format_date)))
            .child(cell(size_width).text_right().child(if entry.folder || entry.package {
                "—".to_string()
            } else {
                fs::format_size(entry.size)
            }))
            .child(cell(kind_width).child(entry.kind.clone()))
    }

    /// Item `ix`'s tile, with `thumbnail` or else its icon, and its name wrapped by `wrapper`.
    fn render_tile(
        &self,
        ix: usize,
        thumbnail: Option<Arc<RenderImage>>,
        wrapper: &mut LineWrapper,
        batch: &Batch,
        cx: &Context<Self>,
    ) -> Stateful<Div> {
        let entry = &self.listing[ix];
        let (icon, width) = (batch.tile_icon, batch.tile_width);
        // Inside the tile's padding, less the cloud mark beside the name.
        let room = width - TILE_PADDING * 2. - if entry.cloud_only { CLOUD_MARK } else { Pixels::ZERO };
        let (first, rest) = tile_name(one_line(&entry.name), room, wrapper);
        let dimmed = batch.dimmed(entry);
        let picture = match thumbnail {
            // Fitted in a square, or the picture's own shape would stretch the tile. A badge goes on
            // the picture's corner.
            Some(thumbnail) => {
                let Size { width, height } = thumbnail.size(0).map(|side| side.0 as f32);
                let fit = f32::from(icon) / width.max(height);
                let picture = img(thumbnail).w(px(width * fit)).h(px(height * fit));
                div()
                    .size(icon)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(icons::badged(picture, entry, icon))
                    .into_any_element()
            }
            // What it has no picture of, or not yet, shows its icon.
            None => icons::entry(entry, icon).into_any_element(),
        };
        let tile = self.item(v_flex().id(ElementId::Name(entry.name.clone())), ix, batch, cx);
        self.grip(tile, ix, batch, cx)
            .w(width)
            .p(TILE_PADDING)
            .gap_1()
            .items_center()
            .rounded(cx.theme().radius)
            .text_xs()
            .child(div().when(dimmed, |el| el.opacity(0.5)).child(picture))
            .child(div().w_full().when(dimmed, |el| el.opacity(0.6)).map(|el| {
                match self.rename.as_ref().filter(|rename| rename.name == entry.name) {
                    Some(rename) => el.child(self.render_rename(rename, cx)),
                    None => el
                        .flex()
                        .justify_center()
                        .gap_0p5()
                        .child(
                            v_flex().min_w_0().items_center().child(div().whitespace_nowrap().child(first)).children(
                                rest.map(|rest| div().max_w_full().truncate().text_ellipsis_middle().child(rest)),
                            ),
                        )
                        .children(cloud_mark(entry, cx)),
                }
            }))
    }

    /// The field a name is edited in.
    fn render_rename(&self, rename: &RenameState, cx: &Context<Self>) -> Div {
        div()
            .key_context("Rename")
            .on_action(cx.listener(|this, _: &CancelRename, window, cx| this.cancel_rename(window, cx)))
            // Clicks in the field place the caret, so the item mustn't take them.
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(Input::new(&rename.input).xsmall())
    }

    /// What rows and tiles share: the selection's colors, right and middle clicks, and drops.
    fn item(&self, el: Stateful<Div>, ix: usize, batch: &Batch, cx: &Context<Self>) -> Stateful<Div> {
        let theme = cx.theme();
        let entry = &self.listing[ix];
        let selected = self.selection.contains(&entry.name);
        let is_cursor = self.selection.cursor.as_ref() == Some(&entry.name);
        let (path, into) = (entry.path.clone(), entry.target().to_path_buf());
        el.border_1()
            .border_color(transparent_black())
            .when(selected, |el| el.bg(batch.selected_tint))
            // The outline marks the end of a selection that the arrows move, so it goes once the
            // item's no longer selected.
            .when(is_cursor && selected && batch.focused, |el| el.border_color(theme.list_active_border))
            .when(!selected, |el| el.hover(|style| style.bg(batch.hover_tint)))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, window, cx| {
                    this.focus_handle.focus(window, cx);
                    if !this.selection.contains(&this.name_at(ix)) {
                        this.select_ix(ix, false, cx);
                    }
                }),
            )
            .when(entry.folder, |el| {
                el.on_mouse_down(
                    MouseButton::Middle,
                    cx.listener({
                        let into = into.clone();
                        move |_, _, _, cx| cx.emit(TabEvent::OpenTab(into.clone()))
                    }),
                )
            })
            .when(entry.folder, |el| {
                el.drop_target()
                    .can_drop(move |value, _, _| drag::dragged(value).is_none_or(|paths| !paths.contains(&path)))
                    .on_drop_files(cx.listener(move |this, paths: &[PathBuf], window, cx| {
                        this.drop_paths(paths, into.clone(), window, cx)
                    }))
            })
    }

    /// The part of item `ix` that picks it up: a tile, or a row's icon and name. A click there
    /// selects the item and a drag takes the files. Anywhere else, a drag selects.
    fn grip(&self, el: Stateful<Div>, ix: usize, batch: &Batch, cx: &Context<Self>) -> Stateful<Div> {
        let entry = &self.listing[ix];
        let paths = match self.selection.contains(&entry.name) {
            true => batch.selected_paths.get_or_init(|| Rc::new(self.selected_paths())).clone(),
            false => Rc::new(vec![entry.path.clone()]),
        };
        el.on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event, window, cx| {
                this.press_item(ix, event, true, window, cx);
                cx.stop_propagation();
            }),
        )
        .on_click(cx.listener(move |this, event, _, cx| this.row_click(ix, event, cx)))
        .drag_files(paths)
    }
}

/// The mark after the name of what's only in iCloud, as in Finder.
fn cloud_mark(entry: &Entry, cx: &App) -> Option<Icon> {
    entry
        .cloud_only
        .then(|| Icon::new(IconName::CloudDownload).xsmall().flex_none().text_color(cx.theme().muted_foreground))
}

/// A tile's name as Finder's grid has it: as much as fits on the first line, then the rest on the
/// second, cut in the middle if it's too long, so a long name keeps its extension. GPUI cuts a
/// name wrapped to two lines down to one.
fn tile_name(name: SharedString, width: Pixels, wrapper: &mut LineWrapper) -> (SharedString, Option<SharedString>) {
    let wrap = wrapper.wrap_line(&[LineFragment::text(&name)], width, IndentAdjustment::NoIndent).next();
    match wrap {
        Some(wrap) => (name[..wrap.ix].trim_end().to_string().into(), Some(name[wrap.ix..].to_string().into())),
        None => (name, None),
    }
}

/// `name` on one line: control characters, like a newline, show as "?" the way `ls` shows them.
fn one_line(name: &SharedString) -> SharedString {
    if name.contains(char::is_control) { name.replace(char::is_control, "?").into() } else { name.clone() }
}
