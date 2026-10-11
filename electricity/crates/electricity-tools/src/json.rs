//! Lane C: the `json` tool (`plugins/json.py`) -- `parse`/`stringify`/
//! `extract`, with Circuitry's own exact messages (`JsonPlugin: parse
//! mode requires params['input'] as a string.`, `json: unknown mode
//! {mode!r}`) and `raw: {"mode": mode}`. The third-party JSON-decode text
//! wrapped into `json: parse failed: ...`/`json: extract input is a
//! string but not valid JSON: ...` is byte-identical to CPython's own
//! `json.JSONDecodeError` text too (issue #442, via `electricity_json::
//! ReadError::Syntax`'s Display), not merely at the same character
//! offset.

use crate::{CheckResult, ToolCall, ToolError, ToolPlugin, ToolResult};
use async_trait::async_trait;
use electricity_json::{Separators, WriteMode, dumps_default_str, loads};
use electricity_value::{Dict, Value};

/// `plugins/json.py::JsonPlugin` -- `mode: parse|stringify|extract`.
pub struct JsonTool;

fn get<'a>(params: &'a Value, key: &str) -> Option<&'a Value> {
    params.as_dict()?.get(&Value::Str(key.to_string()))
}

/// Python's `int(value)` coercion, as far as [`JsonTool::execute`]'s own
/// `stringify` mode needs it for an `indent:` param -- `bool`/`int`/
/// `float` convert the way CPython's `int()` builtin does (`int(True)
/// == 1`, truncating a float toward zero); a numeric-looking `str`
/// parses the same way `int("4")` does; anything else is a coercion
/// failure, reported with CPython's own `int()` wording (third-party
/// text, DESIGN.md §1/§12 -- only has to fail, not match verbatim).
fn python_int(value: &Value) -> Result<i64, String> {
    match value {
        Value::Bool(b) => Ok(i64::from(*b)),
        Value::Int(i) => i
            .to_string()
            .parse::<i64>()
            .map_err(|_| format!("int too large to convert: {i}")),
        Value::Float(f) => Ok(f.trunc() as i64),
        Value::Str(s) => s
            .trim()
            .parse::<i64>()
            .map_err(|_| format!("invalid literal for int() with base 10: {:?}", s)),
        other => Err(format!(
            "int() argument must be a string, a bytes-like object or a real number, not '{}'",
            other.type_name()
        )),
    }
}

/// `plugins/json.py::_PATH_TOKEN`'s own walk over an `extract` `path:`
/// -- a dotted name or a `[N]`/`[-N]` index, in either order
/// (`"foo.bar[0].baz"`). Returns `(value, found)`, `found: false` on
/// any miss (an absent key, a list index out of Python's own
/// (negative-wraps) range, or indexing into a non-container) -- never
/// an error for a miss, only for a malformed token.
fn walk_path(value: &Value, path: &str) -> Result<(Value, bool), ToolError> {
    let chars: Vec<char> = path.chars().collect();
    let mut cursor = value.clone();
    let mut pos = 0usize;
    while pos < chars.len() {
        if chars[pos] == '.' {
            pos += 1;
            continue;
        }
        if chars[pos] == '[' {
            let mut end = pos + 1;
            if end < chars.len() && chars[end] == '-' {
                end += 1;
            }
            let digits_start = end;
            while end < chars.len() && chars[end].is_ascii_digit() {
                end += 1;
            }
            if end == digits_start || end >= chars.len() || chars[end] != ']' {
                return Err(ToolError::message(format!(
                    "json: invalid path token at offset {pos}"
                )));
            }
            let index_text: String = chars[pos + 1..end].iter().collect();
            // Python's `int(index)` never fails here (the token already
            // matched `-?\d+`; CPython `int` has no width limit), so an
            // index too large for every real list is simply out of range
            // once it reaches `cursor[int(index)]` -- an `IndexError`,
            // caught the same as any other miss. An `i64` overflow here
            // is that same case (no real `Value::List` ever reaches
            // `i64::MAX` elements), so it's a miss too, never a panic.
            let Ok(index) = index_text.parse::<i64>() else {
                return Ok((Value::None, false));
            };
            pos = end + 1;
            match &cursor {
                Value::List(items) => match python_list_index(items.len(), index) {
                    Some(i) => cursor = items[i].clone(),
                    None => return Ok((Value::None, false)),
                },
                _ => return Ok((Value::None, false)),
            }
        } else if chars[pos].is_ascii_alphabetic() || chars[pos] == '_' {
            let start = pos;
            let mut end = pos + 1;
            while end < chars.len()
                && (chars[end].is_ascii_alphanumeric() || chars[end] == '_' || chars[end] == '-')
            {
                end += 1;
            }
            let key: String = chars[start..end].iter().collect();
            pos = end;
            match &cursor {
                Value::Dict(dict) => match dict.get(&Value::Str(key)) {
                    Some(v) => cursor = v.clone(),
                    None => return Ok((Value::None, false)),
                },
                _ => return Ok((Value::None, false)),
            }
        } else {
            return Err(ToolError::message(format!(
                "json: invalid path token at offset {pos}"
            )));
        }
    }
    Ok((cursor, true))
}

/// Python list indexing (negative indices count from the end), as
/// `plugins/json.py::_walk_path`'s own `try: cursor[int(index)] except
/// (IndexError, ValueError)` needs it -- `None` on any out-of-range
/// index, never a panic.
fn python_list_index(len: usize, index: i64) -> Option<usize> {
    let len = len as i64;
    let normalized = if index < 0 { index + len } else { index };
    if normalized < 0 || normalized >= len {
        None
    } else {
        Some(normalized as usize)
    }
}

#[async_trait(?Send)]
impl ToolPlugin for JsonTool {
    fn name(&self) -> &str {
        "json"
    }

    async fn execute(&self, params: Value, _call: &ToolCall<'_>) -> Result<ToolResult, ToolError> {
        // `mode = str(params.get("mode", "parse")).lower()` -- the
        // Python default applies only when the key is *absent*; an
        // explicit `mode: null` still becomes the literal string
        // `"none"`, same as `str(None).lower()`.
        let mode_value = get(&params, "mode")
            .cloned()
            .unwrap_or_else(|| Value::Str("parse".to_string()));
        let mode = mode_value.py_str().to_lowercase();

        let (value, raw_mode) = match mode.as_str() {
            "parse" => {
                let text = match get(&params, "input") {
                    Some(Value::Str(s)) => s.clone(),
                    _ => {
                        return Err(ToolError::message(
                            "JsonPlugin: parse mode requires params['input'] as a string."
                                .to_string(),
                        ));
                    }
                };
                let value = loads(&text)
                    .map_err(|err| ToolError::message(format!("json: parse failed: {err}")))?;
                (value, mode.clone())
            }
            "stringify" => {
                let input = get(&params, "input").cloned().unwrap_or(Value::None);
                let indent = match get(&params, "indent") {
                    None | Some(Value::None) => None,
                    Some(value) => {
                        let parsed = python_int(value).map_err(ToolError::message)?;
                        Some(parsed.clamp(0, u8::MAX as i64) as u8)
                    }
                };
                let text = dumps_default_str(
                    &input,
                    WriteMode {
                        indent,
                        sort_keys: false,
                        ensure_ascii: false,
                        separators: Separators::Default,
                    },
                )
                .map_err(|err| ToolError::message(err.to_string()))?;
                (Value::Str(text), mode.clone())
            }
            "extract" => {
                let mut subject = get(&params, "input").cloned().unwrap_or(Value::None);
                if let Value::Str(text) = &subject {
                    subject = loads(text).map_err(|err| {
                        ToolError::message(format!(
                            "json: extract input is a string but not valid JSON: {err}"
                        ))
                    })?;
                }
                let path = match get(&params, "path") {
                    Some(Value::Str(s)) if !s.is_empty() => s.clone(),
                    _ => {
                        return Err(ToolError::message(
                            "JsonPlugin: extract mode requires params['path'].".to_string(),
                        ));
                    }
                };
                let (found, hit) = walk_path(&subject, &path)?;
                let value = if hit {
                    found
                } else {
                    get(&params, "default").cloned().unwrap_or(Value::None)
                };
                (value, mode.clone())
            }
            _ => {
                return Err(ToolError::message(format!(
                    "json: unknown mode {}",
                    Value::Str(mode).py_repr()
                )));
            }
        };

        let mut raw = Dict::new();
        raw.insert(Value::Str("mode".to_string()), Value::Str(raw_mode));
        Ok(ToolResult::new(value, Value::Dict(raw)))
    }

    fn check(&self) -> CheckResult {
        // `plugins/json.py::JsonPlugin.check` -- no dependency of its
        // own (pure in-process parsing), so always ready.
        CheckResult {
            ok: true,
            missing: Vec::new(),
            message: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::CancellationToken;

    async fn run(tool: &JsonTool, params: Value) -> Result<ToolResult, ToolError> {
        let token = CancellationToken::new();
        let call = ToolCall {
            timeout_seconds: 30,
            config: &Value::None,
            token: &token,
            armed: false,
        };
        tool.execute(params, &call).await
    }

    fn dict(pairs: Vec<(&str, Value)>) -> Value {
        let mut d = Dict::new();
        for (k, v) in pairs {
            d.insert(Value::Str(k.to_string()), v);
        }
        Value::Dict(d)
    }

    #[test]
    fn json_tool_reports_its_own_name_and_is_always_ready() {
        let tool = JsonTool;
        assert_eq!(tool.name(), "json");
        assert!(tool.check().ok);
    }

    #[tokio::test]
    async fn parse_mode_decodes_a_json_string() {
        let tool = JsonTool;
        let params = dict(vec![
            ("mode", Value::from("parse")),
            ("input", Value::from("{\"a\": 1}")),
        ]);
        let result = run(&tool, params).await.unwrap();
        let mut expected = Dict::new();
        expected.insert(Value::Str("a".to_string()), Value::from(1i64));
        assert_eq!(result.value, Value::Dict(expected));
    }

    #[tokio::test]
    async fn parse_mode_requires_a_string_input() {
        let tool = JsonTool;
        let params = dict(vec![
            ("mode", Value::from("parse")),
            ("input", Value::from(1i64)),
        ]);
        let err = run(&tool, params).await.unwrap_err();
        assert_eq!(
            err.message,
            "JsonPlugin: parse mode requires params['input'] as a string."
        );
    }

    #[tokio::test]
    async fn mode_defaults_to_parse_when_absent() {
        let tool = JsonTool;
        let params = dict(vec![("input", Value::from("null"))]);
        let result = run(&tool, params).await.unwrap();
        assert_eq!(result.value, Value::None);
    }

    #[tokio::test]
    async fn an_explicit_null_mode_is_not_the_default() {
        let tool = JsonTool;
        let params = dict(vec![("mode", Value::None), ("input", Value::from("null"))]);
        let err = run(&tool, params).await.unwrap_err();
        assert_eq!(err.message, "json: unknown mode 'none'");
    }

    #[tokio::test]
    async fn stringify_mode_matches_json_dumps_default_str() {
        let tool = JsonTool;
        let mut input = Dict::new();
        input.insert(Value::Str("a".to_string()), Value::from(1i64));
        let params = dict(vec![
            ("mode", Value::from("stringify")),
            ("input", Value::Dict(input)),
        ]);
        let result = run(&tool, params).await.unwrap();
        assert_eq!(result.value, Value::from("{\"a\": 1}"));
    }

    #[tokio::test]
    async fn stringify_mode_honours_indent() {
        let tool = JsonTool;
        let mut input = Dict::new();
        input.insert(Value::Str("a".to_string()), Value::from(1i64));
        let params = dict(vec![
            ("mode", Value::from("stringify")),
            ("input", Value::Dict(input)),
            ("indent", Value::from(2i64)),
        ]);
        let result = run(&tool, params).await.unwrap();
        assert_eq!(result.value, Value::from("{\n  \"a\": 1\n}"));
    }

    #[tokio::test]
    async fn extract_mode_walks_a_dotted_indexed_path() {
        let tool = JsonTool;
        let mut inner = Dict::new();
        inner.insert(
            Value::Str("bar".to_string()),
            Value::List(vec![Value::from(1i64), Value::from(2i64)]),
        );
        let mut input = Dict::new();
        input.insert(Value::Str("foo".to_string()), Value::Dict(inner));
        let params = dict(vec![
            ("mode", Value::from("extract")),
            ("input", Value::Dict(input)),
            ("path", Value::from("foo.bar[1]")),
        ]);
        let result = run(&tool, params).await.unwrap();
        assert_eq!(result.value, Value::from(2i64));
    }

    #[tokio::test]
    async fn extract_mode_returns_default_on_an_index_too_large_for_i64() {
        // `JsonPlugin().execute(params={"mode": "extract", "input":
        // {"a": [1]}, "path": "a[99999999999999999999]", "default":
        // "d"})` returns `"d"` -- Python's `int()` parses the literal
        // fine, then `IndexError` turns the out-of-range index into a
        // miss; it never raises on the index itself.
        let tool = JsonTool;
        let params = dict(vec![
            ("mode", Value::from("extract")),
            (
                "input",
                dict(vec![("a", Value::List(vec![Value::from(1i64)]))]),
            ),
            ("path", Value::from("a[99999999999999999999]")),
            ("default", Value::from("d")),
        ]);
        let result = run(&tool, params).await.unwrap();
        assert_eq!(result.value, Value::from("d"));
    }

    #[tokio::test]
    async fn extract_mode_returns_default_on_a_negative_index_too_large_for_i64() {
        let tool = JsonTool;
        let params = dict(vec![
            ("mode", Value::from("extract")),
            (
                "input",
                dict(vec![("a", Value::List(vec![Value::from(1i64)]))]),
            ),
            ("path", Value::from("a[-99999999999999999999]")),
            ("default", Value::from("d")),
        ]);
        let result = run(&tool, params).await.unwrap();
        assert_eq!(result.value, Value::from("d"));
    }

    #[tokio::test]
    async fn extract_mode_returns_default_on_a_miss() {
        let tool = JsonTool;
        let params = dict(vec![
            ("mode", Value::from("extract")),
            ("input", Value::Dict(Dict::new())),
            ("path", Value::from("missing")),
            ("default", Value::from("fallback")),
        ]);
        let result = run(&tool, params).await.unwrap();
        assert_eq!(result.value, Value::from("fallback"));
    }

    #[tokio::test]
    async fn extract_mode_requires_a_non_empty_path() {
        let tool = JsonTool;
        let params = dict(vec![
            ("mode", Value::from("extract")),
            ("input", Value::Dict(Dict::new())),
        ]);
        let err = run(&tool, params).await.unwrap_err();
        assert_eq!(
            err.message,
            "JsonPlugin: extract mode requires params['path']."
        );
    }

    #[tokio::test]
    async fn extract_mode_parses_a_json_string_subject_first() {
        let tool = JsonTool;
        let params = dict(vec![
            ("mode", Value::from("extract")),
            ("input", Value::from("{\"a\": 1}")),
            ("path", Value::from("a")),
        ]);
        let result = run(&tool, params).await.unwrap();
        assert_eq!(result.value, Value::from(1i64));
    }

    #[tokio::test]
    async fn extract_mode_rejects_a_string_subject_that_is_not_json() {
        let tool = JsonTool;
        let params = dict(vec![
            ("mode", Value::from("extract")),
            ("input", Value::from("not json")),
            ("path", Value::from("a")),
        ]);
        let err = run(&tool, params).await.unwrap_err();
        assert!(
            err.message
                .starts_with("json: extract input is a string but not valid JSON:")
        );
    }

    #[tokio::test]
    async fn an_unknown_mode_names_itself_by_python_repr() {
        let tool = JsonTool;
        let params = dict(vec![("mode", Value::from("bogus"))]);
        let err = run(&tool, params).await.unwrap_err();
        assert_eq!(err.message, "json: unknown mode 'bogus'");
    }

    #[tokio::test]
    async fn raw_names_the_mode_actually_used() {
        let tool = JsonTool;
        let params = dict(vec![
            ("mode", Value::from("PARSE")),
            ("input", Value::from("1")),
        ]);
        let result = run(&tool, params).await.unwrap();
        let mut expected_raw = Dict::new();
        expected_raw.insert(Value::Str("mode".to_string()), Value::from("parse"));
        assert_eq!(result.raw, Value::Dict(expected_raw));
    }

    #[tokio::test]
    async fn stringify_mode_stringifies_a_non_string_key_the_way_json_dumps_does() {
        // A `yes:`/`1:` YAML key survives as `Value::Bool`/`Value::Int`
        // all the way to the `json` tool (the compiler keeps `ParamNode
        // ::Map` keyed by `Value`, not `String` -- issue #431's finding
        // 4) -- only `stringify`'s own JSON encoding finally turns it
        // into text, the same `"true"`/`"1"` text `json.dumps` writes
        // for a non-`str` key, never `"True"`/`Value::py_str`'s own
        // Python-repr spelling.
        let tool = JsonTool;
        let mut input = Dict::new();
        input.insert(Value::Bool(true), Value::from("yes-value"));
        input.insert(Value::Int(1i64.into()), Value::from("one-value"));
        let params = dict(vec![
            ("mode", Value::from("stringify")),
            ("input", Value::Dict(input)),
        ]);
        let result = run(&tool, params).await.unwrap();
        // `Value::Bool(true)` and `Value::Int(1)` collide as dict keys
        // under Python's own numeric-tower equality (`True == 1`,
        // `hash(True) == hash(1)`) -- a *second* insert under an
        // equal key overwrites the first, exactly as it would in a real
        // Python `dict` literal, so only the last-written entry
        // (`"one-value"`) survives to be stringified.
        assert_eq!(result.value, Value::from("{\"true\": \"one-value\"}"));
    }
}
