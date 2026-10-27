//! `adapters/factory.py::_build_host_claude`, ported -- a registered
//! name whose builder always fails: `host_claude` needs a
//! *request_handler* injected at run time via `RunRequest::adapter`
//! (`circuitry-mcp`'s own use), never built from
//! `runtime.adapters.host_claude` config alone.

use electricity_value::Value;
use electricity_vm::adapter::{Adapter, AdapterError, PyExc};
use electricity_vm::retry::RetryInfo;

/// `adapters/factory.py::_build_host_claude`'s own text, as adjacent
/// literals the way Python's source writes it (`concat!`, not a
/// single long line or a backslash line-continuation, so a trailing
/// space on one piece can't be silently eaten by Rust's own
/// line-continuation whitespace-stripping).
const HOST_CLAUDE_MESSAGE: &str = concat!(
    "host_claude cannot be built from config; it requires a ",
    "request_handler injected at runtime via RunRequest.adapter ",
    "(see circuitry-mcp). Run `circuitry-mcp` (or `cof mcp`) and ",
    "drive the orchestration via the MCP tool loop, or supply ",
    "RunRequest(adapter=HostClaudeAdapter(request_handler=...)) ",
    "from a programmatic caller.",
);

/// `adapters/factory.py::_build_host_claude` -- always fails, with
/// Circuitry's exact text; not retryable (tools-adapters-plugins §7
/// case 17).
pub fn build_host_claude(_name: &str, _cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    Err(AdapterError {
        message: HOST_CLAUDE_MESSAGE.to_string(),
        retry: RetryInfo::not_retryable(),
        py_class: PyExc::RuntimeError,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expect_err(cfg: &Value) -> AdapterError {
        match build_host_claude("host_claude", cfg) {
            Err(err) => err,
            Ok(_) => panic!("build_host_claude unexpectedly succeeded"),
        }
    }

    #[test]
    fn build_host_claude_always_fails_with_pythons_exact_text() {
        let err = expect_err(&Value::Dict(Default::default()));
        assert_eq!(
            err.message,
            "host_claude cannot be built from config; it requires a request_handler injected \
             at runtime via RunRequest.adapter (see circuitry-mcp). Run `circuitry-mcp` (or \
             `cof mcp`) and drive the orchestration via the MCP tool loop, or supply \
             RunRequest(adapter=HostClaudeAdapter(request_handler=...)) from a programmatic \
             caller."
        );
        assert!(!err.retry.retryable);
        assert_eq!(err.py_class, PyExc::RuntimeError);
    }

    #[test]
    fn build_host_claude_ignores_whatever_config_it_is_given() {
        let err = expect_err(&Value::None);
        assert!(
            err.message
                .starts_with("host_claude cannot be built from config")
        );
    }
}
