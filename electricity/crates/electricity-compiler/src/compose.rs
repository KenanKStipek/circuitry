//! Lane D: ports `core/prompt_compose.py`'s composition checks — each
//! fragment's syntax, set-delimiter tags refused, unknown names
//! resolved scope-aware, collisions between declared prompts and
//! effects, references to non-text effects, cycles among declared
//! prompts, and the `type: yield` compile step.
//!
//! Called from `compile::compile_document` (lane C) right after
//! [`crate::prompt_files::compile_declared_prompts`], matching
//! `core/compiler.py::compile_orchestration`'s order: the composition
//! checks run before any effect compiles.

use crate::CompileError;
use electricity_value::Value;
use indexmap::IndexMap;

/// The compile-time half of #406's composition checks against
/// *document* and its already-read *declared_prompts*.
///
/// Seam stub (lane A): a document that neither declares any prompts
/// nor uses `{{> name}}` anywhere passes through (`Ok(())`), so lane C
/// can test compilation of documents that don't use this feature
/// without waiting for lane D. A document with declared prompts or a
/// `{{>` partial reference anywhere still fails with the lane D marker
/// until lane D lands.
pub(crate) fn check_prompt_composition(
    document: &Value,
    declared_prompts: &IndexMap<String, String>,
) -> Result<(), CompileError> {
    if declared_prompts.is_empty() && !contains_partial_tag(document) {
        return Ok(());
    }
    Err(CompileError(crate::not_implemented(
        "compose::check_prompt_composition",
        "D",
    )))
}

/// Whether any string anywhere in *document* contains a `{{>` partial
/// tag -- a document-wide pre-scan, not a template parse (lane D parses
/// each composable field properly; this only decides whether the seam
/// stub above can pass a document through untouched).
///
/// Walks the document's lists/dicts with an explicit stack rather than
/// recursion, so a value as deep as [`electricity_value::MAX_DEPTH`]
/// allows can't overflow the stack here either (`electricity-value`'s
/// own [`Drop`](electricity_value::Value) impl uses the same shape, for
/// the same reason).
fn contains_partial_tag(document: &Value) -> bool {
    let mut pending: Vec<&Value> = vec![document];
    while let Some(value) = pending.pop() {
        match value {
            Value::Str(text) => {
                if text.contains("{{>") {
                    return true;
                }
            }
            Value::List(items) => pending.extend(items.iter()),
            Value::Dict(entries) => {
                for (key, value) in entries {
                    pending.push(key);
                    pending.push(value);
                }
            }
            _ => {}
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::check_prompt_composition;
    use electricity_value::{Dict, Value};
    use indexmap::IndexMap;

    #[test]
    fn document_without_prompts_or_partials_passes_through() {
        let mut dict = Dict::new();
        dict.insert(
            Value::Str("effects".to_string()),
            Value::List(vec![Value::Str("plain text".to_string())]),
        );
        let document = Value::Dict(dict);

        let result = check_prompt_composition(&document, &IndexMap::new());

        assert_eq!(result, Ok(()));
    }

    #[test]
    fn declared_prompts_fail_with_the_lane_d_marker() {
        let document = Value::Dict(Dict::new());
        let mut declared_prompts = IndexMap::new();
        declared_prompts.insert("greeting".to_string(), "hi".to_string());

        let err = check_prompt_composition(&document, &declared_prompts).unwrap_err();

        assert!(err.0.contains("lane D"), "unexpected error: {}", err.0);
    }

    #[test]
    fn a_partial_reference_anywhere_fails_with_the_lane_d_marker() {
        let mut dict = Dict::new();
        dict.insert(
            Value::Str("effects".to_string()),
            Value::List(vec![Value::Str("{{> greeting}}".to_string())]),
        );
        let document = Value::Dict(dict);

        let err = check_prompt_composition(&document, &IndexMap::new()).unwrap_err();

        assert!(err.0.contains("lane D"), "unexpected error: {}", err.0);
    }
}
