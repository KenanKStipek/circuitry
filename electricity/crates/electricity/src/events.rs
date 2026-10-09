//! `--events`: `cli/events.py::EventLog`, ported (issue #431's
//! "`--events`" section; `electricity/docs/spec/runtime-semantics.md`
//! §8.7 has the wire format). One JSONL line per event, flushed at
//! once; a write failure disables every further write and is folded
//! into one run-level warning rather than ever failing the run.
//!
//! Single-threaded (electricity has no worker-thread tree-flow
//! branches of its own the way Circuitry's `DynamicRuntime` does --
//! DESIGN.md §6.1-6.2), so the per-thread `start`/`end` pairing stack
//! `cli/events.py` needs is just one stack per path here, not one per
//! `threading.local()`.

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
    /// one thread (this module's own doc comment).
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
        let text = match electricity_json::dumps(&payload, electricity_json::WriteMode::EVENTS) {
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

    pub fn on_start(&self, path: &str) {
        if self.disabled.get() {
            return;
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
    }

    /// `path` finished, with `error` the text that became (or would
    /// have become) its `meta.error`, `None` on success. A completion
    /// with no matching start on this path's own stack (a double-fire,
    /// or a call this instance never saw the start of) is not an error:
    /// `id: null` and no `ms`, same as `cli/events.py::EventLog.on_complete`.
    pub fn on_complete(&self, path: &str, error: Option<&str>) {
        if self.disabled.get() {
            return;
        }
        let popped = self.stacks.borrow_mut().get_mut(path).and_then(Vec::pop);
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

    #[test]
    fn start_and_complete_pair_by_path_with_an_ms_field() {
        let path = temp_path("pairing");
        let log = EventLog::open(&path);
        log.on_start("prime");
        log.on_complete("prime", None);
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
    fn a_complete_with_no_matching_start_gets_a_null_id_and_no_ms() {
        let path = temp_path("unmatched");
        let log = EventLog::open(&path);
        log.on_complete("prime", Some("boom"));
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
        log.on_complete("prime", Some(&long_error));
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
