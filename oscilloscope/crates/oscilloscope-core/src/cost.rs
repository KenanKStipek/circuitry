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
use crate::treemap::{Measure, TreemapNode, build_tree_for_measure};

/// One row of the "largest stores" table (DESIGN.md: "path, tokens
/// in, tokens out, cost (when reported) and share of the total").
#[derive(Debug, Clone, PartialEq)]
pub struct StoreRow {
    /// The same chain-folded breadcrumb label the treemap's own tile
    /// shows for this store (`plan › agent`) — both read the
    /// identical by-store tree, so the table's own ranking and the
    /// treemap's own tiles can never disagree about what a store is
    /// called.
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
    state: Option<&Value>,
) -> LargestStores {
    let (tree, _excluded) = build_tree_for_measure(root_path, flat, statuses, Measure::Total);
    let mut rows = Vec::new();
    flatten_leaves_into(&tree, flat, &mut rows);
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
            // with the header's `\u2191`/`\u2193` the whole time, not only
            // once the run has ended.
            let (sent, received) = flat.values().fold((0u64, 0u64), |(s, r), node| {
                let (ns, nr) = node_tokens(&node.meta);
                (s + ns, r + nr)
            });
            (sent, received, None)
        });
    let gap_tokens = (run_sent + run_received).saturating_sub(table_tokens);

    LargestStores {
        rows,
        footer: StoresFooter {
            run_tokens_sent: run_sent,
            run_tokens_received: run_received,
            run_cost_usd,
            gap_tokens,
        },
    }
}

fn flatten_leaves_into(
    nodes: &[TreemapNode],
    flat: &BTreeMap<String, NodeMeta>,
    out: &mut Vec<StoreRow>,
) {
    for node in nodes {
        if node.children.is_empty() {
            if let Some(n) = flat.get(&node.path) {
                let (tokens_sent, tokens_received) = node_tokens(&n.meta);
                let cost_usd = n.meta.get("cost_usd").and_then(Value::as_f64);
                out.push(StoreRow {
                    label: node.key.clone(),
                    path: node.path.clone(),
                    tokens_sent,
                    tokens_received,
                    cost_usd,
                    status: node.status,
                });
            }
        } else {
            flatten_leaves_into(&node.children, flat, out);
        }
    }
}

/// The overview's own cost card (DESIGN.md: "the overview's cost card
/// stays as a summary that agrees with the Cost screen's ranking") --
/// the single largest leaf store by total tokens, read straight from
/// `flat` without building the by-store tree at all (the performance
/// rule: the tree is built only while the Cost screen is actually on
/// display). Correct by the same invariant [`largest_stores`] relies
/// on: a container's own tokens are always null (DESIGN.md §1.2), so
/// any node with nonzero tokens of its own is necessarily a leaf —
/// this never needs to check for children to confirm that.
pub fn top_store(flat: &BTreeMap<String, NodeMeta>) -> Option<(String, u64)> {
    flat.iter()
        .filter_map(|(path, node)| {
            let (sent, received) = node_tokens(&node.meta);
            let total = sent + received;
            (total > 0).then(|| (path.clone(), total))
        })
        .max_by_key(|(_, total)| *total)
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
        let done = crate::model::RowStatus {
            kind: StatusKind::Done,
            skip_reason: None,
            retrying: false,
        };
        statuses.insert("prime.plan".to_string(), done.clone());
        statuses.insert("prime.plan.agent".to_string(), done);
        let result = largest_stores("prime", &flat, &statuses, Some(&state));
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
        // final state tree at all (DESIGN.md \u00a71.2) -- the table can
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
        let result = largest_stores("prime", &flat, &statuses, Some(&state));
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
        let table = largest_stores("prime", &flat, &statuses, Some(&state));
        let top = top_store(&flat).expect("a top store");
        assert_eq!(top.0, table.rows[0].path);
        assert_eq!(
            top.1,
            table.rows[0].tokens_sent + table.rows[0].tokens_received
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
    fn the_stores_totals_equal_the_runs_totals_for_a_retried_prompt_on_cof_and_electricity() {
        // cof and electricity wrote byte-identical `_total` fields and
        // `runtime.last_run.totals` for this scenario (confirmed by
        // running both with the scripted adapter) -- one fixture
        // covers both engines.
        let state = retried_prompt_state();
        let flat = flatten_state(&state);
        let statuses = BTreeMap::new();
        let result = largest_stores("prime", &flat, &statuses, Some(&state));
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
