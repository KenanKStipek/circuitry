//! Lays out one `RenderState` (DESIGN.md §6.3): the header, the plan
//! tree and details panes, the log pane, and the key's own overlays
//! (help, both confirms, the full-value view, the filter prompt).
//! The only module in this crate that imports `ratatui` widgets
//! directly, mirroring `oscilloscope-core`'s own "no terminal code"
//! split one level up.

use std::time::Instant;

use oscilloscope_core::model::{RunStatus, StatusKind};
use oscilloscope_core::render::{Details, Row, StateRow};
use oscilloscope_core::treemap::{Measure, TreemapNode};
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
/// line itself never disappears. These are whole-*terminal* sizes
/// (DESIGN.md §6.3, the README's TUI section) — checked against
/// `frame.area()` itself, never against `body` (the terminal minus
/// the header's own single row), which would silently shift every
/// height threshold down by one.
mod thresholds {
    /// Below this, the two-column layout itself collapses to one.
    /// Issue #635: no screen still uses this pair on its own (the
    /// Flow screen's own `FLOW_TWO_PANE_MIN_WIDTH`/`_MIN_HEIGHT`
    /// below happen to share the same numbers); kept for the Cost
    /// screen's own thresholds just below, which are defined relative
    /// to it.
    pub const TWO_COLUMN_MIN_WIDTH: u16 = 80;
    pub const TWO_COLUMN_MIN_HEIGHT: u16 = 20;

    /// The Cost screen's own thresholds (issue #632, DESIGN.md's
    /// "small terminals"): at this size and up, the tokens-over-time
    /// chart sits beside the largest-stores table, under the
    /// treemap. Below it (but still at [`COST_TREEMAP_MIN_WIDTH`]/
    /// `_MIN_HEIGHT`), the chart is dropped and the table takes the
    /// treemap's own row's full width instead. Both figures are whole-
    /// *terminal* sizes, same as every other threshold here -- checked
    /// against `frame.area()`, never `body`, inclusive (100×30 and
    /// 80×20 themselves already qualify).
    pub const COST_CHART_MIN_WIDTH: u16 = 100;
    pub const COST_CHART_MIN_HEIGHT: u16 = 30;
    /// Below this, the treemap itself is dropped too, leaving the
    /// table alone (`⏎` still zooms it) -- the same floor screen 2's
    /// own two-column layout collapses at.
    pub const COST_TREEMAP_MIN_WIDTH: u16 = TWO_COLUMN_MIN_WIDTH;
    pub const COST_TREEMAP_MIN_HEIGHT: u16 = TWO_COLUMN_MIN_HEIGHT;

    /// The Flow screen's own thresholds (issue #635): at this size
    /// and up, flow | (selected over story), 62/38 both ways. Below
    /// it (but still at [`FLOW_TWO_PANE_MIN_WIDTH`]/`_MIN_HEIGHT`),
    /// the story pane is dropped -- flow | selected, with `l`
    /// swapping the log into the selected pane's own place. Below
    /// that, one column: selected over flow, no story, the flow
    /// pane's own columns dropped to duration alone.
    pub const FLOW_THREE_PANE_MIN_WIDTH: u16 = 100;
    pub const FLOW_THREE_PANE_MIN_HEIGHT: u16 = 30;
    pub const FLOW_TWO_PANE_MIN_WIDTH: u16 = TWO_COLUMN_MIN_WIDTH;
    pub const FLOW_TWO_PANE_MIN_HEIGHT: u16 = TWO_COLUMN_MIN_HEIGHT;
    /// Below this flow-pane width, the cost column is dropped first
    /// (issue #635: "narrow panes drop columns in this order: cost,
    /// start, tokens").
    pub const FLOW_COST_COLUMN_MIN_WIDTH: u16 = 95;
    // The flow pane's own width is a golden-section share of the
    // body, never the whole terminal's -- at the widest required
    // size (120 columns), the pane itself is about 74 columns wide,
    // so a threshold above that can never show the start column at
    // any supported terminal size at all.
    pub const FLOW_START_COLUMN_MIN_WIDTH: u16 = 70;
    pub const FLOW_TOKENS_COLUMN_MIN_WIDTH: u16 = 65;
    /// The condition dim second line (issue #635's Decisions: "a dim
    /// second line at 100 columns and wider; otherwise it appears
    /// only in the selected pane") -- checked against the whole
    /// terminal, the same way every other threshold here is.
    pub const FLOW_CONDITION_LINE_MIN_WIDTH: u16 = FLOW_THREE_PANE_MIN_WIDTH;

    /// The Time screen's own thresholds (issue #650, DESIGN.md's
    /// "Small terminals"): at this size and up, the timeline with
    /// its strips (62% of the height) sits above the zoom pane and
    /// the vs usual pane, side by side at 62/38. Below it (but still
    /// at [`TIME_AXIS_ONLY_MIN_WIDTH`]/`_MIN_HEIGHT`), the timeline
    /// and strips alone. Below that, a plain list with no axis.
    pub const TIME_THREE_PANE_MIN_WIDTH: u16 = 100;
    pub const TIME_THREE_PANE_MIN_HEIGHT: u16 = 30;
    pub const TIME_AXIS_ONLY_MIN_WIDTH: u16 = TWO_COLUMN_MIN_WIDTH;
    pub const TIME_AXIS_ONLY_MIN_HEIGHT: u16 = TWO_COLUMN_MIN_HEIGHT;
    /// Below this, the timeline's own start/duration columns are
    /// dropped (DESIGN.md: "Below 90 columns, the start and duration
    /// columns go").
    pub const TIME_START_DURATION_COLUMN_MIN_WIDTH: u16 = 90;
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

/// Unit tests for [`scroll_indicator`] on their own,
/// beyond the rendered goldens that already exercise it indirectly.
#[cfg(test)]
mod scroll_tests {
    use super::*;

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
/// two-way splits; equal panes ... share space equally"). Below
/// [`thresholds::TWO_COLUMN_MIN_WIDTH`]/`_MIN_HEIGHT` -- the same
/// 80×20 line screen 2's own two-column layout collapses at -- the
/// phase strip drops to one line, the now pane stacks the step over
/// the log lines instead of splitting them side by side, and the
/// three cards become one summary line (DESIGN.md: "below 80
/// columns or 20 rows").
fn draw_overview(
    frame: &mut Frame,
    area: Rect,
    overview: &oscilloscope_core::screens::Overview,
    transcript_view: Option<&oscilloscope_core::transcript::TranscriptView>,
    ctx: &DrawCtx,
) {
    if area.height == 0 {
        return;
    }
    let stacked = area.width < thresholds::TWO_COLUMN_MIN_WIDTH
        || area.height < thresholds::TWO_COLUMN_MIN_HEIGHT;
    let phase_height = if stacked { 1 } else { 2 }.min(area.height);
    let cards_height = if stacked { 1 } else { 0 };
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(phase_height),
            Constraint::Min(0),
            Constraint::Length(cards_height.min(area.height.saturating_sub(phase_height))),
        ])
        .split(area);
    draw_phase_strip(frame, sections[0], &overview.phase_strip, ctx, stacked);

    if stacked {
        draw_now_pane(
            frame,
            sections[1],
            &overview.now_pane,
            transcript_view,
            ctx,
            true,
        );
        draw_cards_summary_line(frame, sections[2], overview, ctx);
        return;
    }

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

    draw_now_pane(
        frame,
        rows[0],
        &overview.now_pane,
        transcript_view,
        ctx,
        false,
    );
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
    single_line: bool,
) {
    if area.height == 0 || steps.is_empty() {
        return;
    }
    // The real separator drawn between two items is 5 columns wide
    // (`"  →  "`/`"     "`), not the 3 the old width estimate used --
    // undercounting it let the `○ N more` marker spill past the
    // right edge once steps overflowed.
    const SEP_WIDTH: usize = 5;
    let max_width = area.width as usize;
    let mut line1: Vec<Span> = Vec::new();
    let mut line2: Vec<Span> = Vec::new();
    let mut used = 0usize;
    let mut shown = 0usize;
    for (idx, step) in steps.iter().enumerate() {
        let glyph = status_glyph(step.status.kind);
        let label1 = format!("{glyph} {}", step.label);
        let label2 = if single_line {
            None
        } else {
            let start = step
                .start_ts
                .as_deref()
                .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone));
            let dur = step
                .duration_s
                .map(oscilloscope_core::format::format_duration);
            Some(match (&start, &dur) {
                (Some(s), Some(d)) => format!("{s} · {d}"),
                (Some(s), None) => s.clone(),
                _ => String::new(),
            })
        };
        let col_width = label1
            .chars()
            .count()
            .max(label2.as_deref().map_or(0, |l| l.chars().count()));
        let sep_width = if shown > 0 { SEP_WIDTH } else { 0 };
        // Reserved only when this isn't the last step -- including
        // the last one never needs a trailing `○ N more` marker, so
        // nothing has to be held back for it.
        let more_reserve = if idx + 1 < steps.len() {
            let n = steps.len() - idx - 1;
            2 + format!("○ {n} more").chars().count()
        } else {
            0
        };
        if shown > 0 && used + sep_width + col_width + more_reserve > max_width {
            break;
        }
        used += sep_width + col_width;
        if shown > 0 {
            line1.push(Span::raw("  →  "));
            if !single_line {
                line2.push(Span::raw("     "));
            }
        }
        line1.push(Span::styled(
            format!("{label1:<col_width$}"),
            status_style(step.status.kind, ctx.theme),
        ));
        if let Some(label2) = label2 {
            line2.push(Span::styled(
                format!("{label2:<col_width$}"),
                Style::default().fg(ctx.theme.dim),
            ));
        }
        shown += 1;
    }
    if shown < steps.len() {
        let more = steps.len() - shown;
        if shown > 0 {
            line1.push(Span::raw("  "));
        }
        line1.push(Span::raw(format!("○ {more} more")));
    }
    let lines = if single_line {
        vec![Line::from(line1)]
    } else {
        vec![Line::from(line1), Line::from(line2)]
    };
    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, area);
}

/// The now pane's own left (log) / right (focus) split, 62/38
/// (DESIGN.md's two-way-split rule).
fn draw_now_pane(
    frame: &mut Frame,
    area: Rect,
    pane: &oscilloscope_core::screens::NowPane,
    transcript_view: Option<&oscilloscope_core::transcript::TranscriptView>,
    ctx: &DrawCtx,
    stacked: bool,
) {
    if area.height == 0 {
        return;
    }
    let log_lines: Vec<Line> = pane
        .log_lines
        .iter()
        .map(|l| Line::from(l.clone()))
        .collect();
    let focus_raw = focus_lines(&pane.focus, transcript_view, ctx);
    let log =
        Paragraph::new(log_lines).block(Block::default().borders(Borders::ALL).title(" now "));

    if stacked {
        // The step over the log lines (DESIGN.md: "below 80 columns
        // or 20 rows ... the now pane stacks"); the step's own
        // content is short and bounded, so it takes the golden
        // section's smaller share, the log (which only ever grows by
        // appending) the rest.
        let focus_height = oscilloscope_core::layout::golden_section(area.height);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(focus_height), Constraint::Min(0)])
            .split(area);
        // Truncated to this pane's own real inner width
        // (the border either side) with an ellipsis, same as every
        // other pane here -- without it, a line a touch too long for
        // a small terminal is simply cut off by the widget itself,
        // silently dropping whatever character landed on the edge
        // (a live summary's own `turn 14` reading back as `turn 1`).
        let focus_width = rows[0].width.saturating_sub(2) as usize;
        let focus = Paragraph::new(truncate_lines(focus_raw, focus_width))
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(focus, rows[0]);
        frame.render_widget(log, rows[1]);
        return;
    }
    let left_width = oscilloscope_core::layout::golden_major_section(area.width);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(left_width), Constraint::Min(0)])
        .split(area);
    let focus_width = cols[1].width.saturating_sub(2) as usize;
    let focus = Paragraph::new(truncate_lines(focus_raw, focus_width))
        .block(Block::default().borders(Borders::ALL));
    frame.render_widget(log, cols[0]);
    frame.render_widget(focus, cols[1]);
}

/// The now pane's own right side (issue #626): the step follow
/// selects, or, with nothing running, the last finished step, or,
/// once the run has ended, its outcome and totals.
fn focus_lines(
    focus: &oscilloscope_core::screens::NowFocus,
    transcript_view: Option<&oscilloscope_core::transcript::TranscriptView>,
    ctx: &DrawCtx,
) -> Vec<Line<'static>> {
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
            // Issue #637: an agent session that never completed has
            // no `NowFocus::Running`/`LastFinished` of its own left
            // to be the overview's focus once the run has ended --
            // named and shown here instead, with its own last-known
            // live block, reusing exactly what a running or
            // last-finished step's own focus already renders.
            if let Some(agent) = &summary.stopped_agent {
                lines.push(Line::from(""));
                lines.extend(focus_step_lines(agent, None, transcript_view, ctx));
            }
            lines
        }
        NowFocus::Running(step, more) => focus_step_lines(step, Some(*more), transcript_view, ctx),
        NowFocus::LastFinished(step) => focus_step_lines(step, None, transcript_view, ctx),
    }
}

/// A running agent's own live summary (`docs/plugins/agent.md`'s
/// live-progress table, issue #637), the lines both the overview's
/// now pane and Flow's selected pane show for it -- a figure the CLI
/// doesn't report live (`tokens_received`/`cost_usd` on Claude Code)
/// reads `—`, never `0`; a context window osi doesn't know yet omits
/// the bar but still shows the token count on its own. The tool
/// activity (the glyph strip, the clock-timed list) comes from
/// `transcript_view` when it has any calls at all; with no transcript
/// tailed yet (or a step whose engine never wrote one), it falls back
/// to `live.tools`/`live.current_tool` -- names and counts only, no
/// times or targets, exactly what those fields carry. `tests` reads
/// only from the transcript either way (the engine doesn't publish
/// it at all), independently of whether `files`/`+/−` has anything to
/// show -- a session that has only run a test command, with no edit
/// yet, still gets a `tests` line of its own.
fn agent_live_lines(
    live: &oscilloscope_core::model::AgentLive,
    transcript_view: Option<&oscilloscope_core::transcript::TranscriptView>,
    ctx: &DrawCtx,
    show_doing_header: bool,
) -> Vec<Line<'static>> {
    use oscilloscope_core::format::format_tokens;
    let dim = Style::default().fg(ctx.theme.dim);
    let mut lines = Vec::new();

    let mut header = Vec::new();
    if let Some(engine) = &live.engine {
        header.push(engine.clone());
    }
    if let Some(model) = &live.model {
        header.push(model.clone());
    }
    if let Some(turns) = live.turns {
        header.push(format!("turn {turns}"));
    }
    if let Some(tool_calls) = live.tool_calls {
        header.push(format!("{tool_calls} tools"));
    }
    if !header.is_empty() {
        lines.push(Line::from(header.join(" · ")));
    }

    let sent = live
        .tokens_sent
        .map(format_tokens)
        .unwrap_or_else(|| "—".to_string());
    let received = live
        .tokens_received
        .map(format_tokens)
        .unwrap_or_else(|| "—".to_string());
    let cost = live
        .cost_usd
        .map(|c| format!("${c:.2}"))
        .unwrap_or_else(|| "$—".to_string());
    lines.push(Line::from(format!("↑{sent} ↓{received} · {cost}")));

    // The tool activity comes right after the headline
    // figures, before context/files/tests -- in Flow's own selected
    // pane, nested under its own `doing:` header and a tighter height
    // budget than the overview's now pane has, this is the one part
    // of the summary that would otherwise be the first thing clipped
    // off the bottom.
    match transcript_view.filter(|v| !v.calls.is_empty()) {
        Some(view) => {
            let strip: String = view
                .calls
                .iter()
                .rev()
                .take(32)
                .rev()
                .map(|c| oscilloscope_core::transcript::tool_glyph(&c.tool))
                .collect();
            lines.push(Line::styled(strip, dim));
            if show_doing_header {
                lines.push(Line::styled("doing:", dim));
            }
            for call in view.calls.iter().rev().take(3).rev() {
                let clock = call
                    .start_t
                    .as_deref()
                    .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone))
                    .map(|c| format!("{c} "))
                    .unwrap_or_default();
                let mut text = format!("{clock}› {}", call.tool);
                if let Some(target) = &call.target {
                    text.push(' ');
                    text.push_str(target);
                }
                match call.ok {
                    Some(true) => text.push_str(" ✓"),
                    Some(false) => text.push_str(" ✗"),
                    None if call.stopped => text.push_str(" stopped"),
                    None => text.push_str(" running"),
                }
                if call.lines_added.is_some() || call.lines_removed.is_some() {
                    text.push_str(&format!(
                        " +{} −{}",
                        call.lines_added.unwrap_or(0),
                        call.lines_removed.unwrap_or(0)
                    ));
                }
                lines.push(Line::from(text));
            }
        }
        None => {
            // Issue #637: without a transcript to read calls from at
            // all (not yet tailed, or an engine that never wrote
            // one), the same activity is shown from `meta.live`'s own
            // `tools`/`current_tool` instead -- names and counts
            // only, since that's all either field carries.
            if !live.tools.is_empty() {
                let counts = live
                    .tools
                    .iter()
                    .map(|(name, usage)| format!("{name} ×{}", usage.calls))
                    .collect::<Vec<_>>()
                    .join(" · ");
                if show_doing_header {
                    lines.push(Line::styled("doing:", dim));
                }
                lines.push(Line::from(counts));
            }
            if let Some(current) = &live.current_tool {
                lines.push(Line::from(format!("running: {}", current.name)));
            }
        }
    }

    if let Some(context_tokens) = live.context_tokens {
        match live.context_window {
            Some(window) => {
                lines.push(Line::from(format!(
                    "context {} / {}",
                    format_tokens(context_tokens),
                    format_tokens(window)
                )));
                lines.push(Line::styled(bar(context_tokens, window, 20), dim));
            }
            None => {
                lines.push(Line::from(format!(
                    "context {}",
                    format_tokens(context_tokens)
                )));
            }
        }
    }

    let mut activity = Vec::new();
    if live.files_changed.is_some() || live.lines_added.is_some() || live.lines_removed.is_some() {
        if let Some(files) = live.files_changed {
            activity.push(format!("files {files}"));
        }
        if live.lines_added.is_some() || live.lines_removed.is_some() {
            activity.push(format!(
                "+{} −{}",
                live.lines_added.unwrap_or(0),
                live.lines_removed.unwrap_or(0)
            ));
        }
    }
    if let Some(tests) = transcript_view.and_then(|v| v.tests) {
        use oscilloscope_core::transcript::TestOutcome;
        activity.push(match tests {
            TestOutcome::Running => "tests …".to_string(),
            TestOutcome::Ok => "tests ✓".to_string(),
            TestOutcome::Failed => "tests ✗".to_string(),
        });
    }
    if !activity.is_empty() {
        lines.push(Line::from(activity.join(" · ")));
    }

    lines
}

fn focus_step_lines(
    step: &oscilloscope_core::screens::FocusedStep,
    more_running: Option<usize>,
    transcript_view: Option<&oscilloscope_core::transcript::TranscriptView>,
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
            // The note is a placeholder for the brief
            // window before the first `meta.live` write while the
            // step is actually running (`more_running.is_some()` is
            // exactly `NowFocus::Running`'s own signal) -- a finished
            // agent step reads the same as any other finished tool
            // step here, nothing extra, since `live` is always `None`
            // by then (`meta.live` is removed once the attempt ends).
            FocusDetail::Agent {
                live, note, stale, ..
            } => match live.as_deref() {
                Some(live) => {
                    // An interrupted run's own leftover `meta.live`
                    // (`docs/plugins/agent.md`: "a watcher shows it
                    // as stopped") -- the figures underneath are
                    // still the last ones the session actually
                    // reported, so they stay, with a marker that
                    // they're no longer moving.
                    if *stale {
                        lines.push(Line::styled("stopped", Style::default().fg(ctx.theme.dim)));
                    }
                    lines.extend(agent_live_lines(live, transcript_view, ctx, true));
                }
                None if more_running.is_some() => lines.push(Line::from(Span::styled(
                    *note,
                    Style::default().fg(ctx.theme.dim),
                ))),
                None => {}
            },
            FocusDetail::Plain => {}
        }
        if more_running.is_some() {
            if let (Some(p50), Some(elapsed)) = (step.usual_busy_p50_s, step.elapsed_s) {
                let usual = p50.max(step.usual_busy_p90_s.unwrap_or(p50));
                let bar = usual_progress_bar(elapsed, usual, 6);
                let mut text = format!(
                    "{bar} p50 {}",
                    oscilloscope_core::format::format_duration(p50)
                );
                if let Some(p90) = step.usual_busy_p90_s {
                    text.push_str(&format!(
                        " · p90 {}",
                        oscilloscope_core::format::format_duration(p90)
                    ));
                }
                lines.push(Line::from(text));
            }
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
/// Truncates a line's own displayed text to `max_chars`, ending in
/// `…` rather than letting the terminal clip it mid-word (DESIGN.md:
/// "long values truncate with …"), styling kept span by span.
fn truncate_line(line: Line<'static>, max_chars: usize) -> Line<'static> {
    let total: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
    if total <= max_chars {
        return line;
    }
    if max_chars == 0 {
        return Line::from("");
    }
    let mut budget = max_chars.saturating_sub(1);
    let mut spans = Vec::new();
    for span in line.spans {
        if budget == 0 {
            break;
        }
        let count = span.content.chars().count();
        if count <= budget {
            budget -= count;
            spans.push(span);
        } else {
            let truncated: String = span.content.chars().take(budget).collect();
            spans.push(Span::styled(truncated, span.style));
            budget = 0;
        }
    }
    spans.push(Span::raw("…"));
    Line::from(spans)
}

fn truncate_lines(lines: Vec<Line<'static>>, max_chars: usize) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .map(|l| truncate_line(l, max_chars))
        .collect()
}

/// Below [`thresholds::TWO_COLUMN_MIN_WIDTH`]/`_MIN_HEIGHT`
/// (DESIGN.md: "the cards become a single summary line") -- the
/// same figures the three cards show, collapsed onto one line with
/// no borders, so a tiny terminal still gets elapsed time, effects,
/// failures, tokens and cost without three bordered boxes eating
/// the only rows left.
fn draw_cards_summary_line(
    frame: &mut Frame,
    area: Rect,
    overview: &oscilloscope_core::screens::Overview,
    ctx: &DrawCtx,
) {
    if area.height == 0 {
        return;
    }
    let time = &overview.time_card;
    let flow = &overview.flow_card;
    let cost = &overview.cost_card;
    let effects = match flow.effects_planned {
        Some(planned) if flow.effects_planned_is_lower_bound => {
            format!("{}/{planned}+", flow.effects_done)
        }
        Some(planned) => format!("{}/{planned}", flow.effects_done),
        None => flow.effects_done.to_string(),
    };
    let mut parts = vec![
        oscilloscope_core::format::format_duration(time.elapsed_s),
        format!("{effects} effects"),
    ];
    if flow.failures > 0 {
        parts.push(format!("{} failed", flow.failures));
    }
    parts.push(format!(
        "↑{} ↓{}",
        oscilloscope_core::format::format_tokens(cost.tokens_sent),
        oscilloscope_core::format::format_tokens(cost.tokens_received)
    ));
    if let Some(usd) = cost.cost_usd {
        parts.push(format!("${usd:.2}"));
    }
    let _ = ctx;
    let line = truncate_line(Line::from(parts.join(" · ")), area.width as usize);
    frame.render_widget(Paragraph::new(line), area);
}

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
    if card.eta_past_usual {
        lines.push(Line::from("ETA ?"));
    } else if let Some(eta) = card
        .eta_ts
        .as_deref()
        .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone))
    {
        let label = if card.eta_from_history {
            "ETA"
        } else {
            "loop ETA"
        };
        let remaining = card
            .eta_remaining_s
            .filter(|_| card.eta_from_history)
            .map(|s| format!(" ({})", format_minutes(s)))
            .unwrap_or_default();
        lines.push(Line::from(format!(
            "{label} ~{}{remaining}",
            &eta[..5.min(eta.len())]
        )));
    }
    if card.eta_from_history {
        if let (Some(vs_usual), Some(usual_total)) = (card.vs_usual_s, card.usual_total_s) {
            lines.push(Line::from(format!(
                "vs usual {} (p50 {})",
                format_minutes_signed(vs_usual),
                format_minutes(usual_total)
            )));
        }
        if let (Some(low), Some(high)) = (
            card.usual_low_ts
                .as_deref()
                .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone)),
            card.usual_high_ts
                .as_deref()
                .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone)),
        ) {
            lines.push(Line::from(format!(
                "usual {}\u{2013}{}",
                &low[..5.min(low.len())],
                &high[..5.min(high.len())]
            )));
        }
    } else if let Some(count) = card.history_count {
        lines.push(Line::from(format!(
            "history: {count} of {} runs",
            oscilloscope_core::history::P50_MIN_RUNS
        )));
    }
    if !card.bar.is_empty() {
        let max_ms = card
            .bar
            .iter()
            .map(|(_, secs)| (*secs * 1000.0) as u64)
            .max()
            .unwrap_or(0);
        let text = card
            .bar
            .iter()
            .map(|(label, secs)| {
                let blocks = bar((*secs * 1000.0) as u64, max_ms, 6);
                format!("{label} {blocks}")
            })
            .collect::<Vec<_>>()
            .join(" ");
        lines.push(Line::from(text));
    }
    if let Some((label, secs)) = &card.slowest_finished {
        lines.push(Line::from(format!(
            "slowest {label} {}",
            oscilloscope_core::format::format_duration(*secs)
        )));
    }
    let lines = truncate_lines(lines, area.width.saturating_sub(2) as usize);
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
    let effects_highlight = ctx.highlight("header:effects");
    lines.push(Line::from(Span::styled(
        format!("{effects} effects · {} failed", card.failures),
        with_highlight(Style::default(), effects_highlight),
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
        let noun = if count == 1 {
            "quick step"
        } else {
            "quick steps"
        };
        lines.push(Line::from(format!(
            "{count} {noun} · {}",
            oscilloscope_core::format::format_duration(secs)
        )));
    }
    let _ = ctx;
    let lines = truncate_lines(lines, area.width.saturating_sub(2) as usize);
    let paragraph =
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" flow "));
    frame.render_widget(paragraph, area);
}

/// A sparkline over `buckets` (DESIGN.md: "tokens over time as a
/// sparkline"), one block character per bucket, scaled to the
/// busiest one -- empty when nothing has completed yet.
fn sparkline(buckets: &[u64]) -> String {
    const LEVELS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max = buckets.iter().copied().max().unwrap_or(0);
    if max == 0 {
        return String::new();
    }
    buckets
        .iter()
        .map(|&v| {
            let idx = ((v as f64 / max as f64) * (LEVELS.len() - 1) as f64).round() as usize;
            LEVELS[idx.min(LEVELS.len() - 1)]
        })
        .collect()
}

fn draw_cost_card(
    frame: &mut Frame,
    area: Rect,
    card: &oscilloscope_core::screens::CostCard,
    ctx: &DrawCtx,
) {
    let tokens_style = with_highlight(Style::default(), ctx.highlight("header:tokens"));
    let mut lines = vec![Line::styled(
        format!(
            "↑{} ↓{}",
            oscilloscope_core::format::format_tokens(card.tokens_sent),
            oscilloscope_core::format::format_tokens(card.tokens_received)
        ),
        tokens_style,
    )];
    if let Some(cost) = card.cost_usd {
        lines.push(Line::from(format!("${cost:.2}")));
    }
    let spark = sparkline(&card.tokens_over_time);
    if !spark.is_empty() {
        lines.push(Line::from(spark));
    }
    if let Some((path, total)) = &card.top_store {
        lines.push(Line::from(format!(
            "top {path} {}",
            oscilloscope_core::format::format_tokens(*total)
        )));
    }
    // Issue #637: `card.tokens_sent`/`tokens_received` above never
    // include a session that never completed (`runtime.last_run.
    // totals` doesn't have one) -- its last-known figures are shown
    // here instead, marked as stopped rather than folded into the
    // card's own running total.
    if let Some((label, sent, received)) = &card.stopped_agent {
        let sent = sent
            .map(oscilloscope_core::format::format_tokens)
            .unwrap_or_else(|| "—".to_string());
        let received = received
            .map(oscilloscope_core::format::format_tokens)
            .unwrap_or_else(|| "—".to_string());
        lines.push(Line::from(format!("stopped {label} ↑{sent} ↓{received}")));
    }
    let in_flight_style = with_highlight(Style::default(), ctx.highlight("tokenbar"));
    lines.push(Line::styled(card.in_flight.clone(), in_flight_style));
    let lines = truncate_lines(lines, area.width.saturating_sub(2) as usize);
    let paragraph =
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(" cost "));
    frame.render_widget(paragraph, area);
}

#[cfg(test)]
mod truncate_tests {
    use super::*;

    /// A card's own long value ends in `…` once it overflows its own
    /// width (DESIGN.md: "long values truncate with …"), rather than
    /// being clipped mid-word the way the terminal itself would clip
    /// an over-wide line with no wrap.
    #[test]
    fn truncate_line_ends_in_an_ellipsis_once_it_overflows() {
        let line = Line::from("a somewhat long value that will not fit");
        let truncated = truncate_line(line, 10);
        let text: String = truncated.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text.chars().count(), 10);
        assert!(text.ends_with('…'), "{text:?}");
        assert_eq!(text, "a somewha…");
    }

    #[test]
    fn truncate_line_leaves_a_line_that_already_fits_untouched() {
        let line = Line::from("short");
        let truncated = truncate_line(line, 10);
        let text: String = truncated.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "short");
    }

    #[test]
    fn truncate_line_keeps_each_spans_own_style_up_to_the_cut() {
        let line = Line::from(vec![
            Span::styled("abc", Style::default().fg(Color::Red)),
            Span::styled("defgh", Style::default().fg(Color::Blue)),
        ]);
        let truncated = truncate_line(line, 5);
        assert_eq!(truncated.spans[0].content.as_ref(), "abc");
        assert_eq!(truncated.spans[0].style.fg, Some(Color::Red));
        assert_eq!(truncated.spans[1].content.as_ref(), "d");
        assert_eq!(truncated.spans[1].style.fg, Some(Color::Blue));
        assert_eq!(truncated.spans[2].content.as_ref(), "…");
    }
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

/// A strip label too long for `max`, cut from the right with no
/// ellipsis -- unlike [`truncate_label`], a strip's own label set is
/// small and fixed (`events/min (state)`, `decisions`, `in flight`),
/// so cutting it ever actually losing a character would read as a
/// bug rather than an unavoidable abbreviation; this exists only so
/// a pathologically narrow pane doesn't panic slicing past the end.
fn truncate_strip_label(label: &str, max: usize) -> String {
    label.chars().take(max).collect()
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

/// Issue #637's Cost section: "the per-turn strip comes from `turn_
/// tokens`" -- the last `width` values, oldest first, each scaled to
/// one of eight block heights relative to the largest value in that
/// same window (never the whole session's own history), so a single
/// unusually large turn doesn't flatten every other bar in sight.
fn turn_tokens_sparkline(values: &[u64], width: usize) -> String {
    const LEVELS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if values.is_empty() || width == 0 {
        return String::new();
    }
    let slice = if values.len() > width {
        &values[values.len() - width..]
    } else {
        values
    };
    let max = slice.iter().copied().max().unwrap_or(0);
    slice
        .iter()
        .map(|&v| {
            if max == 0 {
                LEVELS[0]
            } else {
                let idx = ((v as f64 / max as f64) * (LEVELS.len() - 1) as f64).round() as usize;
                LEVELS[idx.min(LEVELS.len() - 1)]
            }
        })
        .collect()
}

/// A running agent tile's own second line (issue #637's Cost
/// section): the per-turn strip, and, when there's room left after
/// it, the cache bar -- `tokens_cached_in`'s own share of the tile's
/// `total` (its live `tokens_sent`, the one figure a live tile is
/// ever sized from). `None` for a tile that isn't a live agent at
/// all, or one with neither figure to show.
fn live_turn_line(cell: &oscilloscope_core::treemap::TreemapCell, width: usize) -> Option<String> {
    let live_turns = cell.live_turns.as_ref()?;
    let spark = turn_tokens_sparkline(&live_turns.turn_tokens, width.min(12));
    let cache = live_turns.tokens_cached_in.map(|cached| {
        let pct = if cell.total > 0 {
            (cached as f64 / cell.total as f64 * 100.0).round() as u64
        } else {
            0
        };
        format!("cache {pct}%")
    });
    let text = match (spark.is_empty(), cache) {
        (false, Some(cache)) => format!("{spark} {cache}"),
        (false, None) => spark,
        (true, Some(cache)) => cache,
        (true, None) => return None,
    };
    Some(truncate_label(&text, width))
}

/// A bar of filled block characters, `value`'s own share of `max`
/// (issue #527's "scaled to the larger of the two") — `max == 0`
/// draws an empty bar rather than dividing by zero. Shared by the
/// overview's own time card (`draw_time_card`) and, previously,
/// screen 2's token bar.
fn bar(value: u64, max: u64, width: usize) -> String {
    if max == 0 || width == 0 {
        return String::new();
    }
    let filled = ((value as f64 / max as f64) * width as f64).round() as usize;
    "█".repeat(filled.min(width))
}

/// `secs`, rounded to the nearest whole minute (issue #649's overview
/// figures: `(12m)`, `(p50 45m)`) -- a coarser rounding than
/// [`oscilloscope_core::format::format_duration`]'s own `6m12s`,
/// deliberately: these are "about how long", not a clock.
fn format_minutes(secs: f64) -> String {
    let minutes = (secs / 60.0).round().max(0.0) as u64;
    format!("{minutes}m")
}

/// [`format_minutes`], signed (issue #649's `vs usual +3m`/`-2m`).
fn format_minutes_signed(secs: f64) -> String {
    let minutes = (secs / 60.0).round() as i64;
    if minutes < 0 {
        format!("{minutes}m")
    } else {
        format!("+{minutes}m")
    }
}

/// A running step's own elapsed busy time against its usual one
/// (issue #649's now-pane bar): full blocks, one trailing half-block
/// for a fractional unit, `░` filling the rest -- `value_s` past
/// `usual_s` still fills the bar completely rather than overflowing
/// it.
fn usual_progress_bar(value_s: f64, usual_s: f64, width: usize) -> String {
    if usual_s <= 0.0 || width == 0 {
        return String::new();
    }
    let frac = (value_s / usual_s).clamp(0.0, 1.0);
    let eighths = (frac * width as f64 * 2.0).round() as usize;
    let full = (eighths / 2).min(width);
    let half = eighths % 2 == 1 && full < width;
    let empty = width - full - usize::from(half);
    format!(
        "{}{}{}",
        "█".repeat(full),
        if half { "▌" } else { "" },
        "░".repeat(empty)
    )
}

/// The Cost screen's own squarified-layout cache (issue #632,
/// DESIGN.md's performance rule: "laid out again only when the tree
/// or the area changes") -- owned by the main loop alongside `App`,
/// not folded into it, since it holds ratatui's own `Rect` and the
/// laid-out `TreemapCell`s, neither of which `oscilloscope_core`
/// (pure geometry/model, no ratatui dependency) ever sees.
#[derive(Default)]
pub struct CostLayoutCache {
    key: Option<(Vec<TreemapNode>, Rect)>,
    cells: Vec<oscilloscope_core::treemap::TreemapCell>,
}

impl CostLayoutCache {
    fn cells(
        &mut self,
        nodes: &[TreemapNode],
        area: Rect,
    ) -> &[oscilloscope_core::treemap::TreemapCell] {
        let stale = match &self.key {
            Some((prev_nodes, prev_area)) => prev_nodes.as_slice() != nodes || *prev_area != area,
            None => true,
        };
        if stale {
            self.cells =
                oscilloscope_core::treemap::layout(nodes, area.x, area.y, area.width, area.height);
            self.key = Some((nodes.to_vec(), area));
        }
        &self.cells
    }
}

/// A measure's own value, formatted the way the Cost screen's own
/// treemap and table show it (DESIGN.md: `reviews 517 t`, `$1.10`)
/// -- `total` is always in [`oscilloscope_core::treemap::build_tree_
/// for_measure`]'s own unit for `measure` (tokens, or micro-dollars
/// for [`Measure::Cost`]), never a raw count to format directly.
fn format_measure_value(total: u64, measure: Measure) -> String {
    match measure {
        Measure::Cost => format!(
            "${:.2}",
            oscilloscope_core::treemap::display_cost_usd(total)
        ),
        _ => oscilloscope_core::format::format_tokens(total),
    }
}

/// The Cost screen's own by-store treemap (issue #632, DESIGN.md:
/// "one tile per state store"). Unlike screen 2's old `tokens by
/// key` treemap, there is no single wrapping root cell -- `screen.
/// tree` is already the root's own children, and `app.cost_zoom`
/// (self-healing: `App::cost_level` drops a trailing entry that no
/// longer resolves) picks which level is actually on screen. A tile
/// for a leaf still running shows "tokens when done" instead of a
/// number, whatever its own placeholder weight happens to be
/// (DESIGN.md), gated on exactly the same `status`/`has_children` a
/// `TreemapCell` already carries -- no extra field needed.
fn draw_cost_treemap(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::screens::CostScreen,
    app: &mut App,
    cost_layout: &mut CostLayoutCache,
    ctx: &DrawCtx,
) {
    let level = app.cost_level(&screen.tree).to_vec();
    let mut title = format!(" tokens by store · {} ", screen.measure.label());
    if !app.cost_zoom.is_empty() {
        // The zoomed store's own name at each level, not its full
        // dotted path -- `app.cost_zoom` stacks a *path* per level,
        // and every deeper one already contains every shallower one
        // as a prefix, so joining the raw paths themselves would
        // read `prime.fan › prime.fan.iter_0` instead of `fan ›
        // iter_0`. The measure stays in the title too, so zooming
        // never hides which measure is on screen.
        let breadcrumb: Vec<&str> = app
            .cost_zoom
            .iter()
            .map(|p| p.rsplit('.').next().unwrap_or(p.as_str()))
            .collect();
        title = format!(
            " tokens by store · {} · {} ",
            screen.measure.label(),
            breadcrumb.join(" › ")
        );
    }
    if screen.stores_without_measure > 0 {
        let noun = if screen.stores_without_measure == 1 {
            "store"
        } else {
            "stores"
        };
        title = format!(
            "{} · {} {noun} without {} ",
            title.trim_end(),
            screen.stores_without_measure,
            screen.measure.label()
        );
        title = format!(" {}", title.trim_start());
    }
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if level.is_empty() || inner.width == 0 || inner.height == 0 {
        let paragraph = Paragraph::new("(no stores yet)");
        frame.render_widget(paragraph, inner);
        return;
    }

    let selected = app.cost_selected.clone();
    let cells = cost_layout.cells(&level, inner).to_vec();
    for cell in &cells {
        let rect = Rect::new(cell.rect.x, cell.rect.y, cell.rect.w, cell.rect.h);
        if rect.width == 0 || rect.height == 0 {
            continue;
        }
        let color = cell
            .status
            .map(|s| status_color(s, ctx.theme))
            .unwrap_or(ctx.theme.dim);
        let is_selected = selected.is_some() && cell.path == selected;
        let mut style = Style::default().fg(color);
        if is_selected {
            style = style.add_modifier(Modifier::REVERSED);
        }
        // DESIGN.md: a tile for a step that is still running reads
        // "tokens when done" until it finishes, never a number --
        // true for a container's own tile exactly as much as a
        // leaf's: `cell.total` is always the real, unboosted value
        // ([`oscilloscope_core::treemap::TreemapNode`]'s own doc
        // comment), so a container with at least one real, finished
        // child already shows that real total (never hidden behind
        // "tokens when done" merely because something else under it
        // is still running), while one with nothing but a running
        // placeholder underneath reads exactly like a bare running
        // leaf would. A run that ended (aborted or cancelled) while a
        // leaf was still in flight leaves it in the exact same state
        // -- no `completed_at` ever written -- but it will never
        // reach "done" either, so it reads "no tokens reported"
        // instead, rather than the placeholder weight's own number
        // read back as if it were a real one.
        let value_text = if cell.total > 0 {
            format_measure_value(cell.total, screen.measure)
        } else {
            match cell.status {
                Some(StatusKind::Running) => "tokens when done".to_string(),
                Some(StatusKind::Aborted) | Some(StatusKind::Cancelled) => {
                    "no tokens reported".to_string()
                }
                _ => format_measure_value(cell.total, screen.measure),
            }
        };
        // Issue #637: a running agent's own tile, sized from its
        // live `tokens_sent`, is labelled `live` instead of a bare
        // number -- the one thing `cell.total > 0` alone can no
        // longer tell apart from a container that already has a
        // real, finished child of its own.
        let labelled = if cell.live {
            format!("{} {value_text} live", cell.label)
        } else {
            format!("{} {value_text}", cell.label)
        };
        if cell.has_children {
            let block = Block::default()
                .borders(Borders::ALL)
                .title(truncate_label(
                    &labelled,
                    rect.width.saturating_sub(2) as usize,
                ))
                .border_style(style);
            frame.render_widget(block, rect);
        } else {
            // Same "fill is the area" rule screen 2's old treemap used
            // (DESIGN.md §6.3): a leaf (or a merged `…` bucket) has no
            // border, so its own background fill reads as an area at
            // all.
            let fill = Style::default().bg(color);
            let fill = if is_selected {
                fill.add_modifier(Modifier::REVERSED)
            } else {
                fill
            };
            frame.render_widget(Block::default().style(fill), rect);
            if rect.width >= 2 {
                let label = truncate_label(&labelled, rect.width as usize);
                let text_style = Style::default().bg(color).fg(ctx.theme.on_accent);
                let text_style = if is_selected {
                    text_style.add_modifier(Modifier::REVERSED)
                } else {
                    text_style
                };
                let mut tile_lines = vec![Line::styled(label, text_style)];
                // Issue #637: the per-turn strip and the cache bar,
                // when the tile actually has a second row to draw
                // them on -- never at the cost of the label itself.
                if rect.height >= 2 {
                    if let Some(extra) = live_turn_line(cell, rect.width as usize) {
                        tile_lines.push(Line::styled(extra, text_style));
                    }
                }
                let paragraph = Paragraph::new(tile_lines);
                frame.render_widget(paragraph, rect);
            }
        }
    }
}

/// One of a small, fixed cycle of theme colours for the tokens-over-
/// time chart's own bands (DESIGN.md: "one band per top-level step")
/// -- there is no per-phase colour role in `Theme` itself (phases are
/// document-specific, unlike a fixed status), so this cycles through
/// roles that are already visually distinct from one another.
fn phase_color(index: usize, theme: &Theme) -> Color {
    const PALETTE_SIZE: usize = 6;
    match index % PALETTE_SIZE {
        0 => theme.token_in,
        1 => theme.token_out,
        2 => theme.status_running,
        3 => theme.status_done,
        4 => theme.warning,
        _ => theme.error,
    }
}

/// A fill glyph for one of the chart's own bands, paired with
/// [`phase_color`] (same index, same cycle length) -- DESIGN.md's
/// "still reads from glyphs and modifiers, never colour alone" rule
/// applies here same as everywhere else: two adjacent bands must
/// still read apart with colour turned off.
fn phase_glyph(index: usize) -> char {
    const GLYPHS: [char; 6] = ['█', '▓', '▒', '░', '▐', '▌'];
    GLYPHS[index % GLYPHS.len()]
}

/// The "tokens over time" stacked chart's own clock axis (DESIGN.md:
/// "a clock axis from the run's start to now") -- the start, middle
/// and end of exactly the span `screen.tokens_over_time` was bucketed
/// over, left-, centre- and right-aligned across `width` columns.
/// Blank once the run has no start time yet (nothing has bucketed
/// data to put an axis under).
fn chart_axis_line(
    screen: &oscilloscope_core::screens::CostScreen,
    ctx: &DrawCtx,
    width: usize,
) -> Line<'static> {
    let Some(start_ts) = screen.chart_start_ts.as_deref() else {
        return Line::from("");
    };
    let format = oscilloscope_core::format::format_clock_time;
    let start = format(start_ts, ctx.zone).unwrap_or_default();
    let end = format(&screen.chart_end_ts, ctx.zone).unwrap_or_default();
    let mid =
        oscilloscope_core::format::midpoint_clock_time(start_ts, &screen.chart_end_ts, ctx.zone)
            .unwrap_or_default();
    let mut row = vec![' '; width];
    place_label(&mut row, 0, &start);
    let mid_start = (width / 2).saturating_sub(mid.chars().count() / 2);
    place_label(&mut row, mid_start, &mid);
    place_label(&mut row, width.saturating_sub(end.chars().count()), &end);
    Line::styled(
        row.into_iter().collect::<String>(),
        Style::default().fg(ctx.theme.dim),
    )
}

/// Overwrites `row` starting at `start` with `text`'s own characters
/// -- later calls (the axis's end label, right of its middle one) can
/// overwrite a few trailing spaces of an earlier one on a narrow
/// pane, but never panics: every index is bounds-checked first.
fn place_label(row: &mut [char], start: usize, text: &str) {
    for (i, c) in text.chars().enumerate() {
        if let Some(slot) = row.get_mut(start + i) {
            *slot = c;
        }
    }
}

/// The chart's own legend (DESIGN.md's "never colour alone" rule): one
/// glyph-and-label pair per phase, truncated to `width` as a whole
/// rather than cut mid-pair.
fn chart_legend_line(
    screen: &oscilloscope_core::screens::CostScreen,
    width: usize,
) -> Line<'static> {
    let mut text = String::new();
    for (idx, (label, _)) in screen.tokens_over_time.iter().enumerate() {
        if idx > 0 {
            text.push(' ');
        }
        text.push(phase_glyph(idx));
        text.push(' ');
        text.push_str(label);
    }
    Line::from(truncate_label(&text, width))
}

/// The "tokens over time" stacked chart (DESIGN.md: "a stacked area
/// chart with one band per top-level step, on a clock axis from the
/// run's start to now"). `screen.tokens_over_time` is already
/// bucketed by [`oscilloscope_core::cost::TOKENS_OVER_TIME_BUCKETS`]
/// equal time spans; this rebins those onto exactly as many columns
/// as the pane's own width has, splitting a bucket's value across
/// every column it spans (or summing several buckets into one when
/// the pane is narrower than the bucket count) rather than capping
/// the chart at the bucket count itself, then fills each column's
/// own rows bottom-up, per phase, proportional to that phase's own
/// share of the column's total -- the nearest a character-cell
/// terminal gets to a real stacked area fill. The bottom two rows
/// (whenever there's room for them) are the clock axis and a glyph
/// legend rather than more bars.
fn draw_cost_chart(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::screens::CostScreen,
    ctx: &DrawCtx,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" tokens over time ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let width = inner.width as usize;
    if width == 0 || inner.height == 0 || screen.tokens_over_time.is_empty() {
        return;
    }
    let reserved = if inner.height >= 3 { 2 } else { 0 };
    let bars_height = inner.height as usize - reserved;
    if bars_height == 0 {
        return;
    }
    let buckets = oscilloscope_core::cost::TOKENS_OVER_TIME_BUCKETS;
    let phases = screen.tokens_over_time.len();
    let columns_n = width;
    let mut columns = vec![vec![0u64; phases]; columns_n];
    for (phase_idx, (_, series)) in screen.tokens_over_time.iter().enumerate() {
        for (bucket_idx, &value) in series.iter().enumerate() {
            if value == 0 {
                continue;
            }
            let col_start = bucket_idx * columns_n / buckets;
            let col_end = ((bucket_idx + 1) * columns_n / buckets)
                .max(col_start + 1)
                .min(columns_n);
            let span = (col_end - col_start) as u64;
            let share = value / span;
            let remainder = value % span;
            for (i, col) in (col_start..col_end).enumerate() {
                let extra = u64::from((i as u64) < remainder);
                columns[col][phase_idx] += share + extra;
            }
        }
    }
    let max_total = columns
        .iter()
        .map(|c| c.iter().sum::<u64>())
        .max()
        .unwrap_or(0);
    if max_total == 0 {
        let paragraph = Paragraph::new("(no tokens yet)");
        frame.render_widget(paragraph, inner);
        return;
    }
    // `grid[row][col]` is the phase index filling that cell, or
    // `None` for blank space above a column's own fill.
    let mut grid: Vec<Vec<Option<usize>>> = vec![vec![None; columns_n]; bars_height];
    for (col_idx, col) in columns.iter().enumerate() {
        let col_total = col.iter().sum::<u64>();
        if col_total == 0 {
            continue;
        }
        let col_height =
            ((col_total as f64 / max_total as f64) * bars_height as f64).round() as usize;
        let col_height = col_height.clamp(1, bars_height);
        let mut filled = 0usize;
        for (phase_idx, &value) in col.iter().enumerate() {
            if filled >= col_height {
                break;
            }
            let share = ((value as f64 / col_total as f64) * col_height as f64).round() as usize;
            let share = share.min(col_height - filled);
            for r in 0..share {
                let row = bars_height - 1 - filled - r;
                grid[row][col_idx] = Some(phase_idx);
            }
            filled += share;
        }
    }
    let mut lines: Vec<Line> = grid
        .into_iter()
        .map(|row| {
            let spans: Vec<Span> = row
                .into_iter()
                .map(|cell| match cell {
                    None => Span::raw(" "),
                    Some(phase_idx) => Span::styled(
                        phase_glyph(phase_idx).to_string(),
                        Style::default().fg(phase_color(phase_idx, ctx.theme)),
                    ),
                })
                .collect();
            Line::from(spans)
        })
        .collect();
    if reserved == 2 {
        lines.push(chart_axis_line(screen, ctx, width));
        lines.push(chart_legend_line(screen, width));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The "largest stores" table (DESIGN.md: "path, tokens in, tokens
/// out, cost (when reported) and share of the total"), with its own
/// footer (the run's own totals, and the gap between them and the
/// table, when there is one).
/// `↑`/`↓`/`⏎`/`esc` select and zoom the table the same way they do
/// the treemap (DESIGN.md: "the table alone, where ⏎ still zooms") --
/// `app.cost_level`'s own self-healing walk of `app.cost_zoom` is
/// what the table's own rows are filtered to, not a second, possibly
/// diverging, copy of that walk, and a row matching `app.cost_
/// selected` is reversed the same way a selected treemap tile is.
fn draw_cost_table(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::screens::CostScreen,
    app: &mut App,
    ctx: &DrawCtx,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" largest stores ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let zoomed = !app.cost_zoom.is_empty();
    let level = app.cost_level(&screen.tree);
    let in_scope: Vec<&str> = level.iter().map(|n| n.path.as_str()).collect();
    let selected = app.cost_selected.clone();
    // Unzoomed, every row already descends from some top-level tile
    // by construction -- skipping the scope check entirely there
    // keeps this `O(rows)`, not `O(rows × level)`, for the common
    // case (issue #527's own 2,000-node redraw-cost bound covers the
    // Cost screen too).
    let in_path_scope = |path: &str| -> bool {
        in_scope.iter().any(|p| {
            path == *p
                || (path.len() > p.len() && path.starts_with(p) && path.as_bytes()[p.len()] == b'.')
        })
    };
    let rows: Vec<&oscilloscope_core::cost::StoreRow> = screen
        .stores
        .rows
        .iter()
        .filter(|r| !zoomed || in_path_scope(&r.path))
        .collect();
    let total_tokens: u64 = rows.iter().map(|r| r.tokens_sent + r.tokens_received).sum();
    // Relative to the zoomed store, not the whole run, once zoomed --
    // `row.label` itself is always root-relative (`oscilloscope_core
    // ::cost::StoreRow`'s own doc comment), so a zoomed view re-
    // derives it from the row's own real path instead of trying to
    // strip a prefix back out of an already-built string.
    let zoom_root = app.cost_zoom.last().map(String::as_str);
    let mut row_lines: Vec<Line> = Vec::new();
    for row in &rows {
        let share = if total_tokens > 0 {
            (row.tokens_sent + row.tokens_received) as f64 / total_tokens as f64 * 100.0
        } else {
            0.0
        };
        let cost = match row.cost_usd {
            Some(c) => format!("  ${c:.2}"),
            None => String::new(),
        };
        let color = row
            .status
            .map(|s| status_color(s, ctx.theme))
            .unwrap_or(ctx.theme.text);
        let mut style = Style::default().fg(color);
        // The orchestration pane's own selection can name a
        // container (`fan`), not only a leaf -- in table-only mode
        // `↑`/`↓` walk the treemap's own tiles, so a selected
        // container never has a row whose path matches it exactly
        // unless that container also happens to be a `while: mode:
        // model` loop with a `check` row of its own. Every row *under*
        // the selection (its path, plus `.`) is reversed too, the same
        // as the treemap highlights every descendant tile of a
        // selected container.
        let is_selected = selected.as_deref().is_some_and(|sel| {
            row.path == sel
                || (row.path.len() > sel.len()
                    && row.path.starts_with(sel)
                    && row.path.as_bytes()[sel.len()] == b'.')
        });
        if is_selected {
            style = style.add_modifier(Modifier::REVERSED);
        }
        // The same "nothing real yet" rule the treemap's own tiles
        // read by (`draw_cost_treemap`'s `value_text`): a running (or
        // ended-without-finishing) row with nothing reported shows
        // the same placeholder text its tile does, never `↑0 t ↓0 t
        // 0%` read back as if it meant something.
        let value_text = if row.tokens_sent + row.tokens_received > 0 {
            format!(
                "↑{} ↓{}{cost}  {share:.0}%",
                oscilloscope_core::format::format_tokens(row.tokens_sent),
                oscilloscope_core::format::format_tokens(row.tokens_received),
            )
        } else {
            match row.status {
                Some(StatusKind::Running) => "tokens when done".to_string(),
                Some(StatusKind::Aborted) | Some(StatusKind::Cancelled) => {
                    "no tokens reported".to_string()
                }
                _ => format!(
                    "↑{} ↓{}{cost}  {share:.0}%",
                    oscilloscope_core::format::format_tokens(row.tokens_sent),
                    oscilloscope_core::format::format_tokens(row.tokens_received),
                ),
            }
        };
        let label = match zoom_root {
            Some(root) => oscilloscope_core::cost::zoomed_row_label(row, root),
            None => row.label.clone(),
        };
        // Shortened from the left (`… › iter_1 › review`), never the
        // right: the leaf is the one segment that actually has to
        // stay readable to tell two same-named rows apart.
        let label_budget = (inner.width as usize).saturating_sub(2 + value_text.chars().count());
        let label = oscilloscope_core::cost::shorten_breadcrumb_from_left(&label, label_budget);
        row_lines.push(Line::styled(format!("{label}  {value_text}"), style));
    }

    let footer = &screen.stores.footer;
    let mut footer_lines: Vec<Line> = Vec::new();
    // The coverage note (`$4.20 (from 3 of 9 prompts)`) gets its own
    // line, not appended to the totals line -- issue #632's small-
    // terminal acceptance item: sharing one line with `total ↑X ↓Y`
    // left only the table pane's own narrower width (beside the
    // chart, DESIGN.md's 62/38 split) for both, truncating the label
    // well before the pane itself ran out of room for it alone.
    footer_lines.push(Line::styled(
        format!(
            "total ↑{} ↓{}",
            oscilloscope_core::format::format_tokens(footer.run_tokens_sent),
            oscilloscope_core::format::format_tokens(footer.run_tokens_received),
        ),
        Style::default().fg(ctx.theme.dim),
    ));
    if let Some(cost_label) = footer.cost_label_for_width(inner.width as usize) {
        footer_lines.push(Line::styled(cost_label, Style::default().fg(ctx.theme.dim)));
    }
    if footer.gap_tokens > 0 {
        footer_lines.push(Line::styled(
            format!(
                "{} not in the table above",
                oscilloscope_core::format::format_tokens(footer.gap_tokens)
            ),
            Style::default().fg(ctx.theme.dim),
        ));
    }

    // The footer (the run's own totals, its gap, its cost coverage)
    // always stays visible, right after the rows: with more rows than
    // fit, rows are what gets cut, ending in a "+N more" line, never
    // the footer itself scrolling off the bottom of a `Paragraph` that
    // simply stops drawing past its own area -- which is exactly what
    // appending the footer after every row, unconditionally, used to
    // risk.
    let reserved = footer_lines.len() + 1; // +1: the blank separator line, always present.
    let available_for_rows = (inner.height as usize).saturating_sub(reserved);
    let mut lines: Vec<Line> = Vec::new();
    if row_lines.len() <= available_for_rows {
        lines.extend(row_lines);
    } else if available_for_rows > 0 {
        let shown = available_for_rows - 1; // -1: this tier's own "+N more" line.
        let dropped = row_lines.len() - shown;
        lines.extend(row_lines.into_iter().take(shown));
        lines.push(Line::styled(
            format!("+{dropped} more"),
            Style::default().fg(ctx.theme.dim),
        ));
    }
    lines.push(Line::from(""));
    lines.extend(footer_lines);

    let lines = truncate_lines(lines, inner.width as usize);
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Screen 3 (`Cost`, issue #632). DESIGN.md's own small-terminal
/// rules: at [`thresholds::COST_CHART_MIN_WIDTH`]/`_MIN_HEIGHT` (the
/// mockup's 100×30) and up, the treemap (62% of the height) sits
/// above the chart and the table, side by side at 62/38; below that
/// but still at [`thresholds::COST_TREEMAP_MIN_WIDTH`]/`_MIN_HEIGHT`
/// (80×20), the chart is dropped and the table takes the full width
/// under the treemap; below that, the table alone, where `⏎` still
/// zooms it.
fn draw_cost_screen(
    frame: &mut Frame,
    frame_area: Rect,
    area: Rect,
    screen: &oscilloscope_core::screens::CostScreen,
    app: &mut App,
    cost_layout: &mut CostLayoutCache,
    ctx: &DrawCtx,
) {
    let show_treemap = frame_area.width >= thresholds::COST_TREEMAP_MIN_WIDTH
        && frame_area.height >= thresholds::COST_TREEMAP_MIN_HEIGHT;
    if !show_treemap {
        draw_cost_table(frame, area, screen, app, ctx);
        return;
    }
    let treemap_rows = oscilloscope_core::layout::golden_major_section(area.height);
    let rest_rows = area.height - treemap_rows;
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(treemap_rows), Constraint::Min(rest_rows)])
        .split(area);
    draw_cost_treemap(frame, rows[0], screen, app, cost_layout, ctx);

    let show_chart = frame_area.width >= thresholds::COST_CHART_MIN_WIDTH
        && frame_area.height >= thresholds::COST_CHART_MIN_HEIGHT;
    if !show_chart {
        draw_cost_table(frame, rows[1], screen, app, ctx);
        return;
    }
    let chart_width = oscilloscope_core::layout::golden_major_section(rows[1].width);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(chart_width),
            Constraint::Min(rows[1].width - chart_width),
        ])
        .split(rows[1]);
    draw_cost_chart(frame, cols[0], screen, ctx);
    draw_cost_table(frame, cols[1], screen, app, ctx);
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

// Every line here fits inside `HELP_OVERLAY_WIDTH`'s own interior
// with no wrapping, and there are few enough of them that `draw_
// dialog_overlay`'s own `HELP_TEXT.len() + 2` height still fits a
// bare 80×24 terminal (`centered` clamps a taller popup to the
// screen's own height, which used to cut "press any key to close"
// off the bottom there).
const HELP_TEXT: &[&str] = &[
    "0 1 2 3      overview · time · flow · cost",
    "↑↓ / j k     move (the focused pane, or cost: select a store)",
    "PgUp PgDn    move a page",
    "Home End     jump to the first / last row",
    "g G          same as Home / End",
    "← →          collapse / expand (orchestration or state)",
    "⏎            overview: open flow; flow/time: expand; cost: zoom in",
    "+ - =        time: zoom in / out / fit",
    "esc i o $    cost: zoom out · switch measure (same key: total)",
    "z            fold on/off (breadcrumbs, quick steps)",
    "tab          flow/time: cycle panes (else: tree → details)",
    "s / l        swap selected for state / story for the log",
    "f            follow the running row (on by default)",
    "e            errors only",
    "/            filter",
    "v            show the full value",
    "t            cycle the colour theme",
    "c / q        cancel / quit (both confirm while a run is going)",
    "?            this help",
    "no output token count until a prompt finishes",
    "press any key to close",
];

/// Wide enough for every [`HELP_TEXT`] line with none of them
/// wrapping -- the longest (the ⏎ line) is 66 columns; the
/// 2-column border leaves no margin to spare, so a new line must
/// stay at or under that same length.
const HELP_OVERLAY_WIDTH: u16 = 68;

/// One redraw's whole layout (issue #626): a one-line header, shared
/// by every screen (replacing the old three-row one), then the
/// screen currently on display (`app.screen`) — screen 2 (`Flow`,
/// issue #635) is the tree as control flow, the selected step and the
/// run's story; screen 1 (`Time`) isn't built yet and draws nothing.
/// `overview`/`cost_screen`/`flow_screen` are screens 0, 3 and 2's own
/// models, `Some` only while that screen is actually on display
/// (issue #626's own performance rule: "models for screens not on
/// display aren't built"); `cost_layout` is the Cost screen's own
/// squarify-layout cache, re-laid out only when its own tree or area
/// actually changes. `raw_state` is the engine's own last snapshot,
/// untruncated — `details`' own `prompt_sent`/`value` are already cut
/// to `DETAILS_PREVIEW_CHARS` for the details pane, which `v`'s own
/// full-value overlay must not be.
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
    cost_screen: Option<&oscilloscope_core::screens::CostScreen>,
    flow_screen: Option<&oscilloscope_core::screens::FlowScreen>,
    time_screen: Option<&oscilloscope_core::screens::TimeScreen>,
    transcript_view: Option<&oscilloscope_core::transcript::TranscriptView>,
    cost_layout: &mut CostLayoutCache,
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

    // `draw_header_line` appends the theme banner (`t`'s own, issue
    // #535) after the line `header_line` itself already sized to
    // the full width -- reserving room for it here, not after, is
    // what keeps the two from overflowing past `area.width` together.
    let banner_width = ctx
        .theme_banner
        .map(|name| format!("  theme: {name}").chars().count() as u16)
        .unwrap_or(0);
    let segments = oscilloscope_core::screens::header_line(
        &state.header,
        now,
        ctx.zone,
        area.width.saturating_sub(banner_width),
        !ctx.no_animation,
        icon_frame,
        Some(app.screen),
    );
    draw_header_line(frame, sections[0], &segments, ctx);

    let body = sections[1];
    match app.screen {
        oscilloscope_core::screens::Screen::Overview => {
            if let Some(overview) = overview {
                draw_overview(frame, body, overview, transcript_view, ctx);
            }
        }
        oscilloscope_core::screens::Screen::Cost => {
            if let Some(cost_screen) = cost_screen {
                draw_cost_screen(frame, area, body, cost_screen, app, cost_layout, ctx);
            }
        }
        oscilloscope_core::screens::Screen::Flow => {
            if let Some(flow_screen) = flow_screen {
                draw_flow_screen(
                    frame,
                    area,
                    body,
                    state,
                    flow_screen,
                    log_lines,
                    log_total,
                    app,
                    transcript_view,
                    ctx,
                );
            }
        }
        oscilloscope_core::screens::Screen::Time => {
            if let Some(time_screen) = time_screen {
                draw_time_screen(
                    frame,
                    area,
                    body,
                    state,
                    time_screen,
                    app,
                    transcript_view,
                    ctx,
                    now,
                );
            }
        }
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

/// One Flow-pane row's own line(s) (issue #635): the status glyph
/// and label, the loop-progress/duration/dim-provider fragments every
/// ordinary row already has, plus the Flow screen's own additions --
/// a decision's `→ then`/`→ else`, the untaken-branch summary's own
/// dim styling, and the start/tokens/cost columns, appended after the
/// label and dropped narrowest-first. `show_condition` is `FLOW_
/// CONDITION_LINE_MIN_WIDTH` already checked by the caller once for
/// the whole pane.
fn flow_row_lines(
    row: &oscilloscope_core::flow::FlowRow,
    columns: FlowColumns,
    show_condition: bool,
    selected: bool,
    ctx: &DrawCtx,
) -> Vec<Line<'static>> {
    use oscilloscope_core::flow::FlowRowKind;
    let indent = "  ".repeat(row.depth);
    let mut main_style = Style::default();
    let mut spans = Vec::new();
    match &row.kind {
        FlowRowKind::Row(_) => {
            let glyph = if row.merged_pass {
                '⟲'
            } else {
                status_glyph(row.status.kind)
            };
            spans.push(Span::styled(
                format!("{indent}{glyph} "),
                status_style(row.status.kind, ctx.theme),
            ));
            spans.push(Span::raw(row.label.clone()));
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
        }
        FlowRowKind::Decision {
            branch,
            inferred,
            nothing_ran,
            ..
        } => {
            spans.push(Span::raw(format!("{indent}◆ {}", row.label)));
            // The label itself already carries the arrow for the
            // same-names case (`if → ?`, `unnamed_if_label`'s own
            // output never otherwise ends this way) -- a second,
            // separate `…` right after it would just repeat the
            // same "not yet decided" meaning twice.
            if !row.label.ends_with("→ ?") {
                let mut arrow = match branch {
                    Some(b) => format!("  → {b}"),
                    None => "  …".to_string(),
                };
                if *nothing_ran {
                    arrow.push_str(" (nothing ran)");
                }
                spans.push(Span::styled(arrow, Style::default().fg(ctx.theme.dim)));
            }
            if *inferred {
                spans.push(Span::styled(
                    "  (inferred)",
                    Style::default().fg(ctx.theme.dim),
                ));
            }
        }
        FlowRowKind::UntakenBranch { .. } => {
            spans.push(Span::styled(
                format!("{indent}· {}", row.label),
                Style::default().fg(ctx.theme.dim),
            ));
            main_style = main_style.add_modifier(Modifier::DIM);
        }
    }
    let mut right = Vec::new();
    if columns.start {
        if let Some(start) = row
            .start_ts
            .as_deref()
            .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone))
        {
            right.push(start);
        }
    }
    if let Some(duration) = row.duration_s {
        right.push(oscilloscope_core::format::format_duration(duration));
    }
    if columns.tokens {
        if let Some(tokens) = row.tokens {
            right.push(oscilloscope_core::format::format_tokens(tokens));
        }
    }
    if columns.cost {
        if let Some(cost) = row.cost_usd {
            right.push(format!("${cost:.2}"));
        }
    }
    if !right.is_empty() {
        spans.push(Span::styled(
            format!("   {}", right.join("  ")),
            Style::default().fg(ctx.theme.dim),
        ));
    }
    if selected {
        main_style = main_style.add_modifier(Modifier::REVERSED);
    }
    main_style = with_highlight(main_style, ctx.highlight(&format!("row:{}", row.path)));
    let mut lines = vec![Line::from(spans).style(main_style)];
    if show_condition {
        if let Some(condition) = &row.condition {
            lines.push(Line::styled(
                format!("{indent}  {}", condition.text),
                Style::default()
                    .fg(ctx.theme.dim)
                    .add_modifier(Modifier::DIM),
            ));
        }
    }
    lines
}

/// [`flow_row_lines`]'s own column visibility (issue #635: "narrow
/// panes drop columns in this order: cost, start, tokens. Duration
/// always stays"), decided once per redraw from the flow pane's own
/// interior width.
#[derive(Debug, Clone, Copy)]
struct FlowColumns {
    cost: bool,
    start: bool,
    tokens: bool,
}

fn flow_columns_for_width(width: u16) -> FlowColumns {
    FlowColumns {
        cost: width >= thresholds::FLOW_COST_COLUMN_MIN_WIDTH,
        start: width >= thresholds::FLOW_START_COLUMN_MIN_WIDTH,
        tokens: width >= thresholds::FLOW_TOKENS_COLUMN_MIN_WIDTH,
    }
}

#[cfg(test)]
mod flow_columns_tests {
    use super::*;

    /// DESIGN.md: "narrow panes drop columns in this order: cost,
    /// start, tokens. Duration always stays" -- duration itself has
    /// no threshold of its own at all (`flow_row_lines` always draws
    /// it), so this only ever needs to check the three that do.
    #[test]
    fn narrower_than_every_threshold_drops_cost_start_and_tokens() {
        let columns = flow_columns_for_width(thresholds::FLOW_TOKENS_COLUMN_MIN_WIDTH - 1);
        assert!(!columns.cost);
        assert!(!columns.start);
        assert!(!columns.tokens);
    }

    #[test]
    fn wide_enough_for_tokens_alone_keeps_only_tokens() {
        let columns = flow_columns_for_width(thresholds::FLOW_TOKENS_COLUMN_MIN_WIDTH);
        assert!(!columns.cost);
        assert!(!columns.start);
        assert!(columns.tokens);
    }

    #[test]
    fn wide_enough_for_start_keeps_tokens_and_start_but_not_cost() {
        let columns = flow_columns_for_width(thresholds::FLOW_START_COLUMN_MIN_WIDTH);
        assert!(!columns.cost);
        assert!(columns.start);
        assert!(columns.tokens);
    }

    #[test]
    fn wide_enough_for_cost_keeps_every_column() {
        let columns = flow_columns_for_width(thresholds::FLOW_COST_COLUMN_MIN_WIDTH);
        assert!(columns.cost);
        assert!(columns.start);
        assert!(columns.tokens);
    }
}

/// Resolves `app.selected_path` (set by an ordinary move, or by
/// follow/`⏎`-on-overview picking a real tree path directly) to
/// the Flow pane's own drawn row (issue #635): a path under a
/// single-pass loop's own merged pass maps to that loop's row, and a
/// path hidden entirely under a decision's untaken branch maps to
/// that branch's own dim summary line -- `rows` is checked for an
/// exact match first, so an ordinary selection (already a real,
/// drawn row's own path) never takes either fallback.
fn flow_selected_row_path(
    rows: &[&oscilloscope_core::flow::FlowRow],
    selected: &str,
) -> Option<String> {
    if let Some(row) = rows.iter().find(|r| r.path == selected) {
        return Some(row.path.clone());
    }
    if let Some(row) = rows
        .iter()
        .find(|r| r.merged_pass && selected.starts_with(&format!("{}.iter_", r.path)))
    {
        return Some(row.path.clone());
    }
    rows.iter().find_map(|r| match &r.kind {
        oscilloscope_core::flow::FlowRowKind::UntakenBranch { paths } => {
            is_under_any_untaken_path(selected, paths).then(|| r.path.clone())
        }
        _ => None,
    })
}

fn is_under_any_untaken_path(path: &str, roots: &[String]) -> bool {
    roots
        .iter()
        .any(|root| path == root || path.starts_with(&format!("{root}.")))
}

#[cfg(test)]
mod flow_selected_row_path_tests {
    use super::*;
    use oscilloscope_core::flow::{FlowRow, FlowRowKind};
    use oscilloscope_core::model::{RowStatus, StatusKind};
    use oscilloscope_core::render::RowKind;

    fn flow_row(path: &str, kind: FlowRowKind, merged_pass: bool) -> FlowRow {
        FlowRow {
            path: path.to_string(),
            depth: 0,
            label: path.rsplit('.').next().unwrap().to_string(),
            kind,
            status: RowStatus {
                kind: StatusKind::Running,
                skip_reason: None,
                retrying: false,
            },
            start_ts: None,
            duration_s: None,
            tokens: None,
            cost_usd: None,
            condition: None,
            loop_progress: None,
            has_children: false,
            merged_pass,
        }
    }

    #[test]
    fn an_exact_match_is_returned_as_is() {
        let rows = [flow_row("prime", FlowRowKind::Row(RowKind::Tool), false)];
        let refs: Vec<&FlowRow> = rows.iter().collect();
        assert_eq!(
            flow_selected_row_path(&refs, "prime"),
            Some("prime".to_string())
        );
    }

    #[test]
    fn a_merged_passs_own_pass_maps_to_its_loop_row() {
        let rows = [flow_row("prime.fan", FlowRowKind::Row(RowKind::Loop), true)];
        let refs: Vec<&FlowRow> = rows.iter().collect();
        assert_eq!(
            flow_selected_row_path(&refs, "prime.fan.iter_0"),
            Some("prime.fan".to_string())
        );
    }

    #[test]
    fn a_step_under_an_untaken_branch_maps_to_its_summary_line() {
        let rows = [flow_row(
            "prime.gate.__untaken",
            FlowRowKind::UntakenBranch {
                paths: vec!["prime.gate.other".to_string()],
            },
            false,
        )];
        let refs: Vec<&FlowRow> = rows.iter().collect();
        assert_eq!(
            flow_selected_row_path(&refs, "prime.gate.other"),
            Some("prime.gate.__untaken".to_string())
        );
    }

    #[test]
    fn a_path_covered_by_nothing_drawn_resolves_to_none() {
        let rows = [flow_row("prime", FlowRowKind::Row(RowKind::Tool), false)];
        let refs: Vec<&FlowRow> = rows.iter().collect();
        assert_eq!(flow_selected_row_path(&refs, "prime.other"), None);
    }
}

/// The flow pane (issue #635's left pane): today's orchestration-pane
/// keys (`↑↓`/`jk`, `PgUp`/`PgDn`, collapse, follow, `z`, `e`, `/`,
/// `⏎` on a quick-steps row) still act on the real `Row`s
/// (`App::selected_path`'s own model) -- the Flow screen's own
/// synthetic decision/untaken-branch rows are display-only here, read
/// straight from `screen.rows` for exactly the positions the real
/// rows don't already cover.
#[allow(clippy::too_many_arguments)]
fn draw_flow_pane(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::screens::FlowScreen,
    visible: &[&Row],
    app: &mut App,
    header: &oscilloscope_core::render::Header,
    frame_width: u16,
    // DESIGN.md: "Below 80×20: one column ... the flow pane shows
    // duration only" -- the one-column layout's own flow pane often
    // ends up no narrower than the two-column one's left column (it
    // gets the whole body width instead of a golden-section share of
    // it), so this can't be the width thresholds' own call to make.
    one_column: bool,
    ctx: &DrawCtx,
) {
    let footer_lines = 3u16;
    let viewport = area.height.saturating_sub(2 + footer_lines) as usize;
    app.tree_viewport = viewport;

    let columns = if one_column {
        FlowColumns {
            cost: false,
            start: false,
            tokens: false,
        }
    } else {
        flow_columns_for_width(area.width)
    };
    let show_condition = frame_width >= thresholds::FLOW_CONDITION_LINE_MIN_WIDTH;
    let visible_paths: std::collections::BTreeSet<&str> =
        visible.iter().map(|r| r.path.as_str()).collect();
    // Issue #635: the one list of drawn lines, in the exact
    // order they're drawn -- a real row collapse/filter/errors-only
    // would hide, by the same rule `visible_paths` already applies
    // elsewhere; the synthetic rows this module invents (an unnamed
    // decision, `inferred: true`; an untaken branch's summary line)
    // carry no real path for that state to hide at all, so they're
    // always drawn. Scrolling and selection both move over exactly
    // this list, never the orchestration pane's own `visible` -- that
    // list still has the untaken branch's own hidden real rows (never
    // drawn here at all) and the pass a single-pass loop merged away
    // (drawn as the loop row instead), so clamping or indexing
    // against it directly could point `j`/`k`/the scrollbar at a
    // position nothing here actually shows.
    let drawable: Vec<&oscilloscope_core::flow::FlowRow> = screen
        .rows
        .iter()
        .filter(|row| {
            !oscilloscope_core::flow::is_real_flow_row(&row.kind)
                || visible_paths.contains(row.path.as_str())
        })
        .collect();
    let selected_key = app
        .selected_path
        .as_deref()
        .and_then(|p| flow_selected_row_path(&drawable, p));
    let selected_idx = selected_key
        .as_deref()
        .and_then(|key| drawable.iter().position(|r| r.path == key));
    // A decision row with a condition takes two screen lines, not
    // one (`flow_row_lines`), so scrolling and the viewport both
    // have to work over line counts rather than row counts, the
    // same way `draw_state` already does for a token line.
    let line_counts: Vec<usize> = drawable
        .iter()
        .map(|row| 1 + (show_condition && row.condition.is_some()) as usize)
        .collect();
    app.tree_scroll = clamp_state_scroll(app.tree_scroll, selected_idx, &line_counts, viewport);

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut end_idx = app.tree_scroll;
    for row in drawable.iter().skip(app.tree_scroll) {
        let selected = selected_key.as_deref() == Some(row.path.as_str());
        let row_lines = flow_row_lines(row, columns, show_condition, selected, ctx);
        if lines.len() + row_lines.len() > viewport {
            break;
        }
        lines.extend(row_lines);
        end_idx += 1;
    }

    let selectable: Vec<&oscilloscope_core::flow::FlowRow> = drawable
        .iter()
        .filter(|r| oscilloscope_core::flow::is_real_flow_row(&r.kind))
        .copied()
        .collect();
    let selectable_idx = selected_key
        .as_deref()
        .and_then(|key| selectable.iter().position(|r| r.path == key));
    let more_planned = drawable
        .iter()
        .skip(end_idx)
        .filter(|r| r.status.kind == oscilloscope_core::model::StatusKind::Pending)
        .count() as u64;

    let border_style = if app.pane == Pane::Tree {
        Style::default().fg(ctx.theme.border_focused)
    } else {
        Style::default().fg(ctx.theme.border)
    };
    // Issue #626: the effects count sits in this pane's own title
    // (moved off the header line entirely), `N/M` or `N/M+` once a
    // loop's own total is still a lower bound (`effects_planned_is_
    // lower_bound`), or just `N` with no plan at all.
    let effects = match header.effects_planned {
        Some(planned) if header.effects_planned_is_lower_bound => {
            format!("{}/{planned}+", header.effects_done)
        }
        Some(planned) => format!("{}/{planned}", header.effects_done),
        None => header.effects_done.to_string(),
    };
    let mut title = format!(
        " flow ({} · {effects}) ",
        orchestration_name(&header.document)
    );
    if app.follow {
        title.push_str("(following) ");
    }
    title.push_str("◆ decision ⟲ loop ▶ running");
    match scroll_indicator(selectable_idx, selectable.len(), viewport) {
        Some(indicator) => title.push_str(&indicator),
        None => title.push(' '),
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(border_style);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    // The list is drawn only into the rows above the 3-line footer
    // (`viewport` is already `inner`'s height minus those 3 lines),
    // so a row the loop above couldn't fit never lands on the
    // footer's own lines underneath it.
    let list_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: viewport as u16,
    };
    let list = List::new(lines.into_iter().map(ListItem::new).collect::<Vec<_>>());
    frame.render_widget(list, list_area);

    draw_flow_footer(frame, area, &screen.footer, more_planned, ctx);
}

/// The flow pane's own footer, drawn as the bottom two rows inside
/// its own border (issue #635: "quick steps folded: …", "health: N
/// failed · N retries · N warnings", "○ N more planned").
fn draw_flow_footer(
    frame: &mut Frame,
    area: Rect,
    footer: &oscilloscope_core::flow::FlowFooter,
    more_planned: u64,
    ctx: &DrawCtx,
) {
    if area.height < 5 {
        return;
    }
    let quick = format!(
        "quick steps folded: {} · {}",
        footer.quick_steps_folded,
        oscilloscope_core::format::format_duration(footer.quick_steps_duration_s)
    );
    let health = format!(
        "health: {} failed · {} retries · {} warnings",
        footer.failed, footer.retries, footer.warnings
    );
    let footer_area = Rect {
        x: area.x + 1,
        y: area.y + area.height.saturating_sub(4),
        width: area.width.saturating_sub(2),
        height: 3,
    };
    let mut lines = vec![
        Line::styled(quick, Style::default().fg(ctx.theme.dim)),
        Line::styled(health, Style::default().fg(ctx.theme.dim)),
    ];
    if more_planned > 0 {
        lines.push(Line::styled(
            format!("○ {more_planned} more planned"),
            Style::default().fg(ctx.theme.dim),
        ));
    }
    frame.render_widget(ratatui::widgets::Paragraph::new(lines), footer_area);
}

/// The selected pane (issue #635's right/top pane): an ordinary step
/// reuses the overview's own `focus_step_lines`, a decision reads its
/// own condition/branch/answer instead, and `s` swaps the whole pane
/// for today's state tree (#610).
#[allow(clippy::too_many_arguments)]
fn draw_flow_selected_pane(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::screens::FlowScreen,
    state_visible: &[&StateRow],
    app: &mut App,
    transcript_view: Option<&oscilloscope_core::transcript::TranscriptView>,
    ctx: &DrawCtx,
) {
    if app.flow_selected_shows_state {
        draw_state(frame, area, state_visible, app, ctx);
        return;
    }
    let lines = if let Some(decision) = &screen.selected_decision {
        decision_detail_lines(decision, ctx)
    } else if let Some(step) = &screen.selected {
        flow_selected_step_lines(step, transcript_view, ctx)
    } else {
        vec![Line::from("nothing selected")]
    };
    let border_style = if app.pane == Pane::Details {
        Style::default().fg(ctx.theme.border_focused)
    } else {
        Style::default().fg(ctx.theme.border)
    };
    let viewport = area.height.saturating_sub(2) as usize;
    app.details_viewport = viewport;
    let total = lines.len();
    app.details_scroll = app.details_scroll.min(total.saturating_sub(1));
    let start = app.details_scroll.min(total);
    let visible_lines: Vec<Line> = lines.into_iter().skip(start).take(viewport).collect();
    let paragraph = ratatui::widgets::Paragraph::new(visible_lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" selected ")
            .border_style(border_style),
    );
    frame.render_widget(paragraph, area);
}

/// The selected pane's own asked/doing/produced sections (issue
/// #635) -- built from the same [`FocusedStep`]/[`FocusDetail`]
/// model the overview's now pane uses (so issue #612's live agent
/// data, once it lands, fills both panes at once), with the
/// prompt text, the in-flight estimate and the reply read only
/// through `Details::sent`/`received` (never `meta.prompt_sent` or
/// `meta.answer` directly), so a later fix to either flows through
/// here unchanged.
fn flow_selected_step_lines(
    step: &oscilloscope_core::screens::FocusedStep,
    transcript_view: Option<&oscilloscope_core::transcript::TranscriptView>,
    ctx: &DrawCtx,
) -> Vec<Line<'static>> {
    use oscilloscope_core::model::StatusKind;
    use oscilloscope_core::screens::FocusDetail;
    let dim = Style::default().fg(ctx.theme.dim);
    let mut lines = vec![Line::from(step.breadcrumb.clone())];
    let mut meta = vec![step.kind_label.to_string()];
    if let Some(provider_or_model) = &step.provider_or_model {
        meta.push(provider_or_model.clone());
    }
    lines.push(Line::styled(meta.join(" · "), dim));

    let running = matches!(
        step.status.kind,
        StatusKind::Running | StatusKind::LikelyRunning | StatusKind::RunningOrQueued
    );
    let finished = matches!(
        step.status.kind,
        StatusKind::Done | StatusKind::Failed | StatusKind::FailedHandled
    );

    let asked = match &step.detail {
        FocusDetail::Prompt { snippet, .. } => snippet.clone(),
        FocusDetail::Tool { command } => command.clone(),
        FocusDetail::Agent { asked, .. } => asked.clone(),
        FocusDetail::Plain => None,
    };
    if let Some(asked) = asked {
        lines.push(Line::styled("asked:", dim));
        lines.push(Line::from(asked));
    }

    let mut doing = Vec::new();
    match &step.detail {
        FocusDetail::Prompt {
            tokens, estimated, ..
        } if running => {
            let mut text = "waiting for the reply".to_string();
            if let Some(tokens) = tokens {
                let prefix = if *estimated { "≈" } else { "" };
                text.push_str(&format!(
                    " · {prefix}{} tokens sent",
                    oscilloscope_core::format::format_tokens(*tokens)
                ));
            }
            doing.push(Line::from(text));
        }
        FocusDetail::Tool { .. } if running => {
            let text = match step.elapsed_s {
                Some(elapsed) => format!(
                    "running {}",
                    oscilloscope_core::format::format_duration(elapsed)
                ),
                None => "running".to_string(),
            };
            doing.push(Line::from(text));
        }
        // `live.is_some()` without `running` is an agent node whose
        // own run has already ended (cancelled, failed or aborted,
        // issue #637): `meta.live` is left behind with no
        // `completed_at`, and `stale` (computed from `now` the
        // instant this step was built) is always `true` by then, so
        // this never reads as running -- only `stopped`, with
        // whatever figures the session last reported.
        FocusDetail::Agent {
            live, note, stale, ..
        } if running || live.is_some() => {
            if let (Some(since), Some(elapsed)) = (
                step.since_ts
                    .as_deref()
                    .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone)),
                step.elapsed_s,
            ) {
                doing.push(Line::from(format!(
                    "since {since} · {}",
                    oscilloscope_core::format::format_duration(elapsed)
                )));
            }
            match live.as_deref() {
                Some(live) => {
                    if *stale {
                        doing.push(Line::styled("stopped", dim));
                    }
                    // `false`: Flow's own `doing:` header below
                    // already covers this content -- `agent_live_
                    // lines` must not print a second one.
                    doing.extend(agent_live_lines(live, transcript_view, ctx, false));
                }
                None if running => doing.push(Line::styled(*note, dim)),
                None => {}
            }
        }
        _ => {}
    }
    if !doing.is_empty() {
        lines.push(Line::styled("doing:", dim));
        lines.extend(doing);
    }

    let mut produced = Vec::new();
    if finished {
        match &step.detail {
            FocusDetail::Prompt { .. } => {
                if let Some(received) = &step.received {
                    if let Some(snippet) = &received.snippet {
                        produced.push(Line::from(snippet.clone()));
                    }
                }
            }
            FocusDetail::Agent {
                raw_transcript,
                raw_files_changed,
                raw_turns,
                ..
            } => {
                if let Some(value) = &step.value {
                    if !value.is_empty() {
                        produced.push(Line::from(value.clone()));
                    }
                }
                // Issue #637: "produced: meta.raw.transcript,
                // files_changed, the tokens and turns" -- the tokens
                // line below is the same `step.tokens` every other
                // finished tool step's own produced block already
                // shows.
                if let Some(transcript) = raw_transcript {
                    produced.push(Line::styled(transcript.clone(), dim));
                }
                if let Some(files) = raw_files_changed {
                    produced.push(Line::from(format!("files {files}")));
                }
                if let Some(turns) = raw_turns {
                    produced.push(Line::from(format!("turns {turns}")));
                }
            }
            FocusDetail::Tool { .. } | FocusDetail::Plain => {
                if let Some(value) = &step.value {
                    if !value.is_empty() {
                        produced.push(Line::from(value.clone()));
                    }
                }
            }
        }
        if let Some(tokens) = step.tokens {
            produced.push(Line::from(format!(
                "tokens {}",
                oscilloscope_core::format::format_tokens(tokens)
            )));
        }
        if let Some(cost) = step.cost_usd {
            produced.push(Line::from(format!("${cost:.2}")));
        }
        if let Some(error) = &step.error {
            produced.push(Line::styled(
                error.clone(),
                Style::default().fg(ctx.theme.error),
            ));
        }
    } else if running {
        produced.push(Line::styled("(when done)", dim));
    }
    if !produced.is_empty() {
        lines.push(Line::styled("produced:", dim));
        lines.extend(produced);
    }

    lines
}

/// A decision row's own detail (issue #635: "a decision row shows its
/// condition, its mode, the branch it took, and the model's answer
/// for a model-mode condition").
fn decision_detail_lines(
    decision: &oscilloscope_core::screens::SelectedDecision,
    ctx: &DrawCtx,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(decision.label.clone())];
    if let Some(condition) = &decision.condition {
        let mode = match condition.mode {
            oscilloscope_core::plan::ConditionMode::Cel => "cel",
            oscilloscope_core::plan::ConditionMode::Model => "model",
        };
        lines.push(Line::styled(
            format!("{mode}  {}", condition.text),
            Style::default().fg(ctx.theme.dim),
        ));
    }
    match &decision.branch {
        Some(branch) => {
            let mut text = format!("→ {branch}");
            if decision.nothing_ran {
                text.push_str(" (nothing ran)");
            }
            if decision.inferred {
                text.push_str("  (inferred)");
            }
            lines.push(Line::from(text));
        }
        None => lines.push(Line::styled(
            "not yet evaluated",
            Style::default().fg(ctx.theme.dim),
        )),
    }
    if let Some(answer) = &decision.answer {
        lines.push(Line::from(format!("answer  {answer}")));
    }
    lines
}

/// The story pane (issue #635's right/bottom pane): the run's own
/// notable events, oldest first, following the latest line while
/// scrolled to the bottom -- `l` swaps it for the full log instead.
fn draw_flow_story_pane(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::screens::FlowScreen,
    log_lines: &[String],
    log_total: u64,
    app: &mut App,
    ctx: &DrawCtx,
) {
    if app.flow_story_shows_log {
        draw_log(frame, area, log_lines, log_total, ctx);
        return;
    }
    let height = area.height.saturating_sub(2) as usize;
    let total = screen.story.len();
    // Follows the latest line while scrolled to the bottom (DESIGN.md:
    // "the latest line is followed when the story is scrolled to the
    // bottom") -- the same bottom-biased default the log pane already
    // has, just without a selection to keep in view.
    if app.story_scroll == usize::MAX || app.story_scroll.saturating_add(height) >= total {
        app.story_scroll = total.saturating_sub(height);
    }
    app.story_viewport = height;
    let start = app.story_scroll.min(total);
    let end = (start + height).min(total);
    let items: Vec<ListItem> = screen.story[start..end]
        .iter()
        .map(|entry| {
            let style = if is_warning_report_log_line(&entry.text) {
                Style::default().fg(ctx.theme.warning)
            } else if entry.inferred {
                Style::default().fg(ctx.theme.dim)
            } else {
                Style::default()
            };
            let shown = if is_report_log_line(&entry.text) {
                truncate_label(&entry.text, area.width.saturating_sub(2) as usize)
            } else {
                entry.text.clone()
            };
            ListItem::new(Line::from(shown)).style(style)
        })
        .collect();
    let border_style = if app.pane == Pane::Story {
        Style::default().fg(ctx.theme.border_focused)
    } else {
        Style::default().fg(ctx.theme.border)
    };
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" story ")
            .border_style(border_style),
    );
    frame.render_widget(list, area);
}

/// Screen 2 (`Flow`, issue #635): the tree as control flow (left),
/// the selected step and the run's story (right). `100×30` and
/// larger splits flow | (selected over story), both 62/38; `80×20`
/// up to that drops the story (flow | selected, `l` showing the log
/// in the selected pane's own place); below `80×20`, one column
/// (selected over flow, today's order), with no story and the flow
/// pane's own columns dropped to duration alone.
#[allow(clippy::too_many_arguments)]
fn draw_flow_screen(
    frame: &mut Frame,
    frame_area: Rect,
    body: Rect,
    state: &oscilloscope_core::render::RenderState,
    screen: &oscilloscope_core::screens::FlowScreen,
    log_lines: &[String],
    log_total: u64,
    app: &mut App,
    transcript_view: Option<&oscilloscope_core::transcript::TranscriptView>,
    ctx: &DrawCtx,
) {
    let visible = visible_rows(&state.rows, app);
    let state_visible = crate::keys::visible_state_rows(&state.state_rows, app);

    let three_pane = frame_area.width >= thresholds::FLOW_THREE_PANE_MIN_WIDTH
        && frame_area.height >= thresholds::FLOW_THREE_PANE_MIN_HEIGHT;
    let two_pane = frame_area.width >= thresholds::FLOW_TWO_PANE_MIN_WIDTH
        && frame_area.height >= thresholds::FLOW_TWO_PANE_MIN_HEIGHT;

    if three_pane {
        let left_width = oscilloscope_core::layout::left_column_width(body.width);
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(left_width),
                Constraint::Length(body.width - left_width),
            ])
            .split(body);
        draw_flow_pane(
            frame,
            columns[0],
            screen,
            &visible,
            app,
            &state.header,
            frame_area.width,
            false,
            ctx,
        );
        let selected_rows = oscilloscope_core::layout::golden_section(columns[1].height);
        let right = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(selected_rows),
                Constraint::Min(columns[1].height - selected_rows),
            ])
            .split(columns[1]);
        draw_flow_selected_pane(
            frame,
            right[0],
            screen,
            &state_visible,
            app,
            transcript_view,
            ctx,
        );
        draw_flow_story_pane(frame, right[1], screen, log_lines, log_total, app, ctx);
    } else if two_pane {
        app.story_viewport = 0;
        let left_width = oscilloscope_core::layout::left_column_width(body.width);
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(left_width),
                Constraint::Length(body.width - left_width),
            ])
            .split(body);
        draw_flow_pane(
            frame,
            columns[0],
            screen,
            &visible,
            app,
            &state.header,
            frame_area.width,
            false,
            ctx,
        );
        if app.flow_story_shows_log {
            draw_log(frame, columns[1], log_lines, log_total, ctx);
        } else {
            draw_flow_selected_pane(
                frame,
                columns[1],
                screen,
                &state_visible,
                app,
                transcript_view,
                ctx,
            );
        }
    } else {
        app.story_viewport = 0;
        let selected_rows = oscilloscope_core::layout::golden_section(body.height);
        let parts = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(selected_rows),
                Constraint::Min(body.height - selected_rows),
            ])
            .split(body);
        draw_flow_selected_pane(
            frame,
            parts[0],
            screen,
            &state_visible,
            app,
            transcript_view,
            ctx,
        );
        draw_flow_pane(
            frame,
            parts[1],
            screen,
            &visible,
            app,
            &state.header,
            frame_area.width,
            true,
            ctx,
        );
    }
}

// ---------------------------------------------------------------------
// The Time screen (issue #650)
// ---------------------------------------------------------------------

/// The timeline's and the strips' own shared layout, computed once
/// per frame so both panes bucket onto -- and draw -- the exact same
/// columns (DESIGN.md: the strips are "on the same axis" as the
/// bars): the gutter (label + start/duration column) width, the bar
/// width itself, and the visible window `+`/`-`/`=` currently zoom
/// to. Every strip buckets against this same bar width and window,
/// rather than its own fixed resolution, so none of them can ever
/// run past the now line the bars themselves line up with.
struct TimeLayout {
    label_width: usize,
    show_columns: bool,
    bar_width: usize,
    vis_start_s: f64,
    vis_end_s: f64,
}

fn time_layout(
    screen: &oscilloscope_core::screens::TimeScreen,
    app: &App,
    area_width: u16,
    frame_width: u16,
) -> TimeLayout {
    let show_columns = frame_width >= thresholds::TIME_START_DURATION_COLUMN_MIN_WIDTH;
    let label_width = 22usize.min((area_width as usize).saturating_sub(10));
    // A row reads `label_width` + 1 (the gutter space) + the bar,
    // plus 19 more (`"  " + 9 + 7` + a trailing space before the
    // border) when the start/duration columns are shown -- all
    // inside the box's own inner width, 2 columns narrower than
    // `area_width` for its left/right border.
    let column_width = if show_columns { 19 } else { 0 };
    let bar_width = (area_width as usize)
        .saturating_sub(label_width + 1 + column_width + 2)
        .max(4);
    let run_start_ts = screen.axis.start_ts.as_deref();
    let full_end_s = run_start_ts
        .and_then(|s| oscilloscope_core::observe::duration_seconds(s, &screen.axis.end_ts))
        .unwrap_or(0.0);
    let (vis_start_s, vis_end_s) = time_visible_window(screen, app, full_end_s);
    TimeLayout {
        label_width,
        show_columns,
        bar_width,
        vis_start_s,
        vis_end_s,
    }
}

/// `run_start_ts` plus `offset_s` (run-relative seconds), formatted
/// in the local zone -- `None` only when `run_start_ts` itself fails
/// to parse (never true of a real run's own start, which
/// `oscilloscope_core::diff::parse_instant` already produced once).
fn offset_clock_label(
    run_start_ts: &str,
    offset_s: f64,
    zone: &dyn oscilloscope_core::format::ClockZone,
    fmt: &str,
) -> Option<String> {
    let dt = oscilloscope_core::diff::parse_instant(run_start_ts)?
        + chrono::Duration::milliseconds((offset_s * 1000.0) as i64);
    Some(
        zone.local(dt.with_timezone(&chrono::Utc))
            .format(fmt)
            .to_string(),
    )
}

/// Writes `┃` (DESIGN.md's now line) into `bar` at `now_s`'s own
/// column -- a running row's own head (`▶`) is one cell to the
/// left of it instead of under it, so the two glyphs stay visible
/// side by side rather than the now line simply overwriting the head
/// (the mockup's own `█▶┃`).
fn overlay_now_line(bar: &mut String, now_s: Option<f64>, layout: &TimeLayout, running: bool) {
    let Some(now_s) = now_s else { return };
    let Some(col) = oscilloscope_core::time::now_column(
        layout.vis_start_s,
        layout.vis_end_s,
        layout.bar_width,
        now_s,
    ) else {
        return;
    };
    let mut chars: Vec<char> = bar.chars().collect();
    if col >= chars.len() {
        return;
    }
    if running && chars[col] == '▶' && col > 0 {
        chars[col - 1] = '▶';
    }
    chars[col] = '┃';
    *bar = chars.into_iter().collect();
}

/// One row the timeline actually draws: a real folded row, one of an
/// expanded loop's own passes (with its own indent depth, since a
/// pass sits one level deeper than its loop row), or the timeline's
/// own trailing `planned` ghost row (DESIGN.md: "The last row,
/// `planned`, runs from now to the ETA").
enum TimeDrawRow<'a> {
    Row(&'a oscilloscope_core::time::TimeRow),
    Pass(&'a oscilloscope_core::screens::TimeLoopPassRow, usize),
    Planned(oscilloscope_core::time::GhostBar),
}

/// The timeline's own drawable rows, in display order -- `visible`
/// already has collapsed/filtered/under-a-loop rows dropped
/// (`draw_time_timeline`'s own job, same rule `draw_flow_pane` builds
/// its own `drawable` list under); this splices each expanded loop's
/// own passes in right after it, the one thing `screen.rows` already
/// orders correctly but `visible` (built from the base tree, which
/// never shows a pass row at all) can't.
fn time_drawable_rows<'a>(
    visible: &[&Row],
    by_path: &std::collections::BTreeMap<String, &'a oscilloscope_core::time::TimeRow>,
    passes: &std::collections::BTreeMap<
        String,
        Vec<&'a oscilloscope_core::screens::TimeLoopPassRow>,
    >,
    planned: Option<oscilloscope_core::time::GhostBar>,
    app: &App,
) -> Vec<TimeDrawRow<'a>> {
    let mut out = Vec::new();
    for row in visible {
        let Some(time_row) = by_path.get(row.path.as_str()) else {
            continue;
        };
        out.push(TimeDrawRow::Row(time_row));
        if time_row.loop_expandable && app.time_expanded_loops.contains(&row.path) {
            if let Some(rows) = passes.get(&row.path) {
                for pass in rows {
                    out.push(TimeDrawRow::Pass(pass, time_row.depth + 1));
                }
            }
        }
    }
    if let Some(ghost) = planned {
        out.push(TimeDrawRow::Planned(ghost));
    }
    out
}

fn time_height_glyph(h: u64, max: u64) -> char {
    const GLYPHS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if max == 0 || h == 0 {
        return ' ';
    }
    let level = ((h as f64 / max as f64) * 7.0).round() as usize;
    GLYPHS[level.min(7)]
}

/// One row's own bar, drawn onto exactly `width` columns of
/// `[vis_start_s, vis_end_s]` -- `▁`-less partial blocks
/// (`oscilloscope_core::time::bar_cells`/`partial_block`), a `▶`
/// head in place of the bar's own end while `running`, and a ghost
/// (`░`) for a pending step with one (DESIGN.md's own glyphs).
#[allow(clippy::too_many_arguments)]
fn time_bar_string(
    start_s: Option<f64>,
    duration_s: Option<f64>,
    running: bool,
    ghost: Option<oscilloscope_core::time::GhostBar>,
    failed: bool,
    vis_start_s: f64,
    vis_end_s: f64,
    width: usize,
) -> String {
    let mut out = vec![' '; width];
    if let Some(ghost) = ghost {
        if ghost.end_s >= vis_start_s && ghost.start_s <= vis_end_s {
            let cells = oscilloscope_core::time::bar_cells(
                vis_start_s,
                vis_end_s,
                width,
                ghost.start_s,
                ghost.end_s,
            );
            for slot in out
                .iter_mut()
                .take((cells.start_col + cells.full_cells).min(width))
                .skip(cells.start_col)
            {
                *slot = '░';
            }
        }
    }
    let Some(start_s) = start_s else {
        return out.into_iter().collect();
    };
    let end_s = start_s + duration_s.unwrap_or(0.0);
    if end_s < vis_start_s || start_s > vis_end_s {
        return out.into_iter().collect();
    }
    let cells = oscilloscope_core::time::bar_cells(vis_start_s, vis_end_s, width, start_s, end_s);
    for slot in out
        .iter_mut()
        .take((cells.start_col + cells.full_cells).min(width))
        .skip(cells.start_col)
    {
        *slot = '█';
    }
    let mut end_idx = cells.start_col + cells.full_cells;
    if let Some(partial) = cells.partial_end {
        if end_idx < width {
            out[end_idx] = partial;
        }
    }
    if running {
        end_idx = end_idx.min(width.saturating_sub(1));
        out[end_idx] = '▶';
    } else if failed && end_idx > 0 {
        out[(end_idx - 1).min(width - 1)] = '✗';
    }
    out.into_iter().collect()
}

/// A zoom-pane band row's own line (DESIGN.md: "one band per kind of
/// tool ... from the transcript's `tool_start`/`tool_end` times"):
/// unlike [`time_bar_string`], a kind can have more than one segment
/// on the same row (an agent calls the same kind of tool more than
/// once in a session), so each of `segments` (`start_s, end_s,
/// running`) is drawn in turn, the last one still running getting
/// its own `▶` head.
fn multi_segment_bar(
    segments: &[(f64, f64, bool)],
    axis_start_s: f64,
    axis_end_s: f64,
    width: usize,
    fill: char,
) -> String {
    let mut out = vec![' '; width];
    for &(start, end, running) in segments {
        if end < axis_start_s || start > axis_end_s {
            continue;
        }
        let cells = oscilloscope_core::time::bar_cells(axis_start_s, axis_end_s, width, start, end);
        for slot in out
            .iter_mut()
            .take((cells.start_col + cells.full_cells).min(width))
            .skip(cells.start_col)
        {
            *slot = fill;
        }
        let mut end_idx = cells.start_col + cells.full_cells;
        if let Some(partial) = cells.partial_end {
            if end_idx < width {
                // `partial_block` reads a near-full fraction as a
                // solid `'█'` regardless of the band it's filling --
                // right for a single-fill status bar, but a stray
                // bash-looking glyph inside a `'▓'`/`'▒'` band here.
                // This band's own fill reads as "nearly full" just as
                // well.
                out[end_idx] = if partial == '█' { fill } else { partial };
            }
        }
        if running {
            end_idx = end_idx.min(width.saturating_sub(1));
            out[end_idx] = '▶';
        }
    }
    out.into_iter().collect()
}

#[cfg(test)]
mod time_bar_tests {
    use super::*;

    /// A tick label too close to the bar's own right edge to fit in
    /// full is left out entirely -- never cut to a stray, unreadable
    /// fragment -- while every other tick still shows in full and
    /// the rule row's own tick marks land on every column regardless.
    #[test]
    fn tick_axis_rows_never_cuts_a_label_that_would_overflow() {
        let ticks = vec![
            (0usize, 0.0, "00:00".to_string()),
            (10usize, 600.0, "00:10".to_string()),
            (18usize, 1080.0, "00:18".to_string()),
        ];
        let (labels, rule) = tick_axis_rows(20, &ticks);
        let text: String = labels.iter().collect();
        assert!(text.starts_with("00:00"), "{text:?}");
        assert!(text.contains("00:10"), "{text:?}");
        assert!(
            !text.contains("00:18"),
            "the last tick's label runs past column 20 and must not appear at all, cut or not: {text:?}"
        );
        assert_eq!(
            &labels[18..20],
            [' ', ' '],
            "left blank rather than cut: {labels:?}"
        );
        assert_eq!(rule[0], '├', "{rule:?}");
        assert_eq!(
            rule[18], '┼',
            "a tick mark lands on its column even with no label: {rule:?}"
        );
    }

    #[test]
    fn decisions_line_places_a_mark_by_its_own_fraction_of_the_window() {
        // A decision at 300s into a 0..600s/10-column window belongs
        // at column 5 (300/600 of the way across) -- raw seconds
        // divided by a column *count* (`d / width`) would instead put
        // it at column 30, clamped to the last one.
        let line = decisions_line(&[300.0], 0.0, 600.0, 10);
        let col = line.chars().position(|c| c == '◆');
        assert_eq!(col, Some(5));
    }

    #[test]
    fn zoom_container_budget_reserves_a_row_for_plus_n_more_once_it_overflows() {
        assert_eq!(
            zoom_container_budget(3, 5),
            5,
            "everything fits, no +N more row needed"
        );
        assert_eq!(
            zoom_container_budget(5, 3),
            2,
            "one row reserved for +N more"
        );
    }

    #[test]
    fn a_running_bars_own_head_is_drawn() {
        // 0..5s over a 0..10s/10-column window: columns 0..4 fill,
        // the head lands in column 5 (`bar_cells`' own `end_idx`).
        let bar = time_bar_string(Some(0.0), Some(5.0), true, None, false, 0.0, 10.0, 10);
        assert_eq!(bar.chars().nth(5), Some('▶'));
    }

    #[test]
    fn a_failed_bars_own_end_is_drawn() {
        let bar = time_bar_string(Some(0.0), Some(5.0), false, None, true, 0.0, 10.0, 10);
        assert_eq!(bar.chars().nth(4), Some('✗'));
    }

    #[test]
    fn a_ghost_bar_draws_without_a_real_start() {
        let ghost = oscilloscope_core::time::GhostBar {
            start_s: 5.0,
            end_s: 8.0,
        };
        let bar = time_bar_string(None, None, false, Some(ghost), false, 0.0, 10.0, 10);
        let chars: Vec<char> = bar.chars().collect();
        assert_eq!(&chars[5..8], ['░', '░', '░']);
    }

    #[test]
    fn overlay_now_line_puts_a_running_heads_own_bar_one_cell_before_the_now_line() {
        let layout = TimeLayout {
            label_width: 0,
            show_columns: false,
            bar_width: 10,
            vis_start_s: 0.0,
            vis_end_s: 10.0,
        };
        let mut bar = time_bar_string(Some(0.0), Some(5.0), true, None, false, 0.0, 10.0, 10);
        assert_eq!(bar.chars().nth(5), Some('▶'), "the head before any overlay");
        overlay_now_line(&mut bar, Some(5.0), &layout, true);
        let chars: Vec<char> = bar.chars().collect();
        assert_eq!(
            chars[4], '▶',
            "the head moved one cell left of the now line"
        );
        assert_eq!(chars[5], '┃', "the now line itself stays visible");
    }

    #[test]
    fn overlay_now_line_on_a_finished_row_just_overlays_the_now_line() {
        let layout = TimeLayout {
            label_width: 0,
            show_columns: false,
            bar_width: 10,
            vis_start_s: 0.0,
            vis_end_s: 10.0,
        };
        let mut bar = time_bar_string(Some(0.0), Some(8.0), false, None, false, 0.0, 10.0, 10);
        overlay_now_line(&mut bar, Some(4.0), &layout, false);
        assert_eq!(bar.chars().nth(4), Some('┃'));
    }

    #[test]
    fn multi_segment_bar_draws_more_than_one_segment_on_one_row() {
        let segments = [(0.0, 2.0, false), (5.0, 8.0, true)];
        let bar = multi_segment_bar(&segments, 0.0, 10.0, 10, '█');
        let chars: Vec<char> = bar.chars().collect();
        assert_eq!(&chars[0..2], ['█', '█']);
        assert_eq!(chars[8], '▶', "the running segment's own head");
    }

    /// A near-full end cell reads as a solid `'█'` from `partial_block`
    /// alone, which looks like a `bash` band dropped into the middle
    /// of any other one -- this band's own fill must show instead.
    #[test]
    fn multi_segment_bar_uses_its_own_fill_for_a_near_full_end_cell() {
        let segments = [(0.0, 1.99, false)];
        let bar = multi_segment_bar(&segments, 0.0, 10.0, 10, '▓');
        let chars: Vec<char> = bar.chars().collect();
        assert_eq!(&chars[0..2], ['▓', '▓'], "{bar:?}");
    }

    fn test_time_row(
        start_s: Option<f64>,
        duration_s: Option<f64>,
        status_kind: StatusKind,
    ) -> oscilloscope_core::time::TimeRow {
        oscilloscope_core::time::TimeRow {
            path: "prime.a".to_string(),
            depth: 1,
            label: "a".to_string(),
            kind: oscilloscope_core::render::RowKind::Tool,
            status: oscilloscope_core::model::RowStatus {
                kind: status_kind,
                skip_reason: None,
                retrying: false,
            },
            start_s,
            duration_s,
            loop_progress: None,
            has_children: false,
            loop_expandable: false,
            ghost: None,
        }
    }

    fn test_draw_ctx<'a>(
        theme: &'a crate::theme::Theme,
        tracker: &'a crate::highlight::Tracker,
        zone: &'a chrono::FixedOffset,
    ) -> DrawCtx<'a> {
        DrawCtx {
            theme,
            highlights: tracker,
            now: std::time::Instant::now(),
            color_mode: ColorMode::TrueColor,
            theme_banner: None,
            zone,
            no_animation: false,
        }
    }

    /// A running step's own time line always reads `→ now`, growing
    /// with `now_s` itself, not a stored duration.
    #[test]
    fn zoom_axis_line_a_running_step_reads_arrow_now() {
        let theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let tracker = crate::highlight::Tracker::new();
        let zone = test_zone();
        let ctx = test_draw_ctx(&theme, &tracker, &zone);
        let row = test_time_row(Some(10.0), None, StatusKind::Running);
        let line = zoom_axis_line(&row, "2026-01-01T00:00:00Z", 70.0, &ctx).unwrap();
        assert!(line.contains("\u{2192} now"), "{line:?}");
        assert!(line.contains("1m00s"), "{line:?}");
    }

    /// Both `…` and `›` are multi-byte in UTF-8 -- cutting by byte
    /// index instead of char index would panic or split one in half.
    /// A title long enough that it actually needs cutting (not merely
    /// short enough to pass through unchanged) exercises that path.
    #[test]
    fn truncate_title_from_left_cuts_a_multi_byte_title() {
        let title = "fan › pass 1 of 8 › leaf";
        assert!(title.chars().count() > 12);
        let cut = truncate_title_from_left(title, 12);
        assert_eq!(cut.chars().count(), 12, "{cut:?}");
        assert!(cut.starts_with('…'), "{cut:?}");
        assert!(cut.ends_with("leaf"), "{cut:?}");
    }

    /// A cancelled step with no real duration of its own must not
    /// keep growing with how long ago `now_s` is -- its own time line
    /// reads the same regardless of how far in the future `now_s` is.
    #[test]
    fn zoom_axis_line_a_cancelled_step_does_not_grow_with_now() {
        let theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let tracker = crate::highlight::Tracker::new();
        let zone = test_zone();
        let ctx = test_draw_ctx(&theme, &tracker, &zone);
        let row = test_time_row(Some(10.0), None, StatusKind::Cancelled);
        let soon = zoom_axis_line(&row, "2026-01-01T00:00:00Z", 15.0, &ctx).unwrap();
        let much_later = zoom_axis_line(&row, "2026-01-01T00:00:00Z", 10_000.0, &ctx).unwrap();
        assert_eq!(soon, much_later, "{soon:?} vs {much_later:?}");
        assert!(!soon.contains("now"), "{soon:?}");
    }
}

/// The decisions strip's own line (DESIGN.md: "a `◆` at the time each
/// named `if` decided") -- each of `decisions` (run-relative
/// seconds) bucketed by [`oscilloscope_core::time::bucket_weighted`],
/// the same window and width every other strip and the bars
/// themselves use, so a mark lands under its own real time rather
/// than at whatever column its own raw seconds happen to divide into.
fn decisions_line(decisions: &[f64], vis_start_s: f64, vis_end_s: f64, width: usize) -> String {
    let points: Vec<(f64, u64)> = decisions.iter().map(|&d| (d, 1)).collect();
    let heights = oscilloscope_core::time::bucket_weighted(vis_start_s, vis_end_s, width, &points);
    heights
        .iter()
        .map(|h| if *h > 0 { '◆' } else { ' ' })
        .collect()
}

/// How many of a container's own children the zoom pane actually
/// draws (DESIGN.md's "`+N more` when they don't fit") -- every one
/// of them when they all fit in `viewport`, one row short of it
/// otherwise, leaving room for the `+N more` line itself.
fn zoom_container_budget(total: usize, viewport: usize) -> usize {
    if total > viewport {
        viewport.saturating_sub(1)
    } else {
        viewport
    }
}

/// The visible window (issue #650's `+`/`-`/`=`): the whole axis by
/// default, narrowed by `app.time_zoom` around now (while following)
/// or around the selected row's own bar (otherwise), clamped back
/// inside `[0, full_end_s]`.
fn time_visible_window(
    screen: &oscilloscope_core::screens::TimeScreen,
    app: &App,
    full_end_s: f64,
) -> (f64, f64) {
    let Some(fraction) = app.time_zoom else {
        return (0.0, full_end_s);
    };
    let window = (full_end_s * fraction).max(1.0);
    let center = if app.follow {
        screen.now_s.unwrap_or(full_end_s)
    } else {
        app.selected_path
            .as_deref()
            .and_then(|path| {
                screen.rows.iter().find_map(|r| match r {
                    oscilloscope_core::screens::TimeDisplayRow::Row(row) if row.path == path => {
                        row.start_s.map(|s| s + row.duration_s.unwrap_or(0.0) / 2.0)
                    }
                    _ => None,
                })
            })
            .unwrap_or(full_end_s / 2.0)
    };
    let half = window / 2.0;
    let mut start = center - half;
    let mut end = center + half;
    if start < 0.0 {
        end += -start;
        start = 0.0;
    }
    if end > full_end_s {
        start -= end - full_end_s;
        end = full_end_s;
        start = start.max(0.0);
    }
    (start, end)
}

/// Splits `screen.rows` into its own loop rows (by path) and each
/// loop's own pass rows, grouped by the loop row they immediately
/// follow -- `draw_time_timeline` interleaves a loop's own passes
/// right after it, only when that loop's own path is both visible
/// and in `app.time_expanded_loops`.
fn time_row_groups(
    rows: &[oscilloscope_core::screens::TimeDisplayRow],
) -> (
    std::collections::BTreeMap<String, &oscilloscope_core::time::TimeRow>,
    std::collections::BTreeMap<String, Vec<&oscilloscope_core::screens::TimeLoopPassRow>>,
) {
    let mut by_path = std::collections::BTreeMap::new();
    let mut passes: std::collections::BTreeMap<
        String,
        Vec<&oscilloscope_core::screens::TimeLoopPassRow>,
    > = std::collections::BTreeMap::new();
    let mut current_loop: Option<String> = None;
    for row in rows {
        match row {
            oscilloscope_core::screens::TimeDisplayRow::Row(r) => {
                by_path.insert(r.path.clone(), r);
                current_loop = Some(r.path.clone());
            }
            oscilloscope_core::screens::TimeDisplayRow::Pass(p) => {
                if let Some(path) = &current_loop {
                    passes.entry(path.clone()).or_default().push(p);
                }
            }
        }
    }
    (by_path, passes)
}

/// The innermost loop row `path` sits under, if any (same walk
/// [`time_zoom_ancestor_loop_label`] does for the zoom title) -- a
/// leaf under a *collapsed* loop never gets a drawn row of its own
/// at all (its path is always filtered out of the timeline's own
/// `visible`/`drawable` rows, expanded or not -- expanded shows
/// separate pass rows instead), so follow selecting it must
/// highlight and scroll to this loop row instead, the nearest one
/// actually on screen.
fn nearest_ancestor_loop_path<'a>(
    by_path: &std::collections::BTreeMap<String, &'a oscilloscope_core::time::TimeRow>,
    path: &str,
) -> Option<&'a str> {
    by_path
        .values()
        .filter(|r| {
            r.kind == oscilloscope_core::render::RowKind::Loop
                && path.starts_with(&format!("{}.", r.path))
        })
        .max_by_key(|r| r.path.len())
        .map(|r| r.path.as_str())
}

/// The clock axis's own tick-label and rule rows (DESIGN.md: a
/// labelled tick row plus a `├─┤` rule), built from
/// [`oscilloscope_core::time::axis_tick_columns`]'s own output -- a
/// label that would run past `bar_width` is left out entirely rather
/// than cut to a stray, unreadable fragment; the rule row's own tick
/// mark still lands on its column either way.
fn tick_axis_rows(bar_width: usize, ticks: &[(usize, f64, String)]) -> (Vec<char>, Vec<char>) {
    let mut tick_label_row = vec![' '; bar_width];
    let mut tick_rule_row = vec!['─'; bar_width];
    for (col, _offset_s, label) in ticks {
        let col = *col;
        tick_rule_row[col] = if col == 0 {
            '├'
        } else if col == bar_width.saturating_sub(1) {
            '┤'
        } else {
            '┼'
        };
        if col + label.chars().count() <= tick_label_row.len() {
            for (i, ch) in label.chars().enumerate() {
                tick_label_row[col + i] = ch;
            }
        }
    }
    (tick_label_row, tick_rule_row)
}

#[allow(clippy::too_many_arguments)]
fn draw_time_timeline(
    frame: &mut Frame,
    area: Rect,
    state: &oscilloscope_core::render::RenderState,
    screen: &oscilloscope_core::screens::TimeScreen,
    layout: &TimeLayout,
    app: &mut App,
    ctx: &DrawCtx,
) {
    let all_visible = visible_rows(&state.rows, app);
    let (by_path, passes) = time_row_groups(&screen.rows);
    // DESIGN.md: "Loops are collapsed by default: one row" -- a
    // loop's own passes never appear as the base tree's own rows
    // here (breadcrumbed or not), only as this screen's own
    // `expand_loop` rows, spliced in below exactly when the loop
    // itself is in `app.time_expanded_loops`.
    // The trailing `.` is appended once per loop row here, not once
    // per (row × loop row) pair inside the filter below -- a `format!`
    // there cost `O(rows × loop rows)` allocations on every redraw.
    let loop_prefixes: Vec<String> = by_path
        .values()
        .filter(|r| r.kind == oscilloscope_core::render::RowKind::Loop)
        .map(|r| format!("{}.", r.path))
        .collect();
    let is_under_a_loop = |path: &str| loop_prefixes.iter().any(|p| path.starts_with(p.as_str()));
    let visible: Vec<&Row> = all_visible
        .into_iter()
        .filter(|r| !is_under_a_loop(&r.path))
        .collect();
    let drawable = time_drawable_rows(&visible, &by_path, &passes, screen.planned, app);
    let run_start_ts = screen.axis.start_ts.as_deref();

    // Follow can select a leaf hidden under a collapsed loop (its
    // own row never drawn at all) -- the nearest ancestor row that
    // *is* drawn stands in for it, both for the highlight below and
    // for scrolling it into view.
    let effective_selected_path: Option<&str> = app.selected_path.as_deref().map(|p| {
        if is_under_a_loop(p) {
            nearest_ancestor_loop_path(&by_path, p).unwrap_or(p)
        } else {
            p
        }
    });
    let selected_idx = effective_selected_path.and_then(|p| {
        drawable
            .iter()
            .position(|r| matches!(r, TimeDrawRow::Row(tr) if tr.path == p))
    });
    // The clock axis's own tick-label and rule rows (DESIGN.md: a
    // labelled tick row plus a `├─┤` rule) sit above the bars, so the
    // viewport the bars themselves scroll through is two rows
    // shorter than the pane's own interior height.
    let axis_rows = 2usize;
    let viewport = (area.height as usize).saturating_sub(2 + axis_rows);
    app.tree_viewport = viewport;
    let line_counts = vec![1usize; drawable.len()];
    app.tree_scroll = clamp_state_scroll(app.tree_scroll, selected_idx, &line_counts, viewport);

    let gutter = " ".repeat(layout.label_width + 1);
    let (tick_label_row, mut tick_rule_row) = run_start_ts.map_or_else(
        || (vec![' '; layout.bar_width], vec!['─'; layout.bar_width]),
        |rs| {
            let ticks = oscilloscope_core::time::axis_tick_columns(
                rs,
                layout.vis_start_s,
                layout.vis_end_s,
                layout.bar_width,
                ctx.zone,
            );
            tick_axis_rows(layout.bar_width, &ticks)
        },
    );
    if let Some(now_s) = screen.now_s {
        if let Some(col) = oscilloscope_core::time::now_column(
            layout.vis_start_s,
            layout.vis_end_s,
            layout.bar_width,
            now_s,
        ) {
            tick_rule_row[col] = '╂';
        }
    }
    let mut lines: Vec<Line<'static>> = vec![
        Line::styled(
            format!("{gutter}{}", tick_label_row.into_iter().collect::<String>()),
            Style::default().fg(ctx.theme.dim),
        ),
        Line::styled(
            format!("{gutter}{}", tick_rule_row.into_iter().collect::<String>()),
            Style::default().fg(ctx.theme.dim),
        ),
    ];

    for item in drawable.iter().skip(app.tree_scroll).take(viewport) {
        match item {
            TimeDrawRow::Row(time_row) => {
                let selected = effective_selected_path == Some(time_row.path.as_str());
                let indent = "  ".repeat(time_row.depth);
                let glyph = status_glyph(time_row.status.kind);
                let mut label = format!("{indent}{glyph} {}", time_row.label);
                if let Some(progress) = &time_row.loop_progress {
                    let total = progress
                        .total
                        .map(|t| t.to_string())
                        .unwrap_or_else(|| "?".to_string());
                    label.push_str(&format!(" ⟲ {}/{total}", progress.done));
                }
                let label = truncate_label(&label, layout.label_width);
                let mut bar = time_bar_string(
                    time_row.start_s,
                    time_row.duration_s,
                    time_row.status.kind == StatusKind::Running,
                    time_row.ghost,
                    time_row.status.kind == StatusKind::Failed,
                    layout.vis_start_s,
                    layout.vis_end_s,
                    layout.bar_width,
                );
                overlay_now_line(
                    &mut bar,
                    screen.now_s,
                    layout,
                    time_row.status.kind == StatusKind::Running,
                );
                let label_width = layout.label_width;
                let mut spans = vec![
                    Span::styled(
                        format!("{label:<label_width$} "),
                        status_style(time_row.status.kind, ctx.theme),
                    ),
                    Span::styled(
                        bar,
                        Style::default().fg(status_color(time_row.status.kind, ctx.theme)),
                    ),
                ];
                if layout.show_columns {
                    let start_text = time_row
                        .start_s
                        .zip(run_start_ts)
                        .and_then(|(s, start_ts)| {
                            offset_clock_label(start_ts, s, ctx.zone, "%H:%M:%S")
                        })
                        .unwrap_or_default();
                    let duration_text = time_row
                        .duration_s
                        .map(oscilloscope_core::format::format_duration)
                        .unwrap_or_default();
                    spans.push(Span::styled(
                        format!("  {start_text:<9}{duration_text:>7} "),
                        Style::default().fg(ctx.theme.dim),
                    ));
                }
                let style = if selected {
                    Style::default().add_modifier(Modifier::REVERSED)
                } else {
                    Style::default()
                };
                lines.push(Line::styled("", style).spans(spans).style(style));
            }
            TimeDrawRow::Pass(pass, depth) => {
                let pass_indent = "  ".repeat(*depth);
                let pass_glyph = status_glyph(pass.status.kind);
                let label_width = layout.label_width;
                let pass_label = truncate_label(
                    &format!("{pass_indent}{pass_glyph} {}", pass.label),
                    label_width,
                );
                let mut pass_bar = time_bar_string(
                    pass.start_s,
                    pass.duration_s,
                    pass.status.kind == StatusKind::Running,
                    None,
                    pass.status.kind == StatusKind::Failed,
                    layout.vis_start_s,
                    layout.vis_end_s,
                    layout.bar_width,
                );
                overlay_now_line(
                    &mut pass_bar,
                    screen.now_s,
                    layout,
                    pass.status.kind == StatusKind::Running,
                );
                let mut pass_spans = vec![
                    Span::styled(
                        format!("{pass_label:<label_width$} "),
                        status_style(pass.status.kind, ctx.theme),
                    ),
                    Span::styled(
                        pass_bar,
                        Style::default().fg(status_color(pass.status.kind, ctx.theme)),
                    ),
                ];
                if layout.show_columns {
                    let start_text = pass
                        .start_s
                        .zip(run_start_ts)
                        .and_then(|(s, start_ts)| {
                            offset_clock_label(start_ts, s, ctx.zone, "%H:%M:%S")
                        })
                        .unwrap_or_default();
                    let duration_text = pass
                        .duration_s
                        .map(oscilloscope_core::format::format_duration)
                        .unwrap_or_default();
                    pass_spans.push(Span::styled(
                        format!("  {start_text:<9}{duration_text:>7} "),
                        Style::default().fg(ctx.theme.dim),
                    ));
                }
                lines.push(Line::from(pass_spans));
            }
            TimeDrawRow::Planned(ghost) => {
                let label_width = layout.label_width;
                let label = truncate_label("planned", label_width);
                let mut bar = vec![' '; layout.bar_width];
                let cells = oscilloscope_core::time::bar_cells(
                    layout.vis_start_s,
                    layout.vis_end_s,
                    layout.bar_width,
                    ghost.start_s,
                    ghost.end_s,
                );
                for slot in bar
                    .iter_mut()
                    .take((cells.start_col + cells.full_cells).min(layout.bar_width))
                    .skip(cells.start_col)
                {
                    *slot = '░';
                }
                let mut bar_str: String = bar.into_iter().collect();
                overlay_now_line(&mut bar_str, screen.now_s, layout, false);
                let mut spans = vec![
                    Span::styled(
                        format!("{label:<label_width$} "),
                        Style::default().fg(ctx.theme.dim),
                    ),
                    Span::styled(bar_str, Style::default().fg(ctx.theme.dim)),
                ];
                if layout.show_columns {
                    let eta_text = run_start_ts
                        .and_then(|rs| offset_clock_label(rs, ghost.end_s, ctx.zone, "%H:%M"))
                        .map(|t| format!("~{t}"))
                        .unwrap_or_default();
                    let dur_text = format!(
                        "~{}",
                        oscilloscope_core::format::format_duration(ghost.end_s - ghost.start_s)
                    );
                    spans.push(Span::styled(
                        format!("  {eta_text:<9}{dur_text:>7} "),
                        Style::default().fg(ctx.theme.dim),
                    ));
                }
                lines.push(Line::from(spans));
            }
        }
    }
    let title = scroll_indicator(selected_idx, drawable.len(), viewport)
        .map(|s| format!(" timeline{s}"))
        .unwrap_or_else(|| " timeline ".to_string());
    frame.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}

fn draw_time_strips(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::screens::TimeScreen,
    layout: &TimeLayout,
) {
    let width = layout.bar_width;
    let events_heights = oscilloscope_core::time::bucket_weighted(
        layout.vis_start_s,
        layout.vis_end_s,
        width,
        &screen.strips.events_points,
    );
    let events_max = events_heights.iter().copied().max().unwrap_or(0);
    let mut events_line: String = events_heights
        .iter()
        .map(|h| time_height_glyph(*h, events_max))
        .collect();
    let events_label = if screen.strips.events_from_state {
        "events/min (state)"
    } else {
        "events/min"
    };

    let in_flight_heights = oscilloscope_core::time::in_flight_heights(
        layout.vis_start_s,
        layout.vis_end_s,
        width,
        &screen.strips.in_flight_intervals,
    );
    let in_flight_max = in_flight_heights.iter().copied().max().unwrap_or(0);
    let mut in_flight_line: String = in_flight_heights
        .iter()
        .map(|h| time_height_glyph(*h, in_flight_max))
        .collect();

    let mut decisions_line = decisions_line(
        &screen.strips.decisions,
        layout.vis_start_s,
        layout.vis_end_s,
        width,
    );

    // DESIGN.md: the now line runs "through every row and strip".
    for line in [&mut events_line, &mut decisions_line, &mut in_flight_line] {
        if let Some(now_s) = screen.now_s {
            if let Some(col) = oscilloscope_core::time::now_column(
                layout.vis_start_s,
                layout.vis_end_s,
                layout.bar_width,
                now_s,
            ) {
                let mut chars: Vec<char> = line.chars().collect();
                if col < chars.len() {
                    chars[col] = '┃';
                }
                *line = chars.into_iter().collect();
            }
        }
    }

    // A row's own bar starts at `label_width + 2`: the timeline box's
    // left border (1), then its label padded to `label_width`, then
    // one space. This pane has no border of its own, so its gutter
    // needs that same two-space width between the label and the bar,
    // not one, to actually land on the same column as the bars
    // themselves. Each label is also truncated to `label_width`, not
    // just padded -- `{:<width$}` never cuts a longer string down, so
    // a label that overflowed would shift everything after it right
    // by however much it overran.
    let label_width = layout.label_width;
    let events_label = truncate_strip_label(events_label, label_width);
    let decisions_label = truncate_strip_label("decisions", label_width);
    let in_flight_label = truncate_strip_label("in flight", label_width);
    let lines = if layout.show_columns {
        let in_flight_suffix = if screen.strips.queued > 0 {
            format!("max {in_flight_max} · {} queued", screen.strips.queued)
        } else {
            format!("max {in_flight_max}")
        };
        let events_suffix = if screen.strips.quiet_agent {
            "quiet = agent"
        } else {
            ""
        };
        vec![
            Line::from(format!(
                "{events_label:<label_width$}  {events_line}  {events_suffix}"
            )),
            Line::from(format!("{decisions_label:<label_width$}  {decisions_line}")),
            Line::from(format!(
                "{in_flight_label:<label_width$}  {in_flight_line}  {in_flight_suffix}"
            )),
        ]
    } else {
        vec![
            Line::from(format!("{events_label:<label_width$}  {events_line}")),
            Line::from(format!("{decisions_label:<label_width$}  {decisions_line}")),
            Line::from(format!("{in_flight_label:<label_width$}  {in_flight_line}")),
        ]
    };
    frame.render_widget(Paragraph::new(lines), area);
}

const TIME_ZOOM_AGENT_BAND_LABELS: [&str; 5] = ["think", "read", "edit", "bash", "other"];

/// The zoom pane's own label column width: sized to the longest
/// label it will actually draw (the agent's own band names, or a
/// container's children, whichever this selection calls for), up to
/// a third of the pane's own width -- a fixed 8 columns would leave
/// every pass of a loop reading a truncated `✓ pass…` instead. `0`
/// for a selection with no padded label column at all (a step's own
/// bar, or nothing selected).
fn time_zoom_label_width(
    screen: &oscilloscope_core::screens::TimeScreen,
    pane_width: u16,
) -> usize {
    let max_label_len = match (&screen.selected, &screen.zoom_extra) {
        (Some(step), _)
            if matches!(
                step.detail,
                oscilloscope_core::screens::FocusDetail::Agent { .. }
            ) =>
        {
            TIME_ZOOM_AGENT_BAND_LABELS
                .iter()
                .map(|s| s.chars().count())
                .max()
                .unwrap_or(0)
        }
        (Some(_), Some(oscilloscope_core::screens::TimeZoomExtra::Container { children, .. })) => {
            children
                .iter()
                .map(|c| {
                    format!("{} {}", status_glyph(c.status.kind), c.label)
                        .chars()
                        .count()
                })
                .max()
                .unwrap_or(0)
        }
        _ => 0,
    };
    if max_label_len == 0 {
        return 0;
    }
    let cap = ((pane_width as usize) / 3).max(2);
    (max_label_len + 1).min(cap)
}

/// The zoom pane's own axis line (DESIGN.md's zoom pane: start and
/// end as clock times, plus the duration), for every kind -- a
/// container, a prompt, a tool or an agent alike. `None` only
/// without a `since_ts` at all (a step that has never actually
/// started).
/// The selected row's own time line (DESIGN.md): its own `start_s`/
/// `duration_s` -- the same fields its bar draws from, never
/// `FocusedStep::since_ts`/`elapsed_s`, which fall back to `now -
/// created_at` for a cancelled, aborted, or likely-running/queued
/// step with no `completed_at` of its own and so keep growing for as
/// long as the TUI stays open. Any status still unfinished and in
/// progress (`Running`/`LikelyRunning`/`RunningOrQueued`) reads
/// `→ now`; every other status reads its own real end, already
/// capped at the run's own end once it has ended (`render`'s own
/// `duration_s` fallback chain).
fn zoom_axis_line(
    row: &oscilloscope_core::time::TimeRow,
    run_start_ts: &str,
    now_s: f64,
    ctx: &DrawCtx,
) -> Option<String> {
    let start_s = row.start_s?;
    let is_in_progress = matches!(
        row.status.kind,
        StatusKind::Running | StatusKind::LikelyRunning | StatusKind::RunningOrQueued
    );
    let (end_s, end_text) = if is_in_progress {
        (now_s, "now".to_string())
    } else {
        let end_s = start_s + row.duration_s.unwrap_or(0.0);
        let end_text = offset_clock_label(run_start_ts, end_s, ctx.zone, "%H:%M:%S")?;
        (end_s, end_text)
    };
    let start_text = offset_clock_label(run_start_ts, start_s, ctx.zone, "%H:%M:%S")?;
    Some(format!(
        "{start_text} → {end_text} · {}",
        oscilloscope_core::format::format_duration(end_s - start_s)
    ))
}

/// The selected step's own nearest ancestor loop row's plain label
/// (never the `⟲ N/total` suffix, added only at draw time) -- the
/// loop row stays in `screen.rows` whether it's collapsed or
/// expanded, so this answers the same whichever the loop is;
/// `max_by_key` on path length picks the closest ancestor when more
/// than one loop wraps the selection.
fn time_zoom_ancestor_loop_label<'a>(
    screen: &'a oscilloscope_core::screens::TimeScreen,
    path: &str,
) -> Option<&'a str> {
    screen
        .rows
        .iter()
        .filter_map(|r| match r {
            oscilloscope_core::screens::TimeDisplayRow::Row(tr)
                if tr.kind == oscilloscope_core::render::RowKind::Loop
                    && path.starts_with(&format!("{}.", tr.path)) =>
            {
                Some(tr)
            }
            _ => None,
        })
        .max_by_key(|tr| tr.path.len())
        .map(|tr| tr.label.as_str())
}

/// A title too long for `max`, cut from the *left* with a leading
/// `…` -- unlike [`truncate_label`], the zoom pane's own title ends
/// in the selected step's own name (`fan › pass 1 of 3 › leaf`), the
/// most specific and identifying part, so losing characters from the
/// front keeps it visible instead of the outer, already-less-useful
/// context.
fn truncate_title_from_left(label: &str, max: usize) -> String {
    let count = label.chars().count();
    if count <= max {
        label.to_string()
    } else {
        let skip = count - max.saturating_sub(1);
        format!("…{}", label.chars().skip(skip).collect::<String>())
    }
}

fn draw_time_zoom_pane(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::screens::TimeScreen,
    app: &mut App,
    transcript_view: Option<&oscilloscope_core::transcript::TranscriptView>,
    ctx: &DrawCtx,
    now: &str,
) {
    let label_width = time_zoom_label_width(screen, area.width);
    let bar_width = (area.width as usize).saturating_sub(2 + label_width).max(4);
    let (by_path, _) = time_row_groups(&screen.rows);
    let mut lines: Vec<Line<'static>> = Vec::new();
    match &screen.selected {
        None => lines.push(Line::styled(
            "nothing selected",
            Style::default().fg(ctx.theme.dim),
        )),
        Some(step) => {
            let selected_row = app.selected_path.as_deref().and_then(|p| by_path.get(p));
            if let Some(row) = selected_row {
                if let (Some(run_start_ts), Some(now_s)) = (
                    screen.axis.start_ts.as_deref(),
                    screen
                        .axis
                        .start_ts
                        .as_deref()
                        .and_then(|rs| oscilloscope_core::observe::duration_seconds(rs, now)),
                ) {
                    if let Some(axis_line) = zoom_axis_line(row, run_start_ts, now_s, ctx) {
                        lines.push(Line::styled(axis_line, Style::default().fg(ctx.theme.dim)));
                    }
                }
            }
            match &step.detail {
                oscilloscope_core::screens::FocusDetail::Agent {
                    live,
                    raw_turns,
                    raw_tool_calls,
                    raw_tool_counts,
                    ..
                } => {
                    let run_start_ts = screen.axis.start_ts.clone();
                    let session_start_s = step
                        .since_ts
                        .as_deref()
                        .zip(run_start_ts.as_deref())
                        .and_then(|(s, rs)| oscilloscope_core::observe::duration_seconds(rs, s))
                        .unwrap_or(0.0);
                    let now_s = run_start_ts
                        .as_deref()
                        .and_then(|rs| oscilloscope_core::observe::duration_seconds(rs, now))
                        .unwrap_or(session_start_s);
                    // Only while the agent is actually running: an
                    // interrupted run's own leftover `meta.live` (issue
                    // #637) must not keep the think band, the footer's
                    // running-tool time or this axis growing forever --
                    // `step.elapsed_s` alone would, since a cancelled
                    // step with no `completed_at` reads `now - created_
                    // at` the same as a running one does.
                    let running = step.status.kind == StatusKind::Running && live.is_some();
                    // The session's own end: `now` while it's still
                    // running, else the row's own real (frozen) span.
                    let axis_end_s = if running {
                        now_s
                    } else {
                        selected_row
                            .and_then(|row| row.start_s.zip(row.duration_s))
                            .map_or(now_s, |(s, d)| s + d)
                    };
                    if let (Some(view), Some(run_start_ts)) =
                        (transcript_view, run_start_ts.as_deref())
                    {
                        let bands = oscilloscope_core::time::agent_zoom_bands(
                            &view.calls,
                            session_start_s,
                            now_s,
                            run_start_ts,
                            running,
                        );
                        // DESIGN.md: "the bands start where the window
                        // starts, after a dim `…`" -- shown only when the
                        // window itself is actually cut off and the
                        // earliest band really does start right at it (a
                        // band starting later than the session's own
                        // start is ordinary thinking time, not a gap the
                        // window missed).
                        if view.truncated
                            && bands
                                .first()
                                .is_some_and(|b| (b.start_s - session_start_s).abs() < 1e-6)
                        {
                            lines.push(Line::styled("…", Style::default().fg(ctx.theme.dim)));
                        }
                        const ORDER: [oscilloscope_core::time::ZoomBandKind; 5] = [
                            oscilloscope_core::time::ZoomBandKind::Think,
                            oscilloscope_core::time::ZoomBandKind::Read,
                            oscilloscope_core::time::ZoomBandKind::Edit,
                            oscilloscope_core::time::ZoomBandKind::Bash,
                            oscilloscope_core::time::ZoomBandKind::Other,
                        ];
                        for kind in ORDER {
                            let segments: Vec<(f64, f64, bool)> = bands
                                .iter()
                                .filter(|b| b.kind == kind)
                                .map(|b| (b.start_s, b.end_s, b.running))
                                .collect();
                            if segments.is_empty() {
                                continue;
                            }
                            let (label, fill) = match kind {
                                oscilloscope_core::time::ZoomBandKind::Think => ("think", '▓'),
                                oscilloscope_core::time::ZoomBandKind::Read => ("read", '░'),
                                oscilloscope_core::time::ZoomBandKind::Edit => ("edit", '▒'),
                                oscilloscope_core::time::ZoomBandKind::Bash => ("bash", '█'),
                                oscilloscope_core::time::ZoomBandKind::Other => ("other", '·'),
                            };
                            let bar = multi_segment_bar(
                                &segments,
                                session_start_s,
                                axis_end_s,
                                bar_width,
                                fill,
                            );
                            lines.push(Line::from(format!("{label:<label_width$}{bar}")));
                        }
                    } else if let Some(current) =
                        live.as_deref().and_then(|l| l.current_tool.as_ref())
                    {
                        // DESIGN.md: "With no transcript, the footer and
                        // a `running: <tool>` line (from `current_tool`)
                        // are all it shows."
                        lines.push(Line::styled(
                            format!("running: {}", current.name),
                            Style::default().fg(ctx.theme.dim),
                        ));
                    } else {
                        lines.push(Line::styled(
                            oscilloscope_core::screens::AGENT_LIVE_DATA_NOTE,
                            Style::default().fg(ctx.theme.dim),
                        ));
                    }
                    // `meta.live`'s own tools/turns/tool_calls while
                    // running, `meta.raw`'s once finished (DESIGN.md).
                    let tools = live
                        .as_deref()
                        .map(|l| l.tools.clone())
                        .unwrap_or_else(|| raw_tool_counts.clone());
                    let turns = live.as_deref().and_then(|l| l.turns).or(*raw_turns);
                    let tool_calls = live
                        .as_deref()
                        .and_then(|l| l.tool_calls)
                        .or(*raw_tool_calls);
                    let elapsed = (axis_end_s - session_start_s).max(0.0);
                    // Only while the agent is actually live: a
                    // finished session's own transcript can end in an
                    // unmatched `tool_start` (an interrupted call that
                    // was never followed by a `tool_end`), which would
                    // otherwise add wall-clock time to a call that
                    // isn't running any more, growing for as long as
                    // the TUI stays open.
                    let running_tool = running
                        .then(|| {
                            live.as_deref().and_then(|l| {
                                let rs = run_start_ts.as_deref()?;
                                let calls =
                                    transcript_view.map(|v| v.calls.as_slice()).unwrap_or(&[]);
                                let current = l
                                    .current_tool
                                    .as_ref()
                                    .map(|c| (c.name.as_str(), c.started_at.as_str()));
                                oscilloscope_core::time::running_tool_elapsed_ms(
                                    calls, current, now_s, rs,
                                )
                            })
                        })
                        .flatten();
                    let footer = oscilloscope_core::time::zoom_agent_footer(
                        turns,
                        tool_calls,
                        &tools,
                        elapsed,
                        running_tool,
                    );
                    let mut footer_parts = Vec::new();
                    if let Some(turns) = footer.turns {
                        footer_parts.push(format!("turn {turns}"));
                    }
                    if let Some(calls) = footer.tool_calls {
                        footer_parts.push(format!("{calls} tools"));
                    }
                    if let Some((name, ms, pct)) = footer.busiest_tool {
                        footer_parts.push(format!(
                            "{name} {} ({pct:.0}%)",
                            oscilloscope_core::format::format_duration(ms as f64 / 1000.0)
                        ));
                    }
                    if !footer_parts.is_empty() {
                        lines.push(Line::styled(
                            footer_parts.join(" · "),
                            Style::default().fg(ctx.theme.dim),
                        ));
                    }
                }
                _ => match &screen.zoom_extra {
                    Some(oscilloscope_core::screens::TimeZoomExtra::Container {
                        children,
                        start_s,
                        axis_end_s,
                    }) => {
                        let viewport = (area.height.saturating_sub(2) as usize).max(1);
                        let budget = zoom_container_budget(children.len(), viewport);
                        for child in children.iter().take(budget) {
                            let glyph = status_glyph(child.status.kind);
                            let label = truncate_label(
                                &format!("{glyph} {}", child.label),
                                label_width - 1,
                            );
                            let bar = time_bar_string(
                                child.start_s,
                                child.duration_s,
                                child.status.kind == StatusKind::Running,
                                child.ghost,
                                child.status.kind == StatusKind::Failed,
                                *start_s,
                                *axis_end_s,
                                bar_width,
                            );
                            lines.push(Line::styled(
                                format!("{label:<label_width$}{bar}"),
                                status_style(child.status.kind, ctx.theme),
                            ));
                        }
                        let more = children.len().saturating_sub(budget);
                        if more > 0 {
                            lines.push(Line::styled(
                                format!("+{more} more"),
                                Style::default().fg(ctx.theme.dim),
                            ));
                        }
                    }
                    Some(oscilloscope_core::screens::TimeZoomExtra::StepBar {
                        start_s,
                        p50_s,
                        p90_s,
                        axis_end_s,
                        waiting_label,
                    }) => {
                        // Wide enough to also show p50/p90 when either
                        // falls past the step's own real end (DESIGN.md:
                        // "with its p50 and p90 marked on the axis").
                        let window_end_s = axis_end_s
                            .max(start_s + p50_s.unwrap_or(0.0))
                            .max(start_s + p90_s.unwrap_or(0.0));
                        let mut bar: Vec<char> = time_bar_string(
                            Some(*start_s),
                            Some(axis_end_s - start_s),
                            waiting_label.is_some(),
                            None,
                            step.error.is_some(),
                            *start_s,
                            window_end_s,
                            bar_width,
                        )
                        .chars()
                        .collect();
                        for (offset, mark) in [(*p50_s, '┆'), (*p90_s, '┄')] {
                            if let Some(offset) = offset {
                                if let Some(col) = oscilloscope_core::time::now_column(
                                    *start_s,
                                    window_end_s,
                                    bar_width,
                                    start_s + offset,
                                ) {
                                    if col < bar.len() {
                                        bar[col] = mark;
                                    }
                                }
                            }
                        }
                        lines.push(Line::from(bar.into_iter().collect::<String>()));
                        let mut marks = Vec::new();
                        if let Some(p50) = p50_s {
                            marks.push(format!(
                                "p50 {}",
                                oscilloscope_core::format::format_duration(*p50)
                            ));
                        }
                        if let Some(p90) = p90_s {
                            marks.push(format!(
                                "p90 {}",
                                oscilloscope_core::format::format_duration(*p90)
                            ));
                        }
                        if !marks.is_empty() {
                            lines.push(Line::styled(
                                marks.join(" · "),
                                Style::default().fg(ctx.theme.dim),
                            ));
                        }
                        if let Some(label) = waiting_label {
                            lines.push(Line::styled(*label, Style::default().fg(ctx.theme.dim)));
                        }
                    }
                    None => {
                        lines.push(Line::from(step.breadcrumb.clone()));
                        if let Some(since) = step
                            .since_ts
                            .as_deref()
                            .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone))
                        {
                            lines.push(Line::styled(
                                format!("since {since}"),
                                Style::default().fg(ctx.theme.dim),
                            ));
                        }
                    }
                },
            }
        }
    }
    let max_scroll = lines
        .len()
        .saturating_sub(area.height.saturating_sub(2) as usize);
    app.time_zoom_viewport = area.height.saturating_sub(2) as usize;
    app.time_zoom_scroll = app.time_zoom_scroll.min(max_scroll);
    let visible_lines: Vec<Line<'static>> = lines.into_iter().skip(app.time_zoom_scroll).collect();
    let title = match &screen.selected {
        Some(step) => {
            let breadcrumb = match time_zoom_ancestor_loop_label(screen, &step.path) {
                Some(loop_label) => format!("{loop_label} › {}", step.breadcrumb),
                None => step.breadcrumb.clone(),
            };
            let budget = (area.width as usize).saturating_sub(" zoom ·  ".chars().count() + 2);
            format!(" zoom · {} ", truncate_title_from_left(&breadcrumb, budget))
        }
        None => " zoom ".to_string(),
    };
    frame.render_widget(
        Paragraph::new(visible_lines).block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}

fn draw_time_vs_usual_pane(
    frame: &mut Frame,
    area: Rect,
    state: &oscilloscope_core::render::RenderState,
    screen: &oscilloscope_core::screens::TimeScreen,
    app: &mut App,
    ctx: &DrawCtx,
) {
    // `now`, `p50` and the mark each take a fixed width (10 + 10 +
    // 1); the label gets whatever is left of the pane's own inner
    // width (2 narrower than `area.width` for its own border), cut
    // with the usual truncation rather than pushing the fixed columns
    // past it.
    let inner_width = (area.width as usize).saturating_sub(2);
    // `now`/`p50`/the mark take 10 + 10 + 1 columns; the extra 1
    // reserves a gutter space so a full-width label never runs
    // straight into the `now` column with nothing between them.
    let label_width = inner_width.saturating_sub(22);
    // `build_vs_usual_pane` has no view of the user's own collapse or
    // filter state -- it must list exactly the timeline's own visible
    // rows (DESIGN.md), so a row collapsed with `←`, or hidden by `/`
    // or `e`, is filtered out here instead.
    let visible_paths: std::collections::BTreeSet<&str> =
        crate::keys::visible_rows_for_time(&state.rows, &screen.rows, app)
            .into_iter()
            .map(|r| r.path.as_str())
            .collect();
    let mut lines: Vec<Line<'static>> = Vec::new();
    if let Some((count, threshold)) = screen.vs_usual.too_few_runs {
        lines.push(Line::styled(
            format!("history: {count} of {threshold} runs of this version"),
            Style::default().fg(ctx.theme.dim),
        ));
    } else {
        for row in screen
            .vs_usual
            .rows
            .iter()
            .filter(|r| visible_paths.contains(r.path.as_str()))
        {
            let mark = match row.mark {
                oscilloscope_core::time::VsUsualMark::Slower => '▲',
                oscilloscope_core::time::VsUsualMark::Faster => '▼',
                oscilloscope_core::time::VsUsualMark::Usual => '≈',
            };
            let now_text = oscilloscope_core::format::format_duration(row.now_s);
            let now_text = if row.running {
                format!("{now_text}…")
            } else {
                now_text
            };
            let p50_text = oscilloscope_core::format::format_duration(row.p50_s);
            let label = truncate_label(&row.label, label_width);
            lines.push(Line::from(format!(
                "{label:<label_width$} {now_text:<10}{p50_text:<10}{mark}"
            )));
        }
        let hhmm = |ts: &str| {
            oscilloscope_core::format::format_clock_time(ts, ctx.zone)
                .map(|hhmmss| hhmmss[..5.min(hhmmss.len())].to_string())
        };
        let mut footer = Vec::new();
        if let Some(finish) = screen.vs_usual.finish_ts.as_deref().and_then(hhmm) {
            footer.push(format!("finish ~{finish}"));
        }
        if let (Some(low), Some(high)) = (
            screen.vs_usual.usual_low_ts.as_deref().and_then(hhmm),
            screen.vs_usual.usual_high_ts.as_deref().and_then(hhmm),
        ) {
            footer.push(format!("usual {low}–{high}"));
        }
        if let Some(runs) = screen.vs_usual.from_runs {
            footer.push(format!("from {runs} runs"));
        }
        // Drops whole parts from the end (`from N runs` first, then
        // the usual range) until what's left fits the pane -- never a
        // part cut to a partial fragment.
        while footer.join(" · ").chars().count() > inner_width && !footer.is_empty() {
            footer.pop();
        }
        if !footer.is_empty() {
            lines.push(Line::styled(
                footer.join(" · "),
                Style::default().fg(ctx.theme.dim),
            ));
        }
    }
    let title = if screen.vs_usual.earlier_versions {
        " vs usual · earlier versions "
    } else {
        " vs usual "
    };
    let max_scroll = lines
        .len()
        .saturating_sub(area.height.saturating_sub(2) as usize);
    app.time_vs_usual_viewport = area.height.saturating_sub(2) as usize;
    app.time_vs_usual_scroll = app.time_vs_usual_scroll.min(max_scroll);
    let visible_lines: Vec<Line<'static>> =
        lines.into_iter().skip(app.time_vs_usual_scroll).collect();
    frame.render_widget(
        Paragraph::new(visible_lines).block(Block::default().borders(Borders::ALL).title(title)),
        area,
    );
}

/// The plain list the Time screen falls back to below 80x20
/// (DESIGN.md's "Small terminals": "a list of status, step, start
/// and duration, with no axis").
fn draw_time_plain_list(
    frame: &mut Frame,
    area: Rect,
    state: &oscilloscope_core::render::RenderState,
    screen: &oscilloscope_core::screens::TimeScreen,
    app: &mut App,
    ctx: &DrawCtx,
) {
    let all_visible = visible_rows(&state.rows, app);
    let (by_path, _) = time_row_groups(&screen.rows);
    let loop_prefixes: Vec<String> = by_path
        .values()
        .filter(|r| r.kind == oscilloscope_core::render::RowKind::Loop)
        .map(|r| format!("{}.", r.path))
        .collect();
    let is_under_a_loop = |path: &str| loop_prefixes.iter().any(|p| path.starts_with(p.as_str()));
    let visible: Vec<&Row> = all_visible
        .into_iter()
        .filter(|r| !is_under_a_loop(&r.path))
        .collect();
    let effective_selected_path: Option<&str> = app.selected_path.as_deref().map(|p| {
        if is_under_a_loop(p) {
            nearest_ancestor_loop_path(&by_path, p).unwrap_or(p)
        } else {
            p
        }
    });
    let selected_idx =
        effective_selected_path.and_then(|p| visible.iter().position(|r| r.path == p));
    let viewport = (area.height as usize).max(1);
    app.tree_viewport = viewport;
    let line_counts = vec![1usize; visible.len()];
    app.tree_scroll = clamp_state_scroll(app.tree_scroll, selected_idx, &line_counts, viewport);
    let run_start_ts = screen.axis.start_ts.as_deref();
    let mut lines = Vec::new();
    for row in visible.iter().skip(app.tree_scroll).take(viewport) {
        let Some(time_row) = by_path.get(row.path.as_str()) else {
            continue;
        };
        let indent = "  ".repeat(time_row.depth);
        let glyph = status_glyph(time_row.status.kind);
        let mut label = format!("{indent}{glyph} {}", time_row.label);
        if let Some(progress) = &time_row.loop_progress {
            let total = progress
                .total
                .map(|t| t.to_string())
                .unwrap_or_else(|| "?".to_string());
            label.push_str(&format!(" ⟲ {}/{total}", progress.done));
        }
        let start_text = time_row
            .start_s
            .zip(run_start_ts)
            .and_then(|(s, rs)| {
                oscilloscope_core::diff::parse_instant(rs)
                    .map(|dt| dt + chrono::Duration::milliseconds((s * 1000.0) as i64))
            })
            .map(|dt| {
                ctx.zone
                    .local(dt.with_timezone(&chrono::Utc))
                    .format("%H:%M:%S")
                    .to_string()
            })
            .unwrap_or_default();
        let duration_text = time_row
            .duration_s
            .map(oscilloscope_core::format::format_duration)
            .unwrap_or_default();
        let selected = effective_selected_path == Some(row.path.as_str());
        let style = if selected {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        lines.push(Line::styled(
            format!(
                "{:<30}{start_text:<9}{duration_text:>7}",
                truncate_label(&label, 30)
            ),
            style,
        ));
    }
    frame.render_widget(Paragraph::new(lines), area);
}

#[allow(clippy::too_many_arguments)]
fn draw_time_screen(
    frame: &mut Frame,
    frame_area: Rect,
    body: Rect,
    state: &oscilloscope_core::render::RenderState,
    screen: &oscilloscope_core::screens::TimeScreen,
    app: &mut App,
    transcript_view: Option<&oscilloscope_core::transcript::TranscriptView>,
    ctx: &DrawCtx,
    now: &str,
) {
    let three_pane = frame_area.width >= thresholds::TIME_THREE_PANE_MIN_WIDTH
        && frame_area.height >= thresholds::TIME_THREE_PANE_MIN_HEIGHT;
    let axis_only = frame_area.width >= thresholds::TIME_AXIS_ONLY_MIN_WIDTH
        && frame_area.height >= thresholds::TIME_AXIS_ONLY_MIN_HEIGHT;

    if !axis_only {
        app.time_zoom_viewport = 0;
        app.time_vs_usual_viewport = 0;
        draw_time_plain_list(frame, body, state, screen, app, ctx);
        return;
    }

    // Below three_pane's own threshold, neither the zoom nor the vs-
    // usual pane is drawn at all -- the timeline and its strips take
    // the whole body rather than reserving a golden-ratio minor
    // section underneath that neither pane would then ever fill.
    let timeline_rows = if three_pane {
        oscilloscope_core::layout::golden_major_section(body.height)
    } else {
        body.height
    };
    let top_bottom = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(timeline_rows),
            Constraint::Min(body.height.saturating_sub(timeline_rows)),
        ])
        .split(body);
    let timeline_strip_rows = top_bottom[0].height.saturating_sub(3);
    let timeline_area_split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(timeline_strip_rows), Constraint::Length(3)])
        .split(top_bottom[0]);
    // One layout, shared by the timeline and the strips, so both
    // bucket and draw onto exactly the same columns (DESIGN.md: the
    // strips are "on the same axis" as the bars) -- both panes have
    // the same width, `timeline_area_split` only having split their
    // shared column vertically.
    let layout = time_layout(screen, app, timeline_area_split[0].width, frame_area.width);
    draw_time_timeline(
        frame,
        timeline_area_split[0],
        state,
        screen,
        &layout,
        app,
        ctx,
    );
    draw_time_strips(frame, timeline_area_split[1], screen, &layout);

    if three_pane {
        let left_width = oscilloscope_core::layout::left_column_width(top_bottom[1].width);
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(left_width),
                Constraint::Length(top_bottom[1].width - left_width),
            ])
            .split(top_bottom[1]);
        draw_time_zoom_pane(frame, columns[0], screen, app, transcript_view, ctx, now);
        draw_time_vs_usual_pane(frame, columns[1], state, screen, app, ctx);
    } else {
        app.time_zoom_viewport = 0;
        app.time_vs_usual_viewport = 0;
    }
}

#[cfg(test)]
mod golden_tests {
    use super::*;
    use electricity_bytecode::{
        Condition, EffectPath, Escape, LeafKind, LoopFlow, LoopId, LoopSpec, NodeKind, OnError, Op,
        ParamNode, Program, PromptContent, PromptOp, PromptType, Region, TemplateText, ToolOp,
    };
    use oscilloscope_core::diff::{LogKind, LogLine};
    use oscilloscope_core::model::{ProcessState, RunModel};
    use oscilloscope_core::observe::Event;
    use oscilloscope_core::plan::PlanTree;
    use oscilloscope_core::render;
    use oscilloscope_core::render::RowKind;
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
        let flow_screen = oscilloscope_core::screens::build_flow_screen(
            &rendered.rows,
            &rendered.unfolded_rows,
            &oscilloscope_core::model::flatten_state(state.unwrap_or(&Value::Null)),
            plan,
            model,
            state,
            &[],
            Some(selected),
            &test_zone(),
            "2026-01-01T00:00:00Z",
        );
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
                    None,
                    Some(&flow_screen),
                    None,
                    None,
                    &mut CostLayoutCache::default(),
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
        overview_snapshot_at_with_log(width, height, plan, model, state, process, &[])
    }

    /// [`overview_snapshot_at`], with the now pane's own log lines
    /// (the TUI's own `log_lines: Vec<String>` buffer, not
    /// `RenderState::log`, which the overview never reads -- see
    /// `build_overview_if_active`'s own doc comment in `main.rs`).
    fn overview_snapshot_at_with_log(
        width: u16,
        height: u16,
        plan: &PlanTree,
        model: &mut RunModel,
        state: Option<&Value>,
        process: ProcessState,
        log_lines: &[String],
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
            log_lines,
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
                    None,
                    None,
                    None,
                    None,
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        terminal.backend().to_string()
    }

    /// [`overview_snapshot_at`], with representative history figures
    /// set directly on the header (issue #649's own overview goldens:
    /// "with and without history") -- the same shape
    /// `history_io::apply_history_eta` itself writes, set here by
    /// hand rather than through a real `HistoryHandle`, which this
    /// module has no reason to depend on.
    #[allow(clippy::too_many_arguments)]
    fn overview_snapshot_at_with_history(
        width: u16,
        height: u16,
        plan: &PlanTree,
        model: &mut RunModel,
        state: Option<&Value>,
        process: ProcessState,
        running_step_key: Option<&str>,
    ) -> String {
        let mut rendered = render::build_folded(
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
        rendered.header.history_eta_ts = Some("2026-01-01T00:12:00Z".to_string());
        rendered.header.history_eta_remaining_s = Some(420.0);
        rendered.header.history_eta_low_ts = Some("2026-01-01T00:11:00Z".to_string());
        rendered.header.history_eta_high_ts = Some("2026-01-01T00:14:00Z".to_string());
        rendered.header.history_vs_usual_s = Some(30.0);
        rendered.header.history_usual_total_s = Some(660.0);
        rendered.header.history_same_hash_count = Some(7);
        if let Some(key) = running_step_key {
            rendered.header.history_step_usual.insert(
                key.to_string(),
                oscilloscope_core::history::StepStats {
                    count: 7,
                    busy_p50_s: Some(300.0),
                    busy_p10_s: None,
                    busy_p90_s: Some(420.0),
                    ..Default::default()
                },
            );
        }
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
            &[],
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
                    None,
                    None,
                    None,
                    None,
                    &mut CostLayoutCache::default(),
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

    #[allow(clippy::too_many_arguments)]
    fn time_snapshot_at(
        width: u16,
        height: u16,
        plan: &PlanTree,
        model: &mut RunModel,
        state: Option<&Value>,
        process: ProcessState,
        selected_path: Option<&str>,
        expanded_loops: &std::collections::BTreeSet<String>,
        with_history: bool,
    ) -> String {
        let mut rendered = render::build_folded(
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
        if with_history {
            rendered.header.history_eta_ts = Some("2026-01-01T00:12:00Z".to_string());
            rendered.header.history_eta_remaining_s = Some(420.0);
            rendered.header.history_eta_low_ts = Some("2026-01-01T00:11:00Z".to_string());
            rendered.header.history_eta_high_ts = Some("2026-01-01T00:14:00Z".to_string());
            rendered.header.history_vs_usual_s = Some(30.0);
            rendered.header.history_usual_total_s = Some(660.0);
            rendered.header.history_same_hash_count = Some(7);
            rendered.header.history_step_usual.insert(
                "a".to_string(),
                oscilloscope_core::history::StepStats {
                    count: 7,
                    busy_p50_s: Some(60.0),
                    busy_p10_s: Some(50.0),
                    busy_p90_s: Some(70.0),
                    start_p50_s: Some(0.0),
                    end_p50_s: Some(60.0),
                    ..Default::default()
                },
            );
            rendered.header.history_step_usual.insert(
                "b".to_string(),
                oscilloscope_core::history::StepStats {
                    count: 7,
                    busy_p50_s: Some(300.0),
                    busy_p10_s: Some(240.0),
                    busy_p90_s: Some(360.0),
                    start_p50_s: Some(60.0),
                    end_p50_s: Some(360.0),
                    ..Default::default()
                },
            );
        }
        let empty = Value::Null;
        let flat = oscilloscope_core::model::flatten_state(state.unwrap_or(&empty));
        let time_screen = oscilloscope_core::screens::build_time_screen(
            &rendered.rows,
            &flat,
            plan,
            model,
            &rendered.header,
            state,
            &[],
            selected_path,
            expanded_loops,
            "2026-01-01T00:05:00Z",
        );
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Time;
        app.selected_path = selected_path.map(str::to_string);
        app.time_expanded_loops = expanded_loops.clone();
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
                    None,
                    None,
                    None,
                    Some(&time_screen),
                    None,
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        terminal.backend().to_string()
    }

    /// vs usual lists only the timeline's own currently visible rows
    /// (DESIGN.md) -- collapsing `prime` with `←` hides its own
    /// children `a` and `b` from the timeline, so neither belongs in
    /// vs usual either, even though both still have history stats of
    /// their own.
    #[test]
    fn time_vs_usual_pane_excludes_a_row_collapsed_out_of_the_timeline() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = overview_running_state();
        let now = "2026-01-01T00:05:00Z";
        let mut rendered = render::build_folded(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            300.0,
            now,
            &[],
            true,
            &Default::default(),
        );
        rendered.header.history_same_hash_count = Some(7);
        rendered.header.history_step_usual.insert(
            "a".to_string(),
            oscilloscope_core::history::StepStats {
                count: 7,
                busy_p50_s: Some(60.0),
                busy_p10_s: Some(50.0),
                busy_p90_s: Some(70.0),
                ..Default::default()
            },
        );
        rendered.header.history_step_usual.insert(
            "b".to_string(),
            oscilloscope_core::history::StepStats {
                count: 7,
                busy_p50_s: Some(300.0),
                busy_p10_s: Some(240.0),
                busy_p90_s: Some(360.0),
                ..Default::default()
            },
        );
        let flat = oscilloscope_core::model::flatten_state(&state);
        let time_screen = oscilloscope_core::screens::build_time_screen(
            &rendered.rows,
            &flat,
            &plan,
            &model,
            &rendered.header,
            Some(&state),
            &[],
            None,
            &Default::default(),
            now,
        );
        // Both rows are really there before any collapsing -- proves
        // the fixture itself would otherwise show them.
        assert_eq!(
            time_screen
                .vs_usual
                .rows
                .iter()
                .map(|r| r.label.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Time;
        app.collapsed.insert("prime".to_string());
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
        let backend = TestBackend::new(100, 30);
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
                    Some(&state),
                    &test_ctx,
                    0,
                    now,
                    None,
                    None,
                    None,
                    Some(&time_screen),
                    None,
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        let text = terminal.backend().to_string();
        let vs_usual_pane: String = text
            .lines()
            .filter(|l| l.contains("vs usual") || l.contains('\u{2502}'))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !vs_usual_pane.contains('≈')
                && !vs_usual_pane.contains('▲')
                && !vs_usual_pane.contains('▼'),
            "no row's own mark may still be drawn once both rows are collapsed out: {vs_usual_pane}"
        );
    }

    fn many_steps_plan(n: usize) -> PlanTree {
        let root_path = EffectPath::root();
        let ops: Vec<Op> = (0..n)
            .map(|i| {
                tool(
                    root_path.clone().push_name(format!("s{i}")),
                    &format!("s{i}"),
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
        PlanTree::from_program(&Program {
            root,
            prompts: Default::default(),
            effect_names: Default::default(),
            document: None,
            runtime_block: None,
            interface: None,
            adapter: None,
            model: None,
        })
    }

    /// Below 80x20 the Time screen falls back to a plain list
    /// (DESIGN.md's "Small terminals") -- it must follow the
    /// selection the same way every other list does, not render every
    /// row starting from the top regardless of height.
    #[test]
    fn time_plain_list_below_80x20_scrolls_to_follow_the_selection() {
        let n = 30;
        let plan = many_steps_plan(n);
        let mut model = RunModel::new();
        let mut state = serde_json::Map::new();
        for i in 0..n {
            state.insert(
                format!("s{i}"),
                json!({"value": "ok", "meta": {
                    "created_at": format!("2026-01-01T00:{:02}:00Z", i),
                    "completed_at": format!("2026-01-01T00:{:02}:01Z", i)
                }}),
            );
        }
        let state = json!({"prime": Value::Object(state)});
        let now = "2026-01-01T00:30:00Z";
        let rendered = render::build_folded(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(true),
            1800.0,
            now,
            &[],
            true,
            &Default::default(),
        );
        let flat = oscilloscope_core::model::flatten_state(&state);
        let last = format!("prime.s{}", n - 1);
        let time_screen = oscilloscope_core::screens::build_time_screen(
            &rendered.rows,
            &flat,
            &plan,
            &model,
            &rendered.header,
            Some(&state),
            &[],
            Some(&last),
            &Default::default(),
            now,
        );
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Time;
        app.selected_path = Some(last.clone());
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
        // Below both of the Time screen's own width/height
        // thresholds -- the plain-list fallback.
        let backend = TestBackend::new(70, 15);
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
                    Some(&state),
                    &test_ctx,
                    0,
                    now,
                    None,
                    None,
                    None,
                    Some(&time_screen),
                    None,
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        let text = terminal.backend().to_string();
        assert!(
            text.contains(&format!("s{}", n - 1)),
            "the selected, last row must be visible: {text}"
        );
        assert!(
            !text.contains("s0 "),
            "scrolled past the first row, which must no longer show: {text}"
        );
    }

    /// Issue #637: a single top-level `agent` tool step, still
    /// running, with a realistic `meta.live` (every field the live-
    /// progress table documents, including a figure Claude Code
    /// never reports live -- `tokens_received`/`cost_usd` stay
    /// `null`, never `0`, DESIGN.md).
    fn agent_running_state() -> Value {
        json!({"prime": {"value": null, "meta": {"created_at": "2026-01-01T11:46:43Z", "completed_at": null},
            "fix": {"value": null, "meta": {
                "created_at": "2026-01-01T11:46:43Z", "completed_at": null, "provider": "agent",
                "params_rendered": {"engine": "claude_code", "prompt": "Fix the bug.\nSee the issue for details."},
                "live": {
                    "engine": "claude_code", "model": "sonnet", "session_id": "s1",
                    "transcript": "/tmp/fix-transcript.jsonl",
                    "attempt": 1, "phase": "turn",
                    "started_at": "2026-01-01T11:46:43Z", "updated_at": "2026-01-01T11:52:54Z",
                    "turns": 14, "tool_calls": 31,
                    "tools": {"bash": {"calls": 20, "ms": 120000}, "read": {"calls": 11, "ms": 5000}},
                    "current_tool": {"name": "bash", "started_at": "2026-01-01T11:52:30Z"},
                    "tokens_sent": 2100000, "tokens_received": null, "cost_usd": null,
                    "tokens_cached_in": 1800000, "tokens_cache_write": 50000,
                    "context_tokens": 142000, "context_window": 200000,
                    "turn_tokens": [100000, 142000],
                    "files_changed": 3, "lines_added": 12, "lines_removed": 3
                }
            }}
        }})
    }

    /// [`agent_running_state`], finished: no `meta.live` left at all
    /// (removed once the attempt ends), the same generic
    /// `tokens_sent`/`tokens_received`/`cost_usd`/`value` fields
    /// every other finished tool step carries.
    fn agent_finished_state() -> Value {
        json!({"prime": {"value": null, "meta": {"created_at": "2026-01-01T11:46:43Z", "completed_at": "2026-01-01T11:52:55Z"},
            "fix": {"value": "Done. All tests pass.", "meta": {
                "created_at": "2026-01-01T11:46:43Z", "completed_at": "2026-01-01T11:52:55Z", "provider": "agent",
                "params_rendered": {"engine": "claude_code", "prompt": "Fix the bug.\nSee the issue for details."},
                "tokens_sent": 2100000, "tokens_received": 18000, "cost_usd": 1.10,
                "raw": {"engine": "claude_code", "turns": 14, "tool_calls": 31, "transcript": "/tmp/fix-transcript.jsonl",
                    "files_changed": 3, "lines_added": 12, "lines_removed": 3}
            }}
        }})
    }

    /// [`agent_finished_state`], with `prime` itself left running
    /// (`completed_at: null`) -- the overview's own goldens for a
    /// finished agent step want `NowFocus::LastFinished` (a real
    /// finished step, nothing else running), not `NowFocus::Ended`
    /// (the whole run having ended is a different state entirely,
    /// already covered by every other finished-run golden here).
    fn agent_finished_but_run_still_going_state() -> Value {
        let mut state = agent_finished_state();
        state["prime"]["meta"]["completed_at"] = Value::Null;
        state
    }

    /// Issue #637's acceptance item 2: the whole run has ended --
    /// SIGINT lands mid the agent's first tool call, same shape
    /// `oscilloscope/tests/fixtures/agent_live_pi_cancelled` recorded
    /// from a real `cof` run (`cancel_state.py`) -- `prime` itself
    /// carries DESIGN.md §2.1 rule 7's own "Interrupted" error, but
    /// `fix`'s own `meta.live` is still there, with no `completed_at`
    /// of its own: `NowFocus::Ended` with no step of its own to focus
    /// on otherwise.
    fn agent_cancelled_state() -> Value {
        json!({
            "runtime": {"last_run": {"completed_at": "2026-01-01T11:46:54Z"}},
            "prime": {"value": null, "meta": {
                "created_at": "2026-01-01T11:46:43Z", "completed_at": "2026-01-01T11:46:54Z",
                "error": "Interrupted (Ctrl-C/SIGINT)"
            },
                "fix": {"value": null, "meta": {
                    "created_at": "2026-01-01T11:46:43Z", "completed_at": null, "provider": "agent",
                    "params_rendered": {"engine": "pi", "prompt": "Fix the bug.\nSee the issue for details."},
                    "live": {
                        "engine": "pi", "model": null, "session_id": "s1",
                        "transcript": "/tmp/fix-transcript.jsonl",
                        "attempt": 1, "phase": "turn",
                        "started_at": "2026-01-01T11:46:43Z", "updated_at": "2026-01-01T11:46:50Z",
                        "turns": 1, "tool_calls": 1,
                        "tools": {"bash": {"calls": 1, "ms": 0}},
                        "current_tool": {"name": "bash", "started_at": "2026-01-01T11:46:50Z"},
                        "tokens_sent": 2100, "tokens_received": 40, "cost_usd": 0.012,
                        "tokens_cached_in": 800, "tokens_cache_write": 100,
                        "context_tokens": 2100, "context_window": null,
                        "turn_tokens": [2100],
                        "files_changed": null, "lines_added": null, "lines_removed": null
                    }
                }}
            }
        })
    }

    /// The transcript lines `agent_running_state`'s own `fix` step
    /// would have written by the time it reached `meta.live.turns ==
    /// 14` -- enough for the now pane / selected pane's own `doing:`
    /// lines and `tests` word to have something real to read.
    fn agent_running_transcript() -> oscilloscope_core::transcript::TranscriptView {
        let lines = vec![
            json!({"t": "2026-01-01T11:47:05Z", "type": "tool_start", "id": 1, "tool": "read", "target": "PLAN.md"}),
            json!({"t": "2026-01-01T11:47:06Z", "type": "tool_end", "id": 1, "ok": true, "ms": 20}),
            json!({"t": "2026-01-01T11:49:30Z", "type": "tool_start", "id": 2, "tool": "bash", "target": "pytest -k timeout"}),
            json!({"t": "2026-01-01T11:49:41Z", "type": "tool_end", "id": 2, "ok": false, "ms": 11000}),
            json!({"t": "2026-01-01T11:51:15Z", "type": "tool_start", "id": 3, "tool": "edit", "target": "src/adapters/ollama.rs"}),
            json!({"t": "2026-01-01T11:51:16Z", "type": "tool_end", "id": 3, "ok": true, "ms": 10, "lines_added": 12, "lines_removed": 3}),
            json!({"t": "2026-01-01T11:51:40Z", "type": "tool_start", "id": 4, "tool": "bash", "target": "pytest -k timeout"}),
            json!({"t": "2026-01-01T11:52:21Z", "type": "tool_end", "id": 4, "ok": true, "ms": 41000}),
            json!({"t": "2026-01-01T11:52:30Z", "type": "tool_start", "id": 5, "tool": "bash", "target": "pytest -q tests/adapters"}),
        ];
        oscilloscope_core::transcript::build_transcript_view(&lines, false)
    }

    /// [`overview_snapshot_at`], with a specific transcript view
    /// attached the way `main.rs`'s own `tail_focused_transcript`
    /// would feed `draw` -- issue #637's own now-pane goldens for a
    /// running agent need its `doing:`/`tests` lines to come from
    /// somewhere real.
    fn overview_agent_snapshot_at(
        width: u16,
        height: u16,
        state: &Value,
        transcript: Option<&oscilloscope_core::transcript::TranscriptView>,
        process: ProcessState,
    ) -> String {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let rendered = render::build_folded(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(state),
            process,
            372.0,
            "2026-01-01T11:52:55Z",
            &[],
            true,
            &Default::default(),
        );
        let flat = oscilloscope_core::model::flatten_state(state);
        let overview = oscilloscope_core::screens::build_overview(
            &rendered.rows,
            &rendered.unfolded_rows,
            &rendered.header,
            &flat,
            &plan,
            &model,
            Some(state),
            &rendered.token_bar,
            &[],
            None,
            "2026-01-01T11:52:55Z",
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
                    Some(state),
                    &test_ctx,
                    0,
                    "2026-01-01T11:52:55Z",
                    Some(&overview),
                    None,
                    None,
                    None,
                    transcript,
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        terminal.backend().to_string()
    }

    /// [`snapshot_at`], with a specific transcript view attached --
    /// issue #637's own Flow selected-pane goldens for a running
    /// agent.
    fn flow_agent_snapshot_at(
        width: u16,
        height: u16,
        state: &Value,
        transcript: Option<&oscilloscope_core::transcript::TranscriptView>,
        process: ProcessState,
    ) -> String {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let rendered = render::build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(state),
            process,
            372.0,
            "2026-01-01T11:52:55Z",
            &[],
        );
        let flow_screen = oscilloscope_core::screens::build_flow_screen(
            &rendered.rows,
            &rendered.unfolded_rows,
            &oscilloscope_core::model::flatten_state(state),
            &plan,
            &model,
            Some(state),
            &[],
            Some("prime.fix"),
            &test_zone(),
            "2026-01-01T11:52:55Z",
        );
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Flow;
        app.selected_path = Some("prime.fix".to_string());
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
                    None,
                    &[],
                    0,
                    &mut app,
                    Some(state),
                    &test_ctx,
                    0,
                    "2026-01-01T11:52:55Z",
                    None,
                    None,
                    Some(&flow_screen),
                    None,
                    transcript,
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        terminal.backend().to_string()
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
    fn overview_running_at_120x40_with_log_lines() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = overview_running_state();
        let log_lines = vec![
            "00:00:00 engine: started".to_string(),
            "00:00:01 a: ok".to_string(),
        ];
        let text = overview_snapshot_at_with_log(
            120,
            40,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            &log_lines,
        );
        assert!(
            text.contains("a: ok"),
            "the now pane's own log side should show log lines:\n{text}"
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn overview_running_at_60x18() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = overview_running_state();
        let text = overview_snapshot_at(
            60,
            18,
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
    fn overview_finished_at_60x18() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = overview_finished_state();
        let process = ProcessState::exited(false);
        let text = overview_snapshot_at(60, 18, &plan, &mut model, Some(&state), process);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn overview_running_with_history_at_120x40() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = overview_running_state();
        let text = overview_snapshot_at_with_history(
            120,
            40,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            Some("b"),
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn overview_running_with_history_at_70x20() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = overview_running_state();
        let text = overview_snapshot_at_with_history(
            70,
            20,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            Some("b"),
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn overview_running_with_history_at_100x30() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = overview_running_state();
        let text = overview_snapshot_at_with_history(
            100,
            30,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            Some("b"),
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn overview_running_with_history_at_80x24() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = overview_running_state();
        let text = overview_snapshot_at_with_history(
            80,
            24,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            Some("b"),
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn overview_finished_with_history_at_120x40() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = overview_finished_state();
        let text = overview_snapshot_at_with_history(
            120,
            40,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(false),
            None,
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn overview_finished_with_history_at_70x20() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = overview_finished_state();
        let text = overview_snapshot_at_with_history(
            70,
            20,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(false),
            None,
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn overview_finished_with_history_at_100x30() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = overview_finished_state();
        let text = overview_snapshot_at_with_history(
            100,
            30,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(false),
            None,
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn overview_finished_with_history_at_80x24() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = overview_finished_state();
        let text = overview_snapshot_at_with_history(
            80,
            24,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(false),
            None,
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn overview_now_pane_running_agent_at_120x40() {
        let state = agent_running_state();
        let transcript = agent_running_transcript();
        let text =
            overview_agent_snapshot_at(120, 40, &state, Some(&transcript), ProcessState::Running);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn overview_now_pane_running_agent_at_80x24() {
        let state = agent_running_state();
        let transcript = agent_running_transcript();
        let text =
            overview_agent_snapshot_at(80, 24, &state, Some(&transcript), ProcessState::Running);
        insta::assert_snapshot!(text);
    }

    /// Issue #637: an interrupted run's own leftover `meta.live` (no
    /// `completed_at`, a heartbeat long gone stale) reads as
    /// `stopped` in the now pane, not as a live running summary --
    /// the last figures the session actually reported are still
    /// shown underneath, just marked as no longer moving.
    #[test]
    fn overview_now_pane_shows_an_interrupted_agents_leftover_as_stopped() {
        let mut state = agent_running_state();
        state["prime"]["fix"]["meta"]["live"]["updated_at"] =
            Value::String("2026-01-01T11:46:50Z".to_string());
        let text = overview_agent_snapshot_at(120, 40, &state, None, ProcessState::Running);
        assert!(text.contains("stopped"), "{text}");
        assert!(
            text.contains("2.1 Mt"),
            "the last known figures must still show:\n{text}"
        );
    }

    #[test]
    fn overview_now_pane_finished_agent_at_120x40() {
        let state = agent_finished_but_run_still_going_state();
        let text = overview_agent_snapshot_at(120, 40, &state, None, ProcessState::Running);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn overview_now_pane_finished_agent_at_80x24() {
        let state = agent_finished_but_run_still_going_state();
        let text = overview_agent_snapshot_at(80, 24, &state, None, ProcessState::Running);
        insta::assert_snapshot!(text);
    }

    /// Issue #637's acceptance item 2: once the whole run has ended
    /// (not just this one step's own heartbeat gone stale while the
    /// run keeps going, `overview_now_pane_shows_an_interrupted_
    /// agents_leftover_as_stopped` above), the now pane's own
    /// `NowFocus::Ended` names the stopped agent and still shows its
    /// last-known live block.
    #[test]
    fn overview_now_pane_cancelled_agent_at_120x40() {
        let state = agent_cancelled_state();
        let text = overview_agent_snapshot_at(120, 40, &state, None, ProcessState::exited(true));
        assert!(text.contains("stopped"), "{text}");
        assert!(
            text.contains("fix"),
            "the stopped agent must be named:\n{text}"
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn overview_now_pane_cancelled_agent_at_80x24() {
        let state = agent_cancelled_state();
        let text = overview_agent_snapshot_at(80, 24, &state, None, ProcessState::exited(true));
        insta::assert_snapshot!(text);
    }

    #[test]
    fn flow_selected_pane_running_agent_at_120x40() {
        let state = agent_running_state();
        let transcript = agent_running_transcript();
        let text =
            flow_agent_snapshot_at(120, 40, &state, Some(&transcript), ProcessState::Running);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn flow_selected_pane_shows_an_interrupted_agents_leftover_as_stopped() {
        let mut state = agent_running_state();
        state["prime"]["fix"]["meta"]["live"]["updated_at"] =
            Value::String("2026-01-01T11:46:50Z".to_string());
        let text = flow_agent_snapshot_at(120, 40, &state, None, ProcessState::Running);
        assert!(text.contains("stopped"), "{text}");
    }

    #[test]
    fn flow_selected_pane_running_agent_at_80x24() {
        let state = agent_running_state();
        let transcript = agent_running_transcript();
        let text = flow_agent_snapshot_at(80, 24, &state, Some(&transcript), ProcessState::Running);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn flow_selected_pane_finished_agent_at_120x40() {
        let state = agent_finished_state();
        let process = ProcessState::exited(false);
        let text = flow_agent_snapshot_at(120, 40, &state, None, process);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn flow_selected_pane_finished_agent_at_80x24() {
        let state = agent_finished_state();
        let process = ProcessState::exited(false);
        let text = flow_agent_snapshot_at(80, 24, &state, None, process);
        insta::assert_snapshot!(text);
    }

    /// Issue #637's acceptance item 2: the run has ended, but `fix`
    /// is still selected -- its own `FocusedStep` never came from
    /// `NowFocus` at all here (Flow's own selection, issue #635), so
    /// this exercises `flow_selected_step_lines`'s own guard
    /// directly: `fix`'s `RowStatus::kind` reads `Cancelled`, never
    /// `Running`, and must still show `stopped` with its last-known
    /// figures rather than nothing at all.
    #[test]
    fn flow_selected_pane_cancelled_agent_at_120x40() {
        let state = agent_cancelled_state();
        let process = ProcessState::exited(true);
        let text = flow_agent_snapshot_at(120, 40, &state, None, process);
        assert!(text.contains("stopped"), "{text}");
        insta::assert_snapshot!(text);
    }

    #[test]
    fn flow_selected_pane_cancelled_agent_at_80x24() {
        let state = agent_cancelled_state();
        let process = ProcessState::exited(true);
        let text = flow_agent_snapshot_at(80, 24, &state, None, process);
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

    #[test]
    fn overview_failed_at_60x18() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = overview_failed_state();
        let process = ProcessState::exited(false);
        let text = overview_snapshot_at(60, 18, &plan, &mut model, Some(&state), process);
        insta::assert_snapshot!(text);
    }

    /// A two-column size, under [`thresholds::STATE_PANE_MIN_WIDTH`]/
    /// `_MIN_HEIGHT`, so the right column is the log pane alone, no
    /// state pane — most goldens here want this, since a one-column
    /// layout at 70×20 drops the error display, the loop pass rows
    /// and everything but `path` from the details pane, which left
    /// these goldens checking far less than they look like they do.
    /// [`snapshot_one_column`] keeps a couple of them at the smaller
    /// size deliberately, as the
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

    /// [`snapshot_at`], for screen 3 (the Cost screen, issue #632):
    /// builds the Cost screen's own model (the by-store treemap,
    /// the largest-stores table, the tokens-over-time chart) the
    /// same way `main.rs`'s own `build_cost_screen_if_active` does,
    /// only while that screen is on display.
    fn cost_snapshot_at(
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
            process,
            300.0,
            "2026-01-01T00:05:00Z",
            &[],
            true,
            &Default::default(),
        );
        let empty = Value::Null;
        let flat = oscilloscope_core::model::flatten_state(state.unwrap_or(&empty));
        let statuses: std::collections::BTreeMap<_, _> = rendered
            .unfolded_rows
            .iter()
            .map(|r| (r.path.clone(), r.status.clone()))
            .collect();
        let kinds: std::collections::BTreeMap<_, _> = rendered
            .unfolded_rows
            .iter()
            .map(|r| (r.path.clone(), r.kind))
            .collect();
        let cost_screen = oscilloscope_core::screens::build_cost_screen(
            &rendered.rows,
            &rendered.header,
            &flat,
            &statuses,
            &kinds,
            state,
            Measure::Total,
            "2026-01-01T00:05:00Z",
        );
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Cost;
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
        let mut cost_layout = CostLayoutCache::default();
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
                    None,
                    Some(&cost_screen),
                    None,
                    None,
                    None,
                    &mut cost_layout,
                )
            })
            .unwrap();
        terminal.backend().to_string()
    }

    /// Three stores with real tokens/cost (`a`, `b`, and `fan`'s own
    /// running pass `nap`, with no tokens yet -- DESIGN.md's "a tile
    /// for a step that is still running reads tokens when done"),
    /// plenty for the treemap, the chart and the table to all show
    /// something real.
    fn cost_running_state() -> Value {
        json!({"prime": {"value": null, "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": null},
            "a": {"value": "ok", "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:01:00Z", "adapter": "scripted", "tokens_sent_total": 900, "tokens_received_total": 400, "cost_usd": 1.2}},
            "b": {"value": "ok", "meta": {"created_at": "2026-01-01T00:01:00Z", "completed_at": "2026-01-01T00:02:00Z", "adapter": "scripted", "tokens_sent_total": 300, "tokens_received_total": 150}},
            "fan": {"value": null, "meta": {"completed_at": null, "progress": {"done": 0, "total": 2}},
                "iter_0": {"value": null, "meta": {"completed_at": null},
                    "nap": {"value": null, "meta": {"created_at": "2026-01-01T00:02:00Z", "completed_at": null, "adapter": "scripted"}}
                }
            }
        }})
    }

    fn cost_finished_state() -> Value {
        json!({
            "runtime": {"last_run": {"completed_at": "2026-01-01T00:05:00Z", "totals": {"wall_time_s": 300.0, "effects_run": 3, "tokens_sent": 1300, "tokens_received": 600, "cost_usd": 1.2}}},
            "prime": {"value": true, "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:05:00Z", "error": null},
            "a": {"value": "ok", "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:01:00Z", "adapter": "scripted", "tokens_sent_total": 900, "tokens_received_total": 400, "cost_usd": 1.2}},
            "b": {"value": "ok", "meta": {"created_at": "2026-01-01T00:01:00Z", "completed_at": "2026-01-01T00:02:00Z", "adapter": "scripted", "tokens_sent_total": 300, "tokens_received_total": 150}},
            "fan": {"value": null, "meta": {"completed_at": "2026-01-01T00:05:00Z", "progress": {"done": 2, "total": 2}},
                "iter_0": {"value": null, "meta": {"completed_at": "2026-01-01T00:03:00Z"},
                    "nap": {"value": "ok", "meta": {"created_at": "2026-01-01T00:02:00Z", "completed_at": "2026-01-01T00:03:00Z", "adapter": "scripted", "tokens_sent_total": 50, "tokens_received_total": 50}}
                },
                "iter_1": {"value": null, "meta": {"completed_at": "2026-01-01T00:05:00Z"},
                    "nap": {"value": "ok", "meta": {"created_at": "2026-01-01T00:04:00Z", "completed_at": "2026-01-01T00:05:00Z", "adapter": "scripted", "tokens_sent_total": 50, "tokens_received_total": 50}}
                }
            }
        }})
    }

    #[test]
    fn cost_running_at_120x40() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = cost_running_state();
        let text = cost_snapshot_at(
            120,
            40,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
        );
        insta::assert_snapshot!(text);
    }

    /// Issue #637: a running agent's own tile, sized and labelled
    /// from `meta.live.tokens_sent` -- `tokens when done` never
    /// appears for it, unlike every other running store.
    fn cost_agent_running_state() -> Value {
        json!({"prime": {"value": null, "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": null},
            "fix": {"value": null, "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": null,
                "provider": "agent", "live": {
                    "tokens_sent": 2100000,
                    "turn_tokens": [400000, 900000, 600000, 2100000],
                    "tokens_cached_in": 1800000
                }}}
        }})
    }

    fn cost_agent_finished_state() -> Value {
        json!({"prime": {"value": null, "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": "t"},
            "fix": {"value": "done", "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": "t",
                "provider": "agent", "tokens_sent": 2100000, "tokens_received": 18000, "cost_usd": 1.10}}
        }})
    }

    #[test]
    fn cost_tile_running_agent_at_120x40() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = cost_agent_running_state();
        let text = cost_snapshot_at(
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
    fn cost_tile_running_agent_at_80x24() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = cost_agent_running_state();
        let text = cost_snapshot_at(
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
    fn cost_tile_finished_agent_at_120x40() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = cost_agent_finished_state();
        let process = ProcessState::exited(false);
        let text = cost_snapshot_at(120, 40, &plan, &mut model, Some(&state), process);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn cost_tile_finished_agent_at_80x24() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = cost_agent_finished_state();
        let process = ProcessState::exited(false);
        let text = cost_snapshot_at(80, 24, &plan, &mut model, Some(&state), process);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn cost_finished_at_120x40() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = cost_finished_state();
        let process = ProcessState::exited(true);
        let text = cost_snapshot_at(120, 40, &plan, &mut model, Some(&state), process);
        insta::assert_snapshot!(text);
    }

    /// Issue #632's own small-terminal acceptance item: at 120x40 the
    /// table pane sits beside the chart (DESIGN.md's 62/38 split,
    /// narrower than the full screen width), and the coverage label
    /// must still show in full there, on its own line, not truncated
    /// mid-word the way sharing the totals line's own width used to
    /// leave it (`$1.20 (from 1 of 4 pr…`).
    #[test]
    fn the_full_cost_coverage_label_shows_at_120x40() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = cost_finished_state();
        let process = ProcessState::exited(true);
        let text = cost_snapshot_at(120, 40, &plan, &mut model, Some(&state), process);
        assert!(
            text.contains("$1.20 (from 1 of 4 prompts)"),
            "the full coverage label did not appear:\n{text}"
        );
    }

    #[test]
    fn cost_running_at_100x30() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = cost_running_state();
        let text = cost_snapshot_at(
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
    fn cost_finished_at_100x30() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = cost_finished_state();
        let process = ProcessState::exited(true);
        let text = cost_snapshot_at(100, 30, &plan, &mut model, Some(&state), process);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn cost_running_at_80x24() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = cost_running_state();
        let text = cost_snapshot_at(
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
    fn cost_finished_at_80x24() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = cost_finished_state();
        let process = ProcessState::exited(true);
        let text = cost_snapshot_at(80, 24, &plan, &mut model, Some(&state), process);
        insta::assert_snapshot!(text);
    }

    /// 80×20 itself still qualifies for the treemap and the table
    /// (`COST_TREEMAP_MIN_WIDTH`/`_MIN_HEIGHT` are inclusive, checked
    /// against the whole terminal) -- only below it does the table
    /// show alone.
    #[test]
    fn cost_running_at_80x20() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = cost_running_state();
        let text = cost_snapshot_at(
            80,
            20,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn cost_finished_at_80x20() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = cost_finished_state();
        let process = ProcessState::exited(true);
        let text = cost_snapshot_at(80, 20, &plan, &mut model, Some(&state), process);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn cost_running_at_70x20() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = cost_running_state();
        let text = cost_snapshot_at(
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
    fn cost_finished_at_70x20() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = cost_finished_state();
        let process = ProcessState::exited(true);
        let text = cost_snapshot_at(70, 20, &plan, &mut model, Some(&state), process);
        insta::assert_snapshot!(text);
    }

    /// Below [`thresholds::COST_TREEMAP_MIN_WIDTH`]/`_MIN_HEIGHT`
    /// (DESIGN.md: "the table alone, where ⏎ still zooms") the table
    /// is the only thing on screen -- it must still honour a zoom
    /// already in effect (`app.cost_zoom`) and mark the selected row,
    /// since nothing else on screen can show either any more.
    #[test]
    fn the_table_alone_still_reflects_zoom_and_selection() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = cost_finished_state();
        let rendered = render::build_folded(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(true),
            300.0,
            "2026-01-01T00:05:00Z",
            &[],
            true,
            &Default::default(),
        );
        let flat = oscilloscope_core::model::flatten_state(&state);
        let statuses: std::collections::BTreeMap<_, _> = rendered
            .unfolded_rows
            .iter()
            .map(|r| (r.path.clone(), r.status.clone()))
            .collect();
        let kinds: std::collections::BTreeMap<_, _> = rendered
            .unfolded_rows
            .iter()
            .map(|r| (r.path.clone(), r.kind))
            .collect();
        let cost_screen = oscilloscope_core::screens::build_cost_screen(
            &rendered.rows,
            &rendered.header,
            &flat,
            &statuses,
            &kinds,
            Some(&state),
            Measure::Total,
            "2026-01-01T00:05:00Z",
        );
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Cost;
        app.cost_zoom = vec!["prime.fan".to_string()];
        app.cost_selected = Some("prime.fan.iter_0.nap".to_string());
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
        let mut cost_layout = CostLayoutCache::default();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    None,
                    &[],
                    0,
                    &mut app,
                    Some(&state),
                    &test_ctx,
                    0,
                    "2026-01-01T00:05:00Z",
                    None,
                    Some(&cost_screen),
                    None,
                    None,
                    None,
                    &mut cost_layout,
                )
            })
            .unwrap();
        let text = terminal.backend().to_string();
        assert!(text.contains("iter_0"), "{text}");
        assert!(
            !text.contains("a  ↑"),
            "zoomed into fan, a must not show: {text}"
        );
        assert!(
            !text.contains("b  ↑"),
            "zoomed into fan, b must not show: {text}"
        );
        assert!(
            text.contains("iter_0 › nap"),
            "zoomed into fan, iter_0's row must read relative to it: {text}"
        );
        assert!(
            text.contains("iter_1 › nap"),
            "zoomed into fan, iter_1's row must read relative to it: {text}"
        );
        assert!(
            !text.contains("fan ›"),
            "zoomed into fan, no row may still carry fan's own name as a prefix: {text}"
        );
        let buffer = terminal.backend().buffer();
        let tinted = (0..buffer.area.height).any(|y| {
            (0..buffer.area.width).any(|x| buffer[(x, y)].modifier.contains(Modifier::REVERSED))
        });
        assert!(tinted, "the selected row must be reversed: {text}");
    }

    #[test]
    fn pending_plan() {
        // Kept at 70×20 deliberately, as one of this suite's own
        // one-column-fallback goldens.
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
        let flow_screen = oscilloscope_core::screens::build_flow_screen(
            &rendered.rows,
            &rendered.unfolded_rows,
            &oscilloscope_core::model::flatten_state(&state),
            &plan,
            &model,
            Some(&state),
            &[],
            app.selected_path.as_deref(),
            &test_zone(),
            "2026-01-01T00:00:00Z",
        );
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
                    None,
                    Some(&flow_screen),
                    None,
                    None,
                    &mut CostLayoutCache::default(),
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
                    None,
                    None,
                    None,
                    None,
                    &mut CostLayoutCache::default(),
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
    fn the_help_overlay_fits_at_80x24_without_losing_its_last_line() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let rendered = render::build_folded(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            None,
            ProcessState::Running,
            1.0,
            "2026-01-01T00:00:00Z",
            &[],
            true,
            &std::collections::BTreeSet::new(),
        );
        let mut app = App::new(false);
        app.dialog = Dialog::Help;
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
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
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
                    &test_ctx,
                    0,
                    "2026-01-01T00:00:00Z",
                    None,
                    None,
                    None,
                    None,
                    None,
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        let text = terminal.backend().to_string();
        assert!(text.contains("press any key to close"), "{text}");
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
        let flow_screen = oscilloscope_core::screens::build_flow_screen(
            &rendered.rows,
            &rendered.unfolded_rows,
            &oscilloscope_core::model::flatten_state(&state),
            &plan,
            &RunModel::new(),
            Some(&state),
            &[],
            Some("prime.q1"),
            &test_zone(),
            "2026-01-01T11:46:50.000Z",
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
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Flow;
        app.selected_path = Some("prime.q1".to_string());
        app.follow = false;
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    None,
                    &[],
                    0,
                    &mut app,
                    Some(&state),
                    &test_ctx,
                    0,
                    "2026-01-01T00:00:00Z",
                    None,
                    None,
                    Some(&flow_screen),
                    None,
                    None,
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        let text = terminal.backend().to_string();
        // Issue #635: the selected pane reuses the overview's own
        // `FocusedStep::kind_label`, not the old details pane's
        // `type   X` line -- selecting the folded quick-steps row
        // still shows its own kind there.
        assert!(text.contains("quick steps"), "{text}");
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
            engine: None,
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
        let story_lines = vec![oscilloscope_core::diff::LogLine {
            ts: Some("2026-01-01T11:04:58Z".to_string()),
            text: "▸ warning: behind".to_string(),
            kind: oscilloscope_core::diff::LogKind::Report,
            path: Some("prime".to_string()),
        }];
        let flow_screen = oscilloscope_core::screens::build_flow_screen(
            &rendered.rows,
            &rendered.unfolded_rows,
            &oscilloscope_core::model::flatten_state(&state),
            &plan,
            &model,
            Some(&state),
            &story_lines,
            Some("prime.a"),
            &test_zone(),
            "2026-01-01T00:00:00Z",
        );
        let backend = TestBackend::new(100, 30);
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
                    None,
                    Some(&flow_screen),
                    None,
                    None,
                    &mut CostLayoutCache::default(),
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
        // Issue #632 removed screen 2's own token bar (the previous
        // end anchor here, a full-width strip below every column so
        // it could never recur earlier) -- the orchestration pane is
        // now the last thing screen 2 draws, so everything after its
        // own title is in scope, and nothing later in the buffer
        // (the state/log columns' own trailing blank padding) has a
        // `status` of its own to false-positive on.
        let orchestration_pane = text
            .split_once("flow (do-thing")
            .map(|(_, after)| after)
            .expect("the flow pane should be in the snapshot");
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
        // The other one-column-fallback golden kept at 70×20.
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

    /// Issue #527's own full layout (now, post-#632, the state pane
    /// over the log pane — the `tokens by key` treemap and the token
    /// bar moved to the Cost screen), on a terminal big enough for
    /// both: the state pane's own nested loop pass (`fan`'s own
    /// container row holding `iter_0`), together with the
    /// orchestration pane's `(following)` title.
    #[test]
    fn full_layout_with_state_pane_following() {
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

    /// [`sample_plan`]'s own `a` is a tool, not a prompt -- selecting
    /// it here would read as `RowKind::Tool` regardless of any
    /// `prompt_sent`/`tokens_*` meta the state carries, since the
    /// plan always wins over a meta-shape guess, leaving this golden
    /// with no `sent`/`received` blocks to actually show at all. A
    /// plan whose `a` really is a prompt is the one thing this test
    /// needs beyond `sample_plan`'s own shape.
    fn prompt_a_tool_b_plan() -> PlanTree {
        let root_path = EffectPath::root();
        let a_path = root_path.clone().push_name("a");
        let b_path = root_path.clone().push_name("b");
        let root = Op {
            path: root_path,
            name: Some("prime".to_string()),
            kind: NodeKind::Control(Region::Block {
                ops: vec![prompt(a_path, "a"), tool(b_path, "b", OnError::Continue)],
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

    /// A selected, finished prompt at 120×40 -- the
    /// `sent`/`received` blocks must land right after `path`/`type`,
    /// before `plan`/the meta list, and each snippet must cap at
    /// [`SNIPPET_MAX_LINES`] wrapped lines so both blocks stay on
    /// screen together rather than one pushing the other past the
    /// details pane's own golden-section height.
    #[test]
    fn full_layout_with_a_selected_finished_prompts_sent_received_blocks() {
        let plan = prompt_a_tool_b_plan();
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

    /// Issue #635: a selected prompt that hasn't replied yet shows
    /// `asked:` with its own sent snippet, then `doing:` with
    /// "waiting for the reply" and its own in-flight token count --
    /// `produced:` reads "(when done)" rather than any real value,
    /// since nothing has come back yet.
    #[test]
    fn full_layout_with_a_selected_running_prompt_shows_asked_and_waiting_for_the_reply() {
        let plan = prompt_a_tool_b_plan();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "adapter": "scripted",
                "prompt_sent": "summarize the quarterly report", "tokens_sent_total": 42}}
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
        assert!(text.contains("asked:"), "{text}");
        assert!(text.contains("waiting for the reply"), "{text}");
        assert!(text.contains("(when done)"), "{text}");
        insta::assert_snapshot!(text);
    }

    /// Finding F1: the two boundary sizes between the full layout and
    /// the one-column fallback, each exercising a different tier
    /// [`thresholds`] documents -- read alongside
    /// [`full_layout_with_state_pane_following`] (120×40) and
    /// [`pending_plan`]/[`aborted_no_final_state`] (70×20, below
    /// `TWO_COLUMN_MIN_WIDTH`/`_MIN_HEIGHT`). Issue #632 removed
    /// `TREEMAP_MIN_WIDTH`/`_HEIGHT` from screen 2 entirely, so 100×30
    /// no longer sits on a tier boundary of its own — it still pins
    /// a comfortably-sized two-column layout with the state pane
    /// shown, same as any size at or above `STATE_PANE_MIN_WIDTH`/
    /// `_HEIGHT`.
    #[test]
    fn two_column_layout_at_100x30_shows_the_state_pane() {
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
    /// are met), but below `STATE_PANE_MIN_WIDTH` (90) -- the right
    /// column is the log pane alone.
    #[test]
    fn fallback_at_80x24_drops_the_state_pane() {
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
        let flow_screen = oscilloscope_core::screens::build_flow_screen(
            &rendered.rows,
            &rendered.unfolded_rows,
            &oscilloscope_core::model::flatten_state(&state),
            &plan,
            &model,
            Some(&state),
            &[],
            app.selected_path.as_deref(),
            &test_zone(),
            "2026-01-01T00:00:00Z",
        );
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
                    None,
                    Some(&flow_screen),
                    None,
                    None,
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        let text = terminal.backend().to_string();
        assert!(
            flow_screen
                .selected
                .as_ref()
                .is_some_and(|s| s.path == "prime.fan.iter_0"),
            "the selected pane must show the row follow actually picked: {:?}",
            flow_screen.selected
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
        let flow_screen = oscilloscope_core::screens::build_flow_screen(
            &rendered.rows,
            &rendered.unfolded_rows,
            &oscilloscope_core::model::flatten_state(&state),
            &plan,
            &model,
            Some(&state),
            &[],
            app.selected_path.as_deref(),
            &test_zone(),
            "2026-01-01T00:00:00Z",
        );
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
                    None,
                    Some(&flow_screen),
                    None,
                    None,
                    &mut CostLayoutCache::default(),
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
        // Issue #637: `n0` is a running agent whose transcript is a
        // real file on disk, so the Flow pane's own selection below
        // exercises transcript tailing too, not only the 2,000-node
        // redraw itself -- the one other thing this same budget must
        // still cover.
        let transcript_dir = tempfile::tempdir().unwrap();
        let transcript_path = transcript_dir.path().join("transcript.jsonl");
        std::fs::write(
            &transcript_path,
            concat!(
                r#"{"t": "t0", "type": "turn", "label": "turn", "session_id": "s1"}"#, "\n",
                r#"{"t": "t0", "type": "tool_start", "id": 1, "tool": "read", "target": "PLAN.md"}"#, "\n",
                r#"{"t": "t0", "type": "tool_end", "id": 1, "ok": true, "ms": 5}"#, "\n",
            ),
        )
        .unwrap();

        let mut prime = serde_json::Map::new();
        prime.insert("value".to_string(), Value::Null);
        let mut prime_meta = serde_json::Map::new();
        prime_meta.insert("completed_at".to_string(), Value::Null);
        prime.insert("meta".to_string(), Value::Object(prime_meta));
        for i in 0..2000 {
            let mut meta = serde_json::Map::new();
            meta.insert("created_at".to_string(), Value::String(format!("t{i}")));
            if i == 0 {
                meta.insert("completed_at".to_string(), Value::Null);
                meta.insert("provider".to_string(), Value::String("agent".to_string()));
                meta.insert(
                    "live".to_string(),
                    serde_json::json!({
                        "engine": "pi", "model": "sonnet", "turns": 1, "tool_calls": 1,
                        "transcript": transcript_path.to_string_lossy(),
                        "started_at": "t0", "updated_at": "t2001",
                        "tokens_sent": 100
                    }),
                );
            } else {
                meta.insert(
                    "completed_at".to_string(),
                    Value::String(format!("t{}", i + 1)),
                );
                meta.insert("provider".to_string(), Value::String("shell".to_string()));
            }
            // Issue #632: also token-bearing by the Cost screen's own
            // rule (`adapter` present), so this same 2,000-node state
            // doubles as that screen's own timing coverage below --
            // every node here is a real store for it to build.
            meta.insert("adapter".to_string(), Value::String("scripted".to_string()));
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

        // Issue #635: a real plan with an unnamed `if` of its own
        // (never `PlanTree::empty()`, which skips `build_flow_rows`'
        // own decision-inference work entirely, timing nothing of it
        // at all) -- its own `then`/`else` paths don't need to match
        // any of the 2,000 `n{i}` nodes below for the timing to be
        // real, only for `unnamed_if_decisions` to have something in
        // it for `resolve_unnamed_decisions`/`push_unnamed_decision`
        // to actually walk.
        let plan = {
            let root_path = EffectPath::root();
            let if_path = root_path.clone();
            let foo_path = if_path.clone().push_name("foo");
            let root = Op {
                path: root_path,
                name: Some("prime".to_string()),
                kind: NodeKind::Control(Region::Block {
                    ops: vec![Op {
                        path: if_path,
                        name: None,
                        kind: NodeKind::Control(Region::If {
                            cond: Condition::Cel {
                                expr: "true".to_string(),
                                strict: false,
                            },
                            then_: Box::new(Region::Block {
                                ops: vec![tool(foo_path, "foo", OnError::Fail)],
                                overlay: true,
                            }),
                            else_: None,
                            threshold: 0.5,
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
            PlanTree::from_program(&Program {
                root,
                prompts: Default::default(),
                effect_names: Default::default(),
                document: None,
                runtime_block: None,
                interface: None,
                adapter: None,
                model: None,
            })
        };
        // Issue #635: a real 2,000-line story -- `build_story`'s own
        // sort (`build_flow_rows` is timed together with it below) is
        // the other thing an empty story timed nothing of at all.
        let story_2000: Vec<oscilloscope_core::diff::LogLine> = (0..2000)
            .map(|i| oscilloscope_core::diff::LogLine {
                ts: Some(format!("2026-01-01T00:00:{:02}Z", i % 60)),
                text: format!("▸ report {i}"),
                kind: oscilloscope_core::diff::LogKind::Report,
                path: None,
            })
            .collect();
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
        // Issue #635: the Flow screen's own model (decisions, loops,
        // quick steps, the story) is built only while it's actually
        // on display, same performance rule as the overview's and the
        // Cost screen's below -- timed together with the draw itself,
        // on the same 2,000-node state.
        let start = std::time::Instant::now();
        let flow_screen = oscilloscope_core::screens::build_flow_screen(
            &rendered.rows,
            &rendered.unfolded_rows,
            &oscilloscope_core::model::flatten_state(&state),
            &plan,
            &model,
            Some(&state),
            &story_2000,
            Some("prime.n0"),
            &test_zone(),
            "t2001",
        );
        // Issue #637: the step on screen (`n0`, Flow's own selection
        // here) gets its transcript tailed every frame -- timed
        // together with everything else above, so a real 64 KiB-
        // bounded read is part of this same budget, not a cost the
        // 2,000-node measurement never actually pays.
        let mut transcript_cache = crate::TranscriptTailCache::default();
        let transcript_view = crate::tail_focused_transcript(
            &app,
            None,
            Some(&flow_screen),
            None,
            &mut transcript_cache,
        );
        assert!(
            transcript_view
                .as_ref()
                .is_some_and(|v| !v.calls.is_empty()),
            "n0's own transcript must actually have been tailed"
        );
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
                    None,
                    Some(&flow_screen),
                    None,
                    transcript_view.as_ref(),
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "building and drawing the Flow screen for a 2000-node state took {elapsed:?}, over 5x the 100ms REDRAW_INTERVAL"
        );

        // The overview's own model is built (and drawn) only while
        // it's actually on display -- so the same budget applies to
        // it, on the same 2000-node state, not just to screen 2's.
        let flat = oscilloscope_core::model::flatten_state(&state);
        let overview_start = std::time::Instant::now();
        let overview = oscilloscope_core::screens::build_overview(
            &rendered.rows,
            &rendered.unfolded_rows,
            &rendered.header,
            &flat,
            &plan,
            &model,
            Some(&state),
            &rendered.token_bar,
            &[],
            Some("n0"),
            "t2001",
        );
        let mut overview_app = App::new(false);
        overview_app.screen = oscilloscope_core::screens::Screen::Overview;
        let overview_backend = TestBackend::new(120, 40);
        let mut overview_terminal = Terminal::new(overview_backend).unwrap();
        overview_terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    None,
                    &[],
                    0,
                    &mut overview_app,
                    Some(&state),
                    &test_ctx,
                    0,
                    "2026-01-01T00:00:00Z",
                    Some(&overview),
                    None,
                    None,
                    None,
                    None,
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        let overview_elapsed = overview_start.elapsed();
        assert!(
            overview_elapsed < std::time::Duration::from_millis(500),
            "building and drawing the overview for a 2000-node state took {overview_elapsed:?}, over 5x the 100ms REDRAW_INTERVAL"
        );

        // Issue #632: the Cost screen's own by-store tree is built (and
        // laid out) only while it is actually on display, same
        // performance rule as the overview's above -- on the same
        // 2,000-node state (every node a real store, see the loop
        // above), not a smaller one of its own.
        let statuses: std::collections::BTreeMap<_, _> = rendered
            .unfolded_rows
            .iter()
            .map(|r| (r.path.clone(), r.status.clone()))
            .collect();
        let kinds: std::collections::BTreeMap<_, _> = rendered
            .unfolded_rows
            .iter()
            .map(|r| (r.path.clone(), r.kind))
            .collect();
        let cost_start = std::time::Instant::now();
        let cost_screen = oscilloscope_core::screens::build_cost_screen(
            &rendered.rows,
            &rendered.header,
            &flat,
            &statuses,
            &kinds,
            Some(&state),
            Measure::Total,
            "t2001",
        );
        let mut cost_app = App::new(false);
        cost_app.screen = oscilloscope_core::screens::Screen::Cost;
        let cost_backend = TestBackend::new(120, 40);
        let mut cost_terminal = Terminal::new(cost_backend).unwrap();
        let mut cost_layout = CostLayoutCache::default();
        cost_terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    None,
                    &[],
                    0,
                    &mut cost_app,
                    Some(&state),
                    &test_ctx,
                    0,
                    "2026-01-01T00:00:00Z",
                    None,
                    Some(&cost_screen),
                    None,
                    None,
                    None,
                    &mut cost_layout,
                )
            })
            .unwrap();
        let cost_elapsed = cost_start.elapsed();
        assert!(
            cost_elapsed < std::time::Duration::from_millis(500),
            "building and drawing the Cost screen for a 2000-node state took {cost_elapsed:?}, over 5x the 100ms REDRAW_INTERVAL"
        );

        // Re-laying out the identical tree at the identical area must
        // be effectively free -- `CostLayoutCache` skips `squarify`
        // entirely when neither has changed (DESIGN.md's performance
        // rule: "laid out again only when the tree or the area
        // changes").
        let cached_start = std::time::Instant::now();
        cost_terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    None,
                    &[],
                    0,
                    &mut cost_app,
                    Some(&state),
                    &test_ctx,
                    0,
                    "2026-01-01T00:00:00Z",
                    None,
                    Some(&cost_screen),
                    None,
                    None,
                    None,
                    &mut cost_layout,
                )
            })
            .unwrap();
        let cached_elapsed = cached_start.elapsed();
        assert!(
            cached_elapsed < std::time::Duration::from_millis(100),
            "redrawing the Cost screen with nothing changed took {cached_elapsed:?} -- the layout cache should have skipped squarify entirely"
        );

        // Issue #650: the Time screen's own model and drawing are
        // built only while screen 1 is actually on display, same
        // performance rule as every other screen above -- on the
        // same 2,000-node state, and only the rows actually in view
        // (120x40's own viewport) are drawn.
        let time_start = std::time::Instant::now();
        let time_screen = oscilloscope_core::screens::build_time_screen(
            &rendered.rows,
            &flat,
            &plan,
            &model,
            &rendered.header,
            Some(&state),
            &story_2000,
            Some("n0"),
            &Default::default(),
            "t2001",
        );
        let mut time_app = App::new(false);
        time_app.screen = oscilloscope_core::screens::Screen::Time;
        time_app.selected_path = Some("n0".to_string());
        let time_backend = TestBackend::new(120, 40);
        let mut time_terminal = Terminal::new(time_backend).unwrap();
        time_terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    None,
                    &[],
                    0,
                    &mut time_app,
                    Some(&state),
                    &test_ctx,
                    0,
                    "2026-01-01T00:00:00Z",
                    None,
                    None,
                    None,
                    Some(&time_screen),
                    None,
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        let time_elapsed = time_start.elapsed();
        assert!(
            time_elapsed < std::time::Duration::from_millis(500),
            "building and drawing the Time screen for a 2000-node state took {time_elapsed:?}, over 5x the 100ms REDRAW_INTERVAL"
        );
    }

    /// [`redraw_from_a_2000_node_state_...`]'s own 2,000-node state
    /// uses `"t{i}"` timestamps, which `duration_seconds` can't
    /// parse at all -- every bar and strip in that test's own Time
    /// screen is blank, so its timing never actually exercises
    /// `bar_cells`/`bucket_weighted`/`in_flight_heights` at scale.
    /// This one uses real RFC 3339 clock times instead, one second
    /// apart, so the bars and strips really do get built.
    #[test]
    fn time_screen_performance_on_a_2000_node_state_with_real_clock_times() {
        let plan = PlanTree::empty();
        let mut prime = serde_json::Map::new();
        prime.insert("value".to_string(), Value::Null);
        let mut prime_meta = serde_json::Map::new();
        prime_meta.insert(
            "created_at".to_string(),
            Value::String("2026-01-01T00:00:00Z".to_string()),
        );
        prime_meta.insert("completed_at".to_string(), Value::Null);
        prime.insert("meta".to_string(), Value::Object(prime_meta));
        for i in 0..2000 {
            let mut meta = serde_json::Map::new();
            meta.insert(
                "created_at".to_string(),
                Value::String(format!("2026-01-01T00:{:02}:{:02}Z", (i / 60) % 60, i % 60)),
            );
            if i == 1999 {
                meta.insert("completed_at".to_string(), Value::Null);
                meta.insert("provider".to_string(), Value::String("shell".to_string()));
            } else {
                meta.insert(
                    "completed_at".to_string(),
                    Value::String(format!(
                        "2026-01-01T00:{:02}:{:02}Z",
                        ((i + 1) / 60) % 60,
                        (i + 1) % 60
                    )),
                );
                meta.insert("provider".to_string(), Value::String("shell".to_string()));
            }
            let mut node = serde_json::Map::new();
            node.insert("value".to_string(), Value::String(format!("ok{i}\n")));
            node.insert("meta".to_string(), Value::Object(meta));
            prime.insert(format!("n{i}"), Value::Object(node));
        }
        let mut root = serde_json::Map::new();
        root.insert("prime".to_string(), Value::Object(prime));
        let state = Value::Object(root);

        let mut model = RunModel::new();
        let now = "2026-01-01T00:33:20Z";
        let rendered = render::build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            2000.0,
            now,
            &[],
        );
        let flat = oscilloscope_core::model::flatten_state(&state);
        let start = std::time::Instant::now();
        let time_screen = oscilloscope_core::screens::build_time_screen(
            &rendered.rows,
            &flat,
            &plan,
            &model,
            &rendered.header,
            Some(&state),
            &[],
            Some("prime.n1999"),
            &Default::default(),
            now,
        );
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Time;
        app.selected_path = Some("prime.n1999".to_string());
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
                    None,
                    &[],
                    0,
                    &mut app,
                    Some(&state),
                    &test_ctx,
                    0,
                    now,
                    None,
                    None,
                    None,
                    Some(&time_screen),
                    None,
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        let elapsed = start.elapsed();
        let text = terminal.backend().to_string();
        assert!(
            text.contains('█') || text.contains('▶'),
            "a real-clock state must actually draw a bar, or this timing proves nothing:\n{text}"
        );
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "building and drawing the Time screen for a 2000-node real-clock state took {elapsed:?}, over 5x the 100ms REDRAW_INTERVAL"
        );
    }

    /// `RunModel::step_interval_offsets`'s own cache parses each
    /// logged interval's timestamps once, not four RFC 3339 parses
    /// per interval (`run_start_ts` included, twice over) on every
    /// redraw -- this is that cache's own 50,000-entry cap.
    #[test]
    fn time_screen_performance_with_50_000_logged_intervals() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let base = chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z").unwrap();
        for i in 0..50_000i64 {
            let start = (base + chrono::Duration::milliseconds(10 * i))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            let end = (base + chrono::Duration::milliseconds(10 * i + 5))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            model.observe_event(&Event::Start {
                ts: start,
                id: Some(i),
                path: "prime.nap".to_string(),
            });
            model.observe_event(&Event::End {
                ts: end,
                id: Some(i),
                path: "prime.nap".to_string(),
                ok: true,
                ms: Some(5),
                error: None,
            });
        }
        assert_eq!(model.step_intervals().len(), 50_000);
        let state = json!({"prime": {"value": null, "meta": {
            "created_at": "2026-01-01T00:00:00Z", "completed_at": null
        }}});
        let now = (base + chrono::Duration::milliseconds(10 * 50_000))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let rendered = render::build_folded(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            500.0,
            &now,
            &[],
            true,
            &Default::default(),
        );
        let flat = oscilloscope_core::model::flatten_state(&state);
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Time;
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
        // Three redraws, the same way a real TUI session's own poll
        // loop would call `build_time_screen` again on every tick --
        // the cache only pays off from the second one on.
        let start_time = std::time::Instant::now();
        for _ in 0..3 {
            let time_screen = oscilloscope_core::screens::build_time_screen(
                &rendered.rows,
                &flat,
                &plan,
                &model,
                &rendered.header,
                Some(&state),
                &[],
                None,
                &Default::default(),
                &now,
            );
            terminal
                .draw(|f| {
                    draw(
                        f,
                        &rendered,
                        None,
                        &[],
                        0,
                        &mut app,
                        Some(&state),
                        &test_ctx,
                        0,
                        &now,
                        None,
                        None,
                        None,
                        Some(&time_screen),
                        None,
                        &mut CostLayoutCache::default(),
                    )
                })
                .unwrap();
        }
        let elapsed = start_time.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "building the strips and drawing three times over a 50,000-interval log took {elapsed:?}, a generous bound even for a debug build"
        );
    }

    /// A tree-loop pass's prompt never
    /// appears in live state at all while it runs (DESIGN.md §1.4), on
    /// either engine -- `fan.iter_0.ask` here has no node of its own in
    /// `state`, confirmed running only by its own `start` event. The
    /// overview's own cost card (issue #632 moved the in-flight line
    /// there from screen 2's own token bar, which no longer exists)
    /// must still show it running rather than "nothing in flight" --
    /// and, issue #631's own fix, as a plain "1 prompt in flight"
    /// rather than "now in ≈0+" or "now in ?": there is no
    /// `prompt_sent` to estimate from at all here, so there is
    /// nothing to put a number on.
    #[test]
    fn overview_cost_card_shows_a_running_tree_loop_prompt_in_flight() {
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
        let rendered = render::build_folded(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            5.0,
            "2026-01-01T00:00:05Z",
            &[],
            true,
            &Default::default(),
        );
        assert_eq!(rendered.token_bar.prompts_running, 1);
        assert!(rendered.token_bar.estimated_input_is_lower_bound);

        let flat = oscilloscope_core::model::flatten_state(&state);
        let overview = oscilloscope_core::screens::build_overview(
            &rendered.rows,
            &rendered.unfolded_rows,
            &rendered.header,
            &flat,
            &plan,
            &model,
            Some(&state),
            &rendered.token_bar,
            &[],
            None,
            "2026-01-01T00:00:05Z",
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
        let backend = TestBackend::new(120, 40);
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
                    Some(&state),
                    &test_ctx,
                    0,
                    "2026-01-01T00:00:00Z",
                    Some(&overview),
                    None,
                    None,
                    None,
                    None,
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        let text = terminal.backend().to_string();
        assert!(
            !text.contains("nothing in flight"),
            "a running tree-loop prompt must show in the cost card:\n{text}"
        );
        assert!(
            text.contains("1 prompt in flight") && !text.contains("now in"),
            "an estimate with nothing known at all must say only what's known, not `now in ≈0+`/`now in ?`:\n{text}"
        );
        insta::assert_snapshot!(text);
    }

    /// Issue #631: the flow card's own effects line reads "1 quick
    /// step" for exactly one, not "1 quick steps".
    #[test]
    fn flow_card_quick_steps_is_singular_for_exactly_one() {
        let card = oscilloscope_core::screens::FlowCard {
            quick_steps: Some((1, 0.3)),
            ..Default::default()
        };
        let test_theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let test_tracker = crate::highlight::Tracker::new();
        let ctx = DrawCtx {
            theme: &test_theme,
            highlights: &test_tracker,
            now: Instant::now(),
            color_mode: ColorMode::TrueColor,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                draw_flow_card(f, area, &card, &ctx);
            })
            .unwrap();
        let text = terminal.backend().to_string();
        assert!(text.contains("1 quick step"), "{text}");
        assert!(!text.contains("1 quick steps"), "{text}");
    }

    /// Issue #631: two or more still read the plural.
    #[test]
    fn flow_card_quick_steps_is_plural_for_more_than_one() {
        let card = oscilloscope_core::screens::FlowCard {
            quick_steps: Some((3, 0.9)),
            ..Default::default()
        };
        let test_theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let test_tracker = crate::highlight::Tracker::new();
        let ctx = DrawCtx {
            theme: &test_theme,
            highlights: &test_tracker,
            now: Instant::now(),
            color_mode: ColorMode::TrueColor,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                draw_flow_card(f, area, &card, &ctx);
            })
            .unwrap();
        let text = terminal.backend().to_string();
        assert!(text.contains("3 quick steps"), "{text}");
    }

    /// Issue #631: `header:effects`, computed in `changes::diff` but
    /// never drawn since the effects count left the header (#626),
    /// now highlights the flow card's own effects line.
    #[test]
    fn header_effects_highlight_tints_the_flow_cards_effects_line() {
        let card = oscilloscope_core::screens::FlowCard {
            effects_done: 3,
            effects_planned: Some(8),
            ..Default::default()
        };
        let theme = Theme::for_name(crate::theme::ThemeName::Dracula);
        let mut tracker = Tracker::new();
        let now = std::time::Instant::now();
        tracker.mark("header:effects", now);
        let ctx = DrawCtx {
            theme: &theme,
            highlights: &tracker,
            now,
            color_mode: ColorMode::TrueColor,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                draw_flow_card(f, area, &card, &ctx);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let tinted = (0..buffer.area.height).any(|y| {
            (0..buffer.area.width).any(|x| buffer[(x, y)].bg != theme.background.unwrap())
        });
        assert!(
            tinted,
            "header:effects must tint the flow card's own effects line"
        );
    }

    /// `header:tokens` and `tokenbar`, computed in `changes::diff`
    /// but unread by any `ctx.highlight` call since the token bar
    /// left screen 2 (#632), now tint the cost card's own ↑/↓ line and
    /// in-flight line respectively.
    #[test]
    fn header_tokens_and_tokenbar_highlights_tint_the_cost_cards_own_lines() {
        let card = oscilloscope_core::screens::CostCard {
            tokens_sent: 900,
            tokens_received: 400,
            in_flight: "3 prompts in flight".to_string(),
            ..Default::default()
        };
        let theme = Theme::for_name(crate::theme::ThemeName::Dracula);
        let mut tracker = Tracker::new();
        let now = std::time::Instant::now();
        tracker.mark("header:tokens", now);
        tracker.mark("tokenbar", now);
        let ctx = DrawCtx {
            theme: &theme,
            highlights: &tracker,
            now,
            color_mode: ColorMode::TrueColor,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                draw_cost_card(f, area, &card, &ctx);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let tinted_rows: Vec<u16> = (0..buffer.area.height)
            .filter(|&y| {
                (0..buffer.area.width).any(|x| buffer[(x, y)].bg != theme.background.unwrap())
            })
            .collect();
        assert!(
            tinted_rows.len() >= 2,
            "both the tokens line and the in-flight line must tint: {tinted_rows:?}"
        );
    }

    /// A leaf that never got `completed_at` because the run itself
    /// ended first (aborted or cancelled) is in the exact same data
    /// shape as a still-running one -- only its own status tells the
    /// two apart, and it must never read as a real (if tiny) token
    /// count.
    #[test]
    fn an_aborted_leafs_placeholder_weight_never_reads_as_a_real_number() {
        // `cut_off`'s own `weight` (what boosting a running/aborted
        // placeholder actually touches, `treemap`'s own tests) is
        // comparable to its sibling's, large enough to draw a real
        // share of the row -- but its `total` (the real value) is
        // honestly zero, which is what this test pins: `draw_cost_
        // treemap`'s display-text decision reads `total`, never
        // `weight`, so the boosted number itself can never leak into
        // the label text.
        let tree = vec![
            TreemapNode::leaf("big", 1_000_000, Some(StatusKind::Done)),
            TreemapNode {
                key: "cut_off".to_string(),
                path: "cut_off".to_string(),
                total: 0,
                weight: 900_000,
                status: Some(StatusKind::Aborted),
                live: false,
                live_turns: None,
                children: Vec::new(),
            },
        ];
        let screen = oscilloscope_core::screens::CostScreen {
            measure: Measure::Total,
            tree,
            stores_without_measure: 0,
            stores: oscilloscope_core::cost::LargestStores::default(),
            tokens_over_time: Vec::new(),
            chart_start_ts: None,
            chart_end_ts: "2026-01-01T00:00:00Z".to_string(),
        };
        let mut app = App::new(false);
        let test_theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let test_tracker = crate::highlight::Tracker::new();
        let ctx = DrawCtx {
            theme: &test_theme,
            highlights: &test_tracker,
            now: Instant::now(),
            color_mode: ColorMode::TrueColor,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut cost_layout = CostLayoutCache::default();
        terminal
            .draw(|f| {
                let area = f.area();
                draw_cost_treemap(f, area, &screen, &mut app, &mut cost_layout, &ctx);
            })
            .unwrap();
        let text = terminal.backend().to_string();
        assert!(text.contains("no tokens reported"), "{text}");
        assert!(!text.contains("cut_off 900"), "{text}");
    }

    fn cost_table_screen(
        rows: Vec<oscilloscope_core::cost::StoreRow>,
    ) -> oscilloscope_core::screens::CostScreen {
        let run_tokens_sent = rows.iter().map(|r| r.tokens_sent).sum();
        let run_tokens_received = rows.iter().map(|r| r.tokens_received).sum();
        oscilloscope_core::screens::CostScreen {
            measure: Measure::Total,
            tree: Vec::new(),
            stores_without_measure: 0,
            stores: oscilloscope_core::cost::LargestStores {
                rows,
                footer: oscilloscope_core::cost::StoresFooter {
                    run_tokens_sent,
                    run_tokens_received,
                    run_cost_usd: None,
                    gap_tokens: 0,
                    cost_reporting: 0,
                    cost_stores: 0,
                },
            },
            tokens_over_time: Vec::new(),
            chart_start_ts: None,
            chart_end_ts: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    fn draw_cost_table_text(
        screen: &oscilloscope_core::screens::CostScreen,
        width: u16,
        height: u16,
    ) -> String {
        let mut app = App::new(false);
        let test_theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let test_tracker = crate::highlight::Tracker::new();
        let ctx = DrawCtx {
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
                let area = f.area();
                draw_cost_table(f, area, screen, &mut app, &ctx);
            })
            .unwrap();
        terminal.backend().to_string()
    }

    /// 20 stores in the table pane an 80×24 terminal's own layout
    /// actually carves out for it (DESIGN.md's small-terminal rule:
    /// the treemap above, the table taking the rest -- no chart below
    /// 100 columns wide): the footer (the run's own totals, its cost
    /// coverage) must still show in full, at the bottom, with a
    /// `+N more` line accounting for whatever rows had to drop to
    /// make room for it, never the footer itself scrolled off the
    /// bottom the way appending it after every row used to risk.
    #[test]
    fn the_footer_stays_pinned_with_20_stores_at_80x24() {
        let rows: Vec<oscilloscope_core::cost::StoreRow> = (0..20)
            .map(|i| oscilloscope_core::cost::StoreRow {
                label: format!("store{i}"),
                path: format!("prime.store{i}"),
                tokens_sent: 100,
                tokens_received: 50,
                cost_usd: None,
                status: Some(StatusKind::Done),
            })
            .collect();
        let screen = cost_table_screen(rows);
        // The table pane's own area at a nominal 80×24 terminal
        // (`draw_cost_screen`'s own derivation: a 1-line header, then
        // the treemap's golden-major share of the remaining height,
        // the table taking the rest, full width since the chart needs
        // 100 columns).
        let text = draw_cost_table_text(&screen, 80, 9);
        assert!(
            text.contains("total ↑"),
            "the footer's own totals line must still show: {text}"
        );
        assert!(
            text.contains("more"),
            "rows had to be dropped to fit, so a +N more line must show: {text}"
        );
        assert!(text.contains("store0"), "{text}");
    }

    /// DESIGN.md: a still-running store's table row reads the same
    /// "tokens when done" its tile does, never `↑0 t ↓0 t 0%` read
    /// back as if it meant something.
    #[test]
    fn a_running_rows_own_table_line_reads_tokens_when_done() {
        let rows = vec![
            oscilloscope_core::cost::StoreRow {
                label: "review".to_string(),
                path: "prime.review".to_string(),
                tokens_sent: 0,
                tokens_received: 0,
                cost_usd: None,
                status: Some(StatusKind::Running),
            },
            oscilloscope_core::cost::StoreRow {
                label: "plan".to_string(),
                path: "prime.plan".to_string(),
                tokens_sent: 100,
                tokens_received: 50,
                cost_usd: None,
                status: Some(StatusKind::Done),
            },
        ];
        let screen = cost_table_screen(rows);
        let text = draw_cost_table_text(&screen, 60, 10);
        assert!(
            text.contains("review  tokens when done"),
            "a running row must read \"tokens when done\", not 0 t: {text}"
        );
        assert!(!text.contains("↑0 t ↓0 t"), "{text}");
    }

    /// DESIGN.md's own performance rule, directly, rather than only
    /// through the timing bound [`drawing_a_2000_node_state_stays_
    /// well_under_the_redraw_interval`] checks indirectly -- corrupts
    /// the cache's own stored cells after the first call, then calls
    /// again with the identical nodes and area: a cache *hit* must
    /// return that untouched corruption, not a freshly squarified
    /// (and so no longer corrupt) set of cells.
    #[test]
    fn cost_layout_cache_skips_squarify_when_neither_the_tree_nor_the_area_changed() {
        let nodes = vec![
            TreemapNode::leaf("a", 2000, Some(StatusKind::Done)),
            TreemapNode::leaf("b", 1000, Some(StatusKind::Done)),
        ];
        let area = Rect::new(0, 0, 30, 10);
        let mut cache = CostLayoutCache::default();
        let first = cache.cells(&nodes, area).to_vec();
        assert!(!first.is_empty());
        cache.cells.clear();
        let second = cache.cells(&nodes, area);
        assert!(
            second.is_empty(),
            "a cache hit must skip squarify entirely, not recompute and overwrite the corruption"
        );
        // Changing the area is what must actually invalidate it.
        let third = cache.cells(&nodes, Rect::new(0, 0, 40, 12));
        assert!(
            !third.is_empty(),
            "a changed area must trigger a fresh layout"
        );
    }

    #[test]
    fn the_treemap_says_no_stores_yet_rather_than_drawing_an_empty_grid() {
        let screen = oscilloscope_core::screens::CostScreen {
            measure: Measure::Total,
            tree: Vec::new(),
            stores_without_measure: 0,
            stores: oscilloscope_core::cost::LargestStores::default(),
            tokens_over_time: Vec::new(),
            chart_start_ts: None,
            chart_end_ts: "2026-01-01T00:00:00Z".to_string(),
        };
        let mut app = App::new(false);
        let test_theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let test_tracker = crate::highlight::Tracker::new();
        let ctx = DrawCtx {
            theme: &test_theme,
            highlights: &test_tracker,
            now: Instant::now(),
            color_mode: ColorMode::TrueColor,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut cost_layout = CostLayoutCache::default();
        terminal
            .draw(|f| {
                let area = f.area();
                draw_cost_treemap(f, area, &screen, &mut app, &mut cost_layout, &ctx);
            })
            .unwrap();
        let text = terminal.backend().to_string();
        assert!(text.contains("no stores yet"), "{text}");
    }

    /// A zoomed treemap (DESIGN.md: "the zoomed store's own name at
    /// each level ... shown alongside the measure"): zooming into
    /// `fan` must show only its own children, title the pane with
    /// `fan`'s own name (not `prime.fan`) next to the measure, and
    /// leave `a`/`b` off screen entirely.
    #[test]
    fn zoomed_into_fan_at_120x40() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = cost_finished_state();
        let rendered = render::build_folded(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(true),
            300.0,
            "2026-01-01T00:05:00Z",
            &[],
            true,
            &Default::default(),
        );
        let flat = oscilloscope_core::model::flatten_state(&state);
        let statuses: std::collections::BTreeMap<_, _> = rendered
            .unfolded_rows
            .iter()
            .map(|r| (r.path.clone(), r.status.clone()))
            .collect();
        let kinds: std::collections::BTreeMap<_, _> = rendered
            .unfolded_rows
            .iter()
            .map(|r| (r.path.clone(), r.kind))
            .collect();
        let cost_screen = oscilloscope_core::screens::build_cost_screen(
            &rendered.rows,
            &rendered.header,
            &flat,
            &statuses,
            &kinds,
            Some(&state),
            Measure::Total,
            "2026-01-01T00:05:00Z",
        );
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Cost;
        app.cost_zoom = vec!["prime.fan".to_string()];
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
        let mut cost_layout = CostLayoutCache::default();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    None,
                    &[],
                    0,
                    &mut app,
                    Some(&state),
                    &test_ctx,
                    0,
                    "2026-01-01T00:05:00Z",
                    None,
                    Some(&cost_screen),
                    None,
                    None,
                    None,
                    &mut cost_layout,
                )
            })
            .unwrap();
        let text = terminal.backend().to_string();
        assert!(text.contains("tokens by store · tokens · fan"), "{text}");
        assert!(text.contains("iter_0"), "{text}");
        assert!(
            !text.contains("a 900"),
            "a must be off screen while zoomed: {text}"
        );
        assert!(
            text.contains("iter_0 › nap"),
            "zoomed into fan, iter_0's table row must read relative to it: {text}"
        );
        assert!(
            text.contains("iter_1 › nap"),
            "zoomed into fan, iter_1's table row must read relative to it: {text}"
        );
        assert!(
            !text.contains("fan ›"),
            "zoomed into fan, no table row may still carry fan's own name as a prefix: {text}"
        );
    }
    /// One plan exercising every acceptance-list glyph at once
    /// (issue #635): two quick tool steps (`a`/`b`) that fold into
    /// one row, a named `if` (`gate`, taken `then` into `chosen`), an
    /// unnamed `if` (inferred from its own `u_then`/`u_else`
    /// branches), and a named loop (`fan`) whose single observed
    /// pass merges into one row.
    fn flow_matrix_plan() -> PlanTree {
        let root_path = EffectPath::root();
        let a_path = root_path.clone().push_name("a");
        let b_path = root_path.clone().push_name("b");
        let gate_path = root_path.clone().push_name("gate");
        let chosen_path = gate_path.clone().push_name("chosen");
        let other_path = gate_path.clone().push_name("other");
        let u_then_path = root_path.clone().push_name("u_then");
        let u_else_path = root_path.clone().push_name("u_else");
        let fan_path = root_path.clone().push_name("fan");
        let pass_path = fan_path.clone().push_pass(LoopId(0));
        let nap_path = pass_path.push_name("nap");
        // A transparent (unnamed) `if`'s own `PlanEntry` is recorded
        // at its *enclosing* scope's own path, never a fresh one of
        // its own (`PlanTree::unnamed_if_decisions`' own doc comment)
        // -- here, that's the top-level chain's own path, `prime`
        // itself, same as `flow.rs`'s own `unnamed_if_plan` test
        // fixture.
        let unnamed_if_path = root_path.clone();
        let root = Op {
            path: root_path,
            name: Some("prime".to_string()),
            kind: NodeKind::Control(Region::Block {
                ops: vec![
                    tool(a_path, "a", OnError::Fail),
                    tool(b_path, "b", OnError::Fail),
                    Op {
                        path: gate_path,
                        name: Some("gate".to_string()),
                        kind: NodeKind::Control(Region::If {
                            cond: Condition::Cel {
                                expr: "x > 1".to_string(),
                                strict: false,
                            },
                            then_: Box::new(Region::Block {
                                ops: vec![tool(chosen_path, "chosen", OnError::Fail)],
                                overlay: true,
                            }),
                            else_: Some(Box::new(Region::Block {
                                ops: vec![tool(other_path, "other", OnError::Fail)],
                                overlay: true,
                            })),
                            threshold: 0.5,
                        }),
                        on_error: OnError::Fail,
                        labels: None,
                        enabled: true,
                    },
                    Op {
                        path: unnamed_if_path,
                        name: None,
                        kind: NodeKind::Control(Region::If {
                            cond: Condition::Cel {
                                expr: "y > 1".to_string(),
                                strict: false,
                            },
                            then_: Box::new(Region::Block {
                                ops: vec![tool(u_then_path, "u_then", OnError::Fail)],
                                overlay: true,
                            }),
                            else_: Some(Box::new(Region::Block {
                                ops: vec![tool(u_else_path, "u_else", OnError::Fail)],
                                overlay: true,
                            })),
                            threshold: 0.5,
                        }),
                        on_error: OnError::Fail,
                        labels: None,
                        enabled: true,
                    },
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

    /// [`flow_matrix_plan`]'s own state, running (`finished: false`)
    /// or finished (`finished: true`): `gate` has taken `then`, the
    /// unnamed `if`'s own `u_then` branch has started (inferred), and
    /// `fan`'s single pass is either still running or done.
    fn flow_matrix_state(finished: bool) -> Value {
        let nap = if finished {
            json!({"value": "ok\n", "meta": {
                "created_at": "2026-01-01T00:00:01Z", "completed_at": "2026-01-01T00:00:01.400Z",
                "provider": "shell"
            }})
        } else {
            json!({"value": null, "meta": {
                "created_at": "2026-01-01T00:00:01Z", "completed_at": null, "provider": "shell"
            }})
        };
        let fan_completed = if finished {
            json!("2026-01-01T00:00:01.400Z")
        } else {
            Value::Null
        };
        let prime_completed = if finished {
            json!("2026-01-01T00:00:02Z")
        } else {
            Value::Null
        };
        let prime_error = if finished { json!(null) } else { Value::Null };
        let runtime = if finished {
            json!({"last_run": {"completed_at": "2026-01-01T00:00:02Z"}})
        } else {
            Value::Null
        };
        json!({"runtime": runtime, "prime": {
            "value": null,
            "meta": {
                "created_at": "2026-01-01T00:00:00Z", "completed_at": prime_completed,
                "error": prime_error
            },
            "a": {"value": "ok\n", "meta": {
                "created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:00:00.200Z",
                "provider": "shell"
            }},
            "b": {"value": "ok\n", "meta": {
                "created_at": "2026-01-01T00:00:00.200Z", "completed_at": "2026-01-01T00:00:00.400Z",
                "provider": "shell"
            }},
            "gate": {"value": null, "meta": {
                "created_at": "2026-01-01T00:00:00.400Z", "completed_at": "2026-01-01T00:00:00.400Z",
                "branch": "then"
            },
                "chosen": {"value": "ok\n", "meta": {
                    "created_at": "2026-01-01T00:00:00.400Z", "completed_at": "2026-01-01T00:00:00.700Z",
                    "provider": "shell"
                }}
            },
            "u_then": {"value": "ok\n", "meta": {
                "created_at": "2026-01-01T00:00:00.700Z", "completed_at": "2026-01-01T00:00:00.900Z",
                "provider": "shell"
            }},
            "fan": {"value": null, "meta": {
                "completed_at": fan_completed,
                "progress": {"done": if finished { 1 } else { 0 }, "total": 1}
            },
                "iter_0": nap
            }
        }})
    }

    fn flow_matrix_story() -> Vec<LogLine> {
        vec![
            LogLine {
                ts: Some("2026-01-01T00:00:00Z".to_string()),
                text: "■ run started  abc".to_string(),
                kind: LogKind::RunStart,
                path: None,
            },
            LogLine {
                ts: Some("2026-01-01T00:00:00.400Z".to_string()),
                text: "◆ prime.gate → then".to_string(),
                kind: LogKind::Decision,
                path: Some("prime.gate".to_string()),
            },
        ]
    }

    /// [`snapshot_at`], with a non-empty story (DESIGN.md's own
    /// right/bottom pane) -- every other golden here passes `&[]`
    /// for it, since none needed one until this matrix did.
    #[allow(clippy::too_many_arguments)]
    fn flow_matrix_snapshot_at(
        width: u16,
        height: u16,
        plan: &PlanTree,
        model: &mut RunModel,
        state: &Value,
        process: ProcessState,
        selected: &str,
        story: &[LogLine],
    ) -> String {
        let rendered = render::build_folded(
            "do-thing.yml",
            "cof",
            plan,
            model,
            Some(state),
            process,
            2.0,
            "2026-01-01T00:00:02Z",
            &[],
            true,
            &Default::default(),
        );
        let details = render::details_for(selected, plan, model, Some(state));
        let flow_screen = oscilloscope_core::screens::build_flow_screen(
            &rendered.rows,
            &rendered.unfolded_rows,
            &oscilloscope_core::model::flatten_state(state),
            plan,
            model,
            Some(state),
            story,
            Some(selected),
            &test_zone(),
            "2026-01-01T00:00:02Z",
        );
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Flow;
        app.selected_path = Some(selected.to_string());
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
                    &[],
                    0,
                    &mut app,
                    Some(state),
                    &test_ctx,
                    0,
                    "2026-01-01T00:00:02Z",
                    None,
                    None,
                    Some(&flow_screen),
                    None,
                    None,
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        terminal.backend().to_string()
    }

    #[test]
    fn flow_matrix_running_at_120x40() {
        let plan = flow_matrix_plan();
        let state = flow_matrix_state(false);
        let mut model = RunModel::new();
        let text = flow_matrix_snapshot_at(
            120,
            40,
            &plan,
            &mut model,
            &state,
            ProcessState::Running,
            "prime.fan.iter_0",
            &flow_matrix_story(),
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn flow_matrix_finished_at_120x40() {
        let plan = flow_matrix_plan();
        let state = flow_matrix_state(true);
        let mut model = RunModel::new();
        let text = flow_matrix_snapshot_at(
            120,
            40,
            &plan,
            &mut model,
            &state,
            ProcessState::exited(false),
            "prime.gate.chosen",
            &flow_matrix_story(),
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn flow_matrix_running_at_100x30() {
        let plan = flow_matrix_plan();
        let state = flow_matrix_state(false);
        let mut model = RunModel::new();
        let text = flow_matrix_snapshot_at(
            100,
            30,
            &plan,
            &mut model,
            &state,
            ProcessState::Running,
            "prime.fan.iter_0",
            &flow_matrix_story(),
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn flow_matrix_finished_at_100x30() {
        let plan = flow_matrix_plan();
        let state = flow_matrix_state(true);
        let mut model = RunModel::new();
        let text = flow_matrix_snapshot_at(
            100,
            30,
            &plan,
            &mut model,
            &state,
            ProcessState::exited(false),
            "prime.gate.chosen",
            &flow_matrix_story(),
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn flow_matrix_running_at_80x24() {
        let plan = flow_matrix_plan();
        let state = flow_matrix_state(false);
        let mut model = RunModel::new();
        let text = flow_matrix_snapshot_at(
            80,
            24,
            &plan,
            &mut model,
            &state,
            ProcessState::Running,
            "prime.fan.iter_0",
            &flow_matrix_story(),
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn flow_matrix_finished_at_80x24() {
        let plan = flow_matrix_plan();
        let state = flow_matrix_state(true);
        let mut model = RunModel::new();
        let text = flow_matrix_snapshot_at(
            80,
            24,
            &plan,
            &mut model,
            &state,
            ProcessState::exited(false),
            "prime.gate.chosen",
            &flow_matrix_story(),
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn flow_matrix_running_at_70x20() {
        let plan = flow_matrix_plan();
        let state = flow_matrix_state(false);
        let mut model = RunModel::new();
        let text = flow_matrix_snapshot_at(
            70,
            20,
            &plan,
            &mut model,
            &state,
            ProcessState::Running,
            "prime.fan.iter_0",
            &flow_matrix_story(),
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn flow_matrix_finished_at_70x20() {
        let plan = flow_matrix_plan();
        let state = flow_matrix_state(true);
        let mut model = RunModel::new();
        let text = flow_matrix_snapshot_at(
            70,
            20,
            &plan,
            &mut model,
            &state,
            ProcessState::exited(false),
            "prime.gate.chosen",
            &flow_matrix_story(),
        );
        insta::assert_snapshot!(text);
    }

    /// `End`/`G` sets `story_scroll` to `usize::MAX` (`keys::jump`),
    /// and used to add `height` to it before checking for that
    /// sentinel at all, which panics on overflow in a debug build.
    #[test]
    fn end_in_the_story_pane_draws_without_overflowing() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

        let plan = flow_matrix_plan();
        let state = flow_matrix_state(false);
        let mut model = RunModel::new();
        let rendered = render::build_folded(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            2.0,
            "2026-01-01T00:00:02Z",
            &[],
            true,
            &Default::default(),
        );
        let details = render::details_for("prime.fan.iter_0", &plan, &model, Some(&state));
        let story = flow_matrix_story();
        let flow_screen = oscilloscope_core::screens::build_flow_screen(
            &rendered.rows,
            &rendered.unfolded_rows,
            &oscilloscope_core::model::flatten_state(&state),
            &plan,
            &model,
            Some(&state),
            &story,
            Some("prime.fan.iter_0"),
            &test_zone(),
            "2026-01-01T00:00:02Z",
        );
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Flow;
        app.selected_path = Some("prime.fan.iter_0".to_string());
        app.follow = false;
        app.pane = Pane::Story;
        app.handle_key(
            KeyEvent {
                code: KeyCode::End,
                modifiers: KeyModifiers::NONE,
                kind: KeyEventKind::Press,
                state: KeyEventState::NONE,
            },
            &[],
            &[],
            true,
            false,
            &[],
        );
        assert_eq!(app.story_scroll, usize::MAX);
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
                    "2026-01-01T00:00:02Z",
                    None,
                    None,
                    Some(&flow_screen),
                    None,
                    None,
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
    }

    /// Eight named `if`s in a row, each with a condition (shown at
    /// 100 columns and wider, so each takes two screen lines), then
    /// one more step -- enough two-line rows ahead of the selected
    /// one to overflow a 100x30 viewport if scrolling still counted
    /// rows instead of screen lines.
    fn flow_overflow_plan() -> PlanTree {
        let root_path = EffectPath::root();
        let mut ops = Vec::new();
        for i in 1..=8 {
            let gate_path = root_path.clone().push_name(format!("g{i}"));
            let then_path = gate_path.clone().push_name("then_step");
            let else_path = gate_path.clone().push_name("else_step");
            ops.push(Op {
                path: gate_path,
                name: Some(format!("g{i}")),
                kind: NodeKind::Control(Region::If {
                    cond: Condition::Cel {
                        expr: format!("x{i} > 1"),
                        strict: false,
                    },
                    then_: Box::new(Region::Block {
                        ops: vec![tool(then_path, "then_step", OnError::Fail)],
                        overlay: true,
                    }),
                    else_: Some(Box::new(Region::Block {
                        ops: vec![tool(else_path, "else_step", OnError::Fail)],
                        overlay: true,
                    })),
                    threshold: 0.5,
                }),
                on_error: OnError::Fail,
                labels: None,
                enabled: true,
            });
        }
        ops.push(tool(
            root_path.clone().push_name("tail"),
            "tail",
            OnError::Fail,
        ));
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
        PlanTree::from_program(&program)
    }

    /// [`flow_overflow_plan`]'s own state: every gate has taken
    /// `then`, and `tail` (the selected row, below all eight) is
    /// still running.
    fn flow_overflow_state() -> Value {
        let mut prime = serde_json::Map::new();
        prime.insert("value".to_string(), Value::Null);
        prime.insert(
            "meta".to_string(),
            json!({"created_at": "2026-01-01T00:00:00Z", "completed_at": null}),
        );
        for i in 1..=8 {
            prime.insert(
                format!("g{i}"),
                json!({"value": null, "meta": {
                    "created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:00:00.100Z",
                    "branch": "then"
                },
                    "then_step": {"value": "ok\n", "meta": {
                        "created_at": "2026-01-01T00:00:00.100Z", "completed_at": "2026-01-01T00:00:00.200Z",
                        "provider": "shell"
                    }}
                }),
            );
        }
        prime.insert(
            "tail".to_string(),
            json!({"value": null, "meta": {
                "created_at": "2026-01-01T00:00:01Z", "completed_at": null, "provider": "shell"
            }}),
        );
        json!({"runtime": Value::Null, "prime": Value::Object(prime)})
    }

    /// At 100x30, eight named `if`s each take two lines (their own
    /// condition shown underneath), overflowing the viewport, so
    /// `tail` -- selected, below all eight -- must still be drawn
    /// above the footer, not cut off by it or drawn into it, which a
    /// by-row (rather than by-line) scroll clamp used to do.
    #[test]
    fn flow_matrix_overflow_at_100x30_keeps_the_selected_row_above_the_footer() {
        let plan = flow_overflow_plan();
        let state = flow_overflow_state();
        let mut model = RunModel::new();
        let text = flow_matrix_snapshot_at(
            100,
            30,
            &plan,
            &mut model,
            &state,
            ProcessState::Running,
            "prime.tail",
            &[],
        );
        let tail_idx = text
            .lines()
            .position(|l| l.contains("◐ tail"))
            .unwrap_or_else(|| panic!("the selected flow-pane row must be drawn at all: {text}"));
        let footer_idx = text
            .lines()
            .position(|l| l.contains("quick steps folded"))
            .unwrap_or_else(|| panic!("the footer must be drawn: {text}"));
        assert!(
            tail_idx < footer_idx,
            "the selected row must be drawn above the footer, not under or inside it: {text}"
        );
        insta::assert_snapshot!(text);
    }
    #[test]
    fn time_running_with_history_at_120x40() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = overview_running_state();
        let text = time_snapshot_at(
            120,
            40,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            Some("prime.b"),
            &Default::default(),
            true,
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn time_running_with_history_at_100x30() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = overview_running_state();
        let text = time_snapshot_at(
            100,
            30,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            Some("prime.b"),
            &Default::default(),
            true,
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn time_running_without_history_at_80x24() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = overview_running_state();
        let text = time_snapshot_at(
            80,
            24,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            Some("prime.b"),
            &Default::default(),
            false,
        );
        insta::assert_snapshot!(text);
    }

    /// The now line must sit at the exact same column in every
    /// timeline row and every strip, with and without `--events` --
    /// the strips' own gutter must not leave out the timeline box's
    /// left border, which would land it one column short.
    #[test]
    fn time_strip_and_row_now_columns_always_match() {
        for use_events in [false, true] {
            let plan = sample_plan();
            let mut model = RunModel::new();
            if use_events {
                model.observe_event(&oscilloscope_core::observe::Event::Start {
                    ts: "2026-01-01T00:00:00Z".to_string(),
                    id: Some(1),
                    path: "prime.a".to_string(),
                });
            }
            let state = overview_running_state();
            let text = time_snapshot_at(
                100,
                30,
                &plan,
                &mut model,
                Some(&state),
                ProcessState::Running,
                Some("prime.b"),
                &Default::default(),
                false,
            );
            let now_cols: Vec<usize> = text
                .lines()
                .filter_map(|l| l.chars().position(|c| c == '┃'))
                .collect();
            assert!(
                now_cols.len() >= 2,
                "expected both bar rows and strips to show a now line (events={use_events})"
            );
            assert!(
                now_cols.windows(2).all(|w| w[0] == w[1]),
                "now column must line up everywhere (events={use_events}): {now_cols:?}"
            );
        }
    }

    /// The same now-column alignment, at the two smaller widths the
    /// start/duration columns are first shown or first dropped at
    /// (`thresholds::TIME_START_DURATION_COLUMN_MIN_WIDTH`) -- a
    /// layout miscalculation at either boundary would show up as a
    /// now line that lines up at 100 columns but not at one just
    /// above or below the threshold.
    #[test]
    fn time_strip_and_row_now_columns_always_match_at_80x24_and_85x24() {
        for (width, height) in [(80u16, 24u16), (85u16, 24u16)] {
            let plan = sample_plan();
            let mut model = RunModel::new();
            model.observe_event(&oscilloscope_core::observe::Event::Start {
                ts: "2026-01-01T00:00:00Z".to_string(),
                id: Some(1),
                path: "prime.a".to_string(),
            });
            let state = overview_running_state();
            let text = time_snapshot_at(
                width,
                height,
                &plan,
                &mut model,
                Some(&state),
                ProcessState::Running,
                Some("prime.b"),
                &Default::default(),
                false,
            );
            let now_cols: Vec<usize> = text
                .lines()
                .filter_map(|l| l.chars().position(|c| c == '┃'))
                .collect();
            assert!(
                now_cols.len() >= 2,
                "expected both bar rows and strips to show a now line at {width}x{height}"
            );
            assert!(
                now_cols.windows(2).all(|w| w[0] == w[1]),
                "now column must line up everywhere at {width}x{height}: {now_cols:?}"
            );
        }
    }

    /// A golden with the short `events/min` label (no `(state)`
    /// suffix) -- the strips' own gutter must still line up with the
    /// bars with it too, not just the longer one.
    #[test]
    fn time_strip_labels_at_100x30_with_the_short_events_label() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        model.observe_event(&oscilloscope_core::observe::Event::Start {
            ts: "2026-01-01T00:00:00Z".to_string(),
            id: Some(1),
            path: "prime.a".to_string(),
        });
        let state = overview_running_state();
        let text = time_snapshot_at(
            100,
            30,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            Some("prime.b"),
            &Default::default(),
            false,
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn time_finished_without_history_at_70x20() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = overview_finished_state();
        let text = time_snapshot_at(
            70,
            20,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(false),
            Some("prime.b"),
            &Default::default(),
            false,
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn time_finished_with_history_at_120x40() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = overview_finished_state();
        let text = time_snapshot_at(
            120,
            40,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(false),
            Some("prime.b"),
            &Default::default(),
            true,
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn time_finished_with_history_at_80x24() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = overview_finished_state();
        let text = time_snapshot_at(
            80,
            24,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(false),
            Some("prime.b"),
            &Default::default(),
            true,
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn time_running_without_history_at_70x20() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = overview_running_state();
        let text = time_snapshot_at(
            70,
            20,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            Some("prime.b"),
            &Default::default(),
            false,
        );
        insta::assert_snapshot!(text);
    }

    fn each_loop_state() -> Value {
        json!({"prime": {"value": null, "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": null},
            "items": {"value": null, "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": null, "progress": {"done": 1, "total": 2}},
                "iter_0": {"value": null, "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:00:10Z"},
                    "nap": {"value": "n0", "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:00:10Z", "provider": "shell"}}
                },
                "iter_1": {"value": null, "meta": {"created_at": "2026-01-01T00:00:10Z", "completed_at": null},
                    "nap": {"value": null, "meta": {"created_at": "2026-01-01T00:00:10Z", "completed_at": null, "provider": "shell"}}
                }
            }
        }})
    }

    fn each_loop_plan_for_tui() -> PlanTree {
        use electricity_bytecode::{Program, Region};
        let root_path = EffectPath::root();
        let loop_path = root_path.push_name("items");
        let pass0 = loop_path.push_pass(LoopId(0));
        let nap = tool(pass0.push_name("nap"), "nap", OnError::Fail);
        let root = Op {
            path: root_path,
            name: Some("prime".to_string()),
            kind: NodeKind::Control(Region::Block {
                ops: vec![Op {
                    path: loop_path.clone(),
                    name: Some("items".to_string()),
                    kind: NodeKind::Control(Region::Loop {
                        name: Some("items".to_string()),
                        spec: LoopSpec::Each {
                            in_path: "prime.xs".to_string(),
                            as_name: "x".to_string(),
                            truncate: false,
                        },
                        body: Box::new(Region::Block {
                            ops: vec![nap],
                            overlay: true,
                        }),
                        flow: LoopFlow::Chain,
                        max_concurrency: None,
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

    #[test]
    fn time_collapsed_loop_at_100x30() {
        let plan = each_loop_plan_for_tui();
        let mut model = RunModel::new();
        let state = each_loop_state();
        let text = time_snapshot_at(
            100,
            30,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            Some("prime.items"),
            &Default::default(),
            false,
        );
        assert!(
            text.contains('⟲'),
            "a collapsed loop row shows its own progress:\n{text}"
        );
        insta::assert_snapshot!(text);
    }

    /// Follow picking a running leaf inside a collapsed loop (the
    /// leaf's own breadcrumb row never appears in the timeline
    /// itself) must still name it in the zoom title -- the loop's
    /// own label, then the leaf's own breadcrumb.
    #[test]
    fn time_zoom_title_names_a_running_leaf_under_a_collapsed_loop() {
        let plan = each_loop_plan_for_tui();
        let mut model = RunModel::new();
        let state = each_loop_state();
        let text = time_snapshot_at(
            100,
            30,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            Some("prime.items.iter_1.nap"),
            &Default::default(),
            false,
        );
        assert!(
            text.contains("zoom · items › pass 2 of 2 › nap"),
            "the zoom title must name the selected leaf and its loop:\n{text}"
        );
        insta::assert_snapshot!(text);
    }

    fn many_steps_then_a_named_loop_plan(n: usize) -> PlanTree {
        let root_path = EffectPath::root();
        let mut ops: Vec<Op> = (0..n)
            .map(|i| {
                tool(
                    root_path.clone().push_name(format!("s{i}")),
                    &format!("s{i}"),
                    OnError::Fail,
                )
            })
            .collect();
        let loop_path = root_path.clone().push_name("items");
        let pass0 = loop_path.clone().push_pass(LoopId(0));
        let nap = tool(pass0.push_name("nap"), "nap", OnError::Fail);
        ops.push(Op {
            path: loop_path.clone(),
            name: Some("items".to_string()),
            kind: NodeKind::Control(Region::Loop {
                name: Some("items".to_string()),
                spec: LoopSpec::Each {
                    in_path: "prime.xs".to_string(),
                    as_name: "x".to_string(),
                    truncate: false,
                },
                body: Box::new(Region::Block {
                    ops: vec![nap],
                    overlay: true,
                }),
                flow: LoopFlow::Chain,
                max_concurrency: None,
                max_iterations: None,
                min_iterations: 0,
                collect: None,
            }),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        });
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
        PlanTree::from_program(&Program {
            root,
            prompts: Default::default(),
            effect_names: Default::default(),
            document: None,
            runtime_block: None,
            interface: None,
            adapter: None,
            model: None,
        })
    }

    /// A leaf hidden under a collapsed loop (DESIGN.md: "Loops are
    /// collapsed by default") has no row of its own in the timeline
    /// at all -- follow selecting it must highlight and scroll to its
    /// loop row, the nearest one actually drawn, not give up and
    /// leave the viewport wherever it was.
    #[test]
    fn time_timeline_scrolls_to_a_collapsed_loops_row_for_a_hidden_leafs_selection() {
        let n = 30;
        let plan = many_steps_then_a_named_loop_plan(n);
        let mut model = RunModel::new();
        let mut state = serde_json::Map::new();
        for i in 0..n {
            state.insert(
                format!("s{i}"),
                json!({"value": "ok", "meta": {
                    "created_at": format!("2026-01-01T00:{:02}:00Z", i),
                    "completed_at": format!("2026-01-01T00:{:02}:01Z", i)
                }}),
            );
        }
        state.insert(
            "items".to_string(),
            json!({"value": null, "meta": {
                    "created_at": "2026-01-01T00:30:00Z", "completed_at": null
                },
                "iter_0": {"value": null, "meta": {
                        "created_at": "2026-01-01T00:30:00Z", "completed_at": "2026-01-01T00:30:10Z"
                    },
                    "nap": {"value": "n0", "meta": {
                        "created_at": "2026-01-01T00:30:00Z", "completed_at": "2026-01-01T00:30:10Z", "provider": "shell"
                    }}
                },
                "iter_1": {"value": null, "meta": {
                        "created_at": "2026-01-01T00:30:10Z", "completed_at": null
                    },
                    "nap": {"value": null, "meta": {
                        "created_at": "2026-01-01T00:30:10Z", "completed_at": null, "provider": "shell"
                    }}
                }
            }),
        );
        let state = json!({"prime": Value::Object(state)});
        let now = "2026-01-01T00:30:15Z";
        let rendered = render::build_folded(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            900.0,
            now,
            &[],
            true,
            &Default::default(),
        );
        let flat = oscilloscope_core::model::flatten_state(&state);
        let time_screen = oscilloscope_core::screens::build_time_screen(
            &rendered.rows,
            &flat,
            &plan,
            &model,
            &rendered.header,
            Some(&state),
            &[],
            Some("prime.items.iter_1.nap"),
            &Default::default(),
            now,
        );
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Time;
        app.selected_path = Some("prime.items.iter_1.nap".to_string());
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
        let backend = TestBackend::new(100, 22);
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
                    Some(&state),
                    &test_ctx,
                    0,
                    now,
                    None,
                    None,
                    None,
                    Some(&time_screen),
                    None,
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        let text = terminal.backend().to_string();
        assert!(
            text.contains("items"),
            "the selected leaf's own loop row must be scrolled into view: {text}"
        );
        assert!(
            !text.contains("s0 "),
            "scrolled past the first row, which must no longer show: {text}"
        );
    }

    /// DESIGN.md's own performance rule: "A 400-pass loop collapsed
    /// is one row."
    #[test]
    fn a_400_pass_loop_collapses_to_one_row() {
        let plan = each_loop_plan_for_tui();
        // Real, parseable RFC 3339 timestamps -- not `"t0"`..`"t400"`,
        // which `oscilloscope_core::observe::duration_seconds` can't
        // parse at all, so every row's own `start_s`/`duration_s`
        // silently read `None` and no bar this test could have caught
        // ever actually drew. 400 passes 700ms apart stay inside
        // `time_snapshot_at`'s own fixed 5-minute `now`.
        let ts = |i: i64| -> String {
            (chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z").unwrap()
                + chrono::Duration::milliseconds(700 * i))
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
        };
        let mut prime = serde_json::Map::new();
        prime.insert("value".to_string(), Value::Null);
        let mut prime_meta = serde_json::Map::new();
        prime_meta.insert("created_at".to_string(), Value::String(ts(0)));
        prime_meta.insert("completed_at".to_string(), Value::Null);
        prime.insert("meta".to_string(), Value::Object(prime_meta));
        let mut iters = serde_json::Map::new();
        for i in 0..400 {
            let mut nap_meta = serde_json::Map::new();
            nap_meta.insert("created_at".to_string(), Value::String(ts(i)));
            nap_meta.insert("completed_at".to_string(), Value::String(ts(i + 1)));
            nap_meta.insert("provider".to_string(), Value::String("shell".to_string()));
            let mut nap = serde_json::Map::new();
            nap.insert("value".to_string(), Value::String("ok\n".to_string()));
            nap.insert("meta".to_string(), Value::Object(nap_meta));
            let mut iter_meta = serde_json::Map::new();
            iter_meta.insert("created_at".to_string(), Value::String(ts(i)));
            iter_meta.insert("completed_at".to_string(), Value::String(ts(i + 1)));
            let mut iter = serde_json::Map::new();
            iter.insert("value".to_string(), Value::Null);
            iter.insert("meta".to_string(), Value::Object(iter_meta));
            iter.insert("nap".to_string(), Value::Object(nap));
            iters.insert(format!("iter_{i}"), Value::Object(iter));
        }
        let mut items_meta = serde_json::Map::new();
        items_meta.insert("created_at".to_string(), Value::String(ts(0)));
        items_meta.insert("completed_at".to_string(), Value::String(ts(400)));
        items_meta.insert(
            "progress".to_string(),
            serde_json::json!({"done": 400, "total": 400}),
        );
        let mut items = serde_json::Map::new();
        items.insert("value".to_string(), Value::Null);
        items.insert("meta".to_string(), Value::Object(items_meta));
        for (k, v) in iters {
            items.insert(k, v);
        }
        prime.insert("items".to_string(), Value::Object(items));
        let mut root = serde_json::Map::new();
        root.insert("prime".to_string(), Value::Object(prime));
        let state = Value::Object(root);

        let mut model = RunModel::new();
        let text = time_snapshot_at(
            100,
            30,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(false),
            Some("prime.items"),
            &Default::default(),
            false,
        );
        assert_eq!(
            text.matches('⟲').count(),
            1,
            "a 400-pass loop collapsed is one row:\n{text}"
        );
        assert!(
            text.contains("400/400"),
            "the collapsed row shows the real pass count:\n{text}"
        );
        // Only the timeline pane itself (above the zoom pane, which
        // always lists the selected container's own children
        // regardless of whether its loop row is collapsed) must have
        // no per-pass row of its own.
        let timeline_text = text
            .lines()
            .take_while(|l| !l.contains("zoom"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !timeline_text.contains("pass "),
            "a collapsed loop's own timeline shows no per-pass rows:\n{timeline_text}"
        );
    }

    #[test]
    fn time_expanded_loop_at_100x30() {
        let plan = each_loop_plan_for_tui();
        let mut model = RunModel::new();
        let state = each_loop_state();
        let mut expanded = std::collections::BTreeSet::new();
        expanded.insert("prime.items".to_string());
        let text = time_snapshot_at(
            100,
            30,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            Some("prime.items"),
            &expanded,
            false,
        );
        assert!(
            text.contains("pass 1"),
            "an expanded loop shows one row per pass:\n{text}"
        );
        assert!(
            text.contains("pass 2"),
            "an expanded loop shows one row per pass:\n{text}"
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn time_agent_zoom_pane_at_120x40() {
        // Not `time_snapshot_at`: that helper's own hardcoded `now`
        // (`"2026-01-01T00:05:00Z"`) belongs to `sample_plan`'s own
        // state, not `agent_running_state`'s real clock
        // (`11:46:43`-`11:52:54`) -- a real transcript, tailed and
        // drawn the way `main.rs`'s own `tail_focused_transcript`
        // would, is also why this test exists at all.
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = agent_running_state();
        let now = "2026-01-01T11:52:55Z";
        let rendered = render::build_folded(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            372.0,
            now,
            &[],
            true,
            &Default::default(),
        );
        let flat = oscilloscope_core::model::flatten_state(&state);
        let time_screen = oscilloscope_core::screens::build_time_screen(
            &rendered.rows,
            &flat,
            &plan,
            &model,
            &rendered.header,
            Some(&state),
            &[],
            Some("prime.fix"),
            &Default::default(),
            now,
        );
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Time;
        app.selected_path = Some("prime.fix".to_string());
        let transcript = agent_running_transcript();
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
                    None,
                    &[],
                    0,
                    &mut app,
                    Some(&state),
                    &test_ctx,
                    0,
                    now,
                    None,
                    None,
                    None,
                    Some(&time_screen),
                    Some(&transcript),
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        let text = terminal.backend().to_string();
        insta::assert_snapshot!(text);
    }

    /// A finished agent's own transcript can end in an unmatched
    /// `tool_start` (the session stopped mid-call) -- the footer's
    /// busiest-tool figure must come from `meta.raw`'s own aggregate
    /// alone, never grow with how long ago `now` is from that stale
    /// call.
    #[test]
    fn time_agent_zoom_pane_footer_does_not_grow_for_a_finished_agent() {
        let plan = sample_plan();
        let mut state = agent_finished_state();
        state["prime"]["fix"]["meta"]["raw"]["tool_counts"] = json!({
            "bash": {"calls": 3, "ms": 2_500},
            "edit": {"calls": 2, "ms": 900},
        });
        let transcript = agent_running_transcript();
        let footer_line = |now: &str| -> String {
            let mut model = RunModel::new();
            let rendered = render::build_folded(
                "do-thing.yml",
                "cof",
                &plan,
                &mut model,
                Some(&state),
                ProcessState::exited(true),
                372.0,
                now,
                &[],
                true,
                &Default::default(),
            );
            let flat = oscilloscope_core::model::flatten_state(&state);
            let time_screen = oscilloscope_core::screens::build_time_screen(
                &rendered.rows,
                &flat,
                &plan,
                &model,
                &rendered.header,
                Some(&state),
                &[],
                Some("prime.fix"),
                &Default::default(),
                now,
            );
            let mut app = App::new(false);
            app.screen = oscilloscope_core::screens::Screen::Time;
            app.selected_path = Some("prime.fix".to_string());
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
                        None,
                        &[],
                        0,
                        &mut app,
                        Some(&state),
                        &test_ctx,
                        0,
                        now,
                        None,
                        None,
                        None,
                        Some(&time_screen),
                        Some(&transcript),
                        &mut CostLayoutCache::default(),
                    )
                })
                .unwrap();
            terminal
                .backend()
                .to_string()
                .lines()
                .find(|l| l.contains("tools"))
                .unwrap()
                .to_string()
        };
        let right_after = footer_line("2026-01-01T11:52:55Z");
        let hours_later = footer_line("2026-01-01T13:00:00Z");
        assert_eq!(
            right_after, hours_later,
            "the footer must not keep growing once the agent has finished"
        );
        assert!(
            right_after.contains("bash 2.5s"),
            "the busiest tool must come from meta.raw alone: {right_after:?}"
        );
    }

    /// Issue #637: an interrupted agent's own `meta.live` is left
    /// behind with no `completed_at` of its own, and its transcript
    /// can end in an unmatched `tool_start` (the call that was running
    /// when the run was cancelled). The think band's own end, the
    /// footer's running-tool time and the zoom axis must all come
    /// from the row's own (frozen) status, never keep extending to
    /// `now`.
    #[test]
    fn time_agent_zoom_pane_does_not_grow_for_a_cancelled_agent_with_leftover_live() {
        let plan = sample_plan();
        let state = agent_cancelled_state();
        let transcript = oscilloscope_core::transcript::build_transcript_view(
            &[json!({
                "t": "2026-01-01T11:46:50Z", "type": "tool_start", "id": 1,
                "tool": "bash", "target": "pytest -q"
            })],
            false,
        );
        let snapshot_at = |now: &str| -> String {
            let mut model = RunModel::new();
            let rendered = render::build_folded(
                "do-thing.yml",
                "cof",
                &plan,
                &mut model,
                Some(&state),
                ProcessState::exited(true),
                11.0,
                now,
                &[],
                true,
                &Default::default(),
            );
            let flat = oscilloscope_core::model::flatten_state(&state);
            let time_screen = oscilloscope_core::screens::build_time_screen(
                &rendered.rows,
                &flat,
                &plan,
                &model,
                &rendered.header,
                Some(&state),
                &[],
                Some("prime.fix"),
                &Default::default(),
                now,
            );
            let mut app = App::new(false);
            app.screen = oscilloscope_core::screens::Screen::Time;
            app.selected_path = Some("prime.fix".to_string());
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
                        None,
                        &[],
                        0,
                        &mut app,
                        Some(&state),
                        &test_ctx,
                        0,
                        now,
                        None,
                        None,
                        None,
                        Some(&time_screen),
                        Some(&transcript),
                        &mut CostLayoutCache::default(),
                    )
                })
                .unwrap();
            terminal
                .backend()
                .to_string()
                .lines()
                .skip(1)
                .collect::<Vec<_>>()
                .join("\n")
        };
        let right_after = snapshot_at("2026-01-01T11:46:55Z");
        let hours_later = snapshot_at("2026-01-01T13:00:00Z");
        assert_eq!(
            right_after, hours_later,
            "the zoom pane must not keep growing once the run has ended, even with meta.live left over"
        );
    }

    /// DESIGN.md's zoom pane: "With no transcript, the footer and a
    /// `running: <tool>` line (from `current_tool`) are all it
    /// shows" -- `time_snapshot_at` never attaches a transcript at
    /// all, so the running agent's own zoom pane must fall back to
    /// `meta.live.current_tool` instead of the brief-window note.
    #[test]
    fn time_agent_zoom_pane_with_no_transcript_falls_back_to_current_tool() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = agent_running_state();
        let text = time_snapshot_at(
            120,
            40,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            Some("prime.fix"),
            &Default::default(),
            false,
        );
        assert!(
            text.contains("running: bash"),
            "no transcript at all falls back to current_tool:\n{text}"
        );
        assert!(!text.contains(oscilloscope_core::screens::AGENT_LIVE_DATA_NOTE));
    }

    /// DESIGN.md's zoom pane: "the bands start where the window
    /// starts, after a dim `…`" -- shown only once
    /// `TranscriptView::truncated` actually says the tail read
    /// didn't reach the session's own start.
    #[test]
    fn time_agent_zoom_pane_marks_a_truncated_transcript_window() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = agent_running_state();
        let now = "2026-01-01T11:52:55Z";
        let rendered = render::build_folded(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            372.0,
            now,
            &[],
            true,
            &Default::default(),
        );
        let flat = oscilloscope_core::model::flatten_state(&state);
        let time_screen = oscilloscope_core::screens::build_time_screen(
            &rendered.rows,
            &flat,
            &plan,
            &model,
            &rendered.header,
            Some(&state),
            &[],
            Some("prime.fix"),
            &Default::default(),
            now,
        );
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Time;
        app.selected_path = Some("prime.fix".to_string());
        let mut transcript = agent_running_transcript();
        transcript.truncated = true;
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
                    None,
                    &[],
                    0,
                    &mut app,
                    Some(&state),
                    &test_ctx,
                    0,
                    now,
                    None,
                    None,
                    None,
                    Some(&time_screen),
                    Some(&transcript),
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        let text = terminal.backend().to_string();
        assert!(
            text.contains("│…"),
            "a truncated window's own leading gap reads as cut off, not as thinking:\n{text}"
        );
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
                history_eta_ts: None,
                history_eta_past_usual: false,
                history_eta_remaining_s: None,
                history_eta_low_ts: None,
                history_eta_high_ts: None,
                history_vs_usual_s: None,
                history_usual_total_s: None,
                history_same_hash_count: None,
                history_step_usual: Default::default(),
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

        let empty_plan = oscilloscope_core::plan::PlanTree::empty();
        let empty_model = oscilloscope_core::model::RunModel::new();
        let flow_screen = oscilloscope_core::screens::build_flow_screen(
            &rendered.rows,
            &rendered.rows,
            &std::collections::BTreeMap::new(),
            &empty_plan,
            &empty_model,
            None,
            &[],
            app.selected_path.as_deref(),
            &test_zone(),
            "2026-01-01T00:00:00Z",
        );

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
                    None,
                    Some(&flow_screen),
                    None,
                    None,
                    &mut CostLayoutCache::default(),
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
                    None,
                    Some(&flow_screen),
                    None,
                    None,
                    &mut CostLayoutCache::default(),
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
                        None,
                        None,
                        None,
                        None,
                        &mut CostLayoutCache::default(),
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
                    None,
                    None,
                    None,
                    None,
                    &mut CostLayoutCache::default(),
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
                    None,
                    None,
                    None,
                    None,
                    &mut CostLayoutCache::default(),
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
                    None,
                    None,
                    None,
                    None,
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].bg, Color::Reset);
    }
}
