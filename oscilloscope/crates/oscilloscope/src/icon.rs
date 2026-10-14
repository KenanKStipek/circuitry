//! The oscilloscope icon (DESIGN.md §6.3, issue #535): a small scope
//! drawn in text, left of the header, 3 rows high (the header's own
//! height) and about 17 columns wide, drawn as one device — the
//! screen and the knob strip share a single bezel, split by a
//! `┬`/`┴` divider, rather than two separate boxes. The trace itself
//! comes from `oscilloscope_core::icon::trace_dots`; this module only
//! ever turns those dots into braille glyphs and draws the bezel
//! around them — the one place in this crate that knows what a scope
//! should look like.

use oscilloscope_core::icon::trace_dots;
use oscilloscope_core::model::RunStatus;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

use crate::theme::Theme;

/// The whole icon's own width (issue #535: "about 16 columns", one
/// frame wider once the knob strip moved inside the bezel).
pub const ICON_WIDTH: u16 = 17;
/// The header's own height — the icon must never make the header
/// row taller (issue #535).
pub const ICON_HEIGHT: u16 = 3;

/// The screen's own text width inside the bezel (12 columns, 24
/// braille dot-columns — `trace_dots`' own `width`).
const SCREEN_COLS: usize = 12;
/// The knob strip's own width, right of the screen's own divider.
const KNOB_COLS: usize = 2;
const SCREEN_DOTS_WIDE: usize = SCREEN_COLS * 2;
/// One text row of braille is 4 dot-rows tall.
const SCREEN_DOTS_TALL: usize = 4;

/// One redraw tick is one frame (issue #535: "a frame is the
/// existing redraw tick... so the trace visibly moves"). The caller
/// passes in elapsed-time-since-start-divided-by-tick, from the
/// injected clock, never `Instant::now()` here — see `oscilloscope_
/// core::icon::trace_dots`'s own doc comment.
pub fn lines(frame: u64, status: RunStatus, theme: &Theme) -> Vec<Line<'static>> {
    let running = matches!(status, RunStatus::Running);
    let dots = trace_dots(SCREEN_DOTS_WIDE, SCREEN_DOTS_TALL, frame, running);
    let screen = braille_row(&dots);
    let trace_color = trace_color(status, theme);
    let bezel_style = Style::default().fg(theme.icon_bezel);
    let trace_style = Style::default().fg(trace_color);

    let top = Line::from(vec![Span::styled(
        format!("╭{}┬{}╮", "─".repeat(SCREEN_COLS), "─".repeat(KNOB_COLS)),
        bezel_style,
    )]);
    let middle = Line::from(vec![
        Span::styled("│", bezel_style),
        Span::styled(screen, trace_style),
        Span::styled("│", bezel_style),
        Span::styled("◉", trace_style),
        Span::styled(" ", bezel_style),
        Span::styled("│", bezel_style),
    ]);
    let bottom = Line::from(vec![Span::styled(
        format!("╰{}┴{}╯", "─".repeat(SCREEN_COLS), "─".repeat(KNOB_COLS)),
        bezel_style,
    )]);
    vec![top, middle, bottom]
}

fn trace_color(status: RunStatus, theme: &Theme) -> Color {
    match status {
        RunStatus::Running | RunStatus::Ok => theme.icon_trace_ok,
        RunStatus::Failed => theme.icon_trace_failed,
        RunStatus::Cancelled | RunStatus::Aborted => theme.icon_trace_cancelled,
    }
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

/// One text row of braille glyphs (U+2800 base) from a `SCREEN_DOTS_
/// TALL`-row dot matrix — each glyph covers a 2×4 block of dots.
fn braille_row(dots: &[Vec<bool>]) -> String {
    let width = dots.first().map_or(0, Vec::len);
    let mut out = String::new();
    let mut x = 0;
    while x < width {
        let mut byte: u8 = 0;
        for dy in 0..SCREEN_DOTS_TALL {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{ColorMode, ThemeName, resolve};

    fn plain_text(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn frame_zero_is_three_rows_of_seventeen_columns() {
        let theme = resolve(ThemeName::Terminal, ColorMode::TrueColor).theme;
        let rows = lines(0, RunStatus::Running, &theme);
        assert_eq!(rows.len(), 3);
        for line in &rows {
            let width: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
            assert_eq!(width, 17, "{line:?}");
        }
    }

    #[test]
    fn a_running_icon_differs_frame_to_frame() {
        let theme = resolve(ThemeName::Terminal, ColorMode::TrueColor).theme;
        let frame0 = plain_text(&lines(0, RunStatus::Running, &theme));
        let frame1 = plain_text(&lines(1, RunStatus::Running, &theme));
        assert_ne!(frame0[1], frame1[1], "the screen row should scroll");
    }

    #[test]
    fn an_ended_run_is_the_same_flat_line_at_every_frame() {
        let theme = resolve(ThemeName::Terminal, ColorMode::TrueColor).theme;
        let frame0 = plain_text(&lines(0, RunStatus::Ok, &theme));
        let frame9 = plain_text(&lines(9, RunStatus::Ok, &theme));
        assert_eq!(frame0, frame9);
    }

    #[test]
    fn trace_color_follows_run_status() {
        let theme = resolve(ThemeName::Terminal, ColorMode::TrueColor).theme;
        assert_eq!(trace_color(RunStatus::Running, &theme), theme.icon_trace_ok);
        assert_eq!(trace_color(RunStatus::Ok, &theme), theme.icon_trace_ok);
        assert_eq!(
            trace_color(RunStatus::Failed, &theme),
            theme.icon_trace_failed
        );
        assert_eq!(
            trace_color(RunStatus::Aborted, &theme),
            theme.icon_trace_cancelled
        );
        assert_eq!(
            trace_color(RunStatus::Cancelled, &theme),
            theme.icon_trace_cancelled
        );
    }

    #[test]
    fn no_color_draws_the_icon_with_no_color_at_all() {
        let theme = resolve(ThemeName::Dracula, ColorMode::NoColor).theme;
        let rows = lines(0, RunStatus::Running, &theme);
        for line in &rows {
            for span in &line.spans {
                assert_eq!(span.style.fg, Some(Color::Reset));
            }
        }
    }
}
