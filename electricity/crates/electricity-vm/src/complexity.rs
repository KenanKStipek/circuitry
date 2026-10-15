//! Wiring complexity scoring and model routing into the `prompt` effect
//! (issue #516, electricity M1-K2) -- K1's pure
//! `electricity_complexity::{score, route_model}` port has nothing
//! calling it until this module's [`score_and_route`], the Rust
//! equivalent of `core/prompt.py::PromptRuntime._score_and_route`.
//!
//! Also holds issue #516 §4(c)'s container/child display-depth table
//! (the [`depth`] module) -- [`crate::Frame::display_depth`] is the
//! carrier; this is just the handful of pure functions every container
//! executor calls to compute a child's own value of it.

use electricity_bytecode::{Op, PromptContent, PromptOp, RoutingOverride};
use electricity_config::ComplexitySettings;
use electricity_value::{Dict, Value};

use crate::exec::prompt::{prompt_type_str, raw_dict, role_str};
use crate::{RunContext, VmError};

/// Issue #516 §4(c)'s container/child display-depth table, as pure
/// functions of a container's own ambient depth -- every container
/// executor calls exactly one of these, right where it hands a child
/// its own [`crate::Frame`] (never inlines the `+ 1`/`+ 2` arithmetic
/// itself).
pub(crate) mod depth {
    /// A plain `dynamic` (the run root, a `finally:`, a `flow: parallel`
    /// tree branch) handing an ordinary leaf (`prompt`/`tool`/`yield`)
    /// or an `if`/`loop`/`use` child its own frame: both stay at the
    /// container's own ambient depth (`core/dynamic.py::
    /// DynamicRuntime._execute_effect`'s own `depth=self.depth`).
    // No executor ever calls this: issue #516 §4(c)'s own table has
    // this cell as a plain pass-through, which every container
    // already gets for free by simply not overriding `display_depth`
    // at all. Kept (and unit-tested) purely so the table's own
    // identity cells are documented and pinned somewhere, the same as
    // every other cell.
    #[allow(dead_code)]
    pub(crate) fn dynamic_ambient_child(ambient: u32) -> u32 {
        ambient
    }

    /// A `dynamic` handing a *nested* `dynamic` child its own frame
    /// (`core/dynamic.py`'s own `depth=self.depth + 1`) --
    /// [`crate::exec::execute_op`]'s own `Region::Block`/`Parallel`/
    /// `TryFinally` arm is the one place this applies: the run's own
    /// root `dynamic`, and a `use` child's own root `dynamic`
    /// ([`crate::Frame::nested`]), never reach `execute_op` at all, so
    /// they are never "nested" by this rule.
    pub(crate) fn dynamic_nested_dynamic_child(ambient: u32) -> u32 {
        ambient + 1
    }

    /// An `if` branch's or a `loop` body's own ambient depth for
    /// *everything* it dispatches -- one level deeper than the
    /// container holding it (`core/conditional.py::ConditionalRuntime.
    /// execute`'s own `depth=self.depth + 1` on every branch child;
    /// `core/loop.py`'s own body dispatch, same shape). Every child
    /// type's own depth from that point on reduces to
    /// [`dynamic_ambient_child`]/[`dynamic_nested_dynamic_child`]
    /// applied to *this* bumped ambient, which is exactly issue #516
    /// §4(c)'s second row (`d+1`/`d+2`/`d+1`).
    pub(crate) fn branch_or_body_ambient(ambient: u32) -> u32 {
        ambient + 1
    }

    /// A `use`/decomposition child's own root `dynamic`
    /// (`core/use.py::UseRuntime.execute`'s own `depth=self.depth + 1`)
    /// -- one level deeper than the `use` effect itself, whatever
    /// ambient depth *that* ran at. [`crate::Frame::nested`] does
    /// *not* apply this on its own (it only composes the child's own
    /// path prefix); every caller chains
    /// `.nested(..).with_display_depth(use_child_root(frame.display_depth()))`
    /// itself (`exec::use_::execute_use`, `decompose::maybe_decompose`).
    pub(crate) fn use_child_root(ambient: u32) -> u32 {
        ambient + 1
    }

    /// A reflector's own inner `dynamic` and its generated `use` both
    /// reset to depth 0 regardless of the reflector's own ambient depth
    /// (`ReflectorRuntime`/the generated-plan `UseRuntime` are both
    /// built with no `depth=` keyword at all, `core/reflector.py:
    /// ~197-207`/`:~263-273` -- DESIGN §9.7's "forced off" bullet was
    /// wrong about this: Python still *scores* a reflector's planning
    /// prompt, just at depth 0).
    pub(crate) const REFLECTOR_RESET: u32 = 0;
}

/// `PromptDefinition`'s own mapping form `score()` reads (`core/
/// complexity.py:465-510`, `:938-1035`) -- built from the compiled
/// [`PromptOp`] rather than carried on it, since nothing else needs a
/// `Value::Dict` view of a prompt effect. A field the prompt doesn't
/// set is simply absent, which [`electricity_complexity::score`] reads
/// the same as `None` (K1's own calling contract).
fn definition_value(prompt: &PromptOp) -> Value {
    let mut dict = Dict::new();
    match &prompt.content {
        PromptContent::Template(template) => {
            dict.insert(
                Value::Str("template".to_string()),
                Value::Str(template.source.clone()),
            );
        }
        // `template` is `None` on the Python definition: the text is read at
        // dispatch (issue #578), and a rendered prompt is what gets scored.
        PromptContent::TemplateFile(_) => {}
        PromptContent::Messages(messages) => {
            let items = messages
                .iter()
                .map(|message| {
                    let mut entry = Dict::new();
                    entry.insert(
                        Value::Str("role".to_string()),
                        Value::Str(role_str(message.role).to_string()),
                    );
                    entry.insert(
                        Value::Str("content".to_string()),
                        Value::Str(message.content.source.clone()),
                    );
                    Value::Dict(entry)
                })
                .collect();
            dict.insert(Value::Str("messages".to_string()), Value::List(items));
        }
    }
    dict.insert(
        Value::Str("prompt_type".to_string()),
        Value::Str(prompt_type_str(prompt.prompt_type).to_string()),
    );
    if let Some(schema) = &prompt.schema {
        dict.insert(Value::Str("schema".to_string()), schema.clone());
    }
    if let Some(params) = raw_dict(&prompt.model_params) {
        dict.insert(
            Value::Str("params".to_string()),
            Value::Dict(params.clone()),
        );
    }
    if let Some(description) = &prompt.description {
        dict.insert(
            Value::Str("description".to_string()),
            Value::Str(description.clone()),
        );
    }
    Value::Dict(dict)
}

/// `structure={"depth": self.depth}` (`core/prompt.py:981`) --
/// `loop_depth`/`reflector_generated` are not plumbed through yet
/// (issue #516 §8, out of scope here as in the reference, `core/
/// prompt.py:~978-980`).
fn structure_value(display_depth: u32) -> Value {
    let mut dict = Dict::new();
    dict.insert(
        Value::Str("depth".to_string()),
        Value::from(i64::from(display_depth)),
    );
    Value::Dict(dict)
}

/// `meta["band"] = {"name": ..., "model": ...}` (`core/prompt.py:
/// ~1000-1027`), inserted (or overwritten, for a pin) after every other
/// key `ComplexityScore::to_meta` already wrote -- `band` is always the
/// last key, issue #516 §3.2's own key-order ruling.
fn set_band(meta: &mut Value, name: &str, model: &str) {
    let Value::Dict(dict) = meta else {
        unreachable!("score_and_route only ever builds a Dict meta")
    };
    let mut band = Dict::new();
    band.insert(Value::Str("name".to_string()), Value::Str(name.to_string()));
    band.insert(
        Value::Str("model".to_string()),
        Value::Str(model.to_string()),
    );
    dict.insert(Value::Str("band".to_string()), Value::Dict(band));
}

/// The exact text `core/prompt.py::_score_and_route`'s unknown-pin
/// `ValueError` raises (§3 finding 7): `{valid}` is every named band,
/// sorted, or the "no named bands configured" text when none are.
fn unknown_band_error(name: &str, override_repr: &str, settings: &ComplexitySettings) -> VmError {
    let mut names: Vec<&str> = settings
        .routing
        .bands
        .iter()
        .filter_map(|band| band.name.as_deref())
        .filter(|n| !n.is_empty())
        .collect();
    names.sort_unstable();
    let valid = if names.is_empty() {
        "(no named bands configured)".to_string()
    } else {
        names.join(", ")
    };
    let message = format!(
        "Prompt '{name}' is pinned to routing band {override_repr}, which is not in \
         runtime.complexity.routing.bands. Valid band names: {valid}."
    );
    VmError::Raised {
        py_class: electricity_tools::PyExc::ValueError,
        message,
        cause: electricity_tools::ErrorCause::None,
        cause_class: None,
    }
}

/// `core/prompt.py::PromptRuntime._score_and_route`, ported: `(complexity
/// meta, routed model)`. Both halves are independent (K1's own crate
/// docs, "K2's calling contract"); see issue #516 §3/§7 for the full
/// rule set this follows. The `?` this can fail with is only ever the
/// unknown-pinned-band `ValueError` (§3 finding 7) -- reachable now
/// that J2 (issue #518) populates [`PromptOp::routing_override`] from
/// a profile's per-effect `routing:` override; `validate_profile_
/// routing_pins` already rejects an unknown band before the run, so
/// this `?` stays a backstop.
///
/// `resolve_complexity_settings`'s own failure branch (`core/prompt.py:
/// 914-926`) is not ported: `run_ctx.complexity` is already resolved
/// and validated before the run starts (`electricity_config::complexity::
/// validate_complexity`, lane K1/D1), so that branch is unreachable here
/// (issue #516 §7 ruling 6).
pub(crate) fn score_and_route(
    op: &Op,
    prompt: &PromptOp,
    prompt_sent: &str,
    display_depth: u32,
    run_ctx: &RunContext<'_>,
) -> Result<(Option<Value>, Option<String>), VmError> {
    let settings = run_ctx.complexity;
    let name = op.name.as_deref().unwrap_or("");

    let explicit = prompt.model.as_deref().is_some_and(|m| !m.is_empty()) || run_ctx.model_locked;

    let mut opted_out = false;
    let mut pinned_model: Option<String> = None;
    let mut pinned_band_name = String::new();

    if !explicit {
        match &prompt.routing_override {
            None => {}
            Some(RoutingOverride::Bool(false)) => opted_out = true,
            Some(RoutingOverride::Bool(true)) => {
                return Err(unknown_band_error(name, "True", settings));
            }
            Some(RoutingOverride::Named(band_name)) => match settings.routing.band_named(band_name)
            {
                Some(band) => {
                    pinned_model = Some(band.model.clone());
                    pinned_band_name = band_name.clone();
                }
                None => {
                    let repr = Value::Str(band_name.clone()).py_repr();
                    return Err(unknown_band_error(name, &repr, settings));
                }
            },
        }
    }

    if !settings.scoring.enabled {
        return Ok((None, pinned_model));
    }

    let (weights, keyword_weights) = electricity_complexity::runtime_tables(&settings.scoring);
    let definition = definition_value(prompt);
    let structure = structure_value(display_depth);
    let input = electricity_complexity::ScoreInput {
        definition: Some(&definition),
        rendered_prompt: Some(prompt_sent),
        weights: Some(&weights),
        keyword_weights: keyword_weights.as_ref(),
        structure: Some(&structure),
    };

    let result = match electricity_complexity::score(&input) {
        Ok(result) => result,
        Err(_) => {
            // `logger.warning("Complexity scoring failed for prompt %r; the
            // effect will run without a recorded score", self.defn.name,
            // exc_info=True)` -- the traceback lines `exc_info=True` adds
            // are not `WARNING:` lines and are not reproduced (issue #516
            // §3 finding 6).
            let name_repr = Value::Str(name.to_string()).py_repr();
            log::warn!(
                "Complexity scoring failed for prompt {name_repr}; the effect will run without a recorded score"
            );
            return Ok((None, pinned_model));
        }
    };

    let mut meta = result.to_meta();

    if settings.routing.enabled && !settings.routing.bands.is_empty() {
        if let Some(band) = settings.routing.band_for(result.score) {
            set_band(&mut meta, band.name.as_deref().unwrap_or(""), &band.model);
        }
    }

    if opted_out {
        return Ok((Some(meta), None));
    }

    if let Some(model) = pinned_model {
        set_band(&mut meta, &pinned_band_name, &model);
        return Ok((Some(meta), Some(model)));
    }

    let routed = electricity_complexity::route_model(result.score, &settings.routing, explicit)
        .map(|decision| decision.model.to_string());
    Ok((Some(meta), routed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_bytecode::{Message, PromptType, Role, TemplateText};
    use electricity_config::{ComplexityBand, RoutingSettings, ScoringSettings};
    use indexmap::IndexMap;

    // ── §4(c) depth table ───────────────────────────────────────────

    #[test]
    fn dynamic_row_cells() {
        assert_eq!(depth::dynamic_ambient_child(3), 3);
        assert_eq!(depth::dynamic_nested_dynamic_child(3), 4);
        assert_eq!(depth::REFLECTOR_RESET, 0);
    }

    #[test]
    fn branch_or_body_row_cells() {
        let ambient = depth::branch_or_body_ambient(3);
        assert_eq!(ambient, 4);
        assert_eq!(depth::dynamic_ambient_child(ambient), 4);
        assert_eq!(depth::dynamic_nested_dynamic_child(ambient), 5);
    }

    #[test]
    fn use_child_root_is_one_deeper_than_the_use_effects_own_ambient() {
        assert_eq!(depth::use_child_root(0), 1);
        assert_eq!(depth::use_child_root(depth::REFLECTOR_RESET), 1);
        assert_eq!(depth::use_child_root(depth::branch_or_body_ambient(2)), 4);
    }

    // ── definition mapping ──────────────────────────────────────────

    fn template_op(source: &str) -> PromptOp {
        PromptOp {
            content: PromptContent::Template(TemplateText::new(
                source.to_string(),
                true,
                electricity_bytecode::Escape::None,
            )),
            prompt_type: PromptType::Text,
            schema: None,
            model: None,
            provider: None,
            provider_fallbacks: Vec::new(),
            routing_override: None,
            model_params: None,
            timeout_ms: None,
            deterministic: false,
            inputs: None,
            assets: Vec::new(),
            retries: None,
            description: None,
            group: None,
        }
    }

    #[test]
    fn definition_value_maps_a_template_prompt() {
        let op = template_op("hello {{ x }}");
        let Value::Dict(ref d) = definition_value(&op) else {
            panic!("expected a dict");
        };
        assert_eq!(
            d.get(&Value::Str("template".to_string())),
            Some(&Value::Str("hello {{ x }}".to_string()))
        );
        assert_eq!(
            d.get(&Value::Str("prompt_type".to_string())),
            Some(&Value::Str("text".to_string()))
        );
        assert!(!d.contains_key(&Value::Str("messages".to_string())));
        assert!(!d.contains_key(&Value::Str("schema".to_string())));
        assert!(!d.contains_key(&Value::Str("params".to_string())));
        assert!(!d.contains_key(&Value::Str("description".to_string())));
    }

    #[test]
    fn definition_value_maps_a_messages_prompt_and_every_optional_field() {
        let mut op = template_op("unused");
        op.content = PromptContent::Messages(vec![Message {
            role: Role::System,
            content: TemplateText::new(
                "be terse".to_string(),
                true,
                electricity_bytecode::Escape::None,
            ),
            file: None,
        }]);
        op.prompt_type = PromptType::Json;
        let mut schema = Dict::new();
        schema.insert(
            Value::Str("type".to_string()),
            Value::Str("object".to_string()),
        );
        op.schema = Some(Value::Dict(schema));
        let mut params = Dict::new();
        params.insert(Value::Str("max_tokens".to_string()), Value::from(16i64));
        op.model_params = Some(electricity_bytecode::ParamNode::Literal(Value::Dict(
            params,
        )));
        op.description = Some("a test prompt".to_string());

        let Value::Dict(ref d) = definition_value(&op) else {
            panic!("expected a dict");
        };
        assert!(!d.contains_key(&Value::Str("template".to_string())));
        let Some(Value::List(messages)) = d.get(&Value::Str("messages".to_string())) else {
            panic!("expected a messages list");
        };
        assert_eq!(messages.len(), 1);
        let Value::Dict(entry) = &messages[0] else {
            panic!("expected a dict entry");
        };
        assert_eq!(
            entry.get(&Value::Str("role".to_string())),
            Some(&Value::Str("system".to_string()))
        );
        assert_eq!(
            entry.get(&Value::Str("content".to_string())),
            Some(&Value::Str("be terse".to_string()))
        );
        assert_eq!(
            d.get(&Value::Str("prompt_type".to_string())),
            Some(&Value::Str("json".to_string()))
        );
        assert!(d.contains_key(&Value::Str("schema".to_string())));
        assert!(d.contains_key(&Value::Str("params".to_string())));
        assert_eq!(
            d.get(&Value::Str("description".to_string())),
            Some(&Value::Str("a test prompt".to_string()))
        );
    }

    #[test]
    fn structure_value_is_exactly_depth() {
        let Value::Dict(ref d) = structure_value(2) else {
            panic!("expected a dict");
        };
        assert_eq!(d.len(), 1);
        assert_eq!(
            d.get(&Value::Str("depth".to_string())),
            Some(&Value::from(2i64))
        );
    }

    // ── score_and_route ──────────────────────────────────────────────

    fn band(model: &str, max: Option<f64>, name: Option<&str>) -> ComplexityBand {
        ComplexityBand {
            model: model.to_string(),
            max,
            name: name.map(str::to_string),
        }
    }

    fn settings_with(scoring_enabled: bool, routing: RoutingSettings) -> ComplexitySettings {
        ComplexitySettings {
            scoring: ScoringSettings {
                enabled: scoring_enabled,
                ..ScoringSettings::default()
            },
            routing,
            decomposition: electricity_config::DecompositionSettings::default(),
        }
    }

    static EMPTY_RUNTIME_CONFIG: &Value = &Value::None;

    struct Fixtures {
        registry: electricity_tools::ToolRegistry,
        limiter: crate::Limiter,
        adapters: crate::adapter::AdapterRegistry,
        dir: std::path::PathBuf,
        declared_prompts: IndexMap<String, String>,
        effect_names: std::collections::BTreeSet<String>,
    }

    impl Fixtures {
        fn new() -> Self {
            Fixtures {
                registry: electricity_tools::ToolRegistry::new(),
                limiter: crate::Limiter::new(),
                adapters: crate::adapter::AdapterRegistry::new(),
                dir: std::path::PathBuf::from("."),
                declared_prompts: IndexMap::new(),
                effect_names: std::collections::BTreeSet::new(),
            }
        }

        fn run_ctx<'a>(
            &'a self,
            complexity: &'a ComplexitySettings,
            model_locked: bool,
        ) -> RunContext<'a> {
            RunContext {
                registry: &self.registry,
                limiter: &self.limiter,
                model: "default-model",
                adapter: "scripted",
                runtime_config: EMPTY_RUNTIME_CONFIG,
                dry_run: false,
                armed: false,
                adapters: &self.adapters,
                default_adapter: std::rc::Rc::new(crate::adapter::NoopAdapter),
                model_locked,
                adapter_timeout_seconds: 120,
                complexity,
                decomposition_depth: 0,
                orchestration_dir: &self.dir,
                declared_prompts: &self.declared_prompts,
                effect_names: &self.effect_names,
                supported: electricity_bytecode::Supported::m0(&[]),
            }
        }
    }

    fn op_named(name: &str) -> Op {
        Op {
            path: electricity_bytecode::EffectPath::root().push_name(name),
            name: Some(name.to_string()),
            kind: electricity_bytecode::NodeKind::Leaf(Box::new(
                electricity_bytecode::LeafKind::Prompt(template_op("hi")),
            )),
            on_error: electricity_bytecode::OnError::Fail,
            labels: None,
            enabled: true,
        }
    }

    #[test]
    fn scoring_off_gives_no_meta_and_no_route() {
        let fixtures = Fixtures::new();
        let settings = settings_with(false, RoutingSettings::default());
        let run_ctx = fixtures.run_ctx(&settings, false);
        let op = op_named("ask");
        let prompt = template_op("hi");
        let (meta, routed) = score_and_route(&op, &prompt, "hi", 0, &run_ctx).unwrap();
        assert_eq!(meta, None);
        assert_eq!(routed, None);
    }

    #[test]
    fn scoring_off_with_a_pin_still_returns_the_pinned_model() {
        let fixtures = Fixtures::new();
        let routing = RoutingSettings {
            bands: vec![band("big", None, Some("big"))],
            ..RoutingSettings::default()
        };
        let settings = settings_with(false, routing);
        let run_ctx = fixtures.run_ctx(&settings, false);
        let op = op_named("ask");
        let mut prompt = template_op("hi");
        prompt.routing_override = Some(RoutingOverride::Named("big".to_string()));
        let (meta, routed) = score_and_route(&op, &prompt, "hi", 0, &run_ctx).unwrap();
        assert_eq!(meta, None);
        assert_eq!(routed, Some("big".to_string()));
    }

    #[test]
    fn scoring_on_routing_off_scores_with_no_band_key() {
        let fixtures = Fixtures::new();
        let settings = settings_with(true, RoutingSettings::default());
        let run_ctx = fixtures.run_ctx(&settings, false);
        let op = op_named("ask");
        let prompt = template_op("hi");
        let (meta, routed) = score_and_route(&op, &prompt, "hi", 0, &run_ctx).unwrap();
        let Some(Value::Dict(ref meta)) = meta else {
            panic!("expected a meta dict");
        };
        assert!(!meta.contains_key(&Value::Str("band".to_string())));
        assert_eq!(routed, None);
        assert_eq!(
            meta.keys().cloned().collect::<Vec<_>>(),
            vec![
                Value::Str("score".to_string()),
                Value::Str("max_score".to_string()),
                Value::Str("mode".to_string()),
                Value::Str("estimated".to_string()),
                Value::Str("weight_total".to_string()),
                Value::Str("signals".to_string()),
                Value::Str("warnings".to_string()),
            ]
        );
    }

    #[test]
    fn routing_on_records_band_even_when_the_router_defers() {
        let fixtures = Fixtures::new();
        let routing = RoutingSettings {
            enabled: true,
            bands: vec![
                band("small", Some(10.0), Some("low")),
                band("big", None, Some("high")),
            ],
            ..RoutingSettings::default()
        };
        let settings = settings_with(true, routing);
        let run_ctx = fixtures.run_ctx(&settings, false);
        let op = op_named("ask");
        let mut prompt = template_op("hi");
        prompt.model = Some("explicit-model".to_string());
        let (meta, routed) = score_and_route(&op, &prompt, "hi", 0, &run_ctx).unwrap();
        let Some(Value::Dict(ref meta)) = meta else {
            panic!("expected a meta dict");
        };
        assert!(meta.contains_key(&Value::Str("band".to_string())));
        assert_eq!(routed, None);
        // `band` is the key written last, after `warnings`.
        assert_eq!(meta.keys().last(), Some(&Value::Str("band".to_string())));
    }

    #[test]
    fn a_locked_run_defers_but_still_records_band() {
        let fixtures = Fixtures::new();
        let routing = RoutingSettings {
            enabled: true,
            bands: vec![band("big", None, Some("high"))],
            ..RoutingSettings::default()
        };
        let settings = settings_with(true, routing);
        let run_ctx = fixtures.run_ctx(&settings, true);
        let op = op_named("ask");
        let prompt = template_op("hi");
        let (meta, routed) = score_and_route(&op, &prompt, "hi", 0, &run_ctx).unwrap();
        let Some(Value::Dict(ref meta)) = meta else {
            panic!("expected a meta dict");
        };
        assert!(meta.contains_key(&Value::Str("band".to_string())));
        assert_eq!(routed, None);
    }

    #[test]
    fn respect_explicit_false_routes_despite_an_explicit_model() {
        let fixtures = Fixtures::new();
        let routing = RoutingSettings {
            enabled: true,
            respect_explicit: false,
            bands: vec![band("big", None, Some("high"))],
        };
        let settings = settings_with(true, routing);
        let run_ctx = fixtures.run_ctx(&settings, false);
        let op = op_named("ask");
        let mut prompt = template_op("hi");
        prompt.model = Some("explicit-model".to_string());
        let (_, routed) = score_and_route(&op, &prompt, "hi", 0, &run_ctx).unwrap();
        assert_eq!(routed, Some("big".to_string()));
    }

    #[test]
    fn a_band_with_an_empty_model_defers() {
        let fixtures = Fixtures::new();
        let routing = RoutingSettings {
            enabled: true,
            bands: vec![band("", None, Some("empty"))],
            ..RoutingSettings::default()
        };
        let settings = settings_with(true, routing);
        let run_ctx = fixtures.run_ctx(&settings, false);
        let op = op_named("ask");
        let prompt = template_op("hi");
        let (_, routed) = score_and_route(&op, &prompt, "hi", 0, &run_ctx).unwrap();
        assert_eq!(routed, None);
    }

    #[test]
    fn opt_out_records_band_and_never_routes() {
        let fixtures = Fixtures::new();
        let routing = RoutingSettings {
            enabled: true,
            bands: vec![band("big", None, Some("high"))],
            ..RoutingSettings::default()
        };
        let settings = settings_with(true, routing);
        let run_ctx = fixtures.run_ctx(&settings, false);
        let op = op_named("ask");
        let mut prompt = template_op("hi");
        prompt.routing_override = Some(RoutingOverride::Bool(false));
        let (meta, routed) = score_and_route(&op, &prompt, "hi", 0, &run_ctx).unwrap();
        let Some(Value::Dict(ref meta)) = meta else {
            panic!("expected a meta dict");
        };
        assert!(meta.contains_key(&Value::Str("band".to_string())));
        assert_eq!(routed, None);
    }

    #[test]
    fn a_pin_overwrites_band_and_routes() {
        let fixtures = Fixtures::new();
        let routing = RoutingSettings {
            enabled: true,
            bands: vec![band("small", None, Some("low"))],
            ..RoutingSettings::default()
        };
        let settings = settings_with(true, routing);
        let run_ctx = fixtures.run_ctx(&settings, false);
        let op = op_named("ask");
        let mut prompt = template_op("hi");
        prompt.routing_override = Some(RoutingOverride::Named("low".to_string()));
        let (meta, routed) = score_and_route(&op, &prompt, "hi", 0, &run_ctx).unwrap();
        assert_eq!(routed, Some("small".to_string()));
        let Some(Value::Dict(ref meta)) = meta else {
            panic!("expected a meta dict");
        };
        let Some(Value::Dict(band)) = meta.get(&Value::Str("band".to_string())) else {
            panic!("expected a band dict");
        };
        assert_eq!(
            band.get(&Value::Str("name".to_string())),
            Some(&Value::Str("low".to_string()))
        );
        assert_eq!(
            band.get(&Value::Str("model".to_string())),
            Some(&Value::Str("small".to_string()))
        );
    }

    #[test]
    fn an_unknown_pin_raises_with_the_sorted_valid_names() {
        let fixtures = Fixtures::new();
        let routing = RoutingSettings {
            enabled: true,
            bands: vec![
                band("z", None, Some("zeta")),
                band("a", Some(1.0), Some("alpha")),
            ],
            ..RoutingSettings::default()
        };
        let settings = settings_with(true, routing);
        let run_ctx = fixtures.run_ctx(&settings, false);
        let op = op_named("ask");
        let mut prompt = template_op("hi");
        prompt.routing_override = Some(RoutingOverride::Named("nope".to_string()));
        let err = score_and_route(&op, &prompt, "hi", 0, &run_ctx).unwrap_err();
        let VmError::Raised {
            py_class, message, ..
        } = err
        else {
            panic!("expected a Raised error");
        };
        assert_eq!(py_class, electricity_tools::PyExc::ValueError);
        assert_eq!(
            message,
            "Prompt 'ask' is pinned to routing band 'nope', which is not in \
             runtime.complexity.routing.bands. Valid band names: alpha, zeta."
        );
    }

    #[test]
    fn an_unknown_pin_with_no_named_bands_says_so() {
        let fixtures = Fixtures::new();
        let routing = RoutingSettings {
            enabled: true,
            bands: vec![band("z", None, None)],
            ..RoutingSettings::default()
        };
        let settings = settings_with(true, routing);
        let run_ctx = fixtures.run_ctx(&settings, false);
        let op = op_named("ask");
        let mut prompt = template_op("hi");
        prompt.routing_override = Some(RoutingOverride::Named("nope".to_string()));
        let err = score_and_route(&op, &prompt, "hi", 0, &run_ctx).unwrap_err();
        let VmError::Raised { message, .. } = err else {
            panic!("expected a Raised error");
        };
        assert!(message.ends_with("Valid band names: (no named bands configured)."));
    }

    #[test]
    fn a_true_override_takes_the_unknown_pin_path_with_true_as_the_repr() {
        let fixtures = Fixtures::new();
        let settings = settings_with(true, RoutingSettings::default());
        let run_ctx = fixtures.run_ctx(&settings, false);
        let op = op_named("ask");
        let mut prompt = template_op("hi");
        prompt.routing_override = Some(RoutingOverride::Bool(true));
        let err = score_and_route(&op, &prompt, "hi", 0, &run_ctx).unwrap_err();
        let VmError::Raised { message, .. } = err else {
            panic!("expected a Raised error");
        };
        assert!(message.contains("is pinned to routing band True,"));
    }

    #[test]
    fn a_locked_or_explicit_run_never_consults_the_override() {
        let fixtures = Fixtures::new();
        let settings = settings_with(false, RoutingSettings::default());
        let run_ctx = fixtures.run_ctx(&settings, true);
        let op = op_named("ask");
        let mut prompt = template_op("hi");
        // An override that would otherwise raise (an unknown pin) is
        // simply never read once the run is locked/explicit.
        prompt.routing_override = Some(RoutingOverride::Named("nope".to_string()));
        let (meta, routed) = score_and_route(&op, &prompt, "hi", 0, &run_ctx).unwrap();
        assert_eq!(meta, None);
        assert_eq!(routed, None);
    }

    #[test]
    fn a_scoring_failure_gives_no_meta_and_keeps_a_pin() {
        let fixtures = Fixtures::new();
        let routing = RoutingSettings {
            bands: vec![band("big", None, Some("big"))],
            ..RoutingSettings::default()
        };
        let settings = settings_with(true, routing);
        let run_ctx = fixtures.run_ctx(&settings, false);
        let op = op_named("ask");
        let mut prompt = template_op("hi");
        prompt.routing_override = Some(RoutingOverride::Named("big".to_string()));
        // A 400-digit integer overflows `f64`, forcing `score()` to
        // `Err` (issue #516 §3 finding 6/case `complexity-scoring-
        // failure-warns`).
        let mut params = Dict::new();
        params.insert(
            Value::Str("num_predict".to_string()),
            Value::Int(electricity_value::IntValue::parse_decimal(&"1".repeat(400)).unwrap()),
        );
        prompt.model_params = Some(electricity_bytecode::ParamNode::Literal(Value::Dict(
            params,
        )));
        let (meta, routed) = score_and_route(&op, &prompt, "hi", 0, &run_ctx).unwrap();
        assert_eq!(meta, None);
        assert_eq!(routed, Some("big".to_string()));
    }

    #[test]
    fn nonfinite_signal_scores_to_the_catch_all_band() {
        let fixtures = Fixtures::new();
        let routing = RoutingSettings {
            enabled: true,
            bands: vec![band("catch-all", None, None)],
            ..RoutingSettings::default()
        };
        let settings = settings_with(true, routing);
        let run_ctx = fixtures.run_ctx(&settings, false);
        let op = op_named("ask");
        let mut prompt = template_op("hi");
        let mut params = Dict::new();
        params.insert(
            Value::Str("num_predict".to_string()),
            Value::Float(f64::INFINITY),
        );
        prompt.model_params = Some(electricity_bytecode::ParamNode::Literal(Value::Dict(
            params,
        )));
        let (meta, routed) = score_and_route(&op, &prompt, "hi", 0, &run_ctx).unwrap();
        let Some(Value::Dict(ref meta)) = meta else {
            panic!("expected a meta dict");
        };
        assert_eq!(
            meta.get(&Value::Str("score".to_string())),
            Some(&Value::Float(0.0))
        );
        assert_eq!(routed, Some("catch-all".to_string()));
    }
}
