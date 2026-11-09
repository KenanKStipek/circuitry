//! The Flow screen's rail model (issue #691, part 3 of #677):
//! stations in plan order, forking into lanes at a decision or loop
//! and rejoining, built only while screen `2` is actually on display
//! (the same performance rule `screens::build_cost_screen` already
//! follows for screen `3`). Replaces the indented tree
//! [`crate::flow`] used to build.
//!
//! `rows` (`render::build`'s own depth-first list) already nests a
//! named `if`'s taken branch and a loop's own passes properly by
//! depth -- this module reads that shape straight off `rows` for a
//! named `if`. An *unnamed* one writes no state node of its own, so
//! it has no row at all to read a shape off of: its own decision is
//! spliced in here instead, from `PlanTree::unnamed_if_decisions`,
//! the same way the tree screen this replaces used to
//! (`unnamed_branch_state`). What `rows` does not carry either way
//! is the untaken branch (nothing ran there, so it has no row at
//! all) or a loop pass that has not started yet (same reason) --
//! both are synthesised here, from the plan and, for a pass, from
//! the run's own `each.in` input.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::model::{NodeMeta, StatusKind, subtree_tokens};
use crate::plan::{PlanEntryKind, PlanTree, UnnamedIfDecision};
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
    /// An ahead pass's own queued tool, just its own command --
    /// the pass's label already marks it as a preview, so unlike
    /// `ToolAhead` there's no room left for a `tool ·` prefix too.
    QueuedCommand(String),
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
    // An unnamed `if`'s own branch evidence is read from here, never
    // the (possibly folded) `rows` the rest of this module walks --
    // quick-step folding or a breadcrumb can make a branch's own
    // first step vanish from `rows` well before the branch itself
    // stops being real (`render::RenderState::unfolded_rows`'s own
    // doc).
    unfolded_rows: &'a [Row],
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
        _ => {
            // §2.1's Starting state: a tool ahead reads its own static
            // command, when every part of it is a plain literal
            // (issue #678), not just the runtime's own (empty, since
            // it hasn't run yet) `params_rendered` -- same source as
            // the Tape's own `tape_row_content`.
            let command = ctx
                .plan
                .match_path(&row.path)
                .and_then(|m| m.entries.first())
                .and_then(|e| e.tool_command.clone())
                .unwrap_or(command);
            (
                Headline::ToolAhead(command),
                Detail::UsualTokens(usual_tokens(&row.path, ctx)),
            )
        }
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
fn build_level(
    rows: &[Row],
    start: usize,
    depth: usize,
    host: &str,
    ctx: &Ctx<'_>,
) -> (Vec<RailItem>, usize) {
    let mut items = Vec::new();
    let unnamed = ctx
        .plan
        .unnamed_if_decisions()
        .iter()
        .filter(|d| d.host_path == host)
        .collect::<Vec<_>>();
    let mut unnamed_emitted = vec![false; unnamed.len()];
    let mut index_for_host = 0usize;
    let mut hidden: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
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
        if hidden.contains(&row.path) {
            i += 1;
            while i < rows.len() && rows[i].depth > depth {
                i += 1;
            }
            continue;
        }
        if let Some(pos) = (0..unnamed.len()).find(|&k| {
            !unnamed_emitted[k]
                && unnamed[k]
                    .then_paths
                    .iter()
                    .chain(unnamed[k].else_paths.iter())
                    .any(|p| p == &row.path)
        }) {
            unnamed_emitted[pos] = true;
            let index = index_for_host;
            index_for_host += 1;
            let (station, fork, consumed, hide) =
                build_unnamed_decision(unnamed[pos], rows, i, depth, index, ctx);
            items.push(RailItem::Station(station));
            items.push(RailItem::Fork(fork));
            hidden.extend(hide);
            i += consumed;
            continue;
        }
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
    // Any decision this level never found a row for (neither
    // branch has started) is still a real, pending `if` -- placed
    // right after its own host's row, the same way the removed tree
    // screen's `by_host_after` fallback did.
    for (k, decision) in unnamed.iter().enumerate() {
        if unnamed_emitted[k] {
            continue;
        }
        let index = index_for_host;
        index_for_host += 1;
        let (station, fork, _, _) =
            build_unnamed_decision(decision, rows, rows.len(), depth, index, ctx);
        items.push(RailItem::Station(station));
        items.push(RailItem::Fork(fork));
    }
    (items, i)
}

/// An unnamed `if`'s own decision (DESIGN.md: "a decision forks into
/// its taken and untaken lanes" -- no exception for an unnamed one),
/// spliced in here since it writes no state node, and so no row, of
/// its own: `anchor_from` is where its own evidence search starts
/// (the row that triggered this call, when there is one), `depth`
/// the level it belongs at. Returns the synthetic station, its fork,
/// and how many rows at `rows[anchor_from..]` the taken branch's own
/// content consumed (`0` with no evidence yet, or the vacuous-else
/// "nothing ran" case).
fn build_unnamed_decision(
    decision: &UnnamedIfDecision,
    rows: &[Row],
    anchor_from: usize,
    depth: usize,
    index_among_siblings: usize,
    ctx: &Ctx<'_>,
) -> (Station, Fork, usize, Vec<String>) {
    let (branch, nothing_ran) = unnamed_branch_state(decision, ctx.unfolded_rows);
    let same_names = !decision.then_paths.is_empty()
        && decision.then_paths == decision.else_paths
        && branch.is_none();
    let suffix = if index_among_siblings == 0 {
        String::new()
    } else {
        format!("_{index_among_siblings}")
    };
    let path = format!("{}.__if{suffix}", decision.host_path);
    let (taken_paths, consumed) = match branch.as_deref() {
        Some("then") if !nothing_ran => (&decision.then_paths, decision.then_paths.len()),
        Some("else") if !nothing_ran => (&decision.else_paths, decision.else_paths.len()),
        _ => (&decision.then_paths, 0),
    };
    let (taken_items, taken_running, consumed_rows) = if consumed > 0 {
        let end = branch_subtree_end(rows, anchor_from, depth, taken_paths);
        let (items, _) = build_level(&rows[..end], anchor_from, depth, &path, ctx);
        let running = items.iter().any(
            |it| matches!(it, RailItem::Station(s) if edge_style(s.status) == EdgeStyle::Live),
        );
        (items, running, end - anchor_from)
    } else {
        (Vec::new(), false, 0)
    };
    let mut lanes = vec![Lane {
        items: taken_items,
        ghost: None,
        running: taken_running,
        waiting: false,
    }];
    let untaken_paths = match branch.as_deref() {
        Some("then") => Some(&decision.else_paths),
        Some("else") if !nothing_ran => Some(&decision.then_paths),
        _ => None,
    };
    // The untaken side's own paths still get a real row each (an
    // unnamed `if` has no `meta.branch` to suppress them with
    // ahead of time, unlike a named one) -- hidden from the rest of
    // this level's own walk rather than drawn a second time, as a
    // redundant plain sibling, once the ghost lane above already
    // stands in for the whole branch. Never a path the taken side
    // also claims (the parity case: `then`/`else` reusing one step
    // name), or the branch that actually ran would vanish too.
    let mut hide = Vec::new();
    if let Some(untaken) = untaken_paths {
        if let Some(first) = untaken.first() {
            lanes.push(Lane {
                items: Vec::new(),
                ghost: Some(step_label(first)),
                running: false,
                waiting: false,
            });
        }
        hide.extend(untaken.iter().filter(|p| !taken_paths.contains(p)).cloned());
    }
    let status = if branch.is_some() {
        StatusKind::Done
    } else {
        StatusKind::Pending
    };
    let name = if same_names {
        "if \u{2192} ?".to_string()
    } else {
        match &decision.condition {
            Some(c) => format!("if {}", condense_condition(&c.text)),
            None => "if".to_string(),
        }
    };
    let condition = decision
        .condition
        .as_ref()
        .map(|c| condense_condition(&c.text))
        .unwrap_or_default();
    let station = Station {
        path,
        selectable: true,
        is_edge: false,
        row_kind: RowKind::If,
        status,
        name,
        headline: Headline::Branch(branch),
        detail: Detail::Condition(condition),
    };
    let fork = Fork {
        path: decision.host_path.clone(),
        kind: ForkKind::Decision,
        source_status: status,
        lanes,
    };
    (station, fork, consumed_rows, hide)
}

/// Where an unnamed decision's own taken branch ends, starting its
/// search at `start` (its own anchor row, already known to match):
/// every row at `depth` whose own path is one of `taken_paths`, each
/// together with its own descendants (deeper rows immediately
/// following it), consumed in turn until a `depth` row's path is
/// not one of them -- the first row outside the branch's own direct
/// contribution, same stopping rule `build_level`'s own plain-row
/// case already uses for one row's whole subtree.
fn branch_subtree_end(rows: &[Row], start: usize, depth: usize, taken_paths: &[String]) -> usize {
    let mut i = start;
    while i < rows.len() && rows[i].depth == depth && taken_paths.iter().any(|p| *p == rows[i].path)
    {
        i += 1;
        while i < rows.len() && rows[i].depth > depth {
            i += 1;
        }
    }
    i
}

/// Whether any row in `rows` has one of `paths` -- an unnamed `if`'s
/// own branch, read the same way a named one's is, just without a
/// `meta.branch` to ask directly (DESIGN.md: "a decision forks into
/// its taken and untaken lanes", no exception for an unnamed one).
fn any_branch_started(rows: &[Row], paths: &[String]) -> bool {
    paths.iter().any(|p| {
        rows.iter().any(|r| {
            &r.path == p && !matches!(r.status.kind, StatusKind::Pending | StatusKind::Skipped)
        })
    })
}

/// An unnamed `if`'s own branch, inferred from which side's own
/// paths actually started (issue #635; ported from the removed tree
/// screen's own `unnamed_branch_state` verbatim) -- `then`/`else`
/// once one side has real evidence and the other does not, else
/// (vacuously, with `nothing_ran` set) once an `if` with no `else`
/// is settled that its own `then` never will be: its own host
/// stopped running, or a later step in the same chain already
/// started, proving this one's own branch is over either way.
fn unnamed_branch_state(decision: &UnnamedIfDecision, rows: &[Row]) -> (Option<String>, bool) {
    let then_started = any_branch_started(rows, &decision.then_paths);
    let else_started = any_branch_started(rows, &decision.else_paths);
    let host_done = rows
        .iter()
        .find(|r| r.path == decision.host_path)
        .is_some_and(|r| {
            matches!(
                r.status.kind,
                StatusKind::Done
                    | StatusKind::Failed
                    | StatusKind::FailedHandled
                    | StatusKind::Cancelled
            )
        });
    let later_sibling_started = any_branch_started(rows, &decision.later_siblings);
    if then_started && !else_started {
        (Some("then".to_string()), false)
    } else if else_started && !then_started {
        (Some("else".to_string()), false)
    } else if decision.else_paths.is_empty() && (host_done || later_sibling_started) {
        (Some("else".to_string()), true)
    } else {
        (None, false)
    }
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
    let (taken_items, next) = build_level(rows, i + 1, depth + 1, &row.path, ctx);
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
                // A loop usually has more passes than slots, so the
                // template's own path is always the first real
                // pass's own (`iter_0`) -- but the history this own
                // slot actually keeps is per *position*, not per
                // pass, so an ahead lane further along reads its own
                // slot's own history, not `iter_0`'s.
                let slot_path = with_iter_index(&s.path, index);
                let (headline, detail) = match s.row_kind {
                    RowKind::Prompt => (
                        Headline::PassLabel(label.clone()),
                        Detail::UsualTokens(usual_tokens(&slot_path, ctx)),
                    ),
                    RowKind::Tool => (
                        Headline::QueuedCommand(command_text(&s.path, ctx)),
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

/// Swaps a template station's own `iter_0` segment for whichever
/// ahead slot it's standing in for (DESIGN.md §2.3's own `birds`
/// example: `usual 155`, iter_2's own history, not iter_0's).
fn with_iter_index(path: &str, index: u64) -> String {
    let mut replaced = false;
    path.split('.')
        .map(|part| {
            if !replaced
                && part
                    .strip_prefix("iter_")
                    .is_some_and(|n| n.parse::<u64>().is_ok())
            {
                replaced = true;
                format!("iter_{index}")
            } else {
                part.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// A pass's own finished prompt reads the pass's own label standing
/// in for the deviation DESIGN.md §2.3's own frame has no room left
/// for (`cats 130 t`, never `130 t +6%`) -- the first prompt only,
/// since a pass with more than one never shows the label twice.
fn label_first_prompt(items: &mut [RailItem], label: &str) {
    for item in items.iter_mut() {
        if let RailItem::Station(s) = item {
            if let Headline::Tokens { total, .. } = s.headline {
                s.headline = Headline::PassTokens {
                    label: label.to_string(),
                    total,
                };
                return;
            }
        }
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
            let (mut items, next) = build_level(rows, j + 1, depth + 2, &rows[j].path, ctx);
            let running = items.iter().any(
                |it| matches!(it, RailItem::Station(s) if edge_style(s.status) == EdgeStyle::Live),
            );
            if let Some(label) = pass_each_value(&row.path, observed, ctx.plan, ctx.state) {
                label_first_prompt(&mut items, &label);
            }
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
    unfolded_rows: &[Row],
    flat: &BTreeMap<String, NodeMeta>,
    plan: &PlanTree,
    header: &Header,
    state: Option<&Value>,
    zone: &dyn crate::format::ClockZone,
) -> Rail {
    let ctx = Ctx {
        flat,
        plan,
        unfolded_rows,
        header,
        state,
    };
    let mut items = vec![RailItem::Station(start_station(header, zone))];
    if let Some(root) = rows.first() {
        let (children, _) = build_level(rows, 1, root.depth + 1, &root.path, &ctx);
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

    fn compile_doc(yaml: &str) -> PlanTree {
        let dir = tempfile::tempdir().unwrap();
        let doc = dir.path().join("do.yml");
        std::fs::write(&doc, yaml).unwrap();
        let program = crate::plan::compile(&doc, None, &indexmap::IndexMap::new())
            .unwrap_or_else(|e| panic!("doesn't compile: {e}"));
        PlanTree::from_program(&program)
    }

    fn build_rail_for(plan: &PlanTree, state: &Value) -> Rail {
        let mut model = crate::model::RunModel::new();
        let rendered = crate::render::build(
            "do.yml",
            "test",
            plan,
            &mut model,
            Some(state),
            crate::model::ProcessState::Running,
            0.0,
            "2026-01-01T00:00:00Z",
            &[],
        );
        let flat = crate::model::flatten_state(state);
        let zone = chrono::FixedOffset::east_opt(0).unwrap();
        build_rail(
            &rendered.rows,
            &rendered.unfolded_rows,
            &flat,
            plan,
            &rendered.header,
            Some(state),
            &zone,
        )
    }

    fn unnamed_if_yaml(a: &str, b: &str) -> String {
        format!(
            "effects:\n  - type: if\n    if: {{mode: cel, expr: \"true\"}}\n    then:\n      - name: {a}_then\n        type: tool\n        provider: json\n        params: {{mode: parse, input: \"\\\"x\\\"\"}}\n    else:\n      - name: {a}_else\n        type: tool\n        provider: json\n        params: {{mode: parse, input: \"\\\"y\\\"\"}}\n  - type: if\n    if: {{mode: cel, expr: \"true\"}}\n    then:\n      - name: {b}_then\n        type: tool\n        provider: json\n        params: {{mode: parse, input: \"\\\"x\\\"\"}}\n    else:\n      - name: {b}_else\n        type: tool\n        provider: json\n        params: {{mode: parse, input: \"\\\"y\\\"\"}}\n"
        )
    }

    /// Issue #643's third follow-up (ported from the removed tree
    /// screen): two unnamed `if`s directly under the same host,
    /// neither one anchored yet (no real run at all) -- both fall
    /// back to the host-after case, and must still get their own
    /// distinct synthetic station paths, never colliding on the
    /// same `.__if`.
    #[test]
    fn two_unnamed_ifs_sharing_a_host_get_their_own_distinct_paths() {
        let plan = compile_doc(&unnamed_if_yaml("a", "b"));
        let state = serde_json::json!({
            "prime": {"value": null, "meta": {"created_at": "t0", "completed_at": null}}
        });
        let rail = build_rail_for(&plan, &state);
        let decision_paths: Vec<&str> = rail
            .items
            .iter()
            .filter_map(|it| match it {
                RailItem::Station(s) if s.path.contains(".__if") => Some(s.path.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(decision_paths.len(), 2, "{decision_paths:?}");
        assert_ne!(
            decision_paths[0], decision_paths[1],
            "two unnamed ifs sharing a host must not collide on the same path"
        );
    }

    /// The same two-unnamed-ifs-one-host shape, but the first one's
    /// own `then` has actually started -- its own decision resolves
    /// (and its taken branch's own station shows), while the second,
    /// still pending, keeps its own distinct path too.
    #[test]
    fn the_first_of_two_unnamed_ifs_resolving_does_not_collide_with_the_second() {
        let plan = compile_doc(&unnamed_if_yaml("a", "b"));
        let state = serde_json::json!({
            "prime": {
                "value": null,
                "meta": {"created_at": "t0", "completed_at": null},
                "a_then": {"value": "x", "meta": {"created_at": "t0", "completed_at": "t1"}}
            }
        });
        let rail = build_rail_for(&plan, &state);
        let stations: Vec<&Station> = rail
            .items
            .iter()
            .filter_map(|it| match it {
                RailItem::Station(s) if s.path.contains(".__if") => Some(s),
                _ => None,
            })
            .collect();
        assert_eq!(stations.len(), 2, "{stations:?}");
        assert_ne!(stations[0].path, stations[1].path);
        assert_eq!(
            stations[0].headline,
            Headline::Branch(Some("then".to_string()))
        );
        assert_eq!(stations[1].headline, Headline::Branch(None));
        let a_then_shown = rail.items.iter().any(|it| {
            matches!(it, RailItem::Fork(f) if f.lanes.iter().any(|l| {
                l.items.iter().any(|it| matches!(it, RailItem::Station(s) if s.name == "a_then"))
            }))
        });
        assert!(
            a_then_shown,
            "the first decision's own taken branch must show its real station"
        );
    }
}
