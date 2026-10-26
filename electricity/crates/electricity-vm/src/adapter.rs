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

/// The one canonical Python-exception-class enum
/// (`electricity_value::pycompat::PyExc`'s own doc comment) -- this
/// module used to keep its own 5-variant copy, justified by a
/// dependency-direction rule that doesn't actually hold here: this
/// crate (`electricity-vm`) already depends on `electricity-tools`
/// (PR #454).
pub use electricity_tools::PyExc;

impl fmt::Display for AdapterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for AdapterError {}

/// `adapters/base.py::Adapter` (the `Protocol`), plus the three
/// optional hooks (`check`/`list_models`/`accepts_images`) folded
/// directly into the trait -- unlike Python's own structural-typing
/// shim (`call_check`/`call_list_models`), every Rust [`Adapter`]
/// implementor provides all of them; one that genuinely has nothing
/// useful to report for `check`/`list_models` returns an always-ready
/// [`CheckResult`]/an empty list, matching what Python's own shim
/// falls back to for an adapter missing the hook entirely.
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

    /// `preflight.py`'s own per-adapter readiness probe -- `async`
    /// (PR #454: `ollama.py`'s own `check()`
    /// probes the daemon over the network, `adapters/ollama.py:220-`,
    /// so a synchronous signature here would have forced lane F2 to
    /// widen this trait's own "final and complete" method once it
    /// ported that adapter).
    async fn check(&self) -> CheckResult;

    /// `adapters.models.call_list_models`'s own fallback -- `[]` for an
    /// adapter that can't list models (most of them; only a handful of
    /// Python adapters implement this hook at all).
    fn list_models(&self) -> Vec<String> {
        Vec::new()
    }

    /// Whether this adapter accepts image inputs at all -- lane R's own
    /// `image_asset_warnings` (`runtime_shim.py:1610-1640`) warns once
    /// per run when a prompt attaches
    /// an image to an adapter that can't use it. `false` for every
    /// adapter that doesn't override this -- the common case; only a
    /// handful of Python adapters accept images at all.
    fn accepts_images(&self) -> bool {
        false
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

    async fn check(&self) -> CheckResult {
        CheckResult {
            ok: true,
            ..Default::default()
        }
    }
}

/// A builder: the name the built instance should report (its own
/// registry name, or an alias's) and `runtime.adapters.<name>`'s own
/// config slice in, a fresh [`Adapter`] instance out --
/// `adapters/factory.py`'s own per-adapter builder callables,
/// registered under their canonical lower-case name. Every builder
/// takes a name now (#656: any registered adapter except
/// `host_claude` can be an alias's target), not only `claude_code`/
/// `pi`'s own.
pub type AdapterBuilder = Box<dyn Fn(&str, &Value) -> Result<Box<dyn Adapter>, AdapterError>>;

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

/// `runtime.adapters.<name>` as a dict, when it is one.
fn adapter_entry<'a>(runtime_config: &'a Value, name: &str) -> Option<&'a electricity_value::Dict> {
    runtime_config
        .as_dict()?
        .get(&Value::Str("adapters".to_string()))?
        .as_dict()?
        .get(&Value::Str(name.to_string()))?
        .as_dict()
}

fn sets_adapter(entry: &electricity_value::Dict) -> bool {
    entry.contains_key(&Value::Str("adapter".to_string()))
}

fn value_error(message: String) -> AdapterError {
    AdapterError {
        message,
        retry: RetryInfo::not_retryable(),
        py_class: PyExc::ValueError,
    }
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

    /// `adapters/factory.py::alias_names` for one name: whether
    /// `runtime.adapters.<name>` sets `adapter`, valid alias or not.
    pub fn is_alias(&self, name: &str, runtime_config: &Value) -> bool {
        adapter_entry(runtime_config, name).is_some_and(sets_adapter)
    }

    /// `adapters/factory.py::alias_config_errors` -- why each alias in
    /// *runtime_config* does not build, in name order; `[]` when all
    /// do. Every alias in the merged runtime is checked up front here,
    /// whether or not any effect actually selects it (#656).
    pub fn alias_config_errors(&self, runtime_config: &Value) -> Vec<String> {
        let mut names = alias_names(runtime_config);
        names.sort();
        names
            .into_iter()
            .filter_map(|name| match self.build(&name, runtime_config) {
                Err(e) if e.py_class == PyExc::ValueError => Some(e.message),
                _ => None,
            })
            .collect()
    }

    /// Every name *this registry* actually has a builder for, sorted --
    /// what this crate (not Circuitry's own full registry,
    /// [`crate::adapter_names::ADAPTER_NAMES`]) can build right now.
    /// Not used by [`AdapterRegistry::build`]'s own unknown-adapter
    /// message (PR #454); kept for a caller that
    /// wants to know what's actually buildable, e.g. a `--list-adapters`
    /// CLI command.
    pub fn supported_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.builders.keys().cloned().collect();
        names.sort();
        names
    }

    /// `adapters/factory.py::build_adapter`. *adapter_name* is
    /// normalized (`.strip().lower()`) before lookup; *runtime_config*
    /// is the merged `runtime:` block, from which this reads
    /// `runtime.adapters.<adapter_name>`.
    ///
    /// The unknown-adapter message's own "Supported adapters" list is
    /// *not* this registry's own key set -- it is
    /// [`crate::adapter_names::ADAPTER_NAMES`], Circuitry's **full**
    /// registry (PR #454: `supported_names` alone
    /// only ever lists what this one Rust crate has built so far,
    /// empty through M1-A -- not what `build_adapter`'s own message
    /// would say for the same *adapter_name*).
    pub fn build(
        &self,
        adapter_name: &str,
        runtime_config: &Value,
    ) -> Result<Box<dyn Adapter>, AdapterError> {
        let normalized = electricity_value::pycompat::py_strip(adapter_name).to_lowercase();
        if self.is_alias(&normalized, runtime_config) {
            return self.build_alias(&normalized, runtime_config);
        }
        let builder = self.builders.get(normalized.as_str()).ok_or_else(|| {
            let supported = crate::adapter_names::ADAPTER_NAMES.join(", ");
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
        builder(&normalized, &cfg)
    }
}

impl AdapterRegistry {
    /// `adapters/factory.py::_build_alias`: the target adapter built
    /// from the alias's own settings, under the alias's name. The
    /// target's own `runtime.adapters` entry is not consulted. Any
    /// registered adapter can be a target except `host_claude` (#656).
    fn build_alias(
        &self,
        name: &str,
        runtime_config: &Value,
    ) -> Result<Box<dyn Adapter>, AdapterError> {
        if crate::adapter_names::ADAPTER_NAMES.contains(&name) {
            return Err(value_error(format!(
                "runtime.adapters.{name}.adapter is set, but {} is a built-in adapter; \
                 an alias needs a name of its own",
                Value::Str(name.to_string()).py_repr()
            )));
        }
        let entry = adapter_entry(runtime_config, name).expect("is_alias found this entry");
        let raw_target = entry
            .get(&Value::Str("adapter".to_string()))
            .expect("is_alias found the adapter key");
        let target = match raw_target {
            Value::Str(text) => Some(electricity_value::pycompat::py_strip(text).to_lowercase()),
            _ => None,
        };
        let builder = target
            .as_deref()
            .and_then(|target| self.builders.get(target));
        let Some(builder) = builder else {
            let itself_alias = target.as_deref().is_some_and(|target| {
                !crate::adapter_names::ADAPTER_NAMES.contains(&target)
                    && adapter_entry(runtime_config, target).is_some_and(sets_adapter)
            });
            return Err(value_error(format!(
                "runtime.adapters.{name}.adapter must name a built-in adapter, got {}{}",
                raw_target.py_repr(),
                if itself_alias {
                    ", which is itself an alias"
                } else {
                    ""
                }
            )));
        };
        if target.as_deref() == Some("host_claude") {
            return Err(value_error(format!(
                "runtime.adapters.{name}.adapter cannot be 'host_claude': it is supplied \
                 at run time by circuitry-mcp, never built from config"
            )));
        }
        let mut settings = entry.clone();
        settings.shift_remove(&Value::Str("adapter".to_string()));
        builder(name, &Value::Dict(settings))
    }
}

/// The normalised `adapter` target `runtime.adapters.<name>` names, when
/// *name* is an alias (`adapters/factory.py::build_adapter`'s own alias
/// branch, the target half) -- `None` when *name* isn't an alias at all,
/// or its `adapter` key isn't a string. Used to resolve alias -> target
/// before every "known but unbuildable" check (#656): a used alias of
/// `litellm` must get the same refusal a direct `provider: litellm`
/// gets, naming `litellm`, not the alias -- whether or not the target
/// itself is a valid one (an invalid target is `_build_alias`'s own
/// config-error to report, not this function's).
pub fn alias_target(name: &str, runtime_config: &Value) -> Option<String> {
    let entry = adapter_entry(runtime_config, name)?;
    match entry.get(&Value::Str("adapter".to_string()))? {
        Value::Str(text) => Some(electricity_value::pycompat::py_strip(text).to_lowercase()),
        _ => None,
    }
}

/// `adapters/factory.py::alias_names` -- every `runtime.adapters` entry
/// that sets `adapter`, whether or not it is a valid alias (building it
/// reports what is wrong).
pub fn alias_names(runtime_config: &Value) -> Vec<String> {
    let Some(adapters) = runtime_config
        .as_dict()
        .and_then(|d| d.get(&Value::Str("adapters".to_string())))
        .and_then(Value::as_dict)
    else {
        return Vec::new();
    };
    adapters
        .iter()
        .filter_map(|(name, entry)| {
            let name = name.as_str()?;
            let entry = entry.as_dict()?;
            sets_adapter(entry).then(|| name.to_string())
        })
        .collect()
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

        async fn check(&self) -> CheckResult {
            CheckResult {
                ok: true,
                ..Default::default()
            }
        }
    }

    #[test]
    fn an_unregistered_adapter_name_lists_pythons_full_registry() {
        // PR #454: the message names Circuitry's
        // full registry (`adapter_names::ADAPTER_NAMES`), not this
        // registry's own (possibly empty, as it is through M1-A) set
        // of registered builders.
        let registry = AdapterRegistry::new();
        let err = match registry.build("bogus", &Value::None) {
            Err(err) => err,
            Ok(_) => panic!("expected an unknown-adapter error"),
        };
        assert!(
            err.message
                .starts_with("Unknown adapter: 'bogus'. Supported adapters: ")
        );
        assert!(err.message.contains("ollama"));
        assert!(err.message.contains("anthropic"));
        assert!(err.message.ends_with(
            "Check runtime.adapters.<adapter_name> and default_adapter/adapter resolution."
        ));
    }

    #[test]
    fn an_adapter_circuitry_supports_but_this_crate_cannot_build_yet_is_still_unknown_here() {
        // `ollama` is in Circuitry's own registry (and so in the
        // "Supported adapters" list the message above names), but this
        // crate has no builder for it yet (lane F2).
        let registry = AdapterRegistry::new();
        let err = match registry.build("ollama", &Value::None) {
            Err(err) => err,
            Ok(_) => panic!("expected an unknown-adapter error"),
        };
        assert!(err.message.starts_with("Unknown adapter: 'ollama'."));
    }

    #[test]
    fn build_normalizes_whitespace_and_case_before_lookup() {
        let mut registry = AdapterRegistry::new();
        registry.register(
            "stub",
            Box::new(|_name, _cfg| Ok(Box::new(NotImplementedAdapter) as Box<dyn Adapter>)),
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

    #[tokio::test]
    async fn check_is_async_and_accepts_images_defaults_to_false() {
        // PR #454: `check` is `async` (an adapter
        // like `ollama`'s own probes the daemon over the network), and
        // every adapter that doesn't override `accepts_images` reports
        // `false`.
        assert!(NoopAdapter.check().await.ok);
        assert!(!NoopAdapter.accepts_images());
        assert!(NotImplementedAdapter.check().await.ok);
        assert!(!NotImplementedAdapter.accepts_images());
    }

    struct NamedStub(String);

    #[async_trait(?Send)]
    impl Adapter for NamedStub {
        fn name(&self) -> &str {
            &self.0
        }

        async fn generate(
            &self,
            _model: &str,
            _prompt: &str,
            _options: &GenerateOptions,
            _call: &CallContext<'_>,
        ) -> Result<GenerateResult, AdapterError> {
            unreachable!("never generated")
        }

        async fn check(&self) -> CheckResult {
            CheckResult::default()
        }
    }

    fn alias_registry() -> AdapterRegistry {
        let mut registry = AdapterRegistry::new();
        for base in ["claude_code", "pi", "openai", "host_claude"] {
            registry.register(
                base,
                Box::new(|name, cfg| {
                    let seen = cfg
                        .as_dict()
                        .map(|d| {
                            d.keys()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join(",")
                        })
                        .unwrap_or_default();
                    Ok(Box::new(NamedStub(format!("{name}[{seen}]"))) as Box<dyn Adapter>)
                }),
            );
        }
        registry
    }

    fn runtime(adapters: Value) -> Value {
        let mut d = electricity_value::Dict::new();
        d.insert(Value::Str("adapters".to_string()), adapters);
        Value::Dict(d)
    }

    fn entry(pairs: &[(&str, Value)]) -> Value {
        let mut d = electricity_value::Dict::new();
        for (k, v) in pairs {
            d.insert(Value::Str((*k).to_string()), v.clone());
        }
        Value::Dict(d)
    }

    fn build_err(registry: &AdapterRegistry, name: &str, runtime: &Value) -> String {
        match registry.build(name, runtime) {
            Err(err) => {
                assert_eq!(err.py_class, PyExc::ValueError);
                err.message
            }
            Ok(_) => panic!("expected a config error"),
        }
    }

    #[test]
    fn an_alias_builds_its_target_from_its_own_settings_under_its_own_name() {
        let registry = alias_registry();
        let rt = runtime(entry(&[
            (
                "claude_code",
                entry(&[("binary", Value::Str("base".to_string()))]),
            ),
            (
                "claude_alt",
                entry(&[
                    ("adapter", Value::Str(" Claude_Code ".to_string())),
                    ("binary", Value::Str("alt".to_string())),
                ]),
            ),
        ]));
        let alias = registry.build("claude_alt", &rt).unwrap();
        // the `adapter` key is consumed; the alias's own settings pass on,
        // the target's own block untouched.
        assert_eq!(alias.name(), "claude_alt[binary]");
        assert_eq!(
            registry.build("claude_code", &rt).unwrap().name(),
            "claude_code[binary]"
        );
        assert!(registry.is_alias("claude_alt", &rt));
        assert!(!registry.is_alias("claude_code", &rt));
    }

    #[test]
    fn an_alias_can_target_any_registered_adapter_except_host_claude() {
        let registry = alias_registry();
        let rt = runtime(entry(&[(
            "openai_alt",
            entry(&[("adapter", Value::Str("openai".to_string()))]),
        )]));
        assert_eq!(
            registry.build("openai_alt", &rt).unwrap().name(),
            "openai_alt[]"
        );
    }

    #[test]
    fn alias_target_resolves_an_aliass_declared_target_whether_or_not_it_is_valid() {
        let rt = runtime(entry(&[
            (
                "a",
                entry(&[("adapter", Value::Str(" LiteLLM ".to_string()))]),
            ),
            ("b", entry(&[("binary", Value::Str("x".to_string()))])),
        ]));
        assert_eq!(alias_target("a", &rt), Some("litellm".to_string()));
        assert_eq!(alias_target("b", &rt), None);
        assert_eq!(alias_target("nonexistent", &rt), None);
    }

    #[test]
    fn an_entry_without_adapter_is_not_an_alias() {
        let registry = alias_registry();
        let rt = runtime(entry(&[(
            "claude_alt",
            entry(&[("binary", Value::Str("x".to_string()))]),
        )]));
        assert!(
            registry
                .build("claude_alt", &rt)
                .err()
                .unwrap()
                .message
                .starts_with("Unknown adapter: 'claude_alt'.")
        );
    }

    #[test]
    fn an_alias_with_a_bad_target_is_a_config_error_naming_it() {
        let registry = alias_registry();
        let rt = runtime(entry(&[
            (
                "a",
                entry(&[("adapter", Value::Str("nonesuch".to_string()))]),
            ),
            ("c", entry(&[("adapter", Value::from(5i64))])),
            ("d", entry(&[("adapter", Value::Str("e".to_string()))])),
            ("e", entry(&[("adapter", Value::Str("pi".to_string()))])),
            (
                "h",
                entry(&[("adapter", Value::Str("host_claude".to_string()))]),
            ),
        ]));
        assert_eq!(
            build_err(&registry, "a", &rt),
            "runtime.adapters.a.adapter must name a built-in adapter, got 'nonesuch'"
        );
        assert_eq!(
            build_err(&registry, "c", &rt),
            "runtime.adapters.c.adapter must name a built-in adapter, got 5"
        );
        assert_eq!(
            build_err(&registry, "d", &rt),
            "runtime.adapters.d.adapter must name a built-in adapter, got 'e', which is itself an alias"
        );
        assert_eq!(
            build_err(&registry, "h", &rt),
            "runtime.adapters.h.adapter cannot be 'host_claude': it is supplied at run time by \
             circuitry-mcp, never built from config"
        );
    }

    #[test]
    fn an_alias_named_like_a_built_in_adapter_is_a_config_error() {
        let registry = alias_registry();
        let rt = runtime(entry(&[
            (
                "pi",
                entry(&[("adapter", Value::Str("claude_code".to_string()))]),
            ),
            (
                "openai",
                entry(&[("adapter", Value::Str("pi".to_string()))]),
            ),
        ]));
        for name in ["pi", "openai"] {
            assert_eq!(
                build_err(&registry, name, &rt),
                format!(
                    "runtime.adapters.{name}.adapter is set, but '{name}' is a built-in adapter; \
                     an alias needs a name of its own"
                )
            );
        }
    }
}
