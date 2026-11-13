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

/// `core/decompose.py::_candidate_references`'s own per-source call:
/// `core/complexity.py::_extract_references`'s *full* return (issue #682)
/// -- sorted distinct reference names, dropping the section-name half
/// `extract_references` above already drops (nothing here needs it).
pub fn reference_names_sorted(template: &str) -> Result<Vec<String>, ScoreError> {
    let re = tag_regex();
    let mut references: HashSet<String> = HashSet::new();
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
        references.insert(cleaned.to_string());
    }
    let mut sorted: Vec<String> = references.into_iter().collect();
    sorted.sort();
    Ok(sorted)
}

/// `core/decompose.py::_tag_occurrences_and_extensions`'s own raw scan:
/// every `(sigil, cleaned name)` pair `_TAG_RE.findall` would yield, in
/// document order, comments/partials/closing tags already dropped --
/// unlike [`extract_references`]/[`reference_names_sorted`], not deduped
/// at all (every occurrence counts).
pub fn find_tags(template: &str) -> Result<Vec<(String, String)>, ScoreError> {
    let re = tag_regex();
    let mut out = Vec::new();
    for caps in re.captures_iter(template) {
        let caps = caps
            .map_err(|e| ScoreError::runtime_error(format!("state_references scan failed: {e}")))?;
        let sigil = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let name = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        if matches!(sigil, "!" | ">" | "/") {
            continue;
        }
        out.push((sigil.to_string(), py_strip(name).to_string()));
    }
    Ok(out)
}

/// `core/decompose.py::_rewrite_prefix` (issue #682): every tag whose
/// name is exactly *old_prefix*, or extends it with a further dotted
/// path, with just the leading segment replaced by *new_prefix* --
/// every sigil, extensions kept verbatim (the one rewrite that keeps
/// extensions rather than refusing them, for the merge template's own
/// hierarchical reduce).
pub fn rewrite_tag_prefix(
    template: &str,
    old_prefix: &str,
    new_prefix: &str,
) -> Result<String, ScoreError> {
    let re = tag_regex();
    let mut out = String::with_capacity(template.len());
    let mut last = 0usize;
    for found in re.captures_iter(template) {
        let caps = found
            .map_err(|e| ScoreError::runtime_error(format!("tag rewrite scan failed: {e}")))?;
        let whole = caps.get(0).expect("the whole match always exists");
        let sigil = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let name = caps.get(2).map(|m| m.as_str()).unwrap_or("");
        let cleaned = py_strip(name);
        let dotted_prefix = format!("{old_prefix}.");
        let new_name = if cleaned == old_prefix {
            Some(new_prefix.to_string())
        } else {
            cleaned
                .strip_prefix(&dotted_prefix)
                .map(|rest| format!("{new_prefix}.{rest}"))
        };
        out.push_str(&template[last..whole.start()]);
        match new_name {
            Some(new_name) if !matches!(sigil, "!" | ">") => {
                let whole_str = whole.as_str();
                if whole_str.starts_with("{{{") && whole_str.ends_with("}}}") {
                    out.push_str("{{{");
                    out.push_str(&new_name);
                    out.push_str("}}}");
                } else {
                    out.push_str("{{");
                    out.push_str(sigil);
                    out.push_str(&new_name);
                    out.push_str("}}");
                }
            }
            _ => out.push_str(whole.as_str()),
        }
        last = whole.end();
    }
    out.push_str(&template[last..]);
    Ok(out)
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

    #[test]
    fn reference_names_sorted_dedupes_and_sorts() {
        assert_eq!(
            reference_names_sorted("{{b}} {{a}} {{b}} {{{a.c}}}").unwrap(),
            vec!["a".to_string(), "a.c".to_string(), "b".to_string()]
        );
    }

    #[test]
    fn reference_names_sorted_drops_comments_partials_and_closing_tags() {
        assert_eq!(
            reference_names_sorted("{{! c }}{{> p}}{{#x}}{{/x}}").unwrap(),
            vec!["x".to_string()]
        );
    }

    #[test]
    fn find_tags_keeps_every_occurrence_with_its_own_sigil() {
        assert_eq!(
            find_tags("{{{a}}} {{&b}} {{#c}} {{a}}").unwrap(),
            vec![
                ("".to_string(), "a".to_string()),
                ("&".to_string(), "b".to_string()),
                ("#".to_string(), "c".to_string()),
                ("".to_string(), "a".to_string()),
            ]
        );
    }

    #[test]
    fn find_tags_drops_comments_partials_and_closing_tags() {
        assert_eq!(
            find_tags("{{! c }}{{> p}}{{/x}}").unwrap(),
            Vec::<(String, String)>::new()
        );
    }

    #[test]
    fn rewrite_tag_prefix_rewrites_the_exact_name_and_every_extension() {
        assert_eq!(
            rewrite_tag_prefix(
                "{{{parts}}} {{#parts.0}}x{{/parts.0}}",
                "parts",
                "prime.p.value"
            )
            .unwrap(),
            "{{{prime.p.value}}} {{#prime.p.value.0}}x{{/prime.p.value.0}}"
        );
    }

    #[test]
    fn rewrite_tag_prefix_preserves_every_sigil_shape() {
        assert_eq!(
            rewrite_tag_prefix("{{parts}} {{&parts}} {{{parts}}}", "parts", "new").unwrap(),
            "{{new}} {{&new}} {{{new}}}"
        );
    }

    #[test]
    fn rewrite_tag_prefix_leaves_an_unrelated_name_alone() {
        assert_eq!(
            rewrite_tag_prefix("{{other}} {{partskin}}", "parts", "new").unwrap(),
            "{{other}} {{partskin}}"
        );
    }

    #[test]
    fn rewrite_tag_prefix_never_rewrites_a_comment_or_partial() {
        assert_eq!(
            rewrite_tag_prefix("{{! parts }}{{> parts}}", "parts", "new").unwrap(),
            "{{! parts }}{{> parts}}"
        );
    }
}
