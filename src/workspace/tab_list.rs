//! A window's tabs in order and which one is active, apart from what they show, so where tabs go
//! can be tested with plain values.

use std::ops::Deref;

pub(super) struct TabList<T> {
    items: Vec<T>,
    active: usize,
    /// How many tabs opened behind the active one since it became active. Each goes after the
    /// last, like in a browser.
    opened_behind: usize,
}

impl<T> Default for TabList<T> {
    fn default() -> Self {
        Self { items: Vec::new(), active: 0, opened_behind: 0 }
    }
}

impl<T> Deref for TabList<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        &self.items
    }
}

impl<T> TabList<T> {
    /// Where the active tab is.
    pub(super) fn active(&self) -> usize {
        self.active
    }

    pub(super) fn select(&mut self, ix: usize) {
        self.active = ix;
        self.opened_behind = 0;
    }

    /// Puts `item` at `ix`, keeping the active tab active.
    pub(super) fn insert(&mut self, ix: usize, item: T) {
        if ix <= self.active && !self.items.is_empty() {
            self.active += 1;
        }
        self.items.insert(ix, item);
    }

    /// Puts `item` after the active tab and any opened from it before, and says where.
    pub(super) fn insert_behind(&mut self, item: T) -> usize {
        let ix = (self.active + 1 + self.opened_behind).min(self.items.len());
        self.insert(ix, item);
        self.opened_behind += 1;
        ix
    }

    /// Takes out tab `ix`. The one after it becomes active if it was, or the one before when it
    /// was the last.
    pub(super) fn remove(&mut self, ix: usize) -> T {
        let item = self.items.remove(ix);
        if ix < self.active || self.active == self.items.len() {
            self.active = self.active.saturating_sub(1);
        }
        item
    }

    /// Moves tab `from` to `to`, where it takes the place of the one there, or to the end past
    /// it, keeping the active tab active.
    pub(super) fn move_item(&mut self, from: usize, to: usize) {
        let item = self.items.remove(from);
        let to = to.min(self.items.len());
        self.items.insert(to, item);
        self.active = match self.active {
            active if active == from => to,
            active if from < active && active <= to => active - 1,
            active if to <= active && active < from => active + 1,
            active => active,
        };
    }

    /// Where a tab dropped on tab `to` would go, as the gap before the tab at that index, or after
    /// the last at how many there are. `from` is where it is, if it's one of these. None when it
    /// would stay where it is.
    pub(super) fn drop_gap(&self, from: Option<usize>, to: usize) -> Option<usize> {
        let Some(from) = from else { return Some(to) };
        // It leaves its own place first, so further along it goes after the tab it's over.
        let gap = if from < to { (to + 1).min(self.items.len()) } else { to };
        (gap != from && gap != from + 1).then_some(gap)
    }
}

#[cfg(test)]
mod tests {
    use super::TabList;

    fn tabs(items: &str, active: usize) -> TabList<char> {
        TabList { items: items.chars().collect(), active, opened_behind: 0 }
    }

    fn order(tabs: &TabList<char>) -> String {
        tabs.iter().collect()
    }

    #[test]
    fn tabs_opened_behind_go_after_each_other() {
        let mut tabs = tabs("abc", 0);
        assert_eq!(tabs.insert_behind('x'), 1);
        assert_eq!(tabs.insert_behind('y'), 2);
        assert_eq!((order(&tabs), tabs.active()), ("axybc".into(), 0));
        // Another tab becoming active starts again right after it.
        tabs.select(3);
        assert_eq!(tabs.insert_behind('z'), 4);
        assert_eq!(order(&tabs), "axybzc");
    }

    #[test]
    fn inserting_keeps_the_active_tab() {
        let mut tabs = tabs("abc", 1);
        tabs.insert(0, 'x');
        assert_eq!(tabs[tabs.active()], 'b');
        tabs.insert(4, 'y');
        assert_eq!(tabs[tabs.active()], 'b');
        let mut empty = TabList::default();
        empty.insert(0, 'a');
        assert_eq!(empty.active(), 0);
    }

    #[test]
    fn removing_the_active_tab_shows_the_next_or_else_the_last() {
        let mut tabs = tabs("abcd", 1);
        tabs.remove(1);
        assert_eq!(tabs[tabs.active()], 'c');
        tabs.remove(2);
        assert_eq!(tabs[tabs.active()], 'c');
        tabs.remove(1);
        assert_eq!(tabs[tabs.active()], 'a');
    }

    #[test]
    fn removing_another_tab_keeps_the_active_one() {
        let mut tabs = tabs("abcd", 2);
        tabs.remove(0);
        assert_eq!(tabs[tabs.active()], 'c');
        tabs.remove(2);
        assert_eq!(tabs[tabs.active()], 'c');
    }

    #[test]
    fn moving_keeps_the_active_tab() {
        for (from, to, moved) in [(0, 3, "bcda"), (3, 0, "dabc"), (1, 2, "acbd"), (2, 9, "abdc"), (0, 1, "bacd")] {
            for active in 0..4 {
                let mut tabs = tabs("abcd", active);
                let shown = tabs[active];
                tabs.move_item(from, to);
                assert_eq!(order(&tabs), moved);
                assert_eq!(tabs[tabs.active()], shown, "moving {from} to {to} with {active} active");
            }
        }
    }

    /// Reopen Closed Tab opens a tab and then moves it to where the closed one was, or the end if
    /// there are fewer tabs now.
    #[test]
    fn a_reopened_tab_goes_back_where_it_was() {
        let mut tabs = tabs("abc", 0);
        let ix = tabs.insert_behind('x');
        tabs.select(ix);
        tabs.move_item(tabs.active(), 3);
        assert_eq!((order(&tabs), tabs.active()), ("abcx".into(), 3));
        tabs.move_item(tabs.active(), 7);
        assert_eq!((order(&tabs), tabs.active()), ("abcx".into(), 3));
    }

    #[test]
    fn a_dragged_tab_goes_in_the_gap_it_was_let_go_at() {
        let tabs = tabs("abcd", 0);
        // From another window, it goes before the tab it's over, or past the last.
        assert_eq!(tabs.drop_gap(None, 0), Some(0));
        assert_eq!(tabs.drop_gap(None, 4), Some(4));
        // Further along, after the tab it's over.
        assert_eq!(tabs.drop_gap(Some(0), 2), Some(3));
        assert_eq!(tabs.drop_gap(Some(0), 3), Some(4));
        assert_eq!(tabs.drop_gap(Some(0), 4), Some(4));
        // Back, before the tab it's over.
        assert_eq!(tabs.drop_gap(Some(3), 1), Some(1));
        assert_eq!(tabs.drop_gap(Some(1), 0), Some(0));
        // Over itself, or past the last when it's the last, it stays.
        assert_eq!(tabs.drop_gap(Some(1), 1), None);
        assert_eq!(tabs.drop_gap(Some(3), 4), None);
    }
}
