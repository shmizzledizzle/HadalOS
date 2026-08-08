//! Tiled-mode pointer grabs.
//!
//! In floating mode a drag moves a window, because in floating mode position
//! *is* the window's own. In tiled mode position is computed, so a drag that
//! moved the window would put the compositor's layout and the screen into
//! disagreement — every tile would still be recorded where the layout put it
//! while being drawn somewhere else, and the next relayout would snap it back
//! with no explanation.
//!
//! So the same gestures mean different things per §3's "two policies over the
//! same window set":
//!
//! - **Drag** reorders. The window swaps places with whatever tile it is
//!   dropped on, which is how dynamic tilers let you rearrange without
//!   inventing coordinates.
//! - **Resize** drags the master divider, changing the ratio rather than one
//!   window's size. A tile cannot be resized alone — its neighbours have to
//!   give up the space.

use smithay::desktop::Window;
use smithay::input::pointer::{
    AxisFrame, ButtonEvent, GrabStartData, MotionEvent, PointerGrab, PointerInnerHandle,
    RelativeMotionEvent,
};
use smithay::input::pointer::{
    GestureHoldBeginEvent, GestureHoldEndEvent, GesturePinchBeginEvent, GesturePinchEndEvent,
    GesturePinchUpdateEvent, GestureSwipeBeginEvent, GestureSwipeEndEvent, GestureSwipeUpdateEvent,
};
use smithay::utils::{Logical, Point, Rectangle, Size};

use crate::layout;
use crate::Cusk;

/// Drag a tile onto another to swap the two.
pub struct SwapGrab {
    pub start_data: GrabStartData<Cusk>,
    pub window: Window,
}

impl SwapGrab {
    /// Swap on release, not on hover.
    ///
    /// Swapping continuously as the pointer crosses tiles makes the layout
    /// churn under the drag and the window chase the pointer through every
    /// intermediate arrangement. Committing once on drop means the gesture has
    /// exactly one outcome.
    fn commit(&self, data: &mut Cusk, at: Point<f64, Logical>) {
        let windows = data.tiled();
        let area = Rectangle::new(
            Point::from((0, 0)),
            Size::from((data.output_size.0, data.output_size.1)),
        );
        let tiles = data.layout.arrange(area, windows.len(), data.gaps);

        let Some(target) = layout::index_at(&tiles, at.to_i32_round()) else {
            return;
        };
        let Some(source) = windows.iter().position(|w| w == &self.window) else {
            return;
        };
        if source == target {
            return;
        }

        // Swap in `order`, which is what the layout reads. Swapping in the
        // filtered `tiled()` list would discard the result, since that list is
        // a copy rebuilt on every call.
        let (Some(a), Some(b)) = (
            data.order.iter().position(|w| w == &windows[source]),
            data.order.iter().position(|w| w == &windows[target]),
        ) else {
            return;
        };
        data.order.swap(a, b);
        tracing::info!("swapped tiles {source} and {target}");
        data.relayout();
    }
}

impl PointerGrab<Cusk> for SwapGrab {
    fn motion(
        &mut self,
        data: &mut Cusk,
        handle: &mut PointerInnerHandle<'_, Cusk>,
        _focus: Option<(<Cusk as smithay::input::SeatHandler>::PointerFocus, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        // The window deliberately does not follow the pointer. Its position is
        // the layout's to decide, and moving it here would be the exact
        // desynchronisation this grab exists to prevent.
        handle.motion(data, None, event);
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
            let at = data.pointer_location;
            self.commit(data, at);
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

/// Drag the master/stack divider.
pub struct RatioGrab {
    pub start_data: GrabStartData<Cusk>,
}

impl PointerGrab<Cusk> for RatioGrab {
    fn motion(
        &mut self,
        data: &mut Cusk,
        handle: &mut PointerInnerHandle<'_, Cusk>,
        _focus: Option<(<Cusk as smithay::input::SeatHandler>::PointerFocus, Point<f64, Logical>)>,
        event: &MotionEvent,
    ) {
        handle.motion(data, None, event);

        // Live rather than on release: the divider is the one thing in the
        // layout the user is aiming at directly, and aiming without seeing
        // where it lands is guesswork.
        let area = Rectangle::new(
            Point::from((0, 0)),
            Size::from((data.output_size.0, data.output_size.1)),
        );
        let ratio = layout::ratio_at(area, event.location.to_i32_round().x, data.gaps);
        if let layout::Layout::MasterStack { .. } = data.layout {
            data.layout = layout::Layout::MasterStack { ratio };
            data.relayout();
        }
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
