//! Model-mode `if`/`while` and model-mode `expect:` (`core/
//! conditional.py::_evaluate_model`, `core/loop.py::_evaluate_model`,
//! `core/expect.py::evaluate_expect` mode `model`), filled in (issue
//! #513, lane G2).
//!
//! Routed from [`super::conditional`] (a named/unnamed `if`),
//! [`super::loop_`] (a named/unnamed `while`) and [`super::tool`]/
//! [`super::use_`] (`expect: {mode: model}`) once
//! [`electricity_bytecode::refusal::Supported::model_condition`]/
//! [`electricity_bytecode::refusal::Supported::model_expect`] are
//! `true`. Every caller passes its own already-[`crate::Frame::
//! effect_path`]-resolved [`EffectPath`] -- this module never resolves
//! one itself (issue #513's settled question 1: resolving twice would
//! double a `use` prefix).

use crate::adapter::{CallContext, GenerateOptions, GenerateResult};
use crate::{CancellationToken, RunContext, VmError};
use electricity_bytecode::{EffectPath, TemplateText};
use electricity_tools::PyExc;
use electricity_value::Value;
use indexmap::IndexMap;

/// Which prompt suffix/render label/dry-run default a model condition
/// uses -- the one difference between an `if`'s and a `while`'s own
/// `_evaluate_model` (issue #513's own "`if` vs `while`" table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConditionKind {
    If,
    While,
}

impl ConditionKind {
    fn label(self) -> &'static str {
        match self {
            ConditionKind::If => "if.template",
            ConditionKind::While => "while.template",
        }
    }

    fn prompt_suffix(self) -> &'static str {
        match self {
            ConditionKind::If => "Answer (yes/no):",
            ConditionKind::While => "Should the loop continue? Answer (yes/no):",
        }
    }

    /// `core/conditional.py`'s/`core/loop.py`'s own dry-run shortcut --
    /// `if` answers `True` without a call, `while` answers `False`.
    fn dry_run_default(self) -> bool {
        matches!(self, ConditionKind::If)
    }
}

/// A model-mode `if`'s/`while`'s own outcome -- *generated* is `None`
/// when the template render or the adapter dispatch raised, or in a
/// dry run (no call at all); *verdict* is `Err` for a render failure,
/// an adapter failure or an unparseable reply, carrying that failure's
/// own text -- never raised past this function: the caller (`exec::
/// conditional`/`exec::loop_`) is the one that applies `on_error`,
/// exactly like a CEL condition's own evaluation error.
#[derive(Debug)]
pub struct ModelConditionOutcome {
    pub generated: Option<GenerateResult>,
    pub verdict: Result<bool, String>,
}

/// Renders *template*, dispatches it through *run_ctx*'s own default
/// adapter, and parses the reply as a yes/no answer. `Err` is returned
/// only for a fatal [`VmError`] (a cancellation) -- every other failure
/// (render, dispatch, parse) comes back inside
/// [`ModelConditionOutcome::verdict`] instead, for the caller's own
/// `on_error` handling to decide what to do with.
pub async fn evaluate_model_condition(
    kind: ConditionKind,
    template: &TemplateText,
    ctx: &Value,
    path: &EffectPath,
    run_ctx: &RunContext<'_>,
    token: &CancellationToken,
) -> Result<ModelConditionOutcome, VmError> {
    if run_ctx.dry_run {
        return Ok(ModelConditionOutcome {
            generated: None,
            verdict: Ok(kind.dry_run_default()),
        });
    }

    let rendered = match electricity_template::render_template_with_root(
        &template.source,
        ctx,
        &electricity_template::PlainCtx,
        kind.label(),
    ) {
        Ok(text) => text,
        Err(err) => {
            return Ok(ModelConditionOutcome {
                generated: None,
                verdict: Err(err.to_string()),
            });
        }
    };

    let prompt = format!(
        "Evaluate the following condition and respond with ONLY 'yes' or 'no':\n\n{rendered}\n\n{}",
        kind.prompt_suffix(),
    );

    match dispatch(
        &prompt,
        path,
        run_ctx.adapter_timeout_seconds,
        run_ctx,
        token,
    )
    .await?
    {
        Ok(result) => {
            let verdict = super::prompt::answers::parse_boolean_answer(&result.text);
            Ok(ModelConditionOutcome {
                generated: Some(result),
                verdict,
            })
        }
        Err(err) => Ok(ModelConditionOutcome {
            generated: None,
            verdict: Err(err.message),
        }),
    }
}

/// A model-mode `expect:`'s own outcome -- *meta* is the Python key
/// order [`evaluate_model_expect`]'s own doc comment pins (`mode,
/// template, answer, tokens_sent, tokens_received, [error], result`),
/// what the caller records verbatim at `meta.expect`.
#[derive(Debug)]
pub struct ModelExpectOutcome {
    pub passed: bool,
    pub meta: Value,
}

/// A raised failure out of [`evaluate_model_expect`] -- `core/
/// expect.py::evaluate_expect`'s own render/adapter call sites are
/// *not* wrapped in a `try`, unlike the answer-parse step (which comes
/// back as an ordinary [`ModelExpectOutcome`] instead, never raised):
/// a render or adapter failure propagates to the caller (`core/
/// tool.py`/`core/use.py`), which is what classifies it -- `tool.rs`
/// always retries ([`ExpectCallError::Raised`] turns into an ordinary
/// attempt failure there, `expect_failed: true`), `use_.rs` retries
/// unless *py_class* is a `ValueError`/`RecursionError` (a template
/// render failure's own [`PyExc::TemplateError`] is a `ValueError`
/// subclass, so it counts as one there too).
#[derive(Debug)]
pub enum ExpectCallError {
    Fatal(VmError),
    Raised { message: String, py_class: PyExc },
}

/// `core/expect.py::evaluate_expect`'s own `mode: model` half.
/// *timeout_seconds* is the caller's own choice (issue #513 Scope §7:
/// the tool's own resolved timeout for a `tool` expect, the run
/// adapter budget for a `use` expect) -- this function has no opinion
/// of its own about which. *ctx* is the effect's own live state
/// (`exec::tool`/`exec::use_`'s own `ctx.whole()`); *value*/*meta* are
/// merged into a shallow copy of it under their own keys before
/// rendering, `core/expect.py`'s own `{**ctx, "value": value, "meta":
/// meta}` -- *meta* is the effect's own live `meta` snapshot at this
/// call (`meta.expect` already popped by the caller before the
/// snapshot was taken).
#[allow(clippy::too_many_arguments)]
pub async fn evaluate_model_expect(
    template: &TemplateText,
    value: &Value,
    meta: &Value,
    ctx: &Value,
    path: &EffectPath,
    timeout_seconds: u32,
    run_ctx: &RunContext<'_>,
    token: &CancellationToken,
) -> Result<ModelExpectOutcome, ExpectCallError> {
    let mut render_map: IndexMap<Value, Value> = match ctx {
        Value::Dict(map) => map.clone(),
        _ => IndexMap::new(),
    };
    render_map.insert(Value::from("value"), value.clone());
    render_map.insert(Value::from("meta"), meta.clone());
    let render_ctx = Value::Dict(render_map);

    let rendered = electricity_template::render_template_with_root(
        &template.source,
        &render_ctx,
        &electricity_template::PlainCtx,
        "expect.template",
    )
    .map_err(|err| ExpectCallError::Raised {
        message: err.to_string(),
        py_class: PyExc::TemplateError,
    })?;

    let prompt = format!(
        "Evaluate the following and respond with ONLY 'yes' or 'no':\n\n{rendered}\n\nAnswer (yes/no):"
    );

    let result = dispatch(&prompt, path, timeout_seconds, run_ctx, token)
        .await
        .map_err(ExpectCallError::Fatal)?
        .map_err(|err| ExpectCallError::Raised {
            message: err.message,
            py_class: err.py_class,
        })?;

    let summary = expect_model_summary(&template.source);
    let mut meta_map: IndexMap<Value, Value> = IndexMap::new();
    meta_map.insert(Value::from("mode"), Value::from("model"));
    meta_map.insert(Value::from("template"), Value::from(summary.as_str()));
    meta_map.insert(Value::from("answer"), Value::from(result.text.as_str()));
    meta_map.insert(
        Value::from("tokens_sent"),
        result.tokens_sent.map(Value::from).unwrap_or(Value::None),
    );
    meta_map.insert(
        Value::from("tokens_received"),
        result
            .tokens_received
            .map(Value::from)
            .unwrap_or(Value::None),
    );

    match super::prompt::answers::parse_boolean_answer(&result.text) {
        Ok(passed) => {
            meta_map.insert(Value::from("result"), Value::Bool(passed));
            Ok(ModelExpectOutcome {
                passed,
                meta: Value::Dict(meta_map),
            })
        }
        Err(err) => {
            meta_map.insert(Value::from("error"), Value::from(err.as_str()));
            meta_map.insert(Value::from("result"), Value::Bool(false));
            Ok(ModelExpectOutcome {
                passed: false,
                meta: Value::Dict(meta_map),
            })
        }
    }
}

/// `core/expect.py::expect_failure_summary`'s own model-mode half --
/// shared by `tool.rs`'s and `use_.rs`'s own `expect failed: ...`
/// message, and by [`evaluate_model_expect`]'s own `meta.expect.
/// template`. Whitespace-collapsed under Python's own `str.split()`
/// rule ([`electricity_value::pycompat::is_py_whitespace`], not
/// Rust's `split_whitespace` -- issue #513's settled question 12: the
/// gap is `\x1c`-`\x1f`, which Rust's own `char::is_whitespace` does
/// not count), then capped at 80 *code points* (`collapsed[:80]` is a
/// codepoint slice in Python, matched here with `chars()` rather than
/// a byte slice).
pub(crate) fn expect_model_summary(source: &str) -> String {
    let collapsed = source
        .split(electricity_value::pycompat::is_py_whitespace)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if collapsed.chars().count() <= 80 {
        collapsed
    } else {
        let truncated: String = collapsed.chars().take(80).collect();
        format!("{truncated}...")
    }
}

/// Dispatches *prompt* through *run_ctx*'s own default adapter, with
/// no options and no retries -- `adapter.generate(model=..., prompt=...,
/// timeout_seconds=...)`, called directly, never through `execute_
/// prompt`/`generate_with_fallbacks`: those build attempt chains,
/// options, retries, `fallback_attempts` and limiter slots, none of
/// which exist for a condition or an `expect:` (issue #513's own
/// Interfaces §1). The outer `Err` is a fatal [`VmError`] (a
/// cancellation, checked both before dispatch and raced against it
/// with `biased;`, `exec/prompt.rs`'s own pattern) -- nothing is
/// written on that path. The inner `Result` is the adapter's own
/// [`crate::adapter::AdapterError`], for the caller to turn into
/// whatever its own failure shape is.
async fn dispatch(
    prompt: &str,
    path: &EffectPath,
    timeout_seconds: u32,
    run_ctx: &RunContext<'_>,
    token: &CancellationToken,
) -> Result<Result<GenerateResult, crate::adapter::AdapterError>, VmError> {
    if token.is_set() {
        return Err(VmError::Cancelled);
    }
    let call = CallContext {
        path,
        timeout_seconds,
        token,
    };
    let options = GenerateOptions::default();
    tokio::select! {
        biased;
        () = token.cancelled() => Err(VmError::Cancelled),
        result = run_ctx.default_adapter.generate(
            run_ctx.model,
            prompt,
            &options,
            &call,
        ) => Ok(result),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Limiter;
    use crate::adapter::{Adapter, AdapterError, AdapterRegistry, CheckResult};
    use crate::retry::RetryInfo;
    use async_trait::async_trait;
    use electricity_bytecode::template::Escape;
    use electricity_config::ComplexitySettings;
    use electricity_tools::ToolRegistry;
    use std::cell::RefCell;
    use std::collections::BTreeSet;
    use std::path::Path;
    use std::rc::Rc;
    use std::time::Duration;

    /// Records every call it was given and answers however the test
    /// configured it to -- a reply, an error, or (for the cancellation
    /// test) hangs until its caller is dropped.
    struct FakeAdapter {
        reply: RefCell<Option<Result<GenerateResult, AdapterError>>>,
        pending: bool,
        calls: RefCell<Vec<(String, String, GenerateOptions, u32)>>,
    }

    impl FakeAdapter {
        fn replying(result: Result<GenerateResult, AdapterError>) -> Self {
            FakeAdapter {
                reply: RefCell::new(Some(result)),
                pending: false,
                calls: RefCell::new(Vec::new()),
            }
        }

        fn pending() -> Self {
            FakeAdapter {
                reply: RefCell::new(None),
                pending: true,
                calls: RefCell::new(Vec::new()),
            }
        }
    }

    #[async_trait(?Send)]
    impl Adapter for FakeAdapter {
        fn name(&self) -> &str {
            "fake"
        }

        async fn generate(
            &self,
            model: &str,
            prompt: &str,
            options: &GenerateOptions,
            call: &CallContext<'_>,
        ) -> Result<GenerateResult, AdapterError> {
            self.calls.borrow_mut().push((
                model.to_string(),
                prompt.to_string(),
                options.clone(),
                call.timeout_seconds,
            ));
            if self.pending {
                std::future::pending::<()>().await;
                unreachable!("a pending FakeAdapter never resolves its own generate() call");
            }
            self.reply
                .borrow_mut()
                .take()
                .expect("FakeAdapter::replying called more than once")
        }

        async fn check(&self) -> CheckResult {
            CheckResult {
                ok: true,
                ..Default::default()
            }
        }
    }

    fn reply(text: &str, tokens_sent: i64, tokens_received: i64) -> GenerateResult {
        GenerateResult {
            text: text.to_string(),
            raw: Value::None,
            tokens_sent: Some(tokens_sent),
            tokens_received: Some(tokens_received),
            finish_reason: None,
            warnings: vec!["ignored".to_string()],
            cost_usd: Some(9.0),
        }
    }

    fn plain_template(source: &str) -> TemplateText {
        TemplateText::new(source, false, Escape::Html)
    }

    /// Mirrors `exec::prompt::tests::test_run_ctx` -- built fresh here
    /// since it is a module-private helper there, and this module
    /// needs its own *default_adapter*/*dry_run*/*adapter_timeout_
    /// seconds* control per test.
    #[allow(clippy::too_many_arguments)]
    fn test_run_ctx<'a>(
        registry: &'a ToolRegistry,
        limiter: &'a Limiter,
        runtime_config: &'a Value,
        complexity: &'a ComplexitySettings,
        declared_prompts: &'a IndexMap<String, String>,
        effect_names: &'a BTreeSet<String>,
        adapters: &'a AdapterRegistry,
        default_adapter: Rc<dyn Adapter>,
        dry_run: bool,
    ) -> RunContext<'a> {
        RunContext {
            registry,
            limiter,
            model: "m",
            adapter: "fake",
            runtime_config,
            dry_run,
            armed: false,
            adapters,
            default_adapter,
            model_locked: false,
            adapter_timeout_seconds: 42,
            complexity,
            decomposition_depth: 0,
            orchestration_dir: Path::new("."),
            declared_prompts,
            effect_names,
            supported: electricity_bytecode::Supported::m0(&[]),
        }
    }

    macro_rules! with_run_ctx {
        ($adapter:expr, $dry_run:expr, |$run_ctx:ident| $body:block) => {{
            let registry = ToolRegistry::new();
            let limiter = Limiter::new();
            let runtime_config = Value::None;
            let complexity = ComplexitySettings::default();
            let declared_prompts = IndexMap::new();
            let effect_names = BTreeSet::new();
            let adapters = AdapterRegistry::new();
            let $run_ctx = test_run_ctx(
                &registry,
                &limiter,
                &runtime_config,
                &complexity,
                &declared_prompts,
                &effect_names,
                &adapters,
                $adapter,
                $dry_run,
            );
            $body
        }};
    }

    #[tokio::test]
    async fn if_prompt_bytes_match_the_reference_exactly() {
        let adapter = Rc::new(FakeAdapter::replying(Ok(reply("yes", 1, 2))));
        with_run_ctx!(adapter.clone(), false, |run_ctx| {
            let template = plain_template("go {{x}}?");
            let ctx = Value::Dict(IndexMap::from([(Value::from("x"), Value::from("ahead"))]));
            let token = CancellationToken::new();
            let path = EffectPath::root();
            evaluate_model_condition(ConditionKind::If, &template, &ctx, &path, &run_ctx, &token)
                .await
                .expect("not cancelled");
            let calls = adapter.calls.borrow();
            assert_eq!(calls.len(), 1);
            assert_eq!(
                calls[0].1,
                "Evaluate the following condition and respond with ONLY 'yes' or 'no':\n\n\
                 go ahead?\n\nAnswer (yes/no):"
            );
            assert_eq!(calls[0].2, GenerateOptions::default());
        });
    }

    #[tokio::test]
    async fn while_prompt_bytes_match_the_reference_exactly() {
        let adapter = Rc::new(FakeAdapter::replying(Ok(reply("no", 1, 2))));
        with_run_ctx!(adapter.clone(), false, |run_ctx| {
            let template = plain_template("more?");
            let ctx = Value::Dict(IndexMap::new());
            let token = CancellationToken::new();
            let path = EffectPath::root();
            evaluate_model_condition(
                ConditionKind::While,
                &template,
                &ctx,
                &path,
                &run_ctx,
                &token,
            )
            .await
            .expect("not cancelled");
            let calls = adapter.calls.borrow();
            assert_eq!(
                calls[0].1,
                "Evaluate the following condition and respond with ONLY 'yes' or 'no':\n\n\
                 more?\n\nShould the loop continue? Answer (yes/no):"
            );
        });
    }

    #[tokio::test]
    async fn a_dry_run_if_answers_true_without_a_call() {
        let adapter = Rc::new(FakeAdapter::pending());
        with_run_ctx!(adapter.clone(), true, |run_ctx| {
            let template = plain_template("whatever");
            let ctx = Value::None;
            let token = CancellationToken::new();
            let path = EffectPath::root();
            let outcome = evaluate_model_condition(
                ConditionKind::If,
                &template,
                &ctx,
                &path,
                &run_ctx,
                &token,
            )
            .await
            .expect("not cancelled");
            assert_eq!(outcome.verdict, Ok(true));
            assert!(outcome.generated.is_none());
            assert!(adapter.calls.borrow().is_empty());
        });
    }

    #[tokio::test]
    async fn a_dry_run_while_answers_false_without_a_call() {
        let adapter = Rc::new(FakeAdapter::pending());
        with_run_ctx!(adapter.clone(), true, |run_ctx| {
            let template = plain_template("whatever");
            let ctx = Value::None;
            let token = CancellationToken::new();
            let path = EffectPath::root();
            let outcome = evaluate_model_condition(
                ConditionKind::While,
                &template,
                &ctx,
                &path,
                &run_ctx,
                &token,
            )
            .await
            .expect("not cancelled");
            assert_eq!(outcome.verdict, Ok(false));
            assert!(adapter.calls.borrow().is_empty());
        });
    }

    #[tokio::test]
    async fn a_parse_failure_keeps_the_answer_and_tokens() {
        let adapter = Rc::new(FakeAdapter::replying(Ok(reply("maybe", 3, 4))));
        with_run_ctx!(adapter, false, |run_ctx| {
            let template = plain_template("go?");
            let ctx = Value::None;
            let token = CancellationToken::new();
            let path = EffectPath::root();
            let outcome = evaluate_model_condition(
                ConditionKind::If,
                &template,
                &ctx,
                &path,
                &run_ctx,
                &token,
            )
            .await
            .expect("not cancelled");
            assert_eq!(
                outcome.verdict,
                Err("could not parse a yes/no answer from 'maybe'".to_string())
            );
            let generated = outcome.generated.expect("reply kept on a parse failure");
            assert_eq!(generated.text, "maybe");
            assert_eq!(generated.tokens_sent, Some(3));
            assert_eq!(generated.tokens_received, Some(4));
        });
    }

    #[tokio::test]
    async fn a_render_failure_has_no_answer() {
        let adapter = Rc::new(FakeAdapter::pending());
        with_run_ctx!(adapter.clone(), false, |run_ctx| {
            let template = plain_template("{{#bad");
            let ctx = Value::None;
            let token = CancellationToken::new();
            let path = EffectPath::root();
            let outcome = evaluate_model_condition(
                ConditionKind::If,
                &template,
                &ctx,
                &path,
                &run_ctx,
                &token,
            )
            .await
            .expect("not cancelled");
            assert!(outcome.verdict.is_err());
            assert!(outcome.generated.is_none());
            assert!(adapter.calls.borrow().is_empty());
        });
    }

    #[tokio::test]
    async fn an_adapter_failure_has_no_answer() {
        let adapter = Rc::new(FakeAdapter::replying(Err(AdapterError {
            message: "scripted adapter: server_error".to_string(),
            retry: RetryInfo::retryable(),
            py_class: PyExc::AdapterCallError,
        })));
        with_run_ctx!(adapter, false, |run_ctx| {
            let template = plain_template("go?");
            let ctx = Value::None;
            let token = CancellationToken::new();
            let path = EffectPath::root();
            let outcome = evaluate_model_condition(
                ConditionKind::If,
                &template,
                &ctx,
                &path,
                &run_ctx,
                &token,
            )
            .await
            .expect("not cancelled");
            assert_eq!(
                outcome.verdict,
                Err("scripted adapter: server_error".to_string())
            );
            assert!(outcome.generated.is_none());
        });
    }

    #[tokio::test]
    async fn cancellation_mid_call_returns_cancelled_and_writes_nothing() {
        let adapter = Rc::new(FakeAdapter::pending());
        with_run_ctx!(adapter, false, |run_ctx| {
            let template = plain_template("go?");
            let ctx = Value::None;
            let token = CancellationToken::new();
            let path = EffectPath::root();
            let eval = evaluate_model_condition(
                ConditionKind::If,
                &template,
                &ctx,
                &path,
                &run_ctx,
                &token,
            );
            let cancel = async {
                tokio::time::sleep(Duration::from_millis(5)).await;
                token.request(2);
            };
            let (result, ()) = tokio::join!(eval, cancel);
            assert!(matches!(result, Err(VmError::Cancelled)));
        });
    }

    #[tokio::test]
    async fn an_expect_raised_adapter_failure_carries_its_py_class() {
        let adapter = Rc::new(FakeAdapter::replying(Err(AdapterError {
            message: "unauthorized".to_string(),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::AdapterCallError,
        })));
        with_run_ctx!(adapter, false, |run_ctx| {
            let template = plain_template("ok?");
            let ctx = Value::Dict(IndexMap::new());
            let token = CancellationToken::new();
            let path = EffectPath::root();
            let err = evaluate_model_expect(
                &template,
                &Value::None,
                &Value::None,
                &ctx,
                &path,
                7,
                &run_ctx,
                &token,
            )
            .await
            .expect_err("expected the adapter failure to be raised");
            match err {
                ExpectCallError::Raised { message, py_class } => {
                    assert_eq!(message, "unauthorized");
                    assert_eq!(py_class, PyExc::AdapterCallError);
                }
                ExpectCallError::Fatal(e) => panic!("expected Raised, got Fatal({e})"),
            }
        });
    }

    #[tokio::test]
    async fn an_expect_render_failure_is_raised_as_a_template_error() {
        let adapter = Rc::new(FakeAdapter::pending());
        with_run_ctx!(adapter.clone(), false, |run_ctx| {
            let template = plain_template("{{#bad");
            let ctx = Value::Dict(IndexMap::new());
            let token = CancellationToken::new();
            let path = EffectPath::root();
            let err = evaluate_model_expect(
                &template,
                &Value::None,
                &Value::None,
                &ctx,
                &path,
                7,
                &run_ctx,
                &token,
            )
            .await
            .expect_err("expected the render failure to be raised");
            match err {
                ExpectCallError::Raised { py_class, .. } => {
                    assert_eq!(py_class, PyExc::TemplateError);
                }
                ExpectCallError::Fatal(e) => panic!("expected Raised, got Fatal({e})"),
            }
            assert!(adapter.calls.borrow().is_empty());
        });
    }

    #[tokio::test]
    async fn an_expect_parse_failure_is_not_raised() {
        let adapter = Rc::new(FakeAdapter::replying(Ok(reply("maybe", 1, 1))));
        with_run_ctx!(adapter, false, |run_ctx| {
            let template = plain_template("ok?");
            let ctx = Value::Dict(IndexMap::new());
            let token = CancellationToken::new();
            let path = EffectPath::root();
            let outcome = evaluate_model_expect(
                &template,
                &Value::None,
                &Value::None,
                &ctx,
                &path,
                7,
                &run_ctx,
                &token,
            )
            .await
            .expect("a parse failure is not raised");
            assert!(!outcome.passed);
            let meta = outcome.meta.as_dict().expect("a dict");
            assert_eq!(
                meta.keys().cloned().collect::<Vec<_>>(),
                vec![
                    Value::from("mode"),
                    Value::from("template"),
                    Value::from("answer"),
                    Value::from("tokens_sent"),
                    Value::from("tokens_received"),
                    Value::from("error"),
                    Value::from("result"),
                ]
            );
        });
    }

    #[tokio::test]
    async fn the_tools_own_timeout_is_passed_through_unchanged() {
        let adapter = Rc::new(FakeAdapter::replying(Ok(reply("yes", 1, 1))));
        with_run_ctx!(adapter.clone(), false, |run_ctx| {
            let template = plain_template("ok?");
            let ctx = Value::Dict(IndexMap::new());
            let token = CancellationToken::new();
            let path = EffectPath::root();
            evaluate_model_expect(
                &template,
                &Value::None,
                &Value::None,
                &ctx,
                &path,
                99,
                &run_ctx,
                &token,
            )
            .await
            .expect("not cancelled");
            // 99, the tool's own resolved timeout -- not `run_ctx.
            // adapter_timeout_seconds` (42), which a model `if`/`while`
            // uses instead (issue #513 Scope §7).
            assert_eq!(adapter.calls.borrow()[0].3, 99);
        });
    }

    #[tokio::test]
    async fn a_condition_uses_the_run_adapter_budget_not_a_fixed_number() {
        let adapter = Rc::new(FakeAdapter::replying(Ok(reply("yes", 1, 1))));
        with_run_ctx!(adapter.clone(), false, |run_ctx| {
            let template = plain_template("ok?");
            let ctx = Value::None;
            let token = CancellationToken::new();
            let path = EffectPath::root();
            evaluate_model_condition(ConditionKind::If, &template, &ctx, &path, &run_ctx, &token)
                .await
                .expect("not cancelled");
            assert_eq!(adapter.calls.borrow()[0].3, run_ctx.adapter_timeout_seconds);
        });
    }

    #[test]
    fn expect_model_summary_collapses_python_whitespace_and_caps_at_80() {
        assert_eq!(expect_model_summary("a   b\tc"), "a b c");
        assert_eq!(expect_model_summary("a\u{1c}b"), "a b");
        let long = "x".repeat(81);
        assert_eq!(
            expect_model_summary(&long),
            format!("{}...", "x".repeat(80))
        );
        let exactly_80 = "x".repeat(80);
        assert_eq!(expect_model_summary(&exactly_80), exactly_80);
    }
}
