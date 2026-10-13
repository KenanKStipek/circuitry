//! Lays out one `RenderState` (DESIGN.md §6.3): the header, the plan
//! tree and details panes, the log pane, and the key's own overlays
//! (help, both confirms, the full-value view, the filter prompt).
//! The only module in this crate that imports `ratatui` widgets
//! directly, mirroring `oscilloscope-core`'s own "no terminal code"
//! split one level up.

use oscilloscope_core::model::{RunStatus, StatusKind};
use oscilloscope_core::render::{Details, Header, Row, RowKind, StateRow, TokenBar};
use oscilloscope_core::treemap::TreemapNode;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};

use crate::keys::{App, Dialog, Pane, visible_rows};

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

/// Thousands separators for the token bar's exact counts (issue
/// #527: "exact counts (thousands separators)") — `u64` only, so no
/// sign or fractional part to worry about.
fn with_thousands_separators(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
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

fn status_color(kind: StatusKind) -> Color {
    match kind {
        StatusKind::Pending | StatusKind::Skipped => Color::DarkGray,
        StatusKind::Running | StatusKind::LikelyRunning | StatusKind::RunningOrQueued => {
            Color::Yellow
        }
        StatusKind::Done => Color::Green,
        StatusKind::Failed | StatusKind::Aborted => Color::Red,
        StatusKind::FailedHandled => Color::LightRed,
        StatusKind::Cancelled => Color::Magenta,
    }
}

/// `status_color`'s own `Style`, with `LikelyRunning`'s guess
/// (DESIGN.md §2.1 rule 7's "likely" statuses: a row inferred from
/// the run's own exit, never actually observed running) additionally
/// dim and italic -- visibly distinct from a *confirmed* `Running`
/// row, which keeps the plain, full-brightness style (review finding
/// 16 on #458's comment 3).
fn status_style(kind: StatusKind) -> Style {
    let style = Style::default().fg(status_color(kind));
    if kind == StatusKind::LikelyRunning {
        style.add_modifier(Modifier::DIM | Modifier::ITALIC)
    } else {
        style
    }
}

fn format_duration(secs: f64) -> String {
    if secs < 60.0 {
        format!("{secs:.1}s")
    } else {
        format!("{}m{:02.0}s", (secs / 60.0) as u64, secs % 60.0)
    }
}

fn row_line(row: &Row, selected: bool) -> Line<'static> {
    let indent = "  ".repeat(row.depth.saturating_sub(1));
    let glyph = status_glyph(row.status.kind);
    let mut spans = vec![
        Span::styled(format!("{indent}{glyph} "), status_style(row.status.kind)),
        Span::raw(row.label.clone()),
    ];
    if let Some(progress) = &row.loop_progress {
        let total = progress
            .total
            .map(|t| t.to_string())
            .unwrap_or_else(|| "?".to_string());
        spans.push(Span::styled(
            format!("  {}/{total}", progress.done),
            Style::default().fg(Color::DarkGray),
        ));
    }
    if let Some(duration) = row.duration_s {
        spans.push(Span::styled(
            format!("  {}", format_duration(duration)),
            Style::default().fg(Color::DarkGray),
        ));
    }
    if let Some(dim) = &row.dim {
        spans.push(Span::styled(
            format!("  {dim}"),
            Style::default().fg(Color::DarkGray),
        ));
    }
    let style = if selected {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };
    Line::from(spans).style(style)
}

fn draw_header(frame: &mut Frame, area: Rect, header: &oscilloscope_core::render::Header) {
    let (status_text, status_color) = match header.status {
        RunStatus::Running => ("running", Color::Yellow),
        RunStatus::Ok => ("ok", Color::Green),
        RunStatus::Failed => ("failed", Color::Red),
        RunStatus::Cancelled => ("cancelled", Color::Magenta),
        RunStatus::Aborted => ("aborted", Color::Red),
    };
    let effects = match header.effects_planned {
        // Finding 6: a `+` marks "planned" as a lower bound when some
        // loop's own total pass count isn't known yet.
        Some(planned) if header.effects_planned_is_lower_bound => {
            format!("{}/{planned}+", header.effects_done)
        }
        Some(planned) => format!("{}/{planned}", header.effects_done),
        None => header.effects_done.to_string(),
    };
    let mut line = vec![
        Span::raw(format!("{}  ", header.document)),
        Span::styled(header.engine.clone(), Style::default().fg(Color::DarkGray)),
        Span::raw("  "),
        Span::styled(status_text, Style::default().fg(status_color)),
        Span::raw(format!("  {}", format_duration(header.elapsed_s))),
        Span::raw(format!("  effects {effects}")),
        Span::raw(format!(
            "  ↑{} ↓{}",
            header.tokens_sent, header.tokens_received
        )),
    ];
    if let Some(eta) = header.innermost_loop_eta_s {
        line.push(Span::raw(format!("  ETA {}", format_duration(eta))));
    }
    if let Some(run_id) = &header.run_id {
        line.push(Span::styled(
            format!("  {run_id}"),
            Style::default().fg(Color::DarkGray),
        ));
    }
    let paragraph = Paragraph::new(Line::from(line)).block(Block::default().borders(Borders::ALL));
    frame.render_widget(paragraph, area);
}

/// The orchestration pane (issue #527, formerly "plan"): scrolls to
/// keep the selected row in view, with the current position shown in
/// the border once any row is hidden above or below.
fn draw_tree(frame: &mut Frame, area: Rect, rows: &[&Row], app: &mut App, document: &str) {
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
            ListItem::new(row_line(row, selected))
        })
        .collect();
    let border_style = if app.pane == Pane::Tree {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };
    let mut title = format!(" orchestration {}", orchestration_name(document));
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

fn details_lines(details: &Details, kind: RowKind) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(format!("path   {}", details.path)),
        Line::from(format!("type   {}", kind.label())),
    ];
    if !details.plan_summary.is_empty() {
        lines.push(Line::from(format!("plan   {}", details.plan_summary)));
    }
    for (key, value) in &details.meta_summary {
        // Finding 11: a label of 7+ characters ("provider",
        // "adapter_noop") left no gap from `{key:<7}` alone.
        lines.push(Line::from(format!("{key:<8} {value}")));
    }
    if let Some(error) = &details.error {
        lines.push(Line::from(""));
        lines.push(Line::styled(
            format!("error  {error}"),
            Style::default().fg(Color::Red),
        ));
    }
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
    lines
}

/// The details pane (issue #527): scrolls when it has focus
/// (`↑↓`, `PgUp`/`PgDn`), and returns to the top whenever the
/// selected row changes (`App::select_tree_path`'s own job, not
/// this function's).
fn draw_details(
    frame: &mut Frame,
    area: Rect,
    details: Option<&Details>,
    kind: RowKind,
    app: &mut App,
) {
    let border_style = if app.pane == Pane::Details {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };
    let text = details
        .map(|d| details_lines(d, kind))
        .unwrap_or_else(|| vec![Line::from("(nothing selected)")]);
    let viewport = area.height.saturating_sub(2) as usize;
    app.details_viewport = viewport;
    let inner_width = area.width.saturating_sub(2);
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
    let paragraph = paragraph.scroll((app.details_scroll as u16, 0)).block(
        Block::default()
            .borders(Borders::ALL)
            .title(" details ")
            .border_style(border_style),
    );
    frame.render_widget(paragraph, area);
}

fn draw_log(frame: &mut Frame, area: Rect, log_lines: &[String]) {
    let height = area.height.saturating_sub(2) as usize;
    let start = log_lines.len().saturating_sub(height);
    let text: Vec<Line> = log_lines[start..]
        .iter()
        .map(|l| Line::from(l.clone()))
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

/// A label too long for `max`, truncated from the *left*, leading
/// `…` (finding F9: the state pane's own path labels) — two entries
/// sharing a long common prefix (`loop.iter_2.step`/`loop.iter_2.
/// other`) stay distinct this way, where a right-truncated `loop.
/// iter_2…` would show the same text for both.
fn truncate_label_left(label: &str, max: usize) -> String {
    let count = label.chars().count();
    if count <= max {
        label.to_string()
    } else {
        let skip = count - max.saturating_sub(1);
        format!("…{}", label.chars().skip(skip).collect::<String>())
    }
}

/// The state pane's own share of its interior width given to the
/// path label, the rest to the value/error/glyph (finding F9): a
/// fixed 12-column label left no room to tell two long, similarly
/// prefixed paths apart even with [`truncate_label_left`]'s own left
/// truncation, so this scales with the pane's real width instead,
/// within a band that keeps a narrow pane from losing its value
/// column entirely to the label.
fn state_pane_label_width(inner_width: usize) -> usize {
    (inner_width / 3).clamp(10, 24)
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
fn draw_state(frame: &mut Frame, area: Rect, state_rows: &[StateRow], app: &mut App) {
    let viewport = area.height.saturating_sub(2) as usize;
    let inner_width = area.width.saturating_sub(2) as usize;
    let label_width = state_pane_label_width(inner_width);
    let value_width = inner_width.saturating_sub(label_width + 1).max(1);
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
        let (text, color) = if let Some(error) = &row.error_preview {
            (error.clone(), Color::Red)
        } else if let Some(value) = &row.value_preview {
            (value.clone(), Color::Reset)
        } else {
            (
                status_glyph(row.status.kind).to_string(),
                status_color(row.status.kind),
            )
        };
        let text = truncate_label(&text, value_width);
        let label = truncate_label_left(&row.path, label_width);
        let mut line = Line::from(vec![
            Span::raw(format!("{label:label_width$} ")),
            Span::styled(text, Style::default().fg(color)),
        ]);
        if selected {
            line = line.style(Style::default().add_modifier(Modifier::REVERSED));
        }
        lines.push(line);
        if let Some((sent, received)) = row.tokens {
            // Finding F12: the same indent as the value column above
            // (`label_width` plus the one space before it starts).
            lines.push(Line::styled(
                format!("{:label_width$} ↑{sent} ↓{received}", ""),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }

    let border_style = if app.pane == Pane::State {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
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
fn draw_treemap(frame: &mut Frame, area: Rect, nodes: &[TreemapNode], selected_path: Option<&str>) {
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
        let color = cell.status.map(status_color).unwrap_or(Color::DarkGray);
        let is_selected = selected_path.is_some() && cell.path.as_deref() == selected_path;
        let mut style = Style::default().fg(color);
        if is_selected {
            style = style.add_modifier(Modifier::REVERSED);
        }
        if cell.has_children {
            let block = Block::default()
                .borders(Borders::ALL)
                .title(truncate_label(
                    &cell.label,
                    rect.width.saturating_sub(2) as usize,
                ))
                .border_style(style);
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
            frame.render_widget(Block::default().style(fill), rect);
            if rect.width >= 2 {
                let label = truncate_label(&cell.label, rect.width as usize);
                let text_style = Style::default().bg(color).fg(Color::Black);
                let text_style = if is_selected {
                    text_style.add_modifier(Modifier::REVERSED)
                } else {
                    text_style
                };
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
fn draw_token_bar(frame: &mut Frame, area: Rect, header: &Header, token_bar: &TokenBar) {
    let inner_width = area.width.saturating_sub(2) as usize;
    let sent_str = with_thousands_separators(header.tokens_sent);
    let received_str = with_thousands_separators(header.tokens_received);
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
    let line1 = Line::from(vec![
        Span::raw("in  "),
        Span::styled(in_bar, Style::default().fg(Color::Cyan)),
        Span::raw(format!(" {sent_str}   ")),
        Span::raw("out  "),
        Span::styled(out_bar, Style::default().fg(Color::Yellow)),
        Span::raw(format!(" {received_str}")),
    ]);
    let line2 = if token_bar.prompts_running == 0 {
        Line::from("nothing in flight")
    } else {
        let plural = if token_bar.prompts_running == 1 {
            ""
        } else {
            "s"
        };
        // The orchestrator's own finding: a tree-loop pass's prompt,
        // and a `use: path:` child's, are counted as running even
        // with no `prompt_sent` in state to estimate from at all
        // (DESIGN.md §1.4) -- the trailing `+` says the estimate is a
        // lower bound, same as "effects planned" already does in the
        // header, rather than silently under-reporting it as exact.
        let lower_bound = if token_bar.estimated_input_is_lower_bound {
            "+"
        } else {
            ""
        };
        Line::from(format!(
            "now in ≈{}{lower_bound}   ({} prompt{plural} running)",
            with_thousands_separators(token_bar.estimated_input),
            token_bar.prompts_running
        ))
    };
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

fn draw_overlay(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    text: Vec<Line<'static>>,
    w: u16,
    h: u16,
) {
    let popup = centered(area, w, h);
    frame.render_widget(Clear, popup);
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
    "↑↓ / j k     move (the focused pane)",
    "PgUp PgDn    move a page",
    "Home End     jump to the first / last row",
    "g G          same as Home / End",
    "← →          collapse / expand (orchestration pane)",
    "tab          orchestration → details → state",
    "f            follow the running row (on by default)",
    "e            errors only",
    "/            filter",
    "v            show the full value",
    "c            cancel (confirm)",
    "q            quit (confirms while a run is going)",
    "?            this help",
    "",
    "no output token count until a prompt finishes",
    "press any key to close",
];

/// Wide enough for every [`HELP_TEXT`] line with none of them
/// wrapping (finding F7) — the longest is 51 columns; 4 columns of
/// margin plus the 2-column border leaves room to spare.
const HELP_OVERLAY_WIDTH: u16 = 57;

/// One redraw's whole layout (DESIGN.md §6.3, issue #527): the
/// header; a left column of the orchestration pane over details; a
/// right column of the state pane, the `tokens by key` treemap and
/// the log pane; and the token bar across the full bottom width. On
/// a small terminal, panes drop in the order `thresholds` documents
/// until a one-column layout (orchestration, details, log) is all
/// that's left — the header and the token bar never disappear.
/// `raw_state` is the engine's own last snapshot, untruncated —
/// `details`' own `prompt_sent`/`value` are already cut to
/// `DETAILS_PREVIEW_CHARS` for the details pane, which `v`'s own
/// full-value overlay (finding 5) must not be.
pub fn draw(
    frame: &mut Frame,
    state: &oscilloscope_core::render::RenderState,
    details: Option<&Details>,
    log_lines: &[String],
    app: &mut App,
    raw_state: Option<&serde_json::Value>,
) {
    let area = frame.area();
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(4),
        ])
        .split(area);

    draw_header(frame, sections[0], &state.header);
    draw_token_bar(frame, sections[2], &state.header, &state.token_bar);

    let body = sections[1];
    let visible = visible_rows(&state.rows, app);
    let selected_kind = app
        .selected_path
        .as_deref()
        .and_then(|p| state.rows.iter().find(|r| r.path == p))
        .map(|r| r.kind)
        .unwrap_or(RowKind::Unknown);

    let two_column = area.width >= thresholds::TWO_COLUMN_MIN_WIDTH
        && area.height >= thresholds::TWO_COLUMN_MIN_HEIGHT;

    if !two_column {
        app.state_viewport = 0;
        let parts = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(45),
                Constraint::Percentage(25),
                Constraint::Min(5),
            ])
            .split(body);
        draw_tree(frame, parts[0], &visible, app, &state.header.document);
        draw_details(frame, parts[1], details, selected_kind, app);
        draw_log(frame, parts[2], log_lines);
    } else {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(body);

        let left = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(columns[0]);
        draw_tree(frame, left[0], &visible, app, &state.header.document);
        draw_details(frame, left[1], details, selected_kind, app);

        let show_treemap = area.width >= thresholds::TREEMAP_MIN_WIDTH
            && area.height >= thresholds::TREEMAP_MIN_HEIGHT;
        let show_state = area.width >= thresholds::STATE_PANE_MIN_WIDTH
            && area.height >= thresholds::STATE_PANE_MIN_HEIGHT;

        if show_state && show_treemap {
            let right = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Percentage(30),
                    Constraint::Percentage(35),
                    Constraint::Min(5),
                ])
                .split(columns[1]);
            draw_state(frame, right[0], &state.state_rows, app);
            draw_treemap(
                frame,
                right[1],
                &state.treemap,
                app.selected_path.as_deref(),
            );
            draw_log(frame, right[2], log_lines);
        } else if show_state {
            let right = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Percentage(40), Constraint::Min(5)])
                .split(columns[1]);
            draw_state(frame, right[0], &state.state_rows, app);
            draw_log(frame, right[1], log_lines);
        } else {
            app.state_viewport = 0;
            draw_log(frame, columns[1], log_lines);
        }
    }

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
            );
        }
        Dialog::ConfirmCancel { .. } => {
            let text = vec![
                Line::from("cancel the run?"),
                Line::from(""),
                Line::from("y = cancel   n/esc = back"),
            ];
            draw_overlay(frame, area, " confirm ", text, 36, 5);
        }
        Dialog::FullValue(field) => {
            // Finding 5: reads straight from `raw_state`, bypassing
            // `details`' own truncated preview entirely — that's the
            // whole point of asking to see it in full.
            let full = app
                .selected_path
                .as_deref()
                .and_then(|p| oscilloscope_core::render::full_value(p, *field, raw_state));
            let text = vec![Line::from(full.unwrap_or_default())];
            draw_overlay(
                frame,
                area,
                " full value (press any key) ",
                text,
                area.width.saturating_sub(4),
                area.height.saturating_sub(4),
            );
        }
        Dialog::FilterInput(buf) => {
            let text = vec![
                Line::from(format!("/{buf}")),
                Line::from("enter = apply   esc = cancel"),
            ];
            draw_overlay(frame, area, " filter ", text, 40, 4);
        }
        Dialog::None => {}
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
    /// (`ask`) -- the orchestrator's own finding: a tree-loop pass's
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
        let details = render::details_for(selected, plan, state);
        let mut app = App::new(engine == "watch");
        app.selected_path = Some(selected.to_string());
        // The orchestrator's own regression: setting `selected_path`
        // directly (as every golden here does, to pin a specific row's
        // details) bypasses the key handling that would otherwise have
        // turned follow off for a manual selection -- left at its
        // `App::new` default of `true`, the title claimed "(following)"
        // over whatever row the test happened to pick, even one that
        // wasn't running at all. A test that means to show follow
        // actually choosing the running row builds its own `App`
        // instead of calling this helper.
        app.follow = false;
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw(f, &rendered, Some(&details), log_lines, &mut app, state))
            .unwrap();
        terminal.backend().to_string()
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
    /// fallback (orchestration/details/log stacked) is what gets
    /// exercised.
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
    /// is the plain, already-formatted `mm:ss.s text` strings `draw`
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
            "00:00.0 ▶ prime.a  tool".to_string(),
            "00:00.5 ✓ prime.a  0.5s  ok".to_string(),
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
        // The process exited and osp itself forwarded a signal
        // (`interrupted: true`), but the state it has is a snapshot
        // from *before* the final write landed (no `runtime` key at
        // all) -- a transient race, not an ended run. `prime.a`'s own
        // row still reads cancelled (DESIGN.md §2.1 rule 2), but the
        // header, which has no sign this run specifically failed, now
        // correctly reads "aborted" rather than reusing the row's own
        // `interrupted` flag (the bug the orchestrator measured: see
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
        // The orchestrator's own regression: a run the user cancelled
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
        // The orchestrator's own regression: an invalid document
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
            },
            "cof",
            "prime.a",
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn osp_watch() {
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

    /// The orchestrator's own regression: the existing goldens above
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
        assert!(app.follow, "issue #527: follow is on when the TUI starts");
        if let Some(target) = App::follow_target(&rendered.rows) {
            app.select_tree_path(Some(target.to_string()));
        }
        assert_eq!(
            app.selected_path.as_deref(),
            Some("prime.fan.iter_0"),
            "follow must choose the actually-running leaf, not the done one"
        );
        let details =
            render::details_for(app.selected_path.as_deref().unwrap(), &plan, Some(&state));
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw(f, &rendered, Some(&details), &[], &mut app, Some(&state)))
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
        if let Some(target) = App::follow_target(&rendered.rows) {
            app.select_tree_path(Some(target.to_string()));
        }
        assert_eq!(app.selected_path.as_deref(), Some("prime.step_44"));

        let details =
            render::details_for(app.selected_path.as_deref().unwrap(), &plan, Some(&state));
        let backend = TestBackend::new(70, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw(f, &rendered, Some(&details), &[], &mut app, Some(&state)))
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
        let details = render::details_for("n0", &plan, Some(&state));
        let mut app = App::new(false);
        app.selected_path = Some("n0".to_string());

        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let start = std::time::Instant::now();
        terminal
            .draw(|f| draw(f, &rendered, Some(&details), &[], &mut app, Some(&state)))
            .unwrap();
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(500),
            "one TestBackend draw of a 2000-node state took {elapsed:?}, over 5x the 100ms REDRAW_INTERVAL"
        );
    }

    /// The orchestrator's own finding: a tree-loop pass's prompt never
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

        let details = render::details_for("prime.fan.iter_0.ask", &plan, Some(&state));
        let mut app = App::new(false);
        app.selected_path = Some("prime.fan.iter_0.ask".to_string());
        app.follow = false;
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw(f, &rendered, Some(&details), &[], &mut app, Some(&state)))
            .unwrap();
        let text = terminal.backend().to_string();
        assert!(
            !text.contains("nothing in flight"),
            "a running tree-loop prompt must show in the token bar:\n{text}"
        );
        insta::assert_snapshot!(text);
    }
}
