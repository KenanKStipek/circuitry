//! Lays out one `RenderState` (DESIGN.md §6.3): the header, the plan
//! tree and details panes, the log pane, and the key's own overlays
//! (help, both confirms, the full-value view, the filter prompt).
//! The only module in this crate that imports `ratatui` widgets
//! directly, mirroring `oscilloscope-core`'s own "no terminal code"
//! split one level up.

use std::time::Instant;

use oscilloscope_core::model::{RunStatus, StatusKind};
use oscilloscope_core::render::{Details, Header, Row, RowKind, StateRow, TokenBar};
use oscilloscope_core::treemap::TreemapNode;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};

use crate::highlight::Tracker;
use crate::keys::{App, Dialog, Pane, visible_rows};
use crate::theme::{ColorMode, Theme};

/// Everything a draw function needs to style a cell (issue #535): the
/// active theme, and the update-highlight tracker with the clock
/// reading and colour mode its own `style_for` needs. Threaded by
/// reference through every `draw_*` function below so none of them
/// ever reach for a `Color::` literal of their own or call `Instant::
/// now()` themselves.
pub struct DrawCtx<'a> {
    pub theme: &'a Theme,
    pub highlights: &'a Tracker,
    pub now: Instant,
    pub color_mode: ColorMode,
    /// `t`'s own banner (issue #535): the theme's name, shown in the
    /// header for about 2 seconds after a switch. `None` the rest of
    /// the time.
    pub theme_banner: Option<&'a str>,
    /// Resolves a clock time to this run's own local calendar time
    /// (issue #613): the real system zone in production
    /// (`main::LocalZone`), a fixed offset in tests.
    pub zone: &'a dyn oscilloscope_core::format::ClockZone,
    /// `--no-animation` (issue #626): the header line's own scope-icon
    /// trace draws as a static `◉` instead of an animated braille row.
    pub no_animation: bool,
}

/// A fixed UTC offset for tests that need a [`oscilloscope_core::
/// format::ClockZone`] (issue #613) -- the real system zone is
/// `main::LocalZone`, never used in these tests, so every clock-time
/// assertion here is independent of the machine's own `TZ`.
#[cfg(test)]
fn test_zone() -> chrono::FixedOffset {
    chrono::FixedOffset::east_opt(0).unwrap()
}

impl DrawCtx<'_> {
    fn highlight(&self, key: &str) -> Option<Style> {
        self.highlights
            .style_for(key, self.now, self.theme, self.color_mode)
    }
}

/// Overlays an update-highlight's own style onto `style` (issue
/// #535): the highlight's background, when it has one, replaces
/// `style`'s; its modifiers (bold, on a 16-colour terminal or `NO_
/// COLOR`) are added, never replacing `style`'s own foreground or
/// other modifiers (a selected row's `REVERSED` stays, for one).
fn with_highlight(style: Style, highlight: Option<Style>) -> Style {
    match highlight {
        None => style,
        Some(h) => {
            let style = match h.bg {
                Some(bg) => style.bg(bg),
                None => style,
            };
            style.add_modifier(h.add_modifier)
        }
    }
}

/// Issue #527's own small-terminal fallback thresholds: below each,
/// drop one more pane, in this order, until the rest fits. The header
/// and the token bar never disappear. These are whole-*terminal*
/// sizes (DESIGN.md §6.3, the README's TUI section) — checked against
/// `frame.area()` itself, never against `body` (the terminal minus
/// the header's 3 rows and the token bar's 4), which would silently
/// shift every height threshold down by 7.
mod thresholds {
    /// Below this, the `tokens by key` treemap is dropped first.
    pub const TREEMAP_MIN_WIDTH: u16 = 100;
    pub const TREEMAP_MIN_HEIGHT: u16 = 30;
    /// Below this (with the treemap already gone), the state pane is
    /// dropped too — the right column becomes the log pane alone.
    pub const STATE_PANE_MIN_WIDTH: u16 = 90;
    pub const STATE_PANE_MIN_HEIGHT: u16 = 24;
    /// Below this, the two-column layout itself collapses to one:
    /// orchestration, details and log stacked, with the state pane
    /// and the treemap gone regardless of width/height.
    pub const TWO_COLUMN_MIN_WIDTH: u16 = 80;
    pub const TWO_COLUMN_MIN_HEIGHT: u16 = 20;
}

/// Keeps `scroll` (the top-of-viewport index into a list of `total`
/// items) showing `selected` without moving it any more than it has
/// to (issue #527: "the selected row is always kept in view") —
/// jumping straight to the top or bottom only when the selection is
/// already outside the current viewport, never re-centring it.
fn clamp_scroll(scroll: usize, selected: Option<usize>, total: usize, viewport: usize) -> usize {
    let viewport = viewport.max(1);
    let mut top = scroll.min(total);
    if let Some(idx) = selected {
        if idx < top {
            top = idx;
        } else if idx + 1 > top + viewport {
            top = idx + 1 - viewport;
        }
    }
    top.min(total.saturating_sub(viewport))
}

/// A scroll position suffix for a pane's own border title (issue
/// #527: "show a scroll indicator or a position ... in the border"),
/// only when some row is actually hidden above or below.
fn scroll_indicator(selected: Option<usize>, total: usize, viewport: usize) -> Option<String> {
    if total <= viewport {
        return None;
    }
    let position = selected.map(|idx| idx + 1).unwrap_or(1);
    Some(format!(" {position}/{total} "))
}

/// Finding F6: unit tests for [`clamp_scroll`]/[`scroll_indicator`]
/// on their own, beyond the rendered goldens that already exercise
/// them indirectly.
#[cfg(test)]
mod scroll_tests {
    use super::*;

    #[test]
    fn clamp_scroll_pulls_the_viewport_down_to_a_selection_below_it() {
        assert_eq!(clamp_scroll(0, Some(7), 10, 4), 4);
    }

    #[test]
    fn clamp_scroll_pulls_the_viewport_up_to_a_selection_above_it() {
        assert_eq!(clamp_scroll(5, Some(2), 10, 4), 2);
    }

    #[test]
    fn clamp_scroll_does_not_move_when_the_selection_is_already_visible() {
        assert_eq!(clamp_scroll(3, Some(4), 10, 4), 3);
    }

    #[test]
    fn clamp_scroll_never_leaves_the_viewport_underfull_with_no_selection() {
        assert_eq!(clamp_scroll(9, None, 10, 4), 6);
    }

    #[test]
    fn scroll_indicator_is_none_when_everything_already_fits() {
        assert_eq!(scroll_indicator(Some(0), 4, 4), None);
    }

    #[test]
    fn scroll_indicator_shows_a_one_based_position_and_the_total() {
        assert_eq!(scroll_indicator(Some(6), 10, 4), Some(" 7/10 ".to_string()));
    }

    #[test]
    fn scroll_indicator_defaults_the_position_to_one_with_no_selection() {
        assert_eq!(scroll_indicator(None, 10, 4), Some(" 1/10 ".to_string()));
    }

    #[test]
    fn clamp_state_scroll_keeps_a_two_line_entry_fully_in_view() {
        // Finding F3: 10 one-line entries except entry 7, which takes
        // two lines (it has a token line) -- an 8-line viewport with
        // entry 7 selected must put the top at entry 1, not entry 4
        // (a naive by-entry-count clamp over an 8-row viewport would
        // have put the top there, rendering past the bottom edge).
        let mut line_counts = vec![1usize; 10];
        line_counts[7] = 2;
        assert_eq!(clamp_state_scroll(0, Some(7), &line_counts, 8), 1);
    }

    #[test]
    fn clamp_state_scroll_moves_up_to_a_selection_above_the_viewport() {
        let line_counts = vec![1usize; 10];
        assert_eq!(clamp_state_scroll(5, Some(2), &line_counts, 4), 2);
    }

    #[test]
    fn clamp_state_scroll_settles_on_the_selection_itself_when_it_alone_overflows() {
        // A single entry needing more lines than the whole viewport
        // has: nothing smaller can satisfy the bound, so the top is
        // the selection itself rather than scrolling arbitrarily far.
        let line_counts = vec![1, 1, 5];
        assert_eq!(clamp_state_scroll(0, Some(2), &line_counts, 2), 2);
    }
}

/// The orchestration pane's own title (issue #527): ` orchestration
/// <name> `, `<name>` the top-level document's file name without its
/// extension — orchestration documents have no `name:` field of
/// their own.
fn orchestration_name(document: &str) -> &str {
    std::path::Path::new(document)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(document)
}

/// DESIGN.md §6.3's own glyph table. `Aborted` and `LikelyRunning`/
/// `RunningOrQueued` have no glyph of their own in that table — `?`
/// for an abort (nothing else in the set means "unknown"), and the
/// same `◐`/`◌` a confirmed running/queued row uses for a guess at
/// one. `row_line`'s own style ([`status_style`], review finding 16
/// on #458's comment 3) is what tells a guess apart from the real
/// thing instead: dim and italic, rather than a second glyph
/// DESIGN.md's table has no room for.
fn status_glyph(kind: StatusKind) -> char {
    match kind {
        StatusKind::Pending => '·',
        StatusKind::Running | StatusKind::LikelyRunning => '◐',
        StatusKind::RunningOrQueued => '◌',
        StatusKind::Done => '✓',
        StatusKind::Failed => '✗',
        StatusKind::FailedHandled => '!',
        StatusKind::Skipped => '↷',
        StatusKind::Cancelled => '⊘',
        StatusKind::Aborted => '?',
    }
}

fn status_color(kind: StatusKind, theme: &Theme) -> Color {
    theme.status_color(kind)
}

/// `status_color`'s own `Style`, with `LikelyRunning`'s guess
/// (DESIGN.md §2.1 rule 7's "likely" statuses: a row inferred from
/// the run's own exit, never actually observed running) additionally
/// dim and italic -- visibly distinct from a *confirmed* `Running`
/// row, which keeps the plain, full-brightness style (review finding
/// 16 on #458's comment 3).
fn status_style(kind: StatusKind, theme: &Theme) -> Style {
    let style = Style::default().fg(status_color(kind, theme));
    if kind == StatusKind::LikelyRunning {
        style.add_modifier(Modifier::DIM | Modifier::ITALIC)
    } else {
        style
    }
}

fn row_line(row: &Row, selected: bool, ctx: &DrawCtx) -> Line<'static> {
    let indent = "  ".repeat(row.depth.saturating_sub(1));
    let glyph = status_glyph(row.status.kind);
    let mut spans = vec![
        Span::styled(
            format!("{indent}{glyph} "),
            status_style(row.status.kind, ctx.theme),
        ),
        Span::raw(row.label.clone()),
    ];
    if let Some(progress) = &row.loop_progress {
        let total = progress
            .total
            .map(|t| t.to_string())
            .unwrap_or_else(|| "?".to_string());
        spans.push(Span::styled(
            format!("  {}/{total}", progress.done),
            Style::default().fg(ctx.theme.dim),
        ));
    }
    if let Some(duration) = row.duration_s {
        spans.push(Span::styled(
            format!("  {}", oscilloscope_core::format::format_duration(duration)),
            Style::default().fg(ctx.theme.dim),
        ));
    }
    if let Some(dim) = &row.dim {
        spans.push(Span::styled(
            format!("  {dim}"),
            Style::default().fg(ctx.theme.dim),
        ));
    }
    let mut style = if selected {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };
    style = with_highlight(style, ctx.highlight(&format!("row:{}", row.path)));
    Line::from(spans).style(style)
}

/// Draws [`oscilloscope_core::screens::header_line`]'s own segments
/// (issue #626): one line, no border -- the three-row bordered
/// header this replaces is gone, along with the three-row scope icon
/// beside it (the icon's own trace is now the line's first segment).
fn draw_header_line(
    frame: &mut Frame,
    area: Rect,
    segments: &[oscilloscope_core::screens::HeaderSegment],
    ctx: &DrawCtx,
) {
    use oscilloscope_core::screens::HeaderRole;
    let status = status_of(segments);
    let spans: Vec<Span> = segments
        .iter()
        .map(|seg| {
            let style = match seg.role {
                HeaderRole::Icon => {
                    Style::default().fg(crate::icon::trace_color(status, ctx.theme))
                }
                HeaderRole::Alive => Style::default().fg(ctx.theme.status_running),
                HeaderRole::Status => Style::default().fg(run_status_color(status, ctx.theme)),
                HeaderRole::Elapsed | HeaderRole::Clock => Style::default(),
                HeaderRole::Eta => Style::default(),
                HeaderRole::Report => Style::default(),
                HeaderRole::Warning => Style::default().fg(ctx.theme.warning),
                HeaderRole::Document => Style::default(),
                HeaderRole::TabActive => Style::default().add_modifier(Modifier::BOLD),
                HeaderRole::TabInactive => Style::default().fg(ctx.theme.dim),
                HeaderRole::Plain => Style::default(),
            };
            let highlight_key = match seg.role {
                HeaderRole::Status => Some("header:status"),
                HeaderRole::Report | HeaderRole::Warning => Some("header:report"),
                _ => None,
            };
            let style = match highlight_key.and_then(|k| ctx.highlight(k)) {
                Some(h) => with_highlight(style, Some(h)),
                None => style,
            };
            Span::styled(seg.text.clone(), style)
        })
        .collect();
    let mut spans = spans;
    // `t`'s own banner (issue #535), kept as a trailing segment of
    // the one-line header -- unrelated to `Header`/`screens::
    // header_line` (DESIGN.md §6.1: it's a TUI-only, ephemeral
    // notice about the front end's own colour choice, not something
    // `--glance` or a GUI front end would ever need to show).
    if let Some(name) = ctx.theme_banner {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("theme: {name}"),
            Style::default().fg(ctx.theme.dim),
        ));
    }
    let paragraph = Paragraph::new(Line::from(spans));
    frame.render_widget(paragraph, area);
}

/// The header line carries no `RunStatus` of its own in
/// [`oscilloscope_core::screens::HeaderSegment`] (DESIGN.md §6.1: no
/// terminal code, and a status enum is still just data) -- its own
/// `Status` segment's *text* (`"running"`/`"ok"`/...) is the one
/// place it survives into the segment list, so this reads it back
/// out rather than threading `RunStatus` past `header_line`'s own
/// return value as a second, redundant parameter.
fn status_of(segments: &[oscilloscope_core::screens::HeaderSegment]) -> RunStatus {
    segments
        .iter()
        .find(|s| s.role == oscilloscope_core::screens::HeaderRole::Status)
        .map(|s| match s.text.as_str() {
            "ok" => RunStatus::Ok,
            "failed" => RunStatus::Failed,
            "cancelled" => RunStatus::Cancelled,
            "aborted" => RunStatus::Aborted,
            _ => RunStatus::Running,
        })
        .unwrap_or(RunStatus::Running)
}

fn run_status_color(status: RunStatus, theme: &Theme) -> Color {
    match status {
        RunStatus::Running => theme.status_running,
        RunStatus::Ok => theme.status_done,
        RunStatus::Failed => theme.status_failed,
        RunStatus::Cancelled => theme.status_cancelled,
        RunStatus::Aborted => theme.status_aborted,
    }
}

/// Screen 0 (issue #626): a phase strip, the now pane (the golden
/// section's larger share of the remaining height, split 62/38
/// across), and the three cards -- time, flow, cost -- in equal
/// columns across the bottom (DESIGN.md §12: "the 62/38 rule covers
/// two-way splits; equal panes ... share space equally").
fn draw_overview(
    frame: &mut Frame,
    area: Rect,
    overview: &oscilloscope_core::screens::Overview,
    ctx: &DrawCtx,
) {
    if area.height == 0 {
        return;
    }
    let phase_height = 2.min(area.height);
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(phase_height), Constraint::Min(0)])
        .split(area);
    draw_phase_strip(frame, sections[0], &overview.phase_strip, ctx);

    let rest = sections[1];
    let now_height = oscilloscope_core::layout::golden_major_section(rest.height);
    let cards_height = rest.height - now_height;
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(now_height),
            Constraint::Length(cards_height),
        ])
        .split(rest);

    draw_now_pane(frame, rows[0], &overview.now_pane, ctx);
    draw_cards(frame, rows[1], overview, ctx);
}

/// The document's own top-level steps (issue #626), already
/// breadcrumb-folded by `oscilloscope_core::fold::fold_rows` before
/// `screens::build_overview` ever saw them. Steps that don't fit
/// this width become `○ N more` -- the only place this truncation
/// happens; `screens::build_overview` itself returns every step.
fn draw_phase_strip(
    frame: &mut Frame,
    area: Rect,
    steps: &[oscilloscope_core::screens::PhaseStep],
    ctx: &DrawCtx,
) {
    if area.height == 0 || steps.is_empty() {
        return;
    }
    let max_width = area.width as usize;
    let mut line1: Vec<Span> = Vec::new();
    let mut line2: Vec<Span> = Vec::new();
    let mut used = 0usize;
    let mut shown = 0usize;
    for step in steps {
        let glyph = status_glyph(step.status.kind);
        let label1 = format!("{glyph} {}", step.label);
        let start = step
            .start_ts
            .as_deref()
            .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone));
        let dur = step
            .duration_s
            .map(oscilloscope_core::format::format_duration);
        let label2 = match (&start, &dur) {
            (Some(s), Some(d)) => format!("{s} · {d}"),
            (Some(s), None) => s.clone(),
            _ => String::new(),
        };
        let col_width = label1.chars().count().max(label2.chars().count());
        let piece_width = col_width + 3;
        if shown > 0 && used + piece_width > max_width {
            break;
        }
        used += piece_width;
        if shown > 0 {
            line1.push(Span::raw("  →  "));
            line2.push(Span::raw("     "));
        }
        line1.push(Span::styled(
            label1.clone(),
            status_style(step.status.kind, ctx.theme),
        ));
        line2.push(Span::styled(
            format!("{label2:<col_width$}"),
            Style::default().fg(ctx.theme.dim),
        ));
        shown += 1;
    }
    if shown < steps.len() {
        let more = steps.len() - shown;
        if shown > 0 {
            line1.push(Span::raw("  "));
        }
        line1.push(Span::raw(format!("○ {more} more")));
    }
    let paragraph = Paragraph::new(vec![Line::from(line1), Line::from(line2)]);
    frame.render_widget(paragraph, area);
}

/// The now pane's own left (log) / right (focus) split, 62/38
/// (DESIGN.md's two-way-split rule).
fn draw_now_pane(
    frame: &mut Frame,
    area: Rect,
    pane: &oscilloscope_core::screens::NowPane,
    ctx: &DrawCtx,
) {
    if area.height == 0 {
        return;
    }
    let left_width = oscilloscope_core::layout::golden_major_section(area.width);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(left_width), Constraint::Min(0)])
        .split(area);
    let log_lines: Vec<Line> = pane
        .log_lines
        .iter()
        .map(|l| Line::from(l.clone()))
        .collect();
    let log =
        Paragraph::new(log_lines).block(Block::default().borders(Borders::ALL).title(" now "));
    frame.render_widget(log, cols[0]);

    let focus =
        Paragraph::new(focus_lines(&pane.focus, ctx)).block(Block::default().borders(Borders::ALL));
    frame.render_widget(focus, cols[1]);
}

/// The now pane's own right side (issue #626): the step follow
/// selects, or, with nothing running, the last finished step, or,
/// once the run has ended, its outcome and totals.
fn focus_lines(focus: &oscilloscope_core::screens::NowFocus, ctx: &DrawCtx) -> Vec<Line<'static>> {
    use oscilloscope_core::screens::NowFocus;
    match focus {
        NowFocus::None => vec![Line::from("nothing has run yet")],
        NowFocus::Ended(summary) => {
            let (status_text, color) = match summary.outcome {
                RunStatus::Ok => ("ok", ctx.theme.status_done),
                RunStatus::Failed => ("failed", ctx.theme.status_failed),
                RunStatus::Cancelled => ("cancelled", ctx.theme.status_cancelled),
                RunStatus::Aborted => ("aborted", ctx.theme.status_aborted),
                RunStatus::Running => ("running", ctx.theme.status_running),
            };
            let mut lines = vec![Line::from(Span::styled(
                status_text,
                Style::default().fg(color),
            ))];
            let start = summary
                .start_ts
                .as_deref()
                .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone));
            let end = summary
                .end_ts
                .as_deref()
                .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone));
            if let (Some(start), Some(end)) = (start, end) {
                lines.push(Line::from(format!("{start}–{end}")));
            }
            lines.push(Line::from(format!(
                "↑{} ↓{}",
                oscilloscope_core::format::format_tokens(summary.tokens_sent),
                oscilloscope_core::format::format_tokens(summary.tokens_received)
            )));
            if let Some(cost) = summary.cost_usd {
                lines.push(Line::from(format!("${cost:.2}")));
            }
            lines
        }
        NowFocus::Running(step, more) => focus_step_lines(step, Some(*more), ctx),
        NowFocus::LastFinished(step) => focus_step_lines(step, None, ctx),
    }
}

fn focus_step_lines(
    step: &oscilloscope_core::screens::FocusedStep,
    more_running: Option<usize>,
    ctx: &DrawCtx,
) -> Vec<Line<'static>> {
    use oscilloscope_core::screens::FocusDetail;
    {
        let mut lines = vec![Line::from(step.breadcrumb.clone())];
        let mut meta = vec![step.kind_label.to_string()];
        if let Some(provider_or_model) = &step.provider_or_model {
            meta.push(provider_or_model.clone());
        }
        lines.push(Line::from(Span::styled(
            meta.join(" · "),
            Style::default().fg(ctx.theme.dim),
        )));
        if let (Some(since), Some(elapsed)) = (
            step.since_ts
                .as_deref()
                .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone)),
            step.elapsed_s,
        ) {
            lines.push(Line::from(format!(
                "since {since} · {}",
                oscilloscope_core::format::format_duration(elapsed)
            )));
        }
        match &step.detail {
            FocusDetail::Prompt {
                tokens,
                estimated,
                snippet,
            } => {
                if let Some(tokens) = tokens {
                    let prefix = if *estimated { "≈" } else { "" };
                    lines.push(Line::from(format!(
                        "{prefix}{}",
                        oscilloscope_core::format::format_tokens(*tokens)
                    )));
                }
                if let Some(snippet) = snippet {
                    lines.push(Line::from(snippet.clone()));
                }
            }
            FocusDetail::Tool { command } => {
                if let Some(command) = command {
                    lines.push(Line::from(command.clone()));
                }
            }
            FocusDetail::Agent {
                engine,
                model,
                note,
            } => {
                let mut parts = Vec::new();
                if let Some(engine) = engine {
                    parts.push(engine.clone());
                }
                if let Some(model) = model {
                    parts.push(model.clone());
                }
                if !parts.is_empty() {
                    lines.push(Line::from(parts.join(" · ")));
                }
                lines.push(Line::from(Span::styled(
                    *note,
                    Style::default().fg(ctx.theme.dim),
                )));
            }
            FocusDetail::Plain => {}
        }
        if let Some(more) = more_running {
            if more > 0 {
                lines.push(Line::from(format!("+{more} more running")));
            }
        }
        lines
    }
}

/// The overview's own three cards (issue #626) -- time, flow, cost,
/// equal columns (DESIGN.md §12: "equal panes ... share space
/// equally").
fn draw_cards(
    frame: &mut Frame,
    area: Rect,
    overview: &oscilloscope_core::screens::Overview,
    ctx: &DrawCtx,
) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let third = area.width / 3;
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(third),
            Constraint::Length(third),
            Constraint::Length(area.width - 2 * third),
        ])
        .split(area);
    draw_time_card(frame, cols[0], &overview.time_card, ctx);
    draw_flow_card(frame, cols[1], &overview.flow_card, ctx);
    draw_cost_card(frame, cols[2], &overview.cost_card, ctx);
}

fn draw_time_card(
    frame: &mut Frame,
    area: Rect,
    card: &oscilloscope_core::screens::TimeCard,
    ctx: &DrawCtx,
) {
    let mut lines = Vec::new();
    let start = card
        .start_ts
        .as_deref()
        .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone));
    let end = card
        .end_ts
        .as_deref()
        .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone));
    match (&start, &end) {
        (Some(s), Some(e)) => lines.push(Line::from(format!("{s}–{e}"))),
        (Some(s), None) => lines.push(Line::from(format!("{s} → now"))),
        _ => {}
    }
    lines.push(Line::from(oscilloscope_core::format::format_duration(
        card.elapsed_s,
    )));
    if let Some(eta) = card
        .eta_ts
        .as_deref()
        .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone))
    {
        lines.push(Line::from(format!("ETA ~{}", &eta[..5.min(eta.len())])));
    }
    if let Some((label, secs)) = &card.slowest_finished {
        lines.push(Line::from(format!(
            "slowest {label} {}",
            oscilloscope_core::format::format_duration(*secs)
        )));
    }
    let paragraph =
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" time "));
    frame.render_widget(paragraph, area);
}

fn draw_flow_card(
    frame: &mut Frame,
    area: Rect,
    card: &oscilloscope_core::screens::FlowCard,
    ctx: &DrawCtx,
) {
    use oscilloscope_core::screens::FlowEvent;
    let mut lines = Vec::new();
    let effects = match card.effects_planned {
        Some(planned) if card.effects_planned_is_lower_bound => {
            format!("{}/{planned}+", card.effects_done)
        }
        Some(planned) => format!("{}/{planned}", card.effects_done),
        None => card.effects_done.to_string(),
    };
    lines.push(Line::from(format!(
        "{effects} effects · {} failed",
        card.failures
    )));
    for event in &card.recent {
        let (text, ts) = match event {
            FlowEvent::Decision { name, branch, ts } => (format!("{name} → {branch}"), ts),
            FlowEvent::Report { message, ts } => (message.clone(), ts),
        };
        let clock = ts
            .as_deref()
            .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone));
        match clock {
            Some(clock) => lines.push(Line::from(format!("{clock}  {text}"))),
            None => lines.push(Line::from(text)),
        }
    }
    if !card.loops.is_empty() {
        let text = card
            .loops
            .iter()
            .map(|(label, done, total)| match total {
                Some(total) => format!("{label} {done}/{total}"),
                None => format!("{label} {done}"),
            })
            .collect::<Vec<_>>()
            .join(" · ");
        lines.push(Line::from(text));
    }
    if let Some(next) = &card.next_pending {
        lines.push(Line::from(format!("next {next}")));
    }
    if let Some((count, secs)) = card.quick_steps {
        lines.push(Line::from(format!(
            "{count} quick steps · {}",
            oscilloscope_core::format::format_duration(secs)
        )));
    }
    let paragraph =
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" flow "));
    frame.render_widget(paragraph, area);
}

fn draw_cost_card(
    frame: &mut Frame,
    area: Rect,
    card: &oscilloscope_core::screens::CostCard,
    ctx: &DrawCtx,
) {
    let mut lines = vec![Line::from(format!(
        "↑{} ↓{}",
        oscilloscope_core::format::format_tokens(card.tokens_sent),
        oscilloscope_core::format::format_tokens(card.tokens_received)
    ))];
    if let Some(cost) = card.cost_usd {
        lines.push(Line::from(format!("${cost:.2}")));
    }
    if let Some((path, total)) = &card.top_store {
        lines.push(Line::from(format!(
            "top {path} {}",
            oscilloscope_core::format::format_tokens(*total)
        )));
    }
    lines.push(Line::from(card.in_flight.clone()));
    let _ = ctx;
    let paragraph =
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" cost "));
    frame.render_widget(paragraph, area);
}

/// The orchestration pane (issue #527, formerly "plan"): scrolls to
/// keep the selected row in view, with the current position shown in
/// the border once any row is hidden above or below.
fn draw_tree(
    frame: &mut Frame,
    area: Rect,
    rows: &[&Row],
    app: &mut App,
    header: &oscilloscope_core::render::Header,
    ctx: &DrawCtx,
) {
    let document = &header.document;
    let viewport = area.height.saturating_sub(2) as usize;
    app.tree_viewport = viewport;
    let selected_idx = app
        .selected_path
        .as_deref()
        .and_then(|p| rows.iter().position(|r| r.path == p));
    app.tree_scroll = clamp_scroll(app.tree_scroll, selected_idx, rows.len(), viewport);
    let visible_slice =
        &rows[app.tree_scroll.min(rows.len())..(app.tree_scroll + viewport).min(rows.len())];

    let items: Vec<ListItem> = visible_slice
        .iter()
        .map(|row| {
            let selected = app.selected_path.as_deref() == Some(row.path.as_str());
            ListItem::new(row_line(row, selected, ctx))
        })
        .collect();
    let border_style = if app.pane == Pane::Tree {
        Style::default().fg(ctx.theme.border_focused)
    } else {
        Style::default().fg(ctx.theme.border)
    };
    let effects = match header.effects_planned {
        Some(planned) if header.effects_planned_is_lower_bound => {
            format!("{}/{planned}+", header.effects_done)
        }
        Some(planned) => format!("{}/{planned}", header.effects_done),
        None => header.effects_done.to_string(),
    };
    // Issue #626: the effects count moved here from the header line,
    // which no longer shows it at all.
    let mut title = format!(
        " orchestration {} ({effects})",
        orchestration_name(document)
    );
    if app.follow {
        title.push_str(" (following)");
    }
    match scroll_indicator(selected_idx, rows.len(), viewport) {
        Some(indicator) => title.push_str(&indicator),
        None => title.push(' '),
    }
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(border_style),
    );
    frame.render_widget(list, area);
}

/// The `sent`/`received` snippet's own cap (finding M1): three
/// *wrapped* lines at the pane's current inner width, not the
/// issue's original flat 200-character cut — at the golden-section
/// split's own narrower details pane, a long snippet at 200 characters
/// still ran past both blocks fitting on screen at once. `v` still
/// opens the untruncated text either way.
const SNIPPET_MAX_LINES: usize = 3;

/// `row_kind` is the drawn orchestration row's own kind for this same
/// path, when there is one: only the drawn rows know a folded row's
/// kind (`quick steps`), which `details_for` can't derive from the path
/// alone. Without one (a state-pane-only node, such as a named report),
/// the path-derived `details.kind` is used.
fn details_lines(
    details: &Details,
    row_kind: Option<RowKind>,
    ctx: &DrawCtx,
    inner_width: u16,
) -> Vec<Line<'static>> {
    let kind = row_kind.unwrap_or(details.kind);
    let mut lines = vec![
        Line::from(format!("path   {}", details.path)),
        Line::from(format!("type   {}", kind.label())),
    ];
    // Issue #535/finding 6: these two blocks come right after path/
    // type, before plan/meta/error -- at the pane's own golden-
    // section height, a prompt's meta list could otherwise push both
    // snippets below the fold.
    if let Some(sent) = &details.sent {
        lines.push(Line::from(""));
        let style = with_highlight(Style::default(), ctx.highlight("details:sent"));
        lines.push(token_block_header("sent", '↑', sent).style(style));
        if let Some(snippet) = &sent.snippet {
            for line in snippet_lines(snippet, inner_width, SNIPPET_MAX_LINES) {
                lines.push(Line::from(line));
            }
        }
    }
    if let Some(received) = &details.received {
        lines.push(Line::from(""));
        let style = with_highlight(Style::default(), ctx.highlight("details:received"));
        lines.push(token_block_header("received", '↓', received).style(style));
        if received.waiting {
            lines.push(Line::styled(
                "waiting for the reply",
                Style::default().fg(ctx.theme.dim),
            ));
        } else if let Some(snippet) = &received.snippet {
            for line in snippet_lines(snippet, inner_width, SNIPPET_MAX_LINES) {
                lines.push(Line::from(line));
            }
        }
    }
    if !details.plan_summary.is_empty() {
        lines.push(Line::from(format!("plan   {}", details.plan_summary)));
    }
    for (key, value) in &details.meta_summary {
        // Finding 11: a label of 7+ characters ("provider",
        // "adapter_noop") left no gap from `{key:<7}` alone.
        // A report's `time` is a raw timestamp; shown as a local clock
        // time like every other time on screen (#613).
        let shown = if key == "time" {
            oscilloscope_core::format::format_clock_time(value, ctx.zone)
                .unwrap_or_else(|| value.clone())
        } else {
            value.clone()
        };
        lines.push(Line::from(format!("{key:<8} {shown}")));
    }
    if let Some(error) = &details.error {
        lines.push(Line::from(""));
        lines.push(Line::styled(
            format!("error  {error}"),
            Style::default().fg(ctx.theme.error),
        ));
    }
    if details.sent.is_none() {
        if let Some(prompt_sent) = &details.prompt_sent {
            lines.push(Line::from(""));
            lines.push(Line::from("prompt_sent (v for full):"));
            lines.push(Line::from(prompt_sent.clone()));
        }
        if let Some(value) = &details.value {
            if !value.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from("value (v for full):"));
                lines.push(Line::from(value.clone()));
            }
        }
    }
    lines
}

/// Greedy word-wrap at `width` columns, one hard split per
/// overlength word rather than overflowing it (finding M1).
fn wrap_lines(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut current: Vec<char> = Vec::new();
    for word in text.split(' ') {
        let word_chars: Vec<char> = word.chars().collect();
        if !current.is_empty() && current.len() + 1 + word_chars.len() > width {
            lines.push(current.iter().collect());
            current.clear();
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.extend(word_chars.iter());
        while current.len() > width {
            let (head, tail) = current.split_at(width);
            lines.push(head.iter().collect());
            current = tail.to_vec();
        }
    }
    if !current.is_empty() {
        lines.push(current.iter().collect());
    }
    lines
}

/// [`wrap_lines`], capped to `max_lines` -- any remaining text
/// collapses into the last line's own trailing `…` (finding M1).
fn snippet_lines(text: &str, width: u16, max_lines: usize) -> Vec<String> {
    let width = width as usize;
    let wrapped = wrap_lines(text, width);
    if wrapped.len() <= max_lines {
        return wrapped;
    }
    let mut capped: Vec<String> = wrapped.into_iter().take(max_lines).collect();
    if let Some(last) = capped.last_mut() {
        let keep = width.saturating_sub(1);
        let mut chars: Vec<char> = last.chars().take(keep).collect();
        chars.push('…');
        *last = chars.into_iter().collect();
    }
    capped
}

#[cfg(test)]
mod snippet_wrap_tests {
    use super::*;

    #[test]
    fn wrap_lines_breaks_on_whole_words_within_width() {
        let lines = wrap_lines("the quick brown fox jumps", 10);
        assert_eq!(lines, vec!["the quick", "brown fox", "jumps"]);
    }

    #[test]
    fn wrap_lines_hard_splits_a_single_overlong_word() {
        let lines = wrap_lines("abcdefghij", 4);
        assert_eq!(lines, vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn snippet_lines_fits_within_the_cap_untouched() {
        let lines = snippet_lines("one two three", 20, 3);
        assert_eq!(lines, vec!["one two three"]);
    }

    #[test]
    fn snippet_lines_caps_at_three_and_ends_with_an_ellipsis() {
        let text = "one two three four five six seven eight nine ten";
        let lines = snippet_lines(text, 10, 3);
        assert_eq!(lines.len(), 3);
        assert!(lines.last().unwrap().ends_with('…'), "{lines:?}");
        for line in &lines {
            assert!(line.chars().count() <= 10, "{line:?} exceeds width");
        }
    }
}

/// One `sent`/`received` block's own header line (issue #535):
/// `sent  ↑ 1.2 kt` or `received  ↓ ≈567 t` (an estimate), formatted
/// the same way every other token count in `osi` is.
fn token_block_header(
    label: &str,
    arrow: char,
    block: &oscilloscope_core::render::TokenBlock,
) -> Line<'static> {
    let text = match block.tokens {
        Some(tokens) if block.estimated => {
            format!(
                "{label}  {arrow} ≈{}",
                oscilloscope_core::format::format_tokens(tokens)
            )
        }
        Some(tokens) => format!(
            "{label}  {arrow} {}",
            oscilloscope_core::format::format_tokens(tokens)
        ),
        None => format!("{label}  {arrow}"),
    };
    Line::from(text)
}

/// The details pane (issue #527): scrolls when it has focus
/// (`↑↓`, `PgUp`/`PgDn`), and returns to the top whenever the
/// selected row changes (`App::select_tree_path`'s own job, not
/// this function's).
fn draw_details(
    frame: &mut Frame,
    area: Rect,
    details: Option<&Details>,
    row_kind: Option<RowKind>,
    app: &mut App,
    ctx: &DrawCtx,
) {
    let border_style = if app.pane == Pane::Details {
        Style::default().fg(ctx.theme.border_focused)
    } else {
        Style::default().fg(ctx.theme.border)
    };
    let inner_width = area.width.saturating_sub(2);
    let text = details
        .map(|d| details_lines(d, row_kind, ctx, inner_width))
        .unwrap_or_else(|| vec![Line::from("(nothing selected)")]);
    let viewport = area.height.saturating_sub(2) as usize;
    app.details_viewport = viewport;
    let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
    // Finding F8: `text.len()` counts *logical* lines; a long
    // `prompt_sent`/value wraps to more rendered lines than that on a
    // narrow layout, and `Paragraph::scroll` counts the wrapped ones
    // -- clamping to the logical count left `End` unable to reach a
    // wrapped line's own tail.
    let max_scroll = paragraph.line_count(inner_width).saturating_sub(1);
    if app.details_scroll > max_scroll {
        app.details_scroll = max_scroll;
    }
    // A loop pass's own `pass N of M
    // (iter_N)` belongs in this title, not the `plan` line
    // `details_lines` draws inside the pane.
    let title = match details.and_then(|d| d.title.as_deref()) {
        Some(title) => format!(" {title} "),
        None => " details ".to_string(),
    };
    let paragraph = paragraph.scroll((app.details_scroll as u16, 0)).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(border_style),
    );
    frame.render_widget(paragraph, area);
}

/// `log_total` is the plain running count of every line ever pushed
/// onto `log_lines` (finding 4), not `log_lines.len()` -- a line's
/// own highlight key is its own sequence number
/// (`log_total - log_lines.len() + its own index`), which survives
/// `MAX_LOG_LINES`' own trim from the front; a plain buffer index
/// would shift under an already-marked key the moment the trim ran.
/// `print_line`/`format_log_line`'s own clock prefix, `HH:MM:SS `
/// (`main.rs::Clock::format` plus the one space that always follows
/// it) -- always this many bytes, since the clock is always exactly
/// two ASCII digits per field.
const LOG_CLOCK_PREFIX_LEN: usize = 9;

/// Whether `line` (already clock-prefixed) is a report's own line
/// (#610): the leading `▸` right after the clock prefix, written
/// only by `diff::report_line` -- no other line this crate ever
/// builds starts with it, so a front end can tell the two apart from
/// the line's own text alone, with no separate field needed.
fn is_report_log_line(line: &str) -> bool {
    line.get(LOG_CLOCK_PREFIX_LEN..)
        .is_some_and(|rest| rest.starts_with('▸'))
}

fn is_warning_report_log_line(line: &str) -> bool {
    line.get(LOG_CLOCK_PREFIX_LEN..)
        .is_some_and(|rest| rest.starts_with("▸ warning: "))
}

fn draw_log(frame: &mut Frame, area: Rect, log_lines: &[String], log_total: u64, ctx: &DrawCtx) {
    let height = area.height.saturating_sub(2) as usize;
    let start = log_lines.len().saturating_sub(height);
    let seq_start = log_total.saturating_sub(log_lines.len() as u64);
    // A report's own `data` preview is capped to 200 characters when
    // the line is built (`diff::report_line`, the same cap `--log`
    // uses, since there is no pane to fit there) -- narrower still
    // once an actual TUI pane is this width, which `truncate_label`
    // clips further here, with its own `…`, same as every other
    // pane-width cut this crate makes. Only a report's own line gets
    // this: every other kind already keeps well under a usable pane's
    // width on its own (a fixed, short cap of its own, DESIGN.md
    // §2.4), so clipping them too would just risk changing their
    // look for no reason.
    let pane_width = area.width.saturating_sub(2) as usize;
    let text: Vec<Line> = log_lines[start..]
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let seq = seq_start + (start + i) as u64;
            let base = if is_warning_report_log_line(l) {
                Style::default().fg(ctx.theme.warning)
            } else {
                Style::default()
            };
            let style = with_highlight(base, ctx.highlight(&format!("log:{seq}")));
            let shown = if is_report_log_line(l) {
                truncate_label(l, pane_width)
            } else {
                l.clone()
            };
            Line::from(shown).style(style)
        })
        .collect();
    let paragraph =
        Paragraph::new(text).block(Block::default().borders(Borders::ALL).title(" log "));
    frame.render_widget(paragraph, area);
}

/// A label too long for `max`, truncated from the *right*, trailing
/// `…` (treemap cell labels, pane border titles): the start of the
/// text is usually the more identifying part there.
fn truncate_label(label: &str, max: usize) -> String {
    let count = label.chars().count();
    if count <= max {
        label.to_string()
    } else {
        format!(
            "{}…",
            label
                .chars()
                .take(max.saturating_sub(1))
                .collect::<String>()
        )
    }
}

/// [`clamp_scroll`]'s own job, but counting *lines* rather than
/// entries (finding F3): a state-pane entry with a token line takes
/// two of the pane's interior lines, not one, so clamping by entry
/// count alone could keep the selected entry's own second line (or
/// all of it) scrolled out of the viewport even though `scroll` said
/// it was "in view". `line_counts[i]` is how many lines entry `i`
/// takes; the search for the smallest `top` that still fits
/// `line_counts[top..=selected]` within `viewport` uses prefix sums
/// so it stays `O(log n)` beyond building them, not the `O(n)` a
/// linear walk per redraw would cost against issue #527's own
/// redraw-cost acceptance.
fn clamp_state_scroll(
    scroll: usize,
    selected: Option<usize>,
    line_counts: &[usize],
    viewport: usize,
) -> usize {
    let viewport = viewport.max(1);
    let total = line_counts.len();
    if total == 0 {
        return 0;
    }
    let mut top = scroll.min(total - 1);
    let Some(idx) = selected else {
        return top;
    };
    if idx < top {
        return idx;
    }
    let mut prefix = vec![0usize; total + 1];
    for (i, count) in line_counts.iter().enumerate() {
        prefix[i + 1] = prefix[i] + count;
    }
    let target = prefix[idx + 1];
    if target - prefix[top] > viewport {
        let (mut lo, mut hi) = (top, idx);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            if target - prefix[mid] <= viewport {
                hi = mid;
            } else {
                lo = mid + 1;
            }
        }
        top = lo;
    }
    top
}

/// The state pane (issue #527): the state's effect nodes, most
/// recently updated first (`render::build_state_rows`' own order),
/// with an independent selection and scroll from the orchestration
/// pane's. Finding F3: scrolling is clamped and sliced by *lines*,
/// not by entry count, since a token line makes most entries two
/// lines tall rather than one.
fn draw_state(
    frame: &mut Frame,
    area: Rect,
    state_rows: &[&StateRow],
    app: &mut App,
    ctx: &DrawCtx,
) {
    let viewport = area.height.saturating_sub(2) as usize;
    let total = state_rows.len();
    let line_counts: Vec<usize> = state_rows
        .iter()
        .map(|r| if r.tokens.is_some() { 2 } else { 1 })
        .collect();
    let selected_idx = app
        .state_selected_path
        .as_deref()
        .and_then(|p| state_rows.iter().position(|r| r.path == p));
    app.state_scroll = clamp_state_scroll(app.state_scroll, selected_idx, &line_counts, viewport);

    let start = app.state_scroll.min(total);
    let mut end = start;
    let mut used = 0usize;
    while end < total && used + line_counts[end] <= viewport {
        used += line_counts[end];
        end += 1;
    }
    if end == start && total > start {
        // Nothing fits whole; show the top entry anyway rather than
        // an empty pane.
        end += 1;
    }
    app.state_viewport = (end - start).max(1);
    let visible_slice = &state_rows[start..end.min(total)];

    let mut lines: Vec<Line> = Vec::new();
    for row in visible_slice {
        let selected = app.state_selected_path.as_deref() == Some(row.path.as_str());
        let indent = "  ".repeat(row.depth);
        let collapse_marker = if row.has_children {
            if app.state_collapsed.contains(&row.path) {
                "+ "
            } else {
                ""
            }
        } else {
            ""
        };
        let mut spans = vec![
            Span::styled(
                format!(
                    "{indent}{collapse_marker}{} ",
                    status_glyph(row.status.kind)
                ),
                status_style(row.status.kind, ctx.theme),
            ),
            Span::raw(row.key.clone()),
        ];
        if let Some(dim) = &row.dim {
            spans.push(Span::styled(
                format!("  {dim}"),
                Style::default().fg(ctx.theme.dim),
            ));
        }
        let mut line = Line::from(spans);
        let mut style = if selected {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        style = with_highlight(style, ctx.highlight(&format!("state:{}", row.path)));
        line = line.style(style);
        lines.push(line);
        if let Some((sent, received)) = row.tokens {
            lines.push(Line::styled(
                format!(
                    "{indent}  ↑{} ↓{}",
                    oscilloscope_core::format::format_tokens(sent),
                    oscilloscope_core::format::format_tokens(received)
                ),
                Style::default().fg(ctx.theme.dim),
            ));
        }
    }

    let border_style = if app.pane == Pane::State {
        Style::default().fg(ctx.theme.border_focused)
    } else {
        Style::default().fg(ctx.theme.border)
    };
    let mut title = " state".to_string();
    let all_visible = start == 0 && end >= total;
    if all_visible {
        title.push(' ');
    } else {
        let position = selected_idx.map(|i| i + 1).unwrap_or(1);
        title.push_str(&format!(" {position}/{total} "));
    }
    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(title)
            .border_style(border_style),
    );
    frame.render_widget(paragraph, area);
}

/// The `tokens by key` treemap (issue #527): a nested, squarified
/// treemap of the current state's token totals, laid out by
/// `oscilloscope_core::treemap::layout` (pure integer geometry) and
/// drawn here — this function never computes a rectangle of its own.
fn draw_treemap(
    frame: &mut Frame,
    area: Rect,
    nodes: &[TreemapNode],
    selected_path: Option<&str>,
    ctx: &DrawCtx,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" tokens by key ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if nodes.is_empty() || inner.width == 0 || inner.height == 0 {
        let paragraph = Paragraph::new("(no tokens yet)");
        frame.render_widget(paragraph, inner);
        return;
    }

    let cells =
        oscilloscope_core::treemap::layout(nodes, inner.x, inner.y, inner.width, inner.height);
    for cell in &cells {
        let rect = Rect::new(cell.rect.x, cell.rect.y, cell.rect.w, cell.rect.h);
        if rect.width == 0 || rect.height == 0 {
            continue;
        }
        let color = cell
            .status
            .map(|s| status_color(s, ctx.theme))
            .unwrap_or(ctx.theme.dim);
        let is_selected = selected_path.is_some() && cell.path.as_deref() == selected_path;
        let highlight = cell
            .path
            .as_deref()
            .and_then(|p| ctx.highlight(&format!("treemap:{p}")));
        let mut style = Style::default().fg(color);
        if is_selected {
            style = style.add_modifier(Modifier::REVERSED);
        }
        let labelled = format!(
            "{} {}",
            cell.label,
            oscilloscope_core::format::format_tokens(cell.total)
        );
        if cell.has_children {
            let block = Block::default()
                .borders(Borders::ALL)
                .title(truncate_label(
                    &labelled,
                    rect.width.saturating_sub(2) as usize,
                ))
                .border_style(with_highlight(style, highlight));
            frame.render_widget(block, rect);
        } else {
            // Finding F10: a leaf (or a merged `…` bucket, or a
            // container too small for its own border, issue #527) gets
            // no border, so its own *background* fill is what makes it
            // read as an area at all — an fg-only label over the
            // default background left two adjacent leaves with no
            // visible edge between them whatsoever, and a `rect.w < 2`
            // cell styled with `fg` alone (rather than `bg`) drew
            // nothing, contrary to DESIGN.md's "just its own background
            // colour". The label, when there's room for one, is drawn
            // in a contrasting foreground over that same fill so it
            // stays readable regardless of which status colour the
            // cell is filled with.
            let fill = Style::default().bg(color);
            let fill = if is_selected {
                fill.add_modifier(Modifier::REVERSED)
            } else {
                fill
            };
            let fill = with_highlight(fill, highlight);
            frame.render_widget(Block::default().style(fill), rect);
            if rect.width >= 2 {
                let label = truncate_label(&labelled, rect.width as usize);
                let text_style = Style::default().bg(color).fg(ctx.theme.on_accent);
                let text_style = if is_selected {
                    text_style.add_modifier(Modifier::REVERSED)
                } else {
                    text_style
                };
                let text_style = with_highlight(text_style, highlight);
                let paragraph = Paragraph::new(Line::styled(label, text_style));
                frame.render_widget(paragraph, rect);
            }
        }
    }
}

/// A bar of filled block characters, `value`'s own share of `max`
/// (issue #527's "scaled to the larger of the two") — `max == 0`
/// (nothing sent or received yet) draws an empty bar rather than
/// dividing by zero.
fn bar(value: u64, max: u64, width: usize) -> String {
    if max == 0 || width == 0 {
        return String::new();
    }
    let filled = ((value as f64 / max as f64) * width as f64).round() as usize;
    "█".repeat(filled.min(width))
}

/// The token bar (issue #527): a bordered strip across the full
/// bottom width, two content lines. Line 1's totals always equal the
/// header's own `↑`/`↓` — both read the same token rule
/// (`oscilloscope_core::model::node_tokens`).
fn draw_token_bar(
    frame: &mut Frame,
    area: Rect,
    header: &Header,
    token_bar: &TokenBar,
    ctx: &DrawCtx,
) {
    let inner_width = area.width.saturating_sub(2) as usize;
    let sent_str = oscilloscope_core::format::format_tokens(header.tokens_sent);
    let received_str = oscilloscope_core::format::format_tokens(header.tokens_received);
    // Finding 2: the line holds *two* bars, each with its own label and
    // exact count ("in  " + bar + " <sent>   " + "out  " + bar + "
    // <received>") — a fixed-width bar for each (the old `inner - 24`)
    // left no room for the counts themselves once they grew past a
    // couple of digits, cutting off the very numbers this pane exists
    // to show. Subtracting the two labels' and counts' own measured
    // width first, then halving what's left between the two bars,
    // means the numbers are never what runs out of room.
    let overhead = 4 + 1 + sent_str.chars().count() + 3 + 5 + 1 + received_str.chars().count();
    let bar_width = (inner_width.saturating_sub(overhead) / 2).min(40);
    let max = header.tokens_sent.max(header.tokens_received);
    let in_bar = bar(header.tokens_sent, max, bar_width);
    let out_bar = bar(header.tokens_received, max, bar_width);
    let tokenbar_highlight = ctx.highlight("tokenbar");
    // Finding 14: a totals change alone (the header's own `tokens_
    // sent`/`tokens_received`, `header:tokens` in `changes::diff`)
    // doesn't necessarily change the in-flight `TokenBar` struct
    // `tokenbar` keys on, but this line shows those totals, not the
    // in-flight estimate -- it must light up on either key.
    let totals_highlight = tokenbar_highlight.or_else(|| ctx.highlight("header:tokens"));
    let line1 = Line::from(vec![
        Span::raw("in  "),
        Span::styled(
            in_bar,
            with_highlight(Style::default().fg(ctx.theme.token_in), totals_highlight),
        ),
        Span::styled(
            format!(" {sent_str}   "),
            with_highlight(Style::default(), totals_highlight),
        ),
        Span::raw("out  "),
        Span::styled(
            out_bar,
            with_highlight(Style::default().fg(ctx.theme.token_out), totals_highlight),
        ),
        Span::styled(
            format!(" {received_str}"),
            with_highlight(Style::default(), totals_highlight),
        ),
    ]);
    let line2 = Line::from(oscilloscope_core::render::token_bar_in_flight_line(
        token_bar,
    ))
    .style(with_highlight(Style::default(), tokenbar_highlight));
    let paragraph = Paragraph::new(vec![line1, line2])
        .block(Block::default().borders(Borders::ALL).title(" tokens "));
    frame.render_widget(paragraph, area);
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width, height)
}

/// Finding 19: `Clear` resets the popup's own area to the terminal's
/// bare default (no background, no foreground) — a named theme's own
/// base fill (`draw`'s own `Block` at the top of this file) never
/// reaches back over a popup drawn on top of it, so every overlay
/// repaints its own area with the theme's `background`/`text` right
/// after clearing it, the same pair `draw`'s own base fill sets.
fn draw_overlay(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    text: Vec<Line<'static>>,
    w: u16,
    h: u16,
    ctx: &DrawCtx,
) {
    let popup = centered(area, w, h);
    frame.render_widget(Clear, popup);
    if let Some(bg) = ctx.theme.background {
        frame.render_widget(
            Block::default().style(Style::default().bg(bg).fg(ctx.theme.text)),
            popup,
        );
    }
    let paragraph = Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .alignment(Alignment::Left)
        .block(Block::default().borders(Borders::ALL).title(title));
    frame.render_widget(paragraph, popup);
}

// Finding F7: the issue asks for "no output count" in both the help
// text and DESIGN.md — only the latter had it. Every line here also
// fits inside `HELP_OVERLAY_WIDTH`'s own interior with no wrapping;
// the first line used to run to 65 columns in a 58-column interior,
// which wrapped and, since the overlay's own height was `len + 2`
// with no allowance for that, cut off "press any key to close" at
// the bottom.
const HELP_TEXT: &[&str] = &[
    "0            overview",
    "2            flow (today's layout)",
    "↑↓ / j k     move (the focused pane)",
    "PgUp PgDn    move a page",
    "Home End     jump to the first / last row",
    "g G          same as Home / End",
    "← →          collapse / expand (orchestration or state)",
    "⏎            expand a quick-steps row in place",
    "z            fold on/off (breadcrumbs, quick steps)",
    "tab          orchestration → details → state",
    "f            follow the running row (on by default)",
    "e            errors only",
    "/            filter",
    "v            show the full value",
    "t            cycle the colour theme",
    "c            cancel (confirm)",
    "q            quit (confirms while a run is going)",
    "?            this help",
    "",
    "no output token count until a prompt finishes",
    "press any key to close",
];

/// Wide enough for every [`HELP_TEXT`] line with none of them
/// wrapping (finding F7) — the longest (issue #535's own collapse/
/// expand line) is 55 columns; 4 columns of margin plus the 2-column
/// border leaves room to spare.
const HELP_OVERLAY_WIDTH: u16 = 61;

/// One redraw's whole layout (issue #626): a one-line header, shared
/// by every screen (replacing the old three-row one), then the
/// screen currently on display (`app.screen`) — screen 2 (`Flow`) is
/// DESIGN.md §6.3's own layout, otherwise unchanged: a left column
/// of the details pane over the orchestration pane, split at the
/// golden section both ways; a right column of the `tokens by key`
/// treemap, the state pane and the log pane; and the token bar
/// across the full bottom width. `overview` is screen 0's own model,
/// `Some` only while that screen is actually on display (issue
/// #626's own performance rule: "models for screens not on display
/// aren't built"). `raw_state` is the engine's own last snapshot,
/// untruncated — `details`' own `prompt_sent`/`value` are already
/// cut to `DETAILS_PREVIEW_CHARS` for the details pane, which `v`'s
/// own full-value overlay (finding 5) must not be.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    frame: &mut Frame,
    state: &oscilloscope_core::render::RenderState,
    details: Option<&Details>,
    log_lines: &[String],
    log_total: u64,
    app: &mut App,
    raw_state: Option<&serde_json::Value>,
    ctx: &DrawCtx,
    icon_frame: u64,
    now: &str,
    overview: Option<&oscilloscope_core::screens::Overview>,
) {
    let area = frame.area();
    if let Some(bg) = ctx.theme.background {
        // Finding 7: a named theme's own base fill must set both
        // roles, not just `bg` -- with only `bg` painted, a dark-
        // background theme on a *light* terminal (solarized-light,
        // gruvbox-light, catppuccin-latte) drew that terminal's own
        // default foreground over it, which can be as light as the
        // terminal's own default background, not the theme's `text`.
        frame.render_widget(
            Block::default().style(Style::default().bg(bg).fg(ctx.theme.text)),
            area,
        );
    }
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(5)])
        .split(area);

    let segments = oscilloscope_core::screens::header_line(
        &state.header,
        now,
        ctx.zone,
        area.width,
        !ctx.no_animation,
        icon_frame,
        Some(app.screen),
    );
    draw_header_line(frame, sections[0], &segments, ctx);

    let body = sections[1];
    match app.screen {
        oscilloscope_core::screens::Screen::Overview => {
            if let Some(overview) = overview {
                draw_overview(frame, body, overview, ctx);
            }
        }
        _ => draw_screen2(
            frame, area, body, state, details, log_lines, log_total, app, ctx,
        ),
    }

    draw_dialog_overlay(frame, area, app, details, raw_state, ctx);
}

/// [`draw`]'s own dialog overlays (help, confirm, the full-value and
/// filter popups) — unchanged by issue #626, just pulled out so
/// `draw` itself reads as "header, then the current screen, then
/// whatever's on top of it".
fn draw_dialog_overlay(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    details: Option<&Details>,
    raw_state: Option<&serde_json::Value>,
    ctx: &DrawCtx,
) {
    match &app.dialog {
        Dialog::Help => {
            let text = HELP_TEXT.iter().map(|l| Line::from(*l)).collect();
            draw_overlay(
                frame,
                area,
                " help ",
                text,
                HELP_OVERLAY_WIDTH,
                HELP_TEXT.len() as u16 + 2,
                ctx,
            );
        }
        Dialog::ConfirmCancel { .. } => {
            let text = vec![
                Line::from("cancel the run?"),
                Line::from(""),
                Line::from("y = cancel   n/esc = back"),
            ];
            draw_overlay(frame, area, " confirm ", text, 36, 5, ctx);
        }
        Dialog::FullValue(field) => {
            // Finding 5: reads straight from `raw_state`, bypassing
            // `details`' own truncated preview entirely — that's the
            // whole point of asking to see it in full.
            // The node the details pane is showing (#610: a state-pane
            // selection such as a named report), not the tree's own.
            let full = details
                .map(|d| d.path.as_str())
                .and_then(|p| oscilloscope_core::render::full_value(p, *field, raw_state));
            let text = vec![Line::from(full.unwrap_or_default())];
            draw_overlay(
                frame,
                area,
                " full value (press any key) ",
                text,
                area.width.saturating_sub(4),
                area.height.saturating_sub(4),
                ctx,
            );
        }
        Dialog::FilterInput(buf) => {
            let text = vec![
                Line::from(format!("/{buf}")),
                Line::from("enter = apply   esc = cancel"),
            ];
            draw_overlay(frame, area, " filter ", text, 40, 4, ctx);
        }
        Dialog::None => {}
    }
}

/// Screen 2 (`Flow`, DESIGN.md §6.3): today's layout, moved out of
/// [`draw`] when screens landed (issue #626) — every pane, threshold
/// and golden-section split below is exactly as it was, now fed a
/// `body` rect one row shorter (the header's own three rows are now
/// one).
#[allow(clippy::too_many_arguments)]
fn draw_screen2(
    frame: &mut Frame,
    frame_area: Rect,
    body: Rect,
    state: &oscilloscope_core::render::RenderState,
    details: Option<&Details>,
    log_lines: &[String],
    log_total: u64,
    app: &mut App,
    ctx: &DrawCtx,
) {
    let area = frame_area;
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(4)])
        .split(body);
    let body = sections[0];
    draw_token_bar(frame, sections[1], &state.header, &state.token_bar, ctx);

    let visible = visible_rows(&state.rows, app);
    let state_visible = crate::keys::visible_state_rows(&state.state_rows, app);
    // Only while the details follow the orchestration pane's selection:
    // a state-pane selection of a step folded into a `quick steps` row
    // still reads as that step's own kind.
    let details_row_kind = details
        .filter(|_| app.details_source == Pane::Tree)
        .and_then(|d| state.rows.iter().find(|r| r.path == d.path))
        .map(|r| r.kind);
    let two_column = area.width >= thresholds::TWO_COLUMN_MIN_WIDTH
        && area.height >= thresholds::TWO_COLUMN_MIN_HEIGHT;

    if !two_column {
        app.state_viewport = 0;
        // Issue #613: golden-ratio nested the same way the two-column
        // layout's own left column is -- the details-plus-tree block
        // gets the golden section's larger share of the height first
        // (log, which only ever grows by appending, takes the rest),
        // then that block splits again into details (minor) over the
        // tree (major, the pane that grows with the run).
        let log_rows = oscilloscope_core::layout::golden_section(body.height);
        let top_rows = body.height - log_rows;
        let details_rows = oscilloscope_core::layout::golden_section(top_rows);
        let parts = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(details_rows),
                Constraint::Length(top_rows - details_rows),
                Constraint::Min(log_rows),
            ])
            .split(body);
        draw_details(frame, parts[0], details, details_row_kind, app, ctx);
        draw_tree(frame, parts[1], &visible, app, &state.header, ctx);
        draw_log(frame, parts[2], log_lines, log_total, ctx);
    } else {
        let left_width = oscilloscope_core::layout::left_column_width(body.width);
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(left_width),
                Constraint::Length(body.width - left_width),
            ])
            .split(body);

        let details_rows = oscilloscope_core::layout::golden_section(columns[0].height);
        let left = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(details_rows),
                Constraint::Length(columns[0].height - details_rows),
            ])
            .split(columns[0]);
        draw_details(frame, left[0], details, details_row_kind, app, ctx);
        draw_tree(frame, left[1], &visible, app, &state.header, ctx);

        let show_treemap = area.width >= thresholds::TREEMAP_MIN_WIDTH
            && area.height >= thresholds::TREEMAP_MIN_HEIGHT;
        let show_state = area.width >= thresholds::STATE_PANE_MIN_WIDTH
            && area.height >= thresholds::STATE_PANE_MIN_HEIGHT;

        if show_state && show_treemap {
            // Issue #613: golden-ratio nested, top to bottom. The
            // treemap is a snapshot of right-now, not something that
            // grows with the run, so it takes the smaller share
            // first; what's left splits again between the state
            // pane (grows with the run, same reasoning as the
            // orchestration tree) and the log pane (grows only by
            // appending), the state pane keeping the larger share of
            // that remainder.
            let treemap_rows = oscilloscope_core::layout::golden_section(columns[1].height);
            let rest_rows = columns[1].height - treemap_rows;
            let log_rows = oscilloscope_core::layout::golden_section(rest_rows);
            let right = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(treemap_rows),
                    Constraint::Length(rest_rows - log_rows),
                    Constraint::Min(log_rows),
                ])
                .split(columns[1]);
            draw_treemap(
                frame,
                right[0],
                &state.treemap,
                app.selected_path.as_deref(),
                ctx,
            );
            draw_state(frame, right[1], &state_visible, app, ctx);
            draw_log(frame, right[2], log_lines, log_total, ctx);
        } else if show_state {
            // The state pane (grows with the run) keeps the golden
            // section's larger share; the log pane the rest.
            let log_rows = oscilloscope_core::layout::golden_section(columns[1].height);
            let right = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(columns[1].height - log_rows),
                    Constraint::Min(log_rows),
                ])
                .split(columns[1]);
            draw_state(frame, right[0], &state_visible, app, ctx);
            draw_log(frame, right[1], log_lines, log_total, ctx);
        } else {
            app.state_viewport = 0;
            draw_log(frame, columns[1], log_lines, log_total, ctx);
        }
    }
}

#[cfg(test)]
mod golden_tests {
    use super::*;
    use electricity_bytecode::{
        EffectPath, Escape, LeafKind, LoopFlow, LoopId, LoopSpec, NodeKind, OnError, Op, ParamNode,
        Program, PromptContent, PromptOp, PromptType, Region, TemplateText, ToolOp,
    };
    use oscilloscope_core::model::{ProcessState, RunModel};
    use oscilloscope_core::observe::Event;
    use oscilloscope_core::plan::PlanTree;
    use oscilloscope_core::render;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use serde_json::{Value, json};

    use crate::keys::App;

    fn tool(path: EffectPath, name: &str, on_error: OnError) -> Op {
        Op {
            path,
            name: Some(name.to_string()),
            kind: NodeKind::Leaf(Box::new(LeafKind::Tool(ToolOp {
                provider: "shell".to_string(),
                params: ParamNode::Literal(electricity_value::Value::None),
                params_json: None,
                prompt: None,
                model: None,
                timeout_ms: None,
                retries: Default::default(),
                expect: None,
                description: None,
                group: None,
                prompt_file: None,
            }))),
            on_error,
            labels: None,
            enabled: true,
        }
    }

    fn prompt(path: EffectPath, name: &str) -> Op {
        Op {
            path,
            name: Some(name.to_string()),
            kind: NodeKind::Leaf(Box::new(LeafKind::Prompt(PromptOp {
                content: PromptContent::Template(TemplateText::new("hi", false, Escape::None)),
                prompt_type: PromptType::Text,
                schema: None,
                model: None,
                provider: None,
                provider_fallbacks: Vec::new(),
                routing_override: None,
                model_params: None,
                timeout_ms: None,
                deterministic: false,
                inputs: None,
                assets: Vec::new(),
                retries: None,
                description: None,
                group: None,
            }))),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        }
    }

    /// A named tree loop (`fan`) whose body is a single prompt leaf
    /// (`ask`) -- a tree-loop pass's
    /// prompt is confirmed `Running` by events while never once
    /// appearing in live state at all (DESIGN.md §1.4), which
    /// `full_layout_with_a_running_tree_loop_prompt` below exercises.
    fn tree_loop_with_prompt_plan() -> PlanTree {
        let root_path = EffectPath::root();
        let loop_path = root_path.clone().push_name("fan");
        let pass_path = loop_path.clone().push_pass(LoopId(0));
        let ask_path = pass_path.push_name("ask");
        let root = Op {
            path: root_path,
            name: Some("prime".to_string()),
            kind: NodeKind::Control(Region::Block {
                ops: vec![Op {
                    path: loop_path,
                    name: Some("fan".to_string()),
                    kind: NodeKind::Control(Region::Loop {
                        name: Some("fan".to_string()),
                        spec: LoopSpec::Each {
                            in_path: "prime.items".to_string(),
                            as_name: "item".to_string(),
                            truncate: false,
                        },
                        body: Box::new(Region::Block {
                            ops: vec![prompt(ask_path, "ask")],
                            overlay: true,
                        }),
                        flow: LoopFlow::Tree,
                        max_concurrency: Some(2),
                        max_iterations: None,
                        min_iterations: 0,
                        collect: None,
                    }),
                    on_error: OnError::Fail,
                    labels: None,
                    enabled: true,
                }],
                overlay: false,
            }),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        let program = Program {
            root,
            prompts: Default::default(),
            effect_names: Default::default(),
            document: None,
            runtime_block: None,
            interface: None,
            adapter: None,
            model: None,
        };
        PlanTree::from_program(&program)
    }

    /// A plan shared by every golden test below (DESIGN.md §6.3): a
    /// chain of two leaves (`a` fails the run, `b`'s own failure is
    /// handled) followed by a named tree loop (`fan`, `max_concurrency
    /// 2`) with one leaf body (`nap`) -- enough to exercise every
    /// glyph the issue's acceptance list calls for from one plan.
    fn sample_plan() -> PlanTree {
        let root_path = EffectPath::root();
        let a_path = root_path.clone().push_name("a");
        let b_path = root_path.clone().push_name("b");
        let fan_path = root_path.clone().push_name("fan");
        let pass_path = fan_path.clone().push_pass(LoopId(0));
        let nap_path = pass_path.push_name("nap");

        let root = Op {
            path: root_path,
            name: Some("prime".to_string()),
            kind: NodeKind::Control(Region::Block {
                ops: vec![
                    tool(a_path, "a", OnError::Fail),
                    tool(b_path, "b", OnError::Continue),
                    Op {
                        path: fan_path,
                        name: Some("fan".to_string()),
                        kind: NodeKind::Control(Region::Loop {
                            name: Some("fan".to_string()),
                            spec: LoopSpec::Each {
                                in_path: "prime.items".to_string(),
                                as_name: "item".to_string(),
                                truncate: false,
                            },
                            body: Box::new(Region::Block {
                                ops: vec![tool(nap_path, "nap", OnError::Fail)],
                                overlay: true,
                            }),
                            flow: LoopFlow::Tree,
                            max_concurrency: Some(2),
                            max_iterations: None,
                            min_iterations: 0,
                            collect: None,
                        }),
                        on_error: OnError::Fail,
                        labels: None,
                        enabled: true,
                    },
                ],
                overlay: false,
            }),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        let program = Program {
            root,
            prompts: Default::default(),
            effect_names: Default::default(),
            document: None,
            runtime_block: None,
            interface: None,
            adapter: None,
            model: None,
        };
        PlanTree::from_program(&program)
    }

    /// A named *static* tree dynamic (`fan`, `max_concurrency 1`,
    /// two branches `a`/`b`) -- unlike `sample_plan`'s own `each` loop
    /// (a data-dependent pass count the compiler can't enumerate
    /// ahead of time), both branches have a real path of their own in
    /// the plan, so an unobserved one still gets its own row (review
    /// finding 3 on #458's comment 3: a queued ◌ row).
    fn tree_dynamic_plan() -> PlanTree {
        let root_path = EffectPath::root();
        let fan_path = root_path.clone().push_name("fan");
        let branch_a = fan_path.clone().push_name("a");
        let branch_b = fan_path.clone().push_name("b");
        let root = Op {
            path: root_path,
            name: Some("prime".to_string()),
            kind: NodeKind::Control(Region::Block {
                ops: vec![Op {
                    path: fan_path,
                    name: Some("fan".to_string()),
                    kind: NodeKind::Control(Region::Parallel {
                        branches: vec![
                            tool(branch_a, "a", OnError::Fail),
                            tool(branch_b, "b", OnError::Fail),
                        ],
                        max_concurrency: Some(1),
                        stop_on_error: false,
                    }),
                    on_error: OnError::Fail,
                    labels: None,
                    enabled: true,
                }],
                overlay: false,
            }),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        let program = Program {
            root,
            prompts: Default::default(),
            effect_names: Default::default(),
            document: None,
            runtime_block: None,
            interface: None,
            adapter: None,
            model: None,
        };
        PlanTree::from_program(&program)
    }

    /// Every golden snapshot here funnels through this one draw call
    /// (finding F5/F1): the terminal size is an explicit parameter so
    /// a test can pick whichever fallback tier it means to exercise,
    /// rather than every golden defaulting to the same size
    /// regardless of what it's meant to show.
    #[allow(clippy::too_many_arguments)]
    fn snapshot_at(
        width: u16,
        height: u16,
        plan: &PlanTree,
        model: &mut RunModel,
        state: Option<&Value>,
        process: ProcessState,
        engine: &str,
        selected: &str,
        log_lines: &[String],
    ) -> String {
        let rendered = render::build(
            "do-thing.yml",
            engine,
            plan,
            model,
            state,
            process,
            5.0,
            "2026-01-01T00:00:05Z",
            &[],
        );
        let details = render::details_for(selected, plan, model, state);
        let mut app = App::new(engine == "watch");
        app.screen = oscilloscope_core::screens::Screen::Flow;
        app.selected_path = Some(selected.to_string());
        // A regression: setting `selected_path`
        // directly (as every golden here does, to pin a specific row's
        // details) bypasses the key handling that would otherwise have
        // turned follow off for a manual selection -- left at its
        // `App::new` default of `true`, the title claimed "(following)"
        // over whatever row the test happened to pick, even one that
        // wasn't running at all. A test that means to show follow
        // actually choosing the running row builds its own `App`
        // instead of calling this helper.
        app.follow = false;
        let test_theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let test_tracker = crate::highlight::Tracker::new();
        let test_ctx = DrawCtx {
            theme: &test_theme,
            highlights: &test_tracker,
            now: Instant::now(),
            color_mode: ColorMode::TrueColor,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    Some(&details),
                    log_lines,
                    log_lines.len() as u64,
                    &mut app,
                    state,
                    &test_ctx,
                    0,
                    "2026-01-01T00:00:00Z",
                    None,
                )
            })
            .unwrap();
        terminal.backend().to_string()
    }

    /// [`snapshot_at`], for screen 0 (the overview, issue #626):
    /// builds the same `RenderState` plus the overview model that
    /// only gets built while that screen is on display.
    fn overview_snapshot_at(
        width: u16,
        height: u16,
        plan: &PlanTree,
        model: &mut RunModel,
        state: Option<&Value>,
        process: ProcessState,
    ) -> String {
        let rendered = render::build_folded(
            "do-thing.yml",
            "cof",
            plan,
            model,
            state,
            process.clone(),
            300.0,
            "2026-01-01T00:05:00Z",
            &[],
            true,
            &Default::default(),
        );
        let empty = Value::Null;
        let flat = oscilloscope_core::model::flatten_state(state.unwrap_or(&empty));
        let overview = oscilloscope_core::screens::build_overview(
            &rendered.rows,
            &rendered.unfolded_rows,
            &rendered.header,
            &flat,
            plan,
            model,
            state,
            &rendered.token_bar,
            &rendered.treemap,
            &rendered.log,
            None,
            "2026-01-01T00:05:00Z",
        );
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Overview;
        let test_theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let test_tracker = crate::highlight::Tracker::new();
        let test_ctx = DrawCtx {
            theme: &test_theme,
            highlights: &test_tracker,
            now: Instant::now(),
            color_mode: ColorMode::TrueColor,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    None,
                    &[],
                    0,
                    &mut app,
                    state,
                    &test_ctx,
                    0,
                    "2026-01-01T00:05:00Z",
                    Some(&overview),
                )
            })
            .unwrap();
        terminal.backend().to_string()
    }

    fn overview_running_state() -> Value {
        json!({"prime": {"value": null, "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": null},
            "a": {"value": "ok\n", "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:01:00Z", "provider": "shell", "exit_code": 0}},
            "b": {"value": null, "meta": {"created_at": "2026-01-01T00:01:00Z", "completed_at": null, "provider": "shell"}}
        }})
    }

    fn overview_finished_state() -> Value {
        json!({
            "runtime": {"last_run": {"completed_at": "2026-01-01T00:05:00Z", "totals": {"wall_time_s": 300.0, "effects_run": 2, "tokens_sent": 10, "tokens_received": 20, "cost_usd": 1.5}}},
            "prime": {"value": true, "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:05:00Z", "error": null},
            "a": {"value": "ok\n", "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:01:00Z", "provider": "shell", "exit_code": 0}},
            "b": {"value": "ok\n", "meta": {"created_at": "2026-01-01T00:01:00Z", "completed_at": "2026-01-01T00:02:00Z", "provider": "shell", "exit_code": 0}}
        }})
    }

    fn overview_failed_state() -> Value {
        json!({
            "runtime": {"last_run": {"completed_at": "2026-01-01T00:05:00Z", "totals": {"wall_time_s": 300.0, "effects_run": 2, "tokens_sent": 10, "tokens_received": 20, "cost_usd": 0.0}}},
            "prime": {"value": false, "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:05:00Z", "error": "a: /bin/false failed (exit 1)"},
            "a": {"value": null, "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:01:00Z", "provider": "shell", "exit_code": null, "error": "/bin/false failed (exit 1)"}}
        }})
    }

    #[test]
    fn overview_running_at_120x40() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = overview_running_state();
        let text = overview_snapshot_at(
            120,
            40,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn overview_running_at_100x30() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = overview_running_state();
        let text = overview_snapshot_at(
            100,
            30,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn overview_running_at_80x24() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = overview_running_state();
        let text = overview_snapshot_at(
            80,
            24,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn overview_running_at_70x20() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = overview_running_state();
        let text = overview_snapshot_at(
            70,
            20,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn overview_finished_at_120x40() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = overview_finished_state();
        let process = ProcessState::exited(false);
        let text = overview_snapshot_at(120, 40, &plan, &mut model, Some(&state), process);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn overview_finished_at_100x30() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = overview_finished_state();
        let process = ProcessState::exited(false);
        let text = overview_snapshot_at(100, 30, &plan, &mut model, Some(&state), process);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn overview_finished_at_80x24() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = overview_finished_state();
        let process = ProcessState::exited(false);
        let text = overview_snapshot_at(80, 24, &plan, &mut model, Some(&state), process);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn overview_finished_at_70x20() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = overview_finished_state();
        let process = ProcessState::exited(false);
        let text = overview_snapshot_at(70, 20, &plan, &mut model, Some(&state), process);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn overview_failed_at_120x40() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = overview_failed_state();
        let process = ProcessState::exited(false);
        let text = overview_snapshot_at(120, 40, &plan, &mut model, Some(&state), process);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn overview_failed_at_100x30() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = overview_failed_state();
        let process = ProcessState::exited(false);
        let text = overview_snapshot_at(100, 30, &plan, &mut model, Some(&state), process);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn overview_failed_at_80x24() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = overview_failed_state();
        let process = ProcessState::exited(false);
        let text = overview_snapshot_at(80, 24, &plan, &mut model, Some(&state), process);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn overview_failed_at_70x20() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = overview_failed_state();
        let process = ProcessState::exited(false);
        let text = overview_snapshot_at(70, 20, &plan, &mut model, Some(&state), process);
        insta::assert_snapshot!(text);
    }

    /// Finding F5: a two-column size (still well under
    /// [`thresholds::TREEMAP_MIN_WIDTH`]/`_MIN_HEIGHT`, so the right
    /// column is the state pane over the log pane, no treemap) — most
    /// goldens here want this, since a one-column layout at 70×20
    /// drops the error display, the loop pass rows and everything but
    /// `path` from the details pane, which left these goldens checking
    /// far less than they look like they do. [`snapshot_one_column`]
    /// keeps a couple of them at the smaller size deliberately, as the
    /// one-column fallback's own coverage.
    #[allow(clippy::too_many_arguments)]
    fn snapshot(
        plan: &PlanTree,
        model: &mut RunModel,
        state: Option<&Value>,
        process: ProcessState,
        engine: &str,
        selected: &str,
    ) -> String {
        snapshot_at(100, 24, plan, model, state, process, engine, selected, &[])
    }

    /// [`snapshot`], at 70×20 — below
    /// [`thresholds::TWO_COLUMN_MIN_WIDTH`], so the one-column
    /// fallback (issue #535: details/orchestration/log stacked, in
    /// that order) is what gets exercised.
    #[allow(clippy::too_many_arguments)]
    fn snapshot_one_column(
        plan: &PlanTree,
        model: &mut RunModel,
        state: Option<&Value>,
        process: ProcessState,
        engine: &str,
        selected: &str,
    ) -> String {
        snapshot_at(70, 20, plan, model, state, process, engine, selected, &[])
    }

    /// [`snapshot`], with the log pane given real lines (review
    /// finding 3 on #458's comment 3: every golden here left it
    /// empty) -- `snapshot` itself is just this with none. `log_lines`
    /// is the plain, already-formatted `HH:MM:SS text` strings `draw`
    /// itself takes (`do_run`'s own `log_lines: Vec<String>`), not
    /// `render::build`'s own `log: &[LogLine]` -- the TUI's log pane
    /// never reads the latter.
    #[allow(clippy::too_many_arguments)]
    fn snapshot_with_log(
        plan: &PlanTree,
        model: &mut RunModel,
        state: Option<&Value>,
        process: ProcessState,
        engine: &str,
        selected: &str,
        log_lines: &[String],
    ) -> String {
        snapshot_at(
            100, 24, plan, model, state, process, engine, selected, log_lines,
        )
    }

    /// [`snapshot_with_log`], at a terminal big enough for every pane
    /// (issue #527's own layout, "on a 120×40 terminal every pane
    /// shows useful content").
    #[allow(clippy::too_many_arguments)]
    fn snapshot_full_layout(
        plan: &PlanTree,
        model: &mut RunModel,
        state: Option<&Value>,
        process: ProcessState,
        engine: &str,
        selected: &str,
        log_lines: &[String],
    ) -> String {
        snapshot_at(
            120, 40, plan, model, state, process, engine, selected, log_lines,
        )
    }

    #[test]
    fn pending_plan() {
        // Finding F5: kept at 70×20 deliberately, as one of this
        // suite's own one-column-fallback goldens.
        let plan = sample_plan();
        let mut model = RunModel::new();
        let text = snapshot_one_column(
            &plan,
            &mut model,
            None,
            ProcessState::Running,
            "cof",
            "prime.a",
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn running_chain() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": null, "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": null, "provider": "shell"}}
        }});
        let text = snapshot(
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            "cof",
            "prime.a",
        );
        insta::assert_snapshot!(text);
    }

    /// Issue #613's folding, drawn end to end through `render::
    /// build_folded`: `fix` and its one child `agent` are both still
    /// `Running`, so they fold into one breadcrumb row with the
    /// running `agent`'s own path (so its `6.8s` elapsed and
    /// `Running` status are the breadcrumb's own); `review`, already
    /// `Done`, does not fold with its still-running child `fix` --
    /// their statuses differ -- so both rows stay. Three
    /// sub-second finished tool steps fold into one `3 quick steps`
    /// row.
    #[test]
    fn folding_breadcrumbs_and_quick_steps() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "review": {"value": null, "meta": {"created_at": "2026-01-01T11:39:26Z", "completed_at": "2026-01-01T11:46:39Z", "error": null},
                "fix": {"value": null, "meta": {"created_at": "2026-01-01T11:46:40Z", "completed_at": null},
                    "agent": {"value": null, "meta": {"created_at": "2026-01-01T11:46:43Z", "completed_at": null, "provider": "agent"}}
                }
            },
            "q1": {"value": "", "meta": {"created_at": "2026-01-01T11:46:49.000Z", "completed_at": "2026-01-01T11:46:49.300Z", "error": null, "provider": "shell"}},
            "q2": {"value": "", "meta": {"created_at": "2026-01-01T11:46:49.300Z", "completed_at": "2026-01-01T11:46:49.800Z", "error": null, "provider": "shell"}},
            "q3": {"value": "", "meta": {"created_at": "2026-01-01T11:46:49.800Z", "completed_at": "2026-01-01T11:46:50.000Z", "error": null, "provider": "shell"}}
        }});
        let rendered = render::build_folded(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            390.0,
            "2026-01-01T11:46:49.800Z",
            &[],
            true,
            &std::collections::BTreeSet::new(),
        );
        let details = render::details_for(
            "prime.review.fix.agent",
            &plan,
            &RunModel::new(),
            Some(&state),
        );
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Flow;
        app.selected_path = Some("prime.review.fix.agent".to_string());
        app.follow = false;
        let test_theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let test_tracker = crate::highlight::Tracker::new();
        let test_ctx = DrawCtx {
            theme: &test_theme,
            highlights: &test_tracker,
            now: Instant::now(),
            color_mode: ColorMode::TrueColor,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    Some(&details),
                    &[],
                    0,
                    &mut app,
                    Some(&state),
                    &test_ctx,
                    0,
                    "2026-01-01T00:00:00Z",
                    None,
                )
            })
            .unwrap();
        let text = terminal.backend().to_string();
        insta::assert_snapshot!(text);
    }

    #[test]
    fn the_full_value_overlay_opens_the_node_the_details_pane_shows() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "TREE-ROW-VALUE", "meta": {"created_at": "2026-01-01T11:46:49.000Z", "completed_at": "2026-01-01T11:46:50.000Z", "error": null, "provider": "shell"}},
            "status": {"value": {"message": "m", "level": "info", "data": {"k": "REPORT-DATA"}},
                "meta": {"created_at": "2026-01-01T11:46:50.000Z", "completed_at": "2026-01-01T11:46:50.000Z"}}
        }});
        let rendered = render::build_folded(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            1.0,
            "2026-01-01T11:46:51.000Z",
            &[],
            true,
            &std::collections::BTreeSet::new(),
        );
        // The tree selection is `prime.a`; the details pane shows the
        // named report picked in the state pane.
        let details = render::details_for("prime.status", &plan, &model, Some(&state));
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Flow;
        app.selected_path = Some("prime.a".to_string());
        app.state_selected_path = Some("status".to_string());
        app.details_source = Pane::State;
        app.follow = false;
        app.dialog = Dialog::FullValue(oscilloscope_core::render::FullValueField::Value);
        let test_theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let test_tracker = crate::highlight::Tracker::new();
        let test_ctx = DrawCtx {
            theme: &test_theme,
            highlights: &test_tracker,
            now: Instant::now(),
            color_mode: ColorMode::TrueColor,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    Some(&details),
                    &[],
                    0,
                    &mut app,
                    Some(&state),
                    &test_ctx,
                    0,
                    "2026-01-01T00:00:00Z",
                    None,
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let screen: String = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(screen.contains("full value"), "{screen}");
        assert!(screen.contains("REPORT-DATA"), "{screen}");
        assert!(!screen.contains("TREE-ROW-VALUE"), "{screen}");
    }

    #[test]
    fn a_reports_details_show_its_time_as_a_clock_time() {
        let details = Details {
            path: "prime.status".to_string(),
            kind: RowKind::Report,
            meta_summary: vec![
                ("level".to_string(), "info".to_string()),
                ("message".to_string(), "hello".to_string()),
                ("time".to_string(), "2026-01-01T11:46:49.300Z".to_string()),
            ],
            ..Details::default()
        };
        let test_theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let test_tracker = crate::highlight::Tracker::new();
        let test_ctx = DrawCtx {
            theme: &test_theme,
            highlights: &test_tracker,
            now: Instant::now(),
            color_mode: ColorMode::TrueColor,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let text: Vec<String> = details_lines(&details, None, &test_ctx, 80)
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert!(text.iter().any(|l| l == "time     11:46:49"), "{text:?}");
    }

    #[test]
    fn a_selected_quick_steps_row_reads_quick_steps_in_the_details_pane() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "q1": {"value": "", "meta": {"created_at": "2026-01-01T11:46:49.000Z", "completed_at": "2026-01-01T11:46:49.300Z", "error": null, "provider": "shell"}},
            "q2": {"value": "", "meta": {"created_at": "2026-01-01T11:46:49.300Z", "completed_at": "2026-01-01T11:46:49.800Z", "error": null, "provider": "shell"}}
        }});
        let rendered = render::build_folded(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            1.0,
            "2026-01-01T11:46:50.000Z",
            &[],
            true,
            &std::collections::BTreeSet::new(),
        );
        assert!(
            rendered
                .rows
                .iter()
                .any(|r| r.path == "prime.q1" && r.kind == RowKind::QuickSteps),
            "the two quick steps fold into one row keyed on the first step"
        );
        let details = render::details_for("prime.q1", &plan, &RunModel::new(), Some(&state));
        let test_theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let test_tracker = crate::highlight::Tracker::new();
        let test_ctx = DrawCtx {
            theme: &test_theme,
            highlights: &test_tracker,
            now: Instant::now(),
            color_mode: ColorMode::TrueColor,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let screen = |source: crate::keys::Pane| {
            let mut app = App::new(false);
            app.screen = oscilloscope_core::screens::Screen::Flow;
            app.selected_path = Some("prime.q1".to_string());
            app.details_source = source;
            app.follow = false;
            let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
            terminal
                .draw(|f| {
                    draw(
                        f,
                        &rendered,
                        Some(&details),
                        &[],
                        0,
                        &mut app,
                        Some(&state),
                        &test_ctx,
                        0,
                        "2026-01-01T00:00:00Z",
                        None,
                    )
                })
                .unwrap();
            let buffer = terminal.backend().buffer().clone();
            (0..buffer.area.height)
                .map(|y| {
                    (0..buffer.area.width)
                        .map(|x| buffer[(x, y)].symbol().to_string())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        assert!(screen(crate::keys::Pane::Tree).contains("type   quick steps"));
        // Selected from the state pane, the same node is still its own step.
        assert!(!screen(crate::keys::Pane::State).contains("type   quick steps"));
    }
    #[test]
    fn running_tree_loop_with_max_concurrency() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        model.observe_event(&Event::Dispatch {
            ts: "2026-01-01T00:00:00Z".to_string(),
            path: "prime.fan".to_string(),
            branches: 4,
            concurrency: Some(2),
        });
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "error": null, "provider": "shell"}},
            "b": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "error": null, "provider": "shell"}},
            "fan": {"value": null, "meta": {"completed_at": null, "progress": {"done": 1, "total": 4, "elapsed_s": 1.0, "eta_s": 3.0}},
                "iter_0": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "error": null, "provider": "shell"}}
            }
        }});
        let text = snapshot(
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            "cof",
            "prime.fan",
        );
        insta::assert_snapshot!(text);
    }

    /// Review finding 6: with two passes of a known four-pass loop
    /// already done, "effects done out of planned" must multiply the
    /// loop body's own single leaf template by the loop's `progress.
    /// total` (6 planned: `a`, `b`, four `nap` passes) rather than
    /// counting `nap`'s own template path once (3) — and `done` must
    /// count each finished pass separately (4: `a`, `b`, `iter_0`,
    /// `iter_1`), not just leaves-with-a-template (3).
    #[test]
    fn running_tree_loop_with_two_passes_done() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        model.observe_event(&oscilloscope_core::observe::Event::Dispatch {
            ts: "2026-01-01T00:00:00Z".to_string(),
            path: "prime.fan".to_string(),
            branches: 4,
            concurrency: Some(2),
        });
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "error": null, "provider": "shell"}},
            "b": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "error": null, "provider": "shell"}},
            "fan": {"value": null, "meta": {"completed_at": null, "progress": {"done": 2, "total": 4, "elapsed_s": 2.0, "eta_s": 2.0}},
                "iter_0": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "error": null, "provider": "shell"}},
                "iter_1": {"value": "ok\n", "meta": {"created_at": "t1", "completed_at": "t2", "error": null, "provider": "shell"}}
            }
        }});
        let text = snapshot(
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            "cof",
            "prime.fan",
        );
        insta::assert_snapshot!(text);
    }

    /// Review finding 3 on #458's comment 3: a queued ◌ row, with
    /// `fan`'s own `max_concurrency 1` keeping `b` from running
    /// alongside `a`.
    #[test]
    fn queued_row_under_a_concurrency_bound() {
        let plan = tree_dynamic_plan();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "fan": {"value": null, "meta": {"completed_at": null},
                "a": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "provider": "shell"}}
            }
        }});
        let text = snapshot(
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            "cof",
            "prime.fan.b",
        );
        insta::assert_snapshot!(text);
    }

    /// Review finding 3 on #458's comment 3: the log pane with real
    /// lines, not left empty the way every other golden here leaves
    /// it.
    #[test]
    fn log_pane_with_lines() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "error": null, "provider": "shell"}}
        }});
        let log_lines = vec![
            "11:04:58 ▶ prime.a  tool".to_string(),
            "11:04:58 ✓ prime.a  0.5s  ok".to_string(),
        ];
        let text = snapshot_with_log(
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            "cof",
            "prime.a",
            &log_lines,
        );
        insta::assert_snapshot!(text);
    }

    /// #610: once a report has arrived, the header shows it
    /// (warning-styled) in place of the run id, and the log pane
    /// carries the report's own `▸`/`▸ warning:` line, data preview
    /// included.
    #[test]
    fn header_and_log_show_a_report() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        model.observe_event(&Event::RunStart {
            ts: "2026-01-01T11:04:00Z".to_string(),
            run_id: "run-abc123".to_string(),
            pid: Some(1),
        });
        model.observe_event(&Event::Report {
            ts: "2026-01-01T11:04:58Z".to_string(),
            path: "prime".to_string(),
            level: oscilloscope_core::observe::ReportLevel::Warning,
            message: "running behind schedule".to_string(),
            data: Some(json!({"behind_s": 42})),
            data_truncated: false,
        });
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "error": null, "provider": "shell"}}
        }});
        let log_lines =
            vec!["11:04:58 ▸ warning: running behind schedule  {\"behind_s\":42}".to_string()];
        let text = snapshot_with_log(
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            "cof",
            "prime.a",
            &log_lines,
        );
        insta::assert_snapshot!(text);
    }

    /// Issue #626 moved the header line's own truncation order (the
    /// tabs shrink first, then the report, then the document, then
    /// the ETA) into `oscilloscope_core::screens::header_line` itself
    /// -- `oscilloscope-core`'s own tests cover the order directly
    /// now (`header_line_shrinks_tabs_to_numbers_first`, `header_
    /// line_truncates_report_before_document_at_80_columns`,
    /// `header_line_drops_eta_before_status_at_50_columns`). This is
    /// the one place left here that still shows a report actually
    /// reaching the screen through `draw`'s own one-line header.
    #[test]
    fn header_report_collapses_to_a_bare_mark_when_almost_no_room_is_left() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        model.observe_event(&Event::Report {
            ts: "t".to_string(),
            path: "prime".to_string(),
            level: oscilloscope_core::observe::ReportLevel::Warning,
            message: "running behind schedule".to_string(),
            data: None,
            data_truncated: false,
        });
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "error": null, "provider": "shell"}}
        }});
        let text = snapshot_at(
            54,
            24,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            "cof",
            "prime.a",
            &[],
        );
        let line = text.lines().next().unwrap();
        assert!(line.contains("▸"), "{line}");
        assert!(!line.contains("running behind"), "{line}");
        assert!(!line.contains("warning:"), "{line}");
    }

    /// #610: a warning-level report is styled in the theme's own
    /// warning colour, both in the header's own status-line slot and
    /// in its `--log`/log-pane line -- an info-level one gets neither.
    #[test]
    fn a_warning_report_is_styled_in_the_themes_warning_colour_in_the_header_and_the_log() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        model.observe_event(&Event::Report {
            ts: "t".to_string(),
            path: "prime".to_string(),
            level: oscilloscope_core::observe::ReportLevel::Warning,
            message: "behind".to_string(),
            data: None,
            data_truncated: false,
        });
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "error": null, "provider": "shell"}}
        }});
        let rendered = render::build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            5.0,
            "2026-01-01T00:00:05Z",
            &[],
        );
        let theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Dracula);
        let tracker = Tracker::new();
        let ctx = DrawCtx {
            theme: &theme,
            highlights: &tracker,
            now: Instant::now(),
            color_mode: ColorMode::TrueColor,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Flow;
        app.selected_path = Some("prime.a".to_string());
        app.follow = false;
        let log_lines = vec!["11:04:58 ▸ warning: behind".to_string()];
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    None,
                    &log_lines,
                    log_lines.len() as u64,
                    &mut app,
                    Some(&state),
                    &ctx,
                    0,
                    "2026-01-01T00:00:00Z",
                    None,
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();

        fn find_cell(buffer: &ratatui::buffer::Buffer, needle: char) -> (u16, u16) {
            for y in 0..buffer.area.height {
                for x in 0..buffer.area.width {
                    if buffer[(x, y)].symbol() == needle.to_string() {
                        return (x, y);
                    }
                }
            }
            panic!("{needle:?} not found in the buffer");
        }

        let (hx, hy) = find_cell(buffer, '▸');
        assert_eq!(
            buffer[(hx, hy)].fg,
            theme.warning,
            "the header's own report mark must use the theme's warning colour"
        );
        // The second `▸` in the buffer, reading left to right then
        // top to bottom, is the log pane's own -- the header's own row
        // comes first.
        let mut found = 0;
        let mut log_cell = None;
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                if buffer[(x, y)].symbol() == "▸" {
                    found += 1;
                    if found == 2 {
                        log_cell = Some((x, y));
                    }
                }
            }
        }
        let (lx, ly) = log_cell.expect("a second ▸ for the log pane's own line");
        assert_eq!(
            buffer[(lx, ly)].fg,
            theme.warning,
            "the log pane's own warning report line must use the theme's warning colour"
        );
    }

    /// #610: a named report writes a state node (#605), but it
    /// must never be a row in the orchestration tree, even once
    /// selected elsewhere brings up its own details.
    #[test]
    fn tree_has_no_row_for_a_named_report() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        // `sample_plan` has no report of its own to mark
        // `prime.status` through `PlanTree::is_report_path` -- the
        // same `--events` `report` line a real engine would also
        // emit for it is what `RunModel::observe` falls back to
        // instead (the "no plan" rule), exactly as `osi watch` or a
        // `use: inline:`/`ref:` child would have to rely on it too.
        model.observe_event(&Event::Report {
            ts: "t0".to_string(),
            path: "prime.status".to_string(),
            level: oscilloscope_core::observe::ReportLevel::Info,
            message: "on track".to_string(),
            data: None,
            data_truncated: false,
        });
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "error": null, "provider": "shell"}},
            "status": {"value": {"message": "on track", "level": "info", "data": null},
                "meta": {"created_at": "t0", "completed_at": "t0"}}
        }});
        let text = snapshot(
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            "cof",
            "prime.a",
        );
        // The report's own state node still appears in the *state*
        // pane (its own details apply there) -- only the
        // orchestration pane itself must never show a `status` row.
        let orchestration_pane = text
            .split_once("orchestration do-thing")
            .map(|(_, after)| after)
            .and_then(|after| after.split_once("tokens").map(|(before, _)| before))
            .expect("orchestration pane should be in the snapshot");
        assert!(
            !orchestration_pane.contains("status"),
            "a named report must never show as a row in the orchestration tree: {orchestration_pane}"
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn failed() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = json!({
            "runtime": {"last_run": {"completed_at": "t1"}},
            "prime": {"value": null, "meta": {"completed_at": "t1", "error": "prime.a: /bin/ls failed (exit 1): ls: no"},
            "a": {"value": null, "meta": {"created_at": "t0", "completed_at": "t1", "error": "/bin/ls failed (exit 1): ls: no", "provider": "shell"}}
        }});
        let text = snapshot(
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(false),
            "cof",
            "prime.a",
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn failed_and_handled() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = json!({
            "runtime": {"last_run": {"completed_at": "t1"}},
            "prime": {"value": true, "meta": {"completed_at": "t1", "error": null},
            "a": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "error": null, "provider": "shell"}},
            "b": {"value": null, "meta": {"created_at": "t0", "completed_at": "t1", "error": "/bin/ls failed (exit 1): ls: no", "provider": "shell"}}
        }});
        let text = snapshot(
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(false),
            "cof",
            "prime.b",
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn cancelled_row_with_no_final_write_yet() {
        // The process exited and osi itself forwarded a signal
        // (`interrupted: true`), but the state it has is a snapshot
        // from *before* the final write landed (no `runtime` key at
        // all) -- a transient race, not an ended run. `prime.a`'s own
        // row still reads cancelled (DESIGN.md §2.1 rule 2), but the
        // header, which has no sign this run specifically failed, now
        // correctly reads "aborted" rather than reusing the row's own
        // `interrupted` flag (see
        // `header_reads_cancelled_once_the_final_state_lands` below for
        // the real cancelled-header case).
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "provider": "shell"}}
        }});
        let text = snapshot(
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(true),
            "cof",
            "prime.a",
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn header_reads_cancelled_once_the_final_state_lands() {
        // A regression: a run the user cancelled
        // (first SIGINT) always gets a final write before cof exits
        // (DESIGN.md §4.1) -- `runtime.last_run.completed_at` set and
        // `prime.meta.error` starting with "Interrupted". The header
        // must read "cancelled", not "failed".
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = json!({
            "runtime": {"last_run": {"completed_at": "t1"}},
            "prime": {"value": false, "meta": {"completed_at": "t1", "error": "Interrupted (Ctrl-C/SIGINT)"},
            "a": {"value": null, "meta": {"created_at": "t0", "completed_at": "t1", "error": null, "provider": "shell"}}
        }});
        let text = snapshot(
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(true),
            "cof",
            "prime.a",
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn aborted_no_final_state() {
        // Finding F5: the other one-column-fallback golden kept at
        // 70×20.
        let plan = sample_plan();
        let mut model = RunModel::new();
        let text = snapshot_one_column(
            &plan,
            &mut model,
            None,
            ProcessState::exited(false),
            "cof",
            "prime.a",
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn header_reads_failed_for_an_invalid_document_with_no_final_state() {
        // A regression: an invalid document
        // (cof exits 1 before execution, with no state file and its
        // own `{"ok": false, "error": ...}` JSON on stdout) must read
        // "failed", not "aborted" (DESIGN.md §2.1 rule 7's "no ended
        // state" branch).
        let plan = sample_plan();
        let mut model = RunModel::new();
        let text = snapshot(
            &plan,
            &mut model,
            None,
            ProcessState::Exited {
                interrupted: false,
                likely_failed: true,
                has_failure_reason: true,
                exited_at: None,
            },
            "cof",
            "prime.a",
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn osi_watch() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "provider": "shell"}}
        }});
        let text = snapshot(
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            "watch",
            "prime.a",
        );
        insta::assert_snapshot!(text);
    }

    /// Issue #527's own full layout, on a terminal big enough for
    /// every pane: the state pane, a nested `tokens by key` treemap
    /// (`fan`'s own container cell holding `iter_0`), and the token
    /// bar's totals and in-flight line, together with the
    /// orchestration pane's `(following)` title.
    #[test]
    fn full_layout_with_state_treemap_and_token_bar() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "the summary text", "meta": {"created_at": "t0", "completed_at": "t1", "adapter": "scripted", "tokens_sent_total": 120, "tokens_received_total": 340}},
            "b": {"value": "ok\n", "meta": {"created_at": "t1", "completed_at": "t2", "provider": "shell"}},
            "fan": {"value": null, "meta": {"completed_at": null, "progress": {"done": 1, "total": 4}},
                "iter_0": {"value": "the first pass reply", "meta": {"created_at": "t2", "completed_at": null, "adapter": "scripted", "prompt_sent": "summarize the first pass", "waiting_for": null, "tokens_sent_total": 50, "tokens_received_total": 70}}
            }
        }});
        let text = snapshot_full_layout(
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            "cof",
            "prime.a",
            &[],
        );
        insta::assert_snapshot!(text);
    }

    /// Finding F6/M1: a selected, finished prompt at 120×40 -- the
    /// `sent`/`received` blocks must land right after `path`/`type`,
    /// before `plan`/the meta list, and each snippet must cap at
    /// [`SNIPPET_MAX_LINES`] wrapped lines so both blocks stay on
    /// screen together rather than one pushing the other past the
    /// details pane's own golden-section height.
    #[test]
    fn full_layout_with_a_selected_finished_prompts_sent_received_blocks() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let long_prompt = "summarize the quarterly report and call out every risk factor you can find in the appendix tables, in plain prose, no bullet points";
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "the summary text", "meta": {"created_at": "t0", "completed_at": "t1", "adapter": "scripted", "prompt_sent": long_prompt, "tokens_sent_total": 120, "tokens_received_total": 340}},
            "b": {"value": "ok\n", "meta": {"created_at": "t1", "completed_at": "t2", "provider": "shell"}}
        }});
        let text = snapshot_full_layout(
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            "cof",
            "prime.a",
            &[],
        );
        insta::assert_snapshot!(text);
    }

    /// Finding F1: the two boundary sizes between the full layout and
    /// the one-column fallback, each exercising a different tier
    /// [`thresholds`] documents -- read alongside
    /// [`full_layout_with_state_treemap_and_token_bar`] (120×40) and
    /// [`pending_plan`]/[`aborted_no_final_state`] (70×20, below
    /// `TWO_COLUMN_MIN_WIDTH`/`_MIN_HEIGHT`), every tier the acceptance
    /// list's own fallback order names has a golden.
    #[test]
    fn fallback_at_100x30_is_the_smallest_full_layout() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}},
            "fan": {"value": null, "meta": {"completed_at": null, "progress": {"done": 1, "total": 4}},
                "iter_0": {"value": "the reply", "meta": {"created_at": "t1", "completed_at": null, "adapter": "scripted", "prompt_sent": "ask", "waiting_for": null, "tokens_sent_total": 10, "tokens_received_total": 15}}
            }
        }});
        let text = snapshot_at(
            100,
            30,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            "cof",
            "prime.a",
            &[],
        );
        insta::assert_snapshot!(text);
    }

    /// At 80×24: two-column (both `TWO_COLUMN_MIN_WIDTH`/`_MIN_HEIGHT`
    /// are met), but below both `TREEMAP_MIN_WIDTH` (100) and
    /// `STATE_PANE_MIN_WIDTH` (90) -- the right column is the log pane
    /// alone.
    #[test]
    fn fallback_at_80x24_drops_the_treemap_and_the_state_pane() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}}
        }});
        let text = snapshot_at(
            80,
            24,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            "cof",
            "prime.a",
            &[],
        );
        insta::assert_snapshot!(text);
    }

    /// A regression: the existing goldens above
    /// all hard-code `selected_path` directly, which (correctly, see
    /// `snapshot_at`'s own comment) now also turns follow off, so none
    /// of them actually show follow choosing anything. This one builds
    /// its own `App` instead of using the `snapshot*` helpers, the
    /// same way `main.rs`'s own redraw loop updates the selection: via
    /// `App::follow_target`, never a hard-coded path -- with `fan`'s
    /// own pass still running, follow must land on `fan.iter_0`, not
    /// on `a` (already done), and the details pane must show whatever
    /// follow actually picked.
    #[test]
    fn follow_picks_the_running_row_not_a_hard_coded_one() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}},
            "fan": {"value": null, "meta": {"completed_at": null, "progress": {"done": 0, "total": 1}},
                "iter_0": {"value": null, "meta": {"created_at": "t1", "completed_at": null, "provider": "shell"}}
            }
        }});
        let rendered = render::build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            5.0,
            "2026-01-01T00:00:05Z",
            &[],
        );
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Flow;
        assert!(app.follow, "issue #527: follow is on when the TUI starts");
        if let Some(target) = App::follow_target(&rendered.rows) {
            app.select_tree_path(Some(target.to_string()));
        }
        assert_eq!(
            app.selected_path.as_deref(),
            Some("prime.fan.iter_0"),
            "follow must choose the actually-running leaf, not the done one"
        );
        let details = render::details_for(
            app.selected_path.as_deref().unwrap(),
            &plan,
            &RunModel::new(),
            Some(&state),
        );
        let test_theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let test_tracker = crate::highlight::Tracker::new();
        let test_ctx = DrawCtx {
            theme: &test_theme,
            highlights: &test_tracker,
            now: Instant::now(),
            color_mode: ColorMode::TrueColor,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    Some(&details),
                    &[],
                    0,
                    &mut app,
                    Some(&state),
                    &test_ctx,
                    0,
                    "2026-01-01T00:00:00Z",
                    None,
                )
            })
            .unwrap();
        let text = terminal.backend().to_string();
        assert!(
            text.contains("path   prime.fan.iter_0"),
            "the details pane must show the row follow actually picked:\n{text}"
        );
        insta::assert_snapshot!(text);
    }

    /// A chain plan 48 rows deep (the root plus 48 leaves), with the
    /// 45th leaf running and everything before it done -- the
    /// acceptance list's own "a plan taller than the pane, a followed
    /// effect below the fold is visible" (issue #527), tested the same
    /// way `follow_picks_the_running_row_not_a_hard_coded_one` is: via
    /// `App::follow_target`, at a terminal shorter than the plan.
    #[test]
    fn a_long_plan_scrolls_to_keep_a_followed_effect_below_the_fold_visible() {
        const N: usize = 48;
        const RUNNING: usize = 44;

        let root_path = EffectPath::root();
        let ops: Vec<Op> = (0..N)
            .map(|i| {
                tool(
                    root_path.clone().push_name(format!("step_{i}")),
                    "step",
                    OnError::Fail,
                )
            })
            .collect();
        let root = Op {
            path: root_path,
            name: Some("prime".to_string()),
            kind: NodeKind::Control(Region::Block {
                ops,
                overlay: false,
            }),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        let program = Program {
            root,
            prompts: Default::default(),
            effect_names: Default::default(),
            document: None,
            runtime_block: None,
            interface: None,
            adapter: None,
            model: None,
        };
        let plan = PlanTree::from_program(&program);

        let mut prime = serde_json::Map::new();
        prime.insert("value".to_string(), Value::Null);
        let mut prime_meta = serde_json::Map::new();
        prime_meta.insert("completed_at".to_string(), Value::Null);
        prime.insert("meta".to_string(), Value::Object(prime_meta));
        for i in 0..=RUNNING {
            let mut meta = serde_json::Map::new();
            meta.insert("created_at".to_string(), Value::String(format!("t{i}")));
            meta.insert(
                "completed_at".to_string(),
                if i < RUNNING {
                    Value::String(format!("t{}", i + 1))
                } else {
                    Value::Null
                },
            );
            meta.insert("provider".to_string(), Value::String("shell".to_string()));
            let mut node = serde_json::Map::new();
            node.insert(
                "value".to_string(),
                if i < RUNNING {
                    Value::String(format!("ok{i}\n"))
                } else {
                    Value::Null
                },
            );
            node.insert("meta".to_string(), Value::Object(meta));
            prime.insert(format!("step_{i}"), Value::Object(node));
        }
        let mut root_value = serde_json::Map::new();
        root_value.insert("prime".to_string(), Value::Object(prime));
        let state = Value::Object(root_value);

        let mut model = RunModel::new();
        let rendered = render::build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            44.0,
            "t45",
            &[],
        );
        assert_eq!(rendered.rows.len(), N + 1, "the root row plus every leaf");

        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Flow;
        if let Some(target) = App::follow_target(&rendered.rows) {
            app.select_tree_path(Some(target.to_string()));
        }
        assert_eq!(app.selected_path.as_deref(), Some("prime.step_44"));

        let details = render::details_for(
            app.selected_path.as_deref().unwrap(),
            &plan,
            &RunModel::new(),
            Some(&state),
        );
        let test_theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let test_tracker = crate::highlight::Tracker::new();
        let test_ctx = DrawCtx {
            theme: &test_theme,
            highlights: &test_tracker,
            now: Instant::now(),
            color_mode: ColorMode::TrueColor,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let backend = TestBackend::new(70, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    Some(&details),
                    &[],
                    0,
                    &mut app,
                    Some(&state),
                    &test_ctx,
                    0,
                    "2026-01-01T00:00:00Z",
                    None,
                )
            })
            .unwrap();
        let text = terminal.backend().to_string();
        assert!(
            text.contains("step_44"),
            "the followed row, below the fold, must still be drawn:\n{text}"
        );
        assert!(
            !text.contains("step_0 ") && !text.contains("step_0\n"),
            "the first row must have scrolled out of view:\n{text}"
        );
        assert!(
            text.contains(&format!("/{}", N + 1)),
            "the border must show a scroll position once rows are hidden:\n{text}"
        );
        insta::assert_snapshot!(text);
    }

    /// Finding F11: the existing 2,000-node measurement
    /// (`render::tests::redraw_from_a_2000_node_state_stays_well_
    /// under_the_redraw_interval`) times `render::build` alone --
    /// this times one `TestBackend` draw of its own output too, since
    /// the treemap layout and the ratatui draw itself are not part of
    /// that measurement.
    #[test]
    fn drawing_a_2000_node_state_stays_well_under_the_redraw_interval() {
        let mut prime = serde_json::Map::new();
        prime.insert("value".to_string(), Value::Null);
        let mut prime_meta = serde_json::Map::new();
        prime_meta.insert("completed_at".to_string(), Value::Null);
        prime.insert("meta".to_string(), Value::Object(prime_meta));
        for i in 0..2000 {
            let mut meta = serde_json::Map::new();
            meta.insert("created_at".to_string(), Value::String(format!("t{i}")));
            meta.insert(
                "completed_at".to_string(),
                Value::String(format!("t{}", i + 1)),
            );
            meta.insert("provider".to_string(), Value::String("shell".to_string()));
            meta.insert("tokens_sent_total".to_string(), Value::from(1));
            meta.insert("tokens_received_total".to_string(), Value::from(2));
            let mut node = serde_json::Map::new();
            node.insert("value".to_string(), Value::String(format!("ok{i}\n")));
            node.insert("meta".to_string(), Value::Object(meta));
            prime.insert(format!("n{i}"), Value::Object(node));
        }
        let mut root = serde_json::Map::new();
        root.insert("prime".to_string(), Value::Object(prime));
        let state = Value::Object(root);

        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let rendered = render::build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            10.0,
            "t2001",
            &[],
        );
        let details = render::details_for("n0", &plan, &RunModel::new(), Some(&state));
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Flow;
        app.selected_path = Some("n0".to_string());

        let test_theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let test_tracker = crate::highlight::Tracker::new();
        let test_ctx = DrawCtx {
            theme: &test_theme,
            highlights: &test_tracker,
            now: Instant::now(),
            color_mode: ColorMode::TrueColor,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let start = std::time::Instant::now();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    Some(&details),
                    &[],
                    0,
                    &mut app,
                    Some(&state),
                    &test_ctx,
                    0,
                    "2026-01-01T00:00:00Z",
                    None,
                )
            })
            .unwrap();
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "one TestBackend draw of a 2000-node state took {elapsed:?}, over 5x the 100ms REDRAW_INTERVAL"
        );
    }

    /// A tree-loop pass's prompt never
    /// appears in live state at all while it runs (DESIGN.md §1.4), on
    /// either engine -- `fan.iter_0.ask` here has no node of its own in
    /// `state`, confirmed running only by its own `start` event. The
    /// token bar must still show it running, as a lower-bound estimate
    /// (no `prompt_sent` to measure from), rather than "nothing in
    /// flight".
    #[test]
    fn full_layout_with_a_running_tree_loop_prompt() {
        let plan = tree_loop_with_prompt_plan();
        let mut model = RunModel::new();
        model.observe_event(&Event::Dispatch {
            ts: "2026-01-01T00:00:00Z".to_string(),
            path: "prime.fan".to_string(),
            branches: 2,
            concurrency: Some(2),
        });
        model.observe_event(&Event::Start {
            ts: "2026-01-01T00:00:00Z".to_string(),
            id: Some(1),
            path: "prime.fan.iter_0.ask".to_string(),
        });
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null}}});
        let rendered = render::build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            5.0,
            "2026-01-01T00:00:05Z",
            &[],
        );
        assert_eq!(rendered.token_bar.prompts_running, 1);
        assert!(rendered.token_bar.estimated_input_is_lower_bound);

        let details = render::details_for(
            "prime.fan.iter_0.ask",
            &plan,
            &RunModel::new(),
            Some(&state),
        );
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Flow;
        app.selected_path = Some("prime.fan.iter_0.ask".to_string());
        app.follow = false;
        let test_theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let test_tracker = crate::highlight::Tracker::new();
        let test_ctx = DrawCtx {
            theme: &test_theme,
            highlights: &test_tracker,
            now: Instant::now(),
            color_mode: ColorMode::TrueColor,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    Some(&details),
                    &[],
                    0,
                    &mut app,
                    Some(&state),
                    &test_ctx,
                    0,
                    "2026-01-01T00:00:00Z",
                    None,
                )
            })
            .unwrap();
        let text = terminal.backend().to_string();
        assert!(
            !text.contains("nothing in flight"),
            "a running tree-loop prompt must show in the token bar:\n{text}"
        );
        // With no running prompt's
        // `prompt_sent` known at all, the estimate isn't merely low,
        // it's unknown -- `now in ≈0+` reads as a broken number (zero
        // tokens, running?); this shows a plain `?` instead.
        assert!(
            text.contains("now in ?"),
            "an estimate with nothing known at all must read as unknown (`?`), not `≈0+`:\n{text}"
        );
        insta::assert_snapshot!(text);
    }
}

/// Issue #535's own "buffer test" acceptance item: pins a highlighted
/// cell's style, and that a named theme paints the background of an
/// empty cell -- both read straight from a `ratatui::buffer::Buffer`,
/// not a `row_line`/`Line` style, since that's what the acceptance
/// item actually asks for.
#[cfg(test)]
mod highlight_tests {
    use super::*;
    use oscilloscope_core::model::{RowStatus, StatusKind};
    use oscilloscope_core::render::{Header, RenderState, Row, RowKind};
    use oscilloscope_core::treemap::TreemapNode;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::time::Duration;

    use crate::highlight::{FADE_DURATION, Tracker};
    use crate::theme::{ColorMode, Theme, ThemeName};

    fn minimal_render_state() -> RenderState {
        RenderState {
            header: Header {
                document: "do-thing.yml".to_string(),
                engine: "cof".to_string(),
                run_id: None,
                status: oscilloscope_core::model::RunStatus::Running,
                elapsed_s: 1.0,
                run_start_ts: None,
                run_end_ts: None,
                effects_done: 1,
                effects_planned: Some(1),
                effects_planned_is_lower_bound: false,
                tokens_sent: 0,
                tokens_received: 0,
                innermost_loop_eta_s: None,
                innermost_loop_eta_ts: None,
                latest_report: None,
                engine_alive: true,
            },
            unfolded_rows: Vec::new(),
            rows: vec![Row {
                path: "prime.a".to_string(),
                depth: 1,
                label: "a".to_string(),
                kind: RowKind::Tool,
                status: RowStatus {
                    kind: StatusKind::Done,
                    skip_reason: None,
                    retrying: false,
                },
                duration_s: None,
                loop_progress: None,
                dim: None,
                has_children: false,
            }],
            state_rows: Vec::new(),
            treemap: Vec::<TreemapNode>::new(),
            token_bar: Default::default(),
            log: Vec::new(),
        }
    }

    /// Pins a highlighted row's own background at the instant it was
    /// marked (`t = 0`), and that nothing overrides it any more once
    /// the fade has fully ended -- both read from the real `Buffer`
    /// the orchestration pane's row actually lands in, at its own
    /// known position (one row below the header/body top border, one
    /// column in from the pane's own left border).
    #[test]
    fn a_highlighted_row_pins_its_cell_background_at_t0_and_has_none_after_the_fade() {
        let rendered = minimal_render_state();
        let theme = Theme::for_name(ThemeName::Dracula);
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Flow;
        app.selected_path = Some("prime.a".to_string());
        app.follow = false;

        // The orchestration pane sits below the header (3 rows) and
        // the details pane (golden_section of the body's own height);
        // its first row, "a", is the pane's own first interior line.
        // Finding the exact cell by scanning for the row's own label
        // keeps this test correct even if an unrelated layout tweak
        // shifts it by a row or column -- the point under test is the
        // *style* once found, not the coordinates themselves.
        // Searches for the exact sequence `"✓ a"` -- the status glyph,
        // a space, then the row's own label -- rather than a bare
        // `"a"`, which also occurs inside the orchestration pane's own
        // title ("orchestration"), found first since titles sit on an
        // earlier row than the content below them. Returns the `a`
        // cell itself, two columns past the match's own start.
        fn find_row_cell(buffer: &ratatui::buffer::Buffer) -> (u16, u16) {
            let needle: Vec<char> = "✓ a".chars().collect();
            for y in 0..buffer.area.height {
                for x in 0..buffer.area.width {
                    let matches = needle.iter().enumerate().all(|(i, &c)| {
                        x + (i as u16) < buffer.area.width
                            && buffer[(x + i as u16, y)].symbol() == c.to_string()
                    });
                    if matches {
                        return (x + needle.len() as u16 - 1, y);
                    }
                }
            }
            panic!("row \"a\" not found in the buffer");
        }

        let mut tracker = Tracker::new();
        let now = std::time::Instant::now();
        tracker.mark("row:prime.a", now);

        let ctx_t0 = DrawCtx {
            theme: &theme,
            highlights: &tracker,
            now,
            color_mode: ColorMode::TrueColor,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let mut app_t0 = app.clone();
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    None,
                    &[],
                    0,
                    &mut app_t0,
                    None,
                    &ctx_t0,
                    0,
                    "2026-01-01T00:00:00Z",
                    None,
                )
            })
            .unwrap();
        let buffer_t0 = terminal.backend().buffer().clone();
        let (x, y) = find_row_cell(&buffer_t0);
        assert_eq!(
            buffer_t0[(x, y)].bg,
            Color::Rgb(0x44, 0x47, 0x5a),
            "t=0 must show the theme's own highlight tint"
        );

        let ctx_after = DrawCtx {
            theme: &theme,
            highlights: &tracker,
            now: now + FADE_DURATION + Duration::from_millis(1),
            color_mode: ColorMode::TrueColor,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let mut app_after = app.clone();
        let backend2 = TestBackend::new(100, 24);
        let mut terminal2 = Terminal::new(backend2).unwrap();
        terminal2
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    None,
                    &[],
                    0,
                    &mut app_after,
                    None,
                    &ctx_after,
                    0,
                    "2026-01-01T00:00:00Z",
                    None,
                )
            })
            .unwrap();
        let buffer_after = terminal2.backend().buffer().clone();
        assert_eq!(
            buffer_after[(x, y)].bg,
            theme.background.unwrap(),
            "once the fade has ended, the cell reads the painted background, not the tint"
        );
    }

    /// Issue #626: the scope icon's own trace is now the header
    /// line's first segment, drawn at every width -- there is no
    /// longer a narrow-terminal threshold below which it hides (the
    /// old three-row bezel icon had one; the inline trace does not).
    #[test]
    fn the_icon_draws_at_every_width() {
        let rendered = minimal_render_state();
        let theme = Theme::for_name(ThemeName::Terminal);
        let tracker = Tracker::new();
        let ctx = DrawCtx {
            theme: &theme,
            highlights: &tracker,
            now: std::time::Instant::now(),
            color_mode: ColorMode::TrueColor,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let first_cell_is_blank = |width: u16| -> bool {
            let mut app = App::new(false);
            app.screen = oscilloscope_core::screens::Screen::Flow;
            let backend = TestBackend::new(width, 24);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|f| {
                    draw(
                        f,
                        &rendered,
                        None,
                        &[],
                        0,
                        &mut app,
                        None,
                        &ctx,
                        0,
                        "2026-01-01T00:00:00Z",
                        None,
                    )
                })
                .unwrap();
            terminal.backend().buffer()[(0, 0)].symbol() == " "
        };
        assert!(
            !first_cell_is_blank(40),
            "the icon's own trace must draw even on a narrow terminal"
        );
        assert!(
            !first_cell_is_blank(100),
            "the icon's own trace must draw on a wide terminal too"
        );
    }

    /// Finding 14: the token bar's own totals line must highlight on
    /// `header:tokens` too, not only `tokenbar` -- a totals change
    /// with no in-flight `TokenBar` change (an estimate or a running
    /// count that happens to land the same either side) must not
    /// leave this line dark.
    #[test]
    fn the_token_bar_totals_line_highlights_on_header_tokens_alone() {
        let mut rendered = minimal_render_state();
        rendered.header.tokens_sent = 120;
        rendered.header.tokens_received = 340;
        let theme = Theme::for_name(ThemeName::Dracula);
        let mut tracker = Tracker::new();
        let now = std::time::Instant::now();
        tracker.mark("header:tokens", now);
        let ctx = DrawCtx {
            theme: &theme,
            highlights: &tracker,
            now,
            color_mode: ColorMode::TrueColor,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Flow;
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    None,
                    &[],
                    0,
                    &mut app,
                    None,
                    &ctx,
                    0,
                    "2026-01-01T00:00:00Z",
                    None,
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let needle: Vec<char> = "120".chars().collect();
        let mut found = None;
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                let matches = needle.iter().enumerate().all(|(i, &c)| {
                    x + (i as u16) < buffer.area.width
                        && buffer[(x + i as u16, y)].symbol() == c.to_string()
                });
                if matches {
                    found = Some((x, y));
                }
            }
        }
        let (x, y) = found.expect("the sent total \"120\" should appear in the token bar");
        assert_ne!(
            buffer[(x, y)].bg,
            theme.background.unwrap(),
            "header:tokens alone should still tint the totals line"
        );
    }

    /// Issue #535's own theme acceptance item: a named theme paints
    /// the whole screen's background, so an otherwise-untouched cell
    /// (the frame's own top-left corner, drawn over by nothing but
    /// the header block's border, which sets no background of its
    /// own) still reads the theme's background colour.
    #[test]
    fn a_named_theme_paints_the_background_of_an_empty_cell() {
        let rendered = minimal_render_state();
        let theme = Theme::for_name(ThemeName::Nord);
        let tracker = Tracker::new();
        let ctx = DrawCtx {
            theme: &theme,
            highlights: &tracker,
            now: std::time::Instant::now(),
            color_mode: ColorMode::TrueColor,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Flow;
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    None,
                    &[],
                    0,
                    &mut app,
                    None,
                    &ctx,
                    0,
                    "2026-01-01T00:00:00Z",
                    None,
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].bg, theme.background.unwrap());
    }

    /// Finding 7: the base fill set only `bg`, so a named theme's
    /// own `text` colour never actually painted anything -- a plain
    /// cell (nothing drawn over it but the base fill) must read the
    /// theme's `text`, not the terminal's own default foreground.
    /// `(95, 0)` is blank space on the one-line header (issue #626),
    /// to the right of its own content -- the header has no border
    /// of its own any more, so this is the simplest cell that's
    /// reliably untouched by anything but the base fill.
    #[test]
    fn a_named_theme_paints_the_default_text_colour_of_an_empty_cell() {
        let rendered = minimal_render_state();
        let theme = Theme::for_name(ThemeName::SolarizedLight);
        let tracker = Tracker::new();
        let ctx = DrawCtx {
            theme: &theme,
            highlights: &tracker,
            now: std::time::Instant::now(),
            color_mode: ColorMode::TrueColor,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Flow;
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    None,
                    &[],
                    0,
                    &mut app,
                    None,
                    &ctx,
                    0,
                    "2026-01-01T00:00:00Z",
                    None,
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(95, 0)].symbol(), " ");
        assert_eq!(buffer[(95, 0)].fg, theme.text);
    }

    #[test]
    fn the_terminal_theme_paints_no_background_at_all() {
        let rendered = minimal_render_state();
        let theme = Theme::for_name(ThemeName::Terminal);
        let tracker = Tracker::new();
        let ctx = DrawCtx {
            theme: &theme,
            highlights: &tracker,
            now: std::time::Instant::now(),
            color_mode: ColorMode::TrueColor,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Flow;
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    None,
                    &[],
                    0,
                    &mut app,
                    None,
                    &ctx,
                    0,
                    "2026-01-01T00:00:00Z",
                    None,
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].bg, Color::Reset);
    }
}
