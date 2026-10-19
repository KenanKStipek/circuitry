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

/// The header line's own trace (issue #626): about 8 cells wide
/// (`TRACE_CELLS`), one row of braille glyphs -- plain text, no
/// colour (`oscilloscope`'s own `icon::trace_color` picks that from
/// the run status). A static `◉` when `animate` is false
/// (`--no-animation`), the same single glyph an ended run's still
/// trace used to collapse to.
pub const TRACE_CELLS: usize = 8;

/// One text row of braille is 4 dot-rows tall, 2 dot-columns wide
/// per glyph -- `TRACE_CELLS` glyphs need `TRACE_CELLS * 2` dot
/// columns.
pub fn header_trace_text(frame: u64, running: bool, animate: bool) -> String {
    if !animate {
        return "◉".to_string();
    }
    let dots = trace_dots(TRACE_CELLS * 2, 4, frame, running);
    braille_row(&dots)
}

fn bit_for(dy: usize, dx: usize) -> u8 {
    match (dy, dx) {
        (0, 0) => 0x01,
        (1, 0) => 0x02,
        (2, 0) => 0x04,
        (0, 1) => 0x08,
        (1, 1) => 0x10,
        (2, 1) => 0x20,
        (3, 0) => 0x40,
        (3, 1) => 0x80,
        _ => 0,
    }
}

/// One text row of braille glyphs (U+2800 base) from a 4-row dot
/// matrix -- each glyph covers a 2×4 block of dots. Moved here from
/// `oscilloscope`'s own `icon.rs` (issue #626) so [`header_trace_text`]
/// can build the header line's own trace with no terminal code.
pub fn braille_row(dots: &[Vec<bool>]) -> String {
    let width = dots.first().map_or(0, Vec::len);
    let mut out = String::new();
    let mut x = 0;
    while x < width {
        let mut byte: u8 = 0;
        for dy in 0..4 {
            for dx in 0..2 {
                let xx = x + dx;
                if xx < width && dots.get(dy).is_some_and(|row| row[xx]) {
                    byte |= bit_for(dy, dx);
                }
            }
        }
        out.push(char::from_u32(0x2800 + byte as u32).unwrap_or(' '));
        x += 2;
    }
    out
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

    #[test]
    fn no_animation_is_a_static_dot() {
        assert_eq!(header_trace_text(0, true, false), "◉");
        assert_eq!(header_trace_text(41, true, false), "◉");
    }

    #[test]
    fn animated_trace_is_eight_cells_and_moves() {
        let frame0 = header_trace_text(0, true, true);
        let frame1 = header_trace_text(1, true, true);
        assert_eq!(frame0.chars().count(), TRACE_CELLS);
        assert_ne!(frame0, frame1);
    }

    #[test]
    fn an_ended_run_has_a_still_animated_trace() {
        let frame0 = header_trace_text(0, false, true);
        let frame9 = header_trace_text(9, false, true);
        assert_eq!(frame0, frame9);
    }
}
