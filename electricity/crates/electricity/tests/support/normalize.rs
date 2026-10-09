//! A Rust port of `electricity/scripts/_run_corpus.py`'s own
//! normalization rules, just enough of them to compare `electricity::
//! run_orchestration`'s own output against the committed golden run
//! corpus (`tests/golden/run_corpus.json`/`tool_run_corpus.json`):
//! a run id, a timestamp and a wall-clock duration all become a stable
//! placeholder, and *root* (a test's own temporary directory) is
//! replaced with the literal `<root>` everywhere it appears in a
//! string -- exactly `_run_corpus.py::_normalize`'s own two rules.
//!
//! What this port deliberately leaves out, and why:
//! - `--events`' own `pid`/`ts`/`ms`/`engine` top-level normalization
//!   (`_EVENT_KEYS`/`_UNCONDITIONAL_EVENT_KEYS`) and the tree-`dynamic`
//!   branch-reordering/`seq`/`id` renumbering (`canonical_event_order`)
//!   -- the one case here with real concurrency (`tree_dynamic`) is
//!   compared as a per-path multiset instead (this crate's own
//!   `run_corpus.rs`), the same latitude issue #431's own acceptance
//!   criteria gives a *cross-engine* `--events` comparison, since two
//!   engines (here: two runs of the very same engine, Python vs. this
//!   one) can order two concurrent branches' events differently without
//!   either being wrong.
//! - the CLI-invocation-shape fields no direct `run_orchestration`
//!   caller can ever match byte for byte against a real `cof run
//!   --out ...` subprocess's own: `orchestration_path`/
//!   `_orchestration_dir` (`cof` resolves a relative `orchestration.yml`
//!   against its own `cwd`; `run_orchestration` has no `cwd` of its
//!   own to vary -- changing the test process's real one would race
//!   every other parallel test in this binary) and
//!   `effective_settings.out`/`effective_settings.sources.out` (only
//!   ever set/`"cli"` when a real `--out` CLI flag was given, which a
//!   direct `run_orchestration` call never threads through the same
//!   way stdout/`--out`-file writing does). [`strip_invocation_shape_fields`]
//!   blanks exactly these four to a shared sentinel on both sides
//!   before comparing, so a real content difference elsewhere still
//!   fails loudly.

// This module is compiled fresh into each integration-test binary that
// declares `mod support;` -- `#[allow(dead_code)]` throughout since not
// every one of those binaries uses every function here (`tool_run_
// corpus.rs` only needs `normalize`/`strip_invocation_shape_fields`,
// not `normalize_event`).
#![allow(dead_code)]

use serde_json::Value;

const NORMALIZED_KEYS: &[(&str, &str)] = &[
    ("run_id", "<RUN_ID>"),
    ("_run_id", "<RUN_ID>"),
    ("started_at", "<TIMESTAMP>"),
    ("completed_at", "<TIMESTAMP>"),
    ("created_at", "<TIMESTAMP>"),
    ("_timestamp", "<TIMESTAMP>"),
    ("wall_time_s", "<DURATION>"),
];

fn placeholder_for(key: &str) -> Option<&'static str> {
    NORMALIZED_KEYS
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, p)| *p)
}

fn is_uuid(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    let dash_positions = [8, 13, 18, 23];
    for (i, b) in bytes.iter().enumerate() {
        if dash_positions.contains(&i) {
            if *b != b'-' {
                return false;
            }
        } else if !b.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

fn is_compact_timestamp(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() == 15
        && bytes[..8].iter().all(u8::is_ascii_digit)
        && bytes[8] == b'_'
        && bytes[9..].iter().all(u8::is_ascii_digit)
}

fn is_iso_timestamp_prefix(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.len() < 19 {
        return false;
    }
    let digit = |i: usize| bytes[i].is_ascii_digit();
    (0..4).all(digit)
        && bytes[4] == b'-'
        && (5..7).all(digit)
        && bytes[7] == b'-'
        && (8..10).all(digit)
        && bytes[10] == b'T'
        && (11..13).all(digit)
        && bytes[13] == b':'
        && (14..16).all(digit)
        && bytes[16] == b':'
        && (17..19).all(digit)
}

fn looks_environment_dependent(value: &Value) -> bool {
    match value {
        Value::Bool(_) => false,
        Value::Number(_) => true,
        Value::String(s) => is_uuid(s) || is_compact_timestamp(s) || is_iso_timestamp_prefix(s),
        _ => false,
    }
}

/// `_run_corpus.py::_normalize`, ported: *root* is replaced with
/// `<root>` in every string, at any depth; *key* (the enclosing dict
/// key this value was read from, if any) additionally selects a
/// placeholder from [`NORMALIZED_KEYS`] when the value itself still
/// looks environment-dependent after that substring replace.
pub fn normalize(key: Option<&str>, value: &Value, root: &str) -> Value {
    match value {
        Value::String(s) => {
            let replaced = s.replace(root, "<root>");
            if let Some(placeholder) = key.and_then(placeholder_for) {
                if looks_environment_dependent(&Value::String(replaced.clone())) {
                    return Value::String(placeholder.to_string());
                }
            }
            Value::String(replaced)
        }
        Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k.clone(), normalize(Some(k), v, root));
            }
            Value::Object(out)
        }
        Value::Array(items) => {
            Value::Array(items.iter().map(|v| normalize(key, v, root)).collect())
        }
        other => {
            if let Some(placeholder) = key.and_then(placeholder_for) {
                if looks_environment_dependent(other) {
                    return Value::String(placeholder.to_string());
                }
            }
            other.clone()
        }
    }
}

/// `_run_corpus.py::_EVENT_KEYS`/`_UNCONDITIONAL_EVENT_KEYS`, ported:
/// one `--events` line's own top-level keys only -- `engine` is always
/// replaced; `pid`/`ts`/`ms` only when environment-dependent -- every
/// other key (including a nested value inside one of them, which
/// never happens for these three but would for e.g. `path`/`error`)
/// goes through the ordinary [`normalize`].
pub fn normalize_event(event: &Value, root: &str) -> Value {
    let Some(map) = event.as_object() else {
        return normalize(None, event, root);
    };
    let mut out = serde_json::Map::with_capacity(map.len());
    for (key, value) in map {
        if key == "engine" {
            out.insert(key.clone(), Value::String("<ENGINE>".to_string()));
        } else if matches!(key.as_str(), "pid" | "ts" | "ms") && looks_environment_dependent(value)
        {
            out.insert(
                key.clone(),
                Value::String(format!("<{}>", key.to_uppercase())),
            );
        } else {
            out.insert(key.clone(), normalize(Some(key), value, root));
        }
    }
    Value::Object(out)
}

/// `core/cel_eval.py`'s own `f"CEL evaluation failed for {expr!r}: {exc}"`
/// prefix, kept verbatim -- only `{exc}` itself (everything after the
/// closing quote's own `": "`) is third-party text (`cel-python`'s own
/// stringified exception tuple vs. this crate's `electricity-cel`'s
/// own `Display`), so only that part is blanked (PR #441 review
/// finding 7: the pre-fix version blanked Circuitry's own prefix too,
/// and matched by shape -- any object with both an `expr` and an
/// `error` key -- rather than this field's one real location). Falls
/// back to blanking the whole string if it doesn't start with the
/// expected `CEL evaluation failed for <repr>: ` shape at all (a
/// genuinely different error this helper was never meant to touch),
/// so a real divergence there still fails loudly rather than silently
/// matching.
fn blanked_cel_detail(s: &str) -> String {
    const PREFIX: &str = "CEL evaluation failed for ";
    let Some(after_marker) = s.strip_prefix(PREFIX) else {
        return "<DETAIL>".to_string();
    };
    let Some(quote) = after_marker.chars().next() else {
        return "<DETAIL>".to_string();
    };
    if quote != '\'' && quote != '"' {
        return "<DETAIL>".to_string();
    }
    let Some(close_rel) = after_marker[1..].find(quote) else {
        return "<DETAIL>".to_string();
    };
    let close_at = PREFIX.len() + 1 + close_rel + 1;
    if !s[close_at..].starts_with(": ") {
        return "<DETAIL>".to_string();
    }
    let keep_until = close_at + 2;
    format!("{}<DETAIL>", &s[..keep_until])
}

/// Blanks `meta.expect.error`'s own detail text, recursively -- never
/// anywhere else an `error` key happens to appear (PR #441 review
/// finding 7), by walking with the last two keys in hand rather than
/// matching on shape: only a key named `error` whose immediate parent
/// dict's own key (in whatever dict holds it) is `expect`, and *that*
/// dict's own parent key is `meta`, is ever touched.
pub fn strip_expect_error_detail(value: &Value) -> Value {
    strip_expect_error_detail_at(value, None, None)
}

fn strip_expect_error_detail_at(
    value: &Value,
    parent_key: Option<&str>,
    grandparent_key: Option<&str>,
) -> Value {
    match value {
        Value::Object(map) => {
            let is_meta_expect = parent_key == Some("expect") && grandparent_key == Some("meta");
            Value::Object(
                map.iter()
                    .map(|(k, v)| {
                        if is_meta_expect && k == "error" {
                            let blanked = match v {
                                Value::String(s) => blanked_cel_detail(s),
                                _ => "<DETAIL>".to_string(),
                            };
                            (k.clone(), Value::String(blanked))
                        } else {
                            (
                                k.clone(),
                                strip_expect_error_detail_at(v, Some(k.as_str()), parent_key),
                            )
                        }
                    })
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|v| strip_expect_error_detail_at(v, parent_key, grandparent_key))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Blanks the CLI-invocation-shape fields this module's own doc
/// comment names, on a `state`-shaped [`Value`], to a shared sentinel
/// -- called on *both* sides of a comparison, so a real divergence
/// anywhere else still fails loudly.
pub fn strip_invocation_shape_fields(state: &Value) -> Value {
    const SENTINEL: &str = "<invocation-shape>";
    let mut state = state.clone();
    let Some(root) = state.as_object_mut() else {
        return state;
    };
    if let Some(runtime) = root.get_mut("runtime").and_then(Value::as_object_mut) {
        if let Some(last_run) = runtime.get_mut("last_run").and_then(Value::as_object_mut) {
            if last_run.contains_key("orchestration_path") {
                last_run.insert(
                    "orchestration_path".to_string(),
                    Value::String(SENTINEL.to_string()),
                );
            }
        }
        if let Some(effective) = runtime
            .get_mut("effective_settings")
            .and_then(Value::as_object_mut)
        {
            if effective.contains_key("out") {
                effective.insert("out".to_string(), Value::String(SENTINEL.to_string()));
            }
            if let Some(sources) = effective.get_mut("sources").and_then(Value::as_object_mut) {
                if sources.contains_key("out") {
                    sources.insert("out".to_string(), Value::String(SENTINEL.to_string()));
                }
            }
            if let Some(inner) = effective.get_mut("runtime").and_then(Value::as_object_mut) {
                if inner.contains_key("_orchestration_dir") {
                    inner.insert(
                        "_orchestration_dir".to_string(),
                        Value::String(SENTINEL.to_string()),
                    );
                }
            }
        }
    }
    Value::Object(root.clone())
}
