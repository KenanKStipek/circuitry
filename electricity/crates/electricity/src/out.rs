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

/// *state*'s saved-form JSON text, plus a trailing newline -- the exact
/// bytes `--out` writes, and (without `--out`) the exact bytes a
/// successful run with nothing to redirect to prints on stdout.
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
            Some(path) => Value::Str(path.display().to_string()),
            None => Value::None,
        },
    );
    let body = electricity_json::dumps(&Value::Dict(payload), electricity_json::WriteMode::COMPACT)
        .unwrap_or_else(|_| "{\"ok\":false}".to_string());
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
    fn failure_payload_has_the_cof_key_order() {
        let payload = failure_payload(
            "boom",
            &["careful".to_string()],
            Some(Path::new("/tmp/out.json")),
        );
        assert_eq!(
            payload,
            "{\"ok\": false, \"error\": \"boom\", \"warnings\": [\"careful\"], \"state_out\": \"/tmp/out.json\"}\n"
        );
    }

    #[test]
    fn failure_payload_state_out_is_null_when_absent() {
        let payload = failure_payload("boom", &[], None);
        assert_eq!(
            payload,
            "{\"ok\": false, \"error\": \"boom\", \"warnings\": [], \"state_out\": null}\n"
        );
    }
}
