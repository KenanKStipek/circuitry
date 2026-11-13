//! The Select screen's own model (design v1 part 5, issue #698):
//! turning [`crate::discovery::DiscoveredRun`]s into rows for the
//! running list, and [`crate::history::HistoryRecord`]s into rows
//! for the empty screen's own Recent Runs table -- both pure and
//! terminal-independent, so a test builds either from plain data
//! with no real process or disk involved. The binary crate's own
//! `tui.rs` is the only thing that ever draws either.

use std::path::Path;

use crate::discovery::DiscoveredRun;
use crate::history::{HistoryRecord, Outcome};

/// One row of the Select screen's own running list -- `All` first
/// (§2.5), then one [`Row::Run`] per [`DiscoveredRun`], in
/// [`crate::discovery::discover_runs`]'s own launch-time order.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    All {
        running: usize,
    },
    Run {
        run: Box<DiscoveredRun>,
        /// The document's own basename, except when disambiguated
        /// per [`build_rows`]'s own tie-break rule -- `—` for a run
        /// discovered with no document name known yet (the live-
        /// state-in-cwd source, before its own events have named
        /// one).
        document_label: String,
        /// The config's own basename, `None` with no config known at
        /// all -- the running list's own row shows `—` for it
        /// (`tui.rs`'s own `select_row_spans`), never blank.
        config_label: Option<String>,
        /// This run's own short name (issue #717), already
        /// disambiguated against every other row on this same
        /// screen -- `None` for a run whose `run_start` hasn't
        /// arrived yet.
        name: Option<String>,
    },
}

/// Bare `osi`'s own choice of shape once discovery has run (issue
/// #698): exactly one running run attaches to it
/// directly, with no Select screen drawn at all; any other count --
/// none, or more than one -- shows Select, with the empty or running
/// body respectively.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    AttachSingle,
    ShowSelect,
}

pub fn shape_for_run_count(run_count: usize) -> Shape {
    if run_count == 1 {
        Shape::AttachSingle
    } else {
        Shape::ShowSelect
    }
}

fn basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// `a/triage.yml` -- `path`'s own basename, with its immediate
/// parent directory's own basename prefixed, `/`-joined (never the
/// parent's own full path, which the frame's own `a/triage.yml`
/// never shows either).
fn basename_with_parent(path: &str) -> String {
    let p = Path::new(path);
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string());
    match p.parent().and_then(Path::file_name) {
        Some(parent) => format!("{}/{name}", parent.to_string_lossy()),
        None => name,
    }
}

/// Every running [`DiscoveredRun`] turned into [`Row`]s, `All` first
/// (§2.5): basenames for the document and config columns, except
/// that a document whose (document, config) basename pair collides
/// with another row's own gets its parent directory added too
/// (issue #698: "add the parent directory only when two rows
/// would read identically") -- `runs` is already in launch order
/// ([`crate::discovery::discover_runs`]'s own), kept unchanged here.
/// A run with no document name known yet reads `—`, never blank.
pub fn build_rows(runs: &[DiscoveredRun]) -> Vec<Row> {
    let config_labels: Vec<Option<String>> = runs
        .iter()
        .map(|r| r.config.as_deref().map(basename))
        .collect();
    let document_labels = disambiguated_document_labels(runs, &config_labels);
    let labelled_ids: Vec<(Option<&str>, Option<&str>)> = runs
        .iter()
        .map(|r| (r.label.as_deref(), r.run_id.as_deref()))
        .collect();
    let names = crate::names::disambiguated_names(&labelled_ids);

    let mut rows = Vec::with_capacity(runs.len() + 1);
    rows.push(Row::All {
        running: runs.len(),
    });
    for (i, run) in runs.iter().enumerate() {
        rows.push(Row::Run {
            run: Box::new(run.clone()),
            document_label: document_labels[i].clone(),
            config_label: config_labels[i].clone(),
            name: names[i].clone(),
        });
    }
    rows
}

/// The same (document, config) collision rule [`build_rows`] uses
/// for its own running list, exposed for the All screen's own tiles
/// (`main.rs`'s `AllRunSlot`), which need the disambiguated label
/// without a [`Row`] wrapper around it: basenames for the document
/// column, except that a document whose (document, config) basename
/// pair collides with another run's own gets its parent directory
/// added too (issue #698: "add the parent directory only when two
/// rows would read identically"). `config_labels` is `runs`'s own
/// config column, already resolved to a basename (or `None`) by the
/// caller, since both callers already need it for their own purposes
/// too. Issue #717: the name already tells two such runs apart once
/// both have one, so a run with a known `run_id` never counts toward
/// a collision and never gets the parent directory added -- it stays
/// only for runs that have no name yet.
pub fn disambiguated_document_labels(
    runs: &[DiscoveredRun],
    config_labels: &[Option<String>],
) -> Vec<String> {
    let plain_doc_labels: Vec<String> = runs
        .iter()
        .map(|r| {
            r.orchestration
                .as_deref()
                .map(basename)
                .unwrap_or_else(|| "—".to_string())
        })
        .collect();

    let mut counts: std::collections::HashMap<(&str, &Option<String>), usize> =
        std::collections::HashMap::new();
    for ((doc, cfg), run) in plain_doc_labels
        .iter()
        .zip(config_labels.iter())
        .zip(runs.iter())
    {
        if run.run_id.is_some() {
            continue;
        }
        *counts.entry((doc.as_str(), cfg)).or_insert(0) += 1;
    }

    runs.iter()
        .enumerate()
        .map(|(i, run)| {
            if run.run_id.is_some() {
                return plain_doc_labels[i].clone();
            }
            let key = (plain_doc_labels[i].as_str(), &config_labels[i]);
            let collides = counts.get(&key).copied().unwrap_or(0) > 1;
            if collides {
                run.orchestration
                    .as_deref()
                    .map(basename_with_parent)
                    .unwrap_or_else(|| "—".to_string())
            } else {
                plain_doc_labels[i].clone()
            }
        })
        .collect()
}

/// A step in a [`HistoryRecord`]'s own `ok: false` (issue #698's
/// "handled failure"/"the failing step"), named by its display key.
fn failing_step_names(record: &HistoryRecord) -> Vec<&str> {
    record
        .steps
        .iter()
        .filter(|(_, step)| !step.ok)
        .map(|(name, _)| name.as_str())
        .collect()
}

/// The Recent Runs table's own OUTCOME column (issue #698): `ok`
/// alone with no `ok: false` step at all, `ok · N handled failure(s)`
/// for one that still ended ok, `failed · <step>` naming whichever
/// recorded step actually failed the run -- never the raw error
/// text, which this module never reads at all (the privacy note in
/// `history.rs`'s own header). `cancelled` for the one outcome no
/// frame shows.
pub fn outcome_label(record: &HistoryRecord) -> String {
    let failing = failing_step_names(record);
    match record.outcome {
        Outcome::Ok if failing.is_empty() => "ok".to_string(),
        Outcome::Ok => {
            let n = failing.len();
            let plural = if n == 1 { "" } else { "s" };
            format!("ok · {n} handled failure{plural}")
        }
        Outcome::Failed => match failing.first() {
            Some(step) => format!("failed · {step}"),
            None => "failed".to_string(),
        },
        Outcome::Cancelled => "cancelled".to_string(),
    }
}

/// One row of the empty Select screen's own Recent Runs table.
#[derive(Debug, Clone, PartialEq)]
pub struct RecentRow {
    pub document_label: String,
    /// `None` for a record with no `config` at all (an older record,
    /// or a run with none) -- the CONFIG column reads `—` for these.
    pub config_label: Option<String>,
    pub started_at: String,
    pub ended_at: String,
    pub took_s: f64,
    pub tokens: u64,
    pub ok: bool,
    /// The OUTCOME column's own text: [`outcome_label`]'s own
    /// result, or `"gone"` once [`build_recent_rows`] finds the
    /// record's own `run_dir` no longer exists on disk.
    pub outcome_label: String,
    pub run_dir: Option<String>,
    /// This run's own short name (issue #717), the same name it had
    /// while running, derived from `run_id` (every [`HistoryRecord`]
    /// has one) -- already disambiguated against every other row in
    /// this same table.
    pub name: String,
    /// The record's own `run_dir` existed once but no longer does --
    /// distinct from [`Self::openable`] being false for a record with
    /// no `run_dir` recorded at all (an older, pre-#700 record), which
    /// still shows its real outcome, just with no way to open it.
    pub gone: bool,
    /// Whether `enter` can open this row at all: a `run_dir` was
    /// recorded, and it still exists on disk (issue #698's own data
    /// note: an older record with no `run_dir` "cannot be opened"
    /// either, but is not `gone` -- that word is reserved for a
    /// directory that genuinely vanished).
    pub openable: bool,
}

/// Every [`HistoryRecord`] turned into a [`RecentRow`], newest
/// finished run first across every document (issue #698's own Recent
/// Runs rule) -- `run_dir_exists` is injectable so a test never
/// touches the real filesystem to prove the `gone` rule.
pub fn build_recent_rows(
    records: &[HistoryRecord],
    run_dir_exists: &dyn Fn(&str) -> bool,
) -> Vec<RecentRow> {
    let labelled_ids: Vec<(Option<&str>, Option<&str>)> = records
        .iter()
        .map(|r| (r.label.as_deref(), Some(r.run_id.as_str())))
        .collect();
    let names = crate::names::disambiguated_names(&labelled_ids);
    let mut rows: Vec<RecentRow> = records
        .iter()
        .zip(names)
        .map(|(record, name)| {
            let gone = match &record.run_dir {
                Some(dir) => !run_dir_exists(dir),
                None => false,
            };
            let openable = record.run_dir.is_some() && !gone;
            RecentRow {
                document_label: basename(&record.document),
                config_label: record.config.as_deref().map(basename),
                started_at: record.started_at.clone(),
                ended_at: record.ended_at.clone(),
                took_s: record.wall_s,
                tokens: record.tokens_sent + record.tokens_received,
                ok: record.outcome == Outcome::Ok,
                outcome_label: if gone {
                    "gone".to_string()
                } else {
                    outcome_label(record)
                },
                run_dir: record.run_dir.clone(),
                name: name.expect("every HistoryRecord has a run_id"),
                gone,
                openable,
            }
        })
        .collect();
    // Parsed, not a bare string compare: two records' own `ended_at`
    // can use different RFC3339 offset notations (`Z` against a
    // numeric `+00:00`) for the same or a different instant, which a
    // lexicographic compare can get backwards -- falls back to the
    // raw string only for a record whose own timestamp doesn't even
    // parse, rather than ever panicking on one.
    rows.sort_by(|a, b| {
        match (
            crate::diff::parse_instant(&a.ended_at),
            crate::diff::parse_instant(&b.ended_at),
        ) {
            (Some(a_ts), Some(b_ts)) => b_ts.cmp(&a_ts),
            _ => b.ended_at.cmp(&a.ended_at),
        }
    });
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::StepRecord;
    use std::collections::BTreeMap;

    fn run(orchestration: &str, config: Option<&str>) -> DiscoveredRun {
        DiscoveredRun {
            orchestration: Some(orchestration.to_string()),
            config: config.map(str::to_string),
            engine: Some("cof".to_string()),
            pid: Some(1),
            started_at: Some("2026-01-01T00:00:00Z".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn shape_for_run_count_attaches_only_at_exactly_one() {
        assert_eq!(shape_for_run_count(0), Shape::ShowSelect);
        assert_eq!(shape_for_run_count(1), Shape::AttachSingle);
        assert_eq!(shape_for_run_count(2), Shape::ShowSelect);
        assert_eq!(shape_for_run_count(6), Shape::ShowSelect);
    }

    #[test]
    fn build_rows_puts_all_first_with_the_running_count() {
        let rows = build_rows(&[run("a.yml", None), run("b.yml", None)]);
        assert_eq!(rows[0], Row::All { running: 2 });
        assert_eq!(rows.len(), 3);
    }

    #[test]
    fn build_rows_uses_plain_basenames_by_default() {
        let rows = build_rows(&[run("/work/triage.yml", Some("/work/config-triage.json"))]);
        let Row::Run {
            document_label,
            config_label,
            ..
        } = &rows[1]
        else {
            panic!("expected a run row");
        };
        assert_eq!(document_label, "triage.yml");
        assert_eq!(config_label.as_deref(), Some("config-triage.json"));
    }

    #[test]
    fn build_rows_adds_the_parent_directory_only_on_a_real_collision() {
        // Same basename, same config basename -- ambiguous, so both
        // rows get `parent/triage.yml` (rule 9).
        let rows = build_rows(&[
            run("/a/triage.yml", Some("/x/config.json")),
            run("/b/triage.yml", Some("/y/config.json")),
        ]);
        let labels: Vec<&str> = rows[1..]
            .iter()
            .map(|r| match r {
                Row::Run { document_label, .. } => document_label.as_str(),
                Row::All { .. } => unreachable!(),
            })
            .collect();
        assert_eq!(labels, vec!["a/triage.yml", "b/triage.yml"]);
    }

    #[test]
    fn build_rows_tells_two_named_runs_apart_without_the_parent_directory() {
        // Same basename, same config basename, but both runs already
        // have a run id -- the name tells them apart (issue #717), so
        // the parent-directory tie-break never kicks in.
        let mut a = run("/a/x.yml", Some("/x/config.json"));
        a.run_id = Some("936ff268-5f52-479a-887f-fda1a47dae63".to_string());
        let mut b = run("/b/x.yml", Some("/x/config.json"));
        b.run_id = Some("d2c8267e-2864-4e04-89d6-4f174bc64516".to_string());
        let rows = build_rows(&[a, b]);
        let labels: Vec<&str> = rows[1..]
            .iter()
            .map(|r| match r {
                Row::Run { document_label, .. } => document_label.as_str(),
                Row::All { .. } => unreachable!(),
            })
            .collect();
        assert_eq!(labels, vec!["x.yml", "x.yml"]);
    }

    #[test]
    fn build_rows_tells_the_two_triage_rows_apart_by_config_with_no_tie_break() {
        // The real frame's own case: same document basename, but
        // different config basenames already tell the rows apart --
        // no parent directory needed.
        let rows = build_rows(&[
            run("/a/triage.yml", Some("/x/config-triage.json")),
            run("/a/triage.yml", Some("/x/config-triage-b.json")),
        ]);
        let labels: Vec<&str> = rows[1..]
            .iter()
            .map(|r| match r {
                Row::Run { document_label, .. } => document_label.as_str(),
                Row::All { .. } => unreachable!(),
            })
            .collect();
        assert_eq!(labels, vec!["triage.yml", "triage.yml"]);
    }

    #[allow(clippy::too_many_arguments)]
    fn sample_record(
        run_id: &str,
        document: &str,
        config: Option<&str>,
        run_dir: Option<&str>,
        started_at: &str,
        ended_at: &str,
        outcome: Outcome,
        steps: BTreeMap<String, StepRecord>,
    ) -> HistoryRecord {
        HistoryRecord {
            v: crate::history::FORMAT_VERSION,
            run_id: run_id.to_string(),
            document: document.to_string(),
            config: config.map(str::to_string),
            run_dir: run_dir.map(str::to_string),
            document_hash: None,
            engine: Some("cof".to_string()),
            label: None,
            started_at: started_at.to_string(),
            ended_at: ended_at.to_string(),
            outcome,
            wall_s: 5.0,
            effects_run: 3,
            tokens_sent: 100,
            tokens_received: 15,
            cost_usd: None,
            steps,
        }
    }

    fn step(ok: bool) -> StepRecord {
        StepRecord {
            start_s: 0.0,
            end_s: 1.0,
            busy_s: 1.0,
            passes: 1,
            tokens_sent: None,
            tokens_received: None,
            cost_usd: None,
            ok,
        }
    }

    #[test]
    fn outcome_label_reads_ok_alone_with_no_failed_step() {
        let record = sample_record(
            "r1",
            "d.yml",
            None,
            None,
            "t0",
            "t1",
            Outcome::Ok,
            BTreeMap::from([("a".to_string(), step(true))]),
        );
        assert_eq!(outcome_label(&record), "ok");
    }

    #[test]
    fn outcome_label_counts_handled_failures() {
        let record = sample_record(
            "r1",
            "d.yml",
            None,
            None,
            "t0",
            "t1",
            Outcome::Ok,
            BTreeMap::from([
                ("a".to_string(), step(true)),
                ("flaky".to_string(), step(false)),
            ]),
        );
        assert_eq!(outcome_label(&record), "ok · 1 handled failure");
    }

    #[test]
    fn outcome_label_pluralizes_more_than_one_handled_failure() {
        let record = sample_record(
            "r1",
            "d.yml",
            None,
            None,
            "t0",
            "t1",
            Outcome::Ok,
            BTreeMap::from([
                ("flaky".to_string(), step(false)),
                ("flaky2".to_string(), step(false)),
            ]),
        );
        assert_eq!(outcome_label(&record), "ok · 2 handled failures");
    }

    #[test]
    fn outcome_label_names_the_failing_step() {
        let record = sample_record(
            "r1",
            "d.yml",
            None,
            None,
            "t0",
            "t1",
            Outcome::Failed,
            BTreeMap::from([
                ("kind".to_string(), step(true)),
                ("tests".to_string(), step(false)),
            ]),
        );
        assert_eq!(outcome_label(&record), "failed · tests");
    }

    #[test]
    fn build_recent_rows_orders_newest_finished_first() {
        let records = vec![
            sample_record(
                "r1",
                "d.yml",
                None,
                None,
                "t0",
                "2026-01-01T00:00:00Z",
                Outcome::Ok,
                BTreeMap::new(),
            ),
            sample_record(
                "r2",
                "d.yml",
                None,
                None,
                "t0",
                "2026-01-02T00:00:00Z",
                Outcome::Ok,
                BTreeMap::new(),
            ),
        ];
        let rows = build_recent_rows(&records, &|_| true);
        assert_eq!(rows[0].ended_at, "2026-01-02T00:00:00Z");
        assert_eq!(rows[1].ended_at, "2026-01-01T00:00:00Z");
    }

    #[test]
    fn build_recent_rows_reads_gone_when_the_run_dir_no_longer_exists() {
        let records = vec![sample_record(
            "r1",
            "d.yml",
            None,
            Some("/work/run-dir"),
            "t0",
            "t1",
            Outcome::Ok,
            BTreeMap::new(),
        )];
        let rows = build_recent_rows(&records, &|_| false);
        assert!(rows[0].gone);
        assert_eq!(rows[0].outcome_label, "gone");
    }

    #[test]
    fn build_recent_rows_cannot_open_a_record_with_no_run_dir_but_still_shows_its_outcome() {
        let records = vec![sample_record(
            "r1",
            "d.yml",
            None,
            None,
            "t0",
            "t1",
            Outcome::Ok,
            BTreeMap::new(),
        )];
        let rows = build_recent_rows(&records, &|_| true);
        assert!(
            !rows[0].gone,
            "a record with no run_dir at all is not 'gone'"
        );
        assert!(!rows[0].openable);
        assert_eq!(rows[0].outcome_label, "ok");
    }

    #[test]
    fn build_recent_rows_is_openable_only_with_a_run_dir_that_still_exists() {
        let records = vec![
            sample_record(
                "r1",
                "d.yml",
                None,
                Some("/work/run-dir"),
                "t0",
                "t1",
                Outcome::Ok,
                BTreeMap::new(),
            ),
            sample_record(
                "r2",
                "d.yml",
                None,
                Some("/work/gone-dir"),
                "t0",
                "t2",
                Outcome::Ok,
                BTreeMap::new(),
            ),
        ];
        let rows = build_recent_rows(&records, &|dir| dir == "/work/run-dir");
        let by_dir = |dir: &str| {
            rows.iter()
                .find(|r| r.run_dir.as_deref() == Some(dir))
                .unwrap()
        };
        assert!(by_dir("/work/run-dir").openable);
        assert!(!by_dir("/work/run-dir").gone);
        assert!(!by_dir("/work/gone-dir").openable);
        assert!(by_dir("/work/gone-dir").gone);
    }

    #[test]
    fn build_recent_rows_orders_by_parsed_time_not_the_raw_string() {
        // A later instant written with a numeric offset sorts after
        // an earlier one written as `Z` -- a bare string compare
        // would get this backwards (`'+'` sorts before `'Z'`).
        let records = vec![
            sample_record(
                "r1",
                "d.yml",
                None,
                None,
                "t0",
                "2026-01-01T00:00:00Z",
                Outcome::Ok,
                BTreeMap::new(),
            ),
            sample_record(
                "r2",
                "d.yml",
                None,
                None,
                "t0",
                "2026-01-01T01:00:00+00:00",
                Outcome::Ok,
                BTreeMap::new(),
            ),
        ];
        let rows = build_recent_rows(&records, &|_| true);
        assert_eq!(rows[0].ended_at, "2026-01-01T01:00:00+00:00");
        assert_eq!(rows[1].ended_at, "2026-01-01T00:00:00Z");
    }

    #[test]
    fn build_recent_rows_reads_an_em_dash_config_as_none() {
        let records = vec![sample_record(
            "r1",
            "d.yml",
            None,
            Some("/work/run-dir"),
            "t0",
            "t1",
            Outcome::Ok,
            BTreeMap::new(),
        )];
        let rows = build_recent_rows(&records, &|_| true);
        assert_eq!(rows[0].config_label, None);
    }
}
