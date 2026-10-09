//! `cli/effective_settings.py::_merge_runtime` -- a document's own
//! `runtime:` block deep-merged over the config's, with one ceiling
//! re-intersected afterward rather than simply overwritten: the shell
//! `allowed_commands` pin is a host ceiling even for a trusted document
//! (DESIGN.md §11's "every document electricity runs is trusted" does
//! not mean "every document can widen the shell allowlist" -- those are
//! separate axes in Circuitry too, confirmed directly against
//! `_apply_ceiling_intersections`, which runs unconditionally, with no
//! `trusted` parameter of its own at all).

use crate::util::get;
use electricity_value::{Dict, Value};

/// `_DEEP_MERGE_RUNTIME_KEYS`: the two `runtime` keys merged one level
/// deeper than the rest -- a document setting `plugins.sqlite.*` must
/// not drop `plugins.shell.*`, and setting `adapters.ollama.*` must not
/// drop every other adapter's config. Every other top-level `runtime`
/// key replaces wholesale.
const DEEP_MERGE_RUNTIME_KEYS: [&str; 2] = ["plugins", "adapters"];

/// `_CEILING_LIST_KEYS`: `(top_key, name, leaf_key)` triples the merge
/// re-narrows to a host/document intersection after the ordinary merge,
/// rather than letting either side simply replace the other's list.
const CEILING_LIST_KEYS: [(&str, &str, &str); 1] = [("plugins", "shell", "allowed_commands")];

fn deep_merge_dicts(base: &Dict, overlay: &Dict) -> Dict {
    let mut merged = base.clone();
    for (key, value) in overlay {
        let recursed = match (merged.get(key), value) {
            (Some(Value::Dict(base_dict)), Value::Dict(overlay_dict)) => {
                Some(Value::Dict(deep_merge_dicts(base_dict, overlay_dict)))
            }
            _ => None,
        };
        merged.insert(key.clone(), recursed.unwrap_or_else(|| value.clone()));
    }
    merged
}

fn nested_list<'a>(
    runtime: &'a Dict,
    top_key: &str,
    name: &str,
    leaf_key: &str,
) -> Option<&'a [Value]> {
    let top = get(runtime, top_key).and_then(Value::as_dict)?;
    let sub = get(top, name).and_then(Value::as_dict)?;
    get(sub, leaf_key).and_then(Value::as_list)
}

fn set_nested_list(merged: &mut Dict, top_key: &str, name: &str, leaf_key: &str, list: Vec<Value>) {
    let mut top = match merged.get(&Value::Str(top_key.to_string())) {
        Some(Value::Dict(d)) => d.clone(),
        _ => Dict::new(),
    };
    let mut sub = match top.get(&Value::Str(name.to_string())) {
        Some(Value::Dict(d)) => d.clone(),
        _ => Dict::new(),
    };
    sub.insert(Value::Str(leaf_key.to_string()), Value::List(list));
    top.insert(Value::Str(name.to_string()), Value::Dict(sub));
    merged.insert(Value::Str(top_key.to_string()), Value::Dict(top));
}

/// `_apply_ceiling_intersections`: re-narrows each [`CEILING_LIST_KEYS`]
/// entry to the host/document intersection, whenever the host (config)
/// side has a pin at all -- a document's own list, or a `null`/
/// non-list value for that leaf (or a non-object ancestor), narrows it
/// further but can never erase it.
fn apply_ceiling_intersections(
    merged: Dict,
    config_runtime: Option<&Dict>,
    document_runtime: Option<&Dict>,
) -> Dict {
    let mut merged = merged;
    for (top_key, name, leaf_key) in CEILING_LIST_KEYS {
        let Some(host_list) = config_runtime.and_then(|r| nested_list(r, top_key, name, leaf_key))
        else {
            continue;
        };
        let document_list = document_runtime.and_then(|r| nested_list(r, top_key, name, leaf_key));
        let effective: Vec<Value> = match document_list {
            Some(document_list) => host_list
                .iter()
                .filter(|item| document_list.iter().any(|d| d.py_eq(item)))
                .cloned()
                .collect(),
            None => host_list.to_vec(),
        };
        set_nested_list(&mut merged, top_key, name, leaf_key, effective);
    }
    merged
}

/// `cli/effective_settings.py::_merge_runtime(config_runtime,
/// document_runtime)`.
pub fn merge_runtime(
    config_runtime: Option<&Value>,
    document_runtime: Option<&Value>,
) -> Option<Value> {
    if config_runtime.is_none() && document_runtime.is_none() {
        return None;
    }

    let config_dict = config_runtime.and_then(Value::as_dict);
    let document_dict = document_runtime.and_then(Value::as_dict);

    let mut merged = config_dict.cloned().unwrap_or_default();
    if let Some(document_dict) = document_dict {
        for (key, value) in document_dict {
            let key_str = key.as_str();
            let deep = key_str.is_some_and(|k| DEEP_MERGE_RUNTIME_KEYS.contains(&k));
            let recursed = if deep {
                match (merged.get(key), value) {
                    (Some(Value::Dict(base)), Value::Dict(overlay)) => {
                        Some(Value::Dict(deep_merge_dicts(base, overlay)))
                    }
                    _ => None,
                }
            } else {
                None
            };
            merged.insert(key.clone(), recursed.unwrap_or_else(|| value.clone()));
        }
    }

    Some(Value::Dict(apply_ceiling_intersections(
        merged,
        config_dict,
        document_dict,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict_of(pairs: Vec<(&str, Value)>) -> Value {
        let mut dict = Dict::new();
        for (k, v) in pairs {
            dict.insert(Value::Str(k.to_string()), v);
        }
        Value::Dict(dict)
    }

    fn nested(path: &[&str], leaf: Value) -> Value {
        let mut value = leaf;
        for key in path.iter().rev() {
            value = dict_of(vec![(key, value)]);
        }
        value
    }

    #[test]
    fn neither_side_is_none() {
        assert_eq!(merge_runtime(None, None), None);
    }

    #[test]
    fn plugins_merges_one_level_deeper_than_the_rest() {
        let config = dict_of(vec![(
            "plugins",
            dict_of(vec![(
                "sqlite",
                dict_of(vec![("path", Value::from("a.db"))]),
            )]),
        )]);
        let document = dict_of(vec![(
            "plugins",
            dict_of(vec![(
                "shell",
                dict_of(vec![("timeout", Value::from(5i64))]),
            )]),
        )]);
        let merged = merge_runtime(Some(&config), Some(&document)).unwrap();
        let plugins = merged
            .as_dict()
            .unwrap()
            .get(&Value::Str("plugins".into()))
            .unwrap()
            .as_dict()
            .unwrap();
        assert!(plugins.contains_key(&Value::Str("sqlite".into())));
        assert!(plugins.contains_key(&Value::Str("shell".into())));
    }

    #[test]
    fn a_non_merge_key_replaces_wholesale() {
        let config = dict_of(vec![(
            "complexity",
            dict_of(vec![("scoring", Value::from(true))]),
        )]);
        let document = dict_of(vec![(
            "complexity",
            dict_of(vec![("routing", Value::from(true))]),
        )]);
        let merged = merge_runtime(Some(&config), Some(&document)).unwrap();
        let complexity = merged
            .as_dict()
            .unwrap()
            .get(&Value::Str("complexity".into()))
            .unwrap()
            .as_dict()
            .unwrap();
        assert!(!complexity.contains_key(&Value::Str("scoring".into())));
        assert!(complexity.contains_key(&Value::Str("routing".into())));
    }

    #[test]
    fn the_shell_allowlist_ceiling_survives_even_when_the_document_sets_its_own() {
        let config = nested(
            &["plugins", "shell"],
            dict_of(vec![(
                "allowed_commands",
                Value::List(vec![Value::from("ls"), Value::from("cat")]),
            )]),
        );
        let document = nested(
            &["plugins", "shell"],
            dict_of(vec![(
                "allowed_commands",
                Value::List(vec![
                    Value::from("cat"),
                    Value::from("rm"),
                    Value::from("curl"),
                ]),
            )]),
        );
        let merged = merge_runtime(Some(&config), Some(&document)).unwrap();
        let allowed = merged
            .as_dict()
            .unwrap()
            .get(&Value::Str("plugins".into()))
            .unwrap()
            .as_dict()
            .unwrap()
            .get(&Value::Str("shell".into()))
            .unwrap()
            .as_dict()
            .unwrap()
            .get(&Value::Str("allowed_commands".into()))
            .unwrap()
            .as_list()
            .unwrap();
        // Only the intersection survives: the document can narrow the
        // ceiling ("cat") but never widen it ("rm"/"curl" dropped).
        assert_eq!(allowed, &[Value::from("cat")]);
    }

    #[test]
    fn the_shell_allowlist_ceiling_survives_a_document_that_drops_the_block_entirely() {
        let config = nested(
            &["plugins", "shell"],
            dict_of(vec![(
                "allowed_commands",
                Value::List(vec![Value::from("ls")]),
            )]),
        );
        // The document's own `runtime.plugins` has nothing to say about
        // shell at all -- the host's pin must not vanish either.
        let document = dict_of(vec![("adapters", dict_of(vec![]))]);
        let merged = merge_runtime(Some(&config), Some(&document)).unwrap();
        let allowed = merged
            .as_dict()
            .unwrap()
            .get(&Value::Str("plugins".into()))
            .unwrap()
            .as_dict()
            .unwrap()
            .get(&Value::Str("shell".into()))
            .unwrap()
            .as_dict()
            .unwrap()
            .get(&Value::Str("allowed_commands".into()))
            .unwrap()
            .as_list()
            .unwrap();
        assert_eq!(allowed, &[Value::from("ls")]);
    }
}
