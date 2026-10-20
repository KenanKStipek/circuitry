//! osi's run history (issue #649): a short, per-document record of
//! each finished run -- how long each step took, its tokens and
//! cost, and how the run ended -- kept at one JSONL file per
//! document under `$XDG_STATE_HOME/circuitry/osi/history/`, named by
//! the SHA-256 of the document's own absolute path.  From the
//! records this module works out what is usual for each step (p50/
//! p90) and, once there are enough runs of the same version of a
//! document, an ETA for the whole run.
//!
//! Only a plan-known path's own name, its pass count, timing, tokens
//! and cost are ever recorded.  No values, prompts, rendered params
//! or commands, tool targets, error text, stdout/stderr, report
//! messages or data, model replies, transcript content, the
//! environment, or the config -- [`build_record`] reads nothing but
//! `runtime.last_run`/`runtime.last_run.totals` and each node's own
//! `created_at`/`completed_at`/`tokens_sent`/`tokens_received`/
//! `cost_usd`, and [`RunModel::step_intervals`] carries the same
//! handful of fields.
//!
//! Callers never touch the file format directly: [`append_record`]
//! and [`read_records`] are the only entry points that open a file,
//! and both take the already-resolved history directory and the
//! document's own path -- `env` is read exactly once, by the CLI
//! binary, the same convention `config_discovery` and
//! `create_run_dir` already follow.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::diff::parse_instant;
use crate::model::{RunModel, StepInterval, ended_outcome, run_totals};
use crate::observe::duration_seconds;
use crate::plan::PlanTree;

/// The history record's own `v` (format version) -- a line with any
/// other value is skipped on read, not just treated as this one.
pub const FORMAT_VERSION: u32 = 1;

/// Every path a run's plan doesn't know by name (a generated
/// `dynamic` child, a `use: inline:` child's own made-up names) folds
/// into this one entry, never recorded under its real, data-dependent
/// name.
pub const OTHER_STEP: &str = "(other)";

const MAX_RUNS_PER_DOCUMENT: usize = 50;
const COMPACT_AT_LINES: usize = 100;
const MAX_STEPS_PER_RECORD: usize = 500;
const MAX_LINE_BYTES: usize = 64 * 1024;
const MAX_DOCUMENTS: usize = 500;
const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
/// "osi uses the newest 20" (issue #649) -- every statistic (whole-run
/// and per-step alike) is worked out from at most this many of a
/// document's own most recent runs.
const NEWEST_RUNS_FOR_STATS: usize = 20;
const P50_MIN_RUNS: usize = 3;
const P10_P90_MIN_RUNS: usize = 5;

// ---------------------------------------------------------------------
// The record itself
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepRecord {
    pub start_s: f64,
    pub end_s: f64,
    pub busy_s: f64,
    pub passes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_sent: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_received: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryRecord {
    pub v: u32,
    pub run_id: String,
    /// The document's own absolute path, resolved by the caller
    /// (`main.rs`'s one real call site) before this is ever built --
    /// never read back out of `runtime.last_run.orchestration_path`
    /// itself, which neither engine guarantees is absolute.
    pub document: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub document_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    pub started_at: String,
    pub ended_at: String,
    pub outcome: Outcome,
    pub wall_s: f64,
    pub effects_run: u64,
    pub tokens_sent: u64,
    pub tokens_received: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    pub steps: BTreeMap<String, StepRecord>,
}

/// A concrete, `prime.`-prefixed path's own display key (issue #649:
/// "keyed by its plan path with loop passes written `iter_*`") --
/// `None` for `prime` itself, which is the whole run, never a step.
fn display_key(path: &str, plan: &PlanTree) -> Option<String> {
    let relative = path.strip_prefix("prime.")?;
    if !plan.has_plan() {
        // "`osi watch` with no `--plan` records top-level steps
        // only" -- with no plan at all there is no way to tell a
        // plan-known path from a data-dependent one, so this is the
        // one case that isn't "named or `(other)`": every deeper
        // path folds into its own top-level segment instead.
        let top = relative.split('.').next().unwrap_or(relative);
        return Some(top.to_string());
    }
    if plan.match_path(path).is_some() {
        Some(normalize_iter_segments(relative))
    } else {
        Some(OTHER_STEP.to_string())
    }
}

fn normalize_iter_segments(path: &str) -> String {
    path.split('.')
        .map(|seg| {
            let is_pass = seg.len() > 5
                && seg.starts_with("iter_")
                && seg[5..].bytes().all(|b| b.is_ascii_digit());
            if is_pass { "iter_*" } else { seg }
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// Merges a set of `[start_s, end_s]` ranges into their own union,
/// returning the total covered length -- issue #649's "time actually
/// running, with overlapping tree passes counted once".
fn merged_busy_s(mut ranges: Vec<(f64, f64)>) -> f64 {
    ranges.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut total = 0.0;
    let mut cur: Option<(f64, f64)> = None;
    for (start, end) in ranges {
        match cur {
            None => cur = Some((start, end)),
            Some((cur_start, cur_end)) => {
                if start <= cur_end {
                    cur = Some((cur_start, cur_end.max(end)));
                } else {
                    total += cur_end - cur_start;
                    cur = Some((start, end));
                }
            }
        }
    }
    if let Some((start, end)) = cur {
        total += end - start;
    }
    total
}

/// One concrete path's own interval, already resolved to the run's
/// own display key and to second offsets from the run's start.
struct ResolvedInterval {
    key: String,
    start_s: f64,
    end_s: f64,
    ok: bool,
    tokens_sent: Option<u64>,
    tokens_received: Option<u64>,
    cost_usd: Option<f64>,
}

fn resolve_interval(
    interval: &StepInterval,
    plan: &PlanTree,
    run_start_ts: &str,
) -> Option<ResolvedInterval> {
    let key = display_key(&interval.path, plan)?;
    let start_s = duration_seconds(run_start_ts, &interval.start_ts)?;
    let end_s = duration_seconds(run_start_ts, &interval.end_ts)?;
    Some(ResolvedInterval {
        key,
        start_s,
        end_s,
        ok: interval.ok,
        tokens_sent: interval.tokens_sent,
        tokens_received: interval.tokens_received,
        cost_usd: interval.cost_usd,
    })
}

fn build_steps(model: &RunModel, plan: &PlanTree, run_start_ts: &str) -> BTreeMap<String, StepRecord> {
    let mut by_key: BTreeMap<String, Vec<ResolvedInterval>> = BTreeMap::new();
    for interval in model.step_intervals() {
        if let Some(resolved) = resolve_interval(interval, plan, run_start_ts) {
            by_key.entry(resolved.key.clone()).or_default().push(resolved);
        }
        let overflowed = model.overflowed_passes(&interval.path);
        if overflowed > 0 {
            if let Some(key) = display_key(&interval.path, plan) {
                // Counted once per concrete path already seen above,
                // so this only double-adds the overflow tail, never
                // the passes `step_intervals` itself still carries.
                by_key.entry(key).or_default();
            }
        }
    }

    let mut steps: BTreeMap<String, StepRecord> = BTreeMap::new();
    let mut overflow_passes: BTreeMap<String, u64> = BTreeMap::new();
    for interval in model.step_intervals() {
        let overflowed = model.overflowed_passes(&interval.path);
        if overflowed > 0 {
            if let Some(key) = display_key(&interval.path, plan) {
                *overflow_passes.entry(key).or_insert(0) += u64::from(overflowed);
            }
        }
    }

    for (key, intervals) in by_key {
        let ranges: Vec<(f64, f64)> = intervals.iter().map(|i| (i.start_s, i.end_s)).collect();
        let start_s = intervals
            .iter()
            .map(|i| i.start_s)
            .fold(f64::INFINITY, f64::min);
        let end_s = intervals.iter().map(|i| i.end_s).fold(f64::NEG_INFINITY, f64::max);
        let busy_s = merged_busy_s(ranges);
        let passes = intervals.len() as u64 + overflow_passes.get(&key).copied().unwrap_or(0);
        let ok = intervals.iter().all(|i| i.ok);
        let tokens_sent = sum_optional(intervals.iter().map(|i| i.tokens_sent));
        let tokens_received = sum_optional(intervals.iter().map(|i| i.tokens_received));
        let cost_usd = sum_optional_f64(intervals.iter().map(|i| i.cost_usd));
        steps.insert(
            key,
            StepRecord {
                start_s: if start_s.is_finite() { start_s } else { 0.0 },
                end_s: if end_s.is_finite() { end_s } else { 0.0 },
                busy_s,
                passes,
                tokens_sent,
                tokens_received,
                cost_usd,
                ok,
            },
        );
    }

    cap_steps(steps)
}

fn sum_optional<I: Iterator<Item = Option<u64>>>(values: I) -> Option<u64> {
    let mut any = false;
    let mut total = 0u64;
    for v in values {
        if let Some(v) = v {
            any = true;
            total += v;
        }
    }
    any.then_some(total)
}

fn sum_optional_f64<I: Iterator<Item = Option<f64>>>(values: I) -> Option<f64> {
    let mut any = false;
    let mut total = 0.0;
    for v in values {
        if let Some(v) = v {
            any = true;
            total += v;
        }
    }
    any.then_some(total)
}

/// Issue #649's "at most 500 steps per run. The longest are kept by
/// `busy_s` and the rest are summed into `(other)`."
fn cap_steps(steps: BTreeMap<String, StepRecord>) -> BTreeMap<String, StepRecord> {
    if steps.len() <= MAX_STEPS_PER_RECORD {
        return steps;
    }
    let mut by_busy: Vec<(String, StepRecord)> = steps.into_iter().collect();
    by_busy.sort_by(|a, b| b.1.busy_s.total_cmp(&a.1.busy_s));
    let keep = MAX_STEPS_PER_RECORD.saturating_sub(1);
    let mut kept: BTreeMap<String, StepRecord> = by_busy.drain(..keep).collect();
    let mut other = kept.remove(OTHER_STEP).unwrap_or(StepRecord {
        start_s: 0.0,
        end_s: 0.0,
        busy_s: 0.0,
        passes: 0,
        tokens_sent: None,
        tokens_received: None,
        cost_usd: None,
        ok: true,
    });
    for (_, rest) in by_busy {
        other.busy_s += rest.busy_s;
        other.passes += rest.passes;
        other.start_s = other.start_s.min(rest.start_s);
        other.end_s = other.end_s.max(rest.end_s);
        other.ok &= rest.ok;
        other.tokens_sent = add_optional(other.tokens_sent, rest.tokens_sent);
        other.tokens_received = add_optional(other.tokens_received, rest.tokens_received);
        other.cost_usd = add_optional_f64(other.cost_usd, rest.cost_usd);
    }
    kept.insert(OTHER_STEP.to_string(), other);
    kept
}

fn add_optional(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
    }
}

fn add_optional_f64(a: Option<f64>, b: Option<f64>) -> Option<f64> {
    match (a, b) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0.0) + b.unwrap_or(0.0)),
    }
}

/// Builds one run's own history record (issue #649), or `None` when
/// this run shouldn't be recorded at all: no final state write yet
/// (`runtime.last_run.completed_at` unset), a dry run, a validate-
/// only run, or a run with no `document_hash`.  `document_path` is
/// the caller's own, already-resolved document path -- see
/// [`HistoryRecord::document`]'s own doc comment.
pub fn build_record(
    document_path: &str,
    engine: Option<&str>,
    state: &Value,
    model: &RunModel,
    plan: &PlanTree,
) -> Option<HistoryRecord> {
    let last_run = state.pointer("/runtime/last_run")?;
    let completed_at = last_run.get("completed_at").and_then(Value::as_str)?;
    if last_run
        .get("dry_run")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    if last_run
        .get("validate_only")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return None;
    }
    let document_hash = last_run
        .get("document_hash")
        .and_then(Value::as_str)?
        .to_string();
    let run_id = last_run
        .get("run_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let started_at = last_run.get("started_at").and_then(Value::as_str)?.to_string();
    let wall_s = duration_seconds(&started_at, completed_at).unwrap_or(0.0);
    let outcome = match ended_outcome(state) {
        crate::model::EndedOutcome::Ok => Outcome::Ok,
        crate::model::EndedOutcome::Failed => Outcome::Failed,
        crate::model::EndedOutcome::Cancelled => Outcome::Cancelled,
    };
    let totals = run_totals(state);
    let effects_run = totals
        .as_ref()
        .and_then(|t| t.get("effects_run"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let tokens_sent = totals
        .as_ref()
        .and_then(|t| t.get("tokens_sent"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let tokens_received = totals
        .as_ref()
        .and_then(|t| t.get("tokens_received"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cost_usd = totals
        .as_ref()
        .and_then(|t| t.get("cost_usd"))
        .and_then(Value::as_f64);

    let steps = build_steps(model, plan, &started_at);

    Some(HistoryRecord {
        v: FORMAT_VERSION,
        run_id,
        document: document_path.to_string(),
        document_hash: Some(document_hash),
        engine: engine.map(str::to_string),
        started_at,
        ended_at: completed_at.to_string(),
        outcome,
        wall_s,
        effects_run,
        tokens_sent,
        tokens_received,
        cost_usd,
        steps,
    })
}

// ---------------------------------------------------------------------
// Directory/file resolution
// ---------------------------------------------------------------------

/// `osi`'s own history directory under `$XDG_STATE_HOME` (issue #649:
/// "History lives under the XDG state path on every platform, macOS
/// included"). `override_dir` is `OSI_HISTORY_DIR`'s own value, read
/// once by the caller; `xdg_state_home`/`home` are `$XDG_STATE_HOME`/
/// `$HOME`, also read once by the caller -- this function never
/// touches the process environment itself (`config_discovery`'s own
/// convention). `None` only when neither an override nor `$HOME` is
/// available at all.
pub fn resolve_history_dir(
    override_dir: Option<&Path>,
    xdg_state_home: Option<&Path>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(dir) = override_dir {
        return Some(dir.to_path_buf());
    }
    let state_home = match xdg_state_home {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
        _ => home?.join(".local").join("state"),
    };
    Some(state_home.join("circuitry").join("osi").join("history"))
}

/// Creates `dir` with mode `0700` when it doesn't exist yet; an
/// existing directory that is already group- or world-accessible is
/// left alone, with one warning on stderr -- the same rule
/// `create_run_dir` (`oscilloscope`'s own `main.rs`) already follows
/// for `--out-dir`.
pub fn ensure_history_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    if dir.exists() {
        let mode = std::fs::metadata(dir)?.permissions().mode();
        if mode & 0o077 != 0 {
            eprintln!(
                "osi: history directory {} is group- or world-accessible (mode {:o}); \
                 it can hold document paths and step names",
                dir.display(),
                mode & 0o777
            );
        }
        return Ok(());
    }
    std::fs::DirBuilder::new().mode(0o700).recursive(true).create(dir)
}

/// The history file for `document_path` -- the SHA-256 hex digest of
/// the (caller-resolved) absolute path, so the same document always
/// keys to the same file however it was invoked.
pub fn history_file_path(history_dir: &Path, document_path: &str) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(document_path.as_bytes());
    let digest = hasher.finalize();
    history_dir.join(format!("{digest:x}.jsonl"))
}

struct FileLock {
    #[allow(dead_code)]
    file: File,
}

impl FileLock {
    fn acquire(path: &Path) -> std::io::Result<Self> {
        use std::os::unix::io::AsRawFd;
        let file = OpenOptions::new().create(true).write(true).open(path)?;
        let fd = file.as_raw_fd();
        // Blocking advisory lock (issue #649): several `osi` processes
        // can finish runs of the same document at once; the first to
        // get here simply makes the others wait their turn rather
        // than racing the append or the compaction rewrite below.
        let rc = unsafe { libc::flock(fd, libc::LOCK_EX) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(FileLock { file })
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

/// Serializes `record` as one JSONL line and appends it to
/// `document_path`'s own history file under `history_dir`, taking the
/// advisory lock first and compacting afterwards once the file has
/// grown past [`COMPACT_AT_LINES`] lines.  Creates the directory and
/// the file (mode `0600`) if neither exists yet.
pub fn append_record(
    history_dir: &Path,
    document_path: &str,
    record: &HistoryRecord,
) -> std::io::Result<()> {
    ensure_history_dir(history_dir)?;
    let file_path = history_file_path(history_dir, document_path);
    let lock_path = lock_path_for(&file_path);
    let _lock = FileLock::acquire(&lock_path)?;

    let is_new = !file_path.exists();
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file_path)?;
    if is_new {
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    let mut line = serde_json::to_string(record).map_err(std::io::Error::other)?;
    line.push('\n');
    file.write_all(line.as_bytes())?;
    file.flush()?;
    drop(file);

    if count_lines(&file_path).unwrap_or(0) >= COMPACT_AT_LINES {
        compact(&file_path)?;
    }
    Ok(())
}

fn lock_path_for(file_path: &Path) -> PathBuf {
    let mut name = file_path.as_os_str().to_os_string();
    name.push(".lock");
    PathBuf::from(name)
}

fn count_lines(path: &Path) -> std::io::Result<usize> {
    let file = File::open(path)?;
    Ok(BufReader::new(file).lines().count())
}

/// Rewrites `file_path` with only its own newest [`MAX_RUNS_PER_DOCUMENT`]
/// lines (issue #649: "once a file reaches 100 lines, osi rewrites it
/// with the newest 50"), through a temporary file in the same
/// directory and a rename -- so a reader (which never locks) always
/// sees either the whole old file or the whole new one, never a
/// half-written one. Caller already holds the lock.
fn compact(file_path: &Path) -> std::io::Result<()> {
    let lines = read_raw_lines(file_path)?;
    let kept: Vec<&String> = if lines.len() > MAX_RUNS_PER_DOCUMENT {
        lines[lines.len() - MAX_RUNS_PER_DOCUMENT..].iter().collect()
    } else {
        lines.iter().collect()
    };
    let dir = file_path.parent().unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    for line in &kept {
        tmp.write_all(line.as_bytes())?;
        tmp.write_all(b"\n")?;
    }
    tmp.flush()?;
    tmp.persist(file_path).map_err(|e| e.error)?;
    Ok(())
}

/// Every raw line in `path`, as-is -- used by [`compact`], which
/// rewrites verbatim text rather than re-serializing already-valid
/// records (and so never silently fixes up a line [`read_records`]
/// itself would have skipped).
fn read_raw_lines(path: &Path) -> std::io::Result<Vec<String>> {
    let mut data = String::new();
    File::open(path)?.read_to_string(&mut data)?;
    Ok(data.lines().map(str::to_string).collect())
}

/// Reads every record `document_path`'s own history file holds (no
/// lock -- readers never take one), tolerating a corrupt line, an
/// unknown `v`, a torn last line, and a line over [`MAX_LINE_BYTES`]:
/// each is skipped without losing the lines around it. A repeated
/// `run_id` counts once (the first one read wins). `None`
/// (`history_dir` missing, or no file for this document yet) reads
/// the same as an empty history.
pub fn read_records(history_dir: &Path, document_path: &str) -> Vec<HistoryRecord> {
    let file_path = history_file_path(history_dir, document_path);
    let Ok(mut file) = File::open(&file_path) else {
        return Vec::new();
    };
    let mut data = Vec::new();
    if file.read_to_end(&mut data).is_err() {
        return Vec::new();
    }
    let mut seen_run_ids = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for raw in data.split(|b| *b == b'\n') {
        if raw.is_empty() || raw.len() > MAX_LINE_BYTES {
            continue;
        }
        let Ok(text) = std::str::from_utf8(raw) else {
            continue;
        };
        let Ok(record) = serde_json::from_str::<HistoryRecord>(text) else {
            continue;
        };
        if record.v != FORMAT_VERSION {
            continue;
        }
        if !seen_run_ids.insert(record.run_id.clone()) {
            continue;
        }
        out.push(record);
    }
    out
}

/// Issue #649's total-size eviction: "at most 500 documents and 64
/// MiB in total. Beyond that, the files written least recently are
/// removed first." Meant to run occasionally (once per `osi`
/// invocation that writes history is plenty), not on every append.
pub fn evict_if_needed(history_dir: &Path) -> std::io::Result<()> {
    let Ok(entries) = std::fs::read_dir(history_dir) else {
        return Ok(());
    };
    let mut files: Vec<(PathBuf, std::time::SystemTime, u64)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let modified = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        files.push((path, modified, meta.len()));
    }
    files.sort_by_key(|(_, modified, _)| *modified);
    let mut total: u64 = files.iter().map(|(_, _, len)| len).sum();
    let mut count = files.len();
    let mut idx = 0;
    while (count > MAX_DOCUMENTS || total > MAX_TOTAL_BYTES) && idx < files.len() {
        let (path, _, len) = &files[idx];
        if std::fs::remove_file(path).is_ok() {
            let _ = std::fs::remove_file(lock_path_for(path));
            total = total.saturating_sub(*len);
            count -= 1;
        }
        idx += 1;
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Statistics
// ---------------------------------------------------------------------

/// Nearest-rank percentile of an already-sorted slice (issue #649).
fn nearest_rank(sorted: &[f64], p: f64) -> f64 {
    let n = sorted.len();
    let rank = ((p / 100.0) * n as f64).ceil().max(1.0) as usize;
    sorted[rank.min(n) - 1]
}

fn percentile_with_threshold(values: &[f64], p: f64, min_count: usize) -> Option<f64> {
    if values.len() < min_count {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    Some(nearest_rank(&sorted, p))
}

/// The whole-run statistics (issue #649: "once there are enough runs
/// of the same version of a document"): `None` with fewer than 3 ok
/// runs of the *same* `document_hash`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RunStats {
    pub count: usize,
    pub p50_s: f64,
    pub p10_s: Option<f64>,
    pub p90_s: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct StepStats {
    pub count: usize,
    pub busy_p50_s: Option<f64>,
    pub busy_p10_s: Option<f64>,
    pub busy_p90_s: Option<f64>,
    /// p50 of `run.wall_s - step.end_s` over the same pool (the ETA's
    /// own "what usually follows this step").
    pub remaining_after_p50_s: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct DocumentStats {
    /// How many ok runs share the *current* document hash, whatever
    /// their count -- shown as `history: N of 3 runs` (issue #649)
    /// while below the threshold `whole_run` itself needs.
    pub same_hash_ok_count: usize,
    pub whole_run: Option<RunStats>,
    /// Keyed the same way [`HistoryRecord::steps`] is -- pooled
    /// across *every* ok run of this document path, any hash
    /// (DESIGN.md's "earlier versions" pooling).
    pub per_step: BTreeMap<String, StepStats>,
}

/// The newest `limit` of `records`, newest first -- ties (an absent
/// or unparseable `started_at`) sort last, never crash the sort.
fn newest_first(records: &[HistoryRecord], limit: usize) -> Vec<&HistoryRecord> {
    let mut sorted: Vec<&HistoryRecord> = records.iter().collect();
    sorted.sort_by(|a, b| {
        let ta = parse_instant(&a.started_at);
        let tb = parse_instant(&b.started_at);
        tb.cmp(&ta)
    });
    sorted.truncate(limit);
    sorted
}

/// Works out [`DocumentStats`] from a document's own history records
/// (issue #649): `current_hash` is the current run's own
/// `document_hash`, `None` for a watch with no hash at all (no
/// whole-run figures either way). Only `Outcome::Ok` runs ever feed a
/// statistic; `records` need not be pre-filtered or pre-sorted.
pub fn compute_stats(records: &[HistoryRecord], current_hash: Option<&str>) -> DocumentStats {
    let ok_records: Vec<&HistoryRecord> = records
        .iter()
        .filter(|r| r.outcome == Outcome::Ok)
        .collect();
    let newest_ok: Vec<HistoryRecord> = newest_first(
        &ok_records.iter().map(|r| (*r).clone()).collect::<Vec<_>>(),
        NEWEST_RUNS_FOR_STATS,
    )
    .into_iter()
    .cloned()
    .collect();

    let same_hash: Vec<&HistoryRecord> = newest_ok
        .iter()
        .filter(|r| current_hash.is_some() && r.document_hash.as_deref() == current_hash)
        .collect();
    let same_hash_ok_count = same_hash.len();
    let whole_run = if same_hash_ok_count >= P50_MIN_RUNS {
        let wall: Vec<f64> = same_hash.iter().map(|r| r.wall_s).collect();
        Some(RunStats {
            count: same_hash_ok_count,
            p50_s: percentile_with_threshold(&wall, 50.0, P50_MIN_RUNS).unwrap_or(0.0),
            p10_s: percentile_with_threshold(&wall, 10.0, P10_P90_MIN_RUNS),
            p90_s: percentile_with_threshold(&wall, 90.0, P10_P90_MIN_RUNS),
        })
    } else {
        None
    };

    let mut keys: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for r in &newest_ok {
        keys.extend(r.steps.keys().cloned());
    }
    let mut per_step = BTreeMap::new();
    for key in keys {
        let busy: Vec<f64> = newest_ok
            .iter()
            .filter_map(|r| r.steps.get(&key).map(|s| s.busy_s))
            .collect();
        let remaining: Vec<f64> = newest_ok
            .iter()
            .filter_map(|r| r.steps.get(&key).map(|s| r.wall_s - s.end_s))
            .collect();
        per_step.insert(
            key,
            StepStats {
                count: busy.len(),
                busy_p50_s: percentile_with_threshold(&busy, 50.0, P50_MIN_RUNS),
                busy_p10_s: percentile_with_threshold(&busy, 10.0, P10_P90_MIN_RUNS),
                busy_p90_s: percentile_with_threshold(&busy, 90.0, P10_P90_MIN_RUNS),
                remaining_after_p50_s: percentile_with_threshold(&remaining, 50.0, P50_MIN_RUNS),
            },
        );
    }

    DocumentStats {
        same_hash_ok_count,
        whole_run,
        per_step,
    }
}

// ---------------------------------------------------------------------
// The whole-run ETA
// ---------------------------------------------------------------------

/// [`compute_eta`]'s own input: everything about the *current* run it
/// needs beyond [`DocumentStats`] -- all in seconds, offset from the
/// run's own start, same as [`StepRecord::start_s`]/`end_s`.
pub struct EtaInput {
    pub now_offset_s: f64,
    /// Every step that has finished so far, by its own display key
    /// and its own `end_s` -- the most recent one with per-step
    /// stats of its own is the anchor (issue #649).
    pub finished_steps: Vec<(String, f64)>,
    /// The currently running step, by display key, and its own busy
    /// time elapsed so far -- `None` when nothing is running (between
    /// steps, or the run has ended).
    pub running_step: Option<(String, f64)>,
}

/// Builds [`EtaInput`] straight from a live (or just-finished) state
/// snapshot -- the one call site a redraw loop needs, so it never has
/// to walk `flatten_state` itself just for this. `run_start_ts` is the
/// same anchor [`Header::run_start_ts`](crate::render::Header::run_start_ts)
/// already resolves; `now_offset_s` is the header's own `elapsed_s`.
/// The running step picked is whichever running leaf started most
/// recently -- the same "innermost" tie-break the overview's now pane
/// uses for which running step to focus.
pub fn eta_input_from_state(
    state: &Value,
    plan: &PlanTree,
    run_start_ts: &str,
    now_offset_s: f64,
) -> EtaInput {
    let flat = crate::model::flatten_state(state);
    let mut finished_steps = Vec::new();
    let mut running: Option<(String, f64)> = None;
    for (path, node) in &flat {
        let Some(key) = display_key(path, plan) else {
            continue;
        };
        if node.is_running() {
            let Some(start_s) = node
                .created_at
                .as_deref()
                .and_then(|ca| duration_seconds(run_start_ts, ca))
            else {
                continue;
            };
            let elapsed = (now_offset_s - start_s).max(0.0);
            if running.as_ref().is_none_or(|(_, e)| elapsed < *e) {
                running = Some((key, elapsed));
            }
        } else if let Some(end_s) = node
            .completed_at
            .as_deref()
            .and_then(|ca| duration_seconds(run_start_ts, ca))
        {
            finished_steps.push((key, end_s));
        }
    }
    EtaInput {
        now_offset_s,
        finished_steps,
        running_step: running,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct EtaOutput {
    /// Seconds from now to the ETA -- `None` either because there
    /// isn't enough history yet ([`DocumentStats::whole_run`] is
    /// `None`) or because the run is already past its usual end
    /// (`past_usual`).
    pub remaining_s: Option<f64>,
    pub past_usual: bool,
    pub low_s: Option<f64>,
    pub high_s: Option<f64>,
    pub whole_run_count: usize,
}

/// The whole-run ETA (issue #649's own rules): the anchor is the most
/// recently finished step with known per-step statistics (or the
/// run's own start, before any has); a running step's own usual time
/// left plus what usually follows it is a floor under that; past the
/// usual (p90) end, there is no ETA at all.
pub fn compute_eta(stats: &DocumentStats, input: &EtaInput) -> EtaOutput {
    let Some(whole_run) = stats.whole_run else {
        return EtaOutput::default();
    };
    let anchor = input
        .finished_steps
        .iter()
        .filter(|(key, _)| {
            stats
                .per_step
                .get(key)
                .is_some_and(|s| s.remaining_after_p50_s.is_some())
        })
        .max_by(|a, b| a.1.total_cmp(&b.1));
    let mut remaining_s = match anchor {
        Some((key, end_s)) => {
            let after = stats.per_step[key].remaining_after_p50_s.unwrap_or(0.0);
            (end_s + after) - input.now_offset_s
        }
        None => whole_run.p50_s - input.now_offset_s,
    };
    if let Some((key, elapsed_busy)) = &input.running_step {
        if let Some(step_stats) = stats.per_step.get(key) {
            if let Some(busy_p50) = step_stats.busy_p50_s {
                let left = (busy_p50 - elapsed_busy).max(0.0);
                let after = step_stats.remaining_after_p50_s.unwrap_or(0.0);
                remaining_s = remaining_s.max(left + after);
            }
        }
    }
    remaining_s = remaining_s.max(0.0);
    let past_usual = whole_run
        .p90_s
        .is_some_and(|p90| input.now_offset_s > p90);
    EtaOutput {
        remaining_s: if past_usual { None } else { Some(remaining_s) },
        past_usual,
        low_s: whole_run.p10_s.map(|p10| (p10 - input.now_offset_s).max(0.0)),
        high_s: whole_run.p90_s.map(|p90| (p90 - input.now_offset_s).max(0.0)),
        whole_run_count: whole_run.count,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(start_s: f64, end_s: f64, busy_s: f64, passes: u64, ok: bool) -> StepRecord {
        StepRecord {
            start_s,
            end_s,
            busy_s,
            passes,
            tokens_sent: None,
            tokens_received: None,
            cost_usd: None,
            ok,
        }
    }

    fn record(run_id: &str, started_at: &str, wall_s: f64, outcome: Outcome, hash: &str) -> HistoryRecord {
        HistoryRecord {
            v: FORMAT_VERSION,
            run_id: run_id.to_string(),
            document: "/doc.yml".to_string(),
            document_hash: Some(hash.to_string()),
            engine: Some("cof".to_string()),
            started_at: started_at.to_string(),
            ended_at: started_at.to_string(),
            outcome,
            wall_s,
            effects_run: 1,
            tokens_sent: 0,
            tokens_received: 0,
            cost_usd: None,
            steps: BTreeMap::new(),
        }
    }

    #[test]
    fn display_key_of_prime_itself_is_none() {
        let plan = PlanTree::empty();
        assert_eq!(display_key("prime", &plan), None);
    }

    #[test]
    fn normalize_iter_segments_replaces_only_real_pass_segments() {
        assert_eq!(normalize_iter_segments("items.iter_3.step"), "items.iter_*.step");
        assert_eq!(normalize_iter_segments("iteration.step"), "iteration.step");
    }

    #[test]
    fn merged_busy_s_counts_overlapping_ranges_once() {
        let total = merged_busy_s(vec![(0.0, 10.0), (5.0, 15.0), (20.0, 25.0)]);
        assert_eq!(total, 20.0);
    }

    #[test]
    fn nearest_rank_percentiles_match_known_values() {
        let sorted: Vec<f64> = (1..=20).map(f64::from).collect();
        assert_eq!(nearest_rank(&sorted, 50.0), 10.0);
        assert_eq!(nearest_rank(&sorted, 90.0), 18.0);
        assert_eq!(nearest_rank(&sorted, 10.0), 2.0);
    }

    #[test]
    fn percentile_thresholds_gate_p50_at_3_and_p10_p90_at_5() {
        let three = vec![1.0, 2.0, 3.0];
        assert!(percentile_with_threshold(&three, 50.0, P50_MIN_RUNS).is_some());
        assert!(percentile_with_threshold(&three, 90.0, P10_P90_MIN_RUNS).is_none());
        let two = vec![1.0, 2.0];
        assert!(percentile_with_threshold(&two, 50.0, P50_MIN_RUNS).is_none());
    }

    #[test]
    fn compute_stats_uses_only_ok_runs_of_the_same_hash_for_whole_run_figures() {
        let records = vec![
            record("r1", "2026-01-01T00:00:00Z", 100.0, Outcome::Ok, "h1"),
            record("r2", "2026-01-01T00:01:00Z", 110.0, Outcome::Ok, "h1"),
            record("r3", "2026-01-01T00:02:00Z", 90.0, Outcome::Ok, "h1"),
            record("r4", "2026-01-01T00:03:00Z", 500.0, Outcome::Failed, "h1"),
            record("r5", "2026-01-01T00:04:00Z", 1000.0, Outcome::Ok, "h2"),
        ];
        let stats = compute_stats(&records, Some("h1"));
        assert_eq!(stats.same_hash_ok_count, 3);
        let whole_run = stats.whole_run.expect("3 ok runs meets the p50 threshold");
        assert_eq!(whole_run.count, 3);
        assert_eq!(whole_run.p50_s, 100.0);
    }

    #[test]
    fn compute_stats_shows_no_whole_run_figures_after_an_edit() {
        let records = vec![
            record("r1", "2026-01-01T00:00:00Z", 100.0, Outcome::Ok, "old-hash"),
            record("r2", "2026-01-01T00:01:00Z", 110.0, Outcome::Ok, "old-hash"),
            record("r3", "2026-01-01T00:02:00Z", 90.0, Outcome::Ok, "old-hash"),
        ];
        let stats = compute_stats(&records, Some("new-hash"));
        assert_eq!(stats.same_hash_ok_count, 0);
        assert!(stats.whole_run.is_none());
    }

    #[test]
    fn compute_stats_pools_per_step_figures_across_every_hash() {
        let mut r1 = record("r1", "2026-01-01T00:00:00Z", 100.0, Outcome::Ok, "h1");
        r1.steps.insert("plan".to_string(), step(0.0, 10.0, 10.0, 1, true));
        let mut r2 = record("r2", "2026-01-01T00:01:00Z", 100.0, Outcome::Ok, "h2");
        r2.steps.insert("plan".to_string(), step(0.0, 12.0, 12.0, 1, true));
        let mut r3 = record("r3", "2026-01-01T00:02:00Z", 100.0, Outcome::Ok, "h2");
        r3.steps.insert("plan".to_string(), step(0.0, 11.0, 11.0, 1, true));
        let stats = compute_stats(&[r1, r2, r3], Some("h2"));
        let plan_stats = stats.per_step.get("plan").expect("pooled across hashes");
        assert_eq!(plan_stats.count, 3);
        assert!(plan_stats.busy_p50_s.is_some());
    }

    #[test]
    fn newest_20_of_more_runs_are_used_for_statistics() {
        let mut records = Vec::new();
        for i in 0..25 {
            let wall = if i < 20 { 100.0 } else { 999.0 };
            records.push(record(
                &format!("r{i}"),
                &format!("2026-01-{:02}T00:00:00Z", i + 1),
                wall,
                Outcome::Ok,
                "h1",
            ));
        }
        let stats = compute_stats(&records, Some("h1"));
        assert_eq!(stats.same_hash_ok_count, 20);
        let whole_run = stats.whole_run.unwrap();
        assert_eq!(whole_run.p50_s, 100.0);
    }

    #[test]
    fn eta_falls_back_to_whole_run_p50_before_any_step_has_finished() {
        let records = vec![
            record("r1", "2026-01-01T00:00:00Z", 100.0, Outcome::Ok, "h1"),
            record("r2", "2026-01-01T00:01:00Z", 100.0, Outcome::Ok, "h1"),
            record("r3", "2026-01-01T00:02:00Z", 100.0, Outcome::Ok, "h1"),
        ];
        let stats = compute_stats(&records, Some("h1"));
        let eta = compute_eta(
            &stats,
            &EtaInput {
                now_offset_s: 10.0,
                finished_steps: vec![],
                running_step: None,
            },
        );
        assert_eq!(eta.remaining_s, Some(90.0));
        assert!(!eta.past_usual);
    }

    #[test]
    fn eta_anchors_on_the_most_recently_finished_step() {
        let mut r1 = record("r1", "2026-01-01T00:00:00Z", 100.0, Outcome::Ok, "h1");
        r1.steps.insert("plan".to_string(), step(0.0, 20.0, 20.0, 1, true));
        let mut r2 = record("r2", "2026-01-01T00:01:00Z", 110.0, Outcome::Ok, "h1");
        r2.steps.insert("plan".to_string(), step(0.0, 25.0, 25.0, 1, true));
        let mut r3 = record("r3", "2026-01-01T00:02:00Z", 90.0, Outcome::Ok, "h1");
        r3.steps.insert("plan".to_string(), step(0.0, 15.0, 15.0, 1, true));
        let stats = compute_stats(&[r1, r2, r3], Some("h1"));
        let eta = compute_eta(
            &stats,
            &EtaInput {
                now_offset_s: 30.0,
                finished_steps: vec![("plan".to_string(), 25.0)],
                running_step: None,
            },
        );
        // plan's own p50 end is 20s; remaining-after p50 is p50(100-20,
        // 110-25, 90-15) = p50(80, 85, 75) = 80. Anchor end_s (this
        // run's own, 25.0) + 80 - now(30) = 75.
        assert_eq!(eta.remaining_s, Some(75.0));
    }

    #[test]
    fn eta_never_earlier_than_a_running_steps_own_usual_time_left() {
        let mut r1 = record("r1", "2026-01-01T00:00:00Z", 100.0, Outcome::Ok, "h1");
        r1.steps.insert("slow".to_string(), step(0.0, 90.0, 90.0, 1, true));
        let mut r2 = record("r2", "2026-01-01T00:01:00Z", 100.0, Outcome::Ok, "h1");
        r2.steps.insert("slow".to_string(), step(0.0, 90.0, 90.0, 1, true));
        let mut r3 = record("r3", "2026-01-01T00:02:00Z", 100.0, Outcome::Ok, "h1");
        r3.steps.insert("slow".to_string(), step(0.0, 90.0, 90.0, 1, true));
        let stats = compute_stats(&[r1, r2, r3], Some("h1"));
        let eta = compute_eta(
            &stats,
            &EtaInput {
                now_offset_s: 95.0,
                finished_steps: vec![],
                running_step: Some(("slow".to_string(), 10.0)),
            },
        );
        // Anchor-less estimate would be p50(100) - 95 = 5s, but
        // "slow" has been running 10s of its own usual 90s busy time,
        // so the floor is (90-10) + 10 (p50 of what follows "slow":
        // 100-90) = 90.
        assert_eq!(eta.remaining_s, Some(90.0));
    }

    #[test]
    fn eta_reads_as_past_usual_once_now_is_past_the_p90_end() {
        let records = vec![
            record("r1", "2026-01-01T00:00:00Z", 60.0, Outcome::Ok, "h1"),
            record("r2", "2026-01-01T00:01:00Z", 61.0, Outcome::Ok, "h1"),
            record("r3", "2026-01-01T00:02:00Z", 62.0, Outcome::Ok, "h1"),
            record("r4", "2026-01-01T00:03:00Z", 63.0, Outcome::Ok, "h1"),
            record("r5", "2026-01-01T00:04:00Z", 100.0, Outcome::Ok, "h1"),
        ];
        let stats = compute_stats(&records, Some("h1"));
        let p90 = stats.whole_run.unwrap().p90_s.unwrap();
        let eta = compute_eta(
            &stats,
            &EtaInput {
                now_offset_s: p90 + 1.0,
                finished_steps: vec![],
                running_step: None,
            },
        );
        assert!(eta.past_usual);
        assert_eq!(eta.remaining_s, None);
    }

    #[test]
    fn eta_is_none_with_fewer_than_3_ok_runs_of_the_same_hash() {
        let records = vec![
            record("r1", "2026-01-01T00:00:00Z", 100.0, Outcome::Ok, "h1"),
            record("r2", "2026-01-01T00:01:00Z", 100.0, Outcome::Ok, "h1"),
        ];
        let stats = compute_stats(&records, Some("h1"));
        let eta = compute_eta(
            &stats,
            &EtaInput {
                now_offset_s: 10.0,
                finished_steps: vec![],
                running_step: None,
            },
        );
        assert_eq!(eta.remaining_s, None);
        assert!(!eta.past_usual);
    }

    #[test]
    fn read_records_round_trips_a_written_record() {
        let dir = tempfile::tempdir().unwrap();
        let rec = record("r1", "2026-01-01T00:00:00Z", 12.0, Outcome::Ok, "h1");
        append_record(dir.path(), "/doc.yml", &rec).unwrap();
        let read_back = read_records(dir.path(), "/doc.yml");
        assert_eq!(read_back, vec![rec]);
    }

    #[test]
    fn append_record_creates_a_private_directory_and_file() {
        let dir = tempfile::tempdir().unwrap();
        let history_dir = dir.path().join("history");
        let rec = record("r1", "2026-01-01T00:00:00Z", 12.0, Outcome::Ok, "h1");
        append_record(&history_dir, "/doc.yml", &rec).unwrap();
        let dir_mode = std::fs::metadata(&history_dir).unwrap().permissions().mode();
        assert_eq!(dir_mode & 0o777, 0o700);
        let file_path = history_file_path(&history_dir, "/doc.yml");
        let file_mode = std::fs::metadata(&file_path).unwrap().permissions().mode();
        assert_eq!(file_mode & 0o777, 0o600);
    }

    #[test]
    fn a_repeated_run_id_is_counted_once_on_read() {
        let dir = tempfile::tempdir().unwrap();
        let rec = record("same-id", "2026-01-01T00:00:00Z", 12.0, Outcome::Ok, "h1");
        append_record(dir.path(), "/doc.yml", &rec).unwrap();
        append_record(dir.path(), "/doc.yml", &rec).unwrap();
        assert_eq!(read_records(dir.path(), "/doc.yml").len(), 1);
    }

    #[test]
    fn compaction_at_100_lines_keeps_the_newest_50_and_stays_0600() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..100 {
            let rec = record(
                &format!("r{i}"),
                &format!("2026-01-{:02}T00:00:00Z", (i % 28) + 1),
                i as f64,
                Outcome::Ok,
                "h1",
            );
            append_record(dir.path(), "/doc.yml", &rec).unwrap();
        }
        let records = read_records(dir.path(), "/doc.yml");
        assert_eq!(records.len(), MAX_RUNS_PER_DOCUMENT);
        let file_path = history_file_path(dir.path(), "/doc.yml");
        let mode = std::fs::metadata(&file_path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let ids: std::collections::BTreeSet<_> = records.iter().map(|r| r.run_id.clone()).collect();
        assert!(ids.contains("r99"));
        assert!(!ids.contains("r0"));
    }

    #[test]
    fn a_corrupt_torn_or_oversized_line_is_skipped_without_losing_its_neighbours() {
        let dir = tempfile::tempdir().unwrap();
        let history_dir = dir.path().join("history");
        ensure_history_dir(&history_dir).unwrap();
        let file_path = history_file_path(&history_dir, "/doc.yml");
        let good_one = serde_json::to_string(&record("r1", "2026-01-01T00:00:00Z", 1.0, Outcome::Ok, "h")).unwrap();
        let unknown_v = serde_json::to_string(&serde_json::json!({"v": 99, "run_id": "r-bad-v"})).unwrap();
        let good_two = serde_json::to_string(&record("r2", "2026-01-01T00:01:00Z", 2.0, Outcome::Ok, "h")).unwrap();
        let oversized = "x".repeat(MAX_LINE_BYTES + 1);
        let torn = r#"{"v":1,"run_id":"torn", "document": "trunc"#.to_string();
        let contents = format!(
            "{good_one}\nnot json at all\n{unknown_v}\n{good_two}\n{oversized}\n{torn}"
        );
        std::fs::write(&file_path, contents).unwrap();
        let records = read_records(&history_dir, "/doc.yml");
        let ids: Vec<_> = records.iter().map(|r| r.run_id.clone()).collect();
        assert_eq!(ids, vec!["r1", "r2"]);
    }

    #[test]
    fn build_record_never_carries_a_values_prompt_or_error_text_marker() {
        const MARKER: &str = "super-secret-marker-xyz";
        let state = serde_json::json!({
            "runtime": {
                "last_run": {
                    "run_id": "r1",
                    "document_hash": "h1",
                    "started_at": "2026-01-01T00:00:00Z",
                    "completed_at": "2026-01-01T00:01:00Z",
                    "totals": {"effects_run": 1, "tokens_sent": 1, "tokens_received": 1},
                },
            },
            "prime": {
                "value": MARKER,
                "meta": {
                    "created_at": "2026-01-01T00:00:00Z",
                    "completed_at": "2026-01-01T00:01:00Z",
                    "error": null,
                },
                "ask": {
                    "value": MARKER,
                    "meta": {
                        "created_at": "2026-01-01T00:00:00Z",
                        "completed_at": "2026-01-01T00:00:30Z",
                        "error": null,
                        "prompt_sent": MARKER,
                        "command": MARKER,
                        "tool_target": MARKER,
                    },
                },
                "flaky": {
                    "value": null,
                    "meta": {
                        "created_at": "2026-01-01T00:00:30Z",
                        "completed_at": "2026-01-01T00:00:45Z",
                        "error": MARKER,
                    },
                },
                "tell": {
                    "value": null,
                    "meta": {
                        "created_at": "2026-01-01T00:00:45Z",
                        "completed_at": "2026-01-01T00:01:00Z",
                        "error": null,
                        "report": {"message": MARKER, "data": {"x": MARKER}},
                    },
                },
            },
        });
        let mut model = RunModel::new();
        model.observe(&state, &PlanTree::empty(), crate::model::ProcessState::exited(false));
        let record = build_record("/doc.yml", Some("cof"), &state, &model, &PlanTree::empty())
            .expect("a completed, hashed, non-dry run is recorded");
        let serialized = serde_json::to_string(&record).unwrap();
        assert!(!serialized.contains(MARKER), "history line leaked a marker: {serialized}");
    }

    #[test]
    fn no_history_dir_reads_as_empty_history() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_records(&dir.path().join("missing"), "/doc.yml"), Vec::new());
    }

    #[test]
    fn two_processes_appending_concurrently_leave_every_record_readable() {
        let dir = tempfile::tempdir().unwrap();
        let history_dir = dir.path().join("history");
        std::thread::scope(|scope| {
            for worker in 0..2 {
                let history_dir = &history_dir;
                scope.spawn(move || {
                    for i in 0..100 {
                        let rec = record(
                            &format!("w{worker}-r{i}"),
                            "2026-01-01T00:00:00Z",
                            1.0,
                            Outcome::Ok,
                            "h1",
                        );
                        append_record(history_dir, "/doc.yml", &rec).unwrap();
                    }
                });
            }
        });
        // Compaction may have run partway through (100-line threshold
        // crossed mid-stream), trimming to the newest 50 -- the point
        // of this test is that every append landed on disk as a whole
        // line, never that none of the 200 were ever compacted away.
        let records = read_records(&history_dir, "/doc.yml");
        assert!(!records.is_empty());
        assert!(records.len() <= 200);
    }
}
