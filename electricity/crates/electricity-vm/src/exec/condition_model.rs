//! Model-mode `if`/`while` and model-mode `expect:` (`core/
//! conditional.py::_evaluate_model`, `core/loop.py::_evaluate_model`,
//! `core/expect.py::evaluate_expect` mode `model`) -- signatures only;
//! lane G2 fills the bodies. Issue #449's gate lane, item 8.
//!
//! Not yet routed from anywhere: a `Condition::Model`/
//! `ExpectCondition::Model` node is refused by
//! [`electricity_bytecode::refusal::first_unsupported`] before a real
//! run ever reaches `exec::conditional`/`exec::tool`, so neither
//! function below has a real caller in this build -- lane G2 wires
//! both in once [`electricity_bytecode::refusal::Supported::model_condition`]/
//! [`electricity_bytecode::refusal::Supported::model_expect`] flip to
//! `true`.

use crate::adapter::GenerateResult;
use crate::{CancellationToken, Frame, RunContext, VmError};
use electricity_bytecode::{EffectPath, TemplateText};
use electricity_value::Value;

/// A model-mode `if`'s/`while`'s own answer: `core/answers.py::
/// parse_boolean_answer`'s verdict, plus the adapter call's own
/// [`GenerateResult`] (for the container's own `meta.answer`/`adapter`/
/// `model`/`tokens_*`).
pub struct ModelConditionOutcome {
    pub passed: bool,
    pub generated: GenerateResult,
}

/// Renders *template*, dispatches it through *run_ctx*'s own default
/// adapter, and parses the reply as a yes/no answer. *path* is this
/// transparent condition's own container path (`Frame::effect_path`'s
/// result, `None` passed to `core/effect_identity.py::call_path` for a
/// transparent `if`/`while`) -- lane G2's own
/// `crate::adapter::CallContext::path` once it wires this stub in.
pub async fn evaluate_model_condition(
    template: &TemplateText,
    _ctx: &Value,
    _path: &EffectPath,
    _frame: &Frame,
    _run_ctx: &RunContext<'_>,
    _token: &CancellationToken,
) -> Result<ModelConditionOutcome, VmError> {
    Err(VmError::NotImplemented(format!(
        "{:?}: a model-mode condition is not supported by this build of electricity yet (M1-G2)",
        template.source
    )))
}

/// A model-mode `expect:`'s own verdict, plus the adapter call's own
/// [`GenerateResult`] (for the effect's own `meta.expect`).
pub struct ModelExpectOutcome {
    pub passed: bool,
    pub generated: GenerateResult,
}

/// *path* is the `tool`/`prompt` effect's own path this `expect:`
/// belongs to -- [`evaluate_model_condition`]'s own doc comment.
pub async fn evaluate_model_expect(
    template: &TemplateText,
    _value: &Value,
    _ctx: &Value,
    _path: &EffectPath,
    _frame: &Frame,
    _run_ctx: &RunContext<'_>,
    _token: &CancellationToken,
) -> Result<ModelExpectOutcome, VmError> {
    Err(VmError::NotImplemented(format!(
        "{:?}: a model-mode expect is not supported by this build of electricity yet (M1-G2)",
        template.source
    )))
}
