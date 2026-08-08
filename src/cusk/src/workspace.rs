//! Workspaces.
//!
//! Generic over the element type on purpose. A `Window` cannot be constructed
//! without a live Wayland surface, so a `Workspaces<Window>` would be
//! untestable — every bug in switching, moving and removal would have to be
//! found by clicking. `Workspaces<u32>` in the tests exercises exactly the same
//! code the compositor runs.
//!
//! # What is per-workspace and what is not
//!
//! Order, tiling mode, layout and focus all belong to a workspace: switching to
//! a tiled workspace and back must not leave the other one tiled, and returning
//! to a workspace should put the keyboard where you left it. The layout
//! *engine* is shared, because it is a pure function; only the choice of policy
//! is per-workspace.
//!
//! Window geometry is deliberately **not** stored here. It already lives in the
//! window's own `UserDataMap` (see `geometry`), which means a window carries its
//! floating rectangle across a workspace move for free, and there is no second
//! place for that rectangle to be wrong.

use crate::layout::Layout;

#[derive(Debug, Clone)]
pub struct Workspace<T> {
    /// Tile order, oldest first — never stacking order.
    pub order: Vec<T>,
    pub tiling: bool,
    pub layout: Layout,
    /// Focus is per-workspace, so coming back puts the keyboard where it was.
    pub focused: Option<T>,
}

impl<T> Workspace<T> {
    fn new(tiling: bool, layout: Layout) -> Self {
        Workspace { order: Vec::new(), tiling, layout, focused: None }
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
}

#[derive(Debug, Clone)]
pub struct Workspaces<T> {
    spaces: Vec<Workspace<T>>,
    active: usize,
}

/// What the compositor must do to the `Space` after a switch.
#[derive(Debug, Clone, PartialEq)]
pub struct Switch<T> {
    /// Windows to unmap — they belong to the workspace being left.
    pub hide: Vec<T>,
    /// Windows to map — they belong to the workspace being entered.
    pub show: Vec<T>,
    /// Where to put the keyboard afterwards.
    pub focus: Option<T>,
}

impl<T: Clone + PartialEq> Workspaces<T> {
    /// Always at least one workspace: a compositor with zero has nowhere to
    /// map a window, and every caller would need to handle that.
    pub fn new(count: usize, tiling: bool, layout: Layout) -> Self {
        let count = count.max(1);
        Workspaces {
            spaces: (0..count).map(|_| Workspace::new(tiling, layout)).collect(),
            active: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.spaces.len()
    }

    pub fn active_index(&self) -> usize {
        self.active
    }

    pub fn active(&self) -> &Workspace<T> {
        &self.spaces[self.active]
    }

    pub fn active_mut(&mut self) -> &mut Workspace<T> {
        &mut self.spaces[self.active]
    }

    /// Which workspaces hold at least one window, for an indicator.
    pub fn occupied(&self) -> Vec<bool> {
        self.spaces.iter().map(|w| !w.is_empty()).collect()
    }

    /// Switch to a workspace, reporting what has to be unmapped and mapped.
    ///
    /// Returns `None` for the workspace already active — not as an
    /// optimisation, but because acting on it would unmap and remap every
    /// window on screen, which flickers and drops focus for nothing.
    pub fn switch_to(&mut self, index: usize) -> Option<Switch<T>> {
        if index >= self.spaces.len() || index == self.active {
            return None;
        }
        let hide = self.spaces[self.active].order.clone();
        self.active = index;
        let entering = &self.spaces[index];
        Some(Switch {
            hide,
            show: entering.order.clone(),
            // Falling back to the last window rather than to nothing: arriving
            // at a populated workspace with no keyboard focus makes the
            // keyboard look broken.
            focus: entering.focused.clone().or_else(|| entering.order.last().cloned()),
        })
    }

    /// Add a window to the active workspace.
    pub fn insert(&mut self, window: T) {
        self.spaces[self.active].order.push(window);
    }

    /// Move a window from the active workspace to another.
    ///
    /// Returns the window to unmap and where focus should go, or `None` if
    /// there was nothing to move.
    pub fn move_to(&mut self, window: &T, index: usize) -> Option<Option<T>> {
        if index >= self.spaces.len() || index == self.active {
            return None;
        }
        let current = &mut self.spaces[self.active];
        let position = current.order.iter().position(|w| w == window)?;
        let moved = current.order.remove(position);

        if current.focused.as_ref() == Some(window) {
            // Focus cannot follow the window to a workspace that is not on
            // screen. Hand it to a neighbour, preferring the one that took the
            // moved window's place.
            current.focused = current
                .order
                .get(position)
                .or_else(|| current.order.last())
                .cloned();
        }
        let focus = current.focused.clone();

        let target = &mut self.spaces[index];
        target.order.push(moved.clone());
        // The moved window becomes the focus of where it lands, so switching
        // after it arrives puts the keyboard on the thing you just sent.
        target.focused = Some(moved);

        Some(focus)
    }

    /// Forget a window everywhere.
    ///
    /// Searches every workspace, not just the active one: a client can close a
    /// window on a workspace nobody is looking at, and a leftover entry there
    /// would reserve a tile for a window that no longer exists.
    pub fn remove(&mut self, window: &T) {
        for space in &mut self.spaces {
            space.order.retain(|w| w != window);
            if space.focused.as_ref() == Some(window) {
                space.focused = space.order.last().cloned();
            }
        }
    }

    /// Which workspace a window is on.
    ///
    /// No caller in the compositor yet — a panel showing "this window is on
    /// 3" is what wants it. Kept because the tests assert against it, and
    /// because a set of workspaces that cannot answer where a window is would
    /// be an odd thing to have.
    #[cfg(test)]
    pub fn workspace_of(&self, window: &T) -> Option<usize> {
        self.spaces.iter().position(|s| s.order.contains(window))
    }

    /// Grow or shrink the set, keeping windows.
    ///
    /// Windows on workspaces that disappear are moved to the last surviving
    /// one rather than dropped. Losing a window because a number in a config
    /// file got smaller would be unrecoverable from inside the session.
    pub fn resize(&mut self, count: usize, tiling: bool, layout: Layout) {
        let count = count.max(1);
        while self.spaces.len() < count {
            self.spaces.push(Workspace::new(tiling, layout));
        }
        while self.spaces.len() > count {
            let dropped = self.spaces.pop().expect("count is at least 1");
            let last = self.spaces.last_mut().expect("count is at least 1");
            last.order.extend(dropped.order);
        }
        self.active = self.active.min(self.spaces.len() - 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spaces(count: usize) -> Workspaces<u32> {
        Workspaces::new(count, false, Layout::default())
    }

    #[test]
    fn there_is_always_at_least_one_workspace() {
        assert_eq!(spaces(0).len(), 1);
        assert_eq!(spaces(4).len(), 4);
    }

    #[test]
    fn windows_land_on_the_active_workspace() {
        let mut w = spaces(3);
        w.insert(1);
        w.switch_to(1);
        w.insert(2);
        assert_eq!(w.active().order, vec![2]);
        assert_eq!(w.workspace_of(&1), Some(0));
        assert_eq!(w.workspace_of(&2), Some(1));
    }

    /// Switching must report the exact set to unmap and the exact set to map.
    /// A window left mapped from the previous workspace is the classic
    /// workspace bug: it floats above everything and cannot be got rid of.
    #[test]
    fn switching_reports_what_to_hide_and_show() {
        let mut w = spaces(2);
        w.insert(1);
        w.insert(2);
        w.switch_to(1);
        w.insert(3);

        let switch = w.switch_to(0).unwrap();
        assert_eq!(switch.hide, vec![3]);
        assert_eq!(switch.show, vec![1, 2]);
    }

    /// Switching to where you already are must do nothing, rather than unmap
    /// and remap every window on screen.
    #[test]
    fn switching_to_the_active_workspace_is_a_no_op() {
        let mut w = spaces(3);
        w.insert(1);
        assert!(w.switch_to(0).is_none());
        assert_eq!(w.active_index(), 0);
    }

    #[test]
    fn switching_out_of_range_does_nothing() {
        let mut w = spaces(2);
        assert!(w.switch_to(9).is_none());
        assert_eq!(w.active_index(), 0);
    }

    /// Coming back to a workspace should put the keyboard where it was left.
    #[test]
    fn focus_is_remembered_per_workspace() {
        let mut w = spaces(2);
        w.insert(1);
        w.insert(2);
        w.active_mut().focused = Some(1);
        w.switch_to(1);
        let back = w.switch_to(0).unwrap();
        assert_eq!(back.focus, Some(1));
    }

    /// Arriving somewhere populated with no remembered focus must still focus
    /// something, or the keyboard appears dead.
    #[test]
    fn arriving_with_no_remembered_focus_still_focuses_something() {
        let mut w = spaces(2);
        w.switch_to(1);
        w.insert(7);
        w.active_mut().focused = None;
        w.switch_to(0);
        let switch = w.switch_to(1).unwrap();
        assert_eq!(switch.focus, Some(7));
    }

    #[test]
    fn tiling_and_layout_are_per_workspace() {
        let mut w = spaces(2);
        w.active_mut().tiling = true;
        w.active_mut().layout = Layout::Columns;
        w.switch_to(1);
        assert!(!w.active().tiling, "the other workspace must be untouched");
        w.switch_to(0);
        assert!(w.active().tiling);
        assert_eq!(w.active().layout, Layout::Columns);
    }

    #[test]
    fn a_window_can_be_moved_to_another_workspace() {
        let mut w = spaces(3);
        w.insert(1);
        w.insert(2);
        assert!(w.move_to(&2, 2).is_some());
        assert_eq!(w.active().order, vec![1]);
        assert_eq!(w.workspace_of(&2), Some(2));
    }

    /// Focus cannot follow a window to a workspace that is not on screen.
    #[test]
    fn moving_the_focused_window_hands_focus_to_a_neighbour() {
        let mut w = spaces(2);
        w.insert(1);
        w.insert(2);
        w.active_mut().focused = Some(2);
        let focus = w.move_to(&2, 1).unwrap();
        assert_eq!(focus, Some(1), "focus stays on this workspace");
    }

    /// The moved window should be focused where it lands, so switching after
    /// it puts the keyboard on the thing you just sent.
    #[test]
    fn a_moved_window_is_focused_where_it_arrives() {
        let mut w = spaces(2);
        w.insert(1);
        w.move_to(&1, 1);
        let switch = w.switch_to(1).unwrap();
        assert_eq!(switch.focus, Some(1));
    }

    #[test]
    fn moving_to_the_current_workspace_does_nothing() {
        let mut w = spaces(2);
        w.insert(1);
        assert!(w.move_to(&1, 0).is_none());
        assert_eq!(w.active().order, vec![1]);
    }

    /// A client can close a window on a workspace nobody is looking at.
    #[test]
    fn removal_searches_every_workspace() {
        let mut w = spaces(3);
        w.insert(1);
        w.move_to(&1, 2);
        w.remove(&1);
        assert_eq!(w.workspace_of(&1), None);
        assert!(w.occupied().iter().all(|o| !o));
    }

    #[test]
    fn removing_the_focused_window_moves_focus() {
        let mut w = spaces(1);
        w.insert(1);
        w.insert(2);
        w.active_mut().focused = Some(2);
        w.remove(&2);
        assert_eq!(w.active().focused, Some(1));
    }

    #[test]
    fn occupancy_reports_which_workspaces_hold_windows() {
        let mut w = spaces(3);
        w.insert(1);
        w.move_to(&1, 2);
        assert_eq!(w.occupied(), vec![false, false, true]);
    }

    /// Shrinking the set must not destroy windows. Losing one because a number
    /// in a config file got smaller would be unrecoverable from inside the
    /// session.
    #[test]
    fn shrinking_rehomes_windows_rather_than_dropping_them() {
        let mut w = spaces(4);
        w.insert(1);
        w.move_to(&1, 3);
        w.resize(2, false, Layout::default());
        assert_eq!(w.len(), 2);
        assert_eq!(w.workspace_of(&1), Some(1), "the window survived");
    }

    #[test]
    fn shrinking_below_the_active_workspace_moves_you_somewhere_real() {
        let mut w = spaces(5);
        w.switch_to(4);
        w.resize(2, false, Layout::default());
        assert!(w.active_index() < w.len());
    }

    #[test]
    fn growing_adds_empty_workspaces() {
        let mut w = spaces(2);
        w.insert(1);
        w.resize(6, false, Layout::default());
        assert_eq!(w.len(), 6);
        assert_eq!(w.workspace_of(&1), Some(0));
    }
}
