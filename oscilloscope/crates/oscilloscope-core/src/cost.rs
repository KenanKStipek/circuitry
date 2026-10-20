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
            // itself uses, so the footer's own "run totals" agree
            // with the header's `↑`/`↓` the whole time, not only
            // once the run has ended.
            let (sent, received, cost) =
                flat.values().fold((0u64, 0u64, 0.0f64), |(s, r, c), node| {
                    let (ns, nr) = node_tokens(&node.meta);
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
                // its live `tokens_sent` too, the same figure its
                // treemap tile already does (`treemap::leaf_node_for_
                // measure`) -- without this the table kept reading
                // `0 t`/`tokens when done` for exactly the tile that
                // no longer does.
                let tokens_sent = if n.completed_at.is_none() {
                    n.live
                        .as_ref()
                        .and_then(|live| live.tokens_sent)
                        .map_or(tokens_sent, |live_sent| tokens_sent.max(live_sent))
                } else {
                    tokens_sent
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

/// Shortens a " › "-joined breadcrumb to at most `max_width` columns
/// by dropping whole leading segments and prefixing `…`, never the
/// trailing (leaf) segment -- the one part of a row's own path that
/// actually has to stay readable to tell it apart from a same-named
/// sibling elsewhere in the tree. Returns `label` unchanged if it
/// already fits, or has only one segment to begin with (nothing to
/// drop without losing the leaf itself).
pub fn shorten_breadcrumb_from_left(label: &str, max_width: usize) -> String {
    if label.chars().count() <= max_width {
        return label.to_string();
    }
    let segments: Vec<&str> = label.split(" › ").collect();
    if segments.len() <= 1 {
        return label.to_string();
    }
    for drop in 1..segments.len() {
        let candidate = format!("… › {}", segments[drop..].join(" › "));
        if candidate.chars().count() <= max_width {
            return candidate;
        }
    }
    format!("… › {}", segments[segments.len() - 1])
}

/// The overview's own cost card (DESIGN.md: "the overview's cost card
/// stays as a summary that agrees with the Cost screen's ranking") --
/// the single largest store by total tokens, read straight from
/// `flat` without building the by-store tree at all (the performance
/// rule: the tree is built only while the Cost screen is actually on
/// display). Every `flat` entry with nonzero tokens of its own is a
/// store the table also surfaces as a leaf row, whether it is a true
/// leaf or a `while: mode: model` loop reporting its own tokens
/// alongside its children's (DESIGN.md's gap note) -- [`largest_
/// stores`]' own by-store tree gives such a container a leaf tile of
/// its own for exactly this reason. Ties break the same way the
/// table's own sort does, lowest path first (`largest_stores`' own
/// `rows.sort_by`), not merely "the last one found" -- `BTreeMap`'s
/// ascending iteration order makes those two tie-break rules the
/// same rule only if this reads it that way explicitly. `root_path`
/// is excluded the same way [`build_tree_for_measure`] excludes it
/// from the Cost screen's own top-level tiles -- the run's own root
/// carries an `adapter`-bearing meta like any other node and, when it
/// reports tokens of its own, must not surface as "the largest store"
/// in its own run.
pub fn top_store(flat: &BTreeMap<String, NodeMeta>, root_path: &str) -> Option<(String, u64)> {
    flat.iter()
        .filter(|(path, _)| path.as_str() != root_path)
        .filter_map(|(path, node)| {
            let (sent, received) = node_tokens(&node.meta);
            let total = sent + received;
            (total > 0).then(|| (path.clone(), total))
        })
        .fold(
            None,
            |best: Option<(String, u64)>, (path, total)| match &best {
                Some((best_path, best_total))
                    if *best_total > total || (*best_total == total && best_path <= &path) =>
                {
                    best
                }
                _ => Some((path, total)),
            },
        )
}

/// How many equal clock-time buckets the "tokens over time" chart
/// spans the run in (DESIGN.md: "a stacked area chart ... on a clock
/// axis from the run's start to now").
pub const TOKENS_OVER_TIME_BUCKETS: usize = 24;

/// Tokens (sent + received) completed in each bucket, one series per
/// top-level phase (DESIGN.md: "one band per top-level step"),
/// `phases` in the order they should stack/display. `phases` names
/// each top-level step by its own dotted path and the label to show
/// for it (the already breadcrumb-folded label the phase strip uses,
/// not reimplemented here). Tokens are counted when each effect
/// completes (`completed_at`), the same rule the overview's own
/// sparkline (`screens::build_tokens_over_time`) uses, just bucketed
/// per phase instead of summed across the whole run.
pub fn tokens_over_time_by_phase(
    run_start_ts: Option<&str>,
    now: &str,
    flat: &BTreeMap<String, NodeMeta>,
    phases: &[(String, String)],
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
    for (path, node) in flat {
        let Some(completed) = node
            .completed_at
            .as_deref()
            .and_then(crate::diff::parse_instant)
        else {
            continue;
        };
        let (sent, received) = node_tokens(&node.meta);
        let total = sent + received;
        if total == 0 {
            continue;
        }
        let Some(phase_idx) = phases
            .iter()
            .position(|(p, _)| path == p || path.starts_with(&format!("{p}.")))
        else {
            continue;
        };
        let frac = ((completed - start).num_milliseconds().max(0) as f64 / span_ms).min(0.999_999);
        let bucket =
            ((frac * TOKENS_OVER_TIME_BUCKETS as f64) as usize).min(TOKENS_OVER_TIME_BUCKETS - 1);
        series[phase_idx].1[bucket] += total;
    }
    series
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
    fn top_store_matches_the_largest_stores_tables_own_first_row() {
        let state = json!({"prime": {"value": null, "meta": {"completed_at": "t1"},
            "plan": prompt(100, 50, None, "t1"),
            "review": prompt(10, 5, None, "t1"),
        }});
        let flat = flatten_state(&state);
        let statuses = BTreeMap::new();
        let kinds_map: BTreeMap<String, RowKind> = BTreeMap::new();
        let table = largest_stores("prime", &flat, &statuses, &kinds_map, Some(&state));
        let top = top_store(&flat, "prime").expect("a top store");
        assert_eq!(top.0, table.rows[0].path);
        assert_eq!(
            top.1,
            table.rows[0].tokens_sent + table.rows[0].tokens_received
        );
    }

    #[test]
    fn top_store_never_picks_the_run_root_itself() {
        // The root carries its own tokens the same way a `while:
        // mode: model` loop's container does (`cost.rs`'s own gap
        // note) -- without excluding it by path the way `build_tree_
        // for_measure` excludes it from the Cost screen's own top-
        // level tiles, a root reporting more tokens than any real
        // store would win "the largest store" as itself.
        let state = json!({"prime": {"value": null, "meta": {
            "completed_at": "t1", "adapter": "scripted",
            "tokens_sent_total": 900, "tokens_received_total": 400
        },
            "plan": prompt(10, 5, None, "t1"),
        }});
        let flat = flatten_state(&state);
        let top = top_store(&flat, "prime").expect("a top store");
        assert_eq!(
            top.0, "prime.plan",
            "the root itself must never win: {top:?}"
        );
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
        let label = "reviews › iter_1 › review";
        assert_eq!(shorten_breadcrumb_from_left(label, 100), label);
        assert_eq!(
            shorten_breadcrumb_from_left(label, "… › iter_1 › review".chars().count()),
            "… › iter_1 › review"
        );
        assert_eq!(
            shorten_breadcrumb_from_left(label, "… › review".chars().count()),
            "… › review"
        );
        // Even at an extreme width, the leaf is never dropped.
        assert_eq!(shorten_breadcrumb_from_left(label, 1), "… › review");
        // A single segment (no ancestors) is never prefixed with `…` --
        // there is nothing to drop without losing the leaf itself.
        assert_eq!(shorten_breadcrumb_from_left("review", 1), "review");
    }

    #[test]
    fn a_tie_in_total_tokens_picks_the_same_store_both_ways() {
        // Two stores with the identical total (150) -- `top_store`'s
        // own tie-break (`best_path <= &path` keeps the first,
        // lowest-path winner) and the table's own sort
        // (`.then_with(|| a.path.cmp(&b.path))`) must agree on which
        // one that is, not merely agree when there happens to be a
        // single largest store.
        let state = json!({"prime": {"value": null, "meta": {"completed_at": "t1"},
            "beta": prompt(100, 50, None, "t1"),
            "alpha": prompt(100, 50, None, "t1"),
        }});
        let flat = flatten_state(&state);
        let statuses = BTreeMap::new();
        let kinds_map: BTreeMap<String, RowKind> = BTreeMap::new();
        let table = largest_stores("prime", &flat, &statuses, &kinds_map, Some(&state));
        let top = top_store(&flat, "prime").expect("a top store");
        assert_eq!(top.0, "prime.alpha", "the lowest path wins the tie");
        assert_eq!(top.0, table.rows[0].path);
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
        let series = tokens_over_time_by_phase(
            Some("2026-01-01T00:00:00Z"),
            "2026-01-01T00:01:00Z",
            &flat,
            &phases,
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

    #[test]
    fn tokens_over_time_by_phase_is_empty_before_the_run_has_a_start_time() {
        let flat: BTreeMap<String, NodeMeta> = BTreeMap::new();
        let phases = vec![("prime.plan".to_string(), "plan".to_string())];
        let series = tokens_over_time_by_phase(None, "2026-01-01T00:01:00Z", &flat, &phases);
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
