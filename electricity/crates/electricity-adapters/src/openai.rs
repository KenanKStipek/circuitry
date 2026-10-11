//! `adapters/openai.py::OpenAIAdapter`, ported -- signature only
//! (review P2-5 on PR #454's second review, extending M1 gate seam
//! round 2's item S9 to this file too); lane F2 fills every body.
//! Its own file (not folded into [`crate::openai_compat`]): `openai`
//! is Circuitry's own default/reference provider, with its own
//! response-format handling `_openai_compat.py`'s generic shape
//! doesn't cover (`openai.py`'s own module doc comment).

use async_trait::async_trait;
use electricity_value::Value;
use electricity_vm::adapter::{
    Adapter, AdapterError, CallContext, CheckResult, GenerateOptions, GenerateResult, PyExc,
};
use electricity_vm::retry::RetryInfo;

/// `adapters/openai.py::OpenAIAdapter` -- signature only; lane F2
/// fills the body.
pub struct OpenAiAdapter;

#[async_trait(?Send)]
impl Adapter for OpenAiAdapter {
    fn name(&self) -> &str {
        "openai"
    }

    async fn generate(
        &self,
        _model: &str,
        _prompt: &str,
        _options: &GenerateOptions,
        _call: &CallContext<'_>,
    ) -> Result<GenerateResult, AdapterError> {
        Err(AdapterError {
            message: "openai: not implemented yet (lane F2)".to_string(),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::RuntimeError,
        })
    }

    async fn check(&self) -> CheckResult {
        CheckResult {
            ok: false,
            missing: Vec::new(),
            message: Some("openai: not implemented yet (lane F2)".to_string()),
        }
    }
}

/// `adapters/factory.py::_build_openai` -- signature only; lane F2
/// fills the body.
pub fn build_openai(_cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    crate::not_implemented_builder("openai", "lane F2")
}
