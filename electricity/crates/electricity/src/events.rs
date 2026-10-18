//! `--events`: `cli/events.py::EventLog`, ported (issue #431's
//! "`--events`" section; `electricity/docs/spec/runtime-semantics.md`
//! §8.7 has the wire format). One JSONL line per event, flushed at
//! once; a write failure disables every further write and is folded
//! into one run-level warning rather than ever failing the run.
//!
//! Single-threaded (electricity runs concurrent tree/loop branches as
//! tasks on one OS thread, never a worker thread of their own --
//! DESIGN.md §6.1-6.2), so `cli/events.py`'s own per-thread `start`/
//! `end` pairing stack narrows to one stack per path here rather than
//! one per `threading.local()` -- but, unlike Python's thread-affined
//! stack, two *concurrent* calls at the same path (an unnamed tree
//! branch, or a `flow: tree` loop's own concurrent passes, both
//! sharing one path until `electricity_vm::Frame` disambiguates them)
//! can both be open on this single stack at once, so `on_complete`
//! removes *its own* [`EventLog::on_start`] id from wherever it sits
//! on the path's own stack (review finding 3 on PR #454), never just
//! the top -- a bare LIFO `pop` paired the wrong end with the wrong
//! start whenever two such calls overlapped (`id`s came out reversed).

use electricity_value::{Dict, Value};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use std::time::Instant;

/// An effect/run error's text is cut to this many characters before it
/// reaches the stream -- `cli/events.py::_ERROR_MAX_CHARS`.
const ERROR_MAX_CHARS: usize = 500;

/// A `report` event's compact `data` JSON, in bytes, above which it is
/// replaced by `"data_truncated": <that byte count>` instead (#605) --
/// `cli/events.py::_REPORT_DATA_MAX_BYTES`.
const REPORT_DATA_MAX_BYTES: usize = 16 * 1024;

fn truncate_error(error: &str) -> String {
    error.chars().take(ERROR_MAX_CHARS).collect()
}

fn now_iso_ms() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

/// `cli/events.py::_engine_label` -- `"electricity <version>"` (issue
/// #431's "Events engine label" decision; `cof`'s own label is `"cof
/// <version>"`, not reused here since the two engines are never
/// confused for one another in an event stream).
fn engine_label() -> String {
    format!("electricity {}", crate::VERSION)
}

/// Writes one JSONL line per `run_start`/`dispatch`/`start`/`end`/
/// `run_end` event to its own path, opened once at construction
/// (create-or-truncate, parent directory made first).
pub struct EventLog {
    file: RefCell<Option<File>>,
    seq: Cell<u64>,
    next_instance_id: Cell<u64>,
    /// One stack per effect path, the ids (and start `Instant`) still
    /// open on it -- `cli/events.py`'s own per-thread stack, narrowed to
    /// one thread (this module's own doc comment); [`EventLog::on_complete`]
    /// removes its own matching id from anywhere in the `Vec`, not just
    /// the end, so a path with more than one call open at once still
    /// pairs each `end` with its *own* `start`.
    stacks: RefCell<HashMap<String, Vec<(u64, Instant)>>>,
    disabled: Cell<bool>,
}

impl EventLog {
    /// Opens *path* for writing. A failure to open it disables every
    /// further write from the start (`cli/events.py::EventLog.__init__`'s
    /// own `except OSError` branch) -- this never fails to construct;
    /// [`EventLog::close`] is how a caller learns whether that happened.
    pub fn open(path: &Path) -> Self {
        let log = EventLog {
            file: RefCell::new(None),
            seq: Cell::new(0),
            next_instance_id: Cell::new(0),
            stacks: RefCell::new(HashMap::new()),
            disabled: Cell::new(false),
        };
        let opened = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(fs::create_dir_all)
            .unwrap_or(Ok(()))
            .and_then(|()| File::create(path));
        match opened {
            Ok(file) => *log.file.borrow_mut() = Some(file),
            Err(err) => log.disable_with("open", &err),
        }
        log
    }

    /// `cli/events.py::EventLog._disable`'s own `if not self._disabled:
    /// logger.warning("--events %s failed, disabling further writes: %s",
    /// operation, exc)` -- logged once, for whichever operation first
    /// failed (issue #442); every later failure still disables (if it
    /// hasn't already) but never logs a second line.
    fn disable_with(&self, op: &str, err: &dyn std::fmt::Display) {
        if !self.disabled.get() {
            log::warn!("--events {op} failed, disabling further writes: {err}");
        }
        self.disabled.set(true);
    }

    fn take_seq(&self) -> u64 {
        let seq = self.seq.get();
        self.seq.set(seq + 1);
        seq
    }

    fn write_line(&self, op: &str, payload: Value) {
        if self.disabled.get() {
            return;
        }
        // `dumps_default_str`, not `dumps` (#605): a
        // report's own `data` can hold a shadow-state value plain JSON
        // can't otherwise encode (a YAML date, `!!binary` bytes), the same
        // as `--out`. Without it, encoding this one line disables every
        // later write on this stream for the rest of the run -- a watcher
        // would silently lose every subsequent `start`/`end`/`run_end`
        // too, not just this report.
        let text = match electricity_json::dumps_default_str(
            &payload,
            electricity_json::WriteMode::EVENTS,
        ) {
            Ok(text) => text,
            Err(err) => {
                self.disable_with(op, &err);
                return;
            }
        };
        let mut file_slot = self.file.borrow_mut();
        let Some(file) = file_slot.as_mut() else {
            self.disable_with(op, &"the file is not open");
            return;
        };
        if let Err(err) = writeln!(file, "{text}") {
            self.disable_with(op, &err);
        } else if let Err(err) = file.flush() {
            self.disable_with(op, &err);
        }
    }

    pub fn run_start(&self, run_id: &str, orchestration: &str) {
        if self.disabled.get() {
            return;
        }
        let mut payload = Dict::new();
        payload.insert(Value::Str("v".to_string()), Value::from(1i64));
        payload.insert(
            Value::Str("seq".to_string()),
            Value::from(self.take_seq() as i64),
        );
        payload.insert(Value::Str("ts".to_string()), Value::Str(now_iso_ms()));
        payload.insert(
            Value::Str("ev".to_string()),
            Value::Str("run_start".to_string()),
        );
        payload.insert(
            Value::Str("run_id".to_string()),
            Value::Str(run_id.to_string()),
        );
        payload.insert(
            Value::Str("orchestration".to_string()),
            Value::Str(orchestration.to_string()),
        );
        payload.insert(Value::Str("engine".to_string()), Value::Str(engine_label()));
        payload.insert(
            Value::Str("pid".to_string()),
            Value::from(std::process::id() as i64),
        );
        self.write_line("run_start", Value::Dict(payload));
    }

    pub fn on_dispatch(&self, path: &str, branches: usize, concurrency: usize) {
        if self.disabled.get() {
            return;
        }
        let mut payload = Dict::new();
        payload.insert(Value::Str("v".to_string()), Value::from(1i64));
        payload.insert(
            Value::Str("seq".to_string()),
            Value::from(self.take_seq() as i64),
        );
        payload.insert(Value::Str("ts".to_string()), Value::Str(now_iso_ms()));
        payload.insert(
            Value::Str("ev".to_string()),
            Value::Str("dispatch".to_string()),
        );
        payload.insert(Value::Str("path".to_string()), Value::Str(path.to_string()));
        payload.insert(
            Value::Str("branches".to_string()),
            Value::from(branches as i64),
        );
        payload.insert(
            Value::Str("concurrency".to_string()),
            Value::from(concurrency as i64),
        );
        self.write_line("dispatch", Value::Dict(payload));
    }

    /// Returns the id this call was assigned -- the one
    /// [`EventLog::on_complete`] must be given back, so the matching
    /// `end` line still reports it even when another call at the same
    /// path starts (or finishes) in between (review finding 3 on PR
    /// #454). `0` when this log is already disabled: a caller still
    /// holding a token from a now-disabled log hits [`EventLog::on_complete`]'s
    /// own early return before that token is ever looked up again.
    pub fn on_start(&self, path: &str) -> u64 {
        if self.disabled.get() {
            return 0;
        }
        let instance_id = self.next_instance_id.get();
        self.next_instance_id.set(instance_id + 1);
        self.stacks
            .borrow_mut()
            .entry(path.to_string())
            .or_default()
            .push((instance_id, Instant::now()));
        let mut payload = Dict::new();
        payload.insert(Value::Str("v".to_string()), Value::from(1i64));
        payload.insert(
            Value::Str("seq".to_string()),
            Value::from(self.take_seq() as i64),
        );
        payload.insert(Value::Str("ts".to_string()), Value::Str(now_iso_ms()));
        payload.insert(
            Value::Str("ev".to_string()),
            Value::Str("start".to_string()),
        );
        payload.insert(
            Value::Str("id".to_string()),
            Value::from(instance_id as i64),
        );
        payload.insert(Value::Str("path".to_string()), Value::Str(path.to_string()));
        self.write_line("start", Value::Dict(payload));
        instance_id
    }

    /// `path` finished, with `error` the text that became (or would
    /// have become) its `meta.error`, `None` on success. *id* is the
    /// value the matching [`EventLog::on_start`] call returned --
    /// removed from *path*'s own stack by that exact id, not by popping
    /// whatever happens to be on top (review finding 3 on PR #454: a
    /// bare `Vec::pop` pairs the wrong start with the wrong end once
    /// two calls at the same path overlap, e.g. an unnamed `flow: tree`
    /// loop's own concurrent passes). A completion with no matching
    /// start on this path's own stack (a double-fire, or a call this
    /// instance never saw the start of) is not an error: `id: null` and
    /// no `ms`, same as `cli/events.py::EventLog.on_complete`.
    pub fn on_complete(&self, path: &str, id: u64, error: Option<&str>) {
        if self.disabled.get() {
            return;
        }
        let popped = {
            let mut stacks = self.stacks.borrow_mut();
            stacks.get_mut(path).and_then(|stack| {
                let position = stack.iter().position(|&(entry_id, _)| entry_id == id)?;
                Some(stack.remove(position))
            })
        };
        let (id_value, ms_value) = match popped {
            Some((instance_id, started_at)) => {
                let ms = started_at.elapsed().as_millis().min(i64::MAX as u128) as i64;
                (Value::from(instance_id as i64), Some(ms))
            }
            None => (Value::None, None),
        };
        let mut payload = Dict::new();
        payload.insert(Value::Str("v".to_string()), Value::from(1i64));
        payload.insert(
            Value::Str("seq".to_string()),
            Value::from(self.take_seq() as i64),
        );
        payload.insert(Value::Str("ts".to_string()), Value::Str(now_iso_ms()));
        payload.insert(Value::Str("ev".to_string()), Value::Str("end".to_string()));
        payload.insert(Value::Str("id".to_string()), id_value);
        payload.insert(Value::Str("path".to_string()), Value::Str(path.to_string()));
        payload.insert(Value::Str("ok".to_string()), Value::Bool(error.is_none()));
        if let Some(ms) = ms_value {
            payload.insert(Value::Str("ms".to_string()), Value::from(ms));
        }
        if let Some(error) = error {
            payload.insert(
                Value::Str("error".to_string()),
                Value::Str(truncate_error(error)),
            );
        }
        self.write_line("end", Value::Dict(payload));
    }

    /// One `report` line (#605): `v`, `seq`, `ts`, `ev`, `path`,
    /// `level`, `message`, then `data` (omitted when *data* is `None`)
    /// or `data_truncated` (the compact JSON's byte count) when that
    /// JSON exceeds 16 KiB -- a named report still keeps the full
    /// value in the shadow state, only this line is capped. Ports
    /// `cli/events.py::EventLog.on_report`.
    pub fn on_report(&self, path: &str, level: &str, message: &str, data: Option<&Value>) {
        if self.disabled.get() {
            return;
        }
        let mut payload = Dict::new();
        payload.insert(Value::Str("v".to_string()), Value::from(1i64));
        payload.insert(
            Value::Str("seq".to_string()),
            Value::from(self.take_seq() as i64),
        );
        payload.insert(Value::Str("ts".to_string()), Value::Str(now_iso_ms()));
        payload.insert(
            Value::Str("ev".to_string()),
            Value::Str("report".to_string()),
        );
        payload.insert(Value::Str("path".to_string()), Value::Str(path.to_string()));
        payload.insert(
            Value::Str("level".to_string()),
            Value::Str(level.to_string()),
        );
        payload.insert(
            Value::Str("message".to_string()),
            Value::Str(message.to_string()),
        );
        if let Some(data) = data {
            match electricity_json::dumps_default_str(data, electricity_json::WriteMode::EVENTS) {
                Ok(text) => {
                    let byte_len = text.len();
                    if byte_len > REPORT_DATA_MAX_BYTES {
                        payload.insert(
                            Value::Str("data_truncated".to_string()),
                            Value::from(byte_len as i64),
                        );
                    } else {
                        payload.insert(Value::Str("data".to_string()), data.clone());
                    }
                }
                Err(err) => {
                    self.disable_with("report", &err);
                    return;
                }
            }
        }
        self.write_line("report", Value::Dict(payload));
    }

    pub fn run_end(&self, ok: bool, error: Option<&str>, signal: Option<&str>) {
        if self.disabled.get() {
            return;
        }
        let mut payload = Dict::new();
        payload.insert(Value::Str("v".to_string()), Value::from(1i64));
        payload.insert(
            Value::Str("seq".to_string()),
            Value::from(self.take_seq() as i64),
        );
        payload.insert(Value::Str("ts".to_string()), Value::Str(now_iso_ms()));
        payload.insert(
            Value::Str("ev".to_string()),
            Value::Str("run_end".to_string()),
        );
        payload.insert(Value::Str("ok".to_string()), Value::Bool(ok));
        if !ok {
            if let Some(error) = error {
                payload.insert(
                    Value::Str("error".to_string()),
                    Value::Str(truncate_error(error)),
                );
            }
        }
        if let Some(signal) = signal {
            payload.insert(
                Value::Str("signal".to_string()),
                Value::Str(signal.to_string()),
            );
        }
        self.write_line("run_end", Value::Dict(payload));
    }

    /// Stops writing; returns whether this instance ever failed to open
    /// or write the file -- the caller folds that into one run-level
    /// warning, the same as [`crate::live_state::LiveStateMirror::close`].
    pub fn close(&self) -> bool {
        *self.file.borrow_mut() = None;
        self.disabled.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "electricity-events-test-{}-{name}-{}",
            std::process::id(),
            name.len()
        ))
    }

    fn read_lines(path: &Path) -> Vec<Value> {
        fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| electricity_json::loads(line).unwrap())
            .collect()
    }

    #[test]
    fn run_start_and_run_end_write_matching_lines() {
        let path = temp_path("basic");
        let log = EventLog::open(&path);
        log.run_start("run-1", "doc.yml");
        log.run_end(true, None, None);
        assert!(!log.close());
        let lines = read_lines(&path);
        assert_eq!(lines.len(), 2);
        assert_eq!(
            lines[0]
                .as_dict()
                .unwrap()
                .get(&Value::Str("ev".to_string())),
            Some(&Value::Str("run_start".to_string()))
        );
        assert_eq!(
            lines[1]
                .as_dict()
                .unwrap()
                .get(&Value::Str("ev".to_string())),
            Some(&Value::Str("run_end".to_string()))
        );
        fs::remove_file(&path).unwrap();
    }

    fn blob_dict(blob_len: usize) -> Value {
        let mut d = electricity_value::Dict::new();
        d.insert(
            Value::Str("blob".to_string()),
            Value::Str("x".repeat(blob_len)),
        );
        Value::Dict(d)
    }

    fn report_data(lines: &[Value]) -> &Value {
        lines[0]
            .as_dict()
            .unwrap()
            .get(&Value::Str("data".to_string()))
            .unwrap()
    }

    #[test]
    fn report_data_at_exactly_16384_bytes_is_not_truncated() {
        // The limit is "exceeds 16 KiB", not "at least" (#605): a
        // payload whose compact JSON is exactly REPORT_DATA_MAX_BYTES
        // keeps `data`, with no `data_truncated` at that exact boundary.
        let path = temp_path("boundary-exact");
        let log = EventLog::open(&path);
        let blob_len = REPORT_DATA_MAX_BYTES - "{\"blob\":\"\"}".len();
        let data = blob_dict(blob_len);
        let encoded = electricity_json::dumps(&data, electricity_json::WriteMode::EVENTS).unwrap();
        assert_eq!(encoded.len(), REPORT_DATA_MAX_BYTES);
        log.on_report("prime", "info", "boundary", Some(&data));
        log.close();
        let lines = read_lines(&path);
        let payload = lines[0].as_dict().unwrap();
        assert!(!payload.contains_key(&Value::Str("data_truncated".to_string())));
        assert_eq!(report_data(&lines), &data);
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn report_data_one_byte_over_16384_is_truncated() {
        let path = temp_path("boundary-over");
        let log = EventLog::open(&path);
        let blob_len = REPORT_DATA_MAX_BYTES - "{\"blob\":\"\"}".len() + 1;
        let data = blob_dict(blob_len);
        let encoded = electricity_json::dumps(&data, electricity_json::WriteMode::EVENTS).unwrap();
        assert_eq!(encoded.len(), REPORT_DATA_MAX_BYTES + 1);
        log.on_report("prime", "info", "boundary", Some(&data));
        log.close();
        let lines = read_lines(&path);
        let payload = lines[0].as_dict().unwrap();
        assert!(!payload.contains_key(&Value::Str("data".to_string())));
        assert_eq!(
            payload.get(&Value::Str("data_truncated".to_string())),
            Some(&Value::from(encoded.len() as i64))
        );
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn start_and_complete_pair_by_path_with_an_ms_field() {
        let path = temp_path("pairing");
        let log = EventLog::open(&path);
        let id = log.on_start("prime");
        log.on_complete("prime", id, None);
        log.close();
        let lines = read_lines(&path);
        let end = lines[1].as_dict().unwrap();
        assert_eq!(
            end.get(&Value::Str("id".to_string())),
            Some(&Value::from(0i64))
        );
        assert!(end.contains_key(&Value::Str("ms".to_string())));
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn two_overlapping_calls_on_the_same_path_each_end_pairs_with_its_own_start() {
        // Review finding 3 on PR #454's own probe: start A, start B
        // (both on the same path -- an unnamed `flow: tree` loop's own
        // concurrent passes, once lane H lands, share exactly this
        // shape), then end A, then end B. A bare `Vec::pop` would hand
        // B's own id back for A's own end (and vice versa); removing by
        // the exact id `on_start` returned keeps them straight
        // regardless of finish order.
        let path = temp_path("overlapping");
        let log = EventLog::open(&path);
        let id_a = log.on_start("prime.lp.step");
        let id_b = log.on_start("prime.lp.step");
        assert_eq!((id_a, id_b), (0, 1));
        log.on_complete("prime.lp.step", id_a, None);
        log.on_complete("prime.lp.step", id_b, None);
        log.close();
        let lines = read_lines(&path);
        let end_a = lines[2].as_dict().unwrap();
        let end_b = lines[3].as_dict().unwrap();
        assert_eq!(
            end_a.get(&Value::Str("id".to_string())),
            Some(&Value::from(0i64))
        );
        assert_eq!(
            end_b.get(&Value::Str("id".to_string())),
            Some(&Value::from(1i64))
        );
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_complete_with_no_matching_start_gets_a_null_id_and_no_ms() {
        let path = temp_path("unmatched");
        let log = EventLog::open(&path);
        log.on_complete("prime", 0, Some("boom"));
        log.close();
        let lines = read_lines(&path);
        let end = lines[0].as_dict().unwrap();
        assert_eq!(end.get(&Value::Str("id".to_string())), Some(&Value::None));
        assert!(!end.contains_key(&Value::Str("ms".to_string())));
        assert_eq!(
            end.get(&Value::Str("error".to_string())),
            Some(&Value::Str("boom".to_string()))
        );
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_write_failure_disables_further_writes_without_panicking() {
        let log = EventLog::open(Path::new("/this/path/does/not/exist/events.jsonl"));
        log.run_start("run-1", "doc.yml");
        assert!(log.close());
    }

    /// Pins the wire format against `cof`'s own `_write_line`
    /// (`json.dumps(payload, separators=(",", ":"))`, `cli/events.py`):
    /// no space after either `,` or `:`, for a known payload.
    #[test]
    fn write_line_matches_cof_s_compact_separators_for_a_known_payload() {
        let path = temp_path("separators");
        let log = EventLog::open(&path);
        log.run_end(true, None, None);
        log.close();
        let line = fs::read_to_string(&path).unwrap();
        let line = line.trim_end_matches('\n');
        let parsed = electricity_json::loads(line).unwrap();
        let ts = parsed
            .as_dict()
            .unwrap()
            .get(&Value::Str("ts".to_string()))
            .unwrap()
            .as_str()
            .unwrap()
            .to_string();
        let expected =
            format!("{{\"v\":1,\"seq\":0,\"ts\":\"{ts}\",\"ev\":\"run_end\",\"ok\":true}}");
        assert_eq!(line, expected);
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn error_text_is_truncated_at_500_characters() {
        let path = temp_path("truncate");
        let log = EventLog::open(&path);
        let long_error = "x".repeat(600);
        log.on_complete("prime", 0, Some(&long_error));
        log.close();
        let lines = read_lines(&path);
        let error = lines[0]
            .as_dict()
            .unwrap()
            .get(&Value::Str("error".to_string()))
            .unwrap()
            .as_str()
            .unwrap();
        assert_eq!(error.chars().count(), 500);
        fs::remove_file(&path).unwrap();
    }
}
