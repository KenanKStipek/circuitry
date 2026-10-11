//! `adapters/anthropic.py::AnthropicAdapter`, ported -- signature only
//! (review P2-5 on PR #454's second review, extending M1 gate seam
//! round 2's item S9 to this file too); lane F2 fills every body.

use async_trait::async_trait;
use electricity_value::Value;
use electricity_vm::adapter::{
    Adapter, AdapterError, CallContext, CheckResult, GenerateOptions, GenerateResult, PyExc,
};
use electricity_vm::retry::RetryInfo;

/// `adapters/anthropic.py::AnthropicAdapter` -- signature only; lane
/// F2 fills the body.
pub struct AnthropicAdapter;

#[async_trait(?Send)]
impl Adapter for AnthropicAdapter {
    fn name(&self) -> &str {
        "anthropic"
    }

    async fn generate(
        &self,
        _model: &str,
        _prompt: &str,
        _options: &GenerateOptions,
        _call: &CallContext<'_>,
    ) -> Result<GenerateResult, AdapterError> {
        Err(AdapterError {
            message: "anthropic: not implemented yet (lane F2)".to_string(),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::RuntimeError,
        })
    }

    async fn check(&self) -> CheckResult {
        CheckResult {
            ok: false,
            missing: Vec::new(),
            message: Some("anthropic: not implemented yet (lane F2)".to_string()),
        }
    }
}

/// `adapters/factory.py::_build_anthropic` -- signature only; lane F2
/// fills the body.
pub fn build_anthropic(_cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    crate::not_implemented_builder("anthropic", "lane F2")
}
