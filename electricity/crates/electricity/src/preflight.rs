//! Lane R's own preflight driver (issue #495): builds each adapter and
//! tool a raw document references from *config*'s own runtime block
//! (never the merged `effective.runtime` -- a document-level
//! `runtime.adapters.scripted.replies_file` is invisible to preflight
//! but used by the run), and awaits/calls `check()`. The pure half
//! (`classify_preflight_results`, the two formatters,
//! `collect_adapter_usages`) lives in `electricity_config::preflight`,
//! which cannot reach `electricity_vm::adapter::Adapter` without a
//! dependency cycle.
//!
//! [`unbuildable_known_adapter`] is settled ruling 3 (issue #495):
//! a Python-known adapter name this build cannot build at all refuses
//! the whole run with the preview marker, before preflight ever tries
//! to build it -- not an ordinary preflight failure.

use electricity_config::preflight::{self, CheckResult as PreflightCheckResult};
use electricity_value::Value;
use electricity_vm::adapter::{
    Adapter, AdapterError, AdapterRegistry, CallContext, CheckResult, GenerateOptions,
    GenerateResult, PyExc,
};
use futures::FutureExt;
use std::panic::AssertUnwindSafe;

/// Settled ruling 3 (issue #495): the run-default adapter placeholder
/// for a Circuitry-known name (`electricity_vm::adapter_names::
/// ADAPTER_NAMES`) this build has no builder for at all. Installed
/// only when a `use` effect alone makes a run's own `needs_real_
/// adapter` true -- a real `prompt`/model-condition effect that could
/// ever actually call [`Adapter::generate`] is still refused outright
/// in this milestone, so *this* placeholder's own `generate` is never
/// reachable yet either; it exists so that day's `use` effect doesn't
/// have to refuse the whole run just because its own document/config
/// happens to name an adapter this crate can't build *yet*.
pub struct PlaceholderAdapter {
    name: String,
}

impl PlaceholderAdapter {
    pub fn new(name: String) -> Self {
        PlaceholderAdapter { name }
    }
}

#[async_trait::async_trait(?Send)]
impl Adapter for PlaceholderAdapter {
    fn name(&self) -> &str {
        &self.name
    }

    async fn generate(
        &self,
        _model: &str,
        _prompt: &str,
        _options: &GenerateOptions,
        _call: &CallContext<'_>,
    ) -> Result<GenerateResult, AdapterError> {
        Err(AdapterError {
            message: crate::format_config_refusal(&format!("adapter '{}'", self.name)),
            retry: electricity_vm::retry::RetryInfo::not_retryable(),
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

/// One of preflight's own ordered results.
pub type PreflightResult = (String, PreflightCheckResult);

/// `payload.downcast_ref::<&str>()`/`::<String>()`, Rust's own closest
/// equivalent to Python's `str(exc)` for a caught panic -- most panics
/// (`panic!("...")`, `.unwrap()`, an assertion) carry one of these two
/// payload types; anything else reports a fixed placeholder rather
/// than nothing at all.
fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "Box<dyn Any>".to_string()
    }
}

/// Settled ruling 5 (issue #495): a panicking `check()` is caught here,
/// becoming `ok: false` with electricity's own `check() panicked: ...`
/// text -- Python's own `check() raised {Type}: {exc}` names a Python
/// exception class this crate has no equivalent of.
async fn call_adapter_check(adapter: &dyn Adapter) -> PreflightCheckResult {
    match AssertUnwindSafe(adapter.check()).catch_unwind().await {
        Ok(result) => PreflightCheckResult {
            ok: result.ok,
            missing: result.missing,
            message: result.message,
        },
        Err(payload) => PreflightCheckResult {
            ok: false,
            missing: Vec::new(),
            message: Some(format!("check() panicked: {}", panic_message(payload))),
        },
    }
}

/// [`call_adapter_check`]'s own tool-side counterpart --
/// [`electricity_tools::ToolPlugin::check`] is synchronous, so this
/// needs no `catch_unwind` future adapter of its own.
fn call_tool_check(tool: &dyn electricity_tools::ToolPlugin) -> PreflightCheckResult {
    match std::panic::catch_unwind(AssertUnwindSafe(|| tool.check())) {
        Ok(result) => PreflightCheckResult {
            ok: result.ok,
            missing: result.missing,
            message: result.message,
        },
        Err(payload) => PreflightCheckResult {
            ok: false,
            missing: Vec::new(),
            message: Some(format!("check() panicked: {}", panic_message(payload))),
        },
    }
}

/// Settled ruling 3: the first (sorted) adapter name *document*
/// references that is in Circuitry's own full registry
/// (`electricity_vm::adapter_names::ADAPTER_NAMES`) but has no builder
/// in *adapters* -- the whole run is refused for it, named, before
/// preflight is ever run. `None` when every referenced name is either
/// buildable here or not in Circuitry's registry at all (preflight's
/// own job to report that one as an ordinary "unknown adapter"
/// failure).
pub fn unbuildable_known_adapter(document: &Value, adapters: &AdapterRegistry) -> Option<String> {
    let (mut adapter_refs, _tool_refs) = preflight::preflight_refs(document, true);
    adapter_refs.sort();
    let buildable = adapters.supported_names();
    adapter_refs.into_iter().find(|name| {
        let normalized = electricity_value::pycompat::py_strip(name).to_lowercase();
        electricity_vm::adapter_names::ADAPTER_NAMES.contains(&normalized.as_str())
            && !buildable.contains(&normalized)
    })
}

/// [`unbuildable_known_adapter`]'s own tool-side counterpart. Walks
/// the raw document's tool references, *including a disabled one*
/// (unlike the refusal walker, which skips a disabled subtree
/// entirely, issue #518 lane J2's own Scope §4): a document can only
/// ever disable an effect through a profile overlay, so this has to
/// read the raw document, before any overlay exists, to see a tool a
/// profile might disable. Without this, a disabled tool naming a
/// plugin Circuitry knows (`electricity_tools::plugin_names::
/// PLUGIN_NAMES`) but this build has no builder for would reach
/// preflight's own "Unknown plugin" error instead of the preview
/// refusal electricity would otherwise give an *enabled* one -- a
/// silent divergence from `cof run`, which would call that plugin's
/// real `check()`.
pub fn unbuildable_known_tool_plugin(document: &Value) -> Option<String> {
    let (_adapter_refs, mut tool_refs) = preflight::preflight_refs(document, true);
    tool_refs.sort();
    let buildable = electricity_tools::supported_names();
    tool_refs.into_iter().find(|name| {
        let normalized = electricity_value::pycompat::py_strip(name).to_lowercase();
        electricity_tools::plugin_names::PLUGIN_NAMES.contains(&normalized.as_str())
            && !buildable.contains(&normalized)
    })
}

/// `cli/runtime_shim.py::preflight`, narrowed to what this milestone
/// needs (issue #495's "Out of scope"): no `library_ref:`/
/// `runtime_plugin:` preflight (`use: ref:` is always rejected at
/// compile time, DESIGN §14; a config with runtime plugins is already
/// refused before this point), and `skip_adapter_check` is always
/// `false` (electricity has no `RunRequest.adapter`-equivalent
/// injected-adapter path). Callers run [`unbuildable_known_adapter`]
/// first -- this function assumes every referenced adapter name is
/// either genuinely unknown or buildable in *adapters*.
pub async fn run_preflight(
    document: &Value,
    config_runtime: Option<&Value>,
    adapters: &AdapterRegistry,
) -> Vec<PreflightResult> {
    let (mut adapter_refs, mut tool_refs) = preflight::preflight_refs(document, true);
    adapter_refs.sort();
    tool_refs.sort();
    let runtime_value = config_runtime.cloned().unwrap_or(Value::None);

    let mut results = Vec::with_capacity(adapter_refs.len() + tool_refs.len());
    for name in adapter_refs {
        let label = format!("adapter:{name}");
        let result = match adapters.build(&name, &runtime_value) {
            Ok(adapter) => call_adapter_check(adapter.as_ref()).await,
            Err(err) if err.py_class == PyExc::RuntimeError => PreflightCheckResult {
                ok: true,
                missing: Vec::new(),
                message: Some(format!("deferred (runtime-injected): {}", err.message)),
            },
            Err(err) => PreflightCheckResult {
                ok: false,
                missing: Vec::new(),
                message: Some(err.message),
            },
        };
        results.push((label, result));
    }

    for name in tool_refs {
        let label = format!("tool:{name}");
        let result = match electricity_tools::build_plugin(&name, &runtime_value) {
            Ok(tool) => call_tool_check(tool.as_ref()),
            Err(err) => PreflightCheckResult {
                ok: false,
                missing: Vec::new(),
                message: Some(err.message),
            },
        };
        results.push((label, result));
    }

    results
}

/// `cli/runtime_shim.py::image_asset_warnings`. *merged_runtime* is
/// `effective.runtime` (unlike [`run_preflight`]'s own
/// *config_runtime*) -- an adapter actually built for this check must
/// see the same runtime config the run itself would build it with.
pub fn image_asset_warnings(
    document: &Value,
    default_adapter: &str,
    merged_runtime: Option<&Value>,
    adapters: &AdapterRegistry,
) -> Vec<String> {
    let mut warnings = Vec::new();
    let runtime_value = merged_runtime.cloned().unwrap_or(Value::None);
    if let Some(orch) = document.as_dict() {
        let effects = orch.get(&Value::Str("effects".to_string()));
        walk_images(
            effects,
            default_adapter,
            &runtime_value,
            adapters,
            &mut warnings,
        );
    }
    warnings
}

/// `None` (no warning -- an unknown name, left to the checks that
/// report it) for a `ValueError` build failure, `Some(false)` (warn)
/// for a `RuntimeError` one (runtime-injected, relays text only), else
/// `Some(adapter.accepts_images())`.
fn takes_images(name: &str, runtime_value: &Value, adapters: &AdapterRegistry) -> Option<bool> {
    match adapters.build(name, runtime_value) {
        Ok(adapter) => Some(adapter.accepts_images()),
        Err(err) if err.py_class == PyExc::RuntimeError => Some(false),
        Err(_) => None,
    }
}

fn get<'a>(dict: &'a electricity_value::Dict, key: &str) -> Option<&'a Value> {
    dict.get(&Value::Str(key.to_string()))
}

fn walk_images(
    effects: Option<&Value>,
    default_adapter: &str,
    runtime_value: &Value,
    adapters: &AdapterRegistry,
    warnings: &mut Vec<String>,
) {
    let Some(Value::List(items)) = effects else {
        return;
    };
    for effect in items {
        let Some(effect) = effect.as_dict() else {
            continue;
        };
        let etype = get(effect, "type").and_then(Value::as_str).unwrap_or("");
        match etype {
            "prompt" => warn_for_prompt(effect, default_adapter, runtime_value, adapters, warnings),
            "dynamic" | "reflector" => {
                walk_images(
                    get(effect, "effects"),
                    default_adapter,
                    runtime_value,
                    adapters,
                    warnings,
                );
            }
            "if" | "conditional" => {
                walk_images(
                    get(effect, "then"),
                    default_adapter,
                    runtime_value,
                    adapters,
                    warnings,
                );
                walk_images(
                    get(effect, "else"),
                    default_adapter,
                    runtime_value,
                    adapters,
                    warnings,
                );
            }
            "loop" => {
                walk_images(
                    get(effect, "body"),
                    default_adapter,
                    runtime_value,
                    adapters,
                    warnings,
                );
            }
            _ => {}
        }
    }
}

fn has_image_asset(effect: &electricity_value::Dict) -> bool {
    get(effect, "assets")
        .and_then(Value::as_list)
        .is_some_and(|assets| {
            assets.iter().any(|a| {
                a.as_dict()
                    .and_then(|d| get(d, "kind"))
                    .and_then(Value::as_str)
                    == Some("image")
            })
        })
}

fn warn_for_prompt(
    effect: &electricity_value::Dict,
    default_adapter: &str,
    runtime_value: &Value,
    adapters: &AdapterRegistry,
    warnings: &mut Vec<String>,
) {
    if !has_image_asset(effect) {
        return;
    }
    let primary = get(effect, "provider").and_then(Value::as_str);
    let primary_name = primary
        .and_then(preflight::provider_token_to_adapter)
        .unwrap_or_else(|| default_adapter.to_string());
    let mut names = vec![primary_name];
    if let Some(Value::List(fallbacks)) = get(effect, "provider_fallbacks") {
        for token in fallbacks {
            if let Some(token) = token.as_str() {
                if let Some(name) = preflight::provider_token_to_adapter(token) {
                    names.push(name);
                }
            }
        }
    }
    let name_display = get(effect, "name")
        .map(Value::py_str)
        .unwrap_or_else(|| "None".to_string());
    let mut seen = std::collections::HashSet::new();
    for name in names {
        if name.is_empty() || !seen.insert(name.clone()) {
            continue;
        }
        if takes_images(&name, runtime_value, adapters) == Some(false) {
            warnings.push(format!(
                "prompt '{name_display}' has image assets, but adapter '{name}' cannot send \
                 images; it will run without them"
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::Dict;

    fn doc_with_adapter(adapter: &str, effects: Vec<Value>) -> Value {
        let mut dict = Dict::new();
        dict.insert(Value::Str("adapter".to_string()), Value::from(adapter));
        dict.insert(Value::Str("effects".to_string()), Value::List(effects));
        Value::Dict(dict)
    }

    #[tokio::test]
    async fn results_are_ordered_adapters_then_tools_each_sorted() {
        let document = doc_with_adapter("scripted", vec![]);
        let mut dict = document.as_dict().unwrap().clone();
        dict.insert(
            Value::Str("effects".to_string()),
            Value::List(vec![tool_effect("awk"), tool_effect("json")]),
        );
        let document = Value::Dict(dict);
        let adapters = electricity_adapters::build_registry();
        let results = run_preflight(&document, None, &adapters).await;
        let labels: Vec<&str> = results.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(labels, vec!["adapter:scripted", "tool:awk", "tool:json"]);
    }

    fn tool_effect(provider: &str) -> Value {
        let mut dict = Dict::new();
        dict.insert(Value::Str("type".to_string()), Value::from("tool"));
        dict.insert(Value::Str("provider".to_string()), Value::from(provider));
        Value::Dict(dict)
    }

    #[tokio::test]
    async fn config_runtime_not_the_merged_runtime_is_used() {
        let document = doc_with_adapter("scripted", vec![]);
        let mut adapters_cfg = Dict::new();
        let mut scripted_cfg = Dict::new();
        scripted_cfg.insert(
            Value::Str("replies_file".to_string()),
            Value::from("/does/not/exist/replies.yaml"),
        );
        adapters_cfg.insert(
            Value::Str("scripted".to_string()),
            Value::Dict(scripted_cfg),
        );
        let mut runtime = Dict::new();
        runtime.insert(
            Value::Str("adapters".to_string()),
            Value::Dict(adapters_cfg),
        );
        let adapters = electricity_adapters::build_registry();
        let results = run_preflight(&document, Some(&Value::Dict(runtime)), &adapters).await;
        assert_eq!(results.len(), 1);
        assert!(!results[0].1.ok);
        assert!(
            results[0]
                .1
                .message
                .as_deref()
                .unwrap()
                .contains("/does/not/exist/replies.yaml")
        );
    }

    #[tokio::test]
    async fn host_claude_is_reported_as_deferred() {
        let document = doc_with_adapter("host_claude", vec![]);
        let adapters = electricity_adapters::build_registry();
        let results = run_preflight(&document, None, &adapters).await;
        assert_eq!(results.len(), 1);
        assert!(results[0].1.ok);
        assert!(
            results[0]
                .1
                .message
                .as_deref()
                .unwrap()
                .starts_with("deferred (runtime-injected): ")
        );
    }

    #[tokio::test]
    async fn an_unknown_adapter_reports_the_factory_message() {
        let document = doc_with_adapter("bogus", vec![]);
        let adapters = electricity_adapters::build_registry();
        let results = run_preflight(&document, None, &adapters).await;
        assert_eq!(results.len(), 1);
        assert!(!results[0].1.ok);
        assert!(
            results[0]
                .1
                .message
                .as_deref()
                .unwrap()
                .starts_with("Unknown adapter: 'bogus'.")
        );
    }

    #[tokio::test]
    async fn a_tool_build_error_is_reported() {
        let mut dict = Dict::new();
        dict.insert(
            Value::Str("effects".to_string()),
            Value::List(vec![tool_effect("bogus")]),
        );
        let document = Value::Dict(dict);
        let adapters = electricity_adapters::build_registry();
        let results = run_preflight(&document, None, &adapters).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "tool:bogus");
        assert!(!results[0].1.ok);
    }

    #[test]
    fn unbuildable_known_adapter_names_a_python_known_rust_unbuilt_adapter() {
        // `openai` became buildable in lane F2 (issue #503); `litellm`
        // (a Python-SDK-only adapter, out of scope for F2) stays the
        // unbuilt example.
        let document = doc_with_adapter("litellm", vec![]);
        let adapters = electricity_adapters::build_registry();
        assert_eq!(
            unbuildable_known_adapter(&document, &adapters),
            Some("litellm".to_string())
        );
    }

    #[test]
    fn unbuildable_known_adapter_is_none_for_a_buildable_name() {
        let document = doc_with_adapter("scripted", vec![]);
        let adapters = electricity_adapters::build_registry();
        assert_eq!(unbuildable_known_adapter(&document, &adapters), None);
    }

    fn doc_with_effects(effects: Vec<Value>) -> Value {
        let mut dict = Dict::new();
        dict.insert(Value::Str("effects".to_string()), Value::List(effects));
        Value::Dict(dict)
    }

    #[test]
    fn unbuildable_known_tool_plugin_names_a_python_known_rust_unbuilt_plugin() {
        let document = doc_with_effects(vec![tool_effect("comfyui")]);
        assert_eq!(
            unbuildable_known_tool_plugin(&document),
            Some("comfyui".to_string())
        );
    }

    #[test]
    fn unbuildable_known_tool_plugin_is_none_for_a_buildable_name() {
        let document = doc_with_effects(vec![tool_effect("json")]);
        assert_eq!(unbuildable_known_tool_plugin(&document), None);
    }

    #[test]
    fn unbuildable_known_tool_plugin_is_none_for_a_genuinely_unknown_name() {
        let document = doc_with_effects(vec![tool_effect("bogus")]);
        assert_eq!(unbuildable_known_tool_plugin(&document), None);
    }

    #[test]
    fn unbuildable_known_adapter_is_none_for_a_genuinely_unknown_name() {
        let document = doc_with_adapter("bogus", vec![]);
        let adapters = electricity_adapters::build_registry();
        assert_eq!(unbuildable_known_adapter(&document, &adapters), None);
    }

    /// Not reachable from a real run yet (a `use` effect is the only
    /// way to install [`PlaceholderAdapter`], and nothing in a `use`
    /// child can call [`Adapter::generate`] in this milestone either)
    /// -- exercised directly so its message text stays pinned to
    /// `format_config_refusal`'s own phrasing (PR #497 review finding
    /// 5), `{VERSION}` included, not a hand-written duplicate of it.
    #[tokio::test]
    async fn placeholder_adapter_generate_uses_format_config_refusals_own_phrasing() {
        let path = electricity_bytecode::EffectPath::root().push_name("p");
        let token = electricity_value::CancellationToken::new();
        let call = CallContext {
            path: &path,
            timeout_seconds: 30,
            token: &token,
        };
        let adapter = PlaceholderAdapter::new("openai".to_string());
        let err = adapter
            .generate("gpt", "hi", &GenerateOptions::default(), &call)
            .await
            .unwrap_err();
        assert_eq!(
            err.message,
            crate::format_config_refusal("adapter 'openai'")
        );
        assert!(err.message.contains(crate::VERSION));
    }

    fn prompt_with_image(name: &str, provider: &str) -> Value {
        let mut asset = Dict::new();
        asset.insert(Value::Str("kind".to_string()), Value::from("image"));
        let mut dict = Dict::new();
        dict.insert(Value::Str("type".to_string()), Value::from("prompt"));
        dict.insert(Value::Str("name".to_string()), Value::from(name));
        dict.insert(Value::Str("provider".to_string()), Value::from(provider));
        dict.insert(
            Value::Str("assets".to_string()),
            Value::List(vec![Value::Dict(asset)]),
        );
        Value::Dict(dict)
    }

    #[test]
    fn host_claude_warns_because_it_relays_text_only() {
        let document = doc_with_adapter("scripted", vec![prompt_with_image("p", "host_claude")]);
        let adapters = electricity_adapters::build_registry();
        let warnings = image_asset_warnings(&document, "scripted", None, &adapters);
        assert_eq!(
            warnings,
            vec![
                "prompt 'p' has image assets, but adapter 'host_claude' cannot send images; it will run without them"
                    .to_string()
            ]
        );
    }

    #[test]
    fn an_unknown_adapter_name_never_warns() {
        let document = doc_with_adapter("scripted", vec![prompt_with_image("p", "bogus")]);
        let adapters = electricity_adapters::build_registry();
        assert!(image_asset_warnings(&document, "scripted", None, &adapters).is_empty());
    }

    #[test]
    fn an_unnamed_prompt_renders_as_the_literal_none() {
        let mut asset = Dict::new();
        asset.insert(Value::Str("kind".to_string()), Value::from("image"));
        let mut dict = Dict::new();
        dict.insert(Value::Str("type".to_string()), Value::from("prompt"));
        dict.insert(
            Value::Str("provider".to_string()),
            Value::from("host_claude"),
        );
        dict.insert(
            Value::Str("assets".to_string()),
            Value::List(vec![Value::Dict(asset)]),
        );
        let document = doc_with_adapter("scripted", vec![Value::Dict(dict)]);
        let adapters = electricity_adapters::build_registry();
        let warnings = image_asset_warnings(&document, "scripted", None, &adapters);
        assert_eq!(
            warnings,
            vec![
                "prompt 'None' has image assets, but adapter 'host_claude' cannot send images; it will run without them"
                    .to_string()
            ]
        );
    }

    #[test]
    fn duplicate_adapter_names_across_primary_and_fallback_warn_once() {
        let mut asset = Dict::new();
        asset.insert(Value::Str("kind".to_string()), Value::from("image"));
        let mut dict = Dict::new();
        dict.insert(Value::Str("type".to_string()), Value::from("prompt"));
        dict.insert(Value::Str("name".to_string()), Value::from("p"));
        dict.insert(
            Value::Str("provider".to_string()),
            Value::from("host_claude"),
        );
        dict.insert(
            Value::Str("provider_fallbacks".to_string()),
            Value::List(vec![Value::from("host_claude")]),
        );
        dict.insert(
            Value::Str("assets".to_string()),
            Value::List(vec![Value::Dict(asset)]),
        );
        let document = doc_with_adapter("scripted", vec![Value::Dict(dict)]);
        let adapters = electricity_adapters::build_registry();
        let warnings = image_asset_warnings(&document, "scripted", None, &adapters);
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn no_image_assets_means_no_warning() {
        let mut dict = Dict::new();
        dict.insert(Value::Str("type".to_string()), Value::from("prompt"));
        dict.insert(Value::Str("name".to_string()), Value::from("p"));
        dict.insert(
            Value::Str("provider".to_string()),
            Value::from("host_claude"),
        );
        let document = doc_with_adapter("scripted", vec![Value::Dict(dict)]);
        let adapters = electricity_adapters::build_registry();
        assert!(image_asset_warnings(&document, "scripted", None, &adapters).is_empty());
    }

    #[test]
    fn the_walk_descends_into_dynamic_if_and_loop() {
        for wrapper_type in ["dynamic", "if", "loop", "reflector"] {
            let inner = prompt_with_image("p", "host_claude");
            let mut wrapper = Dict::new();
            wrapper.insert(Value::Str("type".to_string()), Value::from(wrapper_type));
            let key = match wrapper_type {
                "if" => "then",
                "loop" => "body",
                _ => "effects",
            };
            wrapper.insert(Value::Str(key.to_string()), Value::List(vec![inner]));
            let document = doc_with_adapter("scripted", vec![Value::Dict(wrapper)]);
            let adapters = electricity_adapters::build_registry();
            let warnings = image_asset_warnings(&document, "scripted", None, &adapters);
            assert_eq!(
                warnings.len(),
                1,
                "wrapper type {wrapper_type} was not walked"
            );
        }
    }
}
