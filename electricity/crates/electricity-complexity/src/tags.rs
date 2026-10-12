//! `_extract_references`/`_TAG_RE` (`core/complexity.py:208`, `:496-520`):
//! the `state_references` signal's mustache-tag scan. This is *not* a
//! Mustache tokenizer -- it is the reference's own fixed regular
//! expression, translated once through `electricity-pyre` and cached for
//! the life of the process.

use crate::ScoreError;
use electricity_pyre::translate::{ResolvedFlags, translate};
use electricity_value::pycompat::py_strip;
use std::collections::HashSet;
use std::sync::OnceLock;

/// `_TAG_RE` (`core/complexity.py:208`) verbatim.
const TAG_PATTERN: &str = r"\{\{\{?\s*([#^/&!>]?)\s*([^{}]*?)\s*\}?\}\}";

fn tag_regex() -> &'static fancy_regex::Regex {
    static RE: OnceLock<fancy_regex::Regex> = OnceLock::new();
    RE.get_or_init(|| {
        let translated = translate(TAG_PATTERN, ResolvedFlags::default())
            .expect("_TAG_RE is a fixed pattern, verified to translate by this module's tests");
        fancy_regex::Regex::new(&translated.pattern)
            .expect("_TAG_RE's translated pattern is always valid")
    })
}

/// `(distinct reference count, distinct section count)` -- `detail`
/// (the actual reference/section names) is dropped from this port
/// entirely: it never reaches `to_meta`, and K1 has no `cof score`/lint
/// surface to report it to (issue #505's "Out of scope").
pub fn extract_references(template: &str) -> Result<(usize, usize), ScoreError> {
    let re = tag_regex();
    let mut references: HashSet<String> = HashSet::new();
    let mut sections: HashSet<String> = HashSet::new();
    for caps in re.captures_iter(template) {
        let caps = caps
            .map_err(|e| ScoreError::runtime_error(format!("state_references scan failed: {e}")))?;
        let sigil = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let name = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        if matches!(sigil, "!" | ">" | "/") {
            continue;
        }
        let cleaned = py_strip(name);
        if cleaned.is_empty() || cleaned == "." {
            continue;
        }
        if matches!(sigil, "#" | "^") {
            sections.insert(cleaned.to_string());
        }
        references.insert(cleaned.to_string());
    }
    Ok((references.len(), sections.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_dotted_and_spaced_names_are_references() {
        assert_eq!(extract_references("{{a}} {{ a.b }}").unwrap(), (2, 0));
    }

    #[test]
    fn comments_partials_and_closing_tags_are_not_references() {
        assert_eq!(
            extract_references("{{! comment }}{{> partial}}{{#x}}{{/x}}").unwrap(),
            (1, 1)
        );
    }

    #[test]
    fn triple_braces_and_ampersand_are_references() {
        assert_eq!(
            extract_references("{{{raw}}} {{&unescaped}}").unwrap(),
            (2, 0)
        );
    }

    #[test]
    fn empty_and_dot_tags_are_not_references() {
        assert_eq!(extract_references("{{}} {{ }} {{.}}").unwrap(), (0, 0));
    }

    #[test]
    fn duplicates_count_once() {
        assert_eq!(extract_references("{{x}} {{x}}").unwrap(), (1, 0));
    }

    #[test]
    fn a_tag_may_span_a_newline() {
        assert_eq!(extract_references("{{\nx\n}}").unwrap(), (1, 0));
    }

    #[test]
    fn delimiter_change_tags_count_as_a_reference() {
        assert_eq!(extract_references("{{=<% %>=}}").unwrap(), (1, 0));
    }
}
