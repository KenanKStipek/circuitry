//! Lays out one `RenderState` (DESIGN.md §6.3): the header, the plan
//! tree and details panes, the log pane, and the key's own overlays
//! (help, both confirms, the full-value view, the filter prompt).
//! The only module in this crate that imports `ratatui` widgets
//! directly, mirroring `oscilloscope-core`'s own "no terminal code"
//! split one level up.

use oscilloscope_core::model::{RunStatus, StatusKind};
use oscilloscope_core::render::{Details, Row, RowKind};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};

use crate::keys::{App, Dialog, Pane, visible_rows};

/// DESIGN.md §6.3's own glyph table. `Aborted` and `LikelyRunning`/
/// `RunningOrQueued` have no glyph of their own in that table — `?`
/// for an abort (nothing else in the set means "unknown"), and the
/// same `◐`/`◌` a confirmed running/queued row uses for a guess at
/// one, left visually distinct only by the dim "likely"/"≤" duration
/// text `row_line` adds.
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
        Span::styled(
            format!("{indent}{glyph} "),
            Style::default().fg(status_color(row.status.kind)),
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

fn draw_tree(frame: &mut Frame, area: Rect, rows: &[&Row], app: &App) {
    let items: Vec<ListItem> = rows
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
    let title = if app.follow {
        " plan (following) "
    } else {
        " plan "
    };
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
        lines.push(Line::from(format!("{key:<7}{value}")));
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

fn draw_details(
    frame: &mut Frame,
    area: Rect,
    details: Option<&Details>,
    kind: RowKind,
    app: &App,
) {
    let border_style = if app.pane == Pane::Details {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };
    let text = details
        .map(|d| details_lines(d, kind))
        .unwrap_or_else(|| vec![Line::from("(nothing selected)")]);
    let paragraph = Paragraph::new(text).wrap(Wrap { trim: false }).block(
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

const HELP_TEXT: &[&str] = &[
    "↑↓ / j k   move",
    "← →       collapse / expand",
    "tab       switch pane",
    "f         follow the running row",
    "e         errors only",
    "/         filter",
    "v         show the full value",
    "c         cancel (confirm)",
    "q         quit (confirms while a run is going)",
    "?         this help",
    "",
    "press any key to close",
];

/// One redraw (DESIGN.md §6.3): the header, the plan tree and its
/// selected row's details, the log pane, and whichever overlay the
/// current dialog calls for.
pub fn draw(
    frame: &mut Frame,
    state: &oscilloscope_core::render::RenderState,
    details: Option<&Details>,
    log_lines: &[String],
    app: &App,
) {
    let area = frame.area();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length((area.height / 4).max(5)),
        ])
        .split(area);

    draw_header(frame, rows[0], &state.header);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(rows[1]);

    let visible = visible_rows(&state.rows, app);
    let selected_kind = app
        .selected_path
        .as_deref()
        .and_then(|p| state.rows.iter().find(|r| r.path == p))
        .map(|r| r.kind)
        .unwrap_or(RowKind::Unknown);
    draw_tree(frame, body[0], &visible, app);
    draw_details(frame, body[1], details, selected_kind, app);
    draw_log(frame, rows[2], log_lines);

    match &app.dialog {
        Dialog::Help => {
            let text = HELP_TEXT.iter().map(|l| Line::from(*l)).collect();
            draw_overlay(frame, area, " help ", text, 48, 14);
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
            let full = app.selected_path.as_deref().and_then(|_| {
                details.and_then(|d| match field {
                    oscilloscope_core::render::FullValueField::PromptSent => d.prompt_sent.clone(),
                    oscilloscope_core::render::FullValueField::Value => d.value.clone(),
                })
            });
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
