//! `plugins/regex.py::RegexPlugin`, ported: `match`/`search`/`findall`/
//! `sub` over Python `re` semantics, through [`translate`] (pattern
//! syntax) and [`replace`] (`sub`'s own substitution semantics).
//!
//! **Known divergences** (DESIGN.md §7.3; each one accepted outright
//! in that section's own table rather than pinned by a conformance
//! case -- no `case.json` sets `known_divergence` at all, issue #539's
//! own settled question 1; `regex-ignorecase-turkish-i-divergence`'s
//! own name is misleading, since its own description calls it "an
//! ordinary parity case, not a divergence"):
//! - `\N{NAME}` (a full Unicode character-name database) is rejected
//!   with the invalid-pattern prefix, not resolved;
//! - a backreference *inside a lookbehind* is always rejected, even
//!   when the referenced group is itself fixed-width (CPython accepts
//!   that case) -- not this translator's own analysis (`translate::
//!   Width` does track each group's own width and uses it for exactly
//!   this check), but `fancy-regex`'s own compiler, which always
//!   reports a backreference as non-constant-width regardless
//!   (confirmed in its `analyze.rs`: `Expr::Backref` never sets
//!   `const_size`);
//! - `fancy-regex` hitting its own backtracking limit is reported as a
//!   `RuntimeError` (`regex: backtrack limit exceeded`), retryable like
//!   any other tool failure, where CPython's `re` (which ignores this
//!   plugin's `timeout_seconds`, like every other mode here) would run
//!   until the effect is killed (#448 decision, "Settled questions").
//!   The limit itself is sized per call, `max(FLOOR, PER_CHAR *
//!   (input_len + 1))` ([`electricity_pyre::backtrack_limit_for`],
//!   moved there in PR #502's round-5 fix so `electricity-schema`
//!   can size its own `pattern`/`patternProperties` limit the same
//!   way; `PER_CHAR = 100`,
//!   `FLOOR = 10_000_000`, each measured directly in a release build
//!   -- DESIGN.md §7.3 has the full measurements), since the crate's
//!   own fixed 1,000,000 default counts backtracks across every start
//!   position of a search, not just the eventual match, and so used
//!   to fail ordinary multi-hundred-KB inputs through the lookarounds
//!   this translator emits for `\b`/`\B`/non-MULTILINE `$`/lookbehind
//!   (PR #480 review, finding R8) even though the real per-character
//!   cost is tiny;
//! - IGNORECASE: `fancy-regex`'s own Unicode case folding is used
//!   directly for everything this translator doesn't expand itself
//!   (`translate::literal_outside_class`/`expand_case_in_class` now
//!   expand ASCII-mode literals/ranges and the Turkish dotless/dotted
//!   `ı`/`İ` pair explicitly, rather than leaving the latter a
//!   divergence);
//! - a shortcut class (`\w`/`\W`/`\s`/`\S`/`\d`/`\D`) nested *inside*
//!   a user's own `[...]` stays subject to the engine's own IGNORECASE
//!   folding (confirmed: it can fold in members CPython's `\w` never
//!   would, e.g. U+0345); the same shortcut used on its own, outside a
//!   class, is guarded against this (`translate::unicode_class_text`'s
//!   own `(?-i:...)` wrapper) -- a group isn't valid syntax inside
//!   `[...]`, so the guard has nowhere to go there.

pub mod replace;

use crate::fs::{get, value_error};
use crate::{CheckResult, ErrorCause, ToolCall, ToolError, ToolPlugin, ToolResult};
use async_trait::async_trait;
use electricity_pyre::backtrack_limit_for;
use electricity_pyre::translate;
use electricity_pyre::translate::ResolvedFlags;
use electricity_value::pycompat::PyExc;
use electricity_value::{Dict, Value};

pub struct RegexPlugin;

/// `re.compile`'s own prefix (`plugins/regex.py::RegexPlugin.execute`:
/// `raise ValueError(f"regex: invalid pattern: {exc}") from exc`) --
/// compile-time only; an execution-time `fancy_regex::Error` goes
/// through [`exec_error`] instead, which doesn't always agree.
fn invalid_pattern(message: impl std::fmt::Display) -> ToolError {
    value_error(format!("regex: invalid pattern: {message}"))
}

/// Our own wording for a `fancy_regex::Error` reached *after* this
/// translator's own `translate()` already succeeded -- never
/// `fancy-regex`'s own `Display` text verbatim (PR #480 review R9:
/// leaking its "Error compiling regex: Look-behind assertion without
/// constant size" broke the "Circuitry's own messages, never a
/// third-party one" rule). The one construct known to reach this
/// point is a backreference inside a look-behind (this module's own
/// doc comment, "Known divergences"): this translator's own `Width`
/// analysis accepts it when the referenced group is fixed-width, but
/// `fancy-regex`'s compiler rejects every backreference there
/// regardless.
fn compile_error_text(e: &fancy_regex::Error) -> String {
    match e {
        fancy_regex::Error::CompileError(fancy_regex::CompileError::LookBehindNotConst) => {
            "a backreference inside a look-behind is not supported".to_string()
        }
        _ => "an unsupported pattern construct".to_string(),
    }
}

/// An error from *running* an already-compiled pattern (`captures`/
/// `find_iter`/`substitute_n`) -- distinct from a compile-time one
/// ([`invalid_pattern`]) because `fancy-regex` hitting its own
/// backtrack limit isn't a pattern-syntax problem Python's `re` would
/// even notice (it has none, and ignores this plugin's
/// `timeout_seconds` like every other mode here): the settled
/// question (issue #479) calls for a `RuntimeError` with electricity's
/// own text, retryable like any other tool failure, never the
/// `ValueError`/invalid-pattern wrapping `plugins/regex.py` never
/// raises for this case either (it can't: CPython's own engine has no
/// such limit).
fn exec_error(e: fancy_regex::Error) -> ToolError {
    match e {
        fancy_regex::Error::RuntimeError(fancy_regex::RuntimeError::BacktrackLimitExceeded) => {
            ToolError {
                py_class: PyExc::RuntimeError,
                message: "regex: backtrack limit exceeded".to_string(),
                cause: ErrorCause::None,
            }
        }
        fancy_regex::Error::RuntimeError(fancy_regex::RuntimeError::StackOverflow) => ToolError {
            py_class: PyExc::RuntimeError,
            message: "regex: stack overflow".to_string(),
            cause: ErrorCause::None,
        },
        other => invalid_pattern(compile_error_text(&other)),
    }
}

/// `re.sub`'s own template-parse failure (`replace::parse_template`):
/// propagates unwrapped, as CPython's own bare `re.error` does (issue
/// #479's "Template errors propagate unwrapped" rule) -- `type(exc).
/// __name__` is `error` on 3.11
/// ([`PyExc::ReError`]), except "unknown group name", which CPython
/// raises as a bare `IndexError` instead (confirmed directly;
/// `replace.rs`'s own doc comment on that branch).
fn template_error(message: String) -> ToolError {
    let py_class = if message.starts_with("unknown group name") {
        PyExc::IndexError
    } else {
        PyExc::ReError
    };
    ToolError {
        py_class,
        message,
        cause: ErrorCause::None,
    }
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
        // `match` anchors the whole pattern at position 0
        // (`\A(?:...)`, a non-capturing wrapper that keeps every group
        // number) -- compiled once, same as every other mode (F9:
        // this translator used to compile the pattern twice for
        // `match`, once unused).
        let pattern_text = if mode == "match" {
            format!("\\A(?:{})", translated.pattern)
        } else {
            translated.pattern.clone()
        };
        let backtrack_limit = backtrack_limit_for(input.len());
        let compiled = fancy_regex::RegexBuilder::new(&pattern_text)
            .backtrack_limit(backtrack_limit)
            .build()
            .map_err(|e| invalid_pattern(compile_error_text(&e)))?;

        let value = match mode.as_str() {
            "match" | "search" => match compiled.captures(&input).map_err(exec_error)? {
                Some(caps) => match_or_search_value(&caps),
                None => Value::None,
            },
            "findall" => {
                let matches = replace::find_iter(&compiled, &input).map_err(exec_error)?;
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
                .map_err(template_error)?;
                let max = replace::max_replacements(count);
                let result =
                    replace::substitute_n(&compiled, &template, &input, max).map_err(exec_error)?;
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
    async fn backreference_inside_lookbehind_never_leaks_fancy_regexs_own_text() {
        // PR #480 review R9: a group with fixed width ("a") makes
        // this translator's own analysis accept `\1` inside the
        // look-behind (CPython accepts it too), but `fancy-regex`'s
        // own compiler rejects every backreference there regardless
        // (DESIGN.md §7.3's own divergence note). The resulting error
        // must be electricity's own wording under the usual prefix,
        // never fancy-regex's raw "Error compiling regex: ..." text.
        let err = run(dict(vec![
            ("pattern", Value::from(r"(a)(?<=\1)b")),
            ("input", Value::from("ab")),
        ]))
        .await
        .unwrap_err();
        assert_eq!(
            err.message,
            "regex: invalid pattern: a backreference inside a look-behind is not supported"
        );
    }

    #[tokio::test]
    async fn match_anchors_a_leading_inline_flag_over_the_whole_pattern() {
        // issue #479: "Check that a leading global inline flag
        // (`(?i)...`) still applies to the whole pattern."
        let result = run(dict(vec![
            ("pattern", Value::from("(?i)ab")),
            ("input", Value::from("AB")),
            ("mode", Value::from("match")),
        ]))
        .await
        .unwrap();
        assert_eq!(result.value, Value::from("AB"));
    }

    #[tokio::test]
    async fn backtrack_limit_is_a_runtime_error_not_an_invalid_pattern() {
        // A synthetic catastrophic-backtracking pattern: `(a+)+`
        // against a long run of `a`s with no trailing `!`, forced
        // onto fancy-regex's backtracking VM by the fixed-width
        // lookbehind (`\b` alone would do it too, but this keeps the
        // blowup isolated to `(a+)+`).
        let err = run(dict(vec![
            ("pattern", Value::from(r"(a+)+(?<=a)!")),
            ("input", Value::from("a".repeat(35))),
        ]))
        .await
        .unwrap_err();
        assert_eq!(err.message, "regex: backtrack limit exceeded");
        assert_eq!(
            err.py_class,
            electricity_value::pycompat::PyExc::RuntimeError
        );
    }

    #[tokio::test]
    async fn word_boundary_no_match_findall_succeeds_on_a_two_megabyte_input() {
        // PR #480 review R8: `\b` forces the backtracking VM (it's
        // translated into a lookaround), and `fancy-regex`'s own
        // 1,000,000 default backtrack limit counts across every start
        // position of the search, not just the eventual match -- so
        // this used to fail past roughly 150 KB even though `cof`
        // succeeds on it instantly.
        let text = "the quick brown fox jumps ".repeat(2_000_000 / 26 + 1);
        let result = run(dict(vec![
            ("pattern", Value::from(r"\bzebra\b")),
            ("input", Value::from(text)),
            ("mode", Value::from("findall")),
        ]))
        .await
        .unwrap();
        assert_eq!(result.value, Value::List(vec![]));
    }

    #[tokio::test]
    async fn trailing_dollar_search_succeeds_on_a_1_2_megabyte_input() {
        // Ordinary text (single-space-separated words, never one
        // giant run of whitespace -- that would overflow
        // `fancy-regex`'s own fixed 1,000,000-entry backtrack *stack*,
        // a separate, unconfigurable limit from `backtrack_limit`)
        // ending in a non-whitespace character: `\s+$` cannot ever
        // succeed, so an unanchored search tries this from every start
        // position -- the review's own `\s+$` case.
        let text = "word ".repeat(1_200_000 / 5) + "x";
        let result = run(dict(vec![
            ("pattern", Value::from(r"\s+$")),
            ("input", Value::from(text)),
            ("mode", Value::from("search")),
        ]))
        .await
        .unwrap();
        assert_eq!(result.value, Value::None);
    }

    #[tokio::test]
    async fn trailing_digits_findall_succeeds_on_fifty_thousand_lines() {
        let mut text = String::new();
        for i in 0..50_000 {
            text.push_str(&format!("item {i} value {i}\n"));
        }
        let result = run(dict(vec![
            ("pattern", Value::from(r"\d+$")),
            ("input", Value::from(text)),
            ("mode", Value::from("findall")),
        ]))
        .await
        .unwrap();
        // Non-MULTILINE `$` only matches once, right before the
        // string's own trailing newline -- the *last* line's own
        // final number, same as `cof`.
        assert_eq!(result.value, Value::List(vec![Value::from("49999")]));
    }

    #[tokio::test]
    async fn catastrophic_but_bounded_pattern_still_returns_no_match_like_cpython() {
        // The review's own R8 baseline: CPython returns `[]` for this
        // in milliseconds; it used to need just past 1,000,000
        // backtracks in fancy-regex (fancy-regex's old default),
        // which this translator's new per-input limit must still
        // clear for an 18-20 character input.
        let result = run(dict(vec![
            ("pattern", Value::from(r"(a+)+(?=b)")),
            ("input", Value::from("a".repeat(18) + "c")),
            ("mode", Value::from("findall")),
        ]))
        .await
        .unwrap();
        assert_eq!(result.value, Value::List(vec![]));
    }

    #[tokio::test]
    async fn sub_template_error_is_re_error_not_value_error() {
        let err = run(dict(vec![
            ("pattern", Value::from("a")),
            ("input", Value::from("a")),
            ("mode", Value::from("sub")),
            ("replacement", Value::from(r"\q")),
        ]))
        .await
        .unwrap_err();
        assert_eq!(err.py_class, electricity_value::pycompat::PyExc::ReError);
        assert_eq!(err.py_class.name(), "error");
    }

    #[tokio::test]
    async fn sub_unknown_group_name_in_template_is_index_error() {
        let err = run(dict(vec![
            ("pattern", Value::from("(a)")),
            ("input", Value::from("a")),
            ("mode", Value::from("sub")),
            ("replacement", Value::from(r"\g<nope>")),
        ]))
        .await
        .unwrap_err();
        assert_eq!(err.py_class, electricity_value::pycompat::PyExc::IndexError);
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
