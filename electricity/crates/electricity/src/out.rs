//! `--out` and the non-TTY stdout contract (issue #431's run-wiring
//! step 20 and "CLI output" decision) -- `cli/app.py::_write_state_json`
//! and `core/saved_state.py::dumps_saved_state`, ported: plain
//! `json.dumps` insertion order, or `--pretty` sorted with indent 2;
//! `ensure_ascii`; a trailing newline. `last` -> `$ref` compaction has
//! already happened by the time a caller gets here -- `Store::saved`'s
//! own job, not this module's.

use electricity_value::{Dict, Value};
use std::fs;
use std::io;
use std::path::Path;

/// A path's own text exactly as Python's `str(pathlib.PurePosixPath(...))`
/// would render it (PR #441 review finding 15): every `.`-only
/// segment and every doubled/trailing separator dropped, `..`
/// segments kept literally (never resolved -- this is string
/// normalization only, not `realpath`), collapsing to `.` for an
/// empty relative path or `/` for an empty absolute one. `Path::
/// display()` on a plain Rust `PathBuf` keeps a leading `./` or a
/// doubled `//` verbatim; every field this crate's own `--out`/stdout
/// renders from a path the CLI was given as-is -- never one this
/// crate resolved itself -- goes through this first instead, so a
/// user who wrote `./orchestration.yml` sees `orchestration.yml` in
/// `runtime.last_run.orchestration_path`, `state_out` and
/// `effective_settings.out`, exactly as a real `cof run` of the same
/// argument would.
pub fn python_path_str(path: &Path) -> String {
    let text = path.to_string_lossy();
    let absolute = text.starts_with('/');
    let segments: Vec<&str> = text
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .collect();
    if segments.is_empty() {
        return if absolute {
            "/".to_string()
        } else {
            ".".to_string()
        };
    }
    if absolute {
        format!("/{}", segments.join("/"))
    } else {
        segments.join("/")
    }
}

/// *state*'s saved-form JSON text, plus a trailing newline -- the exact
/// bytes `--out`/`--live-state` write (`core/saved_state.py::
/// dumps_saved_state`'s own `ensure_ascii=True` default, kept for a
/// file -- never the stdout JSON summary, which goes through Rich's
/// `console.print_json` instead; see [`render_state_for_stdout`]).
pub fn render_state(state: &Value, pretty: bool) -> String {
    let mode = if pretty {
        electricity_json::WriteMode::PRETTY
    } else {
        electricity_json::WriteMode::COMPACT
    };
    // `state` is always built by this crate's own `Store::saved` from a
    // document's own JSON/YAML-sourced values plus this run's own
    // metadata -- never a `Date`/`DateTime`/`Bytes` leaf nothing earlier
    // in the pipeline would already have rejected -- so `dumps` never
    // actually fails in practice. A caller reporting its own run result
    // must never panic doing so, though, so an encode failure still
    // falls back to a minimal, always-valid JSON object naming it
    // rather than unwrapping.
    match electricity_json::dumps(state, mode) {
        Ok(body) => format!("{body}\n"),
        Err(err) => {
            format!("{{\"error\":\"could not serialize run state: {}\"}}\n", err)
        }
    }
}

/// The non-TTY stdout JSON summary's own serialization (issue #431's
/// "CLI output" decision): `indent=2`, `ensure_ascii=False` always --
/// Rich's `console.print_json` always re-dumps with its own defaults,
/// regardless of how the string it's given was itself serialized
/// (`cli/app.py:1406,1451`) -- `sort_keys` only when *sort_keys* is
/// (the success-without-`--out` state print passes `--pretty` here;
/// the failure payload always passes `false`, since `cli/app.py`'s own
/// failure branch calls `console.print_json(json.dumps(payload))` with
/// no `sort_keys` of its own, never governed by `--pretty`).
fn stdout_json_mode(sort_keys: bool) -> electricity_json::WriteMode {
    electricity_json::WriteMode {
        indent: Some(2),
        sort_keys,
        ensure_ascii: false,
    }
}

/// *state*'s own stdout rendering for a successful run with no `--out`
/// to redirect to (issue #431's "CLI output" decision) -- distinct from
/// [`render_state`] (`--out`/`--live-state`'s own file bytes): the
/// terminal gets Rich's `console.print_json` re-dump instead
/// (`indent=2`, `ensure_ascii=False`, sorted exactly when `--pretty`
/// was given), never a trailing newline of its own (the caller's
/// `print!`/`println!` choice, not this function's).
pub fn render_state_for_stdout(state: &Value, pretty: bool) -> String {
    electricity_json::dumps(state, stdout_json_mode(pretty))
        .unwrap_or_else(|err| format!("{{\"error\": \"could not serialize run state: {}\"}}", err))
}

/// Writes *state*'s saved form to *path*, creating its parent directory
/// first (`cli/app.py::_write_state_json`).
pub fn write_out(path: &Path, state: &Value, pretty: bool) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    fs::write(path, render_state(state, pretty))
}

/// The failure payload electricity's own non-TTY stdout contract prints
/// (issue #431's "CLI output" decision) -- `cof run`'s own `{"ok":
/// false, "error": ..., "warnings": [...], "state_out": ...}`, in that
/// exact key order.
pub fn failure_payload(error: &str, warnings: &[String], state_out: Option<&Path>) -> String {
    let mut payload = Dict::new();
    payload.insert(Value::Str("ok".to_string()), Value::Bool(false));
    payload.insert(
        Value::Str("error".to_string()),
        Value::Str(error.to_string()),
    );
    payload.insert(
        Value::Str("warnings".to_string()),
        Value::List(warnings.iter().map(|w| Value::Str(w.clone())).collect()),
    );
    payload.insert(
        Value::Str("state_out".to_string()),
        match state_out {
            Some(path) => Value::Str(python_path_str(path)),
            None => Value::None,
        },
    );
    let body = electricity_json::dumps(&Value::Dict(payload), stdout_json_mode(false))
        .unwrap_or_else(|_| "{\n  \"ok\": false\n}".to_string());
    format!("{body}\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use indexmap::IndexMap;

    #[test]
    fn render_state_is_compact_and_ends_with_a_newline_by_default() {
        let mut dict = IndexMap::new();
        dict.insert(Value::Str("b".to_string()), Value::from(1i64));
        dict.insert(Value::Str("a".to_string()), Value::from(2i64));
        let rendered = render_state(&Value::Dict(dict), false);
        assert_eq!(rendered, "{\"b\": 1, \"a\": 2}\n");
    }

    #[test]
    fn render_state_pretty_sorts_keys_and_indents() {
        let mut dict = IndexMap::new();
        dict.insert(Value::Str("b".to_string()), Value::from(1i64));
        dict.insert(Value::Str("a".to_string()), Value::from(2i64));
        let rendered = render_state(&Value::Dict(dict), true);
        assert_eq!(rendered, "{\n  \"a\": 2,\n  \"b\": 1\n}\n");
    }

    #[test]
    fn write_out_creates_parent_directories() {
        let dir = std::env::temp_dir().join(format!("electricity-out-test-{}", std::process::id()));
        let path = dir.join("nested").join("state.json");
        write_out(&path, &Value::Dict(Dict::new()), false).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "{}\n");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn failure_payload_has_the_cof_key_order_indent_2_and_is_never_sorted() {
        // Rich's own `console.print_json` always re-dumps with
        // `indent=2`/`ensure_ascii=False`, and the failure branch never
        // passes `sort_keys` (`cli/app.py`'s own
        // `console.print_json(json.dumps(payload))`) -- so this stays
        // in `ok, error, warnings, state_out` order regardless of
        // `--pretty` (issue #431 PR #441 review finding 2).
        let payload = failure_payload(
            "boom",
            &["careful".to_string()],
            Some(Path::new("run/out.json")),
        );
        assert_eq!(
            payload,
            "{\n  \"ok\": false,\n  \"error\": \"boom\",\n  \"warnings\": [\n    \"careful\"\n  ],\n  \"state_out\": \"run/out.json\"\n}\n"
        );
    }

    #[test]
    fn failure_payload_state_out_is_null_when_absent() {
        let payload = failure_payload("boom", &[], None);
        assert_eq!(
            payload,
            "{\n  \"ok\": false,\n  \"error\": \"boom\",\n  \"warnings\": [],\n  \"state_out\": null\n}\n"
        );
    }

    #[test]
    fn failure_payload_normalizes_a_leading_dot_slash_in_state_out() {
        // PR #441 review finding 15: `str(Path("./x"))` is `"x"` in
        // Python; `Path::display()` on a plain Rust `PathBuf` would
        // keep the `./` instead.
        let payload = failure_payload("boom", &[], Some(Path::new("./out.json")));
        assert!(payload.contains("\"state_out\": \"out.json\""), "{payload}");
    }

    #[test]
    fn python_path_str_matches_pythons_own_normalization() {
        assert_eq!(python_path_str(Path::new("./x")), "x");
        assert_eq!(python_path_str(Path::new("a/./b")), "a/b");
        assert_eq!(python_path_str(Path::new("a//b")), "a/b");
        assert_eq!(python_path_str(Path::new("./a/../b")), "a/../b");
        assert_eq!(python_path_str(Path::new(".")), ".");
        assert_eq!(python_path_str(Path::new("a/")), "a");
        assert_eq!(python_path_str(Path::new("/a/./b")), "/a/b");
        assert_eq!(python_path_str(Path::new("")), ".");
        assert_eq!(
            python_path_str(Path::new("orchestration.yml")),
            "orchestration.yml"
        );
    }
}
