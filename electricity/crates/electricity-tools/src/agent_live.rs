//! `agent_live.py`, ported (issue #720): the `agent` tool's live
//! progress -- `meta.live` and the compact JSONL transcript, updated as
//! [`crate::agent_cli::run_agent_cli`]'s own `on_line` feed streams the
//! CLI's stdout.
//!
//! Unlike the Python module, this one runs on no thread of its own:
//! electricity's own executor is single-threaded and cooperative, so
//! [`LiveTracker::handle_line`] can simply call *publish* synchronously,
//! with no `LivePublisher`-style background writer or throttling queue
//! needed to keep a streaming CLI from blocking on a slow write.
//!
//! **Never recorded here**: prompt text, reply or thinking text, tool
//! arguments, tool output, file contents, the environment, or error
//! text -- only numbers, times, ids, the model name and tool names, the
//! same contract `docs/plugins/agent.md` commits cof's own module to.

use electricity_value::{Dict, Value};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

/// `agent_live.py::LIVE_SUMMARY_VERSION`.
pub const LIVE_SUMMARY_VERSION: i64 = 1;
/// `agent_live.py::MAX_TOOL_NAMES`.
const MAX_TOOL_NAMES: usize = 32;
/// `agent_live.py::MAX_TOOL_NAME_CHARS`.
const MAX_TOOL_NAME_CHARS: usize = 64;
const OTHER_TOOL: &str = "other";
/// `agent_live.py::MAX_TURN_TOKENS`.
const MAX_TURN_TOKENS: usize = 512;
/// `agent_live.py::TRANSCRIPT_LINE_CHARS`.
const TRANSCRIPT_LINE_CHARS: usize = 300;
/// `agent_live.py::_TARGET_COMMAND_CHARS`.
const TARGET_COMMAND_CHARS: usize = 120;

fn key(name: &str) -> Value {
    Value::Str(name.to_string())
}

fn now_iso() -> String {
    use chrono::SecondsFormat;
    chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Micros, false)
}

fn is_number(value: &Value) -> bool {
    matches!(value, Value::Int(_) | Value::Float(_))
}

fn as_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Int(n) => Some(n.to_f64() as i64),
        Value::Float(f) => Some(*f as i64),
        _ => None,
    }
}

fn as_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Int(n) => Some(n.to_f64()),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

fn clip(text: &str, limit: usize) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = flat.chars().collect();
    if chars.len() > limit {
        let mut truncated: String = chars[..limit.saturating_sub(3)].iter().collect();
        truncated.push_str("...");
        truncated
    } else {
        flat
    }
}

fn line_count(text: Option<&str>) -> Option<i64> {
    let text = text?;
    if text.is_empty() {
        return Some(0);
    }
    Some(text.matches('\n').count() as i64 + 1)
}

/// `agent_live.py::_PATH_ARG_KEYS`.
const PATH_ARG_KEYS: &[&str] = &["file_path", "path", "filepath", "notebook_path"];
/// `agent_live.py::_PATTERN_ARG_KEYS`.
const PATTERN_ARG_KEYS: &[&str] = &["pattern"];
/// `agent_live.py::_OLD_TEXT_KEYS`.
const OLD_TEXT_KEYS: &[&str] = &["old_string", "old_text", "before", "oldText"];
/// `agent_live.py::_NEW_TEXT_KEYS`.
const NEW_TEXT_KEYS: &[&str] = &["new_string", "new_text", "after", "newText"];
/// `agent_live.py::_CONTENT_KEYS`.
const CONTENT_KEYS: &[&str] = &["content", "new_content", "text"];

fn first_str<'a>(args: &'a Dict, keys: &[&str]) -> Option<&'a str> {
    for k in keys {
        if let Some(Value::Str(s)) = args.get(&Value::Str(k.to_string())) {
            if !s.is_empty() {
                return Some(s.as_str());
            }
        }
    }
    None
}

/// `agent_live.py::tool_target`.
pub fn tool_target(args: &Value) -> Option<String> {
    let args = args.as_dict()?;
    let pattern = first_str(args, PATTERN_ARG_KEYS);
    let path = first_str(args, PATH_ARG_KEYS);
    if let Some(pattern) = pattern {
        return Some(match path {
            Some(path) => format!("{pattern} {path}"),
            None => pattern.to_string(),
        });
    }
    if let Some(path) = path {
        return Some(path.to_string());
    }
    let command = args
        .get(&Value::Str("command".to_string()))
        .or_else(|| args.get(&Value::Str("cmd".to_string())));
    match command {
        Some(Value::Str(s)) => Some(chars_prefix(s, TARGET_COMMAND_CHARS)),
        Some(Value::List(items)) => {
            let mut parts = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::Str(s) => parts.push(s.clone()),
                    _ => return None,
                }
            }
            Some(chars_prefix(&parts.join(" "), TARGET_COMMAND_CHARS))
        }
        _ => None,
    }
}

fn chars_prefix(text: &str, limit: usize) -> String {
    text.chars().take(limit).collect()
}

/// `agent_live.py::diff_stats`.
pub fn diff_stats(name: &str, args: &Value) -> (Option<String>, Option<i64>, Option<i64>) {
    let Some(args) = args.as_dict() else {
        return (None, None, None);
    };
    let lowered = name.to_lowercase();
    let path = first_str(args, PATH_ARG_KEYS).map(str::to_string);
    if lowered == "write" {
        let added = line_count(first_str(args, CONTENT_KEYS));
        let removed = added.map(|_| 0);
        return (path, added, removed);
    }
    if lowered == "edit" {
        if let Some(Value::List(edits)) = args.get(&Value::Str("edits".to_string())) {
            let (added, removed) = edits_list_stats(edits);
            return (path, added, removed);
        }
        let added = line_count(first_str(args, NEW_TEXT_KEYS));
        let removed = line_count(first_str(args, OLD_TEXT_KEYS));
        return (path, added, removed);
    }
    if lowered == "multiedit" {
        let (added, removed) = match args.get(&Value::Str("edits".to_string())) {
            Some(Value::List(edits)) => edits_list_stats(edits),
            _ => (None, None),
        };
        return (path, added, removed);
    }
    (None, None, None)
}

fn edits_list_stats(edits: &[Value]) -> (Option<i64>, Option<i64>) {
    let mut added = 0i64;
    let mut removed = 0i64;
    let mut seen = false;
    for edit in edits {
        let Some(edit) = edit.as_dict() else { continue };
        if let Some(a) = line_count(first_str(edit, NEW_TEXT_KEYS)) {
            added += a;
            seen = true;
        }
        if let Some(r) = line_count(first_str(edit, OLD_TEXT_KEYS)) {
            removed += r;
            seen = true;
        }
    }
    if seen {
        (Some(added), Some(removed))
    } else {
        (None, None)
    }
}

struct PendingTool {
    seq: i64,
    bucket: String,
    raw_name: String,
    args: Value,
    started: std::time::Instant,
    started_at: String,
}

/// One tool attempt's `meta.live` and transcript, across its turns --
/// `agent_live.py::LiveTracker`.
pub struct LiveTracker<'a> {
    engine: String,
    model: RefCell<Option<String>>,
    model_locked: bool,
    attempt: i64,
    transcript_path: PathBuf,
    publish: &'a dyn Fn(Value),
    session_id: RefCell<Option<String>>,
    phase: RefCell<String>,
    started_at: String,
    turns: Cell<i64>,
    tool_calls: Cell<i64>,
    /// name -> (calls, ms), insertion-ordered (`agent_live.py`'s own
    /// `dict[str, ...]`), capped at [`MAX_TOOL_NAMES`] distinct names.
    tools: RefCell<Vec<(String, i64, f64)>>,
    current_tool: RefCell<Option<(String, String)>>,
    tokens_sent: Cell<Option<i64>>,
    tokens_received: Cell<Option<i64>>,
    cost_usd: Cell<Option<f64>>,
    tokens_cached_in: Cell<Option<i64>>,
    tokens_cache_write: Cell<Option<i64>>,
    context_tokens: Cell<Option<i64>>,
    context_window: Cell<Option<i64>>,
    turn_tokens: RefCell<VecDeque<i64>>,
    files_touched: RefCell<std::collections::HashSet<String>>,
    lines_added: Cell<Option<i64>>,
    lines_removed: Cell<Option<i64>>,
    pending_by_id: RefCell<HashMap<String, PendingTool>>,
    pending_fifo: RefCell<VecDeque<PendingTool>>,
    tool_seq: Cell<i64>,
    seen_message_ids: RefCell<std::collections::HashSet<String>>,
    earlier_attempts: Option<(Option<i64>, Option<i64>)>,
    /// `agent_live.py::LivePublisher`'s own throttle (#636, ruling 5):
    /// at most one publish a second, with a pending update from inside
    /// that window still flushed once the window ends -- here, by
    /// [`LiveTracker::maybe_flush`], called from the same periodic tick
    /// [`crate::process::run_tracked`]'s own `on_live` cadence already
    /// gives a process-backed tool's live tail (#741), rather than a
    /// background thread of this tracker's own.
    published_at: Cell<Option<std::time::Instant>>,
    live_pending: Cell<bool>,
}

impl<'a> LiveTracker<'a> {
    pub fn new(
        engine: &str,
        model: &str,
        attempt: i64,
        transcript_path: &Path,
        publish: &'a dyn Fn(Value),
        earlier_attempts: Option<(Option<i64>, Option<i64>)>,
    ) -> Self {
        let model = if model.is_empty() {
            None
        } else {
            Some(model.to_string())
        };
        let model_locked = model.is_some();
        let tracker = LiveTracker {
            engine: engine.to_string(),
            model: RefCell::new(model),
            model_locked,
            attempt,
            transcript_path: transcript_path.to_path_buf(),
            publish,
            session_id: RefCell::new(None),
            phase: RefCell::new("turn".to_string()),
            started_at: now_iso(),
            turns: Cell::new(0),
            tool_calls: Cell::new(0),
            tools: RefCell::new(Vec::new()),
            current_tool: RefCell::new(None),
            tokens_sent: Cell::new(None),
            tokens_received: Cell::new(None),
            cost_usd: Cell::new(None),
            tokens_cached_in: Cell::new(None),
            tokens_cache_write: Cell::new(None),
            context_tokens: Cell::new(None),
            context_window: Cell::new(None),
            turn_tokens: RefCell::new(VecDeque::new()),
            files_touched: RefCell::new(std::collections::HashSet::new()),
            lines_added: Cell::new(None),
            lines_removed: Cell::new(None),
            pending_by_id: RefCell::new(HashMap::new()),
            pending_fifo: RefCell::new(VecDeque::new()),
            tool_seq: Cell::new(0),
            seen_message_ids: RefCell::new(std::collections::HashSet::new()),
            earlier_attempts,
            published_at: Cell::new(None),
            live_pending: Cell::new(false),
        };
        tracker.request_publish();
        tracker
    }

    pub fn transcript_path(&self) -> &Path {
        &self.transcript_path
    }

    // ---- publishing ----

    fn snapshot(&self) -> Value {
        let mut dict = Dict::new();
        dict.insert(key("v"), Value::from(LIVE_SUMMARY_VERSION));
        dict.insert(key("engine"), Value::Str(self.engine.clone()));
        dict.insert(
            key("model"),
            self.model
                .borrow()
                .clone()
                .map(Value::Str)
                .unwrap_or(Value::None),
        );
        dict.insert(
            key("session_id"),
            self.session_id
                .borrow()
                .clone()
                .map(Value::Str)
                .unwrap_or(Value::None),
        );
        dict.insert(
            key("transcript"),
            Value::Str(self.transcript_path.display().to_string()),
        );
        dict.insert(key("attempt"), Value::from(self.attempt));
        dict.insert(key("phase"), Value::Str(self.phase.borrow().clone()));
        dict.insert(key("started_at"), Value::Str(self.started_at.clone()));
        dict.insert(key("updated_at"), Value::Str(now_iso()));
        dict.insert(key("turns"), Value::from(self.turns.get()));
        dict.insert(key("tool_calls"), Value::from(self.tool_calls.get()));
        let mut tools_dict = Dict::new();
        for (name, calls, ms) in self.tools.borrow().iter() {
            let mut entry = Dict::new();
            entry.insert(key("calls"), Value::from(*calls));
            entry.insert(key("ms"), Value::from(ms.round() as i64));
            tools_dict.insert(Value::Str(name.clone()), Value::Dict(entry));
        }
        dict.insert(key("tools"), Value::Dict(tools_dict));
        dict.insert(
            key("current_tool"),
            match &*self.current_tool.borrow() {
                Some((name, started_at)) => {
                    let mut entry = Dict::new();
                    entry.insert(key("name"), Value::Str(name.clone()));
                    entry.insert(key("started_at"), Value::Str(started_at.clone()));
                    Value::Dict(entry)
                }
                None => Value::None,
            },
        );
        dict.insert(key("tokens_sent"), opt_i64(self.tokens_sent.get()));
        dict.insert(key("tokens_received"), opt_i64(self.tokens_received.get()));
        dict.insert(key("cost_usd"), opt_f64(self.cost_usd.get()));
        dict.insert(
            key("tokens_cached_in"),
            opt_i64(self.tokens_cached_in.get()),
        );
        dict.insert(
            key("tokens_cache_write"),
            opt_i64(self.tokens_cache_write.get()),
        );
        dict.insert(key("context_tokens"), opt_i64(self.context_tokens.get()));
        dict.insert(key("context_window"), opt_i64(self.context_window.get()));
        dict.insert(
            key("turn_tokens"),
            Value::List(
                self.turn_tokens
                    .borrow()
                    .iter()
                    .map(|v| Value::from(*v))
                    .collect(),
            ),
        );
        let files_changed = self.files_touched.borrow().len();
        dict.insert(
            key("files_changed"),
            if files_changed > 0 {
                Value::from(files_changed as i64)
            } else {
                Value::None
            },
        );
        dict.insert(key("lines_added"), opt_i64(self.lines_added.get()));
        dict.insert(key("lines_removed"), opt_i64(self.lines_removed.get()));
        if let Some((sent, received)) = &self.earlier_attempts {
            let mut entry = Dict::new();
            entry.insert(key("tokens_sent"), opt_i64(*sent));
            entry.insert(key("tokens_received"), opt_i64(*received));
            dict.insert(key("earlier_attempts"), Value::Dict(entry));
        }
        Value::Dict(dict)
    }

    fn publish_now(&self) {
        (self.publish)(self.snapshot());
    }

    /// `LivePublisher`'s own cadence check (`crate::process::live_due`,
    /// the exact gate a process-backed tool's own live tail already uses)
    /// -- `true` at most once a second, coalescing every call in between
    /// into [`LiveTracker::live_pending`] for [`LiveTracker::maybe_flush`]
    /// to flush once the window has actually passed.
    fn live_due(&self) -> bool {
        let mut published_at = self.published_at.get();
        let mut pending = self.live_pending.get();
        let due = crate::process::live_due(&mut published_at, &mut pending);
        self.published_at.set(published_at);
        self.live_pending.set(pending);
        due
    }

    /// Every `agent_live.py::LiveTracker::_publish_now` call site's own
    /// replacement: publishes immediately when the throttle allows it,
    /// otherwise only records that an update is pending.
    fn request_publish(&self) {
        if self.live_due() {
            self.publish_now();
        }
    }

    /// [`crate::agent_cli::run_agent_cli`]'s own periodic tick while the
    /// CLI runs, independent of new stdout: flushes a publish left
    /// pending by [`LiveTracker::request_publish`] once the throttle
    /// window it was denied in has actually ended, so a quiet stretch
    /// mid-turn still surfaces the one update it missed within about a
    /// second of it, exactly as `agent_live.py::LivePublisher`'s own
    /// writer thread would have.
    pub fn maybe_flush(&self) {
        if self.live_pending.get() && self.live_due() {
            self.publish_now();
        }
    }

    // ---- transcript ----

    fn write_transcript(&self, kind: &str, fields: Dict) {
        let mut line = Dict::new();
        line.insert(key("t"), Value::Str(now_iso()));
        line.insert(key("type"), Value::Str(kind.to_string()));
        for (k, v) in fields {
            line.insert(k, v);
        }
        // `agent_live.py::_write_transcript`'s own `json.dumps(...,
        // ensure_ascii=False)` -- unlike `WriteMode::COMPACT`, never
        // escaping a non-ASCII codepoint.
        let transcript_mode = electricity_json::WriteMode {
            ensure_ascii: false,
            ..electricity_json::WriteMode::COMPACT
        };
        let encoded =
            electricity_json::dumps(&Value::Dict(line), transcript_mode).unwrap_or_default();
        if let Some(parent) = self.transcript_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.transcript_path)
        {
            use std::io::Write;
            let _ = writeln!(file, "{encoded}");
        }
    }

    fn note_reply(&self, text: &str) {
        let mut fields = Dict::new();
        fields.insert(key("text"), Value::Str(clip(text, TRANSCRIPT_LINE_CHARS)));
        self.write_transcript("reply", fields);
    }

    // ---- turn boundaries ----

    /// `agent_live.py::LiveTracker.start_turn`.
    pub fn start_turn(&self, phase: &str, session_id: Option<&str>, label: Option<&str>) {
        *self.phase.borrow_mut() = phase.to_string();
        if let Some(session_id) = session_id {
            *self.session_id.borrow_mut() = Some(session_id.to_string());
        }
        *self.current_tool.borrow_mut() = None;
        let mut fields = Dict::new();
        fields.insert(key("label"), Value::Str(label.unwrap_or(phase).to_string()));
        fields.insert(
            key("session_id"),
            self.session_id
                .borrow()
                .clone()
                .map(Value::Str)
                .unwrap_or(Value::None),
        );
        self.write_transcript("turn", fields);
        self.request_publish();
    }

    /// `agent_live.py::LiveTracker.sync_turn_totals`.
    pub fn sync_turn_totals(
        &self,
        turns: i64,
        tool_calls: i64,
        tokens_sent: Option<i64>,
        tokens_received: Option<i64>,
        cost_usd: Option<f64>,
    ) {
        self.turns.set(turns);
        self.tool_calls.set(tool_calls);
        self.tokens_sent.set(tokens_sent);
        self.tokens_received.set(tokens_received);
        self.cost_usd.set(cost_usd);
        self.request_publish();
    }

    fn note_model(&self, value: Option<&str>) -> bool {
        let Some(value) = value else { return false };
        if value.is_empty() || self.model_locked {
            return false;
        }
        if self.model.borrow().as_deref() == Some(value) {
            return false;
        }
        *self.model.borrow_mut() = Some(value.to_string());
        true
    }

    pub fn session_id(&self) -> Option<String> {
        self.session_id.borrow().clone()
    }

    /// `agent_live.py::LiveTracker.raw_extra`.
    pub fn raw_extra(&self) -> Vec<(Value, Value)> {
        let mut tool_counts = Dict::new();
        for (name, calls, ms) in self.tools.borrow().iter() {
            let mut entry = Dict::new();
            entry.insert(key("calls"), Value::from(*calls));
            entry.insert(key("ms"), Value::from(ms.round() as i64));
            tool_counts.insert(Value::Str(name.clone()), Value::Dict(entry));
        }
        let files_changed = self.files_touched.borrow().len();
        vec![
            (
                key("model"),
                self.model
                    .borrow()
                    .clone()
                    .map(Value::Str)
                    .unwrap_or(Value::None),
            ),
            (key("context_tokens"), opt_i64(self.context_tokens.get())),
            (key("context_window"), opt_i64(self.context_window.get())),
            (key("tool_counts"), Value::Dict(tool_counts)),
            (
                key("turn_tokens"),
                Value::List(
                    self.turn_tokens
                        .borrow()
                        .iter()
                        .map(|v| Value::from(*v))
                        .collect(),
                ),
            ),
            (
                key("files_changed"),
                if files_changed > 0 {
                    Value::from(files_changed as i64)
                } else {
                    Value::None
                },
            ),
            (key("lines_added"), opt_i64(self.lines_added.get())),
            (key("lines_removed"), opt_i64(self.lines_removed.get())),
        ]
    }

    // ---- tool name bucketing ----

    fn bucket_tool_name(&self, name: &str) -> String {
        let truncated = chars_prefix(name, MAX_TOOL_NAME_CHARS);
        let truncated = if truncated.is_empty() {
            "?".to_string()
        } else {
            truncated
        };
        let tools = self.tools.borrow();
        if tools.iter().any(|(n, _, _)| n == &truncated) || tools.len() < MAX_TOOL_NAMES {
            truncated
        } else {
            OTHER_TOOL.to_string()
        }
    }

    fn start_tool(&self, name: Option<&str>, args: &Value, call_id: Option<&str>) {
        let raw_name = name.unwrap_or("?").to_string();
        let target = tool_target(args);
        let started_at = now_iso();
        self.tool_calls.set(self.tool_calls.get() + 1);
        let bucket = self.bucket_tool_name(&raw_name);
        {
            let mut tools = self.tools.borrow_mut();
            match tools.iter_mut().find(|(n, _, _)| n == &bucket) {
                Some((_, calls, _)) => *calls += 1,
                None => tools.push((bucket.clone(), 1, 0.0)),
            }
        }
        self.tool_seq.set(self.tool_seq.get() + 1);
        let seq = self.tool_seq.get();
        let pending = PendingTool {
            seq,
            bucket: bucket.clone(),
            raw_name,
            args: args.clone(),
            started: std::time::Instant::now(),
            started_at: started_at.clone(),
        };
        match call_id {
            Some(id) => {
                self.pending_by_id
                    .borrow_mut()
                    .insert(id.to_string(), pending);
            }
            None => {
                self.pending_fifo.borrow_mut().push_back(pending);
            }
        }
        *self.current_tool.borrow_mut() = Some((bucket.clone(), started_at));
        let mut fields = Dict::new();
        fields.insert(key("id"), Value::from(seq));
        fields.insert(key("tool"), Value::Str(bucket));
        fields.insert(key("target"), target.map(Value::Str).unwrap_or(Value::None));
        self.write_transcript("tool_start", fields);
        self.request_publish();
    }

    fn latest_pending_tool(&self) -> Option<(String, String)> {
        let by_id = self.pending_by_id.borrow();
        let fifo = self.pending_fifo.borrow();
        let mut best: Option<&PendingTool> = None;
        for pending in by_id.values().chain(fifo.iter()) {
            if best.is_none_or(|current| pending.seq > current.seq) {
                best = Some(pending);
            }
        }
        best.map(|p| (p.bucket.clone(), p.started_at.clone()))
    }

    fn end_tool(&self, ok: bool, duration_ms: Option<f64>, call_id: Option<&str>) {
        let pending = match call_id {
            Some(id) => self.pending_by_id.borrow_mut().remove(id),
            None => self.pending_fifo.borrow_mut().pop_front(),
        };
        let Some(pending) = pending else {
            self.request_publish();
            return;
        };
        let ms = duration_ms.unwrap_or_else(|| pending.started.elapsed().as_secs_f64() * 1000.0);
        {
            let mut tools = self.tools.borrow_mut();
            if let Some((_, _, total_ms)) = tools.iter_mut().find(|(n, _, _)| n == &pending.bucket)
            {
                *total_ms += ms;
            } else {
                tools.push((pending.bucket.clone(), 0, ms));
            }
        }
        *self.current_tool.borrow_mut() = self.latest_pending_tool();
        let (path, added, removed) = diff_stats(&pending.raw_name, &pending.args);
        if let Some(path) = &path {
            self.files_touched.borrow_mut().insert(path.clone());
        }
        if let Some(added) = added {
            self.lines_added
                .set(Some(self.lines_added.get().unwrap_or(0) + added));
        }
        if let Some(removed) = removed {
            self.lines_removed
                .set(Some(self.lines_removed.get().unwrap_or(0) + removed));
        }
        let mut fields = Dict::new();
        fields.insert(key("id"), Value::from(pending.seq));
        fields.insert(key("ok"), Value::Bool(ok));
        fields.insert(key("ms"), Value::from(ms.round() as i64));
        if path.is_some() {
            fields.insert(key("lines_added"), opt_i64(added));
            fields.insert(key("lines_removed"), opt_i64(removed));
        }
        self.write_transcript("tool_end", fields);
        self.request_publish();
    }

    fn apply_usage(
        &self,
        sent_parts: &[Option<i64>],
        received: Option<i64>,
        cost: Option<f64>,
        cache_read: Option<i64>,
        cache_write: Option<i64>,
    ) {
        let mut context = 0i64;
        let mut any_sent = false;
        for value in sent_parts.iter().flatten() {
            self.tokens_sent
                .set(Some(self.tokens_sent.get().unwrap_or(0) + value));
            context += value;
            any_sent = true;
        }
        if let Some(received) = received {
            self.tokens_received
                .set(Some(self.tokens_received.get().unwrap_or(0) + received));
        }
        if let Some(cost) = cost {
            self.cost_usd
                .set(Some(self.cost_usd.get().unwrap_or(0.0) + cost));
        }
        if let Some(cache_read) = cache_read {
            self.tokens_cached_in
                .set(Some(self.tokens_cached_in.get().unwrap_or(0) + cache_read));
        }
        if let Some(cache_write) = cache_write {
            self.tokens_cache_write.set(Some(
                self.tokens_cache_write.get().unwrap_or(0) + cache_write,
            ));
        }
        if any_sent {
            self.context_tokens.set(Some(context));
            let mut turn_tokens = self.turn_tokens.borrow_mut();
            turn_tokens.push_back(context);
            while turn_tokens.len() > MAX_TURN_TOKENS {
                turn_tokens.pop_front();
            }
        }
    }

    // ---- per-line handlers ----

    /// `agent_live.py::LiveTracker.handle_line`.
    pub fn handle_line(&self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        if self.engine == "pi" {
            self.handle_pi_line(line);
        } else {
            self.handle_claude_line(line);
        }
    }

    fn handle_pi_line(&self, line: &str) {
        // `agent_live.py::_PI_MESSAGE_UPDATE_RE`: a plain prefix check,
        // never worth a full parse.
        if line
            .trim_start()
            .starts_with("{\"type\": \"message_update\"")
            || line
                .trim_start()
                .starts_with("{\"type\":\"message_update\"")
        {
            return;
        }
        let Ok(event) = electricity_json::loads(line) else {
            return;
        };
        let Some(event) = event.as_dict() else { return };
        let kind = event.get(&key("type")).and_then(Value::as_str);
        match kind {
            Some("session") => {
                if let Some(Value::Str(id)) = event.get(&key("id")) {
                    *self.session_id.borrow_mut() = Some(id.clone());
                    self.request_publish();
                }
            }
            Some("tool_execution_start") => {
                let call_id = event.get(&key("toolCallId")).and_then(Value::as_str);
                let args = event.get(&key("args")).cloned().unwrap_or(Value::None);
                let tool_name = event.get(&key("toolName")).and_then(Value::as_str);
                self.start_tool(tool_name, &args, call_id);
            }
            Some("tool_execution_end") => {
                let call_id = event.get(&key("toolCallId")).and_then(Value::as_str);
                let ok = !matches!(event.get(&key("isError")), Some(Value::Bool(true)));
                let duration_ms = event.get(&key("durationMs")).and_then(as_f64);
                self.end_tool(ok, duration_ms, call_id);
            }
            Some("message_start") => {
                let model = event
                    .get(&key("message"))
                    .and_then(Value::as_dict)
                    .and_then(|m| m.get(&key("model")))
                    .and_then(Value::as_str);
                if self.note_model(model) {
                    self.request_publish();
                }
            }
            Some("message_end") => {
                let Some(message) = event.get(&key("message")).and_then(Value::as_dict) else {
                    return;
                };
                if message.get(&key("role")).and_then(Value::as_str) != Some("assistant") {
                    return;
                }
                self.turns.set(self.turns.get() + 1);
                self.note_model(message.get(&key("model")).and_then(Value::as_str));
                if let Some(usage) = message.get(&key("usage")).and_then(Value::as_dict) {
                    let cost = match usage.get(&key("cost")) {
                        Some(Value::Dict(cost_dict)) => {
                            cost_dict.get(&key("total")).and_then(as_f64)
                        }
                        Some(other) => as_f64(other),
                        None => None,
                    };
                    self.apply_usage(
                        &[
                            usage.get(&key("input")).and_then(as_i64),
                            usage.get(&key("cacheRead")).and_then(as_i64),
                            usage.get(&key("cacheWrite")).and_then(as_i64),
                        ],
                        usage.get(&key("output")).and_then(as_i64),
                        cost,
                        usage.get(&key("cacheRead")).and_then(as_i64),
                        usage.get(&key("cacheWrite")).and_then(as_i64),
                    );
                }
                let text = pi_message_text(message.get(&key("content")));
                if !text.is_empty() {
                    self.note_reply(&text);
                }
                self.request_publish();
            }
            _ => {}
        }
    }

    fn handle_claude_line(&self, line: &str) {
        let Ok(event) = electricity_json::loads(line) else {
            return;
        };
        let Some(event) = event.as_dict() else { return };
        let kind = event.get(&key("type")).and_then(Value::as_str);
        match kind {
            Some("system") => {
                let session_id = event.get(&key("session_id")).and_then(Value::as_str);
                let model_changed =
                    self.note_model(event.get(&key("model")).and_then(Value::as_str));
                let mut session_changed = false;
                if let Some(session_id) = session_id {
                    *self.session_id.borrow_mut() = Some(session_id.to_string());
                    session_changed = true;
                }
                if model_changed || session_changed {
                    self.request_publish();
                }
            }
            Some("assistant") => {
                let Some(message) = event.get(&key("message")).and_then(Value::as_dict) else {
                    return;
                };
                let message_id = message.get(&key("id")).and_then(Value::as_str);
                let first_for_message = match message_id {
                    Some(id) => {
                        let mut seen = self.seen_message_ids.borrow_mut();
                        if seen.contains(id) {
                            false
                        } else {
                            seen.insert(id.to_string());
                            true
                        }
                    }
                    None => true,
                };
                let is_subagent = event
                    .get(&key("parent_tool_use_id"))
                    .is_some_and(|v| !matches!(v, Value::None));
                if first_for_message {
                    if !is_subagent {
                        self.turns.set(self.turns.get() + 1);
                    }
                    if !is_subagent {
                        if let Some(usage) = message.get(&key("usage")).and_then(Value::as_dict) {
                            self.apply_usage(
                                &[
                                    usage.get(&key("input_tokens")).and_then(as_i64),
                                    usage.get(&key("cache_read_input_tokens")).and_then(as_i64),
                                    usage
                                        .get(&key("cache_creation_input_tokens"))
                                        .and_then(as_i64),
                                ],
                                None,
                                None,
                                usage.get(&key("cache_read_input_tokens")).and_then(as_i64),
                                usage
                                    .get(&key("cache_creation_input_tokens"))
                                    .and_then(as_i64),
                            );
                        }
                    }
                }
                let mut texts: Vec<String> = Vec::new();
                if let Some(Value::List(content)) = message.get(&key("content")) {
                    for item in content {
                        let Some(item) = item.as_dict() else { continue };
                        match item.get(&key("type")).and_then(Value::as_str) {
                            Some("tool_use") => {
                                let call_id = item.get(&key("id")).and_then(Value::as_str);
                                let input = item.get(&key("input")).cloned().unwrap_or(Value::None);
                                let name = item.get(&key("name")).and_then(Value::as_str);
                                self.start_tool(name, &input, call_id);
                            }
                            Some("text") => {
                                if let Some(Value::Str(text)) = item.get(&key("text")) {
                                    if !text.trim().is_empty() {
                                        texts.push(text.clone());
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                for text in texts {
                    self.note_reply(&text);
                }
                self.request_publish();
            }
            Some("user") => {
                let content = event
                    .get(&key("message"))
                    .and_then(Value::as_dict)
                    .and_then(|m| m.get(&key("content")));
                if let Some(Value::List(items)) = content {
                    for item in items {
                        let Some(item) = item.as_dict() else { continue };
                        if item.get(&key("type")).and_then(Value::as_str) == Some("tool_result") {
                            let call_id = item.get(&key("tool_use_id")).and_then(Value::as_str);
                            let ok = !matches!(item.get(&key("is_error")), Some(Value::Bool(true)));
                            self.end_tool(ok, None, call_id);
                        }
                    }
                }
            }
            Some("result") => {
                if let Some(Value::Str(session_id)) = event.get(&key("session_id")) {
                    *self.session_id.borrow_mut() = Some(session_id.clone());
                }
                if let Some(usage) = event.get(&key("usage")).and_then(Value::as_dict) {
                    if let Some(output) = usage.get(&key("output_tokens")).and_then(as_i64) {
                        self.tokens_received
                            .set(Some(self.tokens_received.get().unwrap_or(0) + output));
                    }
                }
                if let Some(cost) = event.get(&key("total_cost_usd")).and_then(as_f64) {
                    self.cost_usd
                        .set(Some(self.cost_usd.get().unwrap_or(0.0) + cost));
                }
                if let Some(Value::Dict(model_usage)) = event.get(&key("modelUsage")) {
                    for info in model_usage.values() {
                        if let Some(info) = info.as_dict() {
                            if let Some(window) = info.get(&key("contextWindow")).and_then(as_i64) {
                                self.context_window.set(Some(window));
                                break;
                            }
                        }
                    }
                }
                self.request_publish();
            }
            _ => {}
        }
    }
}

fn opt_i64(value: Option<i64>) -> Value {
    value.map(Value::from).unwrap_or(Value::None)
}

fn opt_f64(value: Option<f64>) -> Value {
    value.map(Value::Float).unwrap_or(Value::None)
}

fn pi_message_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::Str(text)) => text.trim().to_string(),
        Some(Value::List(items)) => {
            let mut out = String::new();
            for item in items {
                if let Some(item) = item.as_dict() {
                    if item.get(&key("type")).and_then(Value::as_str) == Some("text") {
                        if let Some(Value::Str(text)) = item.get(&key("text")) {
                            out.push_str(text);
                        }
                    }
                }
            }
            out.trim().to_string()
        }
        _ => String::new(),
    }
}

#[allow(dead_code)]
fn unused_is_number(value: &Value) -> bool {
    is_number(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_target_prefers_pattern_then_path_then_command() {
        let mut args = Dict::new();
        args.insert(key("pattern"), Value::Str("foo".to_string()));
        args.insert(key("path"), Value::Str("src/".to_string()));
        assert_eq!(
            tool_target(&Value::Dict(args)),
            Some("foo src/".to_string())
        );

        let mut args = Dict::new();
        args.insert(key("path"), Value::Str("src/lib.rs".to_string()));
        assert_eq!(
            tool_target(&Value::Dict(args)),
            Some("src/lib.rs".to_string())
        );

        let mut args = Dict::new();
        args.insert(key("command"), Value::Str("echo hi".to_string()));
        assert_eq!(tool_target(&Value::Dict(args)), Some("echo hi".to_string()));
    }

    #[test]
    fn diff_stats_counts_write_content_lines() {
        let mut args = Dict::new();
        args.insert(key("path"), Value::Str("a.txt".to_string()));
        args.insert(key("content"), Value::Str("one\ntwo\nthree".to_string()));
        let (path, added, removed) = diff_stats("write", &Value::Dict(args));
        assert_eq!(path, Some("a.txt".to_string()));
        assert_eq!(added, Some(3));
        assert_eq!(removed, Some(0));
    }

    #[test]
    fn diff_stats_edit_reads_old_and_new_text() {
        let mut args = Dict::new();
        args.insert(key("path"), Value::Str("a.txt".to_string()));
        args.insert(key("old_string"), Value::Str("a\nb".to_string()));
        args.insert(key("new_string"), Value::Str("a\nb\nc".to_string()));
        let (path, added, removed) = diff_stats("edit", &Value::Dict(args));
        assert_eq!(path, Some("a.txt".to_string()));
        assert_eq!(added, Some(3));
        assert_eq!(removed, Some(2));
    }

    fn tracker<'a>(engine: &str, publish: &'a dyn Fn(Value)) -> LiveTracker<'a> {
        LiveTracker::new(
            engine,
            "",
            1,
            Path::new("/tmp/does-not-matter.log"),
            publish,
            None,
        )
    }

    #[test]
    fn snapshot_key_order_matches_agent_live_py() {
        let tracker = tracker("pi", &|_v| {});
        let snapshot = tracker.snapshot();
        let Value::Dict(ref dict) = snapshot else {
            panic!("dict")
        };
        let keys: Vec<String> = dict
            .keys()
            .map(|k| match k {
                Value::Str(s) => s.clone(),
                _ => panic!("string key"),
            })
            .collect();
        assert_eq!(
            keys,
            vec![
                "v",
                "engine",
                "model",
                "session_id",
                "transcript",
                "attempt",
                "phase",
                "started_at",
                "updated_at",
                "turns",
                "tool_calls",
                "tools",
                "current_tool",
                "tokens_sent",
                "tokens_received",
                "cost_usd",
                "tokens_cached_in",
                "tokens_cache_write",
                "context_tokens",
                "context_window",
                "turn_tokens",
                "files_changed",
                "lines_added",
                "lines_removed",
            ]
        );
    }

    #[test]
    fn pi_message_end_counts_a_turn_and_usage() {
        let tracker = tracker("pi", &|_v| {});
        tracker.handle_line(
            r#"{"type": "message_end", "message": {"role": "assistant", "content": "hi", "usage": {"input": 3, "output": 5}}}"#,
        );
        assert_eq!(tracker.turns.get(), 1);
        assert_eq!(tracker.tokens_sent.get(), Some(3));
        assert_eq!(tracker.tokens_received.get(), Some(5));
    }

    #[test]
    fn claude_assistant_tool_use_then_user_tool_result_pairs_by_id() {
        let tracker = tracker("claude_code", &|_v| {});
        tracker.handle_line(
            r#"{"type": "assistant", "message": {"id": "m1", "content": [{"type": "tool_use", "id": "t1", "name": "Read", "input": {"path": "a.txt"}}]}}"#,
        );
        assert_eq!(tracker.tool_calls.get(), 1);
        tracker.handle_line(
            r#"{"type": "user", "message": {"content": [{"type": "tool_result", "tool_use_id": "t1", "is_error": false}]}}"#,
        );
        assert!(tracker.pending_by_id.borrow().is_empty());
    }
}
