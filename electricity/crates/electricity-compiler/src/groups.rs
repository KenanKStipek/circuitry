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
//! Called from `compile::compile_document` (lane C, same lane, after
//! every effect compiles -- `group:` references can name a group
//! defined anywhere in `runtime.concurrency_groups`, order-independent).

use crate::CompileError;
use electricity_value::Value;

/// Every `group:` reference in *document* that doesn't name one of
/// *concurrency_groups*.
///
/// Stub (lane A): always fails until lane C lands.
pub(crate) fn unknown_group_errors(
    document: &Value,
    concurrency_groups: &[String],
) -> Result<(), CompileError> {
    let _ = (document, concurrency_groups);
    Err(CompileError(crate::not_implemented(
        "groups::unknown_group_errors",
        "C",
    )))
}
