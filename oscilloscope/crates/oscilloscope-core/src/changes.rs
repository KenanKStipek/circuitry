//! What changed between two observations (DESIGN.md §6.3, issue
//! #535): front-end independent, so `oscilloscope`'s highlight
//! tracker never has to look at a `RenderState` itself to decide
//! what just changed. Every change is reported as a stable key —
//! a row path, a state-pane path, a treemap key path, a log line's
//! own index, or one of a handful of fixed strings for the header
//! and the token bar — so the front end only has to remember "this
//! key last changed at time T", never re-derive *why*.
//!
//! The first observation (`old: None`) never reports anything (issue
//! #535: "the first observation highlights nothing... that includes
//! `osp watch` attaching to a finished run"). Nothing here ever looks
//! at `elapsed_s`, selection, scrolling, the filter or collapsed
//! sets, or a resize — none of those are part of `RenderState` at
//! all, so they can never appear as a change by construction.

use std::collections::{BTreeMap, HashMap};

use crate::render::RenderState;
use crate::treemap::TreemapNode;

/// Every stable key that changed between `old` (the previous
/// redraw's render state, `None` before the first one has ever been
/// built) and `new` (this redraw's).
pub fn diff(old: Option<&RenderState>, new: &RenderState) -> Vec<String> {
    let Some(old) = old else {
        return Vec::new();
    };

    let mut changed = Vec::new();

    let old_rows: HashMap<&str, &crate::render::Row> =
        old.rows.iter().map(|r| (r.path.as_str(), r)).collect();
    for row in &new.rows {
        match old_rows.get(row.path.as_str()) {
            None => changed.push(format!("row:{}", row.path)),
            Some(prev) => {
                if prev.status != row.status || prev.loop_progress != row.loop_progress {
                    changed.push(format!("row:{}", row.path));
                }
            }
        }
    }

    let old_state: HashMap<&str, &crate::render::StateRow> = old
        .state_rows
        .iter()
        .map(|r| (r.path.as_str(), r))
        .collect();
    for row in &new.state_rows {
        match old_state.get(row.path.as_str()) {
            None => changed.push(format!("state:{}", row.path)),
            Some(prev) => {
                if prev.status != row.status || prev.dim != row.dim || prev.tokens != row.tokens {
                    changed.push(format!("state:{}", row.path));
                }
            }
        }
    }

    let mut old_treemap = BTreeMap::new();
    flatten_treemap(&old.treemap, &mut old_treemap);
    mark_innermost_treemap_changes(&new.treemap, &old_treemap, &mut changed);

    for i in old.log.len()..new.log.len() {
        changed.push(format!("log:{i}"));
    }

    if old.token_bar != new.token_bar {
        changed.push("tokenbar".to_string());
    }

    if old.header.status != new.header.status {
        changed.push("header:status".to_string());
    }
    if old.header.effects_done != new.header.effects_done
        || old.header.effects_planned != new.header.effects_planned
        || old.header.effects_planned_is_lower_bound != new.header.effects_planned_is_lower_bound
    {
        changed.push("header:effects".to_string());
    }
    if old.header.tokens_sent != new.header.tokens_sent
        || old.header.tokens_received != new.header.tokens_received
    {
        changed.push("header:tokens".to_string());
    }

    changed
}

fn flatten_treemap(nodes: &[TreemapNode], out: &mut BTreeMap<String, u64>) {
    for node in nodes {
        out.insert(node.path.clone(), node.total);
        flatten_treemap(&node.children, out);
    }
}

/// Whether `node`'s own total grew since `old_treemap`, or `node` is
/// new outright -- the raw "did this cell change" predicate, before
/// the innermost-only filter below is applied to it.
fn treemap_cell_grew(node: &TreemapNode, old_treemap: &BTreeMap<String, u64>) -> bool {
    match old_treemap.get(node.path.as_str()) {
        None => true,
        Some(prev_total) => node.total > *prev_total,
    }
}

/// Pushes `treemap:<path>` for the innermost node of every grown
/// subtree only (issue #535's treemap-highlight finding): a
/// container's own total is always the sum of its children
/// (`model::subtree_tokens`), so it rises whenever any child's does
/// -- tinting it every time points at nothing, since the change
/// never originated there. A node is reported only when its own
/// total grew (or it's new) *and* none of its direct children also
/// grew (or are new): that leaves exactly the leaf, or the deepest
/// container, the change actually came from. Checking only *direct*
/// children, not every descendant, is still enough to silence every
/// ancestor up the chain: totals only ever grow here, so if a leaf's
/// total rose, every proper ancestor's own total rose too, and each
/// one's own direct child on the path down to that leaf rose right
/// along with it -- `treemap_cell_grew` is true for that child, which
/// is exactly what excludes the ancestor one level up, and the next
/// one up from there, all the way to the root.
fn mark_innermost_treemap_changes(
    nodes: &[TreemapNode],
    old_treemap: &BTreeMap<String, u64>,
    changed: &mut Vec<String>,
) {
    for node in nodes {
        let grew = treemap_cell_grew(node, old_treemap);
        let a_child_grew = node
            .children
            .iter()
            .any(|child| treemap_cell_grew(child, old_treemap));
        if grew && !a_child_grew {
            changed.push(format!("treemap:{}", node.path));
        }
        mark_innermost_treemap_changes(&node.children, old_treemap, changed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::LogLine;
    use crate::model::{ProcessState, RunModel, StatusKind};
    use crate::plan::PlanTree;
    use serde_json::json;

    fn render(state: &serde_json::Value, now: &str, elapsed_s: f64) -> RenderState {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        crate::render::build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(state),
            ProcessState::Running,
            elapsed_s,
            now,
            &[],
        )
    }

    #[test]
    fn the_first_observation_reports_nothing() {
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "x", "meta": {"created_at": "t0", "completed_at": "t1"}}
        }});
        let new = render(&state, "t1", 1.0);
        assert_eq!(diff(None, &new), Vec::<String>::new());
    }

    #[test]
    fn a_rows_status_change_is_reported_by_path() {
        let running = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": null, "meta": {"created_at": "t0", "completed_at": null}}
        }});
        let done = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "x", "meta": {"created_at": "t0", "completed_at": "t1"}}
        }});
        let old = render(&running, "t0", 0.0);
        let new = render(&done, "t1", 1.0);
        let changed = diff(Some(&old), &new);
        assert!(changed.contains(&"row:prime.a".to_string()), "{changed:?}");
    }

    #[test]
    fn elapsed_time_alone_never_counts_as_a_change() {
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "x", "meta": {"created_at": "t0", "completed_at": "t1"}}
        }});
        let old = render(&state, "t1", 1.0);
        let new = render(&state, "t1", 9.0);
        assert_eq!(diff(Some(&old), &new), Vec::<String>::new());
    }

    #[test]
    fn a_new_state_row_and_a_token_change_on_an_existing_one_are_both_reported() {
        let before = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "x", "meta": {"created_at": "t0", "completed_at": "t1"}}
        }});
        let after = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "x", "meta": {"created_at": "t0", "completed_at": "t1", "tokens_sent": 5, "tokens_received": 6}},
            "b": {"value": "y", "meta": {"created_at": "t0", "completed_at": "t1"}}
        }});
        let old = render(&before, "t1", 1.0);
        let new = render(&after, "t1", 1.0);
        let changed = diff(Some(&old), &new);
        assert!(changed.contains(&"state:a".to_string()));
        assert!(changed.contains(&"state:b".to_string()));
    }

    #[test]
    fn a_treemap_cells_growing_total_is_reported_but_a_shrink_is_not() {
        let before = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "x", "meta": {"created_at": "t0", "completed_at": "t1", "tokens_sent": 1, "tokens_received": 1}}
        }});
        let after = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "x", "meta": {"created_at": "t0", "completed_at": "t1", "tokens_sent": 5, "tokens_received": 5}}
        }});
        let old = render(&before, "t1", 1.0);
        let new = render(&after, "t1", 1.0);
        let changed = diff(Some(&old), &new);
        assert!(
            changed
                .iter()
                .any(|k| k.starts_with("treemap:") && k.contains('a'))
        );
        // the fix this test now also covers: the root container's
        // own total rose too (it's the sum of "a"'s), but the change
        // came from "a", not from "prime" itself -- only the leaf is
        // reported.
        assert!(
            !changed.contains(&"treemap:prime".to_string()),
            "{changed:?}"
        );
        // the reverse (shrinking) must not be reported.
        let shrunk = diff(Some(&new), &old);
        assert!(!shrunk.iter().any(|k| k.starts_with("treemap:")));
    }

    /// Builds a `TreemapNode` directly, bypassing `treemap::build_tree`
    /// entirely -- these tests exercise `mark_innermost_treemap_changes`
    /// in isolation, so a node's `total` can be set freely without
    /// also having to construct flat state that would produce it
    /// through `model::subtree_tokens`.
    fn treemap_node(path: &str, total: u64, children: Vec<TreemapNode>) -> TreemapNode {
        TreemapNode {
            key: path.rsplit('.').next().unwrap_or(path).to_string(),
            path: path.to_string(),
            total,
            status: None,
            children,
        }
    }

    /// Issue #535's treemap-highlight finding: a leaf's total growing
    /// marks only that leaf, never the container above it even though
    /// the container's own total (the sum of its children) grew too.
    #[test]
    fn a_leafs_growing_total_marks_only_the_leaf_not_its_ancestors() {
        let mut old_treemap = BTreeMap::new();
        old_treemap.insert("prime".to_string(), 5);
        old_treemap.insert("prime.a".to_string(), 5);

        let new_tree = vec![treemap_node(
            "prime",
            9,
            vec![treemap_node("prime.a", 9, vec![])],
        )];

        let mut changed = Vec::new();
        mark_innermost_treemap_changes(&new_tree, &old_treemap, &mut changed);
        assert_eq!(changed, vec!["treemap:prime.a".to_string()]);
    }

    /// A new leaf appearing under an existing container marks only
    /// that new leaf: the container's own total rose (it now sums one
    /// more child), but the change still didn't originate there.
    #[test]
    fn a_new_leaf_under_an_existing_container_marks_only_the_new_leaf() {
        let mut old_treemap = BTreeMap::new();
        old_treemap.insert("prime".to_string(), 5);
        old_treemap.insert("prime.a".to_string(), 5);

        let new_tree = vec![treemap_node(
            "prime",
            8,
            vec![
                treemap_node("prime.a", 5, vec![]),
                treemap_node("prime.b", 3, vec![]),
            ],
        )];

        let mut changed = Vec::new();
        mark_innermost_treemap_changes(&new_tree, &old_treemap, &mut changed);
        assert_eq!(changed, vec!["treemap:prime.b".to_string()]);
    }

    /// A container whose own total rose with no child total rising is
    /// marked itself -- it's the innermost node the change actually
    /// reached, even though it isn't a leaf.
    #[test]
    fn a_containers_own_token_change_with_no_child_change_marks_the_container() {
        let mut old_treemap = BTreeMap::new();
        old_treemap.insert("prime".to_string(), 10);
        old_treemap.insert("prime.c".to_string(), 6);
        old_treemap.insert("prime.c.x".to_string(), 6);

        let new_tree = vec![treemap_node(
            "prime",
            14,
            vec![treemap_node(
                "prime.c",
                10,
                vec![treemap_node("prime.c.x", 6, vec![])],
            )],
        )];

        let mut changed = Vec::new();
        mark_innermost_treemap_changes(&new_tree, &old_treemap, &mut changed);
        assert_eq!(changed, vec!["treemap:prime.c".to_string()]);
    }

    #[test]
    fn new_log_lines_are_reported_by_index_and_old_ones_are_not() {
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null}}});
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let old = crate::render::build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            1.0,
            "t1",
            &[LogLine {
                ts: None,
                text: "first".into(),
            }],
        );
        let new = crate::render::build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            1.0,
            "t1",
            &[
                LogLine {
                    ts: None,
                    text: "first".into(),
                },
                LogLine {
                    ts: None,
                    text: "second".into(),
                },
            ],
        );
        let changed = diff(Some(&old), &new);
        assert_eq!(changed, vec!["log:1".to_string()]);
    }

    #[test]
    fn a_token_bar_change_is_reported() {
        let idle = json!({"prime": {"value": null, "meta": {"completed_at": null}}});
        let running = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "ask": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "prompt_sent": "hello there"}}
        }});
        let old = render(&idle, "t0", 0.0);
        let new = render(&running, "t0", 0.0);
        let changed = diff(Some(&old), &new);
        assert!(changed.contains(&"tokenbar".to_string()));
    }

    #[test]
    fn header_status_and_effects_changes_are_reported_separately() {
        let before = json!({"prime": {"value": null, "meta": {"completed_at": null}}});
        let after = json!({
            "prime": {"value": true, "meta": {"completed_at": "t1", "error": null},
                "a": {"value": "x", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "shell"}}
            },
            "runtime": {"last_run": {"completed_at": "t1", "totals": {}}}
        });
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let old = crate::render::build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&before),
            ProcessState::Running,
            0.0,
            "t0",
            &[],
        );
        let new = crate::render::build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&after),
            ProcessState::Exited {
                interrupted: false,
                likely_failed: false,
                has_failure_reason: false,
            },
            1.0,
            "t1",
            &[],
        );
        let changed = diff(Some(&old), &new);
        assert!(changed.contains(&"header:status".to_string()));
        assert!(changed.contains(&"header:effects".to_string()));
    }

    #[test]
    fn selection_scroll_filter_and_resize_are_not_part_of_render_state_at_all() {
        // Nothing here to assert beyond the type itself: `RenderState`
        // carries no selection, scroll, filter or terminal-size field,
        // so none of those can ever appear in `diff`'s own output —
        // the acceptance list's "selection, scroll, filter and resize
        // silent" holds by construction, not by a special case.
        let status_kind = StatusKind::Running;
        let _ = status_kind;
    }
}
