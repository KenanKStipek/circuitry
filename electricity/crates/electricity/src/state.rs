//! Seeding a fresh run's state (issue #431's run-wiring step 4):
//! `cli/runtime_shim.py::_load_state(inline, None)`, where *inline* is
//! the CLI's own `-e` entries, JSON-sniffed (`cli/app.py::
//! _parse_env_vars`). `_load_state` is `migrate_legacy_state(inline)`
//! (`core/state_ns.py::migrate_legacy_state`,
//! [`electricity_compiler::state_ns::migrate_legacy_state`] here) --
//! the *whole* root, not just an `input` projection of it (PR #441
//! review finding 1d): every root key that is neither a namespace name
//! (`input`/`prime`/`runtime`) nor `_`-prefixed is lifted under a fresh
//! `input` key appended at the end; a namespace-named or `_`-prefixed
//! key (`-e _tag=x`, `-e prime=...`) is left exactly where it already
//! was, at the root, never lifted; and an inline dict that already has
//! its own `input` key (`-e input={...}`) wins outright, returned
//! completely untouched -- a sibling `-e extra=...` stays at the root
//! too in that case, exactly as it would have if `input` were absent.
//!
//! [`seed_state`] is the reason a check failure from step 5 onward
//! (load, allowlist, effective settings, complexity, concurrency,
//! persistence) still reports the same root `cof run` of the same `-e`
//! values would, instead of an empty `{"input": {}}` -- PR #441 review
//! finding 3. [`run_orchestration`]'s own step 5 reseeds `state["input"]`
//! specifically (restoring a declared `type: string` input's own raw
//! text, PR #441 review finding 1c) once the real document is loaded,
//! replacing this module's sniff-only seed for that one namespace;
//! every other root key this module writes is untouched from here on.
//! Once [`electricity_compiler::pre_state_checks`]/[`build_input_
//! namespace`] itself succeeds (step 10, `check_interface_inputs`), its
//! own coerced/defaulted namespace *replaces* `state["input"]` outright
//! (`run_orchestration`'s own job, not this module's, and a replace,
//! not a merge -- PR #441 review finding 1a: a key the final namespace
//! doesn't carry forward, e.g. an optional input whose seed was `null`,
//! must not survive from this seed); on a step-10 failure, [`electricity_
//! compiler::build_input_namespace_best_effort`] -- not this module --
//! recovers the partial mutation `check_interface_inputs`'s own in-place
//! edit would have left.

use electricity_value::{Dict, Value};
use electricity_vm::Store;

/// *inputs*'s own JSON-sniffed values -- `cli/app.py::_parse_env_vars`'s
/// own per-entry JSON-sniff (a value that isn't valid JSON stays its
/// own literal text, e.g. `-e name=World`).
fn sniffed(inputs: &indexmap::IndexMap<String, String>) -> indexmap::IndexMap<String, Value> {
    inputs
        .iter()
        .map(|(key, raw)| {
            let value = electricity_json::loads(raw).unwrap_or_else(|_| Value::Str(raw.clone()));
            (key.clone(), value)
        })
        .collect()
}

/// Seeds *store*'s root from *inputs* (the CLI's own `-e key=value`
/// entries, in order) -- see this module's own doc comment. Every key
/// [`electricity_compiler::state_ns::migrate_legacy_state`] returns
/// lands at the store root, `input`'s own dict value included;
/// infallible in practice (`Store::set_leaf` never actually fails), so
/// no `Result` return.
pub fn seed_state(store: &Store, inputs: &indexmap::IndexMap<String, String>) {
    let root_namespace = electricity_compiler::state_ns::migrate_legacy_state(sniffed(inputs));
    for (key, value) in root_namespace {
        store.set_leaf(&store.root, Value::Str(key), value);
    }
}

/// Replaces `state["input"]` wholesale with *namespace* -- never a
/// merge onto whatever is already there (PR #441 review finding 1a: a
/// key the new namespace doesn't carry forward must not survive from
/// an earlier seed). The one place [`run_orchestration`] ever writes
/// `state["input"]` after [`seed_state`]'s own initial write, whether
/// re-seeding with the document's own raw-text restoration (step 5,
/// finding 1c) or installing [`electricity_compiler::build_input_
/// namespace`]/[`build_input_namespace_best_effort`]'s own result
/// (step 10).
pub fn replace_input_namespace(store: &Store, namespace: indexmap::IndexMap<Value, Value>) {
    let dict: Dict = namespace.into_iter().collect();
    store.set_leaf(
        &store.root,
        Value::Str("input".to_string()),
        Value::Dict(dict),
    );
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
        let dict = snapshot.as_dict().unwrap();
        let input = dict
            .get(&Value::Str("input".to_string()))
            .unwrap()
            .as_dict()
            .unwrap();
        assert_eq!(
            input.keys().cloned().collect::<Vec<_>>(),
            vec![Value::Str("only".to_string())]
        );
        // `name` is neither a namespace name nor `_`-prefixed, but once
        // `input` already wins outright `migrate_legacy_state` returns
        // the whole inline dict untouched -- Python's own `if INPUT_NS
        // in state: return state` never pops anything at all in that
        // branch, so `name` lands at the root, a sibling of `input`,
        // confirmed directly against a real `cof run -e name=World -e
        // 'input={"only": "this"}'`.
        assert_eq!(
            dict.get(&Value::Str("name".to_string())),
            Some(&Value::Str("World".to_string()))
        );
    }

    // PR #441 review finding 1d.
    #[test]
    fn seed_state_keeps_an_underscore_prefixed_root_key_at_the_root() {
        let store = Store::new();
        let mut inputs = indexmap::IndexMap::new();
        inputs.insert("_tag".to_string(), "x".to_string());
        seed_state(&store, &inputs);
        let snapshot = store.snapshot(&store.root);
        let dict = snapshot.as_dict().unwrap();
        assert_eq!(
            dict.get(&Value::Str("_tag".to_string())),
            Some(&Value::Str("x".to_string()))
        );
        assert_eq!(
            dict.get(&Value::Str("input".to_string())),
            Some(&Value::Dict(Default::default()))
        );
    }

    #[test]
    fn seed_state_keeps_a_namespace_named_root_key_at_the_root() {
        let store = Store::new();
        let mut inputs = indexmap::IndexMap::new();
        inputs.insert("prime".to_string(), "5".to_string());
        seed_state(&store, &inputs);
        let snapshot = store.snapshot(&store.root);
        let dict = snapshot.as_dict().unwrap();
        assert_eq!(
            dict.get(&Value::Str("prime".to_string())),
            Some(&Value::Int(5.into()))
        );
    }

    #[test]
    fn seed_state_keeps_a_sibling_key_at_the_root_when_input_already_wins() {
        let store = Store::new();
        let mut inputs = indexmap::IndexMap::new();
        inputs.insert("input".to_string(), r#"{"a": 1}"#.to_string());
        inputs.insert("extra".to_string(), "2".to_string());
        seed_state(&store, &inputs);
        let snapshot = store.snapshot(&store.root);
        let dict = snapshot.as_dict().unwrap();
        assert_eq!(
            dict.get(&Value::Str("extra".to_string())),
            Some(&Value::Int(2.into()))
        );
        let input = dict
            .get(&Value::Str("input".to_string()))
            .unwrap()
            .as_dict()
            .unwrap();
        assert_eq!(
            input.get(&Value::Str("a".to_string())),
            Some(&Value::Int(1.into()))
        );
    }

    #[test]
    fn replace_input_namespace_drops_a_key_the_new_namespace_does_not_carry() {
        let store = Store::new();
        let mut inputs = indexmap::IndexMap::new();
        inputs.insert("x".to_string(), "null".to_string());
        seed_state(&store, &inputs);

        let mut namespace = indexmap::IndexMap::new();
        namespace.insert(Value::Str("y".to_string()), Value::Int(1.into()));
        replace_input_namespace(&store, namespace);

        let snapshot = store.snapshot(&store.root);
        let input = snapshot
            .as_dict()
            .unwrap()
            .get(&Value::Str("input".to_string()))
            .unwrap()
            .as_dict()
            .unwrap();
        assert_eq!(input.get(&Value::Str("x".to_string())), None);
        assert_eq!(
            input.get(&Value::Str("y".to_string())),
            Some(&Value::Int(1.into()))
        );
    }
}
