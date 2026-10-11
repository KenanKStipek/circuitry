//! `adapters/_openai_compat.py::OpenAICompatibleAdapter`, ported --
//! signature only (review P2-5 on PR #454's second review, extending
//! M1 gate seam round 2's item S9 to this file too); lane F2 fills
//! every body. Shared transport for every OpenAI-compatible provider
//! (`groq`, `openrouter`, `perplexity`, `xai`, `deepseek`, `together`,
//! `fireworks`, `nvidia-nim`, `vllm`, `llamacpp`, `lmstudio`,
//! `mistral`, ... -- #448's own "the OpenAI-compatible family"), one
//! adapter type configured per provider name rather than a file each.

use async_trait::async_trait;
use electricity_value::Value;
use electricity_vm::adapter::{
    Adapter, AdapterError, CallContext, CheckResult, GenerateOptions, GenerateResult, PyExc,
};
use electricity_vm::retry::RetryInfo;

/// `_openai_compat.py::OpenAICompatibleConfig` -- signature only; lane
/// F2 fills the body (base URL/API-key-env resolution per provider
/// name, model-prefix stripping, ...).
pub struct OpenAiCompatibleAdapter {
    pub provider_name: String,
    pub base_url: String,
}

#[async_trait(?Send)]
impl Adapter for OpenAiCompatibleAdapter {
    fn name(&self) -> &str {
        &self.provider_name
    }

    async fn generate(
        &self,
        _model: &str,
        _prompt: &str,
        _options: &GenerateOptions,
        _call: &CallContext<'_>,
    ) -> Result<GenerateResult, AdapterError> {
        Err(AdapterError {
            message: format!("{}: not implemented yet (lane F2)", self.provider_name),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::RuntimeError,
        })
    }

    async fn check(&self) -> CheckResult {
        CheckResult {
            ok: false,
            missing: Vec::new(),
            message: Some(format!(
                "{}: not implemented yet (lane F2)",
                self.provider_name
            )),
        }
    }
}

/// `adapters/factory.py`'s own per-provider `_build_*` functions for
/// the OpenAI-compatible family -- signature only; lane F2 fills the
/// body (one builder per provider name, each constructing an
/// [`OpenAiCompatibleAdapter`] with that provider's own default base
/// URL/API-key-env name).
pub fn build_openai_compatible(
    provider_name: &str,
    _cfg: &Value,
) -> Result<Box<dyn Adapter>, AdapterError> {
    crate::not_implemented_builder(provider_name, "lane F2")
}
