//! The Flow screen's rail model (issue #691, part 3 of #677):
//! stations in plan order, forking into lanes at a decision or loop
//! and rejoining, built only while screen `2` is actually on display
//! (the same performance rule `screens::build_cost_screen` already
//! follows for screen `3`). Replaces the indented tree
//! [`crate::flow`] used to build.
//!
//! `rows` (`render::build`'s own depth-first list) already nests a
//! named `if`'s taken branch and a loop's own passes properly by
//! depth -- unlike the old tree screen, this module never needs to
//! re-derive that shape from the plan's own branch paths; it reads
//! it straight off `rows`. What `rows` does *not* carry is the
//! untaken branch (nothing ran there, so it has no row at all) or a
//! loop pass that has not started yet (same reason) -- both are
//! synthesised here, from the plan and, for a pass, from the run's
//! own `each.in` input.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::model::{NodeMeta, StatusKind, subtree_tokens};
use crate::plan::{PlanEntryKind, PlanTree};
use crate::render::{Header, LoopProgress, Row, RowKind};

/// A connector's or glyph's own place on the run's own timeline
/// (DESIGN.md §2.3: "Connectors are `──` for the past, the accent
/// colour for the live edge, faint ahead").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeStyle {
    Past,
    Live,
    Ahead,
}

pub fn edge_style(status: StatusKind) -> EdgeStyle {
    match status {
        StatusKind::Pending | StatusKind::Skipped => EdgeStyle::Ahead,
        StatusKind::Running | StatusKind::LikelyRunning | StatusKind::RunningOrQueued => {
            EdgeStyle::Live
        }
        StatusKind::Done
        | StatusKind::Failed
        | StatusKind::FailedHandled
        | StatusKind::Cancelled
        | StatusKind::Aborted => EdgeStyle::Past,
    }
}

/// A station's own second line (DESIGN.md §2.3's headline figure):
/// `215 t +6%`, `3.0s`, `→ then`, `0/3 passes`.
#[derive(Debug, Clone, PartialEq)]
pub enum Headline {
    /// The run's own start clock, `Start`'s own headline.
    Clock(String),
    /// A finished prompt's own tokens, with the deviation this
    /// module leaves for the caller to colour (`total` vs `usual`).
    Tokens {
        total: u64,
        usual: Option<u64>,
    },
    /// A loop pass's own finished prompt: the pass's own label
    /// (`cats`) takes the deviation's own place, so no room is left
    /// for it (DESIGN.md's own rail frame).
    PassTokens {
        label: String,
        total: u64,
    },
    Duration(f64),
    /// A decision's own branch, `→ then`/`→ else`, or just `→ …`
    /// before it resolves.
    Branch(Option<String>),
    LoopProgress {
        done: u64,
        total: Option<u64>,
    },
    /// An ahead pass's own label, standing in for a token figure it
    /// doesn't have yet.
    PassLabel(String),
    /// An ahead, top-level tool: `tool · ls`.
    ToolAhead(String),
    /// An ahead, top-level prompt: `usual 402`/`usual —`.
    UsualTokens(Option<u64>),
    /// The untaken branch's own ghost station, second line.
    NotTaken,
    Blank,
}

/// A station's own third line (DESIGN.md §2.3: "the bar or detail").
#[derive(Debug, Clone, PartialEq)]
pub enum Detail {
    Engine(String),
    Bar {
        total: u64,
        unit: u64,
    },
    Command(String),
    /// A named decision's own condition, already shortened
    /// (`kind!=""`, §2.3's cramped station width, never the Tape's
    /// own spaced-out `kind != ""`).
    Condition(String),
    /// A loop's own tokens so far.
    Tokens(u64),
    UsualTokens(Option<u64>),
    Queued,
    Blank,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Station {
    pub path: String,
    pub selectable: bool,
    /// `Root` for the Start/End markers -- drawn with the fixed `■`
    /// glyph regardless of status, never through
    /// [`crate::render::RowKind`]'s own glyph table.
    pub is_edge: bool,
    pub row_kind: RowKind,
    pub status: StatusKind,
    pub name: String,
    pub headline: Headline,
    pub detail: Detail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkKind {
    Decision,
    Loop,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Lane {
    pub items: Vec<RailItem>,
    /// `Some(name)` only for a decision's own untaken lane -- a
    /// single ghost station with no real content, never drawn
    /// through the ordinary station rules.
    pub ghost: Option<String>,
    pub running: bool,
    pub waiting: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Fork {
    pub path: String,
    pub kind: ForkKind,
    pub source_status: StatusKind,
    pub lanes: Vec<Lane>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RailItem {
    Station(Station),
    Fork(Fork),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Rail {
    pub items: Vec<RailItem>,
}

struct Ctx<'a> {
    flat: &'a BTreeMap<String, NodeMeta>,
    plan: &'a PlanTree,
    header: &'a Header,
    state: Option<&'a Value>,
}

fn row_tokens(path: &str, flat: &BTreeMap<String, NodeMeta>) -> Option<u64> {
    let (sent, received) = subtree_tokens(path, flat);
    let total = sent + received;
    (total > 0).then_some(total)
}

fn usual_tokens(path: &str, ctx: &Ctx<'_>) -> Option<u64> {
    crate::screens::tape_row_usual_tokens(path, ctx.plan, ctx.header)
}

fn command_text(path: &str, ctx: &Ctx<'_>) -> String {
    let Some(node) = ctx.flat.get(path) else {
        return String::new();
    };
    let Some(command) = node
        .meta
        .pointer("/params_rendered/command")
        .and_then(Value::as_str)
    else {
        return String::new();
    };
    let args = node
        .meta
        .pointer("/params_rendered/args")
        .and_then(Value::as_array)
        .map(|args| {
            args.iter()
                .map(crate::render::display_value)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|a| !a.is_empty());
    match args {
        Some(args) => format!("{command} {args}"),
        None => command.to_string(),
    }
}

fn condition_text(path: &str, ctx: &Ctx<'_>) -> Option<String> {
    let entry = ctx.plan.match_path(path)?.entries.first()?;
    let PlanEntryKind::If {
        condition: Some(condition),
        ..
    } = &entry.kind
    else {
        return None;
    };
    Some(condense_condition(&condition.text))
}

/// The rail's own cramped condition text (DESIGN.md §2.3's ten-cell
/// station): the same `state.prime.`/`.value` noise the Tape already
/// strips, with the surrounding whitespace squeezed out too, since a
/// spaced-out `kind != ""` never fits the station's own width.
fn condense_condition(text: &str) -> String {
    text.replace("state.prime.", "")
        .replace(".value", "")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

fn plain_station(row: &Row, ctx: &Ctx<'_>) -> Station {
    let (headline, detail) = match row.kind {
        RowKind::Prompt => prompt_headline_detail(row, ctx),
        RowKind::Tool => tool_headline_detail(row, ctx),
        _ => (Headline::Blank, Detail::Blank),
    };
    Station {
        path: row.path.clone(),
        selectable: true,
        is_edge: false,
        row_kind: row.kind,
        status: row.status.kind,
        name: row.label.clone(),
        headline,
        detail,
    }
}

fn prompt_headline_detail(row: &Row, ctx: &Ctx<'_>) -> (Headline, Detail) {
    match row.status.kind {
        StatusKind::Done => {
            let total = row_tokens(&row.path, ctx.flat).unwrap_or(0);
            let usual = usual_tokens(&row.path, ctx);
            (
                Headline::Tokens { total, usual },
                Detail::Bar {
                    total,
                    unit: rail_unit(ctx),
                },
            )
        }
        StatusKind::Running | StatusKind::LikelyRunning | StatusKind::RunningOrQueued => (
            Headline::Duration(row.duration_s.unwrap_or(0.0)),
            Detail::Blank,
        ),
        _ => (
            Headline::UsualTokens(usual_tokens(&row.path, ctx)),
            Detail::Blank,
        ),
    }
}

fn tool_headline_detail(row: &Row, ctx: &Ctx<'_>) -> (Headline, Detail) {
    let command = command_text(&row.path, ctx);
    match row.status.kind {
        StatusKind::Done => (
            Headline::Duration(row.duration_s.unwrap_or(0.0)),
            Detail::Command(command),
        ),
        StatusKind::Running | StatusKind::LikelyRunning | StatusKind::RunningOrQueued => (
            Headline::Duration(row.duration_s.unwrap_or(0.0)),
            Detail::Command(command),
        ),
        _ => (
            Headline::ToolAhead(command),
            Detail::UsualTokens(usual_tokens(&row.path, ctx)),
        ),
    }
}

/// §3.5's unit, reused verbatim from the Tape (DESIGN.md §2.3:
/// "reuse the Tape's row forms and token unit") -- the same
/// derivation `screens::build_tape` itself makes, from the usual
/// largest step across every plan-known step history has a figure
/// for.
fn rail_unit(ctx: &Ctx<'_>) -> u64 {
    let largest_usual_step = ctx
        .header
        .history_step_usual
        .values()
        .filter_map(|s| s.tokens_p50)
        .max();
    crate::screens::tape_unit(largest_usual_step)
}

fn decision_station(row: &Row, ctx: &Ctx<'_>) -> Station {
    let branch = ctx.flat.get(&row.path).and_then(|n| n.branch.clone());
    let condition = condition_text(&row.path, ctx).unwrap_or_default();
    Station {
        path: row.path.clone(),
        selectable: true,
        is_edge: false,
        row_kind: RowKind::If,
        status: row.status.kind,
        name: row.label.clone(),
        headline: Headline::Branch(branch),
        detail: Detail::Condition(condition),
    }
}

fn loop_station(row: &Row, ctx: &Ctx<'_>) -> Station {
    let progress = row.loop_progress.unwrap_or(LoopProgress {
        done: 0,
        total: None,
        eta_s: None,
    });
    let tokens = row_tokens(&row.path, ctx.flat);
    Station {
        path: row.path.clone(),
        selectable: true,
        is_edge: false,
        row_kind: RowKind::Loop,
        status: row.status.kind,
        name: row.label.clone(),
        headline: Headline::LoopProgress {
            done: progress.done,
            total: progress.total,
        },
        detail: tokens.map(Detail::Tokens).unwrap_or(Detail::Blank),
    }
}

/// Walks `rows[start..]` for every direct child at `depth` (a
/// station, or a decision/loop's own station plus its fork),
/// stopping at the first row shallower than `depth` -- the index
/// returned is exactly that stopping point, so a caller consuming
/// one subtree (a branch, a pass) gets back precisely where its own
/// next sibling starts.
fn build_level(rows: &[Row], start: usize, depth: usize, ctx: &Ctx<'_>) -> (Vec<RailItem>, usize) {
    let mut items = Vec::new();
    let mut i = start;
    while i < rows.len() && rows[i].depth >= depth {
        if rows[i].depth > depth {
            // A deeper row than expected at this position means an
            // earlier sibling's own fork builder didn't consume its
            // whole subtree -- never happens via the recursive calls
            // below, but skipped rather than panicking on a plan
            // shape this module doesn't yet know about.
            i += 1;
            continue;
        }
        let row = &rows[i];
        match row.kind {
            RowKind::If => {
                items.push(RailItem::Station(decision_station(row, ctx)));
                let (fork, next) = build_decision_fork(rows, i, ctx);
                items.push(RailItem::Fork(fork));
                i = next;
            }
            RowKind::Loop => {
                items.push(RailItem::Station(loop_station(row, ctx)));
                let (fork, next) = build_loop_fork(rows, i, ctx);
                items.push(RailItem::Fork(fork));
                i = next;
            }
            _ => {
                items.push(RailItem::Station(plain_station(row, ctx)));
                i += 1;
                while i < rows.len() && rows[i].depth > depth {
                    i += 1;
                }
            }
        }
    }
    (items, i)
}

fn untaken_branch_paths(path: &str, taken: Option<&str>, ctx: &Ctx<'_>) -> Option<Vec<String>> {
    let m = ctx.plan.match_path(path)?;
    for e in m.entries {
        if let PlanEntryKind::If {
            named: true,
            then_paths,
            else_paths,
            ..
        } = &e.kind
        {
            return match taken {
                Some("then") => Some(else_paths.clone()),
                Some("else") => Some(then_paths.clone()),
                _ => None,
            };
        }
    }
    None
}

fn step_label(path: &str) -> String {
    path.rsplit('.').next().unwrap_or(path).to_string()
}

fn build_decision_fork(rows: &[Row], i: usize, ctx: &Ctx<'_>) -> (Fork, usize) {
    let row = &rows[i];
    let depth = row.depth;
    let (taken_items, next) = build_level(rows, i + 1, depth + 1, ctx);
    let branch = ctx.flat.get(&row.path).and_then(|n| n.branch.clone());
    let mut lanes = vec![Lane {
        items: taken_items,
        ghost: None,
        running: edge_style(row.status.kind) == EdgeStyle::Live,
        waiting: false,
    }];
    if let Some(untaken) = untaken_branch_paths(&row.path, branch.as_deref(), ctx) {
        if let Some(first) = untaken.first() {
            lanes.push(Lane {
                items: Vec::new(),
                ghost: Some(step_label(first)),
                running: false,
                waiting: false,
            });
        }
    }
    (
        Fork {
            path: row.path.clone(),
            kind: ForkKind::Decision,
            source_status: row.status.kind,
            lanes,
        },
        next,
    )
}

/// `path`'s own slot count (DESIGN.md §2.3: "a loop forks into one
/// lane per slot (`max_concurrency` or the pass count when
/// unbounded)") -- the loop's own known total pass count wins once
/// it has one (a bounded `each` loop, or a `while`/dynamic loop that
/// has finished), `max_concurrency` next, and failing both, however
/// many passes are actually observed so far.
fn loop_lane_count(
    path: &str,
    progress: Option<LoopProgress>,
    observed: u64,
    ctx: &Ctx<'_>,
) -> u64 {
    if let Some(total) = progress.and_then(|p| p.total) {
        return total.max(observed);
    }
    let max_concurrency = ctx.plan.match_path(path).and_then(|m| {
        m.entries.iter().find_map(|e| match &e.kind {
            PlanEntryKind::Loop {
                max_concurrency, ..
            } => *max_concurrency,
            _ => None,
        })
    });
    max_concurrency
        .map(u64::from)
        .unwrap_or(observed)
        .max(observed)
}

/// A pass's own `each.in` value (DESIGN.md §2.3's `cats`/`dogs`/
/// `birds`): resolved straight from the run's own input, so it is
/// known for a pass that has not even started yet, not only one
/// observed in `--events`.
pub(crate) fn pass_each_value(
    loop_path: &str,
    index: u64,
    plan: &PlanTree,
    state: Option<&Value>,
) -> Option<String> {
    let each_in_path = plan.match_path(loop_path).and_then(|m| {
        m.entries.iter().find_map(|e| match &e.kind {
            PlanEntryKind::Loop { each_in_path, .. } => each_in_path.clone(),
            _ => None,
        })
    })?;
    let pointer = format!("/{}", each_in_path.replace('.', "/"));
    state?
        .pointer(&pointer)?
        .as_array()?
        .get(index as usize)?
        .as_str()
        .map(str::to_string)
}

/// An ahead pass's own lane (DESIGN.md §2.3's folded loop example,
/// `birds` / `usual 155` / `sleep 1` / `queued`): shaped off
/// `template`, the first real pass's own stations, with every
/// prompt reading the pass's own label (standing in for tokens it
/// doesn't have yet) and every tool reading its own usual command,
/// both still queued.
fn ahead_pass_lane(template: &[RailItem], loop_path: &str, index: u64, ctx: &Ctx<'_>) -> Lane {
    let label = pass_each_value(loop_path, index, ctx.plan, ctx.state)
        .unwrap_or_else(|| format!("pass {}", index + 1));
    let items = template
        .iter()
        .map(|item| match item {
            RailItem::Station(s) => {
                let (headline, detail) = match s.row_kind {
                    RowKind::Prompt => (
                        Headline::PassLabel(label.clone()),
                        Detail::UsualTokens(usual_tokens(&s.path, ctx)),
                    ),
                    RowKind::Tool => (
                        Headline::ToolAhead(command_text(&s.path, ctx)),
                        Detail::Queued,
                    ),
                    _ => (Headline::Blank, Detail::Blank),
                };
                RailItem::Station(Station {
                    path: format!("{loop_path}.iter_ahead_{index}.{}", step_label(&s.path)),
                    selectable: false,
                    is_edge: false,
                    row_kind: s.row_kind,
                    status: StatusKind::Pending,
                    name: step_label(&s.path),
                    headline,
                    detail,
                })
            }
            RailItem::Fork(_) => item.clone(),
        })
        .collect();
    Lane {
        items,
        ghost: None,
        running: false,
        waiting: true,
    }
}

fn build_loop_fork(rows: &[Row], i: usize, ctx: &Ctx<'_>) -> (Fork, usize) {
    let row = &rows[i];
    let depth = row.depth;
    let mut lanes = Vec::new();
    let mut j = i + 1;
    let mut observed = 0u64;
    while j < rows.len() && rows[j].depth > depth {
        if rows[j].depth == depth + 1 && rows[j].kind == RowKind::Pass {
            let (items, next) = build_level(rows, j + 1, depth + 2, ctx);
            let running = items.iter().any(
                |it| matches!(it, RailItem::Station(s) if edge_style(s.status) == EdgeStyle::Live),
            );
            lanes.push(Lane {
                items,
                ghost: None,
                running,
                waiting: false,
            });
            observed += 1;
            j = next;
        } else {
            j += 1;
        }
    }
    let lane_count = loop_lane_count(&row.path, row.loop_progress, observed, ctx);
    let template: Vec<RailItem> = lanes.first().map(|l| l.items.clone()).unwrap_or_default();
    let mut index = observed;
    while index < lane_count {
        lanes.push(ahead_pass_lane(&template, &row.path, index, ctx));
        index += 1;
    }
    (
        Fork {
            path: row.path.clone(),
            kind: ForkKind::Loop,
            source_status: row.status.kind,
            lanes,
        },
        j,
    )
}

fn start_station(header: &Header, zone: &dyn crate::format::ClockZone) -> Station {
    let clock = header
        .run_start_ts
        .as_deref()
        .and_then(|ts| crate::format::format_clock_time(ts, zone));
    Station {
        path: String::new(),
        selectable: true,
        is_edge: true,
        row_kind: RowKind::Root,
        status: StatusKind::Done,
        name: "Start".to_string(),
        headline: clock.map(Headline::Clock).unwrap_or(Headline::Blank),
        detail: Detail::Engine(header.engine.clone()),
    }
}

fn end_station(header: &Header) -> Station {
    let status = match header.status {
        crate::model::RunStatus::Ok => StatusKind::Done,
        crate::model::RunStatus::Failed | crate::model::RunStatus::Aborted => StatusKind::Failed,
        crate::model::RunStatus::Cancelled => StatusKind::Cancelled,
        crate::model::RunStatus::Running => StatusKind::Pending,
    };
    Station {
        path: String::new(),
        selectable: true,
        is_edge: true,
        row_kind: RowKind::Root,
        status,
        name: "End".to_string(),
        headline: Headline::Blank,
        detail: Detail::Blank,
    }
}

/// Builds the Flow screen's own rail (issue #691): `rows[0]` is
/// always the document root (`render::build`'s own convention); its
/// children, in plan order, become the rail's own items.
pub fn build_rail(
    rows: &[Row],
    flat: &BTreeMap<String, NodeMeta>,
    plan: &PlanTree,
    header: &Header,
    state: Option<&Value>,
    zone: &dyn crate::format::ClockZone,
) -> Rail {
    let ctx = Ctx {
        flat,
        plan,
        header,
        state,
    };
    let mut items = vec![RailItem::Station(start_station(header, zone))];
    if let Some(root) = rows.first() {
        let (children, _) = build_level(rows, 1, root.depth + 1, &ctx);
        items.extend(children);
    }
    items.push(RailItem::Station(end_station(header)));
    Rail { items }
}

pub const STATION_WIDTH: usize = 10;
pub const NORMAL_CONNECTOR: usize = 3;
pub const FOLD_BAND_WIDTH: usize = 16;
/// The fork's own left lead-in, `"─┬─ "` (DESIGN.md §2.3's rail).
pub const FORK_LEAD: usize = 4;
/// The fork's own right lead-out on the top lane, `"─┴── "`.
pub const FORK_TAIL: usize = 5;

/// The total character width a rail item draws when every fork on
/// it is open, used to pad sibling lanes to the same width before
/// their own join (DESIGN.md §2.3) -- whether a given fork is
/// actually drawn open or folded into its own band is a display-time
/// decision (screen width, lane count, the `+`/`-` override), made by
/// the caller, never here.
pub fn item_width(item: &RailItem) -> usize {
    match item {
        RailItem::Station(_) => STATION_WIDTH,
        RailItem::Fork(fork) => fork_width(fork),
    }
}

pub fn lane_width(items: &[RailItem]) -> usize {
    if items.is_empty() {
        return STATION_WIDTH;
    }
    items.iter().map(item_width).sum::<usize>() + NORMAL_CONNECTOR * (items.len() - 1)
}

/// `fork`'s own width fully open -- never folded, regardless of its
/// own lane count; see [`item_width`]'s own doc comment.
pub fn fork_width(fork: &Fork) -> usize {
    let max_lane = fork
        .lanes
        .iter()
        .map(|l| lane_width(&l.items))
        .max()
        .unwrap_or(STATION_WIDTH);
    FORK_LEAD + max_lane + FORK_TAIL
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edge_style_matches_the_rail_s_past_live_ahead_rule() {
        assert_eq!(edge_style(StatusKind::Done), EdgeStyle::Past);
        assert_eq!(edge_style(StatusKind::Running), EdgeStyle::Live);
        assert_eq!(edge_style(StatusKind::Pending), EdgeStyle::Ahead);
        assert_eq!(edge_style(StatusKind::Skipped), EdgeStyle::Ahead);
    }

    #[test]
    fn condense_condition_drops_the_state_prefix_and_every_space() {
        assert_eq!(
            condense_condition(r#"state.prime.kind.value != """#),
            r#"kind!=""#.to_string() + "\""
        );
    }
}
