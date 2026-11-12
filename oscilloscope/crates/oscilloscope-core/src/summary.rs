//! The one summary function behind every row's reply/value words
//! (issue #739): wherever a front end shows a step's own reply or
//! value as a short line, it reads through [`json_value_summary`]
//! rather than a value's raw JSON. An agent step's own "what it did"
//! rule (files/turns/tool calls, from `meta.raw`) stays with
//! `screens::tape_finished_agent_what`, which needs the node's whole
//! `meta`, not just its value -- but falls through to this module's
//! rules once it has nothing of its own to show.
//!
//! Key order: an object's "first keys" read alphabetically, since
//! `serde_json::Value`'s map (`serde_json::Map`) is a `BTreeMap`
//! without the `preserve_order` feature, which this workspace never
//! enables (electricity's own `depth_safety` tests rely on every
//! crate sharing one feature set, issues #492/#549).

use serde_json::Value;

/// A JSON object's summary-like fields (rule 2), tried in this
/// order; the first one present as a non-empty string leads.
const SUMMARY_FIELD_ORDER: [&str; 6] =
    ["summary", "message", "title", "reason", "status", "result"];

/// Issue #739's JSON summary, rules 2-4: an object's own summary-like
/// field or its first keys, an array's own item count (and, for its
/// first item a string, that string's first words), and anything
/// else -- plain text, or a string that only looks like JSON -- by
/// today's first-words rule. `max_chars` bounds the whole result,
/// cut with `⋯` the same way a plain reply already is (§3.8).
pub(crate) fn json_value_summary(value: &Value, max_chars: usize) -> String {
    match value {
        Value::Object(map) => object_summary(map, max_chars),
        Value::Array(items) => array_summary(items, max_chars),
        Value::Null => String::new(),
        other => first_words(&plain_text(other), max_chars),
    }
}

fn plain_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn object_summary(map: &serde_json::Map<String, Value>, max_chars: usize) -> String {
    for field in SUMMARY_FIELD_ORDER {
        if let Some(Value::String(s)) = map.get(field) {
            if !s.trim().is_empty() {
                return first_words(s, max_chars);
            }
        }
    }
    let parts: Vec<String> = map
        .iter()
        .map(|(k, v)| format!("{k} {}", key_value_summary(v, max_chars)))
        .collect();
    cut_chars(&parts.join(" \u{b7} "), max_chars)
}

/// An object's "first keys" rule (rule 2): arrays and objects read
/// their own size, a string its first words, and a number, boolean
/// or null as it is.
fn key_value_summary(value: &Value, max_chars: usize) -> String {
    match value {
        Value::Object(m) => format!("{} keys", m.len()),
        Value::Array(a) => format!("{} items", a.len()),
        Value::String(s) => first_words(s, max_chars),
        other => plain_text(other),
    }
}

fn array_summary(items: &[Value], max_chars: usize) -> String {
    let base = format!("{} items", items.len());
    match items.first() {
        Some(Value::String(s)) if !s.trim().is_empty() => cut_chars(
            &format!("{base} \u{b7} {}", first_words(s, max_chars)),
            max_chars,
        ),
        _ => base,
    }
}

/// §3.4's "first words": the text's own first sentence end or a
/// fixed character budget, whichever comes first -- `Bug.`,
/// `Thanks for the ⋯` (never the whole text, which the detail pane
/// shows in full). §3.8's row-text ellipsis (`⋯`), not a sentence's
/// own `…`.
pub(crate) fn first_words(text: &str, max_chars: usize) -> String {
    let text = text.trim();
    if let Some(end) = first_sentence_end(text) {
        let cut = &text[..=end];
        if cut.chars().count() <= max_chars {
            return cut.to_string();
        }
    }
    cut_chars(text, max_chars)
}

/// The byte offset of the first real sentence end in `text`,
/// skipping two shapes of punctuation that look like one but
/// aren't: a list number's own period (`1.`, `12.` -- a run of
/// nothing but digits before it is a list marker, not a sentence,
/// so a reply that opens `1. Faster parser.` cuts at the real end
/// instead of after the `1`), and a period embedded in a word
/// (`api.py`, immediately followed by another letter or digit with
/// no space, never a sentence's own end).
fn first_sentence_end(text: &str) -> Option<usize> {
    let mut search_from = 0;
    loop {
        let rel = text[search_from..].find(['.', '!', '?'])?;
        let end = search_from + rel;
        let prefix = &text[..end];
        let is_list_marker = !prefix.is_empty() && prefix.chars().all(|c| c.is_ascii_digit());
        let embedded_in_a_word = text[end + 1..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric());
        if is_list_marker || embedded_in_a_word {
            search_from = end + 1;
            continue;
        }
        return Some(end);
    }
}

/// A plain character-budget cut, `⋯` appended only when something
/// was actually cut off.
pub(crate) fn cut_chars(text: &str, max_chars: usize) -> String {
    let truncated: String = text.chars().take(max_chars).collect();
    if truncated.chars().count() < text.chars().count() {
        format!("{truncated}\u{22ef}")
    } else {
        truncated
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A list number's own period (`1.`, `12.`) is never a sentence
    /// end, so `1. Faster parser.` cuts at the real one.
    #[test]
    fn first_sentence_end_skips_a_list_numbers_own_period() {
        assert_eq!(
            first_sentence_end("1. Faster parser. Also fixes a bug."),
            Some(16)
        );
        assert_eq!(first_words("1. Faster parser.", 80), "1. Faster parser.");
    }

    /// A period embedded in a word (no space after it, followed by
    /// another letter or digit) is never a sentence end either.
    #[test]
    fn first_sentence_end_skips_a_period_embedded_in_a_word() {
        assert_eq!(first_sentence_end("See api.py.main for that."), Some(24));
        assert_eq!(first_sentence_end("v1.2.0 shipped."), Some(14));
        assert_eq!(
            first_sentence_end("Read https://example.com/a.b for more."),
            Some(37)
        );
        assert_eq!(first_sentence_end("It costs 3.14 dollars."), Some(21));
    }

    /// `e.g.`/`U.S.` aren't specially recognised at all -- the period
    /// right after the first one-letter abbreviation is itself
    /// followed immediately by another letter with no space
    /// (`e`+`.`+`g`), so it's skipped as "embedded in a word" the
    /// same as `api.py` would be; the *next* period (after `g`/the
    /// second `S`) is followed by a space, so that one reads as the
    /// real end.
    #[test]
    fn first_sentence_end_reads_an_abbreviations_period_as_a_real_one() {
        assert_eq!(first_sentence_end("e.g. a cat."), Some(3));
        assert_eq!(first_sentence_end("U.S. markets rose."), Some(3));
    }

    /// `...` and a trailing `?!` both still end at their own first
    /// punctuation mark.
    #[test]
    fn first_sentence_end_stops_at_the_first_punctuation_mark() {
        assert_eq!(first_sentence_end("Wait... really?"), Some(4));
        assert_eq!(first_sentence_end("Really?! Yes."), Some(6));
    }

    #[test]
    fn first_sentence_end_is_none_with_no_sentence_end_at_all() {
        assert_eq!(first_sentence_end("no punctuation here"), None);
    }

    #[test]
    fn first_words_cuts_to_width_with_an_ellipsis() {
        assert_eq!(
            first_words("no punctuation here at all", 10),
            "no punctua\u{22ef}"
        );
    }

    #[test]
    fn a_top_level_string_keeps_the_first_words_rule() {
        assert_eq!(
            json_value_summary(&json!("Bug. Also did other things."), 24),
            "Bug."
        );
    }

    /// A string that only looks like JSON (never parsed as such by
    /// this function, which only ever sees already-typed
    /// `serde_json::Value`s) still just reads its own first words.
    #[test]
    fn a_json_looking_string_that_doesnt_parse_keeps_the_first_words_rule() {
        let value = json!("{not actually json, just starts like it");
        assert_eq!(
            json_value_summary(&value, 24),
            "{not actually json, just\u{22ef}"
        );
    }

    #[test]
    fn a_null_value_summarises_to_nothing() {
        assert_eq!(json_value_summary(&Value::Null, 24), "");
    }

    #[test]
    fn a_number_and_a_bool_read_plainly() {
        assert_eq!(json_value_summary(&json!(42), 24), "42");
        assert_eq!(json_value_summary(&json!(true), 24), "true");
    }

    #[test]
    fn an_object_with_a_summary_field_leads_with_its_first_words() {
        let value = json!({
            "edits": [{"replace": "  const x = 1;"}],
            "summary": "Renamed the helper. Also tidied imports.",
        });
        assert_eq!(json_value_summary(&value, 24), "Renamed the helper.");
    }

    /// The summary-like fields are tried in order; `message` only
    /// leads once `summary` isn't present.
    #[test]
    fn the_summary_field_order_is_tried_in_order() {
        let value = json!({"message": "From message.", "title": "From title."});
        assert_eq!(json_value_summary(&value, 40), "From message.");
    }

    /// An empty or blank summary field doesn't count as present --
    /// the next field in order, then the first-keys fallback, still
    /// gets a chance.
    #[test]
    fn a_blank_summary_field_is_skipped() {
        let value = json!({"summary": "   ", "message": "Real text."});
        assert_eq!(json_value_summary(&value, 40), "Real text.");
    }

    #[test]
    fn an_object_with_no_summary_field_reads_its_first_keys() {
        let value = json!({"edits": [1, 2], "files": [1, 2, 3]});
        assert_eq!(
            json_value_summary(&value, 40),
            "edits 2 items \u{b7} files 3 items"
        );
    }

    #[test]
    fn an_object_with_mixed_value_kinds_reads_each_by_its_own_kind() {
        let value = json!({
            "count": 3,
            "ok": true,
            "name": "Bug. More text.",
            "nested": {"a": 1, "b": 2},
        });
        assert_eq!(
            json_value_summary(&value, 80),
            "count 3 \u{b7} name Bug. \u{b7} nested 2 keys \u{b7} ok true"
        );
    }

    #[test]
    fn an_empty_object_summarises_to_nothing() {
        assert_eq!(json_value_summary(&json!({}), 24), "");
    }

    #[test]
    fn a_nested_empty_object_and_array_read_as_zero() {
        let value = json!({"a": {}, "b": []});
        assert_eq!(json_value_summary(&value, 40), "a 0 keys \u{b7} b 0 items");
    }

    #[test]
    fn an_array_of_strings_reads_its_count_then_the_firsts_words() {
        let value = json!(["Bug. Second thing.", "Other"]);
        assert_eq!(json_value_summary(&value, 24), "2 items \u{b7} Bug.");
    }

    #[test]
    fn an_array_of_non_strings_reads_only_its_count() {
        let value = json!([{"a": 1}, {"b": 2}]);
        assert_eq!(json_value_summary(&value, 24), "2 items");
    }

    #[test]
    fn an_empty_array_reads_zero_items() {
        assert_eq!(json_value_summary(&json!([]), 24), "0 items");
    }

    #[test]
    fn a_long_object_summary_cuts_to_width_with_an_ellipsis() {
        let value = json!({"aaaaaaaaaa": 1, "bbbbbbbbbb": 2, "cccccccccc": 3});
        let out = json_value_summary(&value, 20);
        assert_eq!(out.chars().count(), 21);
        assert!(out.ends_with('\u{22ef}'));
    }
}
