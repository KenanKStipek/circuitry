//! The Cost screen's own "largest stores" table and "tokens over
//! time" chart (DESIGN.md, issue #632) — pure data, no terminal
//! code; `oscilloscope`'s `tui.rs` only ever draws what these
//! functions already decided. The treemap itself lives in
//! `treemap.rs`; this module reads the same `flat`/`statuses` inputs
//! but never the treemap's own layout, since the table and the chart
//! need every store (and every top-level phase) regardless of which
//! measure the treemap widget currently shows.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::model::{NodeMeta, RowStatus, StatusKind, node_tokens, run_totals};
use crate::render::RowKind;
use crate::treemap::{Measure, TreemapNode, build_tree_for_measure};

/// One row of the "largest stores" table (DESIGN.md: "path, tokens
/// in, tokens out, cost (when reported) and share of the total").
#[derive(Debug, Clone, PartialEq)]
pub struct StoreRow {
    /// This row's own path, relative to the table's own root (its
    /// caller's `root_path`, almost always the whole run), as a
    /// " › "-joined breadcrumb (`reviews › iter_1 › review`,
    /// [`path_breadcrumb_relative_to`]) -- DESIGN.md: "the table
    /// shows the path". Never just a leaf's own bare key: two stores
    /// sharing one (a `review` effect run inside several loop passes,
    /// say) would otherwise produce identical, indistinguishable
    /// rows. A single-child chain the treemap folds into one tile
    /// reads the same here as there (`plan › agent`), since both
    /// come from the same real dotted path.
    pub label: String,
    pub path: String,
    pub tokens_sent: u64,
    pub tokens_received: u64,
    pub cost_usd: Option<f64>,
    pub status: Option<StatusKind>,
}

/// The table's own footer (DESIGN.md: "gives the run's totals from
/// `runtime.last_run.totals` and states any gap between them and the
/// table").
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct StoresFooter {
    pub run_tokens_sent: u64,
    pub run_tokens_received: u64,
    pub run_cost_usd: Option<f64>,
    /// `(run_tokens_sent + run_tokens_received) - the table's own
    /// sum`, zero when the table already accounts for everything the
    /// run itself reports. Never negative: the table's own sum is
    /// read from exactly the same per-node field the run's own totals
    /// are built from (`model::node_tokens`), so it can only ever
    /// fall short of the run's own totals (a declared-outputs `use`
    /// child's effects, counted in the run's totals as they complete
    /// but never landing in the final state tree for the table to see
    /// at all, DESIGN.md §1.2), never exceed them.
    pub gap_tokens: u64,
    /// How many *finished* stores actually reported a cost
    /// (DESIGN.md's Cost section: "when only some effects report a
    /// cost, the figure is marked as partial") -- `cost_reporting ==
    /// cost_stores` means every one of them did, never more. A
    /// still-running store is never counted in either figure: it
    /// hasn't failed to report a cost, it simply hasn't finished yet.
    pub cost_reporting: u64,
    pub cost_stores: u64,
}

impl StoresFooter {
    /// `$4.20`, or `$4.20 (from 3 of 9 prompts)` once some finished
    /// stores never reported a cost at all (DESIGN.md's own example
    /// wording) -- `None` when nothing has reported one yet, same as
    /// [`crate::screens::effective_cost_usd`]'s own "never shown as
    /// $0.00" rule.
    pub fn cost_label(&self) -> Option<String> {
        let cost = self.run_cost_usd?;
        if self.cost_reporting < self.cost_stores {
            Some(format!(
                "${cost:.2} (from {} of {} prompts)",
                self.cost_reporting, self.cost_stores
            ))
        } else {
            Some(format!("${cost:.2}"))
        }
    }

    /// [`Self::cost_label`], shrunk to fit `max_width` columns without
    /// ever losing the one fragment it exists to show at all once it
    /// is partial -- `N of M` (issue #632's own small-terminal rule:
    /// "it may shorten, but N of M must stay"). Tried widest first:
    /// the full label; the same with the leading `$cost` figure
    /// dropped; `N of M prompts` alone. A label with nothing to drop
    /// (every store already reporting one, so there is no coverage
    /// note at all) is returned as-is regardless of `max_width` --
    /// truncating a bare `$4.20` down to a single fragment would lose
    /// the figure itself, the one thing a non-partial label has.
    pub fn cost_label_for_width(&self, max_width: usize) -> Option<String> {
        let full = self.cost_label()?;
        if full.chars().count() <= max_width || self.cost_reporting >= self.cost_stores {
            return Some(full);
        }
        let without_cost = format!(
            "(from {} of {} prompts)",
            self.cost_reporting, self.cost_stores
        );
        if without_cost.chars().count() <= max_width {
            return Some(without_cost);
        }
        Some(format!(
            "{} of {} prompts",
            self.cost_reporting, self.cost_stores
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct LargestStores {
    /// Sorted largest-total-first (ties broken by path, so the order
    /// is stable), same as the treemap's own `sorted_children`.
    pub rows: Vec<StoreRow>,
    pub footer: StoresFooter,
}

/// The single leaf-scan rule every ranking on the Cost screen (and
/// the overview's own "largest store" card, DESIGN.md: "the same
/// store the Cost screen ranks first") shares: the by-store tree's
/// own leaves, under [`Measure::Total`] regardless of which measure
/// the treemap widget currently shows -- the table's own ranking
/// never changes with `i`/`o`/`$`.
pub fn largest_stores(
    root_path: &str,
    flat: &BTreeMap<String, NodeMeta>,
    statuses: &BTreeMap<String, RowStatus>,
    kinds: &BTreeMap<String, RowKind>,
    state: Option<&Value>,
) -> LargestStores {
    let (tree, _excluded) =
        build_tree_for_measure(root_path, flat, statuses, kinds, Measure::Total);
    let mut rows = Vec::new();
    flatten_leaves_into(&tree, flat, root_path, &mut rows);
    rows.sort_by(|a, b| {
        (b.tokens_sent + b.tokens_received)
            .cmp(&(a.tokens_sent + a.tokens_received))
            .then_with(|| a.path.cmp(&b.path))
    });

    let table_tokens: u64 = rows.iter().map(|r| r.tokens_sent + r.tokens_received).sum();
    let (run_sent, run_received, run_cost_usd) = state
        .and_then(run_totals)
        .map(|totals| {
            (
                totals
                    .get("tokens_sent")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                totals
                    .get("tokens_received")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                totals.get("cost_usd").and_then(Value::as_f64),
            )
        })
        .unwrap_or_else(|| {
            // Mid-run, before `runtime.last_run.totals` exists: the
            // same `_total`-first fallback `render::build_header`
            // itself uses, plus the same running-agent live boost
            // the rows above already carry -- without it, a running
            // agent's own row could show more sent/received tokens
            // than this total did, which reads as the table
            // disagreeing with its own footer.
            let (sent, received, cost) =
                flat.values().fold((0u64, 0u64, 0.0f64), |(s, r, c), node| {
                    let (ns, nr) = node_tokens(&node.meta);
                    let (ns, nr) = if node.completed_at.is_none() {
                        match &node.live {
                            Some(live) => (
                                live.tokens_sent.map_or(ns, |v| ns.max(v)),
                                live.tokens_received.map_or(nr, |v| nr.max(v)),
                            ),
                            None => (ns, nr),
                        }
                    } else {
                        (ns, nr)
                    };
                    let nc = node
                        .meta
                        .get("cost_usd")
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0);
                    (s + ns, r + nr, c + nc)
                });
            (sent, received, (cost > 0.0).then_some(cost))
        });
    let gap_tokens = (run_sent + run_received).saturating_sub(table_tokens);
    let finished_rows = rows
        .iter()
        .filter(|r| r.status != Some(StatusKind::Running));
    let cost_reporting = finished_rows
        .clone()
        .filter(|r| r.cost_usd.is_some())
        .count() as u64;
    let cost_stores = finished_rows.count() as u64;

    LargestStores {
        rows,
        footer: StoresFooter {
            run_tokens_sent: run_sent,
            run_tokens_received: run_received,
            run_cost_usd,
            gap_tokens,
            cost_reporting,
            cost_stores,
        },
    }
}

fn flatten_leaves_into(
    nodes: &[TreemapNode],
    flat: &BTreeMap<String, NodeMeta>,
    root_path: &str,
    out: &mut Vec<StoreRow>,
) {
    for node in nodes {
        if node.children.is_empty() {
            if let Some(n) = flat.get(&node.path) {
                let (tokens_sent, tokens_received) = node_tokens(&n.meta);
                // Issue #637: a running agent's own row sizes from
                // its live `tokens_sent`/`tokens_received` too, the
                // same figure its treemap tile already does for
                // `tokens_sent` (`treemap::leaf_node_for_measure`) --
                // without this the table kept reading `0 t`/`tokens
                // when done` for exactly the tile that no longer
                // does, and (pi, which reports both live) a row whose
                // own `↓` never agreed with what it had actually sent
                // and received by the same moment.
                let (tokens_sent, tokens_received) = if n.completed_at.is_none() {
                    match &n.live {
                        Some(live) => (
                            live.tokens_sent.map_or(tokens_sent, |s| tokens_sent.max(s)),
                            live.tokens_received
                                .map_or(tokens_received, |r| tokens_received.max(r)),
                        ),
                        None => (tokens_sent, tokens_received),
                    }
                } else {
                    (tokens_sent, tokens_received)
                };
                let cost_usd = n.meta.get("cost_usd").and_then(Value::as_f64);
                // A `while: mode: model` loop's own "check" leaf
                // (`treemap::build_node_for_measure`) shares its path
                // with the container itself -- `path_breadcrumb_
                // relative_to` alone would read as the container's
                // own row (`fan`), indistinguishable from a tile that
                // doesn't exist. Appending `check` by hand, and
                // reading it bare once the container *is* the root
                // (zoomed into the loop itself, so there is nothing
                // left to strip a prefix off of), keeps this leaf
                // reading as the loop's own tokens, not the loop.
                let label = if node.key == "check" {
                    if node.path == root_path {
                        "check".to_string()
                    } else {
                        format!(
                            "{} › check",
                            path_breadcrumb_relative_to(&node.path, root_path)
                        )
                    }
                } else {
                    path_breadcrumb_relative_to(&node.path, root_path)
                };
                out.push(StoreRow {
                    label,
                    path: node.path.clone(),
                    tokens_sent,
                    tokens_received,
                    cost_usd,
                    status: node.status,
                });
            }
        } else {
            flatten_leaves_into(&node.children, flat, root_path, out);
        }
    }
}

/// A real dotted path, relative to `ancestor_path`, as a " › "-joined
/// breadcrumb (`reviews › iter_1 › review`) -- the table's own row
/// label (relative to the whole run) and, zoomed, a front end's own
/// re-derivation of the same thing relative to the zoomed store
/// instead, both built by calling this with a different
/// `ancestor_path`, never a second, possibly-diverging copy of the
/// stripping rule. `path` unchanged if it doesn't actually start with
/// `ancestor_path` (never expected, since every row's own path always
/// descends from the tree it was read out of).
pub fn path_breadcrumb_relative_to(path: &str, ancestor_path: &str) -> String {
    let prefix = format!("{ancestor_path}.");
    let relative = path.strip_prefix(prefix.as_str()).unwrap_or(path);
    relative.replace('.', " › ")
}

/// A table row's own label once zoomed into some ancestor store,
/// matching the tile the row has in the treemap there -- a `while:
/// mode: model` loop's own "check" row shares its path with the
/// zoomed-into container itself, so [`path_breadcrumb_relative_to`]
/// has no prefix left to strip and would otherwise read back as the
/// container's own unstripped path. The same special case
/// [`flatten_leaves_into`] already applies building the row's own
/// root-relative label, read here instead relative to `zoom_root`.
pub fn zoomed_row_label(row: &StoreRow, zoom_root: &str) -> String {
    if row.path == zoom_root {
        "check".to_string()
    } else {
        path_breadcrumb_relative_to(&row.path, zoom_root)
    }
}

/// An `iter_N` segment's own 1-based pass number, same reading
/// [`crate::render`]'s own (private) loop-pass label uses.
fn pass_number(segment: &str) -> Option<u64> {
    segment
        .strip_prefix("iter_")
        .and_then(|n| n.parse().ok())
        .map(|n: u64| n + 1)
}

/// Shortens a " › "-joined breadcrumb to at most `max_width` columns
/// by dropping whole leading segments and prefixing `…`, never the
/// trailing (leaf) segment -- the one part of a row's own path that
/// actually has to stay readable to tell it apart from a same-named
/// sibling elsewhere in the tree. Returns `label` unchanged if it
/// already fits, or has only one segment to begin with (nothing to
/// drop without losing the leaf itself).
///
/// Dropping blindly from the left can still leave two *different*
/// rows reading identically once shortened -- two loop passes whose
/// own breadcrumb agrees on everything below their own `iter_N`
/// (`… › check › answers`, issue #641) -- so the nearest `iter_N` to
/// the leaf, the one segment that actually told them apart, is kept
/// even once dropped from the plain path, as `pass N` (the same
/// label the orchestration pane's own loop passes read) right after
/// the `…`.
pub fn shorten_breadcrumb_from_left(label: &str, max_width: usize) -> String {
    if label.chars().count() <= max_width {
        return label.to_string();
    }
    let segments: Vec<&str> = label.split(" › ").collect();
    if segments.len() <= 1 {
        return label.to_string();
    }
    let pass = segments
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, seg)| pass_number(seg).map(|n| (i, n)));
    let with_pass_marker = |drop: usize| -> Vec<String> {
        let mut kept: Vec<String> = segments[drop..].iter().map(|s| s.to_string()).collect();
        if let Some((idx, n)) = pass {
            if idx < drop {
                kept.insert(0, format!("pass {n}"));
            }
        }
        kept
    };
    for drop in 1..segments.len() {
        let candidate = format!("… › {}", with_pass_marker(drop).join(" › "));
        if candidate.chars().count() <= max_width {
            return candidate;
        }
    }
    format!("… › {}", with_pass_marker(segments.len() - 1).join(" › "))
}

/// How many equal clock-time buckets the "tokens over time" chart
/// spans the run in (DESIGN.md: "a stacked area chart ... on a clock
/// axis from the run's start to now").
pub const TOKENS_OVER_TIME_BUCKETS: usize = 24;

/// Whether `meta` belongs to an agent session (issue #641): the same
/// `provider` rule the token bar's own "agents in flight" count uses
/// (`render::build_token_bar`) -- unlike `meta.live`, `provider`
/// persists once the session has actually finished, so this still
/// reads `true` for a long-finished one.
fn is_agent_session(meta: &Value) -> bool {
    meta.get("provider")
        .and_then(Value::as_str)
        .is_some_and(|p| p.trim().eq_ignore_ascii_case("agent"))
}

/// Tokens (sent + received) completed in each bucket, one series per
/// top-level phase (DESIGN.md: "one band per top-level step"),
/// `phases` in the order they should stack/display. `phases` names
/// each top-level step by its own dotted path and the label to show
/// for it (the already breadcrumb-folded label the phase strip uses,
/// not reimplemented here).
///
/// An ordinary effect's tokens are counted in full when it completes
/// (`completed_at`), the same rule the overview's own sparkline
/// (`screens::build_tokens_over_time`) uses. An agent session (issue
/// #641) is different: while osi actually watches it run,
/// `model.agent_token_observations` has a sample for every distinct
/// `meta.live` reading it saw, each at the instant osi observed it
/// (not evenly, and not all dumped into one bucket at the session's
/// own end) -- each sample's own increase over the one before it is
/// counted at its own timestamp, except the very first sample of a
/// watch that started partway through the session, which is itself
/// spread evenly from the session's own `created_at` rather than
/// dumped whole at the moment osi happened to start watching. Once
/// the session completes, any shortfall between what was actually
/// observed and the session's own final, authoritative token count
/// (`node_tokens`) is counted at its own `completed_at`, so the
/// chart's own total for the step always ends up exactly right --
/// `model` never lets an overshoot happen in the first place, by
/// dropping an unnamed loop's earlier pass's own samples for a path
/// the moment a later pass resets that path's running totals back to
/// zero (`RunModel::record_live_token_observations`). A session
/// never observed running at all (opened already finished, or
/// watched only after it ended, so `model` has no samples for it)
/// instead spreads its own tokens evenly over its own span,
/// `created_at` to `completed_at` -- or, for an interrupted session's
/// leftover `meta.live` with no `completed_at` of its own, to its own
/// last heartbeat (`live.updated_at`) -- proportional to how much of
/// each chart bucket that span actually overlaps.
pub fn tokens_over_time_by_phase(
    run_start_ts: Option<&str>,
    now: &str,
    flat: &BTreeMap<String, NodeMeta>,
    phases: &[(String, String)],
    model: &crate::model::RunModel,
) -> Vec<(String, Vec<u64>)> {
    let mut series: Vec<(String, Vec<u64>)> = phases
        .iter()
        .map(|(_, label)| (label.clone(), vec![0u64; TOKENS_OVER_TIME_BUCKETS]))
        .collect();
    let Some(start) = run_start_ts.and_then(crate::diff::parse_instant) else {
        return series;
    };
    let Some(end) = crate::diff::parse_instant(now) else {
        return series;
    };
    let span_ms = (end - start).num_milliseconds().max(1) as f64;
    let bucket_for = |instant: chrono::DateTime<chrono::FixedOffset>| -> usize {
        let frac = ((instant - start).num_milliseconds().max(0) as f64 / span_ms).min(0.999_999);
        ((frac * TOKENS_OVER_TIME_BUCKETS as f64) as usize).min(TOKENS_OVER_TIME_BUCKETS - 1)
    };
    for (path, node) in flat {
        let Some(phase_idx) = phases
            .iter()
            .position(|(p, _)| path == p || path.starts_with(&format!("{p}.")))
        else {
            continue;
        };
        if !is_agent_session(&node.meta) {
            let Some(completed) = node
                .completed_at
                .as_deref()
                .and_then(crate::diff::parse_instant)
            else {
                continue;
            };
            let total = {
                let (sent, received) = node_tokens(&node.meta);
                sent + received
            };
            if total > 0 {
                series[phase_idx].1[bucket_for(completed)] += total;
            }
            continue;
        }
        let observations = model.agent_token_observations(path);
        if observations.is_empty() {
            // An interrupted session's leftover `meta.live` has no
            // `completed_at` of its own -- its own last heartbeat
            // (`live.updated_at`) stands in for one, and its own
            // running totals stand in for `node_tokens` (which stays
            // at zero until a node actually finishes).
            let (completed_at, total) = if let Some(completed) = node.completed_at.as_deref() {
                let (sent, received) = node_tokens(&node.meta);
                (Some(completed), sent + received)
            } else if let Some(live) = &node.live {
                (
                    live.updated_at.as_deref(),
                    live.tokens_sent.unwrap_or(0) + live.tokens_received.unwrap_or(0),
                )
            } else {
                (None, 0)
            };
            if let (Some(created), Some(completed)) = (&node.created_at, completed_at) {
                if total > 0 {
                    spread_evenly(
                        &mut series[phase_idx].1,
                        created,
                        completed,
                        total,
                        start,
                        span_ms,
                        &bucket_for,
                    );
                }
            }
            continue;
        }
        let mut observed_total = 0u64;
        let mut prev = (0u64, 0u64);
        for (i, (ts, sent, received)) in observations.iter().enumerate() {
            let delta = sent.saturating_sub(prev.0) + received.saturating_sub(prev.1);
            if delta > 0 {
                if i == 0 {
                    // A watch that starts partway through a long
                    // session sees its own already-accumulated
                    // tokens on the very first observation --
                    // spreading them over the time they actually
                    // took (the session's own start to this first
                    // observation), not dumping the whole lot into
                    // one bucket at the moment osi happened to start
                    // watching.
                    if let Some(created_at) = node.created_at.as_deref() {
                        spread_evenly(
                            &mut series[phase_idx].1,
                            created_at,
                            ts,
                            delta,
                            start,
                            span_ms,
                            &bucket_for,
                        );
                    } else if let Some(instant) = crate::diff::parse_instant(ts) {
                        series[phase_idx].1[bucket_for(instant)] += delta;
                    }
                } else if let Some(instant) = crate::diff::parse_instant(ts) {
                    series[phase_idx].1[bucket_for(instant)] += delta;
                }
                observed_total += delta;
            }
            prev = (*sent, *received);
        }
        if let Some(completed) = node
            .completed_at
            .as_deref()
            .and_then(crate::diff::parse_instant)
        {
            let final_total = {
                let (sent, received) = node_tokens(&node.meta);
                sent + received
            };
            let shortfall = final_total.saturating_sub(observed_total);
            if shortfall > 0 {
                series[phase_idx].1[bucket_for(completed)] += shortfall;
            }
        }
    }
    series
}

/// Distributes `total` across `buckets`, proportional to how much of
/// each one's own clock-time window the `[created_at, completed_at]`
/// span actually overlaps (issue #641) -- a zero-duration span (both
/// parse to the same instant, or either fails to parse) puts the
/// whole total in `completed_at`'s own bucket instead, same as an
/// ordinary effect. Rounding's own leftover goes to the last bucket
/// the span actually touched, so the sum added across every bucket
/// always equals `total` exactly.
#[allow(clippy::too_many_arguments)]
fn spread_evenly(
    buckets: &mut [u64],
    created_at: &str,
    completed_at: &str,
    total: u64,
    run_start: chrono::DateTime<chrono::FixedOffset>,
    run_span_ms: f64,
    bucket_for: &impl Fn(chrono::DateTime<chrono::FixedOffset>) -> usize,
) {
    let parsed =
        crate::diff::parse_instant(created_at).zip(crate::diff::parse_instant(completed_at));
    let Some((node_start, node_end)) = parsed else {
        return;
    };
    if node_end <= node_start {
        buckets[bucket_for(node_end)] += total;
        return;
    }
    let node_ms = (node_end - node_start).num_milliseconds() as f64;
    let bucket_ms = run_span_ms / TOKENS_OVER_TIME_BUCKETS as f64;
    let mut distributed = 0u64;
    let mut last_touched = None;
    for (b, slot) in buckets.iter_mut().enumerate() {
        let bucket_start =
            run_start + chrono::Duration::milliseconds((b as f64 * bucket_ms) as i64);
        let bucket_end =
            run_start + chrono::Duration::milliseconds(((b + 1) as f64 * bucket_ms) as i64);
        let overlap_start = node_start.max(bucket_start);
        let overlap_end = node_end.min(bucket_end);
        if overlap_end <= overlap_start {
            continue;
        }
        let overlap_ms = (overlap_end - overlap_start).num_milliseconds() as f64;
        // Floored, never rounded: rounding several shares up could
        // overshoot `total` before the remainder is even added back,
        // which flooring can never do.
        let share = (total as f64 * overlap_ms / node_ms).floor() as u64;
        *slot += share;
        distributed += share;
        last_touched = Some(b);
    }
    let remainder = total.saturating_sub(distributed);
    if remainder > 0 {
        let idx = last_touched.unwrap_or_else(|| bucket_for(node_end));
        buckets[idx] += remainder;
    }
}

/// Caps [`tokens_over_time_by_phase`]'s own series to `max_bands`
/// distinct ones (issue #641): a front end has only so many glyphs
/// to tell bands apart with colour off, and with more real phases
/// than that, giving every one of them its own band means two
/// different phases eventually share a glyph in the legend --
/// impossible to tell apart. Keeps the `max_bands - 1` phases with
/// the largest total (summed across every bucket) in their original
/// relative order, and folds everything else into one further
/// `other` series (bucket-wise summed), appended last. A no-op once
/// there's already room for every phase (`series.len() <=
/// max_bands`), or with no room for any real phase at all
/// (`max_bands == 0`).
pub fn cap_phase_bands(
    series: Vec<(String, Vec<u64>)>,
    max_bands: usize,
) -> Vec<(String, Vec<u64>)> {
    if series.len() <= max_bands || max_bands == 0 {
        return series;
    }
    let keep_count = max_bands - 1;
    let mut by_total: Vec<(usize, u64)> = series
        .iter()
        .enumerate()
        .map(|(i, (_, buckets))| (i, buckets.iter().sum()))
        .collect();
    // Largest total first; ties keep the original (document) order,
    // never a `BTreeSet`'s own path order, which could keep a
    // smaller-but-later-sorted phase over an earlier, equal one.
    by_total.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let keep: std::collections::BTreeSet<usize> = by_total
        .into_iter()
        .take(keep_count)
        .map(|(i, _)| i)
        .collect();
    let bucket_count = series.first().map_or(0, |(_, b)| b.len());
    let mut other = vec![0u64; bucket_count];
    let mut kept = Vec::new();
    for (i, (label, buckets)) in series.into_iter().enumerate() {
        if keep.contains(&i) {
            kept.push((label, buckets));
        } else {
            for (sum, value) in other.iter_mut().zip(buckets.iter()) {
                *sum += value;
            }
        }
    }
    kept.push(("other".to_string(), other));
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::flatten_state;
    use serde_json::json;

    fn prompt(sent: u64, received: u64, cost: Option<f64>, completed_at: &str) -> Value {
        let mut meta = json!({
            "created_at": "t0",
            "completed_at": completed_at,
            "adapter": "scripted",
            "tokens_sent_total": sent,
            "tokens_received_total": received,
        });
        if let Some(cost) = cost {
            meta["cost_usd"] = json!(cost);
        }
        json!({"value": "y", "meta": meta})
    }

    #[test]
    fn largest_stores_ranks_by_total_tokens_and_reports_a_zero_gap() {
        let state = json!({
            "prime": {"value": null, "meta": {"completed_at": "t1"},
                "plan": {"value": null, "meta": {"created_at": "t0", "completed_at": "t1"},
                    "agent": prompt(100, 50, Some(1.0), "t1")
                },
                "review": prompt(10, 5, None, "t1")
            },
            "runtime": {"last_run": {"totals": {"tokens_sent": 110, "tokens_received": 55, "cost_usd": 1.0}}}
        });
        let flat = flatten_state(&state);
        let mut statuses = BTreeMap::new();
        let kinds_map: BTreeMap<String, RowKind> = [
            ("prime.plan".to_string(), RowKind::Use),
            ("prime.plan.agent".to_string(), RowKind::Prompt),
        ]
        .into_iter()
        .collect();
        let done = crate::model::RowStatus {
            kind: StatusKind::Done,
            skip_reason: None,
            retrying: false,
        };
        statuses.insert("prime.plan".to_string(), done.clone());
        statuses.insert("prime.plan.agent".to_string(), done);
        let result = largest_stores("prime", &flat, &statuses, &kinds_map, Some(&state));
        assert_eq!(result.rows.len(), 2);
        assert_eq!(result.rows[0].path, "prime.plan.agent");
        assert_eq!(result.rows[0].label, "plan \u{203a} agent");
        assert_eq!(result.rows[1].path, "prime.review");
        assert_eq!(result.footer.run_tokens_sent, 110);
        assert_eq!(result.footer.run_tokens_received, 55);
        assert_eq!(result.footer.gap_tokens, 0);
    }

    #[test]
    fn largest_stores_reports_a_gap_when_the_run_totals_exceed_the_table() {
        // A declared-outputs `use` child's own effects count toward
        // the run's totals as they complete but never land in the
        // final state tree at all (DESIGN.md §1.2) -- the table can
        // only ever see what `flat` still has, so the footer's own
        // gap must say so rather than silently matching.
        let state = json!({
            "prime": {"value": null, "meta": {"completed_at": "t1"},
                "review": prompt(10, 5, None, "t1")
            },
            "runtime": {"last_run": {"totals": {"tokens_sent": 110, "tokens_received": 55}}}
        });
        let flat = flatten_state(&state);
        let statuses = BTreeMap::new();
        let kinds_map: BTreeMap<String, RowKind> = BTreeMap::new();
        let result = largest_stores("prime", &flat, &statuses, &kinds_map, Some(&state));
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.footer.gap_tokens, 150);
    }

    #[test]
    fn two_stores_with_the_same_key_under_different_parents_give_distinct_rows() {
        let state = json!({"prime": {"value": null, "meta": {"completed_at": "t1"},
            "reviews": {"value": null, "meta": {"completed_at": "t1"},
                "iter_0": {"value": null, "meta": {"completed_at": "t1"},
                    "review": prompt(10, 5, None, "t1"),
                    "risk": prompt(20, 5, None, "t1")
                },
                "iter_1": {"value": null, "meta": {"completed_at": "t1"},
                    "review": prompt(30, 5, None, "t1"),
                    "risk": prompt(40, 5, None, "t1")
                }
            }
        }});
        let flat = flatten_state(&state);
        let statuses = BTreeMap::new();
        let kinds_map: BTreeMap<String, RowKind> = BTreeMap::new();
        let result = largest_stores("prime", &flat, &statuses, &kinds_map, Some(&state));
        let labels: Vec<&str> = result.rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(
            labels.len(),
            labels
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            "every row's own label must be distinct: {labels:?}"
        );
        assert!(labels.contains(&"reviews › iter_0 › review"));
        assert!(labels.contains(&"reviews › iter_1 › review"));
        assert!(labels.contains(&"reviews › iter_0 › risk"));
        assert!(labels.contains(&"reviews › iter_1 › risk"));
    }

    #[test]
    fn path_breadcrumb_relative_to_strips_the_ancestor_and_joins_with_the_breadcrumb_arrow() {
        assert_eq!(
            path_breadcrumb_relative_to("prime.reviews.iter_1.review", "prime"),
            "reviews › iter_1 › review"
        );
        assert_eq!(
            path_breadcrumb_relative_to("prime.reviews.iter_1.review", "prime.reviews"),
            "iter_1 › review"
        );
        assert_eq!(
            path_breadcrumb_relative_to("prime.plan.agent", "prime"),
            "plan › agent"
        );
    }

    #[test]
    fn shorten_breadcrumb_from_left_drops_leading_segments_but_keeps_the_leaf() {
        // No `iter_N` segment here at all -- nothing to disambiguate
        // two loop passes with, so this reads exactly as before.
        let label = "reviews › check › answers";
        assert_eq!(shorten_breadcrumb_from_left(label, 100), label);
        assert_eq!(
            shorten_breadcrumb_from_left(label, "… › check › answers".chars().count()),
            "… › check › answers"
        );
        assert_eq!(
            shorten_breadcrumb_from_left(label, "… › answers".chars().count()),
            "… › answers"
        );
        // Even at an extreme width, the leaf is never dropped.
        assert_eq!(shorten_breadcrumb_from_left(label, 1), "… › answers");
        // A single segment (no ancestors) is never prefixed with `…` --
        // there is nothing to drop without losing the leaf itself.
        assert_eq!(shorten_breadcrumb_from_left("answers", 1), "answers");
    }

    /// Issue #641: two loop passes whose own breadcrumb agrees on
    /// everything below their own `iter_N` (`reviews › iter_0 ›
    /// check › answers`, `reviews › iter_1 › check › answers`) must
    /// not shorten to the exact same text -- the nearest `iter_N` is
    /// kept, as `pass N`, even once the plain path segment itself
    /// drops away.
    #[test]
    fn shorten_breadcrumb_from_left_keeps_the_pass_that_tells_two_loops_apart() {
        let first = "reviews › iter_0 › check › answers";
        let second = "reviews › iter_1 › check › answers";
        // Narrow enough that even the real `iter_N` segment itself
        // has to drop -- only the leaf fits otherwise.
        let narrow = "… › answers".chars().count();
        let shortened_first = shorten_breadcrumb_from_left(first, narrow);
        let shortened_second = shorten_breadcrumb_from_left(second, narrow);
        assert_ne!(
            shortened_first, shortened_second,
            "two different passes must not read identically once shortened"
        );
        assert_eq!(shortened_first, "… › pass 1 › answers");
        assert_eq!(shortened_second, "… › pass 2 › answers");
    }

    /// At a width that still fits the real `iter_N` segment itself,
    /// shortening reads exactly as it did before `pass N` existed --
    /// the marker only appears once the real segment would otherwise
    /// have dropped away.
    #[test]
    fn shorten_breadcrumb_from_left_keeps_the_real_iter_segment_when_it_still_fits() {
        let label = "reviews › iter_1 › review";
        assert_eq!(
            shorten_breadcrumb_from_left(label, "… › iter_1 › review".chars().count()),
            "… › iter_1 › review"
        );
    }

    #[test]
    fn a_tie_in_total_tokens_picks_the_lowest_path() {
        // Two stores with the identical total (150) -- the table's
        // own sort (`.then_with(|| a.path.cmp(&b.path))`) breaks the
        // tie by path, not merely by whichever `BTreeMap` iteration
        // happened to visit last.
        let state = json!({"prime": {"value": null, "meta": {"completed_at": "t1"},
            "beta": prompt(100, 50, None, "t1"),
            "alpha": prompt(100, 50, None, "t1"),
        }});
        let flat = flatten_state(&state);
        let statuses = BTreeMap::new();
        let kinds_map: BTreeMap<String, RowKind> = BTreeMap::new();
        let table = largest_stores("prime", &flat, &statuses, &kinds_map, Some(&state));
        assert_eq!(
            table.rows[0].path, "prime.alpha",
            "the lowest path wins the tie"
        );
    }

    #[test]
    fn tokens_over_time_by_phase_buckets_each_phase_separately() {
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "plan": prompt(100, 0, None, "2026-01-01T00:00:00Z"),
            "review": prompt(0, 40, None, "2026-01-01T00:00:55Z"),
        }});
        let flat = flatten_state(&state);
        let phases = vec![
            ("prime.plan".to_string(), "plan".to_string()),
            ("prime.review".to_string(), "review".to_string()),
        ];
        let model = crate::model::RunModel::new();
        let series = tokens_over_time_by_phase(
            Some("2026-01-01T00:00:00Z"),
            "2026-01-01T00:01:00Z",
            &flat,
            &phases,
            &model,
        );
        assert_eq!(series.len(), 2);
        let (plan_label, plan_buckets) = &series[0];
        let (review_label, review_buckets) = &series[1];
        assert_eq!(plan_label, "plan");
        assert_eq!(review_label, "review");
        assert_eq!(plan_buckets.iter().sum::<u64>(), 100);
        assert_eq!(review_buckets.iter().sum::<u64>(), 40);
        // `plan` completed at the very start of the run -- its tokens
        // land in the first bucket, not spread across every one.
        assert_eq!(plan_buckets[0], 100);
    }

    /// Issue #641, first case: while osi watches an agent session
    /// run, each observed increase in its own running tokens lands
    /// in the bucket for the moment it was actually observed, not
    /// all dumped into one bucket at the session's own end -- except
    /// the very first observation, which is itself spread from the
    /// session's own `created_at` (the watch here starts two minutes
    /// into the session, so those first 30 tokens took those two
    /// minutes, not the single instant they were observed at) -- and
    /// any difference between what was observed and the session's
    /// own final, authoritative total is reconciled into the bucket
    /// at its own `completed_at`.
    #[test]
    fn tokens_over_time_by_phase_buckets_a_watched_agent_session_by_observed_instant() {
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "agent1": {"value": "done", "meta": {
                "created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:10:00Z",
                "provider": "agent", "tokens_sent_total": 100, "tokens_received_total": 0
            }}
        }});
        let flat = flatten_state(&state);
        let phases = vec![("prime.agent1".to_string(), "agent1".to_string())];
        let mut model = crate::model::RunModel::new();
        model.record_live_token_observations(
            &flatten_state(
                &json!({"prime": {"value": null, "meta": {"completed_at": null},
                    "agent1": {"value": null, "meta": {
                        "created_at": "2026-01-01T00:00:00Z", "completed_at": null,
                        "live": {"tokens_sent": 30, "tokens_received": 0}
                    }}
                }}),
            ),
            "2026-01-01T00:02:00Z",
            false,
        );
        model.record_live_token_observations(
            &flatten_state(
                &json!({"prime": {"value": null, "meta": {"completed_at": null},
                    "agent1": {"value": null, "meta": {
                        "created_at": "2026-01-01T00:00:00Z", "completed_at": null,
                        "live": {"tokens_sent": 70, "tokens_received": 0}
                    }}
                }}),
            ),
            "2026-01-01T00:05:00Z",
            false,
        );
        let series = tokens_over_time_by_phase(
            Some("2026-01-01T00:00:00Z"),
            "2026-01-01T00:24:00Z",
            &flat,
            &phases,
            &model,
        );
        let (_, buckets) = &series[0];
        // The first 30 tokens, observed whole at minute 2, spread
        // evenly back across minutes 0 and 1 instead; 40 more (70
        // total) observed at minute 5 land there in full, since that
        // is a real subsequent observation, not the first one; then
        // the final 30 (100 total, the state's own authoritative
        // figure) is reconciled at completion, minute 10.
        assert_eq!(buckets[0], 15);
        assert_eq!(buckets[1], 15);
        assert_eq!(buckets[2], 0);
        assert_eq!(buckets[5], 40);
        assert_eq!(buckets[10], 30);
        assert_eq!(buckets.iter().sum::<u64>(), 100);
    }

    /// Issue #641: an unnamed loop rerunning the same agent path
    /// starts that path's own running totals back at zero on each
    /// pass -- the chart's total for the step must still equal only
    /// what the final state actually remembers (the last pass's own
    /// tokens), not every pass's tokens added together.
    #[test]
    fn tokens_over_time_by_phase_does_not_overshoot_on_a_rerun_path() {
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "loop": {"value": null, "meta": {"completed_at": null},
                "agent": {"value": "done", "meta": {
                    "created_at": "2026-01-01T00:05:00Z", "completed_at": "2026-01-01T00:08:00Z",
                    "provider": "agent", "tokens_sent_total": 20, "tokens_received_total": 0
                }}
            }
        }});
        let flat = flatten_state(&state);
        let phases = vec![("prime.loop".to_string(), "loop".to_string())];
        let mut model = crate::model::RunModel::new();
        // First pass through the loop, watched to its own end: 10
        // tokens, which the rerun below must not add on top of.
        model.record_live_token_observations(
            &flatten_state(&json!({"prime": {"value": null, "meta": {"completed_at": null},
                "loop": {"value": null, "meta": {"completed_at": null},
                    "agent": {"value": null, "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": null,
                        "live": {"tokens_sent": 10, "tokens_received": 0}}}
                }
            }})),
            "2026-01-01T00:02:00Z",
            false,
        );
        // Second pass reruns the same path, resetting its own
        // running totals back to zero before growing to the 20 the
        // final state alone remembers.
        model.record_live_token_observations(
            &flatten_state(&json!({"prime": {"value": null, "meta": {"completed_at": null},
                "loop": {"value": null, "meta": {"completed_at": null},
                    "agent": {"value": null, "meta": {"created_at": "2026-01-01T00:05:00Z", "completed_at": null,
                        "live": {"tokens_sent": 0, "tokens_received": 0}}}
                }
            }})),
            "2026-01-01T00:05:30Z",
            false,
        );
        model.record_live_token_observations(
            &flatten_state(&json!({"prime": {"value": null, "meta": {"completed_at": null},
                "loop": {"value": null, "meta": {"completed_at": null},
                    "agent": {"value": null, "meta": {"created_at": "2026-01-01T00:05:00Z", "completed_at": null,
                        "live": {"tokens_sent": 20, "tokens_received": 0}}}
                }
            }})),
            "2026-01-01T00:07:00Z",
            false,
        );
        let series = tokens_over_time_by_phase(
            Some("2026-01-01T00:00:00Z"),
            "2026-01-01T00:24:00Z",
            &flat,
            &phases,
            &model,
        );
        let (_, buckets) = &series[0];
        assert_eq!(buckets[7], 20);
        assert_eq!(
            buckets.iter().sum::<u64>(),
            20,
            "the first pass's own 10 tokens must not survive the rerun"
        );
    }

    /// Issue #641: an interrupted session's leftover `meta.live`,
    /// first seen only once the run itself has already ended (osi
    /// opened fresh afterwards), must spread over its own span --
    /// `created_at` to its own last heartbeat -- the same as any
    /// other session never observed running, not land as a single
    /// observation at whatever `now` happens to be when osi opens it.
    #[test]
    fn tokens_over_time_by_phase_spreads_an_interrupted_sessions_leftover_live() {
        let state = json!({"prime": {"value": null, "meta": {
                "completed_at": "2026-01-01T00:04:30Z", "error": "Interrupted (Ctrl-C/SIGINT)"
            },
            "agent1": {"value": null, "meta": {
                "created_at": "2026-01-01T00:00:00Z", "completed_at": null, "provider": "agent",
                "live": {
                    "updated_at": "2026-01-01T00:04:00Z", "tokens_sent": 40, "tokens_received": 0
                }
            }}
        }});
        let flat = flatten_state(&state);
        let phases = vec![("prime.agent1".to_string(), "agent1".to_string())];
        let mut model = crate::model::RunModel::new();
        // A fresh model, as if osi had just opened this already-dead
        // run: this path's live summary is seen for the very first
        // time only once the run is already over.
        model.record_live_token_observations(&flat, "2026-03-01T00:00:00Z", true);
        assert!(model.agent_token_observations("prime.agent1").is_empty());
        let series = tokens_over_time_by_phase(
            Some("2026-01-01T00:00:00Z"),
            "2026-01-01T00:24:00Z",
            &flat,
            &phases,
            &model,
        );
        let (_, buckets) = &series[0];
        // A 4-minute session spread evenly, not dumped into one
        // bucket at some unrelated `now` from opening history long
        // after the run ended.
        assert_eq!(&buckets[0..4], &[10, 10, 10, 10]);
        assert_eq!(buckets.iter().sum::<u64>(), 40);
    }

    /// Issue #641, second case: an agent session never observed
    /// running at all (no recorded samples, say a finished run
    /// opened fresh) spreads its own tokens evenly over its own
    /// span instead of landing them all in one bucket at its own
    /// end.
    #[test]
    fn tokens_over_time_by_phase_spreads_an_unwatched_agent_session_evenly() {
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "agent2": {"value": "done", "meta": {
                "created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:04:00Z",
                "provider": "agent", "tokens_sent_total": 40, "tokens_received_total": 0
            }}
        }});
        let flat = flatten_state(&state);
        let phases = vec![("prime.agent2".to_string(), "agent2".to_string())];
        let model = crate::model::RunModel::new();
        let series = tokens_over_time_by_phase(
            Some("2026-01-01T00:00:00Z"),
            "2026-01-01T00:24:00Z",
            &flat,
            &phases,
            &model,
        );
        let (_, buckets) = &series[0];
        // A 4-minute session over a run bucketed one minute at a
        // time spans exactly 4 buckets -- 10 of its own 40 tokens in
        // each, not all 40 in one.
        assert_eq!(&buckets[0..4], &[10, 10, 10, 10]);
        assert_eq!(buckets.iter().sum::<u64>(), 40);
    }

    /// Issue #641: with more phases than `max_bands` ever gives a
    /// distinct glyph to, the smallest ones fold into one `other`
    /// band instead of two different phases eventually sharing a
    /// glyph in the legend.
    #[test]
    fn cap_phase_bands_keeps_the_largest_and_folds_the_rest_into_other() {
        let series = vec![
            ("a".to_string(), vec![10, 0]),
            ("b".to_string(), vec![0, 5]),
            ("c".to_string(), vec![1, 0]),
            ("d".to_string(), vec![0, 2]),
        ];
        let capped = cap_phase_bands(series, 3);
        assert_eq!(
            capped
                .iter()
                .map(|(label, _)| label.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "b", "other"]
        );
        assert_eq!(capped[2].1, vec![1, 2]); // c + d, bucket-wise
    }

    #[test]
    fn cap_phase_bands_is_a_no_op_with_room_for_every_phase() {
        let series = vec![("a".to_string(), vec![10]), ("b".to_string(), vec![5])];
        let capped = cap_phase_bands(series.clone(), 6);
        assert_eq!(capped, series);
    }

    #[test]
    fn cap_phase_bands_breaks_a_tie_by_the_original_order() {
        let series = vec![
            ("a".to_string(), vec![5]),
            ("b".to_string(), vec![5]),
            ("c".to_string(), vec![5]),
        ];
        let capped = cap_phase_bands(series, 2);
        assert_eq!(
            capped
                .iter()
                .map(|(label, _)| label.as_str())
                .collect::<Vec<_>>(),
            vec!["a", "other"]
        );
    }

    #[test]
    fn tokens_over_time_by_phase_is_empty_before_the_run_has_a_start_time() {
        let flat: BTreeMap<String, NodeMeta> = BTreeMap::new();
        let phases = vec![("prime.plan".to_string(), "plan".to_string())];
        let model = crate::model::RunModel::new();
        let series =
            tokens_over_time_by_phase(None, "2026-01-01T00:01:00Z", &flat, &phases, &model);
        assert_eq!(series.len(), 1);
        assert!(series[0].1.iter().all(|&v| v == 0));
    }

    /// Both engines' own real final state (not hand-written, see
    /// `model::tests::subtree_tokens_matches_run_totals_for_a_
    /// retried_prompt_on_cof`/`_on_electricity`) for a retried
    /// `prompt_type: json` prompt: the winning attempt alone
    /// (`tokens_sent: 11`/`tokens_received: 22`) does not match
    /// `runtime.last_run.totals`, only the `_total` pair the retry
    /// also accumulated into does -- the table's own footer must
    /// agree, not merely the per-node token rule.
    fn retried_prompt_state() -> Value {
        json!({
            "runtime": {"last_run": {"totals": {"tokens_sent": 16, "tokens_received": 29}}},
            "prime": {"value": true, "meta": {
                "completed_at": "t1", "error": null, "tokens_sent": null, "tokens_received": null
            },
            "ask": {"value": {"ans": "ok"}, "meta": {
                "created_at": "t0", "completed_at": "t1", "error": null,
                "prompt_type": "json",
                "retries_used": 1,
                "tokens_sent": 11, "tokens_received": 22,
                "tokens_sent_total": 16, "tokens_received_total": 29,
                "fallback_attempts": [
                    {"adapter": "scripted", "model": "scripted-model", "status": "succeeded",
                     "error": null, "tokens_sent": 11, "tokens_received": 22}
                ]
            }}}
        })
    }

    #[test]
    fn the_cost_label_marks_itself_partial_when_not_every_store_reported_one() {
        let state = json!({
            "prime": {"value": null, "meta": {"completed_at": "t1"},
                "plan": prompt(100, 50, Some(1.5), "t1"),
                "review": prompt(10, 5, Some(1.2), "t1"),
                "polish": prompt(10, 5, None, "t1")
            },
            "runtime": {"last_run": {"totals": {"tokens_sent": 120, "tokens_received": 60, "cost_usd": 2.7}}}
        });
        let flat = flatten_state(&state);
        let statuses = BTreeMap::new();
        let kinds_map: BTreeMap<String, RowKind> = BTreeMap::new();
        let result = largest_stores("prime", &flat, &statuses, &kinds_map, Some(&state));
        assert_eq!(result.rows.len(), 3);
        assert_eq!(result.footer.cost_reporting, 2);
        assert_eq!(result.footer.cost_stores, 3);
        assert_eq!(
            result.footer.cost_label(),
            Some("$2.70 (from 2 of 3 prompts)".to_string())
        );
    }

    #[test]
    fn cost_label_for_width_returns_the_full_label_when_it_fits() {
        let footer = StoresFooter {
            run_tokens_sent: 1300,
            run_tokens_received: 600,
            run_cost_usd: Some(1.2),
            gap_tokens: 0,
            cost_reporting: 1,
            cost_stores: 4,
        };
        let full = "$1.20 (from 1 of 4 prompts)";
        assert_eq!(footer.cost_label().as_deref(), Some(full));
        // 44 columns: the table pane's own interior width at 120x40
        // (issue #632's small-terminal acceptance item).
        assert_eq!(footer.cost_label_for_width(44).as_deref(), Some(full));
        // Exactly the label's own length: still the full label, not
        // shortened one character early.
        assert_eq!(
            footer.cost_label_for_width(full.chars().count()).as_deref(),
            Some(full)
        );
    }

    #[test]
    fn cost_label_for_width_drops_the_dollar_figure_before_the_coverage_note() {
        let footer = StoresFooter {
            run_tokens_sent: 1300,
            run_tokens_received: 600,
            run_cost_usd: Some(1.2),
            gap_tokens: 0,
            cost_reporting: 1,
            cost_stores: 4,
        };
        assert_eq!(
            footer
                .cost_label_for_width("(from 1 of 4 prompts)".chars().count())
                .as_deref(),
            Some("(from 1 of 4 prompts)")
        );
    }

    #[test]
    fn cost_label_for_width_keeps_n_of_m_even_at_an_extreme_width() {
        let footer = StoresFooter {
            run_tokens_sent: 1300,
            run_tokens_received: 600,
            run_cost_usd: Some(1.2),
            gap_tokens: 0,
            cost_reporting: 1,
            cost_stores: 4,
        };
        let label = footer.cost_label_for_width(1).expect("still Some");
        assert!(label.contains("1 of 4"), "label was {label:?}");
    }

    #[test]
    fn cost_label_for_width_never_shortens_a_non_partial_label() {
        let footer = StoresFooter {
            run_tokens_sent: 100,
            run_tokens_received: 50,
            run_cost_usd: Some(1.5),
            gap_tokens: 0,
            cost_reporting: 1,
            cost_stores: 1,
        };
        assert_eq!(footer.cost_label_for_width(1).as_deref(), Some("$1.50"));
    }

    #[test]
    fn a_running_prompt_is_not_counted_against_the_cost_labels_own_coverage() {
        let state = json!({
            "prime": {"value": null, "meta": {"completed_at": null},
                "plan": prompt(100, 50, Some(1.5), "t1"),
                "review": {"value": "y", "meta": {
                    "created_at": "t0", "completed_at": null, "adapter": "scripted"
                }}
            },
            "runtime": {"last_run": {"totals": {"tokens_sent": 100, "tokens_received": 50, "cost_usd": 1.5}}}
        });
        let flat = flatten_state(&state);
        let mut statuses = BTreeMap::new();
        statuses.insert(
            "prime.review".to_string(),
            crate::model::RowStatus {
                kind: StatusKind::Running,
                skip_reason: None,
                retrying: false,
            },
        );
        let kinds_map: BTreeMap<String, RowKind> = BTreeMap::new();
        let result = largest_stores("prime", &flat, &statuses, &kinds_map, Some(&state));
        assert_eq!(result.rows.len(), 2, "the running store still gets a row");
        assert_eq!(
            result.footer.cost_stores, 1,
            "the running store is not counted, finished or not"
        );
        assert_eq!(result.footer.cost_reporting, 1);
        assert_eq!(result.footer.cost_label(), Some("$1.50".to_string()));
    }

    #[test]
    fn the_cost_label_has_no_coverage_note_once_every_store_reported_one() {
        let state = json!({
            "prime": {"value": null, "meta": {"completed_at": "t1"},
                "plan": prompt(100, 50, Some(1.5), "t1")
            },
            "runtime": {"last_run": {"totals": {"tokens_sent": 100, "tokens_received": 50, "cost_usd": 1.5}}}
        });
        let flat = flatten_state(&state);
        let statuses = BTreeMap::new();
        let kinds_map: BTreeMap<String, RowKind> = BTreeMap::new();
        let result = largest_stores("prime", &flat, &statuses, &kinds_map, Some(&state));
        assert_eq!(result.footer.cost_label(), Some("$1.50".to_string()));
    }

    #[test]
    fn the_cost_label_is_none_before_any_store_has_reported_one() {
        let state = json!({
            "prime": {"value": null, "meta": {"completed_at": "t1"},
                "plan": prompt(100, 50, None, "t1")
            }
        });
        let flat = flatten_state(&state);
        let statuses = BTreeMap::new();
        let kinds_map: BTreeMap<String, RowKind> = BTreeMap::new();
        let result = largest_stores("prime", &flat, &statuses, &kinds_map, Some(&state));
        assert_eq!(result.footer.cost_label(), None);
    }

    #[test]
    fn the_stores_totals_equal_the_runs_totals_for_a_retried_prompt_on_cof_and_electricity() {
        // cof and electricity wrote byte-identical `_total` fields and
        // `runtime.last_run.totals` for this scenario (confirmed by
        // running both with the scripted adapter) -- one fixture
        // covers both engines.
        let state = retried_prompt_state();
        let flat = flatten_state(&state);
        let statuses = BTreeMap::new();
        let kinds_map: BTreeMap<String, RowKind> = BTreeMap::new();
        let result = largest_stores("prime", &flat, &statuses, &kinds_map, Some(&state));
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0].tokens_sent, 16);
        assert_eq!(result.rows[0].tokens_received, 29);
        assert_eq!(result.footer.run_tokens_sent, 16);
        assert_eq!(result.footer.run_tokens_received, 29);
        assert_eq!(
            result.footer.gap_tokens, 0,
            "the table's own total must equal the run's, using the _total pair, not the winning attempt alone"
        );
    }
}
