//! The oscilloscope icon's own trace (DESIGN.md §6.3, issue #535):
//! pure dot-matrix math, no terminal code. `oscilloscope`'s `tui.rs`
//! turns this into braille glyphs and draws the bezel around it; this
//! module only ever decides which dots are lit, from a frame index
//! the caller passes in (the injected clock, never `Instant::now()`
//! here).

/// One sine cycle spans this many dot columns — chosen so the trace
/// reads as a smooth wave rather than a near-flat or near-square one
/// at the icon's own screen size (24 dots wide, §6.3).
const CYCLE_DOTS: f64 = 10.0;

/// The trace's own dot matrix, `height` rows of `width` columns, row
/// 0 at the top. While `running`, a sine wave scrolls one dot column
/// per `frame` (column `x` at `frame + 1` is column `x + 1` at
/// `frame` — the same data shifted one column left, with a new one
/// entering on the right); once the run has ended, `frame` is
/// ignored and the trace is a single still, flat line at mid-height.
/// Each column lights exactly one dot, so the wave never clips: the
/// amplitude is `height / 2 - 1` rows either side of centre (0 at
/// `height <= 2`, where there's no room to wave at all).
pub fn trace_dots(width: usize, height: usize, frame: u64, running: bool) -> Vec<Vec<bool>> {
    let mut rows = vec![vec![false; width]; height];
    if height == 0 || width == 0 {
        return rows;
    }
    let center = (height as f64 - 1.0) / 2.0;
    if !running {
        let y = center.round() as usize;
        for dot in rows[y.min(height - 1)].iter_mut() {
            *dot = true;
        }
        return rows;
    }
    let amplitude = ((height as f64) / 2.0 - 1.0).max(0.0);
    // Each column picks its own row, so this isn't a plain
    // index-into-`rows` loop `enumerate()` could replace.
    #[allow(clippy::needless_range_loop)]
    for x in 0..width {
        let phase = (x as f64 + frame as f64) / CYCLE_DOTS * std::f64::consts::TAU;
        let y = (center - amplitude * phase.sin()).round();
        let y = y.clamp(0.0, (height - 1) as f64) as usize;
        rows[y][x] = true;
    }
    rows
}

/// Renders a dot matrix as a text grid (`#`/`.`) for tests and the
/// PR description's own pasted frames — not what the TUI draws
/// (that's braille glyphs, `tui.rs`'s own job), just a human-readable
/// view of the same booleans.
pub fn render_ascii(dots: &[Vec<bool>]) -> String {
    dots.iter()
        .map(|row| {
            row.iter()
                .map(|&lit| if lit { '#' } else { '.' })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 24;
    const H: usize = 4;

    #[test]
    fn frame_zero_is_pinned() {
        let dots = trace_dots(W, H, 0, true);
        insta_free_snapshot(&dots);
    }

    /// A small, dependency-free pin (no `insta` in `oscilloscope-core`):
    /// asserts the exact ASCII rendering rather than snapshotting it.
    fn insta_free_snapshot(dots: &[Vec<bool>]) {
        let rendered = render_ascii(dots);
        assert_eq!(
            rendered,
            "........................\n\
             .#####.....#####.....###\n\
             #.....#####.....#####...\n\
             ........................"
        );
    }

    #[test]
    fn a_later_frame_shifts_the_trace_by_one_dot_column() {
        let frame0 = trace_dots(W, H, 0, true);
        let frame1 = trace_dots(W, H, 1, true);
        for x in 0..W - 1 {
            let col0: Vec<bool> = (0..H).map(|y| frame1[y][x]).collect();
            let col1: Vec<bool> = (0..H).map(|y| frame0[y][x + 1]).collect();
            assert_eq!(
                col0,
                col1,
                "column {x} at frame 1 should be column {} at frame 0",
                x + 1
            );
        }
    }

    #[test]
    fn an_ended_run_gives_a_still_flat_line() {
        let dots = trace_dots(W, H, 0, false);
        let later = trace_dots(W, H, 99, false);
        assert_eq!(dots, later, "the flat line must not depend on the frame");
        let flat_row = (H as f64 - 1.0) / 2.0;
        let y = flat_row.round() as usize;
        for (row_idx, row) in dots.iter().enumerate() {
            if row_idx == y {
                assert!(row.iter().all(|&lit| lit));
            } else {
                assert!(row.iter().all(|&lit| !lit));
            }
        }
    }

    #[test]
    fn every_column_lights_exactly_one_dot_while_running() {
        let dots = trace_dots(W, H, 5, true);
        #[allow(clippy::needless_range_loop)]
        for x in 0..W {
            let lit: usize = (0..H).filter(|&y| dots[y][x]).count();
            assert_eq!(lit, 1);
        }
    }

    #[test]
    fn zero_sized_grid_does_not_panic() {
        assert_eq!(trace_dots(0, 0, 0, true), Vec::<Vec<bool>>::new());
        assert_eq!(trace_dots(4, 0, 0, false), Vec::<Vec<bool>>::new());
    }
}
