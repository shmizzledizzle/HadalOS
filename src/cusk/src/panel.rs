//! The panel: a workspace indicator along the top edge.
//!
//! Drawn by the compositor rather than by a client, and that is a deliberate
//! limitation rather than an architecture. A panel client would want
//! `wlr-layer-shell` — to sit above windows and reserve space — and iced, which
//! the settings editor and launcher are built on, speaks xdg-shell only. The
//! choice was a compositor-drawn bar or a fake panel pretending to be an
//! ordinary window, and a fake would have to be undone later.
//!
//! # No text — and that reason expired
//!
//! This module used to say a window title and a clock were absent "because
//! cusk cannot draw a glyph: there is no font rasteriser in the compositor".
//! **That stopped being true when `text.rs` landed**, which says so in its own
//! first paragraph — "cusk could not draw a glyph until now, which is why
//! milestone 15's indicator is rectangles" — and `main.rs` already loads a
//! `text::Face` at startup.
//!
//! So the panel is bare for no current reason. Two modules disagreed about
//! what the compositor could do, and the one describing a limitation was the
//! one nobody revisited after removing it. This is the same shape as the
//! greeter row in ARCHITECTURE.md §0 and `/etc/os-release` reverting under
//! host-conversion.md §1: a state recorded once and then outlived.
//!
//! What is here — one pill per workspace, filled when occupied, accented when
//! active — remains right, and is exactly what `Super+1..9` was missing:
//! switching to an empty workspace is otherwise indistinguishable from the
//! compositor having hung. What is *missing* is a clock and a focused-window
//! title, both of which are now ordinary work rather than blocked work.
//!
//! Note what `text.rs` will and will not give the panel, since it is fontdue
//! rather than a shaping engine: Latin, Greek and Cyrillic render correctly;
//! Arabic and Devanagari come out visibly wrong rather than absent. A window
//! title is exactly the string most likely to be in a script that needs
//! shaping, so the title is the piece of this that wants `cosmic-text` and the
//! clock is the piece that does not.
//!
//! Everything in this module is pure geometry, so where the pills are and what
//! a click lands on can be tested without a display.

use smithay::utils::{Logical, Point, Rectangle, Size};

/// Space between pills, and from the panel edge.
const GAP: i32 = 6;
/// How much narrower a pill is than it is tall, before the active one grows.
const PILL_WIDTH: i32 = 22;
/// The active pill is wider, so the current workspace is identifiable by shape
/// alone. Colour carries it too, but shape survives a bad monitor, a
/// colourblind user, and a screenshot at low contrast.
const ACTIVE_WIDTH: i32 = 34;

/// The area left for windows once the panel has taken its share.
///
/// Subtracting here rather than at each use is what keeps tiling, floating
/// placement and maximise agreeing about where the usable screen starts. Three
/// separate subtractions would eventually disagree by a pixel, and the symptom
/// would be a window tucked one row under the bar.
pub fn usable_area(output: Size<i32, Logical>, panel_height: i32) -> Rectangle<i32, Logical> {
    let height = panel_height.clamp(0, output.h);
    Rectangle::new(
        Point::from((0, height)),
        Size::from((output.w, (output.h - height).max(1))),
    )
}

/// The panel's own rectangle, given the horizontal span it is allowed.
///
/// `span` is the part of the width no layer-shell client has claimed. A
/// full-width bar drawn beside a right-hand dock overlaps it in the corner —
/// and, because the panel is painted last and swallows clicks first, the dock's
/// topmost icon becomes both hidden and unclickable.
///
/// The panel takes what is left rather than the dock giving way: an exclusive
/// zone is a client's reservation, and a compositor that drew over one would
/// be breaking the promise it asks clients to rely on.
pub fn panel_area(
    output: Size<i32, Logical>,
    panel_height: i32,
    span: (i32, i32),
) -> Rectangle<i32, Logical> {
    let (x, width) = span;
    Rectangle::new(
        Point::from((x.clamp(0, output.w), 0)),
        Size::from((width.clamp(0, output.w), panel_height.clamp(0, output.h))),
    )
}

/// The whole width, for callers with no layer surfaces to consider.
#[allow(dead_code)]
pub fn full_span(output: Size<i32, Logical>) -> (i32, i32) {
    (0, output.w)
}

/// Where each workspace pill sits, left to right.
pub fn pills(
    output: Size<i32, Logical>,
    panel_height: i32,
    count: usize,
    active: usize,
) -> Vec<Rectangle<i32, Logical>> {
    if count == 0 || panel_height <= 0 {
        return Vec::new();
    }
    let height = (panel_height - GAP * 2).max(2);
    let y = (panel_height - height) / 2;

    let mut out = Vec::with_capacity(count);
    let mut x = GAP * 2;
    for index in 0..count {
        let width = if index == active { ACTIVE_WIDTH } else { PILL_WIDTH };
        out.push(Rectangle::new(
            Point::from((x, y)),
            Size::from((width, height)),
        ));
        x += width + GAP;
    }

    // Off the right-hand edge means a screen too narrow for this many
    // workspaces. Returning them anyway would draw pills nobody can see or
    // click; better to draw none than to draw a lie.
    if x > output.w {
        return Vec::new();
    }
    out
}

/// Which pill a point is inside, if any.
pub fn pill_at(pills: &[Rectangle<i32, Logical>], point: Point<i32, Logical>) -> Option<usize> {
    pills.iter().position(|p| p.contains(point))
}

/// Whether a point is on the panel at all.
pub fn contains(
    output: Size<i32, Logical>,
    panel_height: i32,
    span: (i32, i32),
    point: Point<i32, Logical>,
) -> bool {
    panel_height > 0 && panel_area(output, panel_height, span).contains(point)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> Size<i32, Logical> {
        Size::from((1280, 800))
    }

    #[test]
    fn the_panel_takes_its_height_off_the_top() {
        let usable = usable_area(screen(), 28);
        assert_eq!(usable.loc, Point::from((0, 28)));
        assert_eq!(usable.size, Size::from((1280, 772)));
    }

    /// Together they must cover the output exactly, or there is a strip that
    /// belongs to neither and nothing ever draws in it.
    #[test]
    fn the_panel_and_the_usable_area_tile_the_output() {
        for height in [0, 1, 24, 28, 60] {
            let panel = panel_area(screen(), height, full_span(screen()));
            let usable = usable_area(screen(), height);
            assert_eq!(panel.size.h + usable.size.h, 800, "height {height}");
            assert_eq!(panel.loc.y + panel.size.h, usable.loc.y, "height {height}");
        }
    }

    /// A panel taller than the screen must not produce a negative or zero
    /// usable area — the layout divides by it.
    #[test]
    fn an_absurd_panel_height_still_leaves_somewhere_to_put_windows() {
        let usable = usable_area(screen(), 100_000);
        assert!(usable.size.w > 0 && usable.size.h > 0);
    }

    #[test]
    fn there_is_one_pill_per_workspace() {
        assert_eq!(pills(screen(), 28, 4, 0).len(), 4);
        assert_eq!(pills(screen(), 28, 9, 3).len(), 9);
        assert!(pills(screen(), 28, 0, 0).is_empty());
    }

    /// Shape, not just colour, marks the active workspace — that survives a
    /// bad monitor and a colourblind user.
    #[test]
    fn the_active_pill_is_wider_than_the_others() {
        let pills = pills(screen(), 28, 4, 2);
        assert!(pills[2].size.w > pills[0].size.w);
        assert_eq!(pills[0].size.w, pills[1].size.w);
        assert_eq!(pills[1].size.w, pills[3].size.w);
    }

    /// Overlapping pills would make one unclickable, and the click would land
    /// on whichever the search happened to reach first.
    #[test]
    fn pills_never_overlap_and_run_left_to_right() {
        for active in 0..6 {
            let pills = pills(screen(), 28, 6, active);
            for pair in pills.windows(2) {
                assert!(
                    pair[0].loc.x + pair[0].size.w < pair[1].loc.x,
                    "active {active}: {:?} touches {:?}",
                    pair[0],
                    pair[1]
                );
            }
        }
    }

    #[test]
    fn pills_stay_inside_the_panel() {
        let height = 28;
        let panel = panel_area(screen(), height, full_span(screen()));
        for pill in pills(screen(), height, 5, 1) {
            assert!(panel.contains_rect(pill), "{pill:?} escapes {panel:?}");
        }
    }

    /// Drawing pills that run off the edge would show some that cannot be
    /// clicked, which is worse than showing none.
    #[test]
    fn a_screen_too_narrow_for_the_pills_gets_none() {
        let narrow = Size::from((100, 800));
        assert!(pills(narrow, 28, 9, 0).is_empty());
    }

    #[test]
    fn a_click_finds_the_pill_it_is_inside() {
        let pills = pills(screen(), 28, 4, 0);
        for (index, pill) in pills.iter().enumerate() {
            let centre = Point::from((
                pill.loc.x + pill.size.w / 2,
                pill.loc.y + pill.size.h / 2,
            ));
            assert_eq!(pill_at(&pills, centre), Some(index));
        }
    }

    /// The gaps between pills belong to no workspace. Snapping to the nearest
    /// would make a click on empty panel switch workspace unexpectedly.
    #[test]
    fn a_click_between_pills_hits_nothing() {
        let pills = pills(screen(), 28, 4, 0);
        let between = Point::from((pills[0].loc.x + pills[0].size.w + 2, 14));
        assert_eq!(pill_at(&pills, between), None);
        assert_eq!(pill_at(&pills, Point::from((900, 14))), None, "empty panel");
    }

    #[test]
    fn the_panel_owns_the_top_strip_and_nothing_below_it() {
        let all = full_span(screen());
        assert!(contains(screen(), 28, all, Point::from((640, 0))));
        assert!(contains(screen(), 28, all, Point::from((640, 27))));
        assert!(!contains(screen(), 28, all, Point::from((640, 28))));
        assert!(!contains(screen(), 0, all, Point::from((640, 0))), "disabled owns nothing");

        // The strip a dock has reserved is the dock's, including the corner.
        let beside_dock = (0, 1224);
        assert!(contains(screen(), 28, beside_dock, Point::from((640, 10))));
        assert!(
            !contains(screen(), 28, beside_dock, Point::from((1250, 10))),
            "the panel must not own the dock's corner, or its top icon is unclickable"
        );
    }
}
