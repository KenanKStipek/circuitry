//! Folding the orchestration pane's own rows (DESIGN.md, issue #613):
//! breadcrumbs for a single-child container, and a summary row for a
//! run of quick, finished tool steps. This operates purely on
//! [`crate::render::Row`]s a caller already built (never on state or
//! the plan directly), so the TUI and tests see exactly the same
//! rows either way -- [`fold_rows`] is the one place folding happens,
//! called by [`crate::render::build_folded`].

use std::collections::BTreeSet;

use crate::model::StatusKind;
use crate::render::{Row, RowKind};

#[derive(Debug, Clone)]
struct RowTree {
    row: Row,
    children: Vec<RowTree>,
}

/// Recognises a [`fold_rows`] quick-steps summary row from its own
/// label (`"4 quick steps"`) -- the one place a front end needs to
/// tell a merged group apart from an ordinary row, for ⏎'s own
/// "expand in place" (DESIGN.md, issue #613).
pub fn is_quick_steps_label(label: &str) -> bool {
    label
        .strip_suffix(" quick steps")
        .is_some_and(|n| n.parse::<u32>().is_ok())
}

/// Folds `rows` (a depth-first tree, as [`crate::render::build`]
/// produces): a container with exactly one child folds with it into
/// one row with a breadcrumb label (`review › fix › agent`), and two
/// or more consecutive, finished, sub-second tool steps fold into one
/// `N quick steps` summary row. `enabled: false` returns `rows`
/// unchanged -- the toggle key's "off" state. `expanded_quick_steps`
/// names quick-steps groups (by their own representative path, the
/// first step's) the user asked to see in full with ⏎; folding skips
/// exactly those groups.
pub fn fold_rows(
    rows: Vec<Row>,
    enabled: bool,
    expanded_quick_steps: &BTreeSet<String>,
) -> Vec<Row> {
    if !enabled || rows.is_empty() {
        return rows;
    }
    let base_depth = rows[0].depth;
    let mut i = 0;
    let forest = build_forest(&rows, &mut i, base_depth);
    let folded = fold_forest(forest, expanded_quick_steps);
    let mut out = Vec::with_capacity(rows.len());
    flatten_forest(folded, base_depth, &mut out);
    out
}

fn build_forest(rows: &[Row], i: &mut usize, depth: usize) -> Vec<RowTree> {
    let mut out = Vec::new();
    while *i < rows.len() && rows[*i].depth == depth {
        let row = rows[*i].clone();
        *i += 1;
        let children = if *i < rows.len() && rows[*i].depth == depth + 1 {
            build_forest(rows, i, depth + 1)
        } else {
            Vec::new()
        };
        out.push(RowTree { row, children });
    }
    out
}

fn flatten_forest(trees: Vec<RowTree>, depth: usize, out: &mut Vec<Row>) {
    for tree in trees {
        let mut row = tree.row;
        row.depth = depth;
        row.has_children = !tree.children.is_empty();
        out.push(row);
        flatten_forest(tree.children, depth + 1, out);
    }
}

fn fold_forest(trees: Vec<RowTree>, expanded_quick_steps: &BTreeSet<String>) -> Vec<RowTree> {
    let folded: Vec<RowTree> = trees
        .into_iter()
        .map(|t| fold_node(t, expanded_quick_steps))
        .collect();
    merge_quick_steps(folded, expanded_quick_steps)
}

fn fold_node(node: RowTree, expanded_quick_steps: &BTreeSet<String>) -> RowTree {
    let children = fold_forest(node.children, expanded_quick_steps);
    if children.len() == 1
        && breadcrumb_eligible(&node.row)
        && breadcrumb_eligible(&children[0].row)
    {
        let child = children.into_iter().next().expect("checked len == 1");
        let combined = Row {
            path: child.row.path.clone(),
            depth: node.row.depth,
            label: format!("{} › {}", node.row.label, child.row.label),
            kind: child.row.kind,
            status: child.row.status.clone(),
            duration_s: child.row.duration_s,
            loop_progress: child.row.loop_progress,
            dim: child.row.dim.clone(),
            has_children: child.row.has_children,
        };
        return RowTree {
            row: combined,
            children: child.children,
        };
    }
    RowTree {
        row: node.row,
        children,
    }
}

/// A container, or its one child, is eligible for a breadcrumb fold
/// (issue #613) once its own status is actually known -- a still-
/// `Pending` row (including an event-only row discovered from
/// `--events` alone, before its state node or plan match settled
/// what it even is) stays unfolded rather than merging into a
/// breadcrumb that would have to be relabelled again the moment real
/// data arrives. A running row *is* eligible: its own breadcrumb is
/// exactly the row DESIGN.md says follow mode should target. The
/// document root never folds away.
fn breadcrumb_eligible(row: &Row) -> bool {
    row.kind != RowKind::Root && row.status.kind != StatusKind::Pending
}

/// A finished tool step under a second (issue #613: "two or more
/// consecutive finished sibling tool steps, each under a second") --
/// a step that is running, failed, queued or still pending is never
/// folded, so this requires exactly [`StatusKind::Done`], not merely
/// "not running".
fn is_quick_step(row: &Row) -> bool {
    !row.has_children
        && row.kind == RowKind::Tool
        && row.status.kind == StatusKind::Done
        && row.duration_s.is_some_and(|secs| secs < 1.0)
}

/// Merges a run of two or more consecutive [`is_quick_step`] siblings
/// into one `N quick steps` row, keyed (for `expanded_quick_steps`,
/// selection and follow) by the first step's own path -- unless that
/// key is in `expanded_quick_steps`, in which case this run is left
/// exactly as it was (⏎'s own "expand in place").
fn merge_quick_steps(trees: Vec<RowTree>, expanded_quick_steps: &BTreeSet<String>) -> Vec<RowTree> {
    let mut out = Vec::with_capacity(trees.len());
    let mut i = 0;
    while i < trees.len() {
        if !is_quick_step(&trees[i].row) {
            out.push(trees[i].clone());
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < trees.len() && is_quick_step(&trees[j].row) {
            j += 1;
        }
        let run_len = j - i;
        if run_len >= 2 {
            let rep_path = trees[i].row.path.clone();
            if expanded_quick_steps.contains(&rep_path) {
                out.extend(trees[i..j].iter().cloned());
            } else {
                let total: f64 = trees[i..j].iter().filter_map(|t| t.row.duration_s).sum();
                out.push(RowTree {
                    row: Row {
                        path: rep_path,
                        depth: trees[i].row.depth,
                        label: format!("{run_len} quick steps"),
                        kind: RowKind::Unknown,
                        status: trees[i].row.status.clone(),
                        duration_s: Some(total),
                        loop_progress: None,
                        dim: None,
                        has_children: false,
                    },
                    children: Vec::new(),
                });
            }
        } else {
            out.push(trees[i].clone());
        }
        i = j;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::RowStatus;

    fn row(
        path: &str,
        depth: usize,
        kind: RowKind,
        status: StatusKind,
        duration_s: Option<f64>,
    ) -> Row {
        Row {
            path: path.to_string(),
            depth,
            label: path.rsplit('.').next().unwrap().to_string(),
            kind,
            status: RowStatus {
                kind: status,
                skip_reason: None,
                retrying: false,
            },
            duration_s,
            loop_progress: None,
            dim: None,
            has_children: false,
        }
    }

    fn container(path: &str, depth: usize, kind: RowKind, status: StatusKind) -> Row {
        let mut r = row(path, depth, kind, status, None);
        r.has_children = true;
        r
    }

    #[test]
    fn a_single_child_container_folds_into_a_breadcrumb() {
        let rows = vec![
            container("prime", 0, RowKind::Root, StatusKind::Running),
            container("prime.review", 1, RowKind::Dynamic, StatusKind::Running),
            container("prime.review.fix", 2, RowKind::Dynamic, StatusKind::Running),
            row(
                "prime.review.fix.agent",
                3,
                RowKind::Tool,
                StatusKind::Running,
                Some(6.0),
            ),
        ];
        let folded = fold_rows(rows, true, &BTreeSet::new());
        // `prime` never folds away; `review`/`fix`/`agent`, each with
        // exactly one child in turn, collapse into one breadcrumb row.
        assert_eq!(folded.len(), 2);
        assert_eq!(folded[0].path, "prime");
        assert_eq!(folded[1].path, "prime.review.fix.agent");
        assert_eq!(folded[1].label, "review › fix › agent");
        assert_eq!(folded[1].depth, 1);
        assert_eq!(folded[1].status.kind, StatusKind::Running);
    }

    #[test]
    fn a_breadcrumb_chain_with_a_running_leaf_keeps_the_leafs_own_path_for_follow() {
        let rows = vec![
            container("prime", 0, RowKind::Root, StatusKind::Running),
            container("prime.a", 1, RowKind::Dynamic, StatusKind::Running),
            row(
                "prime.a.b",
                2,
                RowKind::Tool,
                StatusKind::Running,
                Some(1.0),
            ),
        ];
        let folded = fold_rows(rows, true, &BTreeSet::new());
        let breadcrumb = folded.iter().find(|r| r.label.contains('›')).unwrap();
        // Follow mode selects by path -- the breadcrumb's own path
        // must be the running leaf's, not the container's.
        assert_eq!(breadcrumb.path, "prime.a.b");
    }

    #[test]
    fn a_pending_single_child_container_stays_unfolded() {
        let rows = vec![
            container("prime", 0, RowKind::Root, StatusKind::Running),
            container("prime.a", 1, RowKind::Dynamic, StatusKind::Pending),
            row("prime.a.b", 2, RowKind::Tool, StatusKind::Pending, None),
        ];
        let folded = fold_rows(rows, true, &BTreeSet::new());
        assert_eq!(folded.len(), 3, "{folded:?}");
    }

    #[test]
    fn two_or_more_consecutive_sub_second_finished_tool_steps_fold_into_one_summary_row() {
        let rows = vec![
            container("prime", 0, RowKind::Root, StatusKind::Done),
            row("prime.a", 1, RowKind::Tool, StatusKind::Done, Some(0.3)),
            row("prime.b", 1, RowKind::Tool, StatusKind::Done, Some(0.5)),
            row("prime.c", 1, RowKind::Tool, StatusKind::Done, Some(0.2)),
        ];
        let folded = fold_rows(rows, true, &BTreeSet::new());
        assert_eq!(folded.len(), 2);
        let summary = &folded[1];
        assert_eq!(summary.label, "3 quick steps");
        assert_eq!(summary.path, "prime.a");
        assert!((summary.duration_s.unwrap() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_quick_steps_run_after_a_non_quick_step_sibling_is_still_merged() {
        // Regression: the run-length scan must restart at the first
        // quick step itself, not skip the whole remaining list just
        // because the *first* sibling in `trees` wasn't one.
        let rows = vec![
            container("prime", 0, RowKind::Root, StatusKind::Done),
            container("prime.review", 1, RowKind::Dynamic, StatusKind::Done),
            row("prime.q1", 1, RowKind::Tool, StatusKind::Done, Some(0.3)),
            row("prime.q2", 1, RowKind::Tool, StatusKind::Done, Some(0.5)),
            row("prime.q3", 1, RowKind::Tool, StatusKind::Done, Some(0.2)),
        ];
        let folded = fold_rows(rows, true, &BTreeSet::new());
        assert_eq!(folded.len(), 3, "{folded:?}");
        assert_eq!(folded[0].path, "prime");
        assert_eq!(folded[1].path, "prime.review");
        assert_eq!(folded[2].label, "3 quick steps");
        assert_eq!(folded[2].path, "prime.q1");
    }

    #[test]
    fn a_single_quick_step_does_not_fold_alone() {
        let rows = vec![
            container("prime", 0, RowKind::Root, StatusKind::Done),
            row("prime.a", 1, RowKind::Tool, StatusKind::Done, Some(0.3)),
            row("prime.b", 1, RowKind::Tool, StatusKind::Done, Some(5.0)),
        ];
        let folded = fold_rows(rows, true, &BTreeSet::new());
        assert_eq!(folded.len(), 3, "{folded:?}");
    }

    #[test]
    fn a_running_failed_queued_or_pending_step_never_folds_into_quick_steps() {
        for status in [
            StatusKind::Running,
            StatusKind::Failed,
            StatusKind::RunningOrQueued,
            StatusKind::Pending,
        ] {
            let rows = vec![
                container("prime", 0, RowKind::Root, StatusKind::Running),
                row("prime.a", 1, RowKind::Tool, StatusKind::Done, Some(0.3)),
                row("prime.b", 1, RowKind::Tool, status, Some(0.2)),
            ];
            let folded = fold_rows(rows, true, &BTreeSet::new());
            assert_eq!(folded.len(), 3, "status {status:?}: {folded:?}");
        }
    }

    #[test]
    fn expanding_a_quick_steps_group_by_its_representative_path_shows_it_in_full() {
        let rows = vec![
            container("prime", 0, RowKind::Root, StatusKind::Done),
            row("prime.a", 1, RowKind::Tool, StatusKind::Done, Some(0.3)),
            row("prime.b", 1, RowKind::Tool, StatusKind::Done, Some(0.5)),
        ];
        let mut expanded = BTreeSet::new();
        expanded.insert("prime.a".to_string());
        let folded = fold_rows(rows, true, &expanded);
        assert_eq!(folded.len(), 3, "{folded:?}");
    }

    #[test]
    fn is_quick_steps_label_recognises_its_own_format_and_nothing_else() {
        assert!(is_quick_steps_label("4 quick steps"));
        assert!(is_quick_steps_label("12 quick steps"));
        assert!(!is_quick_steps_label("agent"));
        assert!(!is_quick_steps_label("quick steps"));
    }

    #[test]
    fn folding_disabled_returns_the_full_tree_unchanged() {
        let rows = vec![
            container("prime", 0, RowKind::Root, StatusKind::Running),
            container("prime.a", 1, RowKind::Dynamic, StatusKind::Running),
            row(
                "prime.a.b",
                2,
                RowKind::Tool,
                StatusKind::Running,
                Some(1.0),
            ),
        ];
        let original = rows.clone();
        let folded = fold_rows(rows, false, &BTreeSet::new());
        assert_eq!(folded, original);
    }
}
