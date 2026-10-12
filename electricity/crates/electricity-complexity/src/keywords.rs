//! `_match_keywords`/`_keyword_pattern` (`core/complexity.py:636-667`): the
//! `keywords` signal's whole-word, case-insensitive phrase matching.
//!
//! Pattern text is built with a port of CPython 3.11's `re.escape`, then
//! translated through `electricity-pyre` the same way `electricity-tools`'s
//! `regex` plugin does -- the only two places this workspace turns Python
//! `re` syntax into `fancy-regex` syntax (issue #505's own "Reuse, don't
//! re-implement" ruling). Compiled patterns are cached by their *Python*
//! pattern text, mirroring `re`'s own internal cache, since the same stem
//! (e.g. every orchestration's shared default table) is matched repeatedly
//! across many `score()` calls.

use crate::{as_number, ScoreError};
use electricity_pyre::backtrack_limit_for;
use electricity_pyre::translate::{translate, ResolvedFlags};
use electricity_value::pycompat::py_strip;
use electricity_value::{Dict, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// Inflections a keyword is allowed to appear in (`core/complexity.py:636`,
/// `_KEYWORD_SUFFIXES`).
const KEYWORD_SUFFIXES: &str = "(?:e|es|s|ed|ing|ion|ions)?";

/// CPython 3.11's `re.escape`: `()[]{}?*+-|^$\.&~#` plus space and
/// `\t\n\r\v\f`, verbatim -- non-ASCII is never escaped.
pub fn py_re_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '(' | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | '?'
                | '*'
                | '+'
                | '-'
                | '|'
                | '^'
                | '$'
                | '.'
                | '&'
                | '~'
                | '#'
                | ' '
                | '\t'
                | '\n'
                | '\r'
                | '\u{0b}'
                | '\u{0c}'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `_keyword_pattern`'s own stem (`core/complexity.py:639-642`): drops
/// a trailing `e` when the code-point length exceeds 3.
fn keyword_stem(phrase: &str) -> String {
    let lowered = phrase.to_lowercase();
    let stripped = py_strip(&lowered);
    let mut chars: Vec<char> = stripped.chars().collect();
    if chars.len() > 3 && chars.last() == Some(&'e') {
        chars.pop();
    }
    chars.into_iter().collect()
}

/// `_keyword_pattern` (`core/complexity.py:639`): the stem wrapped in
/// `\b` + the inflection suffixes.
fn keyword_pattern_text(phrase: &str) -> String {
    format!(r"\b{}{KEYWORD_SUFFIXES}\b", py_re_escape(&keyword_stem(phrase)))
}

fn translated_pattern_cache() -> &'static Mutex<HashMap<String, Arc<String>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Arc<String>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The *translated* (`fancy-regex`-syntax) pattern text for one keyword
/// phrase, from a process-wide cache keyed by the Python pattern text --
/// never by *phrase* directly, since two different phrases can share
/// one stem after the suffix strip. Translation (not compilation) is
/// the step worth caching: it is pure text-to-text work independent of
/// whatever haystack a given `score()` call matches against, where
/// compilation has to bake in a backtrack limit sized to *that* call's
/// own haystack length (`compiled_keyword_pattern`, below) and so can't
/// be shared across calls with different haystack sizes.
///
/// A translate failure here can't happen for a `re.escape` output (see
/// this crate's own unit test covering every special character); if it
/// somehow did, scoring fails rather than silently skipping the phrase.
fn translated_pattern(phrase: &str) -> Result<Arc<String>, ScoreError> {
    let pattern_text = keyword_pattern_text(phrase);
    {
        let cache = translated_pattern_cache().lock().unwrap();
        if let Some(p) = cache.get(&pattern_text) {
            return Ok(p.clone());
        }
    }
    let translated = translate(&pattern_text, ResolvedFlags::default()).map_err(|e| {
        ScoreError::runtime_error(format!(
            "keyword pattern for {phrase:?} failed to translate: {e}"
        ))
    })?;
    let arc = Arc::new(translated.pattern);
    translated_pattern_cache()
        .lock()
        .unwrap()
        .insert(pattern_text, arc.clone());
    Ok(arc)
}

/// The compiled pattern for one keyword phrase, with a backtrack limit
/// sized to *haystack_len* (`electricity_pyre::backtrack_limit_for`,
/// the same sizing `electricity-tools::regex` and `electricity-schema`
/// use) -- `fancy-regex`'s own flat default (1,000,000) undercounts a
/// `\b`-anchored pattern's own backtrack steps on an ordinary
/// multi-hundred-KB-or-larger haystack, well short of anything
/// pathological.
fn compiled_keyword_pattern(
    phrase: &str,
    haystack_len: usize,
) -> Result<fancy_regex::Regex, ScoreError> {
    let pattern_text = translated_pattern(phrase)?;
    fancy_regex::RegexBuilder::new(&pattern_text)
        .backtrack_limit(backtrack_limit_for(haystack_len))
        .build()
        .map_err(|e| {
            ScoreError::runtime_error(format!("keyword pattern for {phrase:?} failed to compile: {e}"))
        })
}

/// `_match_keywords` (`core/complexity.py:646`): whole-word,
/// case-insensitive phrase matches against *haystack* (already the raw,
/// unlowered text -- this function lowercases it itself, via
/// `str::to_lowercase`), sorted by `(phrase, weight)`.
///
/// *table*'s keys that are not non-blank strings, and weights that are
/// not positive numbers, are skipped (`core/complexity.py`'s own filter) --
/// a non-string key can only arrive through direct library use, since
/// config resolution already strips and validates every configured
/// keyword key.
pub fn match_keywords(haystack: &str, table: &Dict) -> Result<Vec<(String, f64)>, ScoreError> {
    if haystack.is_empty() || table.is_empty() {
        return Ok(Vec::new());
    }
    let lowered = haystack.to_lowercase();
    let haystack_len = lowered.len();
    let mut matches: Vec<(String, f64)> = Vec::new();
    for (phrase_value, weight_value) in table {
        let Value::Str(phrase) = phrase_value else {
            continue;
        };
        if py_strip(phrase).is_empty() {
            continue;
        }
        // The regex can only match if its stem literally occurs
        // somewhere in the haystack (every suffix only *extends* the
        // stem, never shortens it) -- a plain substring check first
        // is a correctness-preserving fast path that skips the regex
        // entirely for the common case of a haystack that shares no
        // text with a given phrase, which matters once the haystack is
        // large (a multi-hundred-KB-or-larger rendered prompt against
        // the whole default table).
        if !lowered.contains(&keyword_stem(phrase)) {
            continue;
        }
        // `weight is None or weight <= 0: continue` (`core/complexity.py:
        // 663`) -- written as the same double negative Python uses, not
        // as `w > 0.0`: a `NaN` weight fails `<= 0` (any comparison with
        // `NaN` is `False`), so Python's own check *keeps* a `NaN`
        // weight rather than skipping it, where `w > 0.0` would wrongly
        // skip it too (`NaN > 0.0` is also `False`).
        let weight = match as_number(weight_value)? {
            None => continue,
            Some(w) if w <= 0.0 => continue,
            Some(w) => w,
        };
        let re = compiled_keyword_pattern(phrase, haystack_len)?;
        let found = re.is_match(&lowered).map_err(|e| {
            ScoreError::runtime_error(format!("keyword match for {phrase:?} failed: {e}"))
        })?;
        if found {
            matches.push((phrase.clone(), weight));
        }
    }
    matches.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.partial_cmp(&b.1).unwrap()));
    Ok(matches)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn re_escape_matches_cpython_3_11s_special_set() {
        assert_eq!(py_re_escape("("), "\\(");
        assert_eq!(py_re_escape(")"), "\\)");
        assert_eq!(py_re_escape("["), "\\[");
        assert_eq!(py_re_escape("]"), "\\]");
        assert_eq!(py_re_escape("{"), "\\{");
        assert_eq!(py_re_escape("}"), "\\}");
        assert_eq!(py_re_escape("?"), "\\?");
        assert_eq!(py_re_escape("*"), "\\*");
        assert_eq!(py_re_escape("+"), "\\+");
        assert_eq!(py_re_escape("-"), "\\-");
        assert_eq!(py_re_escape("|"), "\\|");
        assert_eq!(py_re_escape("^"), "\\^");
        assert_eq!(py_re_escape("$"), "\\$");
        assert_eq!(py_re_escape("."), "\\.");
        assert_eq!(py_re_escape("&"), "\\&");
        assert_eq!(py_re_escape("~"), "\\~");
        assert_eq!(py_re_escape("#"), "\\#");
        assert_eq!(py_re_escape(" "), "\\ ");
        assert_eq!(py_re_escape("\t"), "\\\t");
        assert_eq!(py_re_escape("\n"), "\\\n");
        assert_eq!(py_re_escape("\r"), "\\\r");
        assert_eq!(py_re_escape("\u{0b}"), "\\\u{0b}");
        assert_eq!(py_re_escape("\u{0c}"), "\\\u{0c}");
        assert_eq!(py_re_escape("café"), "café");
        assert_eq!(py_re_escape("_"), "_");
    }

    #[test]
    fn every_special_character_still_translates_and_compiles() {
        for phrase in [
            "c++", "a-b", "a.b", "a&b", "a~b", "a#b", "a b", "a\tb", "(a)", "[a]", "{a}", "a?b",
            "a*b", "a+b", "a|b", "a^b", "a$b",
        ] {
            compiled_keyword_pattern(phrase, 100)
                .unwrap_or_else(|e| panic!("phrase {phrase:?} failed: {e:?}"));
        }
    }

    #[test]
    fn translation_is_cached_across_calls() {
        let p1 = translated_pattern("analyze").unwrap();
        let p2 = translated_pattern("analyze").unwrap();
        assert!(Arc::ptr_eq(&p1, &p2));
    }

    #[test]
    fn a_large_haystack_does_not_exceed_the_backtrack_limit() {
        let haystack = "a".repeat(1024 * 1024);
        let table = sample_table();
        assert_eq!(match_keywords(&haystack, &table).unwrap(), vec![]);
    }

    #[test]
    fn a_nan_weight_is_kept_not_skipped() {
        let mut table = Dict::new();
        table.insert(Value::Str("critique".to_string()), Value::from(f64::NAN));
        let matches = match_keywords("please critique this", &table).unwrap();
        assert_eq!(matches.len(), 1);
        assert!(matches[0].1.is_nan());
    }

    #[test]
    fn stem_drops_trailing_e_only_past_three_characters() {
        assert_eq!(keyword_pattern_text("rewrite"), r"\brewrit(?:e|es|s|ed|ing|ion|ions)?\b");
        // "we" is 2 characters -- too short for the trailing-e drop.
        assert_eq!(keyword_pattern_text("we"), r"\bwe(?:e|es|s|ed|ing|ion|ions)?\b");
    }

    #[test]
    fn empty_haystack_or_table_disables_the_signal() {
        assert_eq!(match_keywords("", &sample_table()).unwrap(), vec![]);
        assert_eq!(match_keywords("analyze this", &Dict::new()).unwrap(), vec![]);
    }

    fn sample_table() -> Dict {
        let mut table = Dict::new();
        table.insert(Value::Str("analyze".to_string()), Value::from(0.15));
        table
    }
}
