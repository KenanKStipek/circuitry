//! The Flow screen's own model (issue #635, part of #612): the
//! orchestration tree read as control flow -- each `if`'s own
//! decision and the branch it took, a loop's passes, and the run's
//! story -- built only while screen `2` is actually on display, the
//! same performance rule `screens::build_cost_screen` already
//! follows for screen `3`.
//!
//! An unnamed `if` writes no node and fires no event (DESIGN.md
//! §1.3), so unlike a named one it has no row anywhere in
//! `render::build`'s own tree for this module to augment; its own
//! decision is synthesised here instead, from
//! [`crate::plan::PlanTree::unnamed_if_decisions`], and spliced into
//! the row list at the position its own branches' first real step
//! already occupies.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::diff::{LogKind, LogLine};
use crate::fold::is_quick_step;
use crate::format::{ClockZone, format_clock_time};
use crate::model::{NodeMeta, RowStatus, StatusKind};
use crate::plan::{ConditionInfo, PlanTree, UnnamedIfDecision};
use crate::render::{LoopProgress, Row, RowKind};

/// A decision row's own dim second line (issue #635: "the CEL
/// expression, or the first line of a model-mode template, cut with
/// …") -- the cutting-short itself is a display-width concern, left
/// to the caller, same as [`ConditionInfo`]'s own text.
pub type FlowCondition = ConditionInfo;

#[derive(Debug, Clone, PartialEq)]
pub enum FlowRowKind {
    /// An ordinary orchestration-pane row, unchanged by this module
    /// apart from gaining the Flow screen's own columns.
    Row(RowKind),
    /// A decision row (`◆ <name> → then`, DESIGN.md's own glyph) --
    /// `branch` is `None` until the condition has been evaluated at
    /// all (the row then reads `◆ <name> …`), `inferred` is set only
    /// for an unnamed `if`'s own synthesised decision (never a named
    /// one, whose `meta.branch` is exact), and `nothing_ran` marks an
    /// unnamed `if` with no `else` once its own scope has finished
    /// with the `then` branch never having started at all.
    Decision {
        branch: Option<String>,
        inferred: bool,
        nothing_ran: bool,
        /// `meta.answer` for a model-mode condition (named ifs only
        /// -- DESIGN.md: "the model's answer for a model-mode
        /// condition").
        answer: Option<String>,
    },
    /// The untaken branch's own dim summary line (DESIGN.md: "one
    /// dim line, `· else: <its first step>` (`+N more`, or `nothing`
    /// if it's empty), never a row per untaken step").
    UntakenBranch,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FlowRow {
    pub path: String,
    pub depth: usize,
    pub label: String,
    pub kind: FlowRowKind,
    pub status: RowStatus,
    pub start_ts: Option<String>,
    pub duration_s: Option<f64>,
    /// `node_tokens`'s own sum for a leaf, or the subtree sum
    /// `render::build` already attributes to a container row
    /// (DESIGN.md: "tokens (node_tokens, or the subtree sum for a
    /// container)") -- `None` only when nothing has been reported at
    /// all, never shown as a bare `0`.
    pub tokens: Option<u64>,
    pub cost_usd: Option<f64>,
    pub condition: Option<FlowCondition>,
    pub loop_progress: Option<LoopProgress>,
    pub has_children: bool,
    /// Set only by [`merge_single_pass_loops`], on the loop row a
    /// single observed pass merged into: the Flow pane draws `⟲`
    /// in place of the status glyph, and skips the ordinary
    /// `loop_progress` suffix, since the merged label already reads
    /// `pass N of M`.
    pub merged_pass: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct FlowFooter {
    pub quick_steps_folded: u64,
    pub quick_steps_duration_s: f64,
    pub failed: u64,
    pub retries: u64,
    pub warnings: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoryEntry {
    /// Clock-prefixed and ready to draw (`HH:MM:SS <text>`) -- a real
    /// line reuses `--log`'s own text verbatim (`diff::LogLine::
    /// text`); an inferred decision's own text is built here instead,
    /// since it has no `LogLine` of its own (DESIGN.md: "join the
    /// story but never `--log`").
    pub text: String,
    pub inferred: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FlowModel {
    pub rows: Vec<FlowRow>,
    pub footer: FlowFooter,
    /// Oldest first, capped by the caller's own bounded story buffer
    /// (`MAX_STORY_LINES`) plus whatever inferred decisions this
    /// build adds on top -- never re-capped here, since an inferred
    /// decision is never persisted anywhere a later rebuild could
    /// double it.
    pub story: Vec<StoryEntry>,
}

/// Resolves every `iter_*` placeholder in an unnamed `if`'s own
/// decision against the real pass paths `rows` actually has, one
/// resolved decision per observed pass -- a decision with no `iter_*`
/// at all (the common case: top level, or directly under a tree
/// `dynamic`) resolves to exactly itself, unchanged.
fn resolve_unnamed_decisions(
    decisions: &[UnnamedIfDecision],
    rows: &[Row],
) -> Vec<UnnamedIfDecision> {
    let mut out = Vec::new();
    for decision in decisions {
        let Some(placeholder_at) = decision.host_path.find("iter_*") else {
            out.push(decision.clone());
            continue;
        };
        let prefix = &decision.host_path[..placeholder_at];
        // Every distinct pass segment (`iter_0`, `iter_1`, ...) any
        // row under this exact loop actually has, in the order those
        // rows already appear.
        let loop_prefix = prefix.trim_end_matches('.');
        // Every distinct `iter_N` segment any row's own path has
        // right after the loop's own prefix -- never filtered by
        // `RowKind::Pass`, since a pass container whose only content
        // is this very unnamed `if` already reads as `RowKind::If`
        // instead (the same path carries both the pass container's
        // implicit designation and the if's own `PlanEntry`, and
        // `render::row_kind_for` reads the plan entry first).
        let mut seen = Vec::new();
        for row in rows {
            let Some(rest) = row.path.strip_prefix(&format!("{loop_prefix}.")) else {
                continue;
            };
            let pass = rest.split('.').next().unwrap_or(rest);
            if pass.starts_with("iter_") && !seen.contains(&pass.to_string()) {
                seen.push(pass.to_string());
            }
        }
        for pass in seen {
            out.push(UnnamedIfDecision {
                host_path: decision.host_path.replacen("iter_*", &pass, 1),
                condition: decision.condition.clone(),
                then_paths: decision
                    .then_paths
                    .iter()
                    .map(|p| p.replacen("iter_*", &pass, 1))
                    .collect(),
                else_paths: decision
                    .else_paths
                    .iter()
                    .map(|p| p.replacen("iter_*", &pass, 1))
                    .collect(),
            });
        }
    }
    out
}

/// Whether `path`'s own row shows real evidence the step actually
/// ran (issue #635's inference: "once any step of one branch has
/// appeared, that branch ran") -- `Pending` (plan-seeded, never
/// reached) and `Skipped` (the *other* branch's own untaken-branch
/// inference already marking this path, `model::skip_reason_for`)
/// both mean exactly the opposite: nothing here, so a Pending/Skipped
/// sibling must never itself count as "started".
fn row_started(rows: &[Row], path: &str) -> bool {
    rows.iter().any(|r| {
        r.path == path && !matches!(r.status.kind, StatusKind::Pending | StatusKind::Skipped)
    })
}

/// The earliest `created_at` among `paths`' own rows (issue #635: an
/// unnamed `if`'s inferred decision has no `meta.branch`/`created_at`
/// of its own, so its story entry's clock time is read from the
/// taken branch's own first observed step instead).
fn earliest_created_at(
    rows: &[Row],
    flat: &BTreeMap<String, NodeMeta>,
    paths: &[String],
) -> Option<String> {
    rows.iter()
        .filter(|r| paths.contains(&r.path))
        .filter_map(|r| flat.get(&r.path).and_then(|n| n.created_at.clone()))
        .min()
}

fn any_branch_started(rows: &[Row], paths: &[String]) -> bool {
    paths.iter().any(|p| row_started(rows, p))
}

/// The untaken side's own dim summary (DESIGN.md: "`· else: <its
/// first step>` (`+N more`, or `nothing` if it's empty)").
fn untaken_branch_summary(rows: &[Row], branch_label: &str, paths: &[String]) -> String {
    let Some(first) = paths.first() else {
        return format!("{branch_label}: nothing");
    };
    let label = rows
        .iter()
        .find(|r| &r.path == first)
        .map(|r| r.label.clone())
        .unwrap_or_else(|| first.rsplit('.').next().unwrap_or(first).to_string());
    if paths.len() > 1 {
        format!("{branch_label}: {label}  (+{} more)", paths.len() - 1)
    } else {
        format!("{branch_label}: {label}")
    }
}

/// A named `if`'s own decision state, from the same node `meta`
/// field every other row already reads (DESIGN.md: "A named `if`
/// takes its branch from `meta.branch`").
fn named_decision(node: Option<&NodeMeta>) -> (Option<String>, Option<String>) {
    let Some(node) = node else {
        return (None, None);
    };
    let answer = node.meta.get("answer").map(|v| match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    });
    (node.branch.clone(), answer)
}

/// Whether `path` carries a *named* `if`'s own `PlanEntry` (issue
/// #635) -- never just `row.kind == RowKind::If`, since an unnamed
/// `if` directly inside a named loop's pass body shares its own
/// path with the pass container itself, and `render::row_kind_for`
/// reads that shared path's plan entry (the unnamed `if`'s) ahead of
/// the pass fallback, giving the pass row `RowKind::If` too even
/// though there is no *named* decision there at all -- that one is
/// inferred instead, through `PlanTree::unnamed_if_decisions`.
fn is_named_if_path(row: &Row, flat: &BTreeMap<String, NodeMeta>, plan: &PlanTree) -> bool {
    match plan.match_path(&row.path) {
        // A known plan entry always wins, named or not -- an
        // unnamed `if`'s own row carries `RowKind::If` too
        // (`render::row_kind_for` reads the plan ahead of the node's
        // own shape, and never looks at `named` to decide the row's
        // *kind*), so this must never fall through to the no-plan
        // guess below just because `row.kind == RowKind::If`.
        Some(m) => m
            .entries
            .iter()
            .any(|e| matches!(&e.kind, crate::plan::PlanEntryKind::If { named: true, .. })),
        // `osi watch` without `--plan` (issue #635): `PlanTree::
        // empty()` has no entries at all, so a named `if`'s own row
        // is told apart from an ordinary one by its own observed
        // shape instead -- `render::row_kind_for` already guesses
        // `RowKind::If` from the node's own meta shape once there's
        // no plan entry to read, and `meta.branch` is exact
        // regardless of whether the plan is known.
        None => row.kind == RowKind::If || flat.get(&row.path).is_some_and(|n| n.branch.is_some()),
    }
}

fn condition_for_path(plan: &PlanTree, path: &str) -> Option<ConditionInfo> {
    let m = plan.match_path(path)?;
    m.entries.iter().find_map(|e| match &e.kind {
        crate::plan::PlanEntryKind::If {
            named: true,
            condition,
            ..
        } => condition.clone(),
        _ => None,
    })
}

fn branch_paths_for_path(plan: &PlanTree, path: &str) -> (Vec<String>, Vec<String>) {
    let Some(m) = plan.match_path(path) else {
        return (Vec::new(), Vec::new());
    };
    for e in m.entries {
        if let crate::plan::PlanEntryKind::If {
            named: true,
            then_paths,
            else_paths,
            ..
        } = &e.kind
        {
            return (then_paths.clone(), else_paths.clone());
        }
    }
    (Vec::new(), Vec::new())
}

fn row_tokens(row: &Row, flat: &BTreeMap<String, NodeMeta>) -> Option<u64> {
    let (sent, received) = crate::model::subtree_tokens(&row.path, flat);
    let total = sent + received;
    (total > 0).then_some(total)
}

fn row_cost(row: &Row, flat: &BTreeMap<String, NodeMeta>) -> Option<f64> {
    flat.get(&row.path)?.meta.get("cost_usd")?.as_f64()
}

fn row_start_ts(row: &Row, flat: &BTreeMap<String, NodeMeta>) -> Option<String> {
    flat.get(&row.path).and_then(|n| n.created_at.clone())
}

fn plain_flow_row(row: &Row, flat: &BTreeMap<String, NodeMeta>) -> FlowRow {
    FlowRow {
        path: row.path.clone(),
        depth: row.depth,
        label: row.label.clone(),
        kind: FlowRowKind::Row(row.kind),
        status: row.status.clone(),
        start_ts: row_start_ts(row, flat),
        duration_s: row.duration_s,
        tokens: row_tokens(row, flat),
        cost_usd: row_cost(row, flat),
        condition: None,
        loop_progress: row.loop_progress,
        has_children: row.has_children,
        merged_pass: false,
    }
}

/// Builds the Flow screen's own rows and footer (issue #635) -- the
/// decision/loop/quick-step tree, in the same depth-first order
/// `render::build_folded` already produced `rows` in, with every
/// named `if` augmented with its own condition and untaken-branch
/// summary, and every unnamed `if`'s own inferred decision spliced
/// in at its branches' own position. `unfolded_rows` is the real,
/// pre-fold tree (same as `screens::build_overview`'s own quick-step
/// counting): folding can merge or hide a quick-step run depending on
/// `expanded_quick_steps`, but the footer's own total must count
/// every quick step in the run regardless.
/// Every path that lies under a decision's own untaken branch, once
/// that decision has resolved which side ran -- a named `if` reads
/// its branch from `meta.branch`, an unnamed one the same way
/// [`push_unnamed_decision`] infers it. The untaken side gets exactly
/// one dim summary line ([`untaken_branch_summary`]); its own real
/// rows, still present in `rows` with [`StatusKind::Skipped`], are
/// hidden from the flow pane entirely rather than drawn as a `↷` row
/// each (DESIGN.md: "never a row per untaken step").
fn untaken_root_paths(
    rows: &[Row],
    flat: &BTreeMap<String, NodeMeta>,
    plan: &PlanTree,
    resolved_unnamed: &[UnnamedIfDecision],
) -> BTreeSet<String> {
    let mut hidden = BTreeSet::new();
    for row in rows {
        if !is_named_if_path(row, flat, plan) {
            continue;
        }
        let (branch, _) = named_decision(flat.get(&row.path));
        let (then_paths, else_paths) = branch_paths_for_path(plan, &row.path);
        match branch.as_deref() {
            Some("then") => hidden.extend(else_paths),
            Some("else") => hidden.extend(then_paths),
            _ => {}
        }
    }
    for decision in resolved_unnamed {
        let (branch, nothing_ran) = unnamed_branch_state(decision, rows);
        match branch.as_deref() {
            Some("then") => hidden.extend(decision.else_paths.clone()),
            Some("else") if !nothing_ran => hidden.extend(decision.then_paths.clone()),
            _ => {}
        }
    }
    hidden
}

fn is_under_any(path: &str, roots: &BTreeSet<String>) -> bool {
    roots
        .iter()
        .any(|root| path == root || path.starts_with(&format!("{root}.")))
}

pub fn build_flow_rows(
    rows: &[Row],
    unfolded_rows: &[Row],
    flat: &BTreeMap<String, NodeMeta>,
    plan: &PlanTree,
    story_lines: &[LogLine],
) -> (Vec<FlowRow>, FlowFooter) {
    let resolved = resolve_unnamed_decisions(plan.unnamed_if_decisions(), unfolded_rows);
    // Each decision's own anchor: the first row position (by path)
    // either branch actually has evidence for yet -- `then` or
    // `else`, whichever comes first in the branches' own order --
    // falling back to right *after* its own host row once neither
    // has (a pass that hasn't started, an empty branch with no real
    // path on either side, or a branch whose own first step folded
    // away into an ancestor's breadcrumb). Evidence
    // is read from `unfolded_rows`, never the folded `rows` this
    // loop draws from: a breadcrumb keeps only its own leaf's path,
    // and quick-step folding keeps only its run's first path, so
    // either can make a branch's own first step vanish from `rows`
    // well before the branch itself stops being real.
    let mut by_anchor: BTreeMap<String, Vec<&UnnamedIfDecision>> = BTreeMap::new();
    let mut by_host_after: BTreeMap<String, Vec<&UnnamedIfDecision>> = BTreeMap::new();
    for decision in &resolved {
        let anchor = decision
            .then_paths
            .iter()
            .chain(decision.else_paths.iter())
            .find(|p| unfolded_rows.iter().any(|r| &r.path == *p))
            .cloned();
        match anchor {
            Some(anchor) => by_anchor.entry(anchor).or_default().push(decision),
            None => by_host_after
                .entry(decision.host_path.clone())
                .or_default()
                .push(decision),
        }
    }
    let hidden = untaken_root_paths(unfolded_rows, flat, plan, &resolved);

    let mut out = Vec::new();
    let mut footer = FlowFooter::default();

    for row in rows {
        if is_under_any(&row.path, &hidden) {
            continue;
        }
        // An anchor path a breadcrumb or a quick-steps run folded
        // away still has *some* surviving row at or under its own
        // prefix (folding only ever merges into a descendant or a
        // same-run sibling, never moves content outside its own
        // subtree) -- `starts_with` catches the position the fold
        // left it at.
        let anchored_here: Vec<String> = by_anchor
            .keys()
            .filter(|anchor| row.path == **anchor || row.path.starts_with(&format!("{anchor}.")))
            .cloned()
            .collect();
        for anchor in anchored_here {
            if let Some(decisions) = by_anchor.remove(&anchor) {
                for decision in decisions {
                    push_unnamed_decision(&mut out, decision, unfolded_rows, flat, row.depth);
                }
            }
        }
        if is_named_if_path(row, flat, plan) {
            push_named_decision(&mut out, row, unfolded_rows, flat, plan);
        } else {
            out.push(plain_flow_row(row, flat));
        }
        if let Some(decisions) = by_host_after.remove(&row.path) {
            for decision in decisions {
                push_unnamed_decision(&mut out, decision, unfolded_rows, flat, row.depth + 1);
            }
        }
        if row.status.retrying {
            footer.retries += 1;
        }
    }
    // Any decision whose own host path never appeared as a row at
    // all stays unresolved this frame -- never emitted with nothing
    // to position it against.
    let _ = by_anchor;
    let _ = by_host_after;

    // The same leaf-only rule the overview's own flow card counts
    // failures by (`screens::build_flow_card`): a container's status
    // propagates from a failed descendant, so only the leaves (no
    // failed descendant of their own) are real failures.
    let failing: Vec<&Row> = rows
        .iter()
        .filter(|r| {
            r.kind != RowKind::Root
                && matches!(
                    r.status.kind,
                    StatusKind::Failed | StatusKind::FailedHandled
                )
        })
        .collect();
    footer.failed = failing
        .iter()
        .filter(|r| {
            let prefix = format!("{}.", r.path);
            !failing.iter().any(|other| other.path.starts_with(&prefix))
        })
        .count() as u64;

    footer.quick_steps_folded = unfolded_rows.iter().filter(|r| is_quick_step(r)).count() as u64;
    footer.quick_steps_duration_s = unfolded_rows
        .iter()
        .filter(|r| is_quick_step(r))
        .filter_map(|r| r.duration_s)
        .sum();
    footer.warnings = story_lines
        .iter()
        .filter(|l| l.kind == LogKind::Report && l.text.contains("warning: "))
        .count() as u64;

    merge_single_pass_loops(&mut out);

    (out, footer)
}

/// A named loop with exactly one observed pass shows as one row
/// (DESIGN.md: "a loop and its single pass show as one row, `⟳ <name>
/// pass N of M`") -- the loop container row and its lone pass row
/// merge into one, and that pass's own children shift up one depth
/// to sit directly under it. A loop with more passes keeps one row
/// per pass, unchanged.
fn merge_single_pass_loops(out: &mut Vec<FlowRow>) {
    let mut i = 0;
    while i < out.len() {
        if out[i].kind != FlowRowKind::Row(RowKind::Loop) {
            i += 1;
            continue;
        }
        let loop_depth = out[i].depth;
        // Whether this is genuinely a single-pass loop, not just a
        // loop only one of whose M passes has run so far: a known
        // total settles it outright, and an unknown total (a
        // `dynamic` loop) settles it only once the loop itself has
        // finished, since until then a second pass could still
        // start.
        let total = out[i].loop_progress.as_ref().and_then(|p| p.total);
        let loop_settled = matches!(
            out[i].status.kind,
            StatusKind::Done
                | StatusKind::Failed
                | StatusKind::FailedHandled
                | StatusKind::Cancelled
        );
        let single_pass = match total {
            Some(total) => total == 1,
            None => loop_settled,
        };
        if !single_pass {
            i += 1;
            continue;
        }
        let Some(pass_idx) = out
            .iter()
            .enumerate()
            .skip(i + 1)
            .take_while(|(_, r)| r.depth > loop_depth)
            .find(|(_, r)| r.depth == loop_depth + 1)
            .map(|(idx, _)| idx)
        else {
            i += 1;
            continue;
        };
        // The pass row's own path, not its `kind`, is what marks it
        // as a pass: a single-step pass body has a plan entry of its
        // own at that same path, so `render::row_kind_for` gives it
        // that step's kind (or, folded into a breadcrumb with its
        // own one child, the breadcrumb's kind) well ahead of the
        // `RowKind::Pass` fallback, the same way an unnamed `if`
        // sharing the pass's path can give it `RowKind::If` instead
        // (this module's own doc comment on `is_named_if_path`).
        let is_pass_path = out[pass_idx]
            .path
            .rsplit('.')
            .next()
            .is_some_and(|seg| seg.starts_with("iter_"));
        if !is_pass_path {
            i += 1;
            continue;
        }
        // A second pass row at the same depth means more than one
        // pass has run -- the merge applies only to a *single* pass.
        let has_second_pass = out[pass_idx + 1..]
            .iter()
            .take_while(|r| r.depth > loop_depth)
            .any(|r| r.depth == loop_depth + 1);
        if has_second_pass {
            i += 1;
            continue;
        }
        let pass_label = out[pass_idx].label.clone();
        let pass_status = out[pass_idx].status.clone();
        let pass_duration = out[pass_idx].duration_s;
        let pass_start = out[pass_idx].start_ts.clone();
        out[i].label = format!("{}  {pass_label}", out[i].label);
        out[i].status = pass_status;
        out[i].duration_s = pass_duration;
        out[i].start_ts = pass_start;
        out[i].loop_progress = None;
        out[i].merged_pass = true;
        out.remove(pass_idx);
        for row in out[pass_idx..].iter_mut() {
            if row.depth > loop_depth + 1 {
                row.depth -= 1;
            } else {
                break;
            }
        }
        i += 1;
    }
}

fn push_named_decision(
    out: &mut Vec<FlowRow>,
    row: &Row,
    rows: &[Row],
    flat: &BTreeMap<String, NodeMeta>,
    plan: &PlanTree,
) {
    let node = flat.get(&row.path);
    let (branch, answer) = named_decision(node);
    let condition = condition_for_path(plan, &row.path);
    let (then_paths, else_paths) = branch_paths_for_path(plan, &row.path);
    out.push(FlowRow {
        path: row.path.clone(),
        depth: row.depth,
        label: row.label.clone(),
        kind: FlowRowKind::Decision {
            branch: branch.clone(),
            inferred: false,
            nothing_ran: false,
            answer,
        },
        status: row.status.clone(),
        start_ts: row_start_ts(row, flat),
        duration_s: row.duration_s,
        tokens: row_tokens(row, flat),
        cost_usd: row_cost(row, flat),
        condition,
        loop_progress: row.loop_progress,
        has_children: row.has_children,
        merged_pass: false,
    });
    let untaken = match branch.as_deref() {
        Some("then") => Some(untaken_branch_summary(rows, "else", &else_paths)),
        Some("else") => Some(untaken_branch_summary(rows, "then", &then_paths)),
        _ => None,
    };
    if let Some(summary) = untaken {
        out.push(FlowRow {
            path: format!("{}.__untaken", row.path),
            depth: row.depth + 1,
            label: summary,
            kind: FlowRowKind::UntakenBranch,
            status: RowStatus {
                kind: StatusKind::Skipped,
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
            merged_pass: false,
        });
    }
}

/// An unnamed `if`'s own branch, inferred the same way regardless of
/// whether the caller wants to draw its decision row
/// ([`push_unnamed_decision`]) or just needs to know which side to
/// hide ([`untaken_root_paths`]).
fn unnamed_branch_state(decision: &UnnamedIfDecision, rows: &[Row]) -> (Option<String>, bool) {
    let then_started = any_branch_started(rows, &decision.then_paths);
    let else_started = any_branch_started(rows, &decision.else_paths);
    // Any way the host scope can stop running settles "nothing ran"
    // for a no-`else` `if`, not just a clean `Done` -- a `Failed`/
    // `FailedHandled`/`Cancelled` host is just as settled that its
    // own `then` never started.
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
    if then_started && !else_started {
        (Some("then".to_string()), false)
    } else if else_started && !then_started {
        (Some("else".to_string()), false)
    } else if decision.else_paths.is_empty() && host_done {
        (Some("else".to_string()), true)
    } else {
        (None, false)
    }
}

/// An unnamed `if` has no name of its own to tell two of them apart
/// by, so its row's own label carries a cut-short piece of its
/// condition too (DESIGN.md's Decisions section: "labelled `◆ if
/// <condition, cut short>`"), not just the separate dim condition
/// line a named `if`'s own name already makes optional.
const UNNAMED_IF_LABEL_CHARS: usize = 28;

fn unnamed_if_label(condition: Option<&ConditionInfo>) -> String {
    let Some(condition) = condition else {
        return "if".to_string();
    };
    let text = condition.text.lines().next().unwrap_or(&condition.text);
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= UNNAMED_IF_LABEL_CHARS {
        format!("if {text}")
    } else {
        let cut: String = chars[..UNNAMED_IF_LABEL_CHARS].iter().collect();
        format!("if {cut}…")
    }
}

fn push_unnamed_decision(
    out: &mut Vec<FlowRow>,
    decision: &UnnamedIfDecision,
    rows: &[Row],
    flat: &BTreeMap<String, NodeMeta>,
    depth: usize,
) {
    let (branch, nothing_ran) = unnamed_branch_state(decision, rows);
    let host_row = rows.iter().find(|r| r.path == decision.host_path);
    let same_names = !decision.then_paths.is_empty()
        && decision.then_paths == decision.else_paths
        && branch.is_none();
    let start_ts = match branch.as_deref() {
        Some("then") => earliest_created_at(rows, flat, &decision.then_paths),
        Some("else") if nothing_ran => {
            host_row.and_then(|r| flat.get(&r.path)?.completed_at.clone())
        }
        Some("else") => earliest_created_at(rows, flat, &decision.else_paths),
        _ => None,
    };
    out.push(FlowRow {
        path: format!("{}.__if", decision.host_path),
        depth,
        label: if same_names {
            "if → ?".to_string()
        } else {
            unnamed_if_label(decision.condition.as_ref())
        },
        kind: FlowRowKind::Decision {
            branch: branch.clone(),
            inferred: true,
            nothing_ran,
            answer: None,
        },
        status: RowStatus {
            kind: StatusKind::Pending,
            skip_reason: None,
            retrying: false,
        },
        start_ts,
        duration_s: None,
        tokens: None,
        cost_usd: None,
        condition: decision.condition.clone(),
        loop_progress: None,
        has_children: false,
        merged_pass: false,
    });
    let untaken = match branch.as_deref() {
        Some("then") => Some(untaken_branch_summary(rows, "else", &decision.else_paths)),
        Some("else") if !nothing_ran => {
            Some(untaken_branch_summary(rows, "then", &decision.then_paths))
        }
        _ => None,
    };
    if let Some(summary) = untaken {
        out.push(FlowRow {
            path: format!("{}.__untaken", decision.host_path),
            depth: depth + 1,
            label: summary,
            kind: FlowRowKind::UntakenBranch,
            status: RowStatus {
                kind: StatusKind::Skipped,
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
            merged_pass: false,
        });
    }
}

/// A real decision line's own text (`diff::LogLine::text`, shared
/// verbatim with `--log`) names the step by its full dotted path --
/// `◆ prime.gate → then` -- but the story shows only its own short
/// name, `gate → then` (DESIGN.md's own example line), so this strips
/// everything up to and including the last `.` before the arrow,
/// display-only: the `LogLine` itself, and `--log`'s own output, are
/// never touched.
fn shorten_decision_path(text: &str) -> String {
    let Some(rest) = text.strip_prefix("\u{25c6} ") else {
        return text.to_string();
    };
    let Some((path, tail)) = rest.split_once(' ') else {
        return text.to_string();
    };
    let short = path.rsplit('.').next().unwrap_or(path);
    format!("\u{25c6} {short} {tail}")
}

/// The run's own story (DESIGN.md: "the run's notable events, oldest
/// first, with clock times"): `story_lines` is the caller's own
/// bounded buffer of real `--log` lines, raw `ts`/`text` (never
/// pre-formatted -- `LogKind` already filtered it to the story's own
/// kinds: run start/end, decision, report, failure, same as `--log`
/// otherwise is); every unnamed `if`'s own inferred decision this
/// same build resolved joins in too, with the clock time of whichever
/// branch it read as taken. Sorted by each entry's own real instant
/// (`diff::parse_instant`, the same RFC-3339 parser `sort_log_lines`
/// uses), never a plain string compare -- an `--events` line's own
/// millisecond timestamp and a state-sourced one's microsecond one
/// aren't lexicographically comparable at all, and a `None` (an
/// inferred decision whose own branch hasn't resolved a start time
/// yet) sorts last rather than first.
pub fn build_story(
    story_lines: &[LogLine],
    flow_rows: &[FlowRow],
    zone: &dyn ClockZone,
) -> Vec<StoryEntry> {
    let mut keyed: Vec<(Option<String>, StoryEntry)> = story_lines
        .iter()
        .map(|l| {
            let clock = l.ts.as_deref().and_then(|ts| format_clock_time(ts, zone));
            let body = if l.kind == LogKind::Decision {
                shorten_decision_path(&l.text)
            } else {
                l.text.clone()
            };
            let text = match &clock {
                Some(clock) => format!("{clock} {body}"),
                None => body,
            };
            (
                l.ts.clone(),
                StoryEntry {
                    text,
                    inferred: false,
                },
            )
        })
        .collect();
    for row in flow_rows {
        if let FlowRowKind::Decision {
            branch: Some(branch),
            inferred: true,
            ..
        } = &row.kind
        {
            let clock = row
                .start_ts
                .as_deref()
                .and_then(|ts| format_clock_time(ts, zone));
            let text = match &clock {
                Some(clock) => format!("{clock} \u{25c6} {} \u{2192} {branch}", row.label),
                None => format!("\u{25c6} {} \u{2192} {branch}", row.label),
            };
            keyed.push((
                row.start_ts.clone(),
                StoryEntry {
                    text,
                    inferred: true,
                },
            ));
        }
    }
    keyed.sort_by(|a, b| match (&a.0, &b.0) {
        (Some(x), Some(y)) => {
            match (crate::diff::parse_instant(x), crate::diff::parse_instant(y)) {
                (Some(ix), Some(iy)) => ix.cmp(&iy),
                _ => x.cmp(y),
            }
        }
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    keyed.into_iter().map(|(_, entry)| entry).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ProcessState, RunModel, flatten_state};
    use crate::plan::PlanTree;
    use electricity_bytecode::{
        Condition, EffectPath, LeafKind, LoopFlow, LoopId, LoopSpec, NodeKind, OnError, Op,
        ParamNode, Program, Region, ToolOp,
    };
    use serde_json::json;

    fn tool_op(path: EffectPath, name: &str) -> Op {
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
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        }
    }

    fn cel(expr: &str) -> Condition {
        Condition::Cel {
            expr: expr.to_string(),
            strict: false,
        }
    }

    fn block(ops: Vec<Op>) -> Region {
        Region::Block { ops, overlay: true }
    }

    fn program(root: Op) -> Program {
        Program {
            root,
            prompts: Default::default(),
            effect_names: Default::default(),
            document: None,
            runtime_block: None,
            interface: None,
            adapter: None,
            model: None,
        }
    }

    fn named_if_plan() -> PlanTree {
        let root_path = EffectPath::root();
        let if_path = root_path.clone().push_name("gate");
        let chosen = if_path.clone().push_name("chosen");
        let other = if_path.clone().push_name("other");
        let root = Op {
            path: root_path,
            name: Some("prime".to_string()),
            kind: NodeKind::Control(block(vec![Op {
                path: if_path,
                name: Some("gate".to_string()),
                kind: NodeKind::Control(Region::If {
                    cond: cel("x > 1"),
                    then_: Box::new(block(vec![tool_op(chosen, "chosen")])),
                    else_: Some(Box::new(block(vec![tool_op(other, "other")]))),
                    threshold: 0.5,
                }),
                on_error: OnError::Fail,
                labels: None,
                enabled: true,
            }])),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        PlanTree::from_program(&program(root))
    }

    fn rows_for(
        plan: &PlanTree,
        state: &Value,
        now: &str,
    ) -> (Vec<Row>, BTreeMap<String, NodeMeta>) {
        let flat = flatten_state(state);
        let mut model = RunModel::new();
        let statuses = model.observe(state, plan, ProcessState::Running);
        let all_paths: BTreeMap<String, ()> = statuses
            .keys()
            .chain(flat.keys())
            .map(|k| (k.clone(), ()))
            .collect();
        let rows = crate::render::build_rows(plan, &flat, &statuses, &all_paths, now, &model, None);
        (rows, flat)
    }

    #[test]
    fn a_named_if_before_evaluation_has_no_branch_yet() {
        let plan = named_if_plan();
        let state = json!({"prime": {"value": null, "meta": {"created_at": "t0", "completed_at": null},
            "gate": {"value": null, "meta": {"created_at": "t0", "completed_at": null}}
        }});
        let (rows, flat) = rows_for(&plan, &state, "t1");
        let (flow_rows, _) = build_flow_rows(&rows, &rows, &flat, &plan, &[]);
        let decision = flow_rows
            .iter()
            .find(|r| r.path == "prime.gate")
            .expect("a decision row for the if");
        match &decision.kind {
            FlowRowKind::Decision { branch, .. } => assert_eq!(branch, &None),
            other => panic!("expected a Decision row, got {other:?}"),
        }
        assert_eq!(
            decision.condition,
            Some(ConditionInfo {
                mode: crate::plan::ConditionMode::Cel,
                text: "x > 1".to_string()
            })
        );
    }

    #[test]
    fn a_named_ifs_taken_branch_gets_an_untaken_branch_summary_line() {
        let plan = named_if_plan();
        let state = json!({"prime": {"value": null, "meta": {"created_at": "t0", "completed_at": null},
            "gate": {"value": null, "meta": {"created_at": "t0", "completed_at": "t1", "branch": "then"},
                "chosen": {"value": "ok", "meta": {"created_at": "t1", "completed_at": "t2"}}
            }
        }});
        let (rows, flat) = rows_for(&plan, &state, "t3");
        let (flow_rows, _) = build_flow_rows(&rows, &rows, &flat, &plan, &[]);
        let idx = flow_rows
            .iter()
            .position(|r| r.path == "prime.gate")
            .expect("the decision row");
        match &flow_rows[idx].kind {
            FlowRowKind::Decision { branch, .. } => assert_eq!(branch.as_deref(), Some("then")),
            other => panic!("expected Decision, got {other:?}"),
        }
        let summary = &flow_rows[idx + 1];
        assert_eq!(summary.kind, FlowRowKind::UntakenBranch);
        assert!(summary.label.contains("other"), "{}", summary.label);
    }

    fn unnamed_if_plan(with_else: bool) -> PlanTree {
        let root_path = EffectPath::root();
        let then_child = root_path.clone().push_name("then_branch");
        let else_child = root_path.clone().push_name("else_branch");
        let else_ = if with_else {
            Some(Box::new(block(vec![tool_op(else_child, "else_branch")])))
        } else {
            None
        };
        let root = Op {
            path: root_path.clone(),
            name: Some("prime".to_string()),
            kind: NodeKind::Control(block(vec![Op {
                path: root_path,
                name: None,
                kind: NodeKind::Control(Region::If {
                    cond: cel("true"),
                    then_: Box::new(block(vec![tool_op(then_child, "then_branch")])),
                    else_,
                    threshold: 0.5,
                }),
                on_error: OnError::Fail,
                labels: None,
                enabled: true,
            }])),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        PlanTree::from_program(&program(root))
    }

    #[test]
    fn an_unnamed_if_with_else_infers_the_taken_branch_once_a_step_appears() {
        let plan = unnamed_if_plan(true);
        let state = json!({"prime": {"value": null, "meta": {"created_at": "t0", "completed_at": null},
            "then_branch": {"value": "ok", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}}
        }});
        let (rows, flat) = rows_for(&plan, &state, "t2");
        let (flow_rows, _) = build_flow_rows(&rows, &rows, &flat, &plan, &[]);
        let decision = flow_rows
            .iter()
            .find(|r| matches!(r.kind, FlowRowKind::Decision { inferred: true, .. }))
            .expect("an inferred decision row");
        match &decision.kind {
            FlowRowKind::Decision {
                branch, inferred, ..
            } => {
                assert!(inferred);
                assert_eq!(branch.as_deref(), Some("then"));
            }
            other => panic!("expected Decision, got {other:?}"),
        }
    }

    #[test]
    fn an_unnamed_if_with_no_else_reads_as_nothing_ran_once_the_run_ends() {
        let plan = unnamed_if_plan(false);
        let state =
            json!({"prime": {"value": null, "meta": {"created_at": "t0", "completed_at": "t1"}}});
        let (rows, flat) = rows_for(&plan, &state, "t2");
        let (flow_rows, _) = build_flow_rows(&rows, &rows, &flat, &plan, &[]);
        let decision = flow_rows
            .iter()
            .find(|r| matches!(r.kind, FlowRowKind::Decision { inferred: true, .. }))
            .expect("an inferred decision row");
        match &decision.kind {
            FlowRowKind::Decision {
                branch,
                nothing_ran,
                ..
            } => {
                assert_eq!(branch.as_deref(), Some("else"));
                assert!(*nothing_ran);
            }
            other => panic!("expected Decision, got {other:?}"),
        }
    }

    #[test]
    fn an_unnamed_if_not_yet_decided_has_no_branch() {
        let plan = unnamed_if_plan(true);
        let state =
            json!({"prime": {"value": null, "meta": {"created_at": "t0", "completed_at": null}}});
        let (rows, flat) = rows_for(&plan, &state, "t1");
        let (flow_rows, _) = build_flow_rows(&rows, &rows, &flat, &plan, &[]);
        let decision = flow_rows
            .iter()
            .find(|r| matches!(r.kind, FlowRowKind::Decision { inferred: true, .. }));
        // With nothing started on either branch and the run still
        // going, there is nothing to anchor an undecided row on yet
        // -- plan-seeded Pending rows for both branches exist, so the
        // decision does get emitted, with no branch chosen.
        let decision = decision.expect("an inferred decision row still appears, undecided");
        match &decision.kind {
            FlowRowKind::Decision { branch, .. } => assert_eq!(branch, &None),
            other => panic!("expected Decision, got {other:?}"),
        }
    }

    fn unnamed_if_in_loop_pass_plan() -> PlanTree {
        let root_path = EffectPath::root();
        let loop_path = root_path.clone().push_name("fan");
        let pass_path = loop_path.clone().push_pass(LoopId(0));
        let nap_path = pass_path.clone().push_name("nap");
        let root = Op {
            path: root_path,
            name: Some("prime".to_string()),
            kind: NodeKind::Control(block(vec![Op {
                path: loop_path,
                name: Some("fan".to_string()),
                kind: NodeKind::Control(Region::Loop {
                    name: Some("fan".to_string()),
                    spec: LoopSpec::Each {
                        in_path: "prime.items".to_string(),
                        as_name: "item".to_string(),
                        truncate: false,
                    },
                    body: Box::new(block(vec![Op {
                        path: pass_path.clone(),
                        name: None,
                        kind: NodeKind::Control(Region::If {
                            cond: cel("true"),
                            then_: Box::new(block(vec![tool_op(nap_path, "nap")])),
                            else_: None,
                            threshold: 0.5,
                        }),
                        on_error: OnError::Fail,
                        labels: None,
                        enabled: true,
                    }])),
                    flow: LoopFlow::Chain,
                    max_concurrency: None,
                    max_iterations: None,
                    min_iterations: 0,
                    collect: None,
                }),
                on_error: OnError::Fail,
                labels: None,
                enabled: true,
            }])),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        PlanTree::from_program(&program(root))
    }

    #[test]
    fn an_unnamed_if_inside_a_named_loops_pass_resolves_against_the_real_pass() {
        let plan = unnamed_if_in_loop_pass_plan();
        let state = json!({"prime": {"value": null, "meta": {"created_at": "t0", "completed_at": null},
            "fan": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "mode": "each"},
                "iter_0": {"value": null, "meta": {"created_at": "t0", "completed_at": null},
                    "nap": {"value": "ok", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}}
                }
            }
        }});
        let (rows, flat) = rows_for(&plan, &state, "t2");
        let (flow_rows, _) = build_flow_rows(&rows, &rows, &flat, &plan, &[]);
        let decision = flow_rows
            .iter()
            .find(|r| matches!(r.kind, FlowRowKind::Decision { inferred: true, .. }))
            .expect("a decision row resolved against the real pass");
        assert_eq!(decision.path, "prime.fan.iter_0.__if");
        match &decision.kind {
            FlowRowKind::Decision { branch, .. } => assert_eq!(branch.as_deref(), Some("then")),
            other => panic!("expected Decision, got {other:?}"),
        }
    }

    fn unnamed_if_under_parallel_plan() -> PlanTree {
        let root_path = EffectPath::root();
        let fan_path = root_path.clone().push_name("fan");
        let a_path = fan_path.clone().push_name("a");
        let then_child = fan_path.clone().push_name("then_branch");
        let root = Op {
            path: root_path,
            name: Some("prime".to_string()),
            kind: NodeKind::Control(block(vec![Op {
                path: fan_path.clone(),
                name: Some("fan".to_string()),
                kind: NodeKind::Control(Region::Parallel {
                    branches: vec![
                        tool_op(a_path, "a"),
                        Op {
                            path: fan_path,
                            name: None,
                            kind: NodeKind::Control(Region::If {
                                cond: cel("true"),
                                then_: Box::new(block(vec![tool_op(then_child, "then_branch")])),
                                else_: None,
                                threshold: 0.5,
                            }),
                            on_error: OnError::Fail,
                            labels: None,
                            enabled: true,
                        },
                    ],
                    max_concurrency: None,
                    stop_on_error: false,
                }),
                on_error: OnError::Fail,
                labels: None,
                enabled: true,
            }])),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        PlanTree::from_program(&program(root))
    }

    #[test]
    fn an_unnamed_if_directly_under_a_tree_dynamic_still_gets_a_decision_row() {
        let plan = unnamed_if_under_parallel_plan();
        let state = json!({"prime": {"value": null, "meta": {"created_at": "t0", "completed_at": null},
            "fan": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "flow": "tree"},
                "then_branch": {"value": "ok", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}}
            }
        }});
        let (rows, flat) = rows_for(&plan, &state, "t2");
        let (flow_rows, _) = build_flow_rows(&rows, &rows, &flat, &plan, &[]);
        let decision = flow_rows
            .iter()
            .find(|r| matches!(r.kind, FlowRowKind::Decision { inferred: true, .. }))
            .expect("a decision row under the tree dynamic");
        match &decision.kind {
            FlowRowKind::Decision { branch, .. } => assert_eq!(branch.as_deref(), Some("then")),
            other => panic!("expected Decision, got {other:?}"),
        }
    }

    #[test]
    fn two_branches_with_the_same_step_names_read_as_undecided_question_mark() {
        let root_path = EffectPath::root();
        let then_child = root_path.clone().push_name("chosen");
        let else_child = root_path.clone().push_name("chosen");
        let root = Op {
            path: root_path.clone(),
            name: Some("prime".to_string()),
            kind: NodeKind::Control(block(vec![Op {
                path: root_path,
                name: None,
                kind: NodeKind::Control(Region::If {
                    cond: cel("true"),
                    then_: Box::new(block(vec![tool_op(then_child, "chosen")])),
                    else_: Some(Box::new(block(vec![tool_op(else_child, "chosen")]))),
                    threshold: 0.5,
                }),
                on_error: OnError::Fail,
                labels: None,
                enabled: true,
            }])),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        let plan = PlanTree::from_program(&program(root));
        let state = json!({"prime": {"value": null, "meta": {"created_at": "t0", "completed_at": null},
            "chosen": {"value": "ok", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}}
        }});
        let (rows, flat) = rows_for(&plan, &state, "t2");
        let (flow_rows, _) = build_flow_rows(&rows, &rows, &flat, &plan, &[]);
        let decision = flow_rows
            .iter()
            .find(|r| matches!(r.kind, FlowRowKind::Decision { inferred: true, .. }))
            .expect("a decision row");
        assert_eq!(decision.label, "if \u{2192} ?");
    }

    #[test]
    fn the_footer_counts_quick_steps_from_the_unfolded_rows() {
        let root_path = EffectPath::root();
        let a_path = root_path.clone().push_name("a");
        let b_path = root_path.clone().push_name("b");
        let root = Op {
            path: root_path,
            name: Some("prime".to_string()),
            kind: NodeKind::Control(block(vec![tool_op(a_path, "a"), tool_op(b_path, "b")])),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        let plan = PlanTree::from_program(&program(root));
        let state = json!({"prime": {"value": null, "meta": {
            "created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:00:01Z"},
            "a": {"value": "ok", "meta": {
                "created_at": "2026-01-01T00:00:00.000Z", "completed_at": "2026-01-01T00:00:00.300Z", "provider": "shell"}},
            "b": {"value": "ok", "meta": {
                "created_at": "2026-01-01T00:00:00.300Z", "completed_at": "2026-01-01T00:00:00.600Z", "provider": "shell"}}
        }});
        let (rows, flat) = rows_for(&plan, &state, "2026-01-01T00:00:01Z");
        let (_, footer) = build_flow_rows(&rows, &rows, &flat, &plan, &[]);
        assert_eq!(footer.quick_steps_folded, 2);
    }

    struct FixedZone;
    impl crate::format::ClockZone for FixedZone {
        fn local(
            &self,
            utc: chrono::DateTime<chrono::Utc>,
        ) -> chrono::DateTime<chrono::FixedOffset> {
            utc.with_timezone(&chrono::FixedOffset::east_opt(0).unwrap())
        }
    }

    /// Issue #635: the story merges real log lines and inferred
    /// decisions oldest first, by clock time, including two lines
    /// landing in the same second (a real decision and a report
    /// arriving at the identical `ts`).
    #[test]
    fn build_story_orders_real_and_inferred_lines_oldest_first() {
        let real = vec![
            LogLine {
                ts: Some("2026-01-01T00:00:10Z".to_string()),
                text: "◆ prime.gate → then".to_string(),
                kind: crate::diff::LogKind::Decision,
                path: Some("prime.gate".to_string()),
            },
            LogLine {
                ts: Some("2026-01-01T00:00:10Z".to_string()),
                text: "▸ halfway".to_string(),
                kind: crate::diff::LogKind::Report,
                path: Some("prime.status".to_string()),
            },
            LogLine {
                ts: Some("2026-01-01T00:00:05Z".to_string()),
                text: "■ run started  abc".to_string(),
                kind: crate::diff::LogKind::RunStart,
                path: None,
            },
        ];
        let inferred_row = FlowRow {
            path: "prime.__if".to_string(),
            depth: 0,
            label: "if".to_string(),
            kind: FlowRowKind::Decision {
                branch: Some("then".to_string()),
                inferred: true,
                nothing_ran: false,
                answer: None,
            },
            status: RowStatus {
                kind: StatusKind::Pending,
                skip_reason: None,
                retrying: false,
            },
            start_ts: Some("2026-01-01T00:00:07Z".to_string()),
            duration_s: None,
            tokens: None,
            cost_usd: None,
            condition: None,
            loop_progress: None,
            has_children: false,
            merged_pass: false,
        };
        let entries = build_story(&real, &[inferred_row], &FixedZone);
        let texts: Vec<&str> = entries.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "00:00:05 ■ run started  abc",
                "00:00:07 ◆ if → then",
                "00:00:10 ◆ gate → then",
                "00:00:10 ▸ halfway",
            ]
        );
        assert!(!entries[0].inferred);
        assert!(entries[1].inferred);
    }

    /// An `--events` line's own millisecond timestamp and a state-
    /// sourced one's microsecond one aren't lexicographically
    /// comparable at all -- a plain string sort would read the
    /// state line's `064+00:00` suffix as sorting after the event
    /// line's `Z`, even though the state line's own instant is
    /// earlier.
    #[test]
    fn build_story_sorts_by_real_instant_not_by_string() {
        let real = vec![
            LogLine {
                ts: Some("2026-01-01T00:00:01.500Z".to_string()),
                text: "▸ later".to_string(),
                kind: crate::diff::LogKind::Report,
                path: Some("prime.status".to_string()),
            },
            LogLine {
                ts: Some("2026-01-01T00:00:01.200064+00:00".to_string()),
                text: "▸ earlier".to_string(),
                kind: crate::diff::LogKind::Report,
                path: Some("prime.status".to_string()),
            },
        ];
        let entries = build_story(&real, &[], &FixedZone);
        let texts: Vec<&str> = entries.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, vec!["00:00:01 ▸ earlier", "00:00:01 ▸ later"]);
    }
}
