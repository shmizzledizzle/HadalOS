//! Floating mode: pointer focus, click-to-raise, and interactive move/resize.
//!
//! `docs/cusk.md` §3 calls floating the mode where "position and size are the
//! window's own". This is that policy — and, per §3, it is also what tiling
//! will need to make exceptions to, so the geometry it records is deliberately
//! kept even for windows that are not currently floating.
//!
//! Both grabs implement `PointerGrab`, which means smithay routes every pointer
//! event to them for the duration and restores normal focus afterwards. The
//! alternative — a mode flag checked in the main event loop — loses the pointer
//! the first time it leaves the window being dragged, which is exactly when a
//! drag matters.

use smithay::desktop::{Space, Window};
use smithay::input::pointer::{
    AxisFrame, ButtonEvent, GrabStartData, MotionEvent, PointerGrab, PointerInnerHandle,
    RelativeMotionEvent,
};
use smithay::input::pointer::{
    GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent, GesturePinchEndEvent,
    GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent,
};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::utils::{Logical, Point, Rectangle, Size};

use crate::Cusk;

/// Left mouse button, as libinput reports it.
pub const BTN_LEFT: u32 = 0x110;
/// Right mouse button.
pub const BTN_RIGHT: u32 = 0x111;

/// Smallest window an interactive resize will produce.
///
/// Not a preference. A window dragged to zero size cannot be grabbed again,
/// because there is nothing left to put the pointer on — the user has to kill
/// the client to recover it.
const MIN_SIZE: (i32, i32) = (120, 60);

// ── move ─────────────────────────────────────────────────────────────────

pub struct MoveGrab {
    pub start_data: GrabStartData<Cusk>,
    pub window: Window,
    /// Where the window was when the grab began. Motion is applied as a delta
    /// from the grab origin rather than centring the window on the pointer,
    /// so the window does not jump on the first pixel of movement.
    pub initial_window_location: Point<i32, Logical>,
}

impl PointerGrab<Cusk> for MoveGrab {
    fn motion(
        &mut self,
        data: &mut Cusk,
        handle: &mut PointerInnerHandle<'_, Cusk>,
        _focus: Option<(<Cusk as smithay::input::SeatHandler>::PointerFocus, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        // Focus is forced to None for the duration: during a drag the pointer
        // belongs to the compositor, and letting it enter whatever it passes
        // over would send enter/leave storms to unrelated clients.
        handle.motion(data, None, event);

        let delta = event.location - self.start_data.location;
        let location = self.initial_window_location.to_f64() + delta;
        data.space
            .map_element(self.window.clone(), location.to_i32_round(), true);
    }

    fn relative_motion(
        &mut self,
        data: &mut Cusk,
        handle: &mut PointerInnerHandle<'_, Cusk>,
        _focus: Option<(<Cusk as smithay::input::SeatHandler>::PointerFocus, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        handle.relative_motion(data, None, event);
    }

    fn button(
        &mut self,
        data: &mut Cusk,
        handle: &mut PointerInnerHandle<'_, Cusk>,
        event: &ButtonEvent,
    ) {
        handle.button(data, event);
        // Release of the button that started the grab ends it. Checking the
        // specific button matters: a second button pressed mid-drag must not
        // cancel the drag when it is released.
        if !handle.current_pressed().contains(&self.start_data.button) {
            // The drag is the geometry change; record it now that it has
            // settled. Recording per-motion would be the same value hundreds
            // of times, and would still be wrong if the grab is cancelled.
            crate::geometry::remember(&data.space, &self.window);
            handle.unset_grab(self, data, event.serial, event.time, true);
        }
    }

    fn axis(&mut self, data: &mut Cusk, handle: &mut PointerInnerHandle<'_, Cusk>, details: AxisFrame) {
        handle.axis(data, details)
    }
    fn frame(&mut self, data: &mut Cusk, handle: &mut PointerInnerHandle<'_, Cusk>) {
        handle.frame(data)
    }
    fn start_data(&self) -> &GrabStartData<Cusk> {
        &self.start_data
    }
    fn unset(&mut self, _data: &mut Cusk) {}

    fn gesture_swipe_begin(&mut self, d: &mut Cusk, h: &mut PointerInnerHandle<'_, Cusk>, e: &GestureSwipeBeginEvent) { h.gesture_swipe_begin(d, e) }
    fn gesture_swipe_update(&mut self, d: &mut Cusk, h: &mut PointerInnerHandle<'_, Cusk>, e: &GestureSwipeUpdateEvent) { h.gesture_swipe_update(d, e) }
    fn gesture_swipe_end(&mut self, d: &mut Cusk, h: &mut PointerInnerHandle<'_, Cusk>, e: &GestureSwipeEndEvent) { h.gesture_swipe_end(d, e) }
    fn gesture_pinch_begin(&mut self, d: &mut Cusk, h: &mut PointerInnerHandle<'_, Cusk>, e: &GesturePinchBeginEvent) { h.gesture_pinch_begin(d, e) }
    fn gesture_pinch_update(&mut self, d: &mut Cusk, h: &mut PointerInnerHandle<'_, Cusk>, e: &GesturePinchUpdateEvent) { h.gesture_pinch_update(d, e) }
    fn gesture_pinch_end(&mut self, d: &mut Cusk, h: &mut PointerInnerHandle<'_, Cusk>, e: &GesturePinchEndEvent) { h.gesture_pinch_end(d, e) }
    fn gesture_hold_begin(&mut self, d: &mut Cusk, h: &mut PointerInnerHandle<'_, Cusk>, e: &GestureHoldBeginEvent) { h.gesture_hold_begin(d, e) }
    fn gesture_hold_end(&mut self, d: &mut Cusk, h: &mut PointerInnerHandle<'_, Cusk>, e: &GestureHoldEndEvent) { h.gesture_hold_end(d, e) }
}

// ── resize ───────────────────────────────────────────────────────────────

pub struct ResizeGrab {
    pub start_data: GrabStartData<Cusk>,
    pub window: Window,
    pub edges: xdg_toplevel::ResizeEdge,
    pub initial_rect: Rectangle<i32, Logical>,
    /// The size most recently sent to the client. A resize is a negotiation:
    /// the compositor asks, the client answers with a commit, and the answer
    /// may differ. Tracking what was asked keeps the two from oscillating.
    pub last_requested: Size<i32, Logical>,
}

impl PointerGrab<Cusk> for ResizeGrab {
    fn motion(
        &mut self,
        data: &mut Cusk,
        handle: &mut PointerInnerHandle<'_, Cusk>,
        _focus: Option<(<Cusk as smithay::input::SeatHandler>::PointerFocus, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        handle.motion(data, None, event);

        let delta = event.location - self.start_data.location;
        let (dx, dy) = (delta.x as i32, delta.y as i32);

        // Which edge is dragged decides the sign, and whether the opposite
        // corner has to move to keep it still.
        let mut new = self.initial_rect;
        use xdg_toplevel::ResizeEdge as E;
        let e = self.edges;
        if matches!(e, E::Right | E::TopRight | E::BottomRight) {
            new.size.w = self.initial_rect.size.w + dx;
        } else if matches!(e, E::Left | E::TopLeft | E::BottomLeft) {
            new.size.w = self.initial_rect.size.w - dx;
            new.loc.x = self.initial_rect.loc.x + dx;
        }
        if matches!(e, E::Bottom | E::BottomLeft | E::BottomRight) {
            new.size.h = self.initial_rect.size.h + dy;
        } else if matches!(e, E::Top | E::TopLeft | E::TopRight) {
            new.size.h = self.initial_rect.size.h - dy;
            new.loc.y = self.initial_rect.loc.y + dy;
        }

        // Clamp the size, then correct the origin. Clamping alone would let a
        // left-edge drag keep moving the window after it stopped shrinking.
        if new.size.w < MIN_SIZE.0 {
            if new.loc.x != self.initial_rect.loc.x {
                new.loc.x = self.initial_rect.loc.x + (self.initial_rect.size.w - MIN_SIZE.0);
            }
            new.size.w = MIN_SIZE.0;
        }
        if new.size.h < MIN_SIZE.1 {
            if new.loc.y != self.initial_rect.loc.y {
                new.loc.y = self.initial_rect.loc.y + (self.initial_rect.size.h - MIN_SIZE.1);
            }
            new.size.h = MIN_SIZE.1;
        }

        if new.size != self.last_requested {
            if let Some(toplevel) = self.window.toplevel() {
                toplevel.with_pending_state(|state| state.size = Some(new.size));
                toplevel.send_pending_configure();
            }
            self.last_requested = new.size;
        }
        data.space.map_element(self.window.clone(), new.loc, true);
    }

    fn relative_motion(
        &mut self,
        data: &mut Cusk,
        handle: &mut PointerInnerHandle<'_, Cusk>,
        _focus: Option<(<Cusk as smithay::input::SeatHandler>::PointerFocus, Point<f64, Logical>)>,
        event: &RelativeMotionEvent,
    ) {
        handle.relative_motion(data, None, event);
    }

    fn button(
        &mut self,
        data: &mut Cusk,
        handle: &mut PointerInnerHandle<'_, Cusk>,
        event: &ButtonEvent,
    ) {
        handle.button(data, event);
        if !handle.current_pressed().contains(&self.start_data.button) {
            // The drag is the geometry change; record it now that it has
            // settled. Recording per-motion would be the same value hundreds
            // of times, and would still be wrong if the grab is cancelled.
            crate::geometry::remember(&data.space, &self.window);
            handle.unset_grab(self, data, event.serial, event.time, true);
        }
    }

    fn axis(&mut self, data: &mut Cusk, handle: &mut PointerInnerHandle<'_, Cusk>, details: AxisFrame) {
        handle.axis(data, details)
    }
    fn frame(&mut self, data: &mut Cusk, handle: &mut PointerInnerHandle<'_, Cusk>) {
        handle.frame(data)
    }
    fn start_data(&self) -> &GrabStartData<Cusk> {
        &self.start_data
    }
    fn unset(&mut self, _data: &mut Cusk) {}

    fn gesture_swipe_begin(&mut self, d: &mut Cusk, h: &mut PointerInnerHandle<'_, Cusk>, e: &GestureSwipeBeginEvent) { h.gesture_swipe_begin(d, e) }
    fn gesture_swipe_update(&mut self, d: &mut Cusk, h: &mut PointerInnerHandle<'_, Cusk>, e: &GestureSwipeUpdateEvent) { h.gesture_swipe_update(d, e) }
    fn gesture_swipe_end(&mut self, d: &mut Cusk, h: &mut PointerInnerHandle<'_, Cusk>, e: &GestureSwipeEndEvent) { h.gesture_swipe_end(d, e) }
    fn gesture_pinch_begin(&mut self, d: &mut Cusk, h: &mut PointerInnerHandle<'_, Cusk>, e: &GesturePinchBeginEvent) { h.gesture_pinch_begin(d, e) }
    fn gesture_pinch_update(&mut self, d: &mut Cusk, h: &mut PointerInnerHandle<'_, Cusk>, e: &GesturePinchUpdateEvent) { h.gesture_pinch_update(d, e) }
    fn gesture_pinch_end(&mut self, d: &mut Cusk, h: &mut PointerInnerHandle<'_, Cusk>, e: &GesturePinchEndEvent) { h.gesture_pinch_end(d, e) }
    fn gesture_hold_begin(&mut self, d: &mut Cusk, h: &mut PointerInnerHandle<'_, Cusk>, e: &GestureHoldBeginEvent) { h.gesture_hold_begin(d, e) }
    fn gesture_hold_end(&mut self, d: &mut Cusk, h: &mut PointerInnerHandle<'_, Cusk>, e: &GestureHoldEndEvent) { h.gesture_hold_end(d, e) }
}

/// The window's current outer rectangle, as the compositor sees it.
pub fn window_rect(space: &Space<Window>, window: &Window) -> Rectangle<i32, Logical> {
    let loc = space.element_location(window).unwrap_or_default();
    Rectangle::new(loc, window.geometry().size)
}

/// Which edge a pointer is nearest, for a compositor-initiated resize.
///
/// Corner-first: the diagonal regions overlap the straight ones, and testing
/// them in the other order makes corners unreachable — a resize that only ever
/// moves one axis feels broken long before anyone works out why.
pub fn nearest_edge(
    rect: Rectangle<i32, Logical>,
    point: Point<f64, Logical>,
) -> xdg_toplevel::ResizeEdge {
    use xdg_toplevel::ResizeEdge as E;
    let rel_x = point.x - rect.loc.x as f64;
    let rel_y = point.y - rect.loc.y as f64;
    let left = rel_x < rect.size.w as f64 / 2.0;
    let top = rel_y < rect.size.h as f64 / 2.0;
    match (left, top) {
        (true, true) => E::TopLeft,
        (false, true) => E::TopRight,
        (true, false) => E::BottomLeft,
        (false, false) => E::BottomRight,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xdg_toplevel::ResizeEdge as E;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((x, y)), Size::from((w, h)))
    }

    #[test]
    fn quadrants_map_to_corners() {
        let r = rect(100, 100, 200, 100);
        assert_eq!(nearest_edge(r, Point::from((110.0, 110.0))), E::TopLeft);
        assert_eq!(nearest_edge(r, Point::from((290.0, 110.0))), E::TopRight);
        assert_eq!(nearest_edge(r, Point::from((110.0, 190.0))), E::BottomLeft);
        assert_eq!(nearest_edge(r, Point::from((290.0, 190.0))), E::BottomRight);
    }

    /// The window may be anywhere; the quadrant is relative to it, not to the
    /// output. Testing at the origin would pass with absolute coordinates.
    #[test]
    fn quadrants_are_relative_to_the_window() {
        let r = rect(1000, 500, 200, 100);
        assert_eq!(nearest_edge(r, Point::from((1010.0, 510.0))), E::TopLeft);
        assert_eq!(nearest_edge(r, Point::from((1190.0, 590.0))), E::BottomRight);
    }
}
