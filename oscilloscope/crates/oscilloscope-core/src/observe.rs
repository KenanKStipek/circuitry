//! The live-state poller and the `--events` tailer (DESIGN.md §3 and
//! §6.1): polling every 100 ms rather than `notify`, since the live
//! file is replaced by rename on every write, and reading a partial
//! JSONL line safely at EOF while a write is in progress.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

pub const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileFingerprint {
    mtime: Option<SystemTime>,
    len: u64,
    ino: u64,
    dev: u64,
}

fn fingerprint(path: &Path) -> Option<FileFingerprint> {
    let meta = fs::metadata(path).ok()?;
    Some(FileFingerprint {
        mtime: meta.modified().ok(),
        len: meta.len(),
        ino: meta.ino(),
        dev: meta.dev(),
    })
}

/// Polls a live-state file (`state.live.json`/`state.json`) every call,
/// returning a freshly parsed snapshot only when the file's mtime, size
/// or inode changed since the last call that returned `Some` — the file
/// is replaced by rename on every write (DESIGN.md §1.1), so a change
/// in any of those three means a new, complete write landed.
pub struct LiveStatePoller {
    path: PathBuf,
    last: Option<FileFingerprint>,
}

impl LiveStatePoller {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        LiveStatePoller {
            path: path.into(),
            last: None,
        }
    }

    /// Returns `Some(snapshot)` when the file changed and parsed
    /// cleanly. A change caught mid-write (invalid JSON) is not an
    /// error: the next poll picks up the completed write, since the
    /// fingerprint will have changed again by then. A missing file (no
    /// run started yet, or an engine that doesn't write one) is not an
    /// error either.
    pub fn poll(&mut self) -> Option<Value> {
        let fp = fingerprint(&self.path)?;
        if Some(fp) == self.last {
            return None;
        }
        let Ok(bytes) = fs::read(&self.path) else {
            return None;
        };
        let Ok(value) = serde_json::from_slice(&bytes) else {
            return None;
        };
        self.last = Some(fp);
        Some(value)
    }
}

/// Tails a JSONL file from the start, safely across partial lines
/// (DESIGN.md §3's flushing rules: one `write()` per whole line, so a
/// reader never sees a torn line except a trailing one still being
/// written).
pub struct EventsTailer {
    path: PathBuf,
    offset: u64,
    partial: String,
}

impl EventsTailer {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        EventsTailer {
            path: path.into(),
            offset: 0,
            partial: String::new(),
        }
    }

    /// Returns every newly complete line since the last call, parsed as
    /// JSON. A line that fails to parse is skipped (never crashes the
    /// poll loop) — the format promises whole lines, not valid ones
    /// forever, and a reader that can't make sense of one future field
    /// should not stop reading the rest of the stream.
    pub fn poll(&mut self) -> Vec<Value> {
        let Ok(mut file) = File::open(&self.path) else {
            return Vec::new();
        };
        let Ok(meta) = file.metadata() else {
            return Vec::new();
        };
        if meta.len() < self.offset {
            // Truncated/replaced (a fresh run reusing the same path):
            // start over.
            self.offset = 0;
            self.partial.clear();
        }
        if meta.len() == self.offset {
            return Vec::new();
        }
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return Vec::new();
        }
        let mut buf = String::new();
        if file.read_to_string(&mut buf).is_err() {
            return Vec::new();
        }
        self.offset = meta.len();

        self.partial.push_str(&buf);
        let mut lines = Vec::new();
        let ends_with_newline = self.partial.ends_with('\n');
        let mut parts: Vec<&str> = self.partial.split('\n').collect();
        let trailing = if ends_with_newline {
            ""
        } else {
            parts.pop().unwrap_or("")
        };
        for line in &parts {
            if line.is_empty() {
                continue;
            }
            if let Ok(value) = serde_json::from_str::<Value>(line) {
                lines.push(value);
            }
        }
        self.partial = trailing.to_string();
        lines
    }
}

/// One parsed `--events` line (DESIGN.md §3). `v`/`seq` aren't carried
/// through: osp only orders by `ts`, same as a state diff, and the
/// tailer already delivers lines in file order.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    RunStart {
        ts: String,
        run_id: String,
    },
    /// Sent once by a tree loop or tree dynamic before its branches
    /// start. `branches` is the true total (an `each` loop's item
    /// count, or a `dynamic`'s effect count); `concurrency`, when the
    /// stream has it, is the running ceiling (DESIGN.md §2.1 rule 4) —
    /// optional, so a stream from a `cof` built before it was added
    /// still parses.
    Dispatch {
        ts: String,
        path: String,
        branches: u64,
        concurrency: Option<u64>,
    },
    Start {
        ts: String,
        id: Option<i64>,
        path: String,
    },
    End {
        ts: String,
        id: Option<i64>,
        path: String,
        ok: bool,
        ms: Option<u64>,
        error: Option<String>,
    },
    RunEnd {
        ts: String,
        ok: bool,
        error: Option<String>,
        signal: Option<String>,
    },
}

/// Parses one already-deserialized `--events` line. An unrecognized
/// `ev` (a future addition) or a line missing a field this version
/// needs returns `None` rather than an error — a reader that can't
/// make sense of one future line shouldn't stop reading the rest of
/// the stream (same tolerance as `EventsTailer::poll`'s own per-line
/// parse failures).
pub fn parse_event(value: &Value) -> Option<Event> {
    let ts = value.get("ts")?.as_str()?.to_string();
    match value.get("ev")?.as_str()? {
        "run_start" => Some(Event::RunStart {
            ts,
            run_id: value.get("run_id")?.as_str()?.to_string(),
        }),
        "dispatch" => Some(Event::Dispatch {
            ts,
            path: value.get("path")?.as_str()?.to_string(),
            branches: value.get("branches")?.as_u64()?,
            concurrency: value.get("concurrency").and_then(Value::as_u64),
        }),
        "start" => Some(Event::Start {
            ts,
            id: value.get("id").and_then(Value::as_i64),
            path: value.get("path")?.as_str()?.to_string(),
        }),
        "end" => Some(Event::End {
            ts,
            id: value.get("id").and_then(Value::as_i64),
            path: value.get("path")?.as_str()?.to_string(),
            ok: value.get("ok")?.as_bool()?,
            ms: value.get("ms").and_then(Value::as_u64),
            error: value
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_string),
        }),
        "run_end" => Some(Event::RunEnd {
            ts,
            ok: value.get("ok")?.as_bool()?,
            error: value
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_string),
            signal: value
                .get("signal")
                .and_then(Value::as_str)
                .map(str::to_string),
        }),
        _ => None,
    }
}

/// Parses two `cof`-style timestamps (ISO-8601 UTC) and returns the
/// elapsed seconds between them. Used for a log line's duration
/// (DESIGN.md §2.4) and `--log`'s `mm:ss.s` elapsed clock (§6.2).
pub fn duration_seconds(start: &str, end: &str) -> Option<f64> {
    let start = chrono::DateTime::parse_from_rfc3339(start).ok()?;
    let end = chrono::DateTime::parse_from_rfc3339(end).ok()?;
    let micros = (end - start).num_microseconds()?;
    Some(micros as f64 / 1_000_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn poller_returns_none_until_the_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.live.json");
        let mut poller = LiveStatePoller::new(&path);
        assert!(poller.poll().is_none(), "no file yet");

        fs::write(&path, br#"{"a":1}"#).unwrap();
        let first = poller.poll();
        assert_eq!(first, Some(serde_json::json!({"a": 1})));
        assert!(poller.poll().is_none(), "unchanged file");

        // Simulate the real write pattern: write to a temp file, rename
        // over the original (DESIGN.md §1.1).
        let tmp = dir.path().join("state.live.json.tmp");
        fs::write(&tmp, br#"{"a":2}"#).unwrap();
        fs::rename(&tmp, &path).unwrap();
        assert_eq!(poller.poll(), Some(serde_json::json!({"a": 2})));
    }

    #[test]
    fn tailer_handles_a_partial_line_across_two_polls() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let mut file = fs::File::create(&path).unwrap();
        write!(file, "{{\"ev\":\"a\"}}\n{{\"ev\":\"b\"").unwrap();
        file.flush().unwrap();
        drop(file);

        let mut tailer = EventsTailer::new(&path);
        let first = tailer.poll();
        assert_eq!(first, vec![serde_json::json!({"ev": "a"})]);

        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file, "}}").unwrap();
        drop(file);

        let second = tailer.poll();
        assert_eq!(second, vec![serde_json::json!({"ev": "b"})]);
    }

    #[test]
    fn tailer_on_a_missing_file_returns_nothing() {
        let mut tailer = EventsTailer::new("/nonexistent-osp-events/events.jsonl");
        assert!(tailer.poll().is_empty());
    }

    #[test]
    fn parse_event_reads_dispatch_with_and_without_concurrency() {
        let with_concurrency: Value = serde_json::from_str(
            r#"{"v":1,"seq":7,"ts":"t","ev":"dispatch","path":"prime.each_tree","branches":3,"concurrency":2}"#,
        )
        .unwrap();
        assert_eq!(
            parse_event(&with_concurrency),
            Some(Event::Dispatch {
                ts: "t".to_string(),
                path: "prime.each_tree".to_string(),
                branches: 3,
                concurrency: Some(2),
            })
        );

        let without_concurrency: Value = serde_json::from_str(
            r#"{"v":1,"seq":7,"ts":"t","ev":"dispatch","path":"prime.each_tree","branches":3}"#,
        )
        .unwrap();
        assert_eq!(
            parse_event(&without_concurrency),
            Some(Event::Dispatch {
                ts: "t".to_string(),
                path: "prime.each_tree".to_string(),
                branches: 3,
                concurrency: None,
            })
        );
    }

    #[test]
    fn parse_event_reads_end_with_null_id() {
        let value: Value = serde_json::from_str(
            r#"{"v":1,"seq":1,"ts":"t","ev":"end","id":null,"path":"prime.x","ok":true}"#,
        )
        .unwrap();
        assert_eq!(
            parse_event(&value),
            Some(Event::End {
                ts: "t".to_string(),
                id: None,
                path: "prime.x".to_string(),
                ok: true,
                ms: None,
                error: None
            })
        );
    }

    #[test]
    fn parse_event_ignores_an_unrecognized_ev() {
        let value: Value =
            serde_json::from_str(r#"{"v":1,"seq":1,"ts":"t","ev":"something_future"}"#).unwrap();
        assert_eq!(parse_event(&value), None);
    }

    #[test]
    fn duration_seconds_parses_rfc3339_timestamps() {
        let d = duration_seconds("2026-10-08T19:56:22.000Z", "2026-10-08T19:56:24.500Z").unwrap();
        assert!((d - 2.5).abs() < 1e-9);
    }
}
