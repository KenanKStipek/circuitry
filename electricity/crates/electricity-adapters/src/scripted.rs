//! `adapters/scripted.py::ScriptedAdapter`, ported -- signature only
//! (M1 gate seam round 2, item S9); lane F1 fills every body. No
//! network, ever: a call is matched to its own reply queue by the
//! effect's concretized call path (`electricity_vm::adapter::CallContext::path`),
//! the same key Python's own `current_call_path` reads, for the
//! conformance suite (`electricity/DESIGN.md` §12) and for running an
//! orchestration deterministically with no real model behind it.

use async_trait::async_trait;
use electricity_value::Value;
use electricity_vm::adapter::{
    Adapter, AdapterError, CallContext, CheckResult, GenerateOptions, GenerateResult, PyExc,
};
use electricity_vm::retry::RetryInfo;

/// `ScriptedAdapter` -- signature only; lane F1 fills the body (the
/// replies-file load, the per-path reply queue, and the error-kind ->
/// retry-classification mapping `adapters/scripted.py` documents).
pub struct ScriptedAdapter {
    pub replies_file: String,
}

#[async_trait(?Send)]
impl Adapter for ScriptedAdapter {
    fn name(&self) -> &str {
        "scripted"
    }

    async fn generate(
        &self,
        _model: &str,
        _prompt: &str,
        _options: &GenerateOptions,
        _call: &CallContext<'_>,
    ) -> Result<GenerateResult, AdapterError> {
        Err(AdapterError {
            message: "scripted adapter: not implemented yet (lane F1)".to_string(),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::RuntimeError,
        })
    }

    async fn check(&self) -> CheckResult {
        CheckResult {
            ok: false,
            missing: Vec::new(),
            message: Some("scripted adapter: not implemented yet (lane F1)".to_string()),
        }
    }
}

/// `adapters/factory.py::_build_scripted` -- signature only; lane F1
/// fills the body (`cfg.get("replies_file") or "scripted-replies.yaml"`).
pub fn build_scripted(_cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    crate::not_implemented_builder("scripted", "lane F1")
}
