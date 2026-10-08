//! Lane C: ports `core/compiler.py`'s `unknown_concurrency_group_errors`
//! -- an effect's `group:` reference checked against the resolved
//! `runtime.concurrency_groups` names (`core/concurrency.py`'s
//! `UnknownConcurrencyGroupError`).
//!
//! The `max_concurrency`/`concurrency_groups` *configuration* errors
//! (`core/concurrency.py`'s `parse_max_concurrency`/
//! `parse_concurrency_groups`) are lane B's, in `pipeline.rs`: they
//! validate the merged runtime config itself, before a document even
//! compiles, not an effect's reference into it -- see that module's
//! `concurrency_config_errors`.
//!
//! Called from `pipeline.rs` (lane B), after a document compiles, in
//! both `check_for_run` and `check_report` -- mirroring
//! `cli/runtime_shim.py`'s own two calls to `unknown_concurrency_group_errors`
//! against the merged config's `runtime.concurrency_groups` names:
//! `run(RunRequest(..., validate_only=True))` (`cli/runtime_shim.py:756`)
//! and `validate()` (`cli/runtime_shim.py:1358`). Not a step of
//! `compile::compile_document` itself: unknown group references belong
//! to the pipeline surface, which knows the merged runtime config, not
//! to compilation, which doesn't.

use electricity_bytecode::Program;
use std::collections::BTreeSet;

/// Every `group:` reference under *program*'s root that *known_groups*
/// -- `runtime.concurrency_groups`'s own keys -- doesn't define,
/// porting `core/compiler.py`'s `unknown_concurrency_group_errors`
/// (and the `collect_effect_groups` walk it calls) field for field:
///
/// - walk *program* the way `collect_effect_groups` walks a compiled
///   `EffectDef` tree: collect every tool/prompt effect's `group:` name
///   anywhere under the root, *not* descending into a `use` effect's
///   child (that child document isn't compiled yet when its parent is;
///   its own `group:` fields are checked against the same known groups
///   separately, when it loads);
/// - the names in that collected set that *known_groups* doesn't
///   contain, sorted;
/// - when that sorted list is empty, no errors;
/// - otherwise, one message per unknown name, in that sorted order:
///   `` group 'name' is not defined in runtime.concurrency_groups — known groups: known_desc. ``
///   where `name` is the unknown group's name (Python `repr()`:
///   single-quoted), and `known_desc` is *known_groups*' own names,
///   sorted and joined with `", "`, or the literal text
///   `(none configured)` when *known_groups* is empty.
///
/// Stub (lane A): always fails until lane C lands.
pub(crate) fn unknown_group_errors(
    program: &Program,
    known_groups: &BTreeSet<String>,
) -> Vec<String> {
    let _ = (program, known_groups);
    vec![crate::not_implemented("groups::unknown_group_errors", "C")]
}
