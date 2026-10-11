//! `adapters/scripted.py::ScriptedAdapter`, ported. No network, ever:
//! a call is matched to its own reply queue by the effect's
//! concretized call path (`electricity_vm::adapter::CallContext::path`),
//! the same key Python's own `current_call_path` reads, for the
//! conformance suite (`electricity/DESIGN.md` §12) and for running an
//! orchestration deterministically with no real model behind it. The
//! file format is `electricity/docs/spec/scripted-replies.md`.

use async_trait::async_trait;
use electricity_value::{Dict, IntValue, Value};
use electricity_vm::adapter::{
    Adapter, AdapterError, CallContext, CheckResult, GenerateOptions, GenerateResult, PyExc,
};
use electricity_vm::retry::RetryInfo;
use std::collections::{HashMap, VecDeque};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------
// Path handling: `Path(replies_file).expanduser()`, and `str(Path(...))`
// -- duplicated from `electricity-tools::fs`'s own `expand_user_path`/
// `split_components`/`render_path` (that crate isn't this lane's to
// edit, and isn't a dependency of this one).
// ---------------------------------------------------------------------

struct ResolvedPath {
    buf: PathBuf,
    /// `str(Path(...))` -- what every message below embeds.
    display: String,
}

fn split_components(raw: &str) -> (bool, Vec<String>) {
    let mut absolute = false;
    let mut parts = Vec::new();
    for component in Path::new(raw).components() {
        match component {
            Component::RootDir => absolute = true,
            Component::CurDir => {}
            Component::ParentDir => parts.push("..".to_string()),
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::Prefix(_) => {}
        }
    }
    (absolute, parts)
}

fn render_path(absolute: bool, parts: &[String]) -> String {
    if absolute {
        if parts.is_empty() {
            "/".to_string()
        } else {
            format!("/{}", parts.join("/"))
        }
    } else if parts.is_empty() {
        ".".to_string()
    } else {
        parts.join("/")
    }
}

/// `Path(raw).expanduser()` -- only the plain `~` (current user's own
/// `$HOME`) form is substituted; a `~username` form is left unchanged
/// (a documented, narrow divergence, never exercised by a hermetic
/// test run under a temp `$HOME`).
fn expand_user_path(raw: &str) -> ResolvedPath {
    let (mut absolute, mut parts) = split_components(raw);
    if !absolute && parts.first().map(String::as_str) == Some("~") {
        if let Ok(home) = std::env::var("HOME") {
            if !home.is_empty() {
                let (home_absolute, home_parts) = split_components(&home);
                parts.remove(0);
                let mut merged = home_parts;
                merged.extend(parts);
                parts = merged;
                absolute = home_absolute;
            }
        }
    }
    let display = render_path(absolute, &parts);
    ResolvedPath {
        buf: PathBuf::from(&display),
        display,
    }
}

/// `exc.strerror`: the bare OS error message, without Rust's own
/// `io::Error::to_string()` " (os error N)" suffix -- duplicated from
/// `electricity-tools::fs`'s own `strerror` (same reason as above).
fn strerror(err: &io::Error) -> String {
    let text = err.to_string();
    match text.rfind(" (os error ") {
        Some(idx) if text.ends_with(')') => text[..idx].to_string(),
        _ => text,
    }
}

/// CPython's `OSError.__str__`: `[Errno N] <strerror>: '<path>'`.
fn os_error_text(err: &io::Error, display: &str) -> String {
    let errno = err.raw_os_error().unwrap_or(0);
    format!("[Errno {errno}] {}: '{display}'", strerror(err))
}

// ---------------------------------------------------------------------
// UTF-8 decoding with CPython's exact `UnicodeDecodeError.__str__` --
// duplicated from `electricity-compiler::prompt_files`/
// `electricity-tools::fs`'s own `describe_utf8_error`/
// `find_utf8_error`/`utf8_lead_shape` (neither crate is this lane's to
// edit, and neither is a dependency of this one).
// ---------------------------------------------------------------------

fn describe_utf8_error(bytes: &[u8]) -> String {
    let (start, consumed, reason) =
        find_utf8_error(bytes).expect("describe_utf8_error called on valid UTF-8");
    if consumed == 1 {
        format!(
            "'utf-8' codec can't decode byte 0x{:02x} in position {start}: {reason}",
            bytes[start]
        )
    } else {
        format!(
            "'utf-8' codec can't decode bytes in position {start}-{}: {reason}",
            start + consumed - 1
        )
    }
}

fn find_utf8_error(bytes: &[u8]) -> Option<(usize, usize, &'static str)> {
    let mut i = 0;
    while i < bytes.len() {
        let lead = bytes[i];
        if lead < 0x80 {
            i += 1;
            continue;
        }
        let Some((total_len, min1, max1)) = utf8_lead_shape(lead) else {
            return Some((i, 1, "invalid start byte"));
        };
        for seq_idx in 1..total_len {
            let Some(&cont) = bytes.get(i + seq_idx) else {
                return Some((i, seq_idx, "unexpected end of data"));
            };
            let (lo, hi) = if seq_idx == 1 {
                (min1, max1)
            } else {
                (0x80, 0xBF)
            };
            if cont < lo || cont > hi {
                return Some((i, seq_idx, "invalid continuation byte"));
            }
        }
        i += total_len;
    }
    None
}

fn utf8_lead_shape(lead: u8) -> Option<(usize, u8, u8)> {
    match lead {
        0xC2..=0xDF => Some((2, 0x80, 0xBF)),
        0xE0 => Some((3, 0xA0, 0xBF)),
        0xE1..=0xEC => Some((3, 0x80, 0xBF)),
        0xED => Some((3, 0x80, 0x9F)),
        0xEE..=0xEF => Some((3, 0x80, 0xBF)),
        0xF0 => Some((4, 0x90, 0xBF)),
        0xF1..=0xF3 => Some((4, 0x80, 0x8F)),
        _ => None,
    }
}

fn decode_utf8_strict(bytes: &[u8]) -> Result<String, String> {
    std::str::from_utf8(bytes)
        .map(str::to_string)
        .map_err(|_| describe_utf8_error(bytes))
}

/// `Path.read_text`'s own universal-newline translation: `\r\n` and a
/// lone `\r` both become `\n`; a lone `\n` is left alone. Applied
/// after UTF-8 decoding, before the JSON/YAML parse, so a CRLF or
/// CR-only replies file reports the identical `(char N)`/`line N`
/// position CPython's own `json.loads`/YAML loader would -- duplicated
/// from `electricity-compiler::load`'s own `translate_universal_
/// newlines` (that crate isn't this lane's to edit, and isn't a
/// dependency of this one).
fn translate_universal_newlines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(c);
        }
    }
    out
}

// ---------------------------------------------------------------------
// Python truthiness -- duplicated from `electricity-tools::fs`'s own
// `py_truthy` (same reason as above: not this lane's crate to edit).
// ---------------------------------------------------------------------

fn py_truthy(value: &Value) -> bool {
    match value {
        Value::None => false,
        Value::Bool(b) => *b,
        Value::Int(i) => !i.is_zero(),
        Value::Float(f) => *f != 0.0,
        Value::Str(s) => !s.is_empty(),
        Value::Bytes(b) => !b.is_empty(),
        Value::List(items) => !items.is_empty(),
        Value::Dict(d) => !d.is_empty(),
        Value::Date(_) | Value::DateTime(..) => true,
    }
}

// ---------------------------------------------------------------------
// The reply shapes (`adapters/scripted.py::_Reply`)
// ---------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Reply {
    Text {
        text: String,
        tokens_sent: Option<i64>,
        tokens_received: Option<i64>,
        finish_reason: Option<String>,
    },
    Error {
        kind: String,
        message: Option<String>,
        retry: RetryInfo,
    },
}

/// `adapters/scripted.py::_TEXT_REPLY_KEYS`.
const TEXT_REPLY_KEYS: &[&str] = &["text", "tokens_sent", "tokens_received", "finish_reason"];
/// `adapters/scripted.py::_ERROR_KEYS`.
const ERROR_KEYS: &[&str] = &["kind", "status", "message"];
/// `adapters/scripted.py::_DEFAULT_STATUS`'s own key set.
const KNOWN_KINDS: &[&str] = &[
    "timeout",
    "connection",
    "rate_limited",
    "server_error",
    "invalid_request",
    "unauthorized",
    "not_found",
    "http",
];

fn always_retryable(kind: &str) -> bool {
    matches!(kind, "timeout" | "connection")
}

/// `adapters/scripted.py::_DEFAULT_STATUS[kind]` -- *kind* is already
/// known to be one of [`KNOWN_KINDS`] by the time this is called.
fn default_status_for_kind(kind: &str) -> Option<i64> {
    match kind {
        "rate_limited" => Some(429),
        "server_error" => Some(500),
        "invalid_request" => Some(400),
        "unauthorized" => Some(401),
        "not_found" => Some(404),
        "timeout" | "connection" | "http" => None,
        _ => unreachable!("default_status_for_kind called with an unknown kind"),
    }
}

/// `adapters/_retry.py::_status_is_retryable`, but over the *full*
/// value a status carries (Python ints are unbounded) rather than
/// `electricity_vm::retry::status_is_retryable`'s `u16` -- a status
/// that doesn't fit `i64` (`IntValue::Big`) is, by construction, never
/// `429`/`408`/in `500..=599` (every one of those fits `i64`), so it
/// is simply not retryable by status.
fn status_is_retryable_unbounded(status: &IntValue) -> bool {
    match status {
        IntValue::Small(n) => {
            let n = *n;
            n == 429 || n == 408 || (500..=599).contains(&n)
        }
        IntValue::Big(_) => false,
    }
}

/// Settled question (issue #478): a `status` outside `u16`'s range
/// stores as `None` on the [`RetryInfo`] it ends up in -- the
/// classification itself ([`status_is_retryable_unbounded`]) still
/// uses the full value.
fn int_value_as_u16(value: &IntValue) -> Option<u16> {
    match value {
        IntValue::Small(n) => u16::try_from(*n).ok(),
        IntValue::Big(_) => None,
    }
}

fn scripted_error(message: String) -> AdapterError {
    AdapterError {
        message,
        retry: RetryInfo::not_retryable(),
        py_class: PyExc::ScriptedRepliesError,
    }
}

fn key(name: &str) -> Value {
    Value::Str(name.to_string())
}

/// `adapters/scripted.py::_reject_unknown_keys`.
fn reject_unknown_keys(dict: &Dict, allowed: &[&str], prefix: &str) -> Result<(), AdapterError> {
    let mut unknown: Vec<Value> = Vec::new();
    for k in dict.keys() {
        let is_allowed = matches!(k, Value::Str(s) if allowed.contains(&s.as_str()));
        if !is_allowed {
            unknown.push(k.clone());
        }
    }
    if unknown.is_empty() {
        return Ok(());
    }
    // Python's `sorted(set(entry) - allowed)` compares the keys
    // themselves -- for the `str` keys every real replies document
    // has, that's code-point order, not the order their `repr()`
    // would sort in (which a quote character inside the string can
    // reorder: `"it's"` reprs with a double quote, sorting before a
    // bare `'b'`). `Value::Str`'s `Ord` is UTF-8 byte order, which is
    // code-point order for valid UTF-8, so comparing the strings
    // directly matches. A non-`str` key (unreachable for a real
    // document; Python itself would raise `TypeError: '<' not
    // supported` sorting a mix) falls back to its own `repr()`.
    unknown.sort_by(|a, b| match (a, b) {
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        _ => a.py_repr().cmp(&b.py_repr()),
    });
    let unknown_repr = Value::List(unknown).py_repr();
    let mut allowed_sorted: Vec<&str> = allowed.to_vec();
    allowed_sorted.sort();
    let allowed_repr = Value::List(
        allowed_sorted
            .into_iter()
            .map(|s| Value::Str(s.to_string()))
            .collect(),
    )
    .py_repr();
    Err(scripted_error(format!(
        "{prefix}: unknown key(s) {unknown_repr}; one of {allowed_repr}"
    )))
}

/// `adapters/scripted.py::_parse_error`.
fn parse_error(dict: &Dict, prefix: &str) -> Result<Reply, AdapterError> {
    let error_value = dict
        .get(&key("error"))
        .expect("caller checked 'error' is present");
    let error_dict = match error_value.as_dict() {
        Some(d) if d.contains_key(&key("kind")) => d,
        _ => {
            return Err(scripted_error(format!(
                "{prefix}: 'error' must be a mapping with a 'kind'"
            )));
        }
    };
    reject_unknown_keys(error_dict, ERROR_KEYS, prefix)?;
    let kind_value = error_dict.get(&key("kind")).expect("checked above");
    if matches!(kind_value, Value::List(_) | Value::Dict(_)) {
        // `kind not in _DEFAULT_STATUS` hashes `kind` first -- an
        // unhashable value (a list/dict) raises Python's own
        // `TypeError` here, never a `ScriptedRepliesError`.
        return Err(AdapterError {
            message: format!("unhashable type: '{}'", kind_value.type_name()),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::TypeError,
        });
    }
    let kind = match kind_value {
        Value::Str(s) if KNOWN_KINDS.contains(&s.as_str()) => s.clone(),
        other => {
            let mut kinds_sorted: Vec<&str> = KNOWN_KINDS.to_vec();
            kinds_sorted.sort();
            let kinds_repr = Value::List(
                kinds_sorted
                    .into_iter()
                    .map(|s| Value::Str(s.to_string()))
                    .collect(),
            )
            .py_repr();
            return Err(scripted_error(format!(
                "{prefix}: unknown error kind {}; one of {kinds_repr}",
                other.py_repr()
            )));
        }
    };
    let status_present = error_dict.contains_key(&key("status"));
    let raw_status: Option<Value> = if status_present {
        error_dict.get(&key("status")).cloned()
    } else {
        default_status_for_kind(&kind).map(|n| Value::Int(IntValue::Small(n)))
    };
    // An explicit `status: null` and an absent key with no default
    // both resolve to "no status" here -- Python's own `.get(key,
    // default)` only substitutes `default` when the key is *absent*,
    // but `_DEFAULT_STATUS[kind]` is itself `None` for timeout/
    // connection/http, so the two cases end up equivalent either way.
    let status_opt: Option<Value> = raw_status.filter(|v| !matches!(v, Value::None));
    if let Some(status_value) = &status_opt {
        if !matches!(status_value, Value::Int(_)) {
            return Err(scripted_error(format!("{prefix}: 'status' must be an int")));
        }
    }
    if status_opt.is_none() && !always_retryable(&kind) {
        return Err(scripted_error(format!(
            "{prefix}: error kind {} needs a 'status'",
            Value::Str(kind.clone()).py_repr()
        )));
    }
    let message = match error_dict.get(&key("message")) {
        None | Some(Value::None) => None,
        Some(Value::Str(s)) => Some(s.clone()),
        Some(_) => {
            return Err(scripted_error(format!(
                "{prefix}: 'message' must be a string"
            )));
        }
    };
    let status_int = status_opt.as_ref().and_then(Value::as_int);
    let retryable = if always_retryable(&kind) {
        true
    } else {
        status_is_retryable_unbounded(status_int.expect("checked: needs a status above"))
    };
    let retry = RetryInfo {
        retryable,
        status: status_int.and_then(int_value_as_u16),
        retry_after: None,
    };
    Ok(Reply::Error {
        kind,
        message,
        retry,
    })
}

/// `adapters/scripted.py::_parse_reply`.
fn parse_reply(
    entry: &Value,
    path: &str,
    index: usize,
    source: &str,
) -> Result<Reply, AdapterError> {
    let prefix = format!("{source}: reply {index} for path '{path}'");
    let dict = match entry.as_dict() {
        Some(d) => d,
        None => return Err(scripted_error(format!("{prefix} must be a mapping"))),
    };
    let has_error = dict.contains_key(&key("error"));
    let has_text = dict.contains_key(&key("text"));
    if has_error {
        if has_text {
            return Err(scripted_error(format!(
                "{prefix}: exactly one of 'text' or 'error', not both"
            )));
        }
        return parse_error(dict, &prefix);
    }
    reject_unknown_keys(dict, TEXT_REPLY_KEYS, &prefix)?;
    let text = match dict.get(&key("text")) {
        Some(Value::Str(s)) => s.clone(),
        _ => {
            return Err(scripted_error(format!(
                "{prefix} needs a string 'text', or an 'error'"
            )));
        }
    };
    let tokens_sent = parse_token_count(dict, "tokens_sent", &prefix)?;
    let tokens_received = parse_token_count(dict, "tokens_received", &prefix)?;
    let finish_reason = match dict.get(&key("finish_reason")) {
        None | Some(Value::None) => None,
        Some(Value::Str(s)) => Some(s.clone()),
        Some(_) => {
            return Err(scripted_error(format!(
                "{prefix}: 'finish_reason' must be a string"
            )));
        }
    };
    Ok(Reply::Text {
        text,
        tokens_sent,
        tokens_received,
        finish_reason,
    })
}

/// `isinstance(value, int) and not isinstance(value, bool)` plus
/// `value >= 0`, both folded into one check -- Python's own
/// `_is_plain_int`/`value < 0` pair. Beyond that, a count that doesn't
/// fit `i64` (Python ints are unbounded; this engine's token counts
/// are not) is a documented divergence: rejected here at load time
/// with a new message Python has no equivalent of, rather than risk
/// a silent truncation or a panic later.
fn parse_token_count(dict: &Dict, field: &str, prefix: &str) -> Result<Option<i64>, AdapterError> {
    match dict.get(&key(field)) {
        None | Some(Value::None) => Ok(None),
        Some(Value::Int(i)) => {
            if i.is_negative() {
                return Err(scripted_error(format!(
                    "{prefix}: '{field}' must be an int >= 0"
                )));
            }
            match i {
                IntValue::Small(n) => Ok(Some(*n)),
                IntValue::Big(_) => Err(scripted_error(format!(
                    "{prefix}: '{field}' is too large for this engine to represent (Python's \
                     own int is unbounded; this engine's token counts are i64-bounded)"
                ))),
            }
        }
        Some(_) => Err(scripted_error(format!(
            "{prefix}: '{field}' must be an int >= 0"
        ))),
    }
}

/// `adapters/scripted.py::_parse_replies`.
fn parse_replies(
    raw: Value,
    source: &str,
) -> Result<HashMap<String, VecDeque<Reply>>, AdapterError> {
    let raw = if matches!(raw, Value::None) {
        Value::Dict(Dict::new())
    } else {
        raw
    };
    let dict = match raw.as_dict() {
        Some(d) => d,
        None => {
            return Err(scripted_error(format!(
                "{source}: replies file must be a mapping of path -> list of replies"
            )));
        }
    };
    let mut out = HashMap::with_capacity(dict.len());
    for (path_key, entries) in dict.iter() {
        let path = match path_key {
            Value::Str(s) if !s.is_empty() => s.clone(),
            _ => {
                return Err(scripted_error(format!(
                    "{source}: every key must be a non-empty string path"
                )));
            }
        };
        let list = match entries.as_list() {
            Some(l) if !l.is_empty() => l,
            _ => {
                return Err(scripted_error(format!(
                    "{source}: replies for path '{path}' must be a non-empty list"
                )));
            }
        };
        let mut queue = VecDeque::with_capacity(list.len());
        for (index, entry) in list.iter().enumerate() {
            queue.push_back(parse_reply(entry, &path, index, source)?);
        }
        out.insert(path, queue);
    }
    Ok(out)
}

/// `adapters/scripted.py::_load_raw`.
fn load_raw(path: &ResolvedPath) -> Result<Value, AdapterError> {
    let bytes = std::fs::read(&path.buf).map_err(|e| {
        scripted_error(format!(
            "scripted adapter: cannot read {}: {}",
            path.display,
            os_error_text(&e, &path.display)
        ))
    })?;
    let text = decode_utf8_strict(&bytes).map_err(|message| AdapterError {
        message,
        retry: RetryInfo::not_retryable(),
        py_class: PyExc::UnicodeDecodeError,
    })?;
    let text = translate_universal_newlines(&text);
    let is_json = path
        .buf
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext == "json")
        .unwrap_or(false);
    if is_json {
        electricity_json::loads(&text)
            .map_err(|e| scripted_error(format!("{}: invalid JSON: {e}", path.display)))
    } else {
        electricity_yaml::load_yaml(&text)
            .map_err(|e| scripted_error(format!("{}: invalid YAML: {e}", path.display)))
    }
}

/// `adapters/factory.py::_build_scripted`'s own `cfg.get("replies_file")
/// or "scripted-replies.yaml"` -- a falsy value (`""`, `null`, `0`,
/// `false`) already resolves to the default string here, at build
/// time; a truthy non-string value is kept as is, and only fails once
/// [`ScriptedAdapter::resolve_path`] is reached at load time.
fn resolved_replies_file(cfg: &Value) -> Value {
    let raw = cfg
        .as_dict()
        .and_then(|d| d.get(&key("replies_file")))
        .cloned()
        .unwrap_or(Value::None);
    if py_truthy(&raw) {
        raw
    } else {
        Value::Str("scripted-replies.yaml".to_string())
    }
}

type Queues = HashMap<String, VecDeque<Reply>>;

struct Inner {
    queues: Option<Arc<Mutex<Queues>>>,
    registered_for_export: bool,
}

/// `adapters/scripted.py::ScriptedAdapter` -- deterministic, no-network
/// `generate()` for conformance testing. One instance consumes its
/// replies file's queues for its own lifetime (`_queues`/`_lock`
/// ported as [`Inner`], behind a [`Mutex`]); two instances (a fallback
/// chain, or more than one `runtime.adapters.*` block naming
/// `scripted`) each load their own, independent copy.
pub struct ScriptedAdapter {
    replies_file: Value,
    inner: Mutex<Inner>,
}

impl ScriptedAdapter {
    fn new(replies_file: Value) -> Self {
        ScriptedAdapter {
            replies_file,
            inner: Mutex::new(Inner {
                queues: None,
                registered_for_export: false,
            }),
        }
    }

    /// `Path(replies_file).expanduser()` -- a truthy non-string config
    /// value fails here, not at build time, with CPython's own
    /// `os.fspath`/`Path(...)` text.
    fn resolve_path(&self) -> Result<ResolvedPath, AdapterError> {
        match &self.replies_file {
            Value::Str(s) => Ok(expand_user_path(s)),
            other => Err(AdapterError {
                message: format!(
                    "expected str, bytes or os.PathLike object, not {}",
                    other.type_name()
                ),
                retry: RetryInfo::not_retryable(),
                py_class: PyExc::TypeError,
            }),
        }
    }

    /// `adapters/scripted.py::ScriptedAdapter._ensure_loaded` -- lazy,
    /// per-instance load; a failed load leaves the instance unloaded,
    /// so the next call tries again. Registers this instance for the
    /// leftover-replies export (§7) the moment the load actually
    /// succeeds, under the same lock, so two calls racing to load the
    /// same fresh instance can never register it twice.
    fn ensure_loaded(&self) -> Result<Arc<Mutex<Queues>>, AdapterError> {
        let mut inner = self.inner.lock().unwrap();
        if inner.queues.is_none() {
            let path = self.resolve_path()?;
            let raw = load_raw(&path)?;
            let parsed = parse_replies(raw, &path.display)?;
            inner.queues = Some(Arc::new(Mutex::new(parsed)));
        }
        let queues = inner.queues.clone().expect("just populated above");
        if !inner.registered_for_export {
            let for_export = queues.clone();
            crate::register_for_leftover_export(Arc::new(move || {
                let guard = for_export.lock().unwrap();
                guard.iter().map(|(p, q)| (p.clone(), q.len())).collect()
            }));
            inner.registered_for_export = true;
        }
        Ok(queues)
    }
}

#[async_trait(?Send)]
impl Adapter for ScriptedAdapter {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn generate(
        &self,
        _model: &str,
        _prompt: &str,
        options: &GenerateOptions,
        call: &CallContext<'_>,
    ) -> Result<GenerateResult, AdapterError> {
        let queues = self.ensure_loaded()?;
        let path = call.path.to_string();
        let reply = {
            let mut guard = queues.lock().unwrap();
            match guard.get_mut(&path) {
                Some(queue) if !queue.is_empty() => queue.pop_front().expect("checked non-empty"),
                _ => {
                    return Err(AdapterError {
                        message: format!("scripted adapter: no reply configured for path '{path}'"),
                        retry: RetryInfo::not_retryable(),
                        py_class: PyExc::RuntimeError,
                    });
                }
            }
        };
        match reply {
            Reply::Error {
                kind,
                message,
                retry,
            } => {
                let text = message
                    .filter(|m| !m.is_empty())
                    .unwrap_or_else(|| format!("scripted adapter: {kind} at '{path}'"));
                Err(AdapterError {
                    message: text,
                    retry,
                    py_class: PyExc::AdapterCallError,
                })
            }
            Reply::Text {
                text,
                tokens_sent,
                tokens_received,
                finish_reason,
            } => {
                let mut raw = Dict::new();
                raw.insert(key("path"), Value::Str(path));
                Ok(GenerateResult {
                    text,
                    raw: Value::Dict(raw),
                    tokens_sent,
                    tokens_received,
                    finish_reason,
                    warnings: crate::ignored_options_warning("scripted", options, &[]),
                    cost_usd: None,
                })
            }
        }
    }

    /// `adapters/scripted.py::ScriptedAdapter.check` -- loads the file
    /// through the same lazy path, registering this instance for the
    /// export exactly as `generate()` does. Python's own `check()` only
    /// catches `ScriptedRepliesError`, folding it to `str(e)`; any other
    /// exception (`UnicodeDecodeError`, `TypeError`) propagates out of
    /// `check()` uncaught, for `preflight.py::call_check` to fold to
    /// `check() raised {type(exc).__name__}: {exc}`. [`CheckResult`] has
    /// no way to carry a raised exception separately from an ordinary
    /// `ok: false` result, so this reproduces `call_check`'s own text
    /// for every non-`ScriptedRepliesError` failure here directly,
    /// rather than leaving lane R's own fold to guess which kind it is.
    async fn check(&self) -> CheckResult {
        match self.ensure_loaded() {
            Ok(_) => CheckResult {
                ok: true,
                missing: Vec::new(),
                message: None,
            },
            Err(err) if err.py_class == PyExc::ScriptedRepliesError => CheckResult {
                ok: false,
                missing: Vec::new(),
                message: Some(err.message),
            },
            Err(err) => CheckResult {
                ok: false,
                missing: Vec::new(),
                message: Some(format!(
                    "check() raised {}: {}",
                    err.py_class.name(),
                    err.message
                )),
            },
        }
    }
}

/// `adapters/factory.py::_build_scripted`.
pub fn build_scripted(cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    Ok(Box::new(ScriptedAdapter::new(resolved_replies_file(cfg))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_bytecode::EffectPath;
    use electricity_value::CancellationToken;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    /// A scratch directory under the process's own temp dir, never the
    /// real `$HOME` -- removed on drop (same convention as
    /// `electricity-tools::fs`'s own test-only `TempDir`, duplicated
    /// here rather than shared: neither crate is a dependency of the
    /// other).
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!(
                "electricity-adapters-test-{}-{n}",
                std::process::id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            TempDir(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn call_ctx<'a>(path: &'a EffectPath, token: &'a CancellationToken) -> CallContext<'a> {
        CallContext {
            path,
            timeout_seconds: 30,
            token,
        }
    }

    /// Writes *contents* to a fresh scratch file named `replies<suffix>`
    /// inside a fresh [`TempDir`], returning both so the directory
    /// outlives the path.
    fn write_replies(contents: &str, suffix: &str) -> (TempDir, PathBuf) {
        let dir = TempDir::new();
        let path = dir.0.join(format!("replies{suffix}"));
        std::fs::write(&path, contents.as_bytes()).expect("write scratch replies file");
        (dir, path)
    }

    #[tokio::test]
    async fn the_spec_example_answers_three_paths_in_order() {
        let yaml = "prime.left:\n  - text: \"left reply\"\n    tokens_sent: 3\n    tokens_received: 1\nprime.right:\n  - text: \"right reply\"\n    tokens_sent: 5\n    tokens_received: 2\nprime.flaky:\n  - error:\n      kind: server_error\n  - text: \"recovered\"\n    tokens_sent: 4\n    tokens_received: 6\n";
        let (_dir, file) = write_replies(yaml, ".yaml");
        let adapter = ScriptedAdapter::new(Value::Str(file.to_string_lossy().into_owned()));
        let token = CancellationToken::new();

        let left_path = EffectPath::root().push_name("left");
        let result = adapter
            .generate(
                "m",
                "p",
                &GenerateOptions::default(),
                &call_ctx(&left_path, &token),
            )
            .await
            .unwrap();
        assert_eq!(result.text, "left reply");
        assert_eq!(result.tokens_sent, Some(3));

        let flaky_path = EffectPath::root().push_name("flaky");
        let err = adapter
            .generate(
                "m",
                "p",
                &GenerateOptions::default(),
                &call_ctx(&flaky_path, &token),
            )
            .await
            .unwrap_err();
        assert_eq!(err.py_class, PyExc::AdapterCallError);
        assert_eq!(
            err.message,
            "scripted adapter: server_error at 'prime.flaky'"
        );
        assert!(err.retry.retryable);
        assert_eq!(err.retry.status, Some(500));

        let result = adapter
            .generate(
                "m",
                "p",
                &GenerateOptions::default(),
                &call_ctx(&flaky_path, &token),
            )
            .await
            .unwrap();
        assert_eq!(result.text, "recovered");
    }

    #[tokio::test]
    async fn a_concretized_loop_pass_path_and_a_use_namespaced_path_both_match_by_their_dotted_string()
     {
        let yaml = "prime.shots.iter_2.describe:\n  - text: \"a cat sitting on a mat\"\nprime.first.answer:\n  - text: \"nested\"\n";
        let (_dir, file) = write_replies(yaml, ".yaml");
        let adapter = ScriptedAdapter::new(Value::Str(file.to_string_lossy().into_owned()));
        let token = CancellationToken::new();

        let mut pass_indices = std::collections::HashMap::new();
        let loop_id = electricity_bytecode::LoopId(0);
        pass_indices.insert(loop_id, 2u32);
        let compiled = EffectPath::root()
            .push_name("shots")
            .push_pass(loop_id)
            .push_name("describe");
        let resolved = compiled.resolve_passes(&pass_indices).unwrap();
        let result = adapter
            .generate(
                "m",
                "p",
                &GenerateOptions::default(),
                &call_ctx(&resolved, &token),
            )
            .await
            .unwrap();
        assert_eq!(result.text, "a cat sitting on a mat");

        let nested = EffectPath::root().push_name("first").push_name("answer");
        let result = adapter
            .generate(
                "m",
                "p",
                &GenerateOptions::default(),
                &call_ctx(&nested, &token),
            )
            .await
            .unwrap();
        assert_eq!(result.text, "nested");
    }

    #[tokio::test]
    async fn an_unmatched_path_fails_without_naming_the_replies_file() {
        let (_dir, file) = write_replies("prime.known:\n  - text: \"x\"\n", ".yaml");
        let adapter = ScriptedAdapter::new(Value::Str(file.to_string_lossy().into_owned()));
        let token = CancellationToken::new();
        let path = EffectPath::root().push_name("unknown");
        let err = adapter
            .generate(
                "m",
                "p",
                &GenerateOptions::default(),
                &call_ctx(&path, &token),
            )
            .await
            .unwrap_err();
        assert_eq!(
            err.message,
            "scripted adapter: no reply configured for path 'prime.unknown'"
        );
        assert!(!err.retry.retryable);
        assert_eq!(err.py_class, PyExc::RuntimeError);
    }

    #[tokio::test]
    async fn two_instances_over_one_file_have_independent_queues() {
        let (_dir, file) =
            write_replies("prime.x:\n  - text: \"one\"\n  - text: \"two\"\n", ".yaml");
        let path_str = file.to_string_lossy().into_owned();
        let a = ScriptedAdapter::new(Value::Str(path_str.clone()));
        let b = ScriptedAdapter::new(Value::Str(path_str));
        let token = CancellationToken::new();
        let path = EffectPath::root().push_name("x");

        let from_a = a
            .generate(
                "m",
                "p",
                &GenerateOptions::default(),
                &call_ctx(&path, &token),
            )
            .await
            .unwrap();
        let from_b = b
            .generate(
                "m",
                "p",
                &GenerateOptions::default(),
                &call_ctx(&path, &token),
            )
            .await
            .unwrap();
        assert_eq!(from_a.text, "one");
        // `b`'s own queue is untouched by `a`'s own pop -- both start
        // at the front of their *own* loaded copy.
        assert_eq!(from_b.text, "one");
    }

    #[tokio::test]
    async fn ignored_options_warning_lists_every_set_field_scripted_honours_none_of() {
        let (_dir, file) = write_replies("prime.x:\n  - text: \"ok\"\n", ".yaml");
        let adapter = ScriptedAdapter::new(Value::Str(file.to_string_lossy().into_owned()));
        let token = CancellationToken::new();
        let path = EffectPath::root().push_name("x");
        let options = GenerateOptions {
            temperature: Some(0.1),
            ..GenerateOptions::default()
        };
        let result = adapter
            .generate("m", "p", &options, &call_ctx(&path, &token))
            .await
            .unwrap();
        assert_eq!(
            result.warnings,
            vec!["adapter 'scripted' ignored: temperature".to_string()]
        );
    }

    #[tokio::test]
    async fn check_succeeds_on_a_well_formed_file_and_fails_on_a_malformed_one() {
        let (_good_dir, good) = write_replies("prime.x:\n  - text: \"ok\"\n", ".yaml");
        let adapter = ScriptedAdapter::new(Value::Str(good.to_string_lossy().into_owned()));
        let result = adapter.check().await;
        assert!(result.ok);
        assert!(result.message.is_none());

        let (_bad_dir, bad) = write_replies("not a mapping\n", ".yaml");
        // A plain scalar document parses as a YAML string, not a
        // mapping -- `_parse_replies` rejects it.
        let adapter = ScriptedAdapter::new(Value::Str(bad.to_string_lossy().into_owned()));
        let result = adapter.check().await;
        assert!(!result.ok);
        assert!(result.missing.is_empty());
        assert!(
            result
                .message
                .unwrap()
                .ends_with("replies file must be a mapping of path -> list of replies")
        );
    }

    #[tokio::test]
    async fn check_folds_a_type_error_the_way_preflights_call_check_does() {
        // `preflight.py::call_check` only ever sees `check()` raise
        // uncaught for a non-`ScriptedRepliesError` failure -- a truthy
        // non-string `replies_file` fails at load time with Python's
        // own `TypeError`, which Rust's `check()` must fold to the same
        // `check() raised {type(exc).__name__}: {exc}` text `call_check`
        // builds, rather than leaving it indistinguishable from an
        // ordinary `ScriptedRepliesError` result.
        let adapter = ScriptedAdapter::new(Value::Int(IntValue::Small(5)));
        let result = adapter.check().await;
        assert!(!result.ok);
        assert!(result.missing.is_empty());
        assert_eq!(
            result.message.unwrap(),
            "check() raised TypeError: expected str, bytes or os.PathLike object, not int"
        );
    }

    #[tokio::test]
    async fn check_folds_a_unicode_decode_error_the_way_preflights_call_check_does() {
        let (_dir, file) = write_replies("placeholder", ".yaml");
        std::fs::write(&file, [0xffu8, 0xfe]).unwrap();
        let adapter = ScriptedAdapter::new(Value::Str(file.to_string_lossy().into_owned()));
        let result = adapter.check().await;
        assert!(!result.ok);
        assert!(result.missing.is_empty());
        let message = result.message.unwrap();
        assert!(message.starts_with("check() raised UnicodeDecodeError: "));
    }

    #[tokio::test]
    async fn a_missing_replies_file_is_a_scripted_replies_error_naming_the_errno() {
        let adapter =
            ScriptedAdapter::new(Value::Str("/nonexistent/path/to/replies.yaml".to_string()));
        let token = CancellationToken::new();
        let path = EffectPath::root().push_name("x");
        let err = adapter
            .generate(
                "m",
                "p",
                &GenerateOptions::default(),
                &call_ctx(&path, &token),
            )
            .await
            .unwrap_err();
        assert_eq!(err.py_class, PyExc::ScriptedRepliesError);
        assert!(err.message.starts_with("scripted adapter: cannot read"));
        assert!(!err.retry.retryable);
    }

    #[tokio::test]
    async fn a_truthy_non_string_replies_file_fails_at_load_time_not_build_time() {
        let adapter = ScriptedAdapter::new(Value::Int(IntValue::Small(5)));
        let token = CancellationToken::new();
        let path = EffectPath::root().push_name("x");
        let err = adapter
            .generate(
                "m",
                "p",
                &GenerateOptions::default(),
                &call_ctx(&path, &token),
            )
            .await
            .unwrap_err();
        assert_eq!(err.py_class, PyExc::TypeError);
        assert_eq!(
            err.message,
            "expected str, bytes or os.PathLike object, not int"
        );
    }

    #[test]
    fn build_scripted_defaults_a_falsy_replies_file_to_the_conventional_name() {
        let adapter = build_scripted(&Value::Dict(Dict::new())).unwrap();
        // Only observable indirectly: `name()` is stable regardless.
        assert_eq!(adapter.name(), "scripted");
    }

    #[test]
    fn parse_replies_rejects_an_empty_key() {
        let mut dict = Dict::new();
        dict.insert(
            Value::Str(String::new()),
            Value::List(vec![Value::Dict(Dict::new())]),
        );
        let err = parse_replies(Value::Dict(dict), "<replies>").unwrap_err();
        assert_eq!(
            err.message,
            "<replies>: every key must be a non-empty string path"
        );
    }

    #[test]
    fn parse_replies_rejects_an_empty_list() {
        let mut dict = Dict::new();
        dict.insert(key("prime.x"), Value::List(Vec::new()));
        let err = parse_replies(Value::Dict(dict), "<replies>").unwrap_err();
        assert_eq!(
            err.message,
            "<replies>: replies for path 'prime.x' must be a non-empty list"
        );
    }

    #[test]
    fn a_null_document_is_treated_as_an_empty_mapping() {
        let parsed = parse_replies(Value::None, "<replies>").unwrap();
        assert!(parsed.is_empty());
    }

    #[test]
    fn a_reply_with_both_text_and_error_is_rejected() {
        let mut entry = Dict::new();
        entry.insert(key("text"), Value::Str("x".to_string()));
        let mut error = Dict::new();
        error.insert(key("kind"), Value::Str("timeout".to_string()));
        entry.insert(key("error"), Value::Dict(error));
        let err = parse_reply(&Value::Dict(entry), "prime.x", 0, "<replies>").unwrap_err();
        assert_eq!(
            err.message,
            "<replies>: reply 0 for path 'prime.x': exactly one of 'text' or 'error', not both"
        );
    }

    #[test]
    fn an_error_replys_sibling_keys_are_not_checked() {
        // Ruling (issue #478): Python doesn't check an error reply's
        // *other* sibling keys, only `error`'s own nested keys --
        // `{error: {...}, tokens_sent: 5}` loads silently.
        let mut entry = Dict::new();
        let mut error = Dict::new();
        error.insert(key("kind"), Value::Str("timeout".to_string()));
        entry.insert(key("error"), Value::Dict(error));
        entry.insert(key("tokens_sent"), Value::Int(IntValue::Small(5)));
        let reply = parse_reply(&Value::Dict(entry), "prime.x", 0, "<replies>").unwrap();
        assert!(matches!(reply, Reply::Error { .. }));
    }

    #[test]
    fn an_unknown_error_kind_names_the_sorted_list() {
        let mut entry = Dict::new();
        let mut error = Dict::new();
        error.insert(key("kind"), Value::Str("bogus".to_string()));
        entry.insert(key("error"), Value::Dict(error));
        let err = parse_reply(&Value::Dict(entry), "prime.x", 0, "<replies>").unwrap_err();
        assert_eq!(
            err.message,
            "<replies>: reply 0 for path 'prime.x': unknown error kind 'bogus'; one of \
             ['connection', 'http', 'invalid_request', 'not_found', 'rate_limited', \
             'server_error', 'timeout', 'unauthorized']"
        );
    }

    #[test]
    fn an_unhashable_error_kind_raises_a_type_error() {
        let mut entry = Dict::new();
        let mut error = Dict::new();
        error.insert(
            key("kind"),
            Value::List(vec![Value::Int(IntValue::Small(1))]),
        );
        entry.insert(key("error"), Value::Dict(error));
        let err = parse_reply(&Value::Dict(entry), "prime.x", 0, "<replies>").unwrap_err();
        assert_eq!(err.py_class, PyExc::TypeError);
        assert_eq!(err.message, "unhashable type: 'list'");
    }

    #[test]
    fn http_kind_requires_an_explicit_status() {
        let mut entry = Dict::new();
        let mut error = Dict::new();
        error.insert(key("kind"), Value::Str("http".to_string()));
        entry.insert(key("error"), Value::Dict(error));
        let err = parse_reply(&Value::Dict(entry), "prime.x", 0, "<replies>").unwrap_err();
        assert_eq!(
            err.message,
            "<replies>: reply 0 for path 'prime.x': error kind 'http' needs a 'status'"
        );
    }

    #[test]
    fn http_kind_classifies_by_its_explicit_status() {
        let mut entry = Dict::new();
        let mut error = Dict::new();
        error.insert(key("kind"), Value::Str("http".to_string()));
        error.insert(key("status"), Value::Int(IntValue::Small(503)));
        entry.insert(key("error"), Value::Dict(error));
        let reply = parse_reply(&Value::Dict(entry), "prime.x", 0, "<replies>").unwrap();
        match reply {
            Reply::Error { retry, .. } => {
                assert!(retry.retryable);
                assert_eq!(retry.status, Some(503));
            }
            _ => panic!("expected an error reply"),
        }
    }

    #[test]
    fn timeout_is_always_retryable_with_no_status_by_default() {
        let mut entry = Dict::new();
        let mut error = Dict::new();
        error.insert(key("kind"), Value::Str("timeout".to_string()));
        entry.insert(key("error"), Value::Dict(error));
        let reply = parse_reply(&Value::Dict(entry), "prime.x", 0, "<replies>").unwrap();
        match reply {
            Reply::Error { retry, .. } => {
                assert!(retry.retryable);
                assert_eq!(retry.status, None);
            }
            _ => panic!("expected an error reply"),
        }
    }

    #[test]
    fn an_explicit_null_status_is_only_legal_for_timeout_and_connection() {
        let mut entry = Dict::new();
        let mut error = Dict::new();
        error.insert(key("kind"), Value::Str("server_error".to_string()));
        error.insert(key("status"), Value::None);
        entry.insert(key("error"), Value::Dict(error));
        let err = parse_reply(&Value::Dict(entry), "prime.x", 0, "<replies>").unwrap_err();
        assert_eq!(
            err.message,
            "<replies>: reply 0 for path 'prime.x': error kind 'server_error' needs a 'status'"
        );
    }

    #[test]
    fn a_status_out_of_u16_range_is_stored_as_none_but_still_classified() {
        let big = IntValue::parse_decimal("99999999999999999999999999").unwrap();
        let mut entry = Dict::new();
        let mut error = Dict::new();
        error.insert(key("kind"), Value::Str("server_error".to_string()));
        error.insert(key("status"), Value::Int(big));
        entry.insert(key("error"), Value::Dict(error));
        let reply = parse_reply(&Value::Dict(entry), "prime.x", 0, "<replies>").unwrap();
        match reply {
            Reply::Error { retry, .. } => {
                assert_eq!(retry.status, None);
                assert!(!retry.retryable);
            }
            _ => panic!("expected an error reply"),
        }
    }

    #[test]
    fn a_token_count_beyond_i64_is_rejected_at_load_time() {
        let big = IntValue::parse_decimal("99999999999999999999999999").unwrap();
        let mut entry = Dict::new();
        entry.insert(key("text"), Value::Str("x".to_string()));
        entry.insert(key("tokens_sent"), Value::Int(big));
        let err = parse_reply(&Value::Dict(entry), "prime.x", 0, "<replies>").unwrap_err();
        assert!(err.message.contains("too large"));
        assert_eq!(err.py_class, PyExc::ScriptedRepliesError);
    }

    #[test]
    fn unknown_text_reply_keys_sort_by_string_value_not_by_repr() {
        // Python's `sorted({"b", "it's"})` gives `['b', "it's"]` --
        // sorting by `repr()` instead would put `"it's"` first (a
        // double quote sorts before `b`).
        let mut entry = Dict::new();
        entry.insert(key("text"), Value::Str("x".to_string()));
        entry.insert(key("b"), Value::Int(IntValue::Small(1)));
        entry.insert(key("it's"), Value::Int(IntValue::Small(2)));
        let err = parse_reply(&Value::Dict(entry), "prime.x", 0, "<replies>").unwrap_err();
        assert_eq!(
            err.message,
            "<replies>: reply 0 for path 'prime.x': unknown key(s) ['b', \"it's\"]; one of \
             ['finish_reason', 'text', 'tokens_received', 'tokens_sent']"
        );
    }

    #[test]
    fn unknown_text_reply_keys_are_rejected() {
        let mut entry = Dict::new();
        entry.insert(key("text"), Value::Str("x".to_string()));
        entry.insert(key("token_sent"), Value::Int(IntValue::Small(1)));
        let err = parse_reply(&Value::Dict(entry), "prime.x", 0, "<replies>").unwrap_err();
        assert_eq!(
            err.message,
            "<replies>: reply 0 for path 'prime.x': unknown key(s) ['token_sent']; one of \
             ['finish_reason', 'text', 'tokens_received', 'tokens_sent']"
        );
    }

    #[test]
    fn a_boolean_token_count_is_rejected_like_any_other_non_int() {
        let mut entry = Dict::new();
        entry.insert(key("text"), Value::Str("x".to_string()));
        entry.insert(key("tokens_sent"), Value::Bool(true));
        let err = parse_reply(&Value::Dict(entry), "prime.x", 0, "<replies>").unwrap_err();
        assert_eq!(
            err.message,
            "<replies>: reply 0 for path 'prime.x': 'tokens_sent' must be an int >= 0"
        );
    }

    #[test]
    fn a_dot_json_suffix_parses_as_plain_json() {
        let (_dir, file) = write_replies("{\"prime.x\": [{\"text\": \"json reply\"}]}", ".json");
        let path = expand_user_path(&file.to_string_lossy());
        let raw = load_raw(&path).unwrap();
        let parsed = parse_replies(raw, &path.display).unwrap();
        assert_eq!(parsed.get("prime.x").unwrap().len(), 1);
    }

    #[test]
    fn invalid_json_names_the_path_and_wraps_the_decode_error() {
        let (_dir, file) = write_replies("not json", ".json");
        let path = expand_user_path(&file.to_string_lossy());
        let err = load_raw(&path).unwrap_err();
        assert_eq!(err.py_class, PyExc::ScriptedRepliesError);
        assert!(
            err.message
                .starts_with(&format!("{}: invalid JSON: ", path.display))
        );
    }

    #[test]
    fn invalid_yaml_names_the_path_and_wraps_the_parse_error() {
        let (_dir, file) = write_replies("prime.x: [unterminated\n", ".yaml");
        let path = expand_user_path(&file.to_string_lossy());
        let err = load_raw(&path).unwrap_err();
        assert_eq!(err.py_class, PyExc::ScriptedRepliesError);
        assert!(
            err.message
                .starts_with(&format!("{}: invalid YAML: ", path.display))
        );
    }

    #[test]
    fn invalid_utf8_propagates_unwrapped_not_as_a_scripted_replies_error() {
        let (_dir, file) = write_replies("placeholder", ".yaml");
        std::fs::write(&file, [0xffu8, 0xfe]).unwrap();
        let path = expand_user_path(&file.to_string_lossy());
        let err = load_raw(&path).unwrap_err();
        assert_eq!(err.py_class, PyExc::UnicodeDecodeError);
        assert!(!err.message.starts_with(&path.display));
    }

    #[test]
    fn invalid_json_with_crlf_line_endings_reports_the_same_position_as_lf() {
        // `Path.read_text`'s own universal-newline translation runs
        // before the JSON parse -- a CRLF file with a syntax error on
        // line 2 must report the same `line 2` CPython would, not a
        // position shifted by the extra `\r` bytes.
        let (_dir, crlf_file) = write_replies("{\r\n  \"a\": ,\r\n}", ".json");
        let (_dir2, lf_file) = write_replies("{\n  \"a\": ,\n}", ".json");
        let crlf_path = expand_user_path(&crlf_file.to_string_lossy());
        let lf_path = expand_user_path(&lf_file.to_string_lossy());
        let crlf_err = load_raw(&crlf_path).unwrap_err();
        let lf_err = load_raw(&lf_path).unwrap_err();
        let crlf_suffix = crlf_err
            .message
            .strip_prefix(&format!("{}: invalid JSON: ", crlf_path.display))
            .unwrap()
            .to_string();
        let lf_suffix = lf_err
            .message
            .strip_prefix(&format!("{}: invalid JSON: ", lf_path.display))
            .unwrap()
            .to_string();
        assert_eq!(crlf_suffix, lf_suffix);
    }

    #[test]
    fn expand_user_path_normalizes_a_double_slash_and_a_leading_dot_slash() {
        assert_eq!(expand_user_path("./r.yaml").display, "r.yaml");
        assert_eq!(expand_user_path("a//b.yaml").display, "a/b.yaml");
    }
}
