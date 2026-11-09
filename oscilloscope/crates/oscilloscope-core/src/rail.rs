//! The Flow screen's rail model (issue #691, part 3 of #677):
//! stations in plan order, forking into lanes at a decision or loop
//! and rejoining, built only while screen `2` is actually on display
//! (the same performance rule `screens::build_cost_screen` already
//! follows for screen `3`). Replaces the indented tree
//! [`crate::flow`] used to build.
//!
//! `rows` (`render::build`'s own depth-first list) nests a named
//! `if`'s own children by depth -- but carries *both* branches, the
//! untaken one as faint ahead rows sorted after the taken one's real
//! ones (`render::child_segments`'s own tie-break), so this module
//! bounds its own walk of a named `if`'s children to the taken
//! branch's plan paths before reading that shape off `rows`
//! (`build_decision_fork`). An *unnamed* `if` writes no state node
//! of its own, so it has no row at all to read a shape off of: its
//! own decision is spliced in here instead, from
//! `PlanTree::unnamed_if_decisions`, the same way the tree screen
//! this replaces used to (`unnamed_branch_state`). A loop pass that
//! has not started yet has no row either -- synthesised here, from
//! the plan and, for a pass, from the run's own `each.in` input.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::model::{NodeMeta, StatusKind, subtree_tokens};
use crate::plan::{Flow, PlanEntryKind, PlanTree, UnnamedIfDecision};
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
    /// A container whose own children aren't known yet (a `use`
    /// before its child document loads, a `dynamic` before it runs):
    /// its own type (`use`, `dynamic`), standing in for a child it
    /// has none of yet.
    ContainerAhead(String),
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
    /// A parallel `dynamic` (`Flow::Tree`): one lane per named child,
    /// every lane real (never a loop's own synthetic ahead lane, and
    /// never a decision's own untaken ghost) -- the plan fixes the
    /// lane count, but a front end still folds it into its own band
    /// at four lanes or more, the same as a loop (DESIGN.md §2.3/§9).
    Parallel,
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
    // An unnamed `if`'s own branch evidence is read from here --
    // always the same, unfolded rows the rest of this module walks
    // too (`build_rail`'s own doc: quick-step folding or a
    // breadcrumb would otherwise make a loop pass, or a branch's own
    // first step, vanish from the rail well before either stops
    // being real).
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

/// A container's own station once it has no child to show at all
/// yet -- its own type stands in for a headline (`use`, `dynamic`),
/// and whatever the run already knows about which child it will be
/// (a `use`'s own resolved orchestration, a `dynamic`'s own `flow`)
/// stands in for a detail, empty when even that isn't known yet.
fn container_ahead_station(row: &Row, ctx: &Ctx<'_>) -> Station {
    let detail = ctx
        .flat
        .get(&row.path)
        .and_then(|n| {
            n.meta
                .get("resolved_path")
                .and_then(Value::as_str)
                .or_else(|| n.meta.get("orchestration").and_then(Value::as_str))
                .map(str::to_string)
                .or_else(|| {
                    n.meta
                        .get("flow")
                        .and_then(Value::as_str)
                        .map(|f| format!("flow: {f}"))
                })
        })
        .unwrap_or_default();
    Station {
        path: row.path.clone(),
        selectable: true,
        is_edge: false,
        row_kind: row.kind,
        status: row.status.kind,
        name: row.label.clone(),
        headline: Headline::ContainerAhead(row.kind.label().to_string()),
        detail: Detail::Command(detail),
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
    // A decision inside a *named* loop's pass has its own host, and
    // every path either of its own branches contributes, recorded
    // with the `iter_*` placeholder (`PlanTree::unnamed_if_decisions`'s
    // own doc comment), never a concrete pass's own `iter_N` -- both
    // are concretised against this level's own real `host` before
    // anything below compares them against `rows`' own real paths,
    // or such a decision never matches a row, a host, or its own
    // untaken branch at all.
    let normalized_host = crate::plan::normalize_pass_segments(host);
    let unnamed: Vec<UnnamedIfDecision> = ctx
        .plan
        .unnamed_if_decisions()
        .iter()
        .filter(|d| d.host_path == normalized_host)
        .map(|d| concretize_decision(d, &normalized_host, host))
        .collect();
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
                build_unnamed_decision(&unnamed[pos], rows, i, depth, index, ctx);
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
            RowKind::Dynamic if dynamic_is_parallel(&row.path, ctx) => {
                let (fork, next) = build_parallel_fork(rows, i, ctx);
                let station = if fork.lanes.is_empty() {
                    container_ahead_station(row, ctx)
                } else {
                    plain_station(row, ctx)
                };
                items.push(RailItem::Station(station));
                items.push(RailItem::Fork(fork));
                i = next;
            }
            // A sequential container (`use`, a chain-flow `dynamic`,
            // a `try`/`finally`) runs its own children one after
            // another in the same lane its own station sits in --
            // never a fork of their own, so its children are just
            // inlined straight after it, the same as any other
            // plain station's own sibling. With no child at all yet
            // (its own child document hasn't loaded, or it hasn't
            // run), the station stands in for the child it has none
            // of, rather than drawing a blank one.
            RowKind::Use | RowKind::TryFinally | RowKind::Dynamic => {
                let (children, next) = build_level(rows, i + 1, depth + 1, &row.path, ctx);
                let station = if children.is_empty() {
                    container_ahead_station(row, ctx)
                } else {
                    plain_station(row, ctx)
                };
                items.push(RailItem::Station(station));
                items.extend(children);
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

/// `decision`, with its own host and every path either branch
/// contributes rewritten from `normalized_host`'s own `iter_*`
/// placeholder to `host`'s own concrete pass (a no-op when `decision`
/// isn't inside a loop pass at all, since `normalized_host == host`
/// then and every rewrite leaves its own path unchanged).
fn concretize_decision(
    decision: &UnnamedIfDecision,
    normalized_host: &str,
    host: &str,
) -> UnnamedIfDecision {
    let translate = |p: &str| -> String {
        match p.strip_prefix(normalized_host) {
            Some(rest) => format!("{host}{rest}"),
            None => p.to_string(),
        }
    };
    UnnamedIfDecision {
        host_path: host.to_string(),
        condition: decision.condition.clone(),
        then_paths: decision.then_paths.iter().map(|p| translate(p)).collect(),
        else_paths: decision.else_paths.iter().map(|p| translate(p)).collect(),
        later_siblings: decision
            .later_siblings
            .iter()
            .map(|p| translate(p))
            .collect(),
    }
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
    let mut hide = Vec::new();
    let lanes = if consumed > 0 {
        // The decision resolved and its taken branch actually ran:
        // the taken side draws its own real stations, the untaken
        // side (an unnamed `if` has no `meta.branch` to suppress it
        // with ahead of time, unlike a named one) a single ghost
        // lane standing in for every one of its own steps, hidden
        // from the rest of this level's own walk rather than drawn a
        // second time as a redundant plain sibling. Never a path the
        // taken side also claims (the parity case: `then`/`else`
        // reusing one step name), or the branch that actually ran
        // would vanish too.
        let mut lanes = vec![Lane {
            items: taken_items,
            ghost: None,
            running: taken_running,
            waiting: false,
        }];
        let untaken_paths = match branch.as_deref() {
            Some("then") => Some(&decision.else_paths),
            Some("else") => Some(&decision.then_paths),
            _ => None,
        };
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
        lanes
    } else if nothing_ran {
        // Settled, but neither branch ever actually ran (no `else`
        // at all, and the `then` branch's own scope is over) --
        // every real path either branch has is hidden the same way
        // an untaken branch's own is, and both sides draw as ghosts
        // reading "not taken", the same ghost form a named `if`'s
        // own untaken branch already uses.
        let mut lanes = Vec::new();
        for side in [&decision.then_paths, &decision.else_paths] {
            if let Some(first) = side.first() {
                lanes.push(Lane {
                    items: Vec::new(),
                    ghost: Some(step_label(first)),
                    running: false,
                    waiting: false,
                });
                hide.extend(side.iter().cloned());
            }
        }
        lanes
    } else {
        // Still pending: both branches draw as ahead ghosts, never
        // hidden from the rest of this level's own walk, since
        // either one might still turn out to be the one that ran.
        [&decision.then_paths, &decision.else_paths]
            .into_iter()
            .filter_map(|side| side.first())
            .map(|first| Lane {
                items: Vec::new(),
                ghost: Some(step_label(first)),
                running: false,
                waiting: false,
            })
            .collect()
    };
    let status = if branch.is_some() {
        StatusKind::Done
    } else {
        StatusKind::Pending
    };
    let headline_branch = if nothing_ran {
        Some("none".to_string())
    } else {
        branch.clone()
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
        headline: Headline::Branch(headline_branch),
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

fn named_if_branch_paths(path: &str, ctx: &Ctx<'_>) -> Option<(Vec<String>, Vec<String>)> {
    let m = ctx.plan.match_path(path)?;
    for e in m.entries {
        if let PlanEntryKind::If {
            named: true,
            then_paths,
            else_paths,
            ..
        } = &e.kind
        {
            return Some((then_paths.clone(), else_paths.clone()));
        }
    }
    None
}

fn untaken_branch_paths(path: &str, taken: Option<&str>, ctx: &Ctx<'_>) -> Option<Vec<String>> {
    let (then_paths, else_paths) = named_if_branch_paths(path, ctx)?;
    match taken {
        Some("then") => Some(else_paths),
        Some("else") => Some(then_paths),
        _ => None,
    }
}

/// The branch that actually ran's own top-level child paths --
/// `None` while still pending (neither side has a `meta.branch`
/// yet to pick one), the same `None` meaning `build_decision_fork`
/// falls back to its old unfiltered walk for.
fn taken_branch_paths(path: &str, taken: Option<&str>, ctx: &Ctx<'_>) -> Option<Vec<String>> {
    let (then_paths, else_paths) = named_if_branch_paths(path, ctx)?;
    match taken {
        Some("then") => Some(then_paths),
        Some("else") => Some(else_paths),
        _ => None,
    }
}

fn step_label(path: &str) -> String {
    path.rsplit('.').next().unwrap_or(path).to_string()
}

/// A named `if`'s taken lane must hold only the branch that ran:
/// `rows` carries the untaken branch's own steps too, as faint ahead
/// stations sorted after the taken ones (`render::child_segments`'s
/// own tie-break: a row with a real `created_at` always sorts before
/// one with none), so an unbounded walk of every row at this if's
/// own child depth draws them as if they belonged to the taken lane
/// as well. Bounded by the taken branch's own plan paths
/// (`branch_subtree_end`, the same bound an unnamed `if`'s own
/// decision already uses) once the branch has resolved; still
/// pending, nothing has a row yet, so the old unbounded walk is a
/// no-op.
fn build_decision_fork(rows: &[Row], i: usize, ctx: &Ctx<'_>) -> (Fork, usize) {
    let row = &rows[i];
    let depth = row.depth;
    let branch = ctx.flat.get(&row.path).and_then(|n| n.branch.clone());
    let taken_paths = taken_branch_paths(&row.path, branch.as_deref(), ctx);
    let (taken_items, taken_end) = match &taken_paths {
        Some(paths) => {
            let end = branch_subtree_end(rows, i + 1, depth + 1, paths);
            let (items, _) = build_level(&rows[..end], i + 1, depth + 1, &row.path, ctx);
            (items, end)
        }
        None => build_level(rows, i + 1, depth + 1, &row.path, ctx),
    };
    let mut lanes = vec![Lane {
        items: taken_items,
        ghost: None,
        running: edge_style(row.status.kind) == EdgeStyle::Live,
        waiting: false,
    }];
    // The untaken branch's own rows are siblings of the taken
    // branch's at the same depth, never drawn of their own accord
    // (the ghost lane stands in for all of them) -- `next` must
    // still skip past them, or the level above reprocesses them as
    // orphan plain stations.
    let mut next = taken_end;
    if let Some(untaken) = untaken_branch_paths(&row.path, branch.as_deref(), ctx) {
        if let Some(first) = untaken.first() {
            lanes.push(Lane {
                items: Vec::new(),
                ghost: Some(step_label(first)),
                running: false,
                waiting: false,
            });
        }
        while next < rows.len() && rows[next].depth > depth {
            next += 1;
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

/// Whether `path` is a parallel `dynamic` (`Flow::Tree`, a
/// `Region::Parallel` -- DESIGN.md is silent on a bare container,
/// but its children genuinely run concurrently, the same reason a
/// loop's own passes get one lane each): `false` for a chain-flow
/// one, which runs its own children in the one lane its own station
/// already sits in, same as any other plain sequence.
fn dynamic_is_parallel(path: &str, ctx: &Ctx<'_>) -> bool {
    ctx.plan.match_path(path).is_some_and(|m| {
        m.entries
            .iter()
            .any(|e| matches!(&e.kind, PlanEntryKind::Dynamic { flow: Flow::Tree }))
    })
}

/// A parallel `dynamic`'s own fork: one lane per named child, each
/// holding that child's own whole subtree -- never padded with an
/// ahead lane, since the plan already names every child there will
/// ever be. Whether it draws folded into its own band at four or
/// more lanes (DESIGN.md §2.3/§9, the same rule a loop's own lanes
/// already follow) is a display-time decision, never made here.
fn build_parallel_fork(rows: &[Row], i: usize, ctx: &Ctx<'_>) -> (Fork, usize) {
    let row = &rows[i];
    let depth = row.depth;
    let mut lanes = Vec::new();
    let mut j = i + 1;
    while j < rows.len() && rows[j].depth > depth {
        if rows[j].depth == depth + 1 {
            let mut end = j + 1;
            while end < rows.len() && rows[end].depth > depth + 1 {
                end += 1;
            }
            let (items, _) = build_level(&rows[..end], j, depth + 1, &row.path, ctx);
            let running = items.iter().any(
                |it| matches!(it, RailItem::Station(s) if edge_style(s.status) == EdgeStyle::Live),
            );
            let waiting = items.iter().all(|it| {
                matches!(it, RailItem::Station(s) if matches!(s.status, StatusKind::Pending | StatusKind::Skipped))
            });
            lanes.push(Lane {
                items,
                ghost: None,
                running,
                waiting,
            });
            j = end;
        } else {
            j += 1;
        }
    }
    (
        Fork {
            path: row.path.clone(),
            kind: ForkKind::Parallel,
            source_status: row.status.kind,
            lanes,
        },
        j,
    )
}

/// A loop's own lane count -- one lane per *pass*, never per
/// concurrency slot (DESIGN.md §2.3 reads "one lane per slot
/// (`max_concurrency` or the pass count when unbounded)", but the
/// frames draw a lane per pass throughout, and the folded band's own
/// `● n run · m wait` reading only makes sense that way: a lane that
/// *waits* is a pass that hasn't started, never an idle concurrency
/// slot). The loop's own known total pass count wins once it has one
/// -- from `progress.total` once there's been an event at all, or
/// (DESIGN.md's own "Starting" state, before any event) from a named
/// `each` loop's own input, the same resolution `pass_each_value`
/// itself reads. Failing both (an unbounded loop still running, with
/// nothing resolved yet either), however many passes are actually
/// observed so far -- there is no later pass to preview ahead of
/// time either way.
fn loop_lane_count(
    path: &str,
    progress: Option<LoopProgress>,
    observed: u64,
    ctx: &Ctx<'_>,
) -> u64 {
    if let Some(total) = progress.and_then(|p| p.total) {
        return total.max(observed);
    }
    if let Some(total) = ctx
        .state
        .and_then(|state| ctx.plan.resolve_each_len(path, state))
    {
        return total.max(observed);
    }
    observed
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

/// A loop's own body, read straight from the plan (never from any
/// observed pass): every leaf under `loop_path`'s own `iter_*`
/// placeholder, in declaration order, each standing in for
/// [`ahead_pass_lane`]'s own per-station mapping below (which reads
/// only a template station's `row_kind` and `path`, never its
/// headline or detail) -- used only once a loop hasn't produced a
/// single real pass yet, so [`build_loop_fork`] has no observed
/// lane of its own to shape the rest off instead.
fn loop_body_template(loop_path: &str, ctx: &Ctx<'_>) -> Vec<RailItem> {
    let prefix = format!("{loop_path}.iter_*.");
    ctx.plan
        .leaf_paths()
        .iter()
        .filter(|p| p.starts_with(&prefix))
        .map(|p| {
            let path = p.replacen("iter_*", "iter_0", 1);
            // `match_path` resolves a real `iter_N` pass segment
            // through its own trie logic, never the literal `iter_*`
            // placeholder text `leaf_paths` itself uses -- resolved
            // against the concrete path just built above, not `p`.
            let row_kind = ctx
                .plan
                .match_path(&path)
                .and_then(|m| m.entries.first())
                .map(|e| RowKind::from_plan_entry(&e.kind))
                .unwrap_or(RowKind::Unknown);
            RailItem::Station(Station {
                path,
                selectable: false,
                is_edge: false,
                row_kind,
                status: StatusKind::Pending,
                name: step_label(p),
                headline: Headline::Blank,
                detail: Detail::Blank,
            })
        })
        .collect()
}

/// An ahead pass's own lane (DESIGN.md §2.3's folded loop example,
/// `birds` / `usual 155` / `sleep 1` / `queued`): shaped off
/// `template`, the first real pass's own stations, with every
/// prompt reading the pass's own label (standing in for tokens it
/// doesn't have yet) and every tool reading its own usual command,
/// both still queued.
fn ahead_pass_lane(template: &[RailItem], loop_path: &str, index: u64, ctx: &Ctx<'_>) -> Lane {
    let items = template
        .iter()
        .map(|item| ahead_template_item(item, loop_path, index, ctx))
        .collect();
    Lane {
        items,
        ghost: None,
        running: false,
        waiting: true,
    }
}

/// A template item's own ahead counterpart: a station reads pending
/// with no tokens yet, its prompt/tool headline standing in for one
/// it doesn't have (the same shape `ahead_pass_lane` always drew for
/// a plain station); a nested fork (an unnamed `if` inside the
/// loop's own body, a nested loop, a parallel `dynamic` inside a
/// pass) keeps its own shape -- kind and lane count -- but maps
/// every one of its own lanes through this same function, rather
/// than keeping pass 0's real, already-finished fork verbatim: an
/// ahead pass that has not run yet must never show a real ✓ Done
/// status or real tokens copied from the pass this lane is only
/// templated on.
fn ahead_template_item(item: &RailItem, loop_path: &str, index: u64, ctx: &Ctx<'_>) -> RailItem {
    match item {
        RailItem::Station(s) => {
            // A loop usually has more passes than slots, so the
            // template's own path is always the first real pass's
            // own (`iter_0`) -- but the history this own slot
            // actually keeps is per *position*, not per pass, so an
            // ahead lane further along reads its own slot's own
            // history, not `iter_0`'s.
            let slot_path = with_iter_index(&s.path, index);
            let (headline, detail) = match s.row_kind {
                RowKind::Prompt => (
                    Headline::PassLabel(
                        pass_each_value(loop_path, index, ctx.plan, ctx.state)
                            .unwrap_or_else(|| format!("pass {}", index + 1)),
                    ),
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
        RailItem::Fork(f) => {
            let lanes = f
                .lanes
                .iter()
                .map(|lane| Lane {
                    items: lane
                        .items
                        .iter()
                        .map(|it| ahead_template_item(it, loop_path, index, ctx))
                        .collect(),
                    ghost: lane.ghost.clone(),
                    running: false,
                    waiting: true,
                })
                .collect();
            RailItem::Fork(Fork {
                path: ahead_fork_path(&f.path, index),
                kind: f.kind,
                source_status: StatusKind::Pending,
                lanes,
            })
        }
    }
}

/// Rewrites a template fork's own path the same way a template
/// station's is (`ahead_template_item`'s own `iter_ahead_{index}`),
/// so two ahead passes' own forks never collide on the same
/// `app.flow_lanes_open` fold state or `find_station` lookup --
/// falls back to a trailing suffix on the rare path with no
/// `iter_<N>` segment of its own to replace at all.
fn ahead_fork_path(fork_path: &str, index: u64) -> String {
    let mut replaced = false;
    let rewritten: String = fork_path
        .split('.')
        .map(|part| {
            if !replaced
                && part
                    .strip_prefix("iter_")
                    .is_some_and(|n| n.parse::<u64>().is_ok())
            {
                replaced = true;
                format!("iter_ahead_{index}")
            } else {
                part.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(".");
    if replaced {
        rewritten
    } else {
        format!("{rewritten}.iter_ahead_{index}")
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

/// Whether `candidate` is one of `loop_path`'s own passes -- by its
/// own path shape (`loop_path.iter_<digits>`), never by its own
/// `RowKind`: a loop whose body is a single container op (an `if`,
/// another loop, a `dynamic`) places that op's own entry directly at
/// the pass's own path, with no separate `Pass`-kind row of its own
/// at all to tell the pass boundary apart from its own body by kind
/// alone. The loop's own un-pass-resolved template row
/// (`loop_path.iter_*`, DESIGN.md's own placeholder) never matches:
/// `*` isn't a digit.
pub fn is_pass_path(candidate: &str, loop_path: &str) -> bool {
    candidate
        .strip_prefix(loop_path)
        .and_then(|rest| rest.strip_prefix(".iter_"))
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

fn build_loop_fork(rows: &[Row], i: usize, ctx: &Ctx<'_>) -> (Fork, usize) {
    let row = &rows[i];
    let depth = row.depth;
    let mut lanes = Vec::new();
    let mut j = i + 1;
    let mut observed = 0u64;
    while j < rows.len() && rows[j].depth > depth {
        if rows[j].depth == depth + 1 && is_pass_path(&rows[j].path, &row.path) {
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
    let template: Vec<RailItem> = match lanes.first() {
        Some(first) => first.items.clone(),
        None => loop_body_template(&row.path, ctx),
    };
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
/// children, in plan order, become the rail's own items. `rows` is
/// always the unfolded tree -- the tree screen's own breadcrumb and
/// quick-steps folding ([`crate::fold`]) never applies here (a
/// single-step loop pass losing its own `Pass` row to a breadcrumb
/// would make [`build_loop_fork`] below find no passes at all);
/// callers pass [`crate::render::RenderState::unfolded_rows`], never
/// `rows`.
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
        unfolded_rows: rows,
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
/// Finds the station at `path`, anywhere on the rail (recursing
/// into every fork's own lanes) -- the only way to resolve an
/// unnamed decision's own synthetic `.__if` path, which has no row
/// of its own for anything else to look it up by.
pub fn find_station<'a>(items: &'a [RailItem], path: &str) -> Option<&'a Station> {
    for item in items {
        match item {
            RailItem::Station(s) if s.path == path => return Some(s),
            RailItem::Fork(fork) => {
                for lane in &fork.lanes {
                    if let Some(found) = find_station(&lane.items, path) {
                        return Some(found);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// Every unnamed `if`'s own inferred decision anywhere on the rail
/// (recursing into every fork's own lanes), ready for the Story pane
/// (`crate::flow::build_story`'s own `inferred` parameter): the
/// taken branch's own first station gives the clock the decision
/// itself has none of its own (an unnamed `if` writes no node, and
/// so no `created_at`, of its own at all).
pub fn inferred_decisions(
    items: &[RailItem],
    flat: &BTreeMap<String, NodeMeta>,
) -> Vec<crate::flow::InferredDecision> {
    let mut out = Vec::new();
    for (i, item) in items.iter().enumerate() {
        if let RailItem::Station(s) = item {
            if s.row_kind == RowKind::If
                && s.path
                    .rsplit('.')
                    .next()
                    .is_some_and(|seg| seg.starts_with("__if"))
            {
                if let Headline::Branch(branch) = &s.headline {
                    let start_ts = match items.get(i + 1) {
                        Some(RailItem::Fork(fork)) => fork.lanes.first().and_then(|lane| {
                            lane.items.iter().find_map(|it| match it {
                                RailItem::Station(station) => {
                                    flat.get(&station.path).and_then(|n| n.created_at.clone())
                                }
                                _ => None,
                            })
                        }),
                        _ => None,
                    };
                    out.push(crate::flow::InferredDecision {
                        label: s.name.clone(),
                        branch: branch.clone(),
                        start_ts,
                    });
                }
            }
        }
        if let RailItem::Fork(fork) = item {
            for lane in &fork.lanes {
                out.extend(inferred_decisions(&lane.items, flat));
            }
        }
    }
    out
}

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

    fn plain_station(path: &str) -> Station {
        Station {
            path: path.to_string(),
            selectable: true,
            is_edge: false,
            row_kind: RowKind::Tool,
            status: StatusKind::Done,
            name: path.to_string(),
            headline: Headline::Blank,
            detail: Detail::Blank,
        }
    }

    #[test]
    fn lane_width_sums_its_own_items_plus_the_normal_connector_between_them() {
        let items = vec![
            RailItem::Station(plain_station("a")),
            RailItem::Station(plain_station("b")),
        ];
        assert_eq!(lane_width(&items), STATION_WIDTH * 2 + NORMAL_CONNECTOR);
        assert_eq!(lane_width(&[]), STATION_WIDTH);
    }

    #[test]
    fn fork_width_is_its_own_lead_and_tail_plus_the_widest_lane() {
        let fork = Fork {
            path: "p".to_string(),
            kind: ForkKind::Decision,
            source_status: StatusKind::Done,
            lanes: vec![
                Lane {
                    items: vec![RailItem::Station(plain_station("taken"))],
                    ghost: None,
                    running: false,
                    waiting: false,
                },
                Lane {
                    items: Vec::new(),
                    ghost: Some("untaken".to_string()),
                    running: false,
                    waiting: false,
                },
            ],
        };
        assert_eq!(fork_width(&fork), FORK_LEAD + STATION_WIDTH + FORK_TAIL);
    }

    #[test]
    fn fork_width_reads_the_widest_lane_when_lanes_differ() {
        let fork = Fork {
            path: "p".to_string(),
            kind: ForkKind::Loop,
            source_status: StatusKind::Running,
            lanes: vec![
                Lane {
                    items: vec![RailItem::Station(plain_station("a"))],
                    ghost: None,
                    running: false,
                    waiting: false,
                },
                Lane {
                    items: vec![
                        RailItem::Station(plain_station("b")),
                        RailItem::Station(plain_station("c")),
                    ],
                    ghost: None,
                    running: false,
                    waiting: false,
                },
            ],
        };
        let widest_lane = STATION_WIDTH * 2 + NORMAL_CONNECTOR;
        assert_eq!(fork_width(&fork), FORK_LEAD + widest_lane + FORK_TAIL);
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

    fn each_loop_yaml(body_tool: &str) -> String {
        format!(
            "effects:\n  - type: loop\n    name: fan\n    each:\n      in: input.items\n      as: item\n    body:\n      - name: {body_tool}\n        type: tool\n        provider: json\n        params: {{mode: parse, input: \"\\\"x\\\"\"}}\n"
        )
    }

    fn find_fork_after<'a>(items: &'a [RailItem], name: &str) -> Option<&'a Fork> {
        items.iter().enumerate().find_map(|(i, it)| match it {
            RailItem::Station(s) if s.name == name => match items.get(i + 1) {
                Some(RailItem::Fork(f)) => Some(f),
                _ => None,
            },
            _ => None,
        })
    }

    /// A loop pass with only one step never breadcrumb-folds away on
    /// the rail (the tree screen's own fold -- [`crate::fold`] --
    /// never applies to it at all, since the rail always reads
    /// unfolded rows): `build_loop_fork` must still find it as a
    /// real pass, not an empty lane.
    #[test]
    fn a_loop_pass_with_a_single_step_still_shows_its_own_station() {
        let plan = compile_doc(&each_loop_yaml("nap"));
        let state = serde_json::json!({"prime": {"value": null, "meta": {"completed_at": null},
            "fan": {"value": null, "meta": {"completed_at": null, "progress": {"done": 1, "total": 2}},
                "iter_0": {
                    "nap": {"value": "x", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "json"}}
                }
            }
        }});
        let rail = build_rail_for(&plan, &state);
        let fork = find_fork_after(&rail.items, "fan").expect("a fork after fan");
        assert_eq!(fork.lanes.len(), 2, "{fork:?}");
        let nap_shown = fork.lanes[0].items.iter().any(
            |it| matches!(it, RailItem::Station(s) if s.name == "nap" && s.row_kind == RowKind::Tool),
        );
        assert!(
            nap_shown,
            "a single-step pass must still show its own step: {fork:?}"
        );
    }

    /// An unnamed `if` inside a *named* loop's own pass (host
    /// `fan.iter_*`, DESIGN.md's own placeholder) resolves against
    /// each pass's own concrete rows independently -- pass 0's own
    /// decision here is settled (`then` ran), and its synthetic
    /// path is pass 0's own concrete `iter_0`, never the placeholder
    /// itself (which every pass would otherwise collide on).
    #[test]
    fn an_unnamed_if_inside_a_loop_pass_resolves_against_that_passs_own_rows() {
        let yaml = "effects:\n  - type: loop\n    name: fan\n    each:\n      in: input.items\n      as: item\n    body:\n      - type: if\n        if: {mode: cel, expr: \"true\"}\n        then:\n          - name: nap\n            type: tool\n            provider: json\n            params: {mode: parse, input: \"\\\"x\\\"\"}\n        else:\n          - name: snooze\n            type: tool\n            provider: json\n            params: {mode: parse, input: \"\\\"y\\\"\"}\n";
        let plan = compile_doc(yaml);
        let state = serde_json::json!({"prime": {"value": null, "meta": {"completed_at": null},
            "fan": {"value": null, "meta": {"completed_at": null, "progress": {"done": 1, "total": 1}},
                "iter_0": {
                    "nap": {"value": "x", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "json"}}
                }
            }
        }});
        let rail = build_rail_for(&plan, &state);
        let decision = find_station(&rail.items, "prime.fan.iter_0.__if").unwrap_or_else(|| {
            panic!(
                "no decision at the pass's own concrete path: {:?}",
                rail.items
            )
        });
        assert_eq!(
            decision.headline,
            Headline::Branch(Some("then".to_string()))
        );
    }

    /// A loop that hasn't produced a single real pass yet -- not
    /// even a scaffolded Pending one, which `render::build_rows`
    /// itself seeds whenever a bounded each loop's own count already
    /// resolves -- still previews its own body on every ahead lane,
    /// read straight from the plan rather than from an observed pass
    /// (there is none): built with the same clean rows `render::
    /// build` gives a loop no event has ever mentioned, but a state
    /// that *does* resolve `each.in` passed to `build_rail` directly,
    /// the one piece `build_rows` itself would otherwise have
    /// scaffolded real rows from first.
    #[test]
    fn an_unstarted_loop_previews_its_own_plan_body_on_every_ahead_lane() {
        let plan = compile_doc(&each_loop_yaml("nap"));
        let rows_state =
            serde_json::json!({"prime": {"value": null, "meta": {"completed_at": null}}});
        let mut model = crate::model::RunModel::new();
        let rendered = crate::render::build(
            "do.yml",
            "test",
            &plan,
            &mut model,
            Some(&rows_state),
            crate::model::ProcessState::Running,
            0.0,
            "2026-01-01T00:00:00Z",
            &[],
        );
        assert!(
            !rendered
                .unfolded_rows
                .iter()
                .any(|r| r.kind == RowKind::Pass),
            "the fixture itself must start with zero observed passes: {:?}",
            rendered.unfolded_rows
        );
        let lookup_state = serde_json::json!({"input": {"items": ["cats", "dogs"]}});
        let flat = crate::model::flatten_state(&rows_state);
        let zone = chrono::FixedOffset::east_opt(0).unwrap();
        let rail = build_rail(
            &rendered.unfolded_rows,
            &flat,
            &plan,
            &rendered.header,
            Some(&lookup_state),
            &zone,
        );
        let fork = find_fork_after(&rail.items, "fan").expect("a fork after fan");
        assert_eq!(fork.lanes.len(), 2, "{fork:?}");
        for lane in &fork.lanes {
            assert!(lane.waiting, "{fork:?}");
            let nap_previewed = lane
                .items
                .iter()
                .any(|it| matches!(it, RailItem::Station(s) if s.row_kind == RowKind::Tool));
            assert!(
                nap_previewed,
                "an ahead lane must preview the plan body: {fork:?}"
            );
        }
    }

    fn named_if_yaml(then_name: &str, else_name: &str) -> String {
        format!(
            "effects:\n  - name: pick\n    type: if\n    if: {{mode: cel, expr: \"true\"}}\n    then:\n      - name: {then_name}\n        type: tool\n        provider: json\n        params: {{mode: parse, input: \"\\\"x\\\"\"}}\n    else:\n      - name: {else_name}\n        type: tool\n        provider: json\n        params: {{mode: parse, input: \"\\\"y\\\"\"}}\n"
        )
    }

    /// A named `if` whose `else` branch ran must draw only the
    /// `else` step on the taken lane -- the `then` step (an ahead
    /// row, sorted after the taken one since it has no `created_at`
    /// of its own) must appear nowhere but the untaken ghost lane.
    #[test]
    fn a_named_ifs_taken_lane_holds_only_the_else_branch_that_ran() {
        let plan = compile_doc(&named_if_yaml("many", "few"));
        let state = serde_json::json!({"prime": {"value": null, "meta": {"completed_at": null},
            "pick": {
                "value": {"branch": "else", "result": false},
                "meta": {"branch": "else", "created_at": "t0", "completed_at": "t1"},
                "few": {"value": "y", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "json"}}
            }
        }});
        let rail = build_rail_for(&plan, &state);
        let fork = find_fork_after(&rail.items, "pick").expect("a fork after pick");
        assert_eq!(fork.lanes.len(), 2, "{fork:?}");
        let taken_names: Vec<&str> = fork.lanes[0]
            .items
            .iter()
            .filter_map(|it| match it {
                RailItem::Station(s) => Some(s.name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(taken_names, vec!["few"], "{fork:?}");
        assert_eq!(fork.lanes[1].ghost.as_deref(), Some("many"));
        assert!(fork.lanes[1].items.is_empty(), "{fork:?}");
    }

    /// The symmetric case: `then` taken, `else`'s own step must not
    /// leak onto the taken lane either.
    #[test]
    fn a_named_ifs_taken_lane_holds_only_the_then_branch_that_ran() {
        let plan = compile_doc(&named_if_yaml("many", "few"));
        let state = serde_json::json!({"prime": {"value": null, "meta": {"completed_at": null},
            "pick": {
                "value": {"branch": "then", "result": true},
                "meta": {"branch": "then", "created_at": "t0", "completed_at": "t1"},
                "many": {"value": "x", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "json"}}
            }
        }});
        let rail = build_rail_for(&plan, &state);
        let fork = find_fork_after(&rail.items, "pick").expect("a fork after pick");
        assert_eq!(fork.lanes.len(), 2, "{fork:?}");
        let taken_names: Vec<&str> = fork.lanes[0]
            .items
            .iter()
            .filter_map(|it| match it {
                RailItem::Station(s) => Some(s.name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(taken_names, vec!["many"], "{fork:?}");
        assert_eq!(fork.lanes[1].ghost.as_deref(), Some("few"));
        assert!(fork.lanes[1].items.is_empty(), "{fork:?}");
    }

    /// A nested named `if` inside the taken branch of an outer named
    /// `if` still resolves once the outer fork's own walk is bounded
    /// to its own taken branch -- the inner decision is simply one
    /// more item inside that bound.
    #[test]
    fn a_nested_named_if_inside_a_taken_branch_still_resolves() {
        let yaml = "effects:\n  - name: outer\n    type: if\n    if: {mode: cel, expr: \"true\"}\n    then:\n      - name: inner\n        type: if\n        if: {mode: cel, expr: \"true\"}\n        then:\n          - name: deep\n            type: tool\n            provider: json\n            params: {mode: parse, input: \"\\\"x\\\"\"}\n        else:\n          - name: shallow\n            type: tool\n            provider: json\n            params: {mode: parse, input: \"\\\"y\\\"\"}\n    else:\n      - name: bail\n        type: tool\n        provider: json\n        params: {mode: parse, input: \"\\\"z\\\"\"}\n";
        let plan = compile_doc(yaml);
        let state = serde_json::json!({"prime": {"value": null, "meta": {"completed_at": null},
            "outer": {
                "value": {"branch": "then", "result": true},
                "meta": {"branch": "then", "created_at": "t0", "completed_at": "t1"},
                "inner": {
                    "value": {"branch": "then", "result": true},
                    "meta": {"branch": "then", "created_at": "t0", "completed_at": "t1"},
                    "deep": {"value": "x", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "json"}}
                }
            }
        }});
        let rail = build_rail_for(&plan, &state);
        let outer_fork = find_fork_after(&rail.items, "outer").expect("a fork after outer");
        assert_eq!(outer_fork.lanes.len(), 2, "{outer_fork:?}");
        let inner_fork = find_fork_after(&outer_fork.lanes[0].items, "inner")
            .expect("a fork after inner, inside outer's own taken lane");
        assert_eq!(inner_fork.lanes.len(), 2, "{inner_fork:?}");
        let deep_shown = inner_fork.lanes[0]
            .items
            .iter()
            .any(|it| matches!(it, RailItem::Station(s) if s.name == "deep"));
        assert!(deep_shown, "{inner_fork:?}");
        assert_eq!(inner_fork.lanes[1].ghost.as_deref(), Some("shallow"));
        assert_eq!(outer_fork.lanes[1].ghost.as_deref(), Some("bail"));
    }

    fn every_station(items: &[RailItem]) -> Vec<&Station> {
        let mut out = Vec::new();
        for item in items {
            match item {
                RailItem::Station(s) => out.push(s),
                RailItem::Fork(f) => {
                    for lane in &f.lanes {
                        out.extend(every_station(&lane.items));
                    }
                }
            }
        }
        out
    }

    /// A 3-pass loop whose body holds an unnamed `if`: pass 0 is
    /// done (its own `then` branch really ran), passes 1 and 2
    /// haven't started -- their own ahead lanes must preview pass
    /// 0's shape (an inner decision fork, not a flattened station),
    /// but never pass 0's real ✓ Done status, since neither of them
    /// has run at all yet.
    #[test]
    fn an_ahead_passes_own_nested_if_never_shows_pass_0s_real_done_status() {
        let yaml = "effects:\n  - type: loop\n    name: fan\n    each:\n      in: input.items\n      as: item\n    body:\n      - type: if\n        if: {mode: cel, expr: \"true\"}\n        then:\n          - name: nap\n            type: tool\n            provider: json\n            params: {mode: parse, input: \"\\\"x\\\"\"}\n        else:\n          - name: snooze\n            type: tool\n            provider: json\n            params: {mode: parse, input: \"\\\"y\\\"\"}\n";
        let plan = compile_doc(yaml);
        let state = serde_json::json!({
            "input": {"items": ["cats", "dogs", "birds"]},
            "prime": {"value": null, "meta": {"completed_at": null},
                "fan": {"value": null, "meta": {"completed_at": null, "progress": {"done": 1, "total": 3}},
                    "iter_0": {
                        "nap": {"value": "x", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "json"}}
                    }
                }
            }
        });
        let rail = build_rail_for(&plan, &state);
        let fork = find_fork_after(&rail.items, "fan").expect("a fork after fan");
        assert_eq!(fork.lanes.len(), 3, "{fork:?}");
        // Pass 0's own lane keeps its real inner decision, resolved
        // to `then` -- a real station, really done.
        let pass0_inner = fork.lanes[0]
            .items
            .iter()
            .find_map(|it| match it {
                RailItem::Fork(f) => Some(f),
                _ => None,
            })
            .expect("pass 0's own lane keeps its real inner decision fork");
        let pass0_done = every_station(&pass0_inner.lanes[0].items)
            .iter()
            .any(|s| s.status == StatusKind::Done);
        assert!(pass0_done, "pass 0 really ran: {pass0_inner:?}");
        // Passes 1 and 2's own ahead lanes must hold the same inner
        // decision *shape* -- a fork, not a flattened station list
        // -- but no station inside either one may read Done.
        for ahead in &fork.lanes[1..] {
            let inner = ahead.items.iter().find_map(|it| match it {
                RailItem::Fork(f) => Some(f),
                _ => None,
            });
            let inner = inner.unwrap_or_else(|| {
                panic!("an ahead pass must preview the inner fork too: {ahead:?}")
            });
            assert_eq!(inner.kind, ForkKind::Decision, "{inner:?}");
            for station in every_station(&ahead.items) {
                assert_ne!(
                    station.status,
                    StatusKind::Done,
                    "an ahead pass must never show a real ✓: {ahead:?}"
                );
            }
        }
    }
}
