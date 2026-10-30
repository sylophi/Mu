//! Selecting items with the arrow keys, with clicks, and by typing the start of a name.

use std::{
    collections::HashSet,
    path::PathBuf,
    time::{Duration, Instant},
};

use gpui_kit::*;

use super::{
    Browser,
    listing::Listing,
    view::{CELL_HEIGHT, Mode, ROW_HEIGHT},
};
use crate::{fs::Entry, quicklook, workspace::TabEvent};

/// What's selected, by name, so it holds as the folder reloads.
#[derive(Default)]
pub(super) struct Selection {
    pub(super) names: HashSet<SharedString>,
    /// The item the arrow keys go on from, which an outline marks.
    pub(super) cursor: Option<SharedString>,
    /// Where a range selected with ⇧ starts from.
    pub(super) anchor: Option<SharedString>,
}

impl Selection {
    pub(super) fn contains(&self, name: &str) -> bool {
        self.names.contains(name)
    }

    pub(super) fn len(&self) -> usize {
        self.names.len()
    }

    /// Selects just `names`, with the cursor and the anchor on the first.
    pub(super) fn set(&mut self, names: Vec<SharedString>) {
        self.cursor = names.first().cloned();
        self.anchor = self.cursor.clone();
        self.names = names.into_iter().collect();
    }

    /// Selects item `ix` of `listing`, or with `extend`, the items from the anchor to it. The
    /// cursor goes to it.
    pub(super) fn select(&mut self, listing: &Listing, ix: usize, extend: bool) {
        let name = listing[ix].name.clone();
        let anchor_ix = self.anchor.as_ref().and_then(|anchor| listing.position(anchor));
        match anchor_ix.filter(|_| extend) {
            Some(anchor_ix) => {
                let range = anchor_ix.min(ix)..=anchor_ix.max(ix);
                self.names = range.map(|ix| listing[ix].name.clone()).collect();
            }
            None => {
                self.names = HashSet::from([name.clone()]);
                self.anchor = Some(name.clone());
            }
        }
        self.cursor = Some(name);
    }

    /// Selects `name`, or lets it go if it's selected, as a ⌘-click does. The cursor and the anchor
    /// go to it.
    pub(super) fn toggle(&mut self, name: SharedString) {
        if !self.names.remove(&name) {
            self.names.insert(name.clone());
        }
        self.cursor = Some(name.clone());
        self.anchor = Some(name);
    }

    /// Selects everything `listing` shows, with the cursor on the first item unless it's somewhere.
    pub(super) fn select_all(&mut self, listing: &Listing) {
        self.names = listing.iter().map(|e| e.name.clone()).collect();
        if self.cursor.is_none() && !listing.is_empty() {
            self.cursor = Some(listing[0].name.clone());
        }
    }

    /// Lets go of what `listing` doesn't show, the cursor and the anchor too.
    pub(super) fn keep_to(&mut self, listing: &Listing) {
        self.names.retain(|name| listing.position(name).is_some());
        for slot in [&mut self.cursor, &mut self.anchor] {
            if slot.as_ref().is_some_and(|name| listing.position(name).is_none()) {
                *slot = None;
            }
        }
    }
}

impl Browser {
    pub(super) fn name_at(&self, ix: usize) -> SharedString {
        self.listing[ix].name.clone()
    }

    pub(super) fn cursor_ix(&self) -> Option<usize> {
        self.listing.position(self.selection.cursor.as_ref()?)
    }

    pub(super) fn cursor_entry(&self) -> Option<&Entry> {
        Some(&self.listing[self.cursor_ix()?])
    }

    /// Selected entries in display order.
    pub(super) fn selected_entries(&self) -> Vec<&Entry> {
        let mut ixs: Vec<usize> = self.selection.names.iter().filter_map(|name| self.listing.position(name)).collect();
        ixs.sort_unstable();
        ixs.into_iter().map(|ix| &self.listing[ix]).collect()
    }

    pub fn selected_paths(&self) -> Vec<PathBuf> {
        self.selected_entries().into_iter().map(|e| e.path.clone()).collect()
    }

    pub(super) fn select_ix(&mut self, ix: usize, extend: bool, cx: &mut Context<Self>) {
        self.selection.select(&self.listing, ix, extend);
        self.scroll_to(ix, ScrollStrategy::Nearest, cx);
        cx.notify();
    }

    /// Moves the cursor `delta` items. With no cursor yet, a step forward lands on the first item
    /// and a step back on the last.
    pub(super) fn move_cursor(&mut self, delta: isize, extend: bool, cx: &mut Context<Self>) {
        let Some(last) = self.listing.len().checked_sub(1) else { return };
        let ix = match self.cursor_ix() {
            Some(ix) => ix.saturating_add_signed(delta).min(last),
            None if delta > 0 => 0,
            None => last,
        };
        self.select_ix(ix, extend, cx);
    }

    /// Moves the cursor `rows` up or down, which in the grid is a whole row of tiles.
    pub(super) fn move_rows(&mut self, rows: isize, extend: bool, cx: &mut Context<Self>) {
        self.move_cursor(rows * self.columns(cx) as isize, extend, cx);
    }

    /// Selects the first or the last item.
    pub(super) fn select_end(&mut self, last: bool, extend: bool, cx: &mut Context<Self>) {
        if let Some(end) = self.listing.len().checked_sub(1) {
            self.select_ix(if last { end } else { 0 }, extend, cx);
        }
    }

    /// Moves a page up or down: as many rows as fit.
    pub(super) fn move_page(&mut self, down: bool, cx: &mut Context<Self>) {
        let row = match self.mode(cx) {
            Mode::List => ROW_HEIGHT,
            Mode::Grid => CELL_HEIGHT,
            // Its items aren't in rows, and it's a picture that's mostly all in view.
            Mode::Layout(_) => return self.select_end(down, false, cx),
        };
        let rows = (self.scroll_handle().bounds().size.height / row).floor().max(1.) as isize;
        self.move_rows(if down { rows } else { -rows }, false, cx);
    }

    pub(super) fn select_all(&mut self, cx: &mut Context<Self>) {
        self.selection.select_all(&self.listing);
        cx.notify();
    }

    pub(super) fn clear_selection(&mut self, cx: &mut Context<Self>) {
        if quicklook::close() {
            return;
        }
        self.selection.names.clear();
        cx.notify();
    }

    /// A press on item `ix`. When it's `grabbed`, by its name or picture, a selection it's part of
    /// stays whole so it can be dragged, and a plain click narrows it on mouse up. Pressed elsewhere
    /// in its row, a drag selects instead, so the press narrows it right away.
    pub(super) fn press_item(
        &mut self,
        ix: usize,
        event: &MouseDownEvent,
        grabbed: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_handle.focus(window, cx);
        let name = self.name_at(ix);
        let modifiers = event.modifiers;
        // The first press of what may be a double click.
        if event.click_count == 1 && !modifiers.shift && !modifiers.platform && self.listing[ix].folder {
            self.prefetch(self.listing[ix].target().to_path_buf(), window, cx);
        }
        if modifiers.platform && event.click_count == 2 {
            // ⌘-double-click opens a folder in a new tab, like Finder. The first click selected it.
            let entry = &self.listing[ix];
            if entry.folder {
                cx.emit(TabEvent::OpenTab(entry.target().to_path_buf()));
            }
        } else if modifiers.shift {
            self.select_ix(ix, true, cx);
        } else if modifiers.platform {
            self.selection.toggle(name);
        } else if !grabbed || !self.selection.contains(&name) {
            self.select_ix(ix, false, cx);
        } else {
            self.selection.cursor = Some(name);
        }
        if event.click_count == 2 && !modifiers.shift && !modifiers.platform {
            self.open(window, cx);
        }
        cx.notify();
    }

    pub(super) fn row_click(&mut self, ix: usize, event: &ClickEvent, cx: &mut Context<Self>) {
        let modifiers = event.modifiers();
        if !modifiers.shift && !modifiers.platform && event.click_count() == 1 && self.selection.len() > 1 {
            self.select_ix(ix, false, cx);
        }
    }

    pub(super) fn type_ahead(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.focus_handle.is_focused(window) {
            return;
        }
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        if modifiers.platform || modifiers.control || modifiers.alt || modifiers.function {
            return;
        }
        let Some(text) = keystroke.key_char.clone() else { return };
        if text.chars().any(char::is_control) {
            return;
        }
        if !self.typing_ahead() {
            self.type_ahead.clear();
        }
        self.extend_type_ahead(&text, cx);
        cx.stop_propagation();
    }

    /// Whether a type-ahead search is underway, so Space continues it instead of opening Quick Look.
    pub(super) fn typing_ahead(&self) -> bool {
        !self.type_ahead.is_empty() && self.type_ahead_at.elapsed() < Duration::from_millis(900)
    }

    pub(super) fn extend_type_ahead(&mut self, text: &str, cx: &mut Context<Self>) {
        self.type_ahead.push_str(text);
        self.type_ahead_at = Instant::now();
        let needle = self.type_ahead.to_lowercase();
        let found = self.listing.iter().position(|e| e.name.to_lowercase().starts_with(&needle));
        if let Some(ix) = found {
            self.select_ix(ix, false, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Selection;
    use crate::browser::listing::tests::listing;

    fn names(selection: &Selection) -> Vec<&str> {
        let mut names: Vec<&str> = selection.names.iter().map(|name| name.as_ref()).collect();
        names.sort();
        names
    }

    #[test]
    fn selects_a_range_from_the_anchor() {
        let listing = listing(&["a", "b", "c", "d"]);
        let mut selection = Selection::default();
        selection.select(&listing, 1, false);
        assert_eq!(names(&selection), ["b"]);
        selection.select(&listing, 3, true);
        assert_eq!(names(&selection), ["b", "c", "d"]);
        // The anchor stays put, so the range turns around it.
        selection.select(&listing, 0, true);
        assert_eq!(names(&selection), ["a", "b"]);
        assert_eq!((selection.cursor.as_deref(), selection.anchor.as_deref()), (Some("a"), Some("b")));
    }

    #[test]
    fn toggles_and_keeps_to_what_shows() {
        let mut selection = Selection::default();
        selection.select_all(&listing(&["a", "b", "c"]));
        assert_eq!(selection.cursor.as_deref(), Some("a"));
        selection.toggle("b".into());
        assert_eq!(names(&selection), ["a", "c"]);
        assert_eq!(selection.anchor.as_deref(), Some("b"));
        // Hidden by a filter, "c" is let go of, and the cursor and the anchor with it.
        selection.select(&listing(&["a", "b", "c"]), 1, false);
        selection.toggle("c".into());
        selection.keep_to(&listing(&["a", "b"]));
        assert_eq!(names(&selection), ["b"]);
        assert_eq!((selection.cursor.as_deref(), selection.anchor.as_deref()), (None, None));
    }
}
