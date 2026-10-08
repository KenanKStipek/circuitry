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
use std::collections::BTreeSet;

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

/// Container fields whose values are lists of child effect dicts,
/// walked when collecting every effect name anywhere in the document
/// (not into a `use` effect's own child document -- that compiles
/// separately) -- `core/prompt_compose.py`'s `_CHILD_LISTS`.
///
/// Unused outside tests until lane C wires [`all_effect_names`] into
/// `compile::compile_document`'s `Program.effect_names`.
#[allow(dead_code)]
const CHILD_LISTS: [&str; 6] = ["effects", "steps", "body", "then", "else", "finally"];

/// Every effect name anywhere in *document*, flattened regardless of
/// nesting -- `core/prompt_compose.py`'s `all_effect_names`
/// (`core/prompt_compose.py:188-213`), ported field for field:
/// `compile_orchestration` sets the root's `effect_names` from it
/// (`Program.effect_names`), and it also backs the declared-prompt/
/// effect-name collision check.
///
/// Starts from the top-level `effects` list, or `steps` when `effects`
/// is missing, not a list, or an empty list (Python's `orch.get(
/// "effects") or orch.get("steps") or []`, by truthiness, not by key
/// presence), then separately walks the top-level `finally` list the
/// same way. From there, each mapping item in a walked list contributes
/// its own non-empty string `name` and is recursed into through every
/// [`CHILD_LISTS`] field that is itself a list; a non-mapping item, a
/// missing/non-string/empty `name`, and a child field that isn't a list
/// are all silently skipped, exactly as the Python walk skips them.
///
/// Walks with an explicit stack rather than recursion, so a document as
/// deep as [`electricity_value::MAX_DEPTH`] allows can't overflow the
/// stack here either (the same shape [`contains_partial_tag`] and
/// `electricity-value`'s own [`Drop`](electricity_value::Value) impl
/// use, for the same reason).
///
/// Unused outside tests until lane C wires it into
/// `compile::compile_document`'s `Program.effect_names`.
#[allow(dead_code)]
pub(crate) fn all_effect_names(document: &Value) -> BTreeSet<String> {
    let mut names = BTreeSet::new();

    let effects = dict_get(document, "effects");
    let start = if is_truthy(effects) {
        effects
    } else {
        dict_get(document, "steps")
    };

    let mut pending: Vec<&Value> = Vec::new();
    if let Some(Value::List(items)) = start {
        pending.extend(items.iter());
    }
    if let Some(Value::List(items)) = dict_get(document, "finally") {
        pending.extend(items.iter());
    }

    while let Some(effect) = pending.pop() {
        let Some(dict) = effect.as_dict() else {
            continue;
        };
        if let Some(Value::Str(name)) = dict.get(&Value::Str("name".to_string())) {
            if !name.is_empty() {
                names.insert(name.clone());
            }
        }
        for field in CHILD_LISTS {
            if let Some(Value::List(items)) = dict.get(&Value::Str(field.to_string())) {
                pending.extend(items.iter());
            }
        }
    }

    names
}

/// Unused outside [`all_effect_names`] until lane C wires it in.
#[allow(dead_code)]
fn dict_get<'a>(document: &'a Value, key: &str) -> Option<&'a Value> {
    document.as_dict()?.get(&Value::Str(key.to_string()))
}

/// Python truthiness, for the `effects or steps` fallback above: a
/// missing key (`None` here), `None`, `False`, a numeric zero, and any
/// empty `str`/`bytes`/`list`/`dict` are falsy; everything else is
/// truthy.
///
/// Unused outside [`all_effect_names`] until lane C wires it in.
#[allow(dead_code)]
fn is_truthy(value: Option<&Value>) -> bool {
    match value {
        None => false,
        Some(Value::None) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Int(i)) => !i.is_zero(),
        Some(Value::Float(f)) => *f != 0.0,
        Some(Value::Str(s)) => !s.is_empty(),
        Some(Value::Bytes(b)) => !b.is_empty(),
        Some(Value::List(items)) => !items.is_empty(),
        Some(Value::Dict(d)) => !d.is_empty(),
        Some(Value::Date(_)) | Some(Value::DateTime(..)) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::{all_effect_names, check_prompt_composition};
    use electricity_value::{Dict, Value};
    use indexmap::IndexMap;
    use std::collections::BTreeSet;

    fn names(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn effect_with(name: Value, children: &[(&str, Vec<Value>)]) -> Value {
        let mut dict = Dict::new();
        dict.insert(Value::Str("name".to_string()), name);
        for (field, items) in children {
            dict.insert(Value::Str((*field).to_string()), Value::List(items.clone()));
        }
        Value::Dict(dict)
    }

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

    #[test]
    fn collects_names_nested_under_if_loop_dynamic_and_finally() {
        let a = effect_with(
            Value::Str("a".to_string()),
            &[
                ("then", vec![effect_with(Value::Str("b".to_string()), &[])]),
                ("else", vec![effect_with(Value::Str("c".to_string()), &[])]),
                ("body", vec![effect_with(Value::Str("d".to_string()), &[])]),
                (
                    "finally",
                    vec![effect_with(Value::Str("e".to_string()), &[])],
                ),
            ],
        );
        let mut document = Dict::new();
        document.insert(Value::Str("effects".to_string()), Value::List(vec![a]));
        document.insert(
            Value::Str("finally".to_string()),
            Value::List(vec![effect_with(Value::Str("f".to_string()), &[])]),
        );

        let result = all_effect_names(&Value::Dict(document));

        assert_eq!(result, names(&["a", "b", "c", "d", "e", "f"]));
    }

    #[test]
    fn non_string_and_empty_names_are_skipped() {
        let mut document = Dict::new();
        document.insert(
            Value::Str("effects".to_string()),
            Value::List(vec![
                effect_with(Value::Int(123.into()), &[]),
                effect_with(Value::Str(String::new()), &[]),
                effect_with(Value::Str("kept".to_string()), &[]),
            ]),
        );

        let result = all_effect_names(&Value::Dict(document));

        assert_eq!(result, names(&["kept"]));
    }

    #[test]
    fn an_empty_effects_list_falls_back_to_steps() {
        let mut document = Dict::new();
        document.insert(Value::Str("effects".to_string()), Value::List(Vec::new()));
        document.insert(
            Value::Str("steps".to_string()),
            Value::List(vec![effect_with(Value::Str("s".to_string()), &[])]),
        );

        let result = all_effect_names(&Value::Dict(document));

        assert_eq!(result, names(&["s"]));
    }
}
