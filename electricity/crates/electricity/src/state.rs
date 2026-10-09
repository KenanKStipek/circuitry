//! Seeding a fresh run's state (issue #431's run-wiring step 4):
//! `cli/runtime_shim.py::_load_state(None, None)` -- the one branch
//! electricity's own run wiring ever reaches, since there is no
//! `--state` flag and no out-of-band `initial_state` object: every
//! `-e` value instead flows through `electricity_compiler::
//! build_input_namespace` (reached later, inside `pre_state_checks`),
//! not through `_load_state`'s own `initial_state` argument.
//!
//! `_load_state(None, None)` is `migrate_legacy_state({})`, which lifts
//! every non-namespace root key under `input` -- on an empty dict there
//! is nothing to lift, so the only observable effect is that `state`
//! ends up carrying an empty `input` namespace before anything else
//! runs. [`seed_state`] is that one effect, directly, rather than a
//! full port of `migrate_legacy_state` (whose "lift a legacy bare root
//! key" behaviour electricity's own run wiring has no caller for: no
//! `--state` file, no persisted-snapshot hydration in M0-H).

use electricity_value::Value;
use electricity_vm::Store;

/// Seeds *store*'s root with an empty `input` namespace -- see this
/// module's own doc comment. Infallible in practice (`Store::
/// ensure_dict` never actually returns `Err`); `.expect` rather than a
/// `Result` return keeps every caller from having to handle an error
/// that cannot happen.
pub fn seed_state(store: &Store) {
    store
        .ensure_dict(&store.root, Value::Str("input".to_string()))
        .expect("Store::ensure_dict on a fresh root never fails");
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_vm::Store;

    #[test]
    fn seed_state_gives_the_root_an_empty_input_namespace() {
        let store = Store::new();
        seed_state(&store);
        let snapshot = store.snapshot(&store.root);
        let dict = snapshot.as_dict().unwrap();
        assert_eq!(
            dict.get(&Value::Str("input".to_string())),
            Some(&Value::Dict(Default::default()))
        );
    }
}
