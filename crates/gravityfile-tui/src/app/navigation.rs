//! Navigation abstractions for list-based views.
//!
//! This module provides reusable navigation primitives for list-based views.
//! The `ListNavigator` trait and `SimpleListNav` struct are designed for future
//! refactoring to unify navigation across different views.

#![allow(dead_code)]

use super::constants::PAGE_SIZE;

/// Trait for types that support list-style navigation.
pub trait ListNavigator {
    /// Get the currently selected index.
    fn selected(&self) -> usize;

    /// Set the selected index.
    fn set_selected(&mut self, index: usize);

    /// Get the maximum valid index (item count - 1, or 0 if empty).
    fn max_index(&self) -> usize;

    /// Move selection up by count items.
    fn move_up(&mut self, count: usize) {
        let current = self.selected();
        self.set_selected(current.saturating_sub(count));
    }

    /// Move selection down by count items.
    fn move_down(&mut self, count: usize) {
        let current = self.selected();
        let max = self.max_index();
        self.set_selected((current + count).min(max));
    }

    /// Move selection up by one page.
    fn page_up(&mut self) {
        self.move_up(PAGE_SIZE);
    }

    /// Move selection down by one page.
    fn page_down(&mut self) {
        self.move_down(PAGE_SIZE);
    }

    /// Jump to the first item.
    fn jump_to_top(&mut self) {
        self.set_selected(0);
    }

    /// Jump to the last item.
    fn jump_to_bottom(&mut self) {
        self.set_selected(self.max_index());
    }
}

/// Simple list navigator for index-based lists.
#[derive(Debug, Clone, Default)]
pub struct SimpleListNav {
    selected: usize,
    count: usize,
}

impl SimpleListNav {
    /// Create a new list navigator with the given item count.
    pub fn new(count: usize) -> Self {
        Self { selected: 0, count }
    }

    /// Update the item count, clamping selection if necessary.
    pub fn set_count(&mut self, count: usize) {
        self.count = count;
        if self.selected > self.max_index() {
            self.selected = self.max_index();
        }
    }

    /// Get the item count.
    pub fn count(&self) -> usize {
        self.count
    }
}

impl ListNavigator for SimpleListNav {
    fn selected(&self) -> usize {
        self.selected
    }

    fn set_selected(&mut self, index: usize) {
        self.selected = index.min(self.max_index());
    }

    fn max_index(&self) -> usize {
        self.count.saturating_sub(1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_list_nav() {
        let mut nav = SimpleListNav::new(10);
        assert_eq!(nav.selected(), 0);
        assert_eq!(nav.max_index(), 9);

        nav.move_down(3);
        assert_eq!(nav.selected(), 3);

        nav.move_up(1);
        assert_eq!(nav.selected(), 2);

        nav.jump_to_bottom();
        assert_eq!(nav.selected(), 9);

        nav.jump_to_top();
        assert_eq!(nav.selected(), 0);
    }

    #[test]
    fn test_list_nav_bounds() {
        let mut nav = SimpleListNav::new(5);

        // Can't go below 0
        nav.move_up(10);
        assert_eq!(nav.selected(), 0);

        // Can't go above max
        nav.move_down(100);
        assert_eq!(nav.selected(), 4);
    }

    #[test]
    fn test_list_nav_empty() {
        let mut nav = SimpleListNav::new(0);
        assert_eq!(nav.max_index(), 0);
        nav.move_down(1);
        assert_eq!(nav.selected(), 0);
    }
}
