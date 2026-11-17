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
/// would render it (PR #441): every `.`-only
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
///
/// A `Date`/`DateTime`/`Bytes` leaf -- a reflector's own
/// `meta.iterations[i].parsed`, a tool's `meta.params_rendered`, a `use`
/// effect's literal inputs, or an `interface.inputs` default with no
/// declared `type:`, each able to carry an unquoted YAML 1.1 date/`!!binary`
/// straight from the document -- is stringified via [`Value::py_str`]
/// instead of failing the whole write, the same as `core/saved_state.py`'s
/// own `json.dumps(saved, default=...)` (issue #512). Anything else `dumps`
/// can't encode (a NaN-keyed sort, nesting past [`crate::MAX_DEPTH`], ...)
/// still returns `Err`; the caller (`write_out`) turns that into a
/// nonzero exit rather than writing a `{"error": ...}` placeholder and
/// exiting 0.
pub fn render_state(state: &Value, pretty: bool) -> Result<String, electricity_json::WriteError> {
    let ordered = pretty_sort_if_needed(state, pretty)?;
    electricity_json::dumps_default_str(ordered.as_ref(), pretty_write_mode(pretty))
        .map(|body| format!("{body}\n"))
}

/// `--pretty`'s own sort rule (issue #778): Python's own `sorted()` order
/// when the keys compare directly -- all strings (code-point order) or
/// all numbers (`bool` counting as `0`/`1`) -- the same order plain
/// `json.dumps(..., sort_keys=True)` already gave for either shape.
/// Only when `sorted()` can't compare the keys at all (a mix of types,
/// e.g. the bool `true` beside the string `"true"`) does this fall back
/// to [`electricity_json::canonical_key_order`]'s own order: the JSON
/// text each key would write, then [`electricity_json::key_type_rank`]
/// as a tiebreak -- never insertion order. This is
/// [`electricity_json::pretty_key_order`], the same rule cof's own
/// `core/saved_state.py::pretty_key_order` uses; [`canonical_key_order`]
/// itself stays reserved for `electricity-vm`'s reflector-plan digest,
/// which must keep hashing a plan the same way regardless of this issue
/// (#810; see [`electricity_json::canonical_key_order`]'s own doc).
/// `core/saved_state.py::dumps_saved_state` pre-sorts the tree itself this
/// same way and writes the result with `sort_keys=False`; ported the same
/// way here: reorder every `Dict`'s entries first, then write with
/// `sort_keys` always `false` (see [`pretty_write_mode`]) --
/// `WriteMode::PRETTY`'s own `sort_keys: true` is deliberately never used
/// here, since other callers of that constant (`service`'s own saved
/// record, matching `plugins/service.py`'s independent `json.dumps(record,
/// indent=2, sort_keys=True)`) still mean literal `sort_keys=True`,
/// unchanged by this issue.
///
/// Not pretty: a borrow of *state* itself, no copy. A key that still can't
/// be written at all (a `List`/`Dict`/`Bytes`/`Date`/`DateTime` key --
/// never valid for `sort_keys=True` either) still raises, via the same
/// [`electricity_json::stringify_key`] plain writing already calls, since
/// its own text is computed before its type rank is ever looked up.
fn pretty_sort_if_needed(
    state: &Value,
    pretty: bool,
) -> Result<std::borrow::Cow<'_, Value>, electricity_json::WriteError> {
    if pretty {
        Ok(std::borrow::Cow::Owned(electricity_json::pretty_key_order(
            state,
        )?))
    } else {
        Ok(std::borrow::Cow::Borrowed(state))
    }
}

/// [`render_state`]/[`render_state_for_stdout`]'s own write mode: `--pretty`
/// always writes `indent=2`/`sort_keys=false` now -- the tree is already
/// reordered by [`pretty_sort_if_needed`] when *pretty* is true, so the
/// writer itself never needs to sort (see [`pretty_sort_if_needed`]'s own
/// doc for why this must not be `WriteMode::PRETTY`).
fn pretty_write_mode(pretty: bool) -> electricity_json::WriteMode {
    if pretty {
        electricity_json::WriteMode {
            indent: Some(2),
            sort_keys: false,
            ensure_ascii: true,
            separators: electricity_json::Separators::Default,
        }
    } else {
        electricity_json::WriteMode::COMPACT
    }
}

/// The non-TTY stdout JSON summary's own serialization (issue #431's
/// "CLI output" decision): `indent=2`, `ensure_ascii=False` always,
/// never sorted by the writer itself -- `sort_keys` is handled by
/// pre-reordering the tree instead (see [`pretty_sort_if_needed`]), the
/// same way [`render_state`]'s own write mode is (issue #778): the
/// success-without-`--out` state print pre-sorts exactly when `--pretty`
/// was given; the failure payload never does, since `cli/app.py`'s own
/// failure branch calls `console.print_json(json.dumps(payload))` with
/// no `sort_keys` of its own, never governed by `--pretty`.
fn stdout_json_mode() -> electricity_json::WriteMode {
    electricity_json::WriteMode {
        indent: Some(2),
        sort_keys: false,
        ensure_ascii: false,
        separators: electricity_json::Separators::Default,
    }
}

/// *state*'s own stdout rendering for a successful run with no `--out`
/// to redirect to (issue #431's "CLI output" decision) -- distinct from
/// [`render_state`] (`--out`/`--live-state`'s own file bytes): the
/// terminal gets Rich's `console.print_json` re-dump instead
/// (`indent=2`, `ensure_ascii=False`, sorted exactly when `--pretty`
/// was given), never a trailing newline of its own (the caller's
/// `print!`/`println!` choice, not this function's). Stringifies a
/// `Date`/`DateTime`/`Bytes` leaf the same way [`render_state`] does
/// (issue #512); anything else `dumps` can't encode (a non-string dict
/// key, nesting past [`crate::MAX_DEPTH`], ...) still returns `Err` --
/// the caller's job to turn into a nonzero exit with a stderr message,
/// the same as `cli/app.py::run_cmd`'s own `console.print_json(dumps_
/// saved_state(...))` crashing uncaught rather than printing a
/// placeholder and exiting 0.
///
/// Rich's `console.print_json` re-parses the JSON text it is handed
/// (`json.loads`) before re-dumping it for syntax highlighting -- every
/// JSON object key becomes a plain string there, so two dict entries
/// whose *text* is the same (the bool `true` beside the string `"true"`)
/// collapse into one entry, at the first one's position, holding the
/// last one's value (cof's console does this
/// on stdout, never on `--out`'s own file, which keeps both --
/// [`render_state`] never calls [`collapse_duplicate_text_keys`]).
pub fn render_state_for_stdout(
    state: &Value,
    pretty: bool,
) -> Result<String, electricity_json::WriteError> {
    let ordered = pretty_sort_if_needed(state, pretty)?;
    let collapsed = collapse_duplicate_text_keys(ordered.as_ref())?;
    electricity_json::dumps_default_str(&collapsed, stdout_json_mode())
}

/// *value* as a `console.print_json`-style `json.loads(json.dumps(value))`
/// round trip would reshape it, without ever writing the intermediate
/// text: every dict key becomes the plain JSON string [`electricity_json::
/// stringify_key`] would have written for it, and two keys that write the
/// same text collapse to the one `json.loads` would have kept -- the
/// *first* one's position (an `IndexMap::insert` on an existing key
/// updates the value in place rather than moving it), holding the *last*
/// one's value (whichever of the two writes later wins the resulting
/// Python dict's single slot for that text). See [`render_state_for_
/// stdout`].
fn collapse_duplicate_text_keys(value: &Value) -> Result<Value, electricity_json::WriteError> {
    match value {
        Value::Dict(dict) => {
            let mut ordered: indexmap::IndexMap<String, Value> =
                indexmap::IndexMap::with_capacity(dict.len());
            for (key, v) in dict.iter() {
                let text = electricity_json::stringify_key(key)?;
                let collapsed_v = collapse_duplicate_text_keys(v)?;
                ordered.insert(text, collapsed_v);
            }
            let mut out = Dict::new();
            for (text, v) in ordered {
                out.insert(Value::Str(text), v);
            }
            Ok(Value::Dict(out))
        }
        Value::List(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(collapse_duplicate_text_keys(item)?);
            }
            Ok(Value::List(out))
        }
        other => Ok(other.clone()),
    }
}

/// Writes *state*'s saved form to *path*, creating its parent directory
/// first (`cli/app.py::_write_state_json`). A state [`render_state`] still
/// can't serialize (issue #512) fails this call -- the caller
/// (`electricity-cli`'s `run_action`) reports it on stderr and exits
/// nonzero, rather than writing a `{"error": ...}` placeholder to *path*
/// and exiting 0.
pub fn write_out(path: &Path, state: &Value, pretty: bool) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let body = render_state(state, pretty)
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
    fs::write(path, body)
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
    let body = electricity_json::dumps(&Value::Dict(payload), stdout_json_mode())
        .unwrap_or_else(|_| "{\n  \"ok\": false\n}".to_string());
    format!("{body}\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use indexmap::IndexMap;

    #[test]
    fn render_state_is_compact_and_ends_with_a_newline_by_default() {
        let mut dict = IndexMap::new();
        dict.insert(Value::Str("b".to_string()), Value::from(1i64));
        dict.insert(Value::Str("a".to_string()), Value::from(2i64));
        let rendered = render_state(&Value::Dict(dict), false).unwrap();
        assert_eq!(rendered, "{\"b\": 1, \"a\": 2}\n");
    }

    #[test]
    fn render_state_pretty_sorts_keys_and_indents() {
        let mut dict = IndexMap::new();
        dict.insert(Value::Str("b".to_string()), Value::from(1i64));
        dict.insert(Value::Str("a".to_string()), Value::from(2i64));
        let rendered = render_state(&Value::Dict(dict), true).unwrap();
        assert_eq!(rendered, "{\n  \"a\": 2,\n  \"b\": 1\n}\n");
    }

    #[test]
    fn render_state_pretty_sorts_a_dict_mixing_key_types_by_json_text() {
        // Issue #778: `sort_keys=True`'s own native-value sort raises
        // `TypeError` comparing a bool key to a string key (CPython's
        // `sorted(dct.items())`, confirmed directly) -- `--pretty` falls
        // back to each key's own JSON text instead, so this must
        // succeed, and in the same order `core/saved_state.py::
        // pretty_key_order` produces: "input" < "mode" < "true"
        // lexicographically.
        let mut dict = IndexMap::new();
        dict.insert(Value::Bool(true), Value::Str("push".to_string()));
        dict.insert(
            Value::Str("mode".to_string()),
            Value::Str("parse".to_string()),
        );
        dict.insert(Value::Str("input".to_string()), Value::from(1i64));
        let rendered = render_state(&Value::Dict(dict), true).unwrap();
        assert_eq!(
            rendered,
            "{\n  \"input\": 1,\n  \"mode\": \"parse\",\n  \"true\": \"push\"\n}\n"
        );
    }

    #[test]
    fn render_state_pretty_ranks_a_bool_before_a_string_with_equal_json_text() {
        // Two distinct, Python-unequal keys with the same JSON text --
        // the bool `True` and the string `"true"` are never equal in
        // Python, so both survive as separate dict entries. Type rank
        // (bool before str), not insertion order, breaks the tie here --
        // the bool sorts first even though it was inserted second
        // (issue #778, cof's own `test_canonical_key_order_ranks_a_bool_
        // before_a_string_of_the_same_text`).
        let mut dict = IndexMap::new();
        dict.insert(Value::Str("true".to_string()), Value::from(2i64));
        dict.insert(Value::Bool(true), Value::from(1i64));
        let rendered = render_state(&Value::Dict(dict), true).unwrap();
        assert_eq!(rendered, "{\n  \"true\": 1,\n  \"true\": 2\n}\n");
    }

    #[test]
    fn render_state_pretty_sorts_an_all_int_keyed_dict_numerically() {
        // Issue #778's own regression: `--pretty` must sort an all-int
        // dict by the int itself (1, 2, 10), not by each key's own
        // decimal text ("1", "10", "2") -- `pretty_key_order` tries
        // Python's native `sorted()` order before ever falling back to
        // `canonical_key_order`'s JSON-text order. Pinned against cof's
        // own `dumps_saved_state({"extra": {10: "ten", 1: "one", 2:
        // "two"}}, pretty=True)`.
        let mut inner = IndexMap::new();
        inner.insert(Value::from(10i64), Value::Str("ten".to_string()));
        inner.insert(Value::from(1i64), Value::Str("one".to_string()));
        inner.insert(Value::from(2i64), Value::Str("two".to_string()));
        let mut dict = IndexMap::new();
        dict.insert(Value::Str("extra".to_string()), Value::Dict(inner));
        let rendered = render_state(&Value::Dict(dict), true).unwrap();
        assert_eq!(
            rendered,
            "{\n  \"extra\": {\n    \"1\": \"one\",\n    \"2\": \"two\",\n    \"10\": \"ten\"\n  }\n}\n"
        );
    }

    #[test]
    fn render_state_pretty_sorts_a_nan_keyed_dict_by_json_text_not_numeric_comparison() {
        // Issue #778: a `Float` `NaN` key makes the dict's keys
        // incomparable even though comparing it never panics --
        // `nan < 1` and `1 < nan` are both `false` -- so `--pretty`
        // must route it through `canonical_key_order`'s own order (by
        // each key's own JSON text, `"1" < "2" < "NaN"`) rather than
        // trust its own numeric-key fast path to tolerate it the same
        // way cof's `sorted()` does. Pinned against cof's own
        // `dumps_saved_state({"extra": {float("nan"): "nan-value", 1:
        // "one", 2: "two"}}, pretty=True)`.
        let mut inner = IndexMap::new();
        inner.insert(Value::Float(f64::NAN), Value::Str("nan-value".to_string()));
        inner.insert(Value::from(1i64), Value::Str("one".to_string()));
        inner.insert(Value::from(2i64), Value::Str("two".to_string()));
        let mut dict = IndexMap::new();
        dict.insert(Value::Str("extra".to_string()), Value::Dict(inner));
        let rendered = render_state(&Value::Dict(dict), true).unwrap();
        assert_eq!(
            rendered,
            "{\n  \"extra\": {\n    \"1\": \"one\",\n    \"2\": \"two\",\n    \"NaN\": \"nan-value\"\n  }\n}\n"
        );
    }

    #[test]
    fn render_state_pretty_sorts_non_ascii_string_keys_by_code_point() {
        // All-string keys sort by plain `sorted()` -- code-point order,
        // the same as `json.dumps(..., sort_keys=True)` -- not
        // `canonical_key_order`'s JSON-text fallback (though for an
        // all-string dict the two happen to agree, since a string key's
        // JSON text is the key itself). Expected bytes taken from cof's
        // own `dumps_saved_state({"\u00f1": 1, "z": 2, "a": 3, "\u00e9":
        // 4}, pretty=True)`; `render_state`'s own `ensure_ascii: true`
        // (see `pretty_write_mode`) escapes both non-ASCII keys the same
        // way cof's default `ensure_ascii=True` does.
        let mut dict = IndexMap::new();
        dict.insert(Value::Str("\u{f1}".to_string()), Value::from(1i64));
        dict.insert(Value::Str("z".to_string()), Value::from(2i64));
        dict.insert(Value::Str("a".to_string()), Value::from(3i64));
        dict.insert(Value::Str("\u{e9}".to_string()), Value::from(4i64));
        let rendered = render_state(&Value::Dict(dict), true).unwrap();
        assert_eq!(
            rendered,
            "{\n  \"a\": 3,\n  \"z\": 2,\n  \"\\u00e9\": 4,\n  \"\\u00f1\": 1\n}\n"
        );
    }

    #[test]
    fn render_state_for_stdout_collapses_keys_with_equal_json_text_when_pretty() {
        // cof's console re-parses the already-serialized JSON for
        // syntax highlighting, so the bool `true` and the string
        // `"true"` -- two distinct dict entries up to this point --
        // collapse into cof's console's own single slot for the text
        // `"true"`: the first one's position, the last one's value --
        // `--pretty`'s own tie-break (bool before str) means the bool
        // entry is serialized first here, so the string entry's value
        // is the one that survives (issue #778).
        let mut dict = IndexMap::new();
        dict.insert(Value::Str("true".to_string()), Value::from(2i64));
        dict.insert(Value::Bool(true), Value::from(1i64));
        let rendered = render_state_for_stdout(&Value::Dict(dict), true).unwrap();
        assert_eq!(rendered, "{\n  \"true\": 2\n}");
    }

    #[test]
    fn render_state_for_stdout_collapses_keys_with_equal_json_text_without_pretty() {
        // The same collapse happens whether or not `--pretty` was given
        // -- cof's console re-parses on every success print, not only a
        // pretty one; only the key order differs (insertion order here,
        // since `pretty` is false): the bool entry is serialized first
        // (it was inserted first), so the string entry's value survives.
        let mut dict = IndexMap::new();
        dict.insert(Value::Bool(true), Value::from(1i64));
        dict.insert(Value::Str("true".to_string()), Value::from(2i64));
        let rendered = render_state_for_stdout(&Value::Dict(dict), false).unwrap();
        assert_eq!(rendered, "{\n  \"true\": 2\n}");
    }

    #[test]
    fn render_state_stringifies_a_date_datetime_and_bytes_leaf() {
        // Issue #512: an unquoted YAML 1.1 date/timestamp or `!!binary` in
        // a reflector's own `meta.iterations[i].parsed`, a tool's
        // `meta.params_rendered`, or similar, is written the way
        // `core/saved_state.py::dumps_saved_state`'s `default=` does it,
        // not rejected.
        let mut dict = IndexMap::new();
        dict.insert(
            Value::Str("d".to_string()),
            Value::Date(NaiveDate::from_ymd_opt(2024, 1, 2).unwrap()),
        );
        dict.insert(Value::Str("b".to_string()), Value::Bytes(b"hi".to_vec()));
        let rendered = render_state(&Value::Dict(dict), false).unwrap();
        assert_eq!(rendered, "{\"d\": \"2024-01-02\", \"b\": \"b'hi'\"}\n");
    }

    #[test]
    fn write_out_fails_on_a_value_default_str_still_cant_encode() {
        // A `Date` dict key: `default=` is never consulted for keys, in
        // Python either (`WriteError::KeyNotStrIntFloatBoolNone`) -- this
        // must still fail the write and the caller's exit code, never a
        // `{"error": ...}` placeholder written to *path* with a zero exit
        // (issue #512's electricity decision).
        let mut dict = IndexMap::new();
        dict.insert(
            Value::Date(NaiveDate::from_ymd_opt(2024, 1, 2).unwrap()),
            Value::from(1i64),
        );
        let dir = std::env::temp_dir().join(format!(
            "electricity-out-test-unserializable-{}",
            std::process::id()
        ));
        let path = dir.join("state.json");
        let err = write_out(&path, &Value::Dict(dict), false).unwrap_err();
        assert!(!path.exists());
        assert!(err.to_string().contains("keys must be str"), "{err}");
        fs::remove_dir_all(&dir).unwrap();
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
        // `--pretty` (issue #431, PR #441's review).
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
        // PR #441's review: `str(Path("./x"))` is `"x"` in
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
