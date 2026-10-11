//! `adapters/factory.py::_build_host_claude`, ported -- a registered
//! name whose builder always fails: `host_claude` needs a
//! *request_handler* injected at run time via `RunRequest::adapter`
//! (`circuitry-mcp`'s own use), never built from
//! `runtime.adapters.host_claude` config alone. Signature only (M1
//! gate seam round 2, item S9); lane F1 fills the exact message text.

use electricity_value::Value;
use electricity_vm::adapter::{Adapter, AdapterError};

/// `adapters/factory.py::_build_host_claude` -- signature only; lane
/// F1 fills the exact message text (Python's own four-sentence
/// explanation of `circuitry-mcp`/`RunRequest(adapter=...)`).
pub fn build_host_claude(_cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    crate::not_implemented_builder("host_claude", "lane F1")
}
