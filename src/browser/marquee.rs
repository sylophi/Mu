//! Selecting with a box dragged out over the listing, and finding which item is where.

use std::{collections::HashSet, time::Duration};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

use super::{
    Browser,
    view::{CELL_HEIGHT, Mode, ROW_HEIGHT, TILE, TILE_TOP},
};
use crate::quicklook;

/// A box being dragged out over the listing, which selects what it touches.
pub(super) struct SelectArea;

impl Render for SelectArea {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// A press on an empty spot of the listing, and the box it becomes once it's dragged.
pub(super) struct Marquee {
    /// Where the press was, in the listing's content, so it stays put as the listing scrolls.
    start: Point<Pixels>,
    /// Where the pointer is in the window, once it's moved.
    pointer: Option<Point<Pixels>>,
    /// The listing's scroll position the selection was last worked out at.
    offset: Point<Pixels>,
    /// What was selected before, which a press with ⌘ or ⇧ keeps. The box toggles what it touches.
    base: HashSet<SharedString>,
    _scroll: Task<()>,
}

impl Browser {
    /// A press on the listing outside any item's name or picture. In a row, it selects the row. On
    /// an empty spot, it clears the selection, unless ⌘ or ⇧ is down. Either way, dragging from
    /// there draws a box that selects what it touches.
    pub(super) fn press_listing(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_handle.focus(window, cx);
        let extend = event.modifiers.platform || event.modifiers.shift;
        let base = if extend { self.selection.names.clone() } else { HashSet::new() };
        let (bounds, offset) = self.viewport();
        let start = event.position - bounds.origin - offset;
        let row = (start.y / ROW_HEIGHT).floor() as usize;
        if matches!(self.mode(cx), Mode::List) && start.y >= px(0.) && row < self.listing.len() {
            self.press_item(row, event, false, window, cx);
        } else if !extend {
            self.selection.names.clear();
        }
        self.marquee = Some(Marquee { start, pointer: None, offset, base, _scroll: Task::ready(()) });
        cx.notify();
    }

    pub(super) fn drag_marquee(&mut self, event: &DragMoveEvent<SelectArea>, _: &mut Window, cx: &mut Context<Self>) {
        let Some(marquee) = &mut self.marquee else { return };
        if marquee.pointer.is_none() {
            marquee._scroll = cx.spawn(async |this, cx| {
                while let Ok(true) = this.update(cx, |this, cx| this.scroll_marquee(cx)) {
                    cx.background_executor().timer(Duration::from_millis(16)).await;
                }
            });
        }
        marquee.pointer = Some(event.event.position);
        self.select_marquee(cx);
    }

    /// While the pointer is past the listing's top or bottom, scrolls that way, faster the further
    /// past it is. False once the box is let go.
    fn scroll_marquee(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(pointer) = self.marquee.as_ref().and_then(|marquee| marquee.pointer) else { return false };
        let handle = self.scroll_handle();
        let bounds = handle.bounds();
        let past = (pointer.y - bounds.top()).min(px(0.)) + (pointer.y - bounds.bottom()).max(px(0.));
        let offset = handle.offset();
        let y = (offset.y - past / 2.).clamp(-handle.max_offset().y, px(0.));
        if y != offset.y {
            handle.set_offset(point(offset.x, y));
        }
        // Scrolled by this or by the wheel, the box now reaches over other items.
        if self.marquee.as_ref().is_some_and(|marquee| marquee.offset != handle.offset()) {
            self.select_marquee(cx);
        }
        true
    }

    /// Selects what the box touches, toggled against what the press kept.
    fn select_marquee(&mut self, cx: &mut Context<Self>) {
        let (bounds, offset) = self.viewport();
        let Some(marquee) = &mut self.marquee else { return };
        let Some(pointer) = marquee.pointer else { return };
        marquee.offset = offset;
        let (start, end) = (marquee.start, pointer - bounds.origin - offset);
        let mut selection = marquee.base.clone();
        // A pixel more, so a drag straight across still touches the row it's in.
        let area = Bounds::from_corners(start.min(&end), start.max(&end) + point(px(1.), px(1.)));
        let touched: Vec<usize> =
            (0..self.listing.len()).filter(|&ix| self.item_bounds(ix, cx).intersects(&area)).collect();
        for &ix in &touched {
            let name = self.name_at(ix);
            if !selection.remove(&name) {
                selection.insert(name);
            }
        }
        self.selection.names = selection;
        // The cursor goes where the drag is headed, so the keyboard carries on from there.
        if let (Some(&first), Some(&last)) = (touched.first(), touched.last()) {
            let (from, to) = if end.y < start.y { (last, first) } else { (first, last) };
            self.selection.anchor = Some(self.name_at(from));
            self.selection.cursor = Some(self.name_at(to));
        }
        cx.notify();
    }

    pub(super) fn end_marquee(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.marquee.take().is_some_and(|marquee| marquee.pointer.is_some()) {
            cx.notify();
        }
    }

    /// Where the listing is in the window, and how far it's scrolled.
    pub(super) fn viewport(&self) -> (Bounds<Pixels>, Point<Pixels>) {
        let handle = self.scroll_handle();
        (handle.bounds(), handle.offset())
    }

    /// Where item `ix` is in the listing's content: its whole row in the list, its tile in the grid
    /// or the layout.
    pub(super) fn item_bounds(&self, ix: usize, cx: &App) -> Bounds<Pixels> {
        match self.mode(cx) {
            Mode::List => Bounds::new(point(px(0.), ROW_HEIGHT * ix as f32), size(self.row_width(cx), ROW_HEIGHT)),
            Mode::Grid => {
                let columns = self.columns(cx);
                let (row, column) = (ix / columns, ix % columns);
                let cell = self.width / columns as f32;
                let origin =
                    point(cell * column as f32 + (cell - TILE.width) / 2., CELL_HEIGHT * row as f32 + TILE_TOP);
                Bounds::new(origin, TILE)
            }
            Mode::Layout(layout) => {
                let tile = layout.tile(self.spots[ix]);
                Bounds::new(tile.origin + self.layout_origin(layout), tile.size)
            }
        }
    }

    /// Shows the item at `position` in the window in Quick Look, selected, or closes Quick Look on it
    /// if it's open. It's what a tap with three fingers does in Finder.
    pub fn look_up(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let (bounds, offset) = self.viewport();
        let at = position - bounds.origin - offset;
        let hit = (0..self.listing.len()).find(|&ix| self.item_bounds(ix, cx).contains(&at));
        let Some(ix) = hit.filter(|_| bounds.contains(&position)) else { return };
        if self.selection.contains(&self.name_at(ix)) {
            quicklook::toggle(self.selected_paths());
        } else {
            self.select_ix(ix, false, cx);
            quicklook::show(self.selected_paths());
        }
    }

    /// The box being dragged out, kept to the listing.
    pub(super) fn render_marquee(&self, cx: &App) -> Option<Div> {
        let marquee = self.marquee.as_ref()?;
        let (bounds, offset) = self.viewport();
        let (start, end) = (marquee.start + offset, marquee.pointer? - bounds.origin);
        let area = Bounds::from_corners(start.min(&end), start.max(&end))
            .intersect(&Bounds::new(Point::default(), bounds.size));
        Some(
            div()
                .absolute()
                .left(area.origin.x)
                .top(area.origin.y)
                .w(area.size.width)
                .h(area.size.height)
                .border_1()
                .border_color(cx.theme().list_active_border)
                .bg(cx.theme().list_active.opacity(0.5)),
        )
    }
}
