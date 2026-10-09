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
/// current dialog calls for. `raw_state` is the engine's own last
/// snapshot, untruncated — `details`' own `prompt_sent`/`value` are
/// already cut to `DETAILS_PREVIEW_CHARS` for the details pane, which
/// `v`'s own full-value overlay (finding 5) must not be.
pub fn draw(
    frame: &mut Frame,
    state: &oscilloscope_core::render::RenderState,
    details: Option<&Details>,
    log_lines: &[String],
    app: &App,
    raw_state: Option<&serde_json::Value>,
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
        EffectPath, LeafKind, LoopFlow, LoopId, LoopSpec, NodeKind, OnError, Op, ParamNode,
        Program, Region, ToolOp,
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

    #[allow(clippy::too_many_arguments)]
    fn snapshot(
        plan: &PlanTree,
        model: &mut RunModel,
        state: Option<&Value>,
        process: ProcessState,
        engine: &str,
        selected: &str,
    ) -> String {
        let rendered = render::build(
            "do-thing.yml",
            engine,
            plan,
            model,
            state,
            process,
            5.0,
            &[],
        );
        let details = render::details_for(selected, plan, state);
        let mut app = App::new(engine == "watch");
        app.selected_path = Some(selected.to_string());
        let backend = TestBackend::new(70, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw(f, &rendered, Some(&details), &[], &app, state))
            .unwrap();
        terminal.backend().to_string()
    }

    #[test]
    fn pending_plan() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let text = snapshot(
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
            ProcessState::Exited { interrupted: false },
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
            ProcessState::Exited { interrupted: false },
            "cof",
            "prime.b",
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn cancelled() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "provider": "shell"}}
        }});
        let text = snapshot(
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Exited { interrupted: true },
            "cof",
            "prime.a",
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn aborted_no_final_state() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let text = snapshot(
            &plan,
            &mut model,
            None,
            ProcessState::Exited { interrupted: false },
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
}
