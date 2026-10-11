//! `adapters/factory.py::ADAPTER_REGISTRY`/`build_adapter`, ported --
//! the crate that implements concrete [`electricity_vm::adapter::Adapter`]s
//! (DESIGN.md §2's own dependency direction: this crate depends on
//! `electricity-vm`, which defines the trait boundary, never the
//! reverse). Not created until lane F1's own first PR per
//! `electricity/docs/spec/vm-lanes.md`'s M1 table -- this file set is
//! the gate's own stub (M1 gate seam round 2, item S9), so lane F1's
//! PR only ever fills a body, never adds a new file or a `pub mod`
//! line to this list.
//!
//! - [`scripted`]: the Rust `scripted` adapter (lane F1) --
//!   deterministic replies from a replies file, keyed by call path,
//!   for the conformance suite (`electricity/DESIGN.md` §12) and for
//!   running an orchestration with no real model behind it.
//! - [`host_claude`]: the `host_claude` registration stub (lane F1) --
//!   a name `build_registry` recognizes, whose builder always fails:
//!   this adapter needs a *request_handler* injected at run time via
//!   `RunRequest::adapter` (`circuitry-mcp`'s own use), never built
//!   from `runtime.adapters.host_claude` config alone.
//!
//! Every real network-backed provider (`ollama`, the OpenAI-compatible
//! family, `anthropic`) is lane F2's own, added as sibling modules
//! here in that lane's own PR.

pub mod host_claude;
pub mod scripted;

use electricity_vm::adapter::{Adapter, AdapterError, AdapterRegistry, PyExc};
use electricity_vm::retry::RetryInfo;

/// `adapters/factory.py::build_adapter`'s own per-attempt registry --
/// signature only; lane F1 fills the body (registering [`scripted`]/
/// [`host_claude`]'s own builders; lane F2 adds its own providers'
/// builders to the same registry in its own PR, each one line in this
/// function, never a new file).
pub fn build_registry() -> AdapterRegistry {
    AdapterRegistry::new()
}

/// A builder stub shared by every not-yet-implemented adapter in this
/// crate -- lane F1/F2 replaces each call site with a real builder,
/// never widening [`electricity_vm::adapter::AdapterBuilder`]'s own
/// signature.
pub(crate) fn not_implemented_builder(
    adapter_name: &str,
    lane: &str,
) -> Result<Box<dyn Adapter>, AdapterError> {
    Err(AdapterError {
        message: format!("electricity_adapters::{adapter_name}: not implemented yet ({lane})"),
        retry: RetryInfo::not_retryable(),
        py_class: PyExc::RuntimeError,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_registry_has_no_builders_yet() {
        let registry = build_registry();
        assert!(registry.supported_names().is_empty());
    }
}
