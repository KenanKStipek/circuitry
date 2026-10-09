//! The model-adapter seam (issue #449's gate lane, item 4) --
//! `adapters/base.py`/`adapters/factory.py`, ported. The trait lives in
//! this crate, not `electricity-adapters`, following DESIGN.md §2's own
//! dependency direction: the VM defines the trait boundary, and the
//! crate that implements concrete adapters (`electricity-adapters`,
//! lane F1/F2) depends on this one, never the reverse.
//!
//! [`Adapter`] itself has no stub implementor in this crate -- every
//! concrete adapter (the Rust `scripted` adapter, `host_claude`'s
//! registration stub, the real providers) lives in `electricity-adapters`.
//! [`AdapterRegistry`] holds *builders* (one per adapter name), not
//! instances, because a non-default adapter is built fresh for every
//! attempt (`adapters/factory.py::build_adapter`'s own per-call
//! semantics, mirrored by [`AdapterRegistry::build`]) -- its own reply
//! queue (the scripted adapter) or per-call state must restart each
//! time, exactly as it would under Python's own `_resolve_adapter`
//! (`core/prompt.py`).

use async_trait::async_trait;
use electricity_bytecode::EffectPath;
use electricity_value::Value;
use std::collections::HashMap;
use std::fmt;

use crate::CancellationToken;
use crate::retry::RetryInfo;

/// `preflight.py::CheckResult`, mirrored here rather than reused from
/// `electricity-tools` -- this crate must not depend on that crate
/// (DESIGN.md §2's dependency direction: tools/adapters depend on vm,
/// never the reverse), so the two small structs stay independent
/// copies of the same Python shape instead of sharing a type.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CheckResult {
    pub ok: bool,
    pub missing: Vec<String>,
    pub message: Option<String>,
}

/// One rendered conversation turn (`adapters/base.py::ChatMessage`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatMessage {
    /// `system | user | assistant | tool`.
    pub role: String,
    pub content: String,
}

/// An image for a vision model (`adapters/base.py::ImageInput`) --
/// exactly one of *data*/*url* is set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageInput {
    pub data: Option<Vec<u8>>,
    pub media_type: Option<String>,
    pub url: Option<String>,
}

/// Per-call generation settings a `prompt` effect hands its adapter
/// (`adapters/base.py::GenerateOptions`, field for field).
#[derive(Debug, Clone, PartialEq)]
pub struct GenerateOptions {
    pub temperature: Option<f64>,
    pub max_tokens: Option<i64>,
    pub stop: Vec<String>,
    /// Every other key of the effect's `params:`, raw -- goes to the
    /// provider unchanged, in whatever slot its own API keeps them.
    pub params: Value,
    pub deterministic: bool,
    pub messages: Vec<ChatMessage>,
    pub images: Vec<ImageInput>,
    pub json_schema: Option<Value>,
}

impl Default for GenerateOptions {
    fn default() -> Self {
        GenerateOptions {
            temperature: None,
            max_tokens: None,
            stop: Vec::new(),
            params: Value::None,
            deterministic: false,
            messages: Vec::new(),
            images: Vec::new(),
            json_schema: None,
        }
    }
}

/// What a successful [`Adapter::generate`] call reports
/// (`adapters/base.py::GenerateResult`, field for field).
#[derive(Debug, Clone, PartialEq)]
pub struct GenerateResult {
    pub text: String,
    pub raw: Value,
    pub tokens_sent: Option<i64>,
    pub tokens_received: Option<i64>,
    pub finish_reason: Option<String>,
    pub warnings: Vec<String>,
    pub cost_usd: Option<f64>,
}

/// What [`Adapter::generate`] needs beyond *model*/*prompt*/*options*
/// (issue #449's gate lane) -- replaces Python's `core.effect_identity.
/// current_call_path` contextvar with an explicit argument, since this
/// VM has no thread-local/contextvar-equivalent of its own. *path* is
/// the effect's own **concretized** [`EffectPath`] (loop passes
/// resolved to their real index, DESIGN.md's own `concretize`) -- the
/// scripted adapter's own reply-queue key (issue #449's gate lane item
/// 4; `adapters/scripted.py`).
pub struct CallContext<'a> {
    pub path: &'a EffectPath,
    pub timeout_seconds: u32,
    pub token: &'a CancellationToken,
}

/// A failed [`Adapter::generate`]/[`Adapter::check`]/registry build --
/// `adapters/_retry.py::AdapterCallError`'s own shape (a message plus a
/// [`RetryInfo`]), widened with a Python exception class name for the
/// same reason [`electricity_tools::PyExc`] exists on the tool side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterError {
    pub message: String,
    pub retry: RetryInfo,
    pub py_class: PyExc,
}

/// Mirrors [`electricity_tools::PyExc`] -- kept as its own copy for the
/// same dependency-direction reason [`CheckResult`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PyExc {
    ValueError,
    RuntimeError,
    KeyError,
    TimeoutError,
    ConnectionError,
}

impl PyExc {
    pub fn name(self) -> &'static str {
        match self {
            PyExc::ValueError => "ValueError",
            PyExc::RuntimeError => "RuntimeError",
            PyExc::KeyError => "KeyError",
            PyExc::TimeoutError => "TimeoutError",
            PyExc::ConnectionError => "ConnectionError",
        }
    }
}

impl fmt::Display for AdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for AdapterError {}

/// `adapters/base.py::Adapter` (the `Protocol`), plus the two optional
/// hooks (`check`/`list_models`) folded directly into the trait --
/// unlike Python's own structural-typing shim (`call_check`/
/// `call_list_models`), every Rust [`Adapter`] implementor provides all
/// three; one that genuinely has nothing useful to report for
/// `check`/`list_models` returns an always-ready [`CheckResult`]/an
/// empty list, matching what Python's own shim falls back to for an
/// adapter missing the hook entirely.
#[async_trait(?Send)]
pub trait Adapter {
    fn name(&self) -> &str;

    async fn generate(
        &self,
        model: &str,
        prompt: &str,
        options: &GenerateOptions,
        call: &CallContext<'_>,
    ) -> Result<GenerateResult, AdapterError>;

    fn check(&self) -> CheckResult;

    /// `adapters.models.call_list_models`'s own fallback -- `[]` for an
    /// adapter that can't list models (most of them; only a handful of
    /// Python adapters implement this hook at all).
    fn list_models(&self) -> Vec<String> {
        Vec::new()
    }
}

/// The default-adapter placeholder for a run with no real adapter
/// configured -- the [`Adapter`] counterpart to the `adapter: &str`
/// `"_noop"` sentinel [`crate::RunContext`] already carries forward
/// from M0-H (that field's own doc comment). [`NoopAdapter::generate`]
/// is never actually reachable in M0 (every `prompt` effect is refused
/// before a run starts), so it reports [`AdapterError`] rather than a
/// fake success, same as every other M1-A stub.
pub struct NoopAdapter;

#[async_trait(?Send)]
impl Adapter for NoopAdapter {
    fn name(&self) -> &str {
        "_noop"
    }

    async fn generate(
        &self,
        _model: &str,
        _prompt: &str,
        _options: &GenerateOptions,
        _call: &CallContext<'_>,
    ) -> Result<GenerateResult, AdapterError> {
        Err(AdapterError {
            message: "_noop adapter: no real adapter is configured for this run".to_string(),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::RuntimeError,
        })
    }

    fn check(&self) -> CheckResult {
        CheckResult {
            ok: true,
            ..Default::default()
        }
    }
}

/// A builder: `runtime.adapters.<name>`'s own config slice in, a fresh
/// [`Adapter`] instance out -- `adapters/factory.py`'s own per-adapter
/// builder callables, registered under their canonical lower-case name.
pub type AdapterBuilder = Box<dyn Fn(&Value) -> Result<Box<dyn Adapter>, AdapterError>>;

/// `adapters/factory.py::ADAPTER_REGISTRY` plus `build_adapter`, ported
/// -- a name -> builder lookup, built fresh per call
/// ([`AdapterRegistry::build`]) rather than holding long-lived
/// instances, so a non-default adapter (`core/prompt.py::_resolve_adapter`)
/// restarts any per-call state (a scripted adapter's reply queue) every
/// attempt, exactly as Python's own `build_adapter` call does.
#[derive(Default)]
pub struct AdapterRegistry {
    builders: HashMap<String, AdapterBuilder>,
}

impl AdapterRegistry {
    pub fn new() -> Self {
        AdapterRegistry::default()
    }

    /// Registers *builder* under *name* (already Circuitry's own
    /// canonical lower-case spelling -- this registry does not
    /// re-normalize a registration the way [`AdapterRegistry::build`]
    /// normalizes a *lookup*).
    pub fn register(&mut self, name: &str, builder: AdapterBuilder) {
        self.builders.insert(name.to_string(), builder);
    }

    /// Every registered name, sorted -- `adapters/factory.py::
    /// _supported_names`'s own `tuple(sorted(ADAPTER_REGISTRY.keys()))`.
    pub fn supported_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.builders.keys().cloned().collect();
        names.sort();
        names
    }

    /// `adapters/factory.py::build_adapter`. *adapter_name* is
    /// normalized (`.strip().lower()`) before lookup; *runtime_config*
    /// is the merged `runtime:` block, from which this reads
    /// `runtime.adapters.<adapter_name>`.
    pub fn build(
        &self,
        adapter_name: &str,
        runtime_config: &Value,
    ) -> Result<Box<dyn Adapter>, AdapterError> {
        let normalized = adapter_name.trim().to_lowercase();
        let builder = self.builders.get(normalized.as_str()).ok_or_else(|| {
            let supported = self.supported_names().join(", ");
            AdapterError {
                message: format!(
                    "Unknown adapter: {}. Supported adapters: {supported}. Check runtime.adapters.<adapter_name> and default_adapter/adapter resolution.",
                    Value::Str(normalized.clone()).py_repr()
                ),
                retry: RetryInfo::not_retryable(),
                py_class: PyExc::ValueError,
            }
        })?;
        let cfg = adapter_config_slice(runtime_config, &normalized);
        builder(&cfg)
    }
}

/// `(runtime or {}).get("adapters") or {}`, then `.get(adapter_name) or
/// {}` -- `runtime.adapters.<adapter_name>`, the same falsy-defaulting
/// convention [`electricity_tools`]'s own plugin config slice uses.
fn adapter_config_slice(runtime_config: &Value, adapter_name: &str) -> Value {
    let adapters = runtime_config
        .as_dict()
        .and_then(|d| d.get(&Value::Str("adapters".to_string())))
        .and_then(Value::as_dict);
    match adapters.and_then(|d| d.get(&Value::Str(adapter_name.to_string()))) {
        Some(Value::Dict(dict)) => Value::Dict(dict.clone()),
        _ => Value::Dict(Default::default()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NotImplementedAdapter;

    #[async_trait(?Send)]
    impl Adapter for NotImplementedAdapter {
        fn name(&self) -> &str {
            "stub"
        }

        async fn generate(
            &self,
            _model: &str,
            _prompt: &str,
            _options: &GenerateOptions,
            _call: &CallContext<'_>,
        ) -> Result<GenerateResult, AdapterError> {
            Err(AdapterError {
                message: "stub adapter: not implemented".to_string(),
                retry: RetryInfo::not_retryable(),
                py_class: PyExc::RuntimeError,
            })
        }

        fn check(&self) -> CheckResult {
            CheckResult {
                ok: true,
                ..Default::default()
            }
        }
    }

    #[test]
    fn an_unregistered_adapter_name_lists_every_supported_name() {
        let mut registry = AdapterRegistry::new();
        registry.register(
            "stub",
            Box::new(|_cfg| Ok(Box::new(NotImplementedAdapter) as Box<dyn Adapter>)),
        );
        let err = match registry.build("bogus", &Value::None) {
            Err(err) => err,
            Ok(_) => panic!("expected an unknown-adapter error"),
        };
        assert_eq!(
            err.message,
            "Unknown adapter: 'bogus'. Supported adapters: stub. \
             Check runtime.adapters.<adapter_name> and default_adapter/adapter resolution."
        );
    }

    #[test]
    fn build_normalizes_whitespace_and_case_before_lookup() {
        let mut registry = AdapterRegistry::new();
        registry.register(
            "stub",
            Box::new(|_cfg| Ok(Box::new(NotImplementedAdapter) as Box<dyn Adapter>)),
        );
        let adapter = registry.build(" STUB ", &Value::None).unwrap();
        assert_eq!(adapter.name(), "stub");
    }

    #[tokio::test]
    async fn a_stub_adapter_reports_not_implemented_rather_than_a_fake_success() {
        let token = CancellationToken::new();
        let path = EffectPath::root();
        let call = CallContext {
            path: &path,
            timeout_seconds: 30,
            token: &token,
        };
        let err = NotImplementedAdapter
            .generate("m", "p", &GenerateOptions::default(), &call)
            .await
            .unwrap_err();
        assert_eq!(err.message, "stub adapter: not implemented");
    }
}
