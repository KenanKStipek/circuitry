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
use crate::{CancellationToken, RunContext, VmError};
use electricity_bytecode::TemplateText;
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
/// adapter, and parses the reply as a yes/no answer.
pub async fn evaluate_model_condition(
    template: &TemplateText,
    _ctx: &Value,
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

pub async fn evaluate_model_expect(
    template: &TemplateText,
    _value: &Value,
    _ctx: &Value,
    _run_ctx: &RunContext<'_>,
    _token: &CancellationToken,
) -> Result<ModelExpectOutcome, VmError> {
    Err(VmError::NotImplemented(format!(
        "{:?}: a model-mode expect is not supported by this build of electricity yet (M1-G2)",
        template.source
    )))
}
