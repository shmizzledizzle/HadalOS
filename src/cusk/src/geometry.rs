//! Remembered floating geometry.
//!
//! `docs/cusk.md` §3 lists this as a prerequisite for mode switching: a window
//! that leaves floating and comes back must return where it was, not to the
//! origin. The same requirement covers maximise and fullscreen, which §3 says
//! are *neither* mode — they are departures from floating that have to be
//! undoable.
//!
//! State lives in the window's own `UserDataMap` rather than a side table in
//! the compositor. A side table must be pruned on unmap, and the failure when
//! it is not is a slow leak plus, eventually, a stale rectangle applied to some
//! unrelated window. Attached to the window, it dies with the window.
//!
//! # The guard that makes this work
//!
//! Geometry is recorded on every move and resize, because recording only on the
//! way *into* another mode loses the last drag. But recording unconditionally
//! is worse: maximising a window would overwrite the rectangle it is supposed
//! to return to with the maximised one, and "restore" would become "do
//! nothing". So a window is explicitly marked displaced, and while displaced
//! its floating rectangle is frozen.

use std::cell::{Cell, RefCell};

use smithay::desktop::{Space, Window};
use smithay::utils::{Logical, Rectangle};

#[derive(Debug, Default)]
pub struct FloatingGeometry {
    /// Where to return to. Only updated while the window is *not* displaced.
    floating: RefCell<Option<Rectangle<i32, Logical>>>,
    /// Set while the window is maximised, fullscreened or tiled.
    displaced: Cell<bool>,
    /// This window floats even in a tiled workspace.
    ///
    /// §3: "tiling must have a floating exception, or every file chooser
    /// becomes a tile." Kept separate from `displaced` because they answer
    /// different questions — `displaced` is where the window is right now,
    /// this is whether the layout is entitled to move it at all.
    exempt: Cell<bool>,
}

impl FloatingGeometry {
    /// Record the current rectangle as the floating one.
    ///
    /// Silently ignored while displaced. That is not defensive — it is the
    /// single rule the module exists to enforce, and callers legitimately call
    /// this on every geometry change without knowing the mode.
    pub fn remember(&self, rect: Rectangle<i32, Logical>) {
        if self.displaced.get() {
            return;
        }
        // A window that has not committed a buffer yet reports 0x0. Storing
        // that would restore it to nothing later, which looks like the window
        // vanishing rather than like a bad rectangle.
        if rect.size.w <= 0 || rect.size.h <= 0 {
            return;
        }
        *self.floating.borrow_mut() = Some(rect);
    }

    pub fn recall(&self) -> Option<Rectangle<i32, Logical>> {
        *self.floating.borrow()
    }

    pub fn displaced(&self) -> bool {
        self.displaced.get()
    }

    pub fn set_displaced(&self, value: bool) {
        self.displaced.set(value);
    }

    pub fn exempt(&self) -> bool {
        self.exempt.get()
    }

    pub fn set_exempt(&self, value: bool) {
        self.exempt.set(value);
    }
}

fn state(window: &Window) -> &FloatingGeometry {
    window.user_data().insert_if_missing(FloatingGeometry::default);
    window
        .user_data()
        .get::<FloatingGeometry>()
        .expect("just inserted")
}

/// Record a window's current rectangle as its floating one.
pub fn remember(space: &Space<Window>, window: &Window) {
    // A window with no location is not mapped; remembering (0,0) would later
    // restore it to the corner as though that had been chosen.
    let Some(loc) = space.element_location(window) else { return };
    state(window).remember(Rectangle::new(loc, window.geometry().size));
}

pub fn recall(window: &Window) -> Option<Rectangle<i32, Logical>> {
    state(window).recall()
}

pub fn is_displaced(window: &Window) -> bool {
    state(window).displaced()
}

pub fn set_displaced(window: &Window, value: bool) {
    state(window).set_displaced(value);
}

/// Whether the tiling layout must leave this window alone.
pub fn is_exempt(window: &Window) -> bool {
    state(window).exempt()
}

pub fn set_exempt(window: &Window, value: bool) {
    state(window).set_exempt(value);
}

#[cfg(test)]
mod tests {
    use super::*;
    use smithay::utils::{Point, Size};

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((x, y)), Size::from((w, h)))
    }

    #[test]
    fn nothing_is_remembered_until_something_is_recorded() {
        assert_eq!(FloatingGeometry::default().recall(), None);
    }

    #[test]
    fn the_most_recent_floating_rectangle_wins() {
        let g = FloatingGeometry::default();
        g.remember(rect(10, 10, 100, 100));
        g.remember(rect(50, 60, 300, 200));
        assert_eq!(g.recall(), Some(rect(50, 60, 300, 200)));
    }

    /// The rule the module exists for. Without the guard, maximising
    /// overwrites the rectangle to restore, and "unmaximise" does nothing
    /// visible — which reads as a broken keybinding, not a lost rectangle.
    #[test]
    fn displacement_freezes_the_remembered_rectangle() {
        let g = FloatingGeometry::default();
        let floating = rect(137, 42, 640, 480);
        g.remember(floating);

        g.set_displaced(true);
        g.remember(rect(0, 0, 1920, 1080)); // maximised — must not stick
        assert_eq!(g.recall(), Some(floating));

        g.set_displaced(false);
        assert_eq!(g.recall(), Some(floating), "restoring must return the original");
    }

    /// After coming back, ordinary drags must start being recorded again.
    #[test]
    fn recording_resumes_once_restored() {
        let g = FloatingGeometry::default();
        g.remember(rect(0, 0, 100, 100));
        g.set_displaced(true);
        g.remember(rect(0, 0, 1920, 1080));
        g.set_displaced(false);
        g.remember(rect(300, 300, 200, 200));
        assert_eq!(g.recall(), Some(rect(300, 300, 200, 200)));
    }

    #[test]
    fn an_uncommitted_window_is_not_remembered() {
        let g = FloatingGeometry::default();
        g.remember(rect(0, 0, 0, 0));
        assert_eq!(g.recall(), None, "0x0 would restore to an invisible window");
    }

    /// A real rectangle must survive a later 0x0 report rather than be erased.
    #[test]
    fn a_zero_size_does_not_clobber_a_real_one() {
        let g = FloatingGeometry::default();
        g.remember(rect(10, 10, 400, 300));
        g.remember(rect(0, 0, 0, 0));
        assert_eq!(g.recall(), Some(rect(10, 10, 400, 300)));
    }

    #[test]
    fn a_window_starts_undisplaced_and_unexempt() {
        let g = FloatingGeometry::default();
        assert!(!g.displaced());
        assert!(!g.exempt(), "windows tile by default; exemption is opt-in");
    }

    /// The two flags are independent. An exempt window is never displaced by
    /// the layout, but it can still be maximised — and conflating them would
    /// make maximising a dialog un-restorable.
    #[test]
    fn exemption_and_displacement_are_independent() {
        let g = FloatingGeometry::default();
        g.set_exempt(true);
        assert!(!g.displaced());
        g.set_displaced(true);
        assert!(g.exempt(), "displacing must not clear exemption");
    }
}
