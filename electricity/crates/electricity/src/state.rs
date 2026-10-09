//! Seeding a fresh run's state (issue #431's run-wiring step 4):
//! `cli/runtime_shim.py::_load_state(inline, None)`, where *inline* is
//! the CLI's own `-e` entries, JSON-sniffed (`cli/app.py::
//! _parse_env_vars`) -- *before* `_restore_raw_text_for_string_inputs`'s
//! own best-effort peek at the document's declared `interface.inputs`,
//! which only ever refines a `type: string` input's own text, and
//! which this step runs strictly before the document is even loaded
//! (step 5). `_load_state` is `migrate_legacy_state(inline)`
//! (`core/state_ns.py::migrate_legacy_state`,
//! [`electricity_compiler::state_ns::migrate_legacy_input_namespace`]
//! here), which lifts every non-namespace, non-`_`-prefixed root key
//! under `input` -- or, if *inline* itself has an `input` key, takes
//! that key's own dict value outright.
//!
//! [`seed_state`] is the reason a check failure from step 5 onward
//! (load, allowlist, effective settings, complexity, concurrency,
//! persistence) still reports the same `state["input"]` a `cof run` of
//! the same `-e` values would, instead of an empty `{}` -- PR #441
//! review finding 3. Once [`electricity_compiler::pre_state_checks`]
//! itself succeeds (step 10, `check_interface_inputs`), its own
//! coerced/defaulted namespace overwrites this seed outright
//! (`run_orchestration`'s own job, not this module's); on a step-10
//! failure, [`electricity_compiler::build_input_namespace_best_effort`]
//! -- not this module -- recovers the partial mutation `check_
//! interface_inputs`'s own in-place edit would have left.

use electricity_value::Value;
use electricity_vm::Store;

/// *inputs*'s own JSON-sniffed, [`electricity_compiler::state_ns::
/// migrate_legacy_input_namespace`]-lifted namespace -- see this
/// module's own doc comment. `cli/app.py::_parse_env_vars`'s own
/// per-entry JSON-sniff (a value that isn't valid JSON stays its own
/// literal text, e.g. `-e name=World`).
fn sniffed_input_namespace(
    inputs: &indexmap::IndexMap<String, String>,
) -> indexmap::IndexMap<String, Value> {
    let sniffed: indexmap::IndexMap<String, Value> = inputs
        .iter()
        .map(|(key, raw)| {
            let value = electricity_json::loads(raw).unwrap_or_else(|_| Value::Str(raw.clone()));
            (key.clone(), value)
        })
        .collect();
    electricity_compiler::state_ns::migrate_legacy_input_namespace(&sniffed)
}

/// Seeds *store*'s root with an `input` namespace built from *inputs*
/// (the CLI's own `-e key=value` entries, in order) -- see this
/// module's own doc comment. Infallible in practice (`Store::
/// ensure_dict`/`Store::set_leaf` never actually fail); no `Result`
/// return, so every caller is spared handling an error that cannot
/// happen.
pub fn seed_state(store: &Store, inputs: &indexmap::IndexMap<String, String>) {
    let input_node = store
        .ensure_dict(&store.root, Value::Str("input".to_string()))
        .expect("Store::ensure_dict on a fresh root never fails");
    for (key, value) in sniffed_input_namespace(inputs) {
        store.set_leaf(&input_node, Value::Str(key), value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_vm::Store;

    #[test]
    fn seed_state_gives_the_root_an_empty_input_namespace_with_no_e_values() {
        let store = Store::new();
        seed_state(&store, &indexmap::IndexMap::new());
        let snapshot = store.snapshot(&store.root);
        let dict = snapshot.as_dict().unwrap();
        assert_eq!(
            dict.get(&Value::Str("input".to_string())),
            Some(&Value::Dict(Default::default()))
        );
    }

    #[test]
    fn seed_state_lifts_a_sniffed_e_value_into_input() {
        let store = Store::new();
        let mut inputs = indexmap::IndexMap::new();
        inputs.insert("name".to_string(), "World".to_string());
        inputs.insert("count".to_string(), "5".to_string());
        seed_state(&store, &inputs);
        let snapshot = store.snapshot(&store.root);
        let input = snapshot
            .as_dict()
            .unwrap()
            .get(&Value::Str("input".to_string()))
            .unwrap()
            .as_dict()
            .unwrap();
        assert_eq!(
            input.get(&Value::Str("name".to_string())),
            Some(&Value::Str("World".to_string()))
        );
        // `-e count=5` JSON-sniffs to the int `5` at this seeding step --
        // the raw-text restore for a declared `type: string` input only
        // happens once the document is loaded, inside `build_input_
        // namespace`/`build_input_namespace_best_effort`.
        assert_eq!(
            input.get(&Value::Str("count".to_string())),
            Some(&Value::Int(5.into()))
        );
    }

    #[test]
    fn seed_state_an_input_key_wins_outright_over_every_other_e_key() {
        let store = Store::new();
        let mut inputs = indexmap::IndexMap::new();
        inputs.insert("name".to_string(), "World".to_string());
        inputs.insert("input".to_string(), r#"{"only": "this"}"#.to_string());
        seed_state(&store, &inputs);
        let snapshot = store.snapshot(&store.root);
        let input = snapshot
            .as_dict()
            .unwrap()
            .get(&Value::Str("input".to_string()))
            .unwrap()
            .as_dict()
            .unwrap();
        assert_eq!(
            input.keys().cloned().collect::<Vec<_>>(),
            vec![Value::Str("only".to_string())]
        );
    }
}
