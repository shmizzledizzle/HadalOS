//! The layout engine.
//!
//! `docs/cusk.md` §3: "Not two window managers behind a switch. One layout
//! engine with two policies over the same window set, so a window does not
//! change identity when the mode does."
//!
//! That is why this module knows nothing about Wayland. `arrange` is a pure
//! function from an area and a window count to a list of rectangles. The
//! compositor decides which windows participate and applies the result; the
//! geometry itself is decided here, where it can be tested exhaustively
//! without a display, a client, or an event loop.
//!
//! Floating is deliberately *not* one of these. A floating window's rectangle
//! is its own — there is nothing to compute — so floating is the absence of an
//! arrangement rather than an arrangement that happens to be identity. Adding
//! a `Layout::Floating` variant here would force every caller to ask whether
//! the returned rectangles mean anything.

use smithay::utils::{Logical, Point, Rectangle, Size};

/// Space between tiles, and between tiles and the screen edge.
///
/// Hardcoded for now. §4 makes these schema settings; they are grouped in a
/// struct so that when the schema lands there is one place to feed, rather
/// than a scattering of literals to hunt down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gaps {
    pub inner: i32,
    pub outer: i32,
}

impl Default for Gaps {
    fn default() -> Self {
        Self { inner: 8, outer: 8 }
    }
}

/// How tiled windows divide a workspace.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Layout {
    /// One window takes a fraction of the width; the rest share a column.
    /// The familiar dwm/Hyprland arrangement, and the one that behaves
    /// sensibly at n=1 and n=2 without special cases.
    MasterStack { ratio: f64 },
    /// Equal-width columns. Included because a second layout is the only
    /// thing that proves the engine is an engine and not one hardcoded rule.
    Columns,
}

impl Default for Layout {
    fn default() -> Self {
        Self::MasterStack { ratio: 0.6 }
    }
}

/// Floor for the *master column* width, so an extreme ratio cannot starve
/// either side to nothing.
///
/// Deliberately not applied to individual stack tiles. Clamping each tile up
/// while advancing by the clamped size makes the column overflow its area and
/// the tiles overlap — worse than small tiles, and invisible in tests unless
/// overlap and minimum are checked on the same inputs. Tiles shrink instead:
/// visibly cramped is honest, silently stacked is not.
pub const MIN_MASTER: i32 = 80;

impl Layout {
    pub fn name(self) -> &'static str {
        match self {
            Layout::MasterStack { .. } => "master-stack",
            Layout::Columns => "columns",
        }
    }

    /// Cycle through layouts, for a keybinding.
    pub fn next(self) -> Self {
        match self {
            Layout::MasterStack { .. } => Layout::Columns,
            Layout::Columns => Layout::default(),
        }
    }

    /// Adjust the master fraction. Clamped rather than wrapped: a ratio that
    /// wraps from 0.9 to 0.1 on one extra keypress reads as a glitch.
    pub fn widen(self, delta: f64) -> Self {
        match self {
            Layout::MasterStack { ratio } => Layout::MasterStack {
                ratio: (ratio + delta).clamp(0.1, 0.9),
            },
            other => other,
        }
    }

    /// Divide `area` among `n` windows.
    ///
    /// Returns exactly `n` rectangles, in the same order as the windows they
    /// are for. `n == 0` returns empty.
    pub fn arrange(self, area: Rectangle<i32, Logical>, n: usize, gaps: Gaps) -> Vec<Rectangle<i32, Logical>> {
        if n == 0 {
            return Vec::new();
        }

        // The outer gap applies once, to the whole area; the inner gap then
        // applies between tiles. Insetting up front means the split maths
        // below never has to know about screen edges.
        let area = inset(area, gaps.outer);

        match self {
            Layout::MasterStack { ratio } => master_stack(area, n, ratio, gaps.inner),
            Layout::Columns => split(area, n, gaps.inner, Axis::Horizontal),
        }
    }
}

/// Which tile contains `point`, if any.
///
/// Used to decide what a drag in tiled mode swaps with. Returns an index into
/// the same slice `arrange` produced, so the caller can map it straight back
/// to a window without a second lookup that could disagree.
pub fn index_at(
    tiles: &[Rectangle<i32, Logical>],
    point: Point<i32, Logical>,
) -> Option<usize> {
    tiles.iter().position(|t| t.contains(point))
}

/// The master ratio implied by dragging the master/stack divider to `x`.
///
/// Clamped to the same range as `widen`, so dragging past the edge parks the
/// divider rather than inverting the layout.
pub fn ratio_at(area: Rectangle<i32, Logical>, x: i32, gaps: Gaps) -> f64 {
    let inner = area.size.w - gaps.outer * 2;
    if inner <= 0 {
        return 0.5;
    }
    let offset = (x - area.loc.x - gaps.outer) as f64;
    (offset / inner as f64).clamp(0.1, 0.9)
}

/// Step an index through `len` items, wrapping at both ends.
///
/// Wrapping rather than clamping, for both focus and reordering. Clamping
/// makes the first and last positions dead ends: the key stops responding and
/// there is no way to tell a binding that did nothing from a binding that is
/// not bound. Wrapping always moves something, so the gesture is always
/// legible.
pub fn step(len: usize, from: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    let len_i = len as isize;
    // Rust's % keeps the sign of the dividend, so a negative delta would give
    // a negative index. The extra +len_i before the second % is what makes
    // stepping backwards from 0 land on the last element rather than panic.
    (((from as isize + delta) % len_i + len_i) % len_i) as usize
}

#[derive(Clone, Copy)]
enum Axis {
    Horizontal,
    Vertical,
}

fn master_stack(
    area: Rectangle<i32, Logical>,
    n: usize,
    ratio: f64,
    gap: i32,
) -> Vec<Rectangle<i32, Logical>> {
    // A single window takes everything. Not a special case for its own sake:
    // splitting off an empty stack column would leave the master at 60% width
    // with dead space beside it, which looks like a bug every time.
    if n == 1 {
        return vec![area];
    }

    let master_w = ((area.size.w - gap) as f64 * ratio).round() as i32;
    let master_w = master_w.clamp(MIN_MASTER, (area.size.w - gap - MIN_MASTER).max(MIN_MASTER));
    let stack_w = area.size.w - master_w - gap;

    let master = Rectangle::new(area.loc, Size::from((master_w, area.size.h)));
    let stack_area = Rectangle::new(
        Point::from((area.loc.x + master_w + gap, area.loc.y)),
        Size::from((stack_w, area.size.h)),
    );

    let mut out = vec![master];
    out.extend(split(stack_area, n - 1, gap, Axis::Vertical));
    out
}

/// Divide `area` into `n` equal parts along `axis`, separated by `gap`.
///
/// Remainder pixels are distributed one per tile rather than dumped on the
/// last one, so a 3-way split of an odd width does not leave one tile visibly
/// wider. This is also what keeps the tiles exactly filling the area: the sum
/// of parts plus gaps equals the whole, with no off-by-n seam at the edge.
fn split(
    area: Rectangle<i32, Logical>,
    n: usize,
    gap: i32,
    axis: Axis,
) -> Vec<Rectangle<i32, Logical>> {
    if n == 0 {
        return Vec::new();
    }
    let n_i = n as i32;
    let total = match axis {
        Axis::Horizontal => area.size.w,
        Axis::Vertical => area.size.h,
    };
    let usable = total - gap * (n_i - 1);
    let each = usable / n_i;
    let mut remainder = usable % n_i;

    let mut out = Vec::with_capacity(n);
    let mut offset = 0;
    for _ in 0..n {
        let mut len = each;
        if remainder > 0 {
            len += 1;
            remainder -= 1;
        }
        // Floor of 1, not of MIN_MASTER: a zero-sized configure is degenerate,
        // but anything above that must be honoured exactly or the tiles stop
        // tiling the area they were given.
        let len = len.max(1);
        out.push(match axis {
            Axis::Horizontal => Rectangle::new(
                Point::from((area.loc.x + offset, area.loc.y)),
                Size::from((len, area.size.h)),
            ),
            Axis::Vertical => Rectangle::new(
                Point::from((area.loc.x, area.loc.y + offset)),
                Size::from((area.size.w, len)),
            ),
        });
        offset += len + gap;
    }
    out
}

fn inset(r: Rectangle<i32, Logical>, by: i32) -> Rectangle<i32, Logical> {
    Rectangle::new(
        Point::from((r.loc.x + by, r.loc.y + by)),
        Size::from(((r.size.w - by * 2).max(1), (r.size.h - by * 2).max(1))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((0, 0)), Size::from((1920, 1080)))
    }

    fn no_gaps() -> Gaps {
        Gaps { inner: 0, outer: 0 }
    }

    #[test]
    fn no_windows_no_rectangles() {
        assert!(Layout::default().arrange(screen(), 0, Gaps::default()).is_empty());
    }

    #[test]
    fn every_window_gets_exactly_one_rectangle() {
        for n in 1..=12 {
            for layout in [Layout::default(), Layout::Columns] {
                assert_eq!(
                    layout.arrange(screen(), n, Gaps::default()).len(),
                    n,
                    "{} with {n} windows",
                    layout.name()
                );
            }
        }
    }

    /// A lone window should not be squeezed into the master fraction with
    /// empty space beside it.
    #[test]
    fn a_single_window_fills_the_area() {
        let r = Layout::default().arrange(screen(), 1, no_gaps());
        assert_eq!(r[0], screen());
    }

    #[test]
    fn master_takes_its_ratio_of_the_width() {
        let r = Layout::MasterStack { ratio: 0.6 }.arrange(screen(), 2, no_gaps());
        assert_eq!(r[0].size.w, 1152, "60% of 1920");
        assert_eq!(r[1].size.w, 768);
        assert_eq!(r[1].loc.x, 1152, "stack begins where master ends");
    }

    #[test]
    fn the_stack_divides_the_remaining_height() {
        let r = Layout::MasterStack { ratio: 0.5 }.arrange(screen(), 4, no_gaps());
        assert_eq!(r[0].size.h, 1080, "master spans full height");
        for tile in &r[1..] {
            assert_eq!(tile.size.h, 360, "three stack tiles share 1080");
        }
    }

    /// The property that matters most: tiles must not overlap, or windows
    /// obscure each other and tiling stops being tiling.
    #[test]
    fn tiles_never_overlap() {
        for n in 1..=8 {
            for layout in [
                Layout::MasterStack { ratio: 0.6 },
                Layout::MasterStack { ratio: 0.3 },
                Layout::Columns,
            ] {
                let tiles = layout.arrange(screen(), n, Gaps::default());
                for (i, a) in tiles.iter().enumerate() {
                    for b in &tiles[i + 1..] {
                        assert!(
                            a.intersection(*b).is_none(),
                            "{} n={n}: {a:?} overlaps {b:?}",
                            layout.name()
                        );
                    }
                }
            }
        }
    }

    /// With no gaps the tiles should account for every pixel. A split that
    /// dumps its remainder produces a visible seam at one edge.
    #[test]
    fn tiles_exactly_fill_the_area() {
        for n in 1..=7 {
            let tiles = Layout::Columns.arrange(screen(), n, no_gaps());
            let covered: i32 = tiles.iter().map(|t| t.size.w).sum();
            assert_eq!(covered, 1920, "columns n={n} left a gap");
            assert_eq!(tiles[0].loc.x, 0);
            let last = tiles.last().unwrap();
            assert_eq!(last.loc.x + last.size.w, 1920, "columns n={n} overshot");
        }
    }

    /// An odd width across three columns must not make one column visibly
    /// wider — the remainder is spread, not dumped.
    #[test]
    fn remainder_pixels_are_spread_not_dumped() {
        let odd = Rectangle::new(Point::from((0, 0)), Size::from((1001, 100)));
        let tiles = Layout::Columns.arrange(odd, 3, no_gaps());
        let widths: Vec<i32> = tiles.iter().map(|t| t.size.w).collect();
        assert_eq!(widths, vec![334, 334, 333]);
        assert_eq!(widths.iter().sum::<i32>(), 1001);
    }

    #[test]
    fn gaps_inset_the_outside_and_separate_the_inside() {
        let gaps = Gaps { inner: 10, outer: 20 };
        let tiles = Layout::Columns.arrange(screen(), 2, gaps);
        assert_eq!(tiles[0].loc.x, 20, "outer gap on the left");
        assert_eq!(tiles[0].loc.y, 20, "outer gap on top");
        assert_eq!(tiles[0].size.h, 1040, "outer gap top and bottom");
        let gap = tiles[1].loc.x - (tiles[0].loc.x + tiles[0].size.w);
        assert_eq!(gap, 10, "inner gap between them");
        let last = tiles.last().unwrap();
        assert_eq!(last.loc.x + last.size.w, 1900, "outer gap on the right");
    }

    /// The two properties above, checked on the *same* inputs. Testing
    /// "never overlaps" on a big screen and "never too small" on a small one
    /// lets a layout satisfy each separately while satisfying neither where
    /// they collide.
    #[test]
    fn overlap_and_minimum_hold_together() {
        let small = Rectangle::new(Point::from((0, 0)), Size::from((640, 480)));
        for n in 1..=20 {
          for layout in [Layout::default(), Layout::Columns] {
            let tiles = layout.arrange(small, n, Gaps::default());
            for (i, a) in tiles.iter().enumerate() {
                for b in &tiles[i + 1..] {
                    assert!(a.intersection(*b).is_none(), "n={n}: {a:?} overlaps {b:?}");
                }
            }
            for t in &tiles {
                assert!(t.size.w > 0 && t.size.h > 0, "n={n}: {t:?} is degenerate");
                assert!(
                    t.loc.y + t.size.h <= 480 && t.loc.x + t.size.w <= 640,
                    "n={n}: {t:?} escapes the screen"
                );
            }
          }
        }
    }

    #[test]
    fn the_ratio_is_clamped_rather_than_wrapped() {
        let mut l = Layout::default();
        for _ in 0..20 {
            l = l.widen(0.05);
        }
        assert_eq!(l, Layout::MasterStack { ratio: 0.9 });
        for _ in 0..40 {
            l = l.widen(-0.05);
        }
        assert_eq!(l, Layout::MasterStack { ratio: 0.1 });
    }

    /// An extreme ratio must still leave the other side usable.
    #[test]
    fn an_extreme_ratio_does_not_starve_the_stack() {
        for ratio in [0.1, 0.9] {
            let tiles = Layout::MasterStack { ratio }.arrange(screen(), 3, Gaps::default());
            for tile in tiles {
                assert!(tile.size.w >= MIN_MASTER, "ratio {ratio} gave {tile:?}");
            }
        }
    }

    #[test]
    fn cycling_layouts_returns_to_the_start() {
        let start = Layout::default();
        assert_eq!(start.next().next(), start);
    }

    #[test]
    fn a_point_lands_in_the_tile_that_contains_it() {
        let tiles = Layout::Columns.arrange(screen(), 3, no_gaps());
        assert_eq!(index_at(&tiles, Point::from((10, 10))), Some(0));
        assert_eq!(index_at(&tiles, Point::from((700, 500))), Some(1));
        assert_eq!(index_at(&tiles, Point::from((1900, 900))), Some(2));
    }

    /// With gaps there is genuine dead space between tiles. Reporting the
    /// nearest tile instead of None would make a drag released on a gap swap
    /// with whichever tile happened to be closer.
    #[test]
    fn a_point_in_a_gap_belongs_to_no_tile() {
        let tiles = Layout::Columns.arrange(screen(), 2, Gaps { inner: 20, outer: 20 });
        let between = tiles[0].loc.x + tiles[0].size.w + 10;
        assert_eq!(index_at(&tiles, Point::from((between, 500))), None);
        assert_eq!(index_at(&tiles, Point::from((5, 500))), None, "outer gap");
    }

    #[test]
    fn dragging_the_divider_gives_the_matching_ratio() {
        let a = screen();
        assert!((ratio_at(a, 960, no_gaps()) - 0.5).abs() < 0.01);
        assert!((ratio_at(a, 1440, no_gaps()) - 0.75).abs() < 0.01);
    }

    #[test]
    fn the_divider_cannot_be_dragged_past_the_edge() {
        let a = screen();
        assert_eq!(ratio_at(a, -500, no_gaps()), 0.1);
        assert_eq!(ratio_at(a, 5000, no_gaps()), 0.9);
    }

    #[test]
    fn stepping_moves_one_place() {
        assert_eq!(step(4, 0, 1), 1);
        assert_eq!(step(4, 2, -1), 1);
    }

    #[test]
    fn stepping_wraps_at_both_ends() {
        assert_eq!(step(4, 3, 1), 0, "past the end wraps to the start");
        assert_eq!(step(4, 0, -1), 3, "before the start wraps to the end");
    }

    /// A single window must be a fixed point, not a panic or an out-of-bounds
    /// index — this is the common case right after opening the compositor.
    #[test]
    fn a_lone_window_stays_put() {
        assert_eq!(step(1, 0, 1), 0);
        assert_eq!(step(1, 0, -1), 0);
    }

    #[test]
    fn stepping_an_empty_set_is_harmless() {
        assert_eq!(step(0, 0, 1), 0);
        assert_eq!(step(0, 0, -1), 0);
    }

    /// Whatever the delta, the result must be a valid index. A reorder that
    /// returns one past the end panics on the swap rather than misbehaving
    /// visibly.
    #[test]
    fn any_step_yields_a_valid_index() {
        for len in 1..=6usize {
            for from in 0..len {
                for delta in -9isize..=9 {
                    assert!(step(len, from, delta) < len, "len={len} from={from} d={delta}");
                }
            }
        }
    }
}
