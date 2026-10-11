//! `plugins/regex.py::RegexPlugin`, ported: `match`/`search`/`findall`/
//! `sub` over Python `re` semantics, through [`translate`] (pattern
//! syntax) and [`replace`] (`sub`'s own substitution semantics).
//!
//! **Known divergences** (DESIGN.md §7.3; pinned by a `known_divergence`
//! conformance case rather than left silently different):
//! - `\N{NAME}` (a full Unicode character-name database) is rejected
//!   with the invalid-pattern prefix, not resolved;
//! - a lookbehind/pattern this translator can't prove fixed-width is
//!   rejected even where CPython's own, more precise analysis would
//!   accept it (`translate::Width`'s own conservative widening through
//!   a group boundary);
//! - `fancy-regex` hitting its own backtracking limit is reported as a
//!   `RuntimeError` (`regex: backtrack limit exceeded`), retryable like
//!   any other tool failure, where CPython's `re` (which ignores this
//!   plugin's `timeout_seconds`, like every other mode here) would run
//!   until the effect is killed (#448 decision, "Settled questions");
//! - IGNORECASE: `fancy-regex`'s own Unicode case folding is used
//!   directly (confirmed against CPython 3.11's `_sre.unicode_tolower`/
//!   `re._casefix._EXTRA_CASES` to match everywhere except Turkish
//!   dotless/dotted I -- `i`/`I`/`ı`/`İ` fold together under CPython,
//!   but `fancy-regex` keeps `ı` (U+0131) and `İ` (U+0130) apart from
//!   plain `i`/`I`).

pub mod replace;
pub mod translate;

use crate::fs::{get, value_error};
use crate::{CheckResult, ErrorCause, ToolCall, ToolError, ToolPlugin, ToolResult};
use async_trait::async_trait;
use electricity_value::{Dict, Value};
use translate::ResolvedFlags;

pub struct RegexPlugin;

fn invalid_pattern(message: impl std::fmt::Display) -> ToolError {
    value_error(format!("regex: invalid pattern: {message}"))
}

/// `_resolve_flags`: falsy -> no flags; a non-list -> `ValueError`; each
/// name is `str(name).upper()`, checked against the five known flags,
/// with the *original* item's own `repr` in the unknown-flag message.
fn resolve_flags(flags_param: Option<&Value>) -> Result<ResolvedFlags, ToolError> {
    let mut flags = ResolvedFlags::default();
    let Some(value) = flags_param else {
        return Ok(flags);
    };
    if !crate::fs::py_truthy(value) {
        return Ok(flags);
    }
    let Value::List(items) = value else {
        return Err(value_error(
            "regex: params['flags'] must be a list of flag names.",
        ));
    };
    for item in items {
        let key = item.py_str().to_uppercase();
        match key.as_str() {
            "IGNORECASE" => flags.ignorecase = true,
            "MULTILINE" => flags.multiline = true,
            "DOTALL" => flags.dotall = true,
            "VERBOSE" => flags.verbose = true,
            "ASCII" => flags.ascii = true,
            _ => {
                return Err(value_error(format!(
                    "regex: unknown flag {}",
                    item.py_repr()
                )));
            }
        }
    }
    Ok(flags)
}

/// `str(params.get("mode", "findall")).lower()` -- an explicit `null`
/// becomes `"none"`; a non-string value converts through `str()`
/// first; the default (key absent) is used verbatim, never
/// re-stringified.
fn resolve_mode(params: &Value) -> String {
    match get(params, "mode") {
        None => "findall".to_string(),
        Some(value) => value.py_str().to_lowercase(),
    }
}

fn group_value(m: Option<fancy_regex::Match>) -> Value {
    match m {
        Some(m) => Value::Str(m.as_str().to_string()),
        None => Value::None,
    }
}

/// `m.groups()`/one `findall` tuple element: a non-participating
/// group is `None` for `match`/`search`, but `''` for `findall`
/// (issue #479's own "Results" list) -- *empty_as_none* selects which.
fn group_value_for(m: Option<fancy_regex::Match>, empty_as_none: bool) -> Value {
    match m {
        Some(m) => Value::Str(m.as_str().to_string()),
        None if empty_as_none => Value::None,
        None => Value::Str(String::new()),
    }
}

fn match_or_search_value(caps: &fancy_regex::Captures) -> Value {
    let group_count = caps.len() - 1;
    if group_count == 0 {
        return group_value(caps.get(0));
    }
    let items = (1..=group_count)
        .map(|i| group_value_for(caps.get(i), true))
        .collect();
    Value::List(items)
}

fn findall_value(caps: &fancy_regex::Captures) -> Value {
    let group_count = caps.len() - 1;
    match group_count {
        0 => Value::Str(caps.get(0).unwrap().as_str().to_string()),
        1 => group_value_for(caps.get(1), false),
        _ => Value::List(
            (1..=group_count)
                .map(|i| group_value_for(caps.get(i), false))
                .collect(),
        ),
    }
}

#[async_trait(?Send)]
impl ToolPlugin for RegexPlugin {
    fn name(&self) -> &str {
        "regex"
    }

    async fn execute(&self, params: Value, _call: &ToolCall<'_>) -> Result<ToolResult, ToolError> {
        let pattern = match get(&params, "pattern") {
            Some(Value::Str(s)) if !s.is_empty() => s.clone(),
            _ => return Err(value_error("RegexPlugin requires params['pattern'].")),
        };
        let input = match get(&params, "input") {
            Some(Value::Str(s)) => s.clone(),
            _ => {
                return Err(value_error(
                    "RegexPlugin requires params['input'] as a string.",
                ));
            }
        };
        let mode = resolve_mode(&params);
        if !matches!(mode.as_str(), "match" | "search" | "findall" | "sub") {
            return Err(value_error(format!(
                "regex: unknown mode {}; expected match|search|findall|sub.",
                Value::Str(mode).py_repr()
            )));
        }

        let flags = resolve_flags(get(&params, "flags"))?;
        let translated = translate::translate(&pattern, flags).map_err(invalid_pattern)?;
        let compiled = fancy_regex::Regex::new(&translated.pattern).map_err(invalid_pattern)?;

        let value = match mode.as_str() {
            "match" => {
                let anchored = fancy_regex::Regex::new(&format!("\\A(?:{})", translated.pattern))
                    .map_err(invalid_pattern)?;
                match anchored.captures(&input).map_err(invalid_pattern)? {
                    Some(caps) => match_or_search_value(&caps),
                    None => Value::None,
                }
            }
            "search" => match compiled.captures(&input).map_err(invalid_pattern)? {
                Some(caps) => match_or_search_value(&caps),
                None => Value::None,
            },
            "findall" => {
                let matches = replace::find_iter(&compiled, &input).map_err(invalid_pattern)?;
                Value::List(matches.iter().map(findall_value).collect())
            }
            "sub" => {
                let replacement = match get(&params, "replacement") {
                    Some(Value::Str(s)) => s.clone(),
                    _ => {
                        return Err(value_error(
                            "regex: params['replacement'] required when mode='sub'.",
                        ));
                    }
                };
                let count_param = get(&params, "count")
                    .cloned()
                    .unwrap_or(Value::Int(0i64.into()));
                let count =
                    electricity_value::pycompat::py_int(&count_param).map_err(|e| ToolError {
                        py_class: e.py_class,
                        message: e.message,
                        cause: ErrorCause::None,
                    })?;
                let template = replace::parse_template(
                    &replacement,
                    translated.group_count,
                    &translated.group_names,
                )
                .map_err(value_error)?;
                let max = replace::max_replacements(count);
                let result = replace::substitute_n(&compiled, &template, &input, max)
                    .map_err(invalid_pattern)?;
                Value::Str(result)
            }
            _ => unreachable!(),
        };

        let mut raw = Dict::new();
        raw.insert(Value::Str("pattern".to_string()), Value::Str(pattern));
        raw.insert(Value::Str("mode".to_string()), Value::Str(mode));
        raw.insert(
            Value::Str("flags".to_string()),
            get(&params, "flags").cloned().unwrap_or(Value::None),
        );
        Ok(ToolResult::new(value, Value::Dict(raw)))
    }

    fn check(&self) -> CheckResult {
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

    fn dict(pairs: Vec<(&str, Value)>) -> Value {
        let mut d = Dict::new();
        for (k, v) in pairs {
            d.insert(Value::Str(k.to_string()), v);
        }
        Value::Dict(d)
    }

    async fn run(params: Value) -> Result<ToolResult, ToolError> {
        let token = CancellationToken::new();
        let call = ToolCall {
            timeout_seconds: 30,
            config: &Value::None,
            token: &token,
            armed: false,
        };
        RegexPlugin.execute(params, &call).await
    }

    #[tokio::test]
    async fn sub_with_backreference_replacement() {
        let result = run(dict(vec![
            ("pattern", Value::from(r"(\w+)@(\w+)")),
            ("input", Value::from("a@b")),
            ("mode", Value::from("sub")),
            ("replacement", Value::from(r"\2@\1")),
        ]))
        .await
        .unwrap();
        assert_eq!(result.value, Value::from("b@a"));
    }

    #[tokio::test]
    async fn sub_empty_match_iteration_matches_pythons_own_example() {
        let result = run(dict(vec![
            ("pattern", Value::from("x*")),
            ("input", Value::from("abxd")),
            ("mode", Value::from("sub")),
            ("replacement", Value::from("-")),
        ]))
        .await
        .unwrap();
        assert_eq!(result.value, Value::from("-a-b--d-"));
    }

    #[tokio::test]
    async fn sub_bad_replacement_escape_has_no_invalid_pattern_prefix() {
        let err = run(dict(vec![
            ("pattern", Value::from("a")),
            ("input", Value::from("a")),
            ("mode", Value::from("sub")),
            ("replacement", Value::from(r"\q")),
        ]))
        .await
        .unwrap_err();
        assert!(err.message.contains("bad escape"));
        assert!(!err.message.contains("invalid pattern"));
    }

    #[tokio::test]
    async fn findall_with_two_groups_gives_tuples_with_empty_for_non_participating() {
        let result = run(dict(vec![
            ("pattern", Value::from(r"(a)|(b)")),
            ("input", Value::from("ab")),
            ("mode", Value::from("findall")),
        ]))
        .await
        .unwrap();
        assert_eq!(
            result.value,
            Value::List(vec![
                Value::List(vec![Value::from("a"), Value::from("")]),
                Value::List(vec![Value::from(""), Value::from("b")]),
            ])
        );
    }

    #[tokio::test]
    async fn findall_with_no_groups_gives_whole_match_strings() {
        let result = run(dict(vec![
            ("pattern", Value::from(r"\d+")),
            ("input", Value::from("a1 b22")),
            ("mode", Value::from("findall")),
        ]))
        .await
        .unwrap();
        assert_eq!(
            result.value,
            Value::List(vec![Value::from("1"), Value::from("22")])
        );
    }

    #[tokio::test]
    async fn search_with_a_non_participating_group_is_none() {
        let result = run(dict(vec![
            ("pattern", Value::from(r"(a)|(b)")),
            ("input", Value::from("b")),
            ("mode", Value::from("search")),
        ]))
        .await
        .unwrap();
        assert_eq!(
            result.value,
            Value::List(vec![Value::None, Value::from("b")])
        );
    }

    #[tokio::test]
    async fn match_is_anchored_at_position_zero_not_a_full_match() {
        let result = run(dict(vec![
            ("pattern", Value::from("ab")),
            ("input", Value::from("abc")),
            ("mode", Value::from("match")),
        ]))
        .await
        .unwrap();
        assert_eq!(result.value, Value::from("ab"));

        let no_match = run(dict(vec![
            ("pattern", Value::from("bc")),
            ("input", Value::from("abc")),
            ("mode", Value::from("match")),
        ]))
        .await
        .unwrap();
        assert_eq!(no_match.value, Value::None);
    }

    #[tokio::test]
    async fn lowercase_flag_name_is_accepted() {
        let result = run(dict(vec![
            ("pattern", Value::from("ABC")),
            ("input", Value::from("abc")),
            ("mode", Value::from("search")),
            ("flags", Value::List(vec![Value::from("ignorecase")])),
        ]))
        .await
        .unwrap();
        assert_eq!(result.value, Value::from("abc"));
    }

    #[tokio::test]
    async fn unknown_flag_reports_the_original_items_repr() {
        let err = run(dict(vec![
            ("pattern", Value::from("a")),
            ("input", Value::from("a")),
            ("flags", Value::List(vec![Value::from("BOGUS")])),
        ]))
        .await
        .unwrap_err();
        assert_eq!(err.message, "regex: unknown flag 'BOGUS'");
    }

    #[tokio::test]
    async fn missing_pattern_is_a_value_error() {
        let err = run(dict(vec![("input", Value::from("a"))]))
            .await
            .unwrap_err();
        assert_eq!(err.message, "RegexPlugin requires params['pattern'].");
    }

    #[tokio::test]
    async fn unknown_mode_names_the_lowercased_value() {
        let err = run(dict(vec![
            ("pattern", Value::from("a")),
            ("input", Value::from("a")),
            ("mode", Value::from("BOGUS")),
        ]))
        .await
        .unwrap_err();
        assert_eq!(
            err.message,
            "regex: unknown mode 'bogus'; expected match|search|findall|sub."
        );
    }

    #[tokio::test]
    async fn invalid_pattern_gets_the_wrapped_prefix() {
        let err = run(dict(vec![
            ("pattern", Value::from("(")),
            ("input", Value::from("a")),
        ]))
        .await
        .unwrap_err();
        assert!(err.message.starts_with("regex: invalid pattern: "));
    }

    #[tokio::test]
    async fn raw_carries_the_flags_param_as_given_not_resolved() {
        let result = run(dict(vec![
            ("pattern", Value::from("a")),
            ("input", Value::from("a")),
            ("mode", Value::from("search")),
        ]))
        .await
        .unwrap();
        let raw = result.raw.as_dict().unwrap();
        assert_eq!(raw.get(&Value::from("flags")), Some(&Value::None));
        assert_eq!(raw.get(&Value::from("mode")), Some(&Value::from("search")));
    }
}
