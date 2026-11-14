//! `circuitry.generated_doc`, ported -- the generated-document ceiling
//! (#772). A document produced while a run executes (a reflector's
//! plan, or a `use` whose `inline:` is rendered from state) can carry a
//! tool effect that looks exactly like a document the author wrote by
//! hand. This module tells the two apart and enforces the ceiling, with
//! the same wording Python's own `circuitry.generated_doc` uses.
//!
//! See that module's own doc comment for the rule in full; ported
//! function-for-function here so a change to one is a visible diff
//! against the other. Effect *type* and tool *provider* names are
//! matched case-insensitively everywhere in this module, the same way
//! `electricity_tools::registry::build_plugin` normalizes a provider
//! name before dispatch.

use electricity_value::pycompat::py_strip;
use electricity_value::{Dict, Value};
use std::collections::BTreeSet;

/// `runtime_config` key holding the current document's own, statically
/// referenced tool-plugin provider names -- `circuitry.generated_doc.
/// GENERATED_DOCUMENT_PROVIDERS_KEY`. Not read from electricity's own
/// `runtime_config` today (that value is a single, run-wide, immutable
/// reference -- `RunContext::runtime_config`'s own doc comment); the
/// per-document ceiling instead rides `Frame::document_tool_providers`,
/// which is threaded the same way `Frame::use_call_stack` is. Kept here,
/// unused, only so a reader comparing the two modules finds the matching
/// constant in both.
#[allow(dead_code)]
pub const GENERATED_DOCUMENT_PROVIDERS_KEY: &str = "_document_tool_providers";

/// A decomposition plan has no generating `use`/`reflector` effect of its
/// own to carry a literal `allow_tools:` (#772 rule 3) -- its own merge
/// step commonly needs a tool the document it serves doesn't otherwise
/// use, so every decomposition plan is additionally allowed this fixed
/// set of tool providers, none of which run a command or reach anything
/// outside the process: pure data transforms, never a side effect.
/// `circuitry.generated_doc.DECOMPOSITION_SAFE_PROVIDERS`.
pub fn decomposition_safe_providers() -> BTreeSet<String> {
    [
        "json",
        "regex",
        "math",
        "cel",
        "clock",
        "hash",
        "base64",
        "hex",
        "uuid",
        "csv",
        "validate_yaml",
    ]
    .into_iter()
    .map(String::from)
    .collect()
}

/// Security-sensitive keys (#772 rule 6): a scalar at one of these, read
/// anywhere in an effect's own dict (an exact-key match, any depth --
/// never narrowed to e.g. only `params.allowed_commands`, since that is
/// strictly the safer direction), must be byte-identical between the
/// unrendered template and the rendered document for [`is_generated_inline`]
/// to still call it the author's own.
fn is_sensitive_shape_key(key: &str) -> bool {
    matches!(
        key,
        "type" | "provider" | "allow_tools" | "allowed_commands" | "shell"
    )
}

/// *value* as a lowercase, stripped provider/effect-type name, or `None`
/// when it isn't a (non-blank) string -- `circuitry.generated_doc.
/// _normalize_name`, matching the exact normalization
/// `electricity_tools::registry::build_plugin` applies to a `provider:`
/// field before dispatch.
fn normalize_name(value: Option<&Value>) -> Option<String> {
    let s = value?.as_str()?;
    let normalized = py_strip(s).to_lowercase();
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

fn get<'a>(dict: &'a Dict, key: &str) -> Option<&'a Value> {
    dict.get(&Value::Str(key.to_string()))
}

fn effects_of(effect: &Dict) -> Option<&Value> {
    get(effect, "effects").or_else(|| get(effect, "steps"))
}

/// Best-effort YAML parse, or `None` on any failure -- used only to look
/// for literal structure, never to execute or trust the result as a
/// real document.
fn best_effort_yaml(text: &str) -> Option<Value> {
    electricity_yaml::load_yaml(text).ok()
}

fn walk_tool_providers(effects: Option<&Value>, providers: &mut BTreeSet<String>) {
    let Some(Value::List(effects)) = effects else {
        return;
    };
    for effect in effects {
        let Some(effect) = effect.as_dict() else {
            continue;
        };
        let etype = normalize_name(get(effect, "type")).unwrap_or_default();
        match etype.as_str() {
            "tool" => {
                if let Some(provider) = normalize_name(get(effect, "provider")) {
                    providers.insert(provider);
                }
            }
            "dynamic" => {
                walk_tool_providers(effects_of(effect), providers);
                walk_tool_providers(get(effect, "finally"), providers);
            }
            "if" | "conditional" => {
                walk_tool_providers(get(effect, "then"), providers);
                walk_tool_providers(get(effect, "else"), providers);
            }
            "loop" => {
                walk_tool_providers(get(effect, "body"), providers);
            }
            "reflector" => {
                walk_tool_providers(effects_of(effect), providers);
            }
            "use" => {
                if let Some(inline) = get(effect, "inline").and_then(Value::as_str) {
                    if let Some(parsed) = best_effort_yaml(inline) {
                        if let Some(parsed) = parsed.as_dict() {
                            walk_tool_providers(effects_of(parsed), providers);
                            walk_tool_providers(get(parsed, "finally"), providers);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// `circuitry.generated_doc.static_tool_providers` -- every `type: tool`
/// provider this document's own text references, recursing into a
/// literal `use: inline:` child's own text; never follows `path:`.
pub fn static_tool_providers(orch: &Value) -> BTreeSet<String> {
    let mut providers = BTreeSet::new();
    if let Some(dict) = orch.as_dict() {
        walk_tool_providers(effects_of(dict), &mut providers);
        walk_tool_providers(get(dict, "finally"), &mut providers);
    }
    providers
}

/// The root document's own static tool providers (#772), walked off the
/// *compiled* tree rather than raw YAML -- `execute_root`'s own entry
/// point has no raw document in hand (`electricity_bytecode::Program`
/// never carries one, only what compiled from it), unlike a `use`
/// child's own [`LoadedChild::document`](crate::exec::use_), which does
/// and uses [`static_tool_providers`] directly instead. Walked off the
/// compiled tree *after* a profile's own per-effect `provider` override
/// already applied (#772 rule 9: Python's own root-document equivalent,
/// `circuitry.generated_doc.static_tool_providers_from_definitions`,
/// walks the same post-override compiled tree). Walks the same shape
/// [`static_tool_providers`] walks over raw YAML: every `LeafKind::
/// Tool` provider, recursing into every control region and a
/// reflector's own inner effects, plus a literal `use: inline:` leaf's
/// own text (best-effort re-parsed) -- never a `path:`/`orchestration:`
/// leaf, a separate file judged on its own.
pub fn static_tool_providers_from_op(op: &electricity_bytecode::Op) -> BTreeSet<String> {
    let mut providers = BTreeSet::new();
    walk_op_tool_providers(op, &mut providers);
    providers
}

fn walk_op_tool_providers(op: &electricity_bytecode::Op, providers: &mut BTreeSet<String>) {
    use electricity_bytecode::{LeafKind, NodeKind, UseSource};
    match &op.kind {
        NodeKind::Leaf(leaf) => match leaf.as_ref() {
            LeafKind::Tool(tool_op) => {
                let normalized = py_strip(&tool_op.provider).to_lowercase();
                if !normalized.is_empty() {
                    providers.insert(normalized);
                }
            }
            LeafKind::Use(use_op) => {
                if let UseSource::Inline(text) = &use_op.source {
                    if let Some(parsed) = best_effort_yaml(&text.source) {
                        if let Some(parsed) = parsed.as_dict() {
                            walk_tool_providers(effects_of(parsed), providers);
                            walk_tool_providers(get(parsed, "finally"), providers);
                        }
                    }
                }
            }
            LeafKind::Reflector(reflector_op) => {
                walk_region_tool_providers(&reflector_op.inner, providers);
            }
            LeafKind::Prompt(_) | LeafKind::Yield(_) | LeafKind::Report(_) => {}
        },
        NodeKind::Control(region) => walk_region_tool_providers(region, providers),
    }
}

/// Every `use`/`reflector` effect's own literal `allow_tools:` entry that
/// doesn't name a known tool-plugin provider -- `core/compiler.py::
/// _compile_allow_tools`'s own check, which `compile_allow_tools`
/// (`electricity-compiler/src/compile/leaves.rs`) cannot make itself: that
/// crate cannot depend on `electricity-tools` (circular), so an unknown
/// name there compiles as inert text rather than a compile error. Walked
/// off the already-compiled tree instead, once, by a caller (the
/// `electricity` crate) that depends on both. Named with each effect's own
/// local path (`EffectPath`'s own `Display`, not a [`crate::frame::Frame`]-
/// prefixed one -- there is no [`crate::frame::Frame`] yet at this point,
/// before the run has even started).
pub fn unknown_allow_tools_errors(op: &electricity_bytecode::Op) -> Vec<String> {
    let known: BTreeSet<String> = electricity_tools::registry::supported_names()
        .into_iter()
        .collect();
    let mut errors = Vec::new();
    walk_op_unknown_allow_tools(op, &known, &mut errors);
    errors
}

fn walk_op_unknown_allow_tools(
    op: &electricity_bytecode::Op,
    known: &BTreeSet<String>,
    errors: &mut Vec<String>,
) {
    use electricity_bytecode::{LeafKind, NodeKind};
    match &op.kind {
        NodeKind::Leaf(leaf) => match leaf.as_ref() {
            LeafKind::Use(use_op) => {
                check_allow_tools(&use_op.allow_tools, "Use", &op.path, known, errors);
            }
            LeafKind::Reflector(reflector_op) => {
                check_allow_tools(
                    &reflector_op.allow_tools,
                    "Reflector",
                    &op.path,
                    known,
                    errors,
                );
                walk_region_unknown_allow_tools(&reflector_op.inner, known, errors);
            }
            LeafKind::Tool(_) | LeafKind::Prompt(_) | LeafKind::Yield(_) | LeafKind::Report(_) => {}
        },
        NodeKind::Control(region) => walk_region_unknown_allow_tools(region, known, errors),
    }
}

fn check_allow_tools(
    allow_tools: &BTreeSet<String>,
    label: &str,
    path: &electricity_bytecode::EffectPath,
    known: &BTreeSet<String>,
    errors: &mut Vec<String>,
) {
    for name in allow_tools {
        if !known.contains(name) {
            let known_sorted: Vec<&str> = known.iter().map(String::as_str).collect();
            errors.push(format!(
                "{label} effect at '{path}': 'allow_tools' names unknown tool provider \
                 '{name}'. Known providers: {}.",
                known_sorted.join(", ")
            ));
        }
    }
}

fn walk_region_unknown_allow_tools(
    region: &electricity_bytecode::Region,
    known: &BTreeSet<String>,
    errors: &mut Vec<String>,
) {
    use electricity_bytecode::Region;
    match region {
        Region::Block { ops, .. } => {
            for op in ops {
                walk_op_unknown_allow_tools(op, known, errors);
            }
        }
        Region::Parallel { branches, .. } => {
            for op in branches {
                walk_op_unknown_allow_tools(op, known, errors);
            }
        }
        Region::If { then_, else_, .. } => {
            walk_region_unknown_allow_tools(then_, known, errors);
            if let Some(else_) = else_ {
                walk_region_unknown_allow_tools(else_, known, errors);
            }
        }
        Region::Loop { body, .. } => walk_region_unknown_allow_tools(body, known, errors),
        Region::TryFinally { body, finally } => {
            walk_region_unknown_allow_tools(body, known, errors);
            walk_region_unknown_allow_tools(finally, known, errors);
        }
    }
}

fn walk_region_tool_providers(
    region: &electricity_bytecode::Region,
    providers: &mut BTreeSet<String>,
) {
    use electricity_bytecode::Region;
    match region {
        Region::Block { ops, .. } => {
            for op in ops {
                walk_op_tool_providers(op, providers);
            }
        }
        Region::Parallel { branches, .. } => {
            for op in branches {
                walk_op_tool_providers(op, providers);
            }
        }
        Region::If { then_, else_, .. } => {
            walk_region_tool_providers(then_, providers);
            if let Some(else_) = else_ {
                walk_region_tool_providers(else_, providers);
            }
        }
        Region::Loop { body, .. } => walk_region_tool_providers(body, providers),
        Region::TryFinally { body, finally } => {
            walk_region_tool_providers(body, providers);
            walk_region_tool_providers(finally, providers);
        }
    }
}

fn walk_shell_allowed_commands(effects: Option<&Value>, commands: &mut BTreeSet<String>) {
    let Some(Value::List(effects)) = effects else {
        return;
    };
    for effect in effects {
        let Some(effect) = effect.as_dict() else {
            continue;
        };
        let etype = normalize_name(get(effect, "type")).unwrap_or_default();
        match etype.as_str() {
            "tool" => {
                let provider = normalize_name(get(effect, "provider"));
                if provider.as_deref() == Some("shell") {
                    if let Some(params) = get(effect, "params").and_then(Value::as_dict) {
                        if let Some(Value::List(allowed)) = get(params, "allowed_commands") {
                            for c in allowed {
                                if let Some(s) = c.as_str() {
                                    commands.insert(s.to_string());
                                }
                            }
                        }
                    }
                }
            }
            "dynamic" => {
                walk_shell_allowed_commands(effects_of(effect), commands);
                walk_shell_allowed_commands(get(effect, "finally"), commands);
            }
            "if" | "conditional" => {
                walk_shell_allowed_commands(get(effect, "then"), commands);
                walk_shell_allowed_commands(get(effect, "else"), commands);
            }
            "loop" => {
                walk_shell_allowed_commands(get(effect, "body"), commands);
            }
            "reflector" => {
                walk_shell_allowed_commands(effects_of(effect), commands);
            }
            "use" => {
                if let Some(inline) = get(effect, "inline").and_then(Value::as_str) {
                    if let Some(parsed) = best_effort_yaml(inline) {
                        if let Some(parsed) = parsed.as_dict() {
                            walk_shell_allowed_commands(effects_of(parsed), commands);
                            walk_shell_allowed_commands(get(parsed, "finally"), commands);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// `circuitry.generated_doc.static_shell_allowed_commands` -- every
/// command this document's own literal `shell` effects allow (#772 rule
/// 2): every `params.allowed_commands` entry on a literal `type: tool,
/// provider: shell` effect anywhere in the document's own text,
/// recursing the same way [`static_tool_providers`] does.
pub fn static_shell_allowed_commands(orch: &Value) -> BTreeSet<String> {
    let mut commands = BTreeSet::new();
    if let Some(dict) = orch.as_dict() {
        walk_shell_allowed_commands(effects_of(dict), &mut commands);
        walk_shell_allowed_commands(get(dict, "finally"), &mut commands);
    }
    commands
}

/// The root document's own static shell allowlist (#772 rule 2), walked
/// off the *compiled* tree the same way [`static_tool_providers_from_op`]
/// walks providers -- `execute_root`'s own entry point has no raw
/// document in hand, unlike a `use` child's own
/// [`LoadedChild::document`](crate::exec::use_), which does and uses
/// [`static_shell_allowed_commands`] directly instead.
pub fn static_shell_allowed_commands_from_op(op: &electricity_bytecode::Op) -> BTreeSet<String> {
    let mut commands = BTreeSet::new();
    walk_op_shell_allowed_commands(op, &mut commands);
    commands
}

fn walk_op_shell_allowed_commands(op: &electricity_bytecode::Op, commands: &mut BTreeSet<String>) {
    use electricity_bytecode::{LeafKind, NodeKind, ParamNode, UseSource};
    match &op.kind {
        NodeKind::Leaf(leaf) => match leaf.as_ref() {
            LeafKind::Tool(tool_op) => {
                if py_strip(&tool_op.provider).to_lowercase() == "shell" {
                    if let ParamNode::Map(entries) = &tool_op.params {
                        if let Some(ParamNode::Literal(Value::List(items))) =
                            entries.get(&Value::Str("allowed_commands".to_string()))
                        {
                            for item in items {
                                if let Some(s) = item.as_str() {
                                    commands.insert(s.to_string());
                                }
                            }
                        }
                    }
                }
            }
            LeafKind::Use(use_op) => {
                if let UseSource::Inline(text) = &use_op.source {
                    if let Some(parsed) = best_effort_yaml(&text.source) {
                        if let Some(parsed) = parsed.as_dict() {
                            walk_shell_allowed_commands(effects_of(parsed), commands);
                            walk_shell_allowed_commands(get(parsed, "finally"), commands);
                        }
                    }
                }
            }
            LeafKind::Reflector(reflector_op) => {
                walk_region_shell_allowed_commands(&reflector_op.inner, commands);
            }
            LeafKind::Prompt(_) | LeafKind::Yield(_) | LeafKind::Report(_) => {}
        },
        NodeKind::Control(region) => walk_region_shell_allowed_commands(region, commands),
    }
}

fn walk_region_shell_allowed_commands(
    region: &electricity_bytecode::Region,
    commands: &mut BTreeSet<String>,
) {
    use electricity_bytecode::Region;
    match region {
        Region::Block { ops, .. } => {
            for op in ops {
                walk_op_shell_allowed_commands(op, commands);
            }
        }
        Region::Parallel { branches, .. } => {
            for op in branches {
                walk_op_shell_allowed_commands(op, commands);
            }
        }
        Region::If { then_, else_, .. } => {
            walk_region_shell_allowed_commands(then_, commands);
            if let Some(else_) = else_ {
                walk_region_shell_allowed_commands(else_, commands);
            }
        }
        Region::Loop { body, .. } => walk_region_shell_allowed_commands(body, commands),
        Region::TryFinally { body, finally } => {
            walk_region_shell_allowed_commands(body, commands);
            walk_region_shell_allowed_commands(finally, commands);
        }
    }
}

/// `circuitry.generated_doc.host_shell_pin` -- `runtime.plugins.shell.
/// allowed_commands`, the host's own ceiling, or `None` when the host
/// sets no pin at all.
pub fn host_shell_pin(runtime_config: &Value) -> Option<BTreeSet<String>> {
    let plugins = runtime_config.as_dict().and_then(|d| get(d, "plugins"))?;
    let shell_cfg = plugins.as_dict().and_then(|d| get(d, "shell"))?;
    let pin = shell_cfg
        .as_dict()
        .and_then(|d| get(d, "allowed_commands"))?;
    let Value::List(items) = pin else {
        return None;
    };
    let mut out = BTreeSet::new();
    for item in items {
        out.insert(item.as_str()?.to_string());
    }
    Some(out)
}

/// `circuitry.generated_doc.resolved_shell_allowlist` -- the host's
/// default allowlist, plus *document_literal_commands*
/// ([`static_shell_allowed_commands`] of some enclosing, non-generated
/// document), intersected with *host_pin* when the host sets one -- the
/// ceiling a generated `shell` effect's own `allowed_commands` may never
/// exceed (#772 rule 2). Idempotent: re-resolving an already-resolved set
/// against the same *host_pin* returns it unchanged, which is what lets a
/// generated document's own ceiling simply carry this set forward
/// unexamined for its own nested generated children.
pub fn resolved_shell_allowlist(
    document_literal_commands: &BTreeSet<String>,
    host_pin: Option<&BTreeSet<String>>,
) -> BTreeSet<String> {
    let mut allowed: BTreeSet<String> = electricity_tools::shell::DEFAULT_ALLOWED
        .iter()
        .map(|s| s.to_string())
        .collect();
    allowed.extend(document_literal_commands.iter().cloned());
    if let Some(pin) = host_pin {
        allowed = allowed.intersection(pin).cloned().collect();
    }
    allowed
}

/// `circuitry.generated_doc.same_shape` -- whether *a* (parsed from the
/// unrendered template) and *b* (the rendered, final document) agree on
/// structure at this node and below. A scalar is compatible with a
/// scalar -- rendering a value in is exactly what "only values templated
/// in" means -- unless *sensitive* says this position is one of the
/// security-sensitive keys (#772 rule 6): there, a rendered value that
/// merely happens to equal what the author could have written is still
/// generated. A dict/list is compatible only with a dict/list of the
/// same keys/length, recursively.
fn same_shape(a: &Value, b: &Value, sensitive: bool) -> bool {
    match (a.as_dict(), b.as_dict()) {
        (Some(_), None) | (None, Some(_)) => return false,
        (Some(a_dict), Some(b_dict)) => {
            let a_keys: BTreeSet<&str> = a_dict.keys().filter_map(Value::as_str).collect();
            let b_keys: BTreeSet<&str> = b_dict.keys().filter_map(Value::as_str).collect();
            if a_dict.len() != a_keys.len() || b_dict.len() != b_keys.len() || a_keys != b_keys {
                return false;
            }
            return a_dict.iter().all(|(key, a_value)| {
                let key_sensitive = key.as_str().is_some_and(is_sensitive_shape_key);
                b_dict
                    .get(key)
                    .is_some_and(|b_value| same_shape(a_value, b_value, key_sensitive))
            });
        }
        (None, None) => {}
    }
    match (a.as_list(), b.as_list()) {
        (Some(_), None) | (None, Some(_)) => false,
        (Some(a_list), Some(b_list)) => {
            a_list.len() == b_list.len()
                && a_list
                    .iter()
                    .zip(b_list.iter())
                    .all(|(x, y)| same_shape(x, y, sensitive))
        }
        (None, None) => {
            if sensitive && a != b {
                return false;
            }
            true
        }
    }
}

/// `circuitry.generated_doc.is_generated_inline` -- whether a `use:
/// inline:` child counts as generated (#772). `False` means the
/// unrendered template already has the rendered document's exact shape
/// (only non-sensitive scalar values differ): the author's own content,
/// with values templated in. `True` -- including when the unrendered
/// text does not parse as YAML at all, the common case for a template
/// that is just one `{{{...}}}` tag -- means some or all of this
/// document's structure (or a security-sensitive field's value) only
/// exists because it was rendered.
pub fn is_generated_inline(raw_template: &str, rendered: &Value) -> bool {
    match best_effort_yaml(raw_template) {
        Some(raw_parsed) if raw_parsed.as_dict().is_some() => {
            !same_shape(&raw_parsed, rendered, false)
        }
        _ => true,
    }
}

fn as_bool(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(b)) => *b,
        Some(Value::Str(s)) => matches!(
            py_strip(s).to_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Some(Value::None) => false,
        Some(_) => true,
        None => false,
    }
}

#[allow(clippy::too_many_arguments)]
fn walk_generated_errors(
    effects: Option<&Value>,
    path: &str,
    allowed_providers: Option<&BTreeSet<String>>,
    known_providers: Option<&BTreeSet<String>>,
    allowed_shell_commands: &BTreeSet<String>,
    generating_effect_path: &str,
    allow_tools_available: bool,
    errors: &mut Vec<String>,
) {
    let Some(Value::List(effects)) = effects else {
        return;
    };
    for effect in effects {
        let Some(effect) = effect.as_dict() else {
            continue;
        };
        let etype = normalize_name(get(effect, "type")).unwrap_or_default();
        let name = get(effect, "name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("<unnamed>");
        let effect_label = if path.is_empty() {
            name.to_string()
        } else {
            format!("{path}.{name}")
        };
        if etype == "tool" {
            let provider_name = normalize_name(get(effect, "provider"));
            let empty_params = Dict::new();
            let params = get(effect, "params")
                .and_then(Value::as_dict)
                .unwrap_or(&empty_params);
            if provider_name.as_deref() == Some("shell")
                && params.contains_key(&Value::Str("allowed_commands".to_string()))
            {
                let requested = get(params, "allowed_commands");
                let mut bad: BTreeSet<String> = BTreeSet::new();
                let is_list = matches!(requested, Some(Value::List(_)));
                if let Some(Value::List(items)) = requested {
                    for item in items {
                        if let Some(s) = item.as_str() {
                            if !allowed_shell_commands.contains(s) {
                                bad.insert(s.to_string());
                            }
                        }
                    }
                }
                if !is_list && bad.is_empty() {
                    bad.insert("<invalid>".to_string());
                }
                if !bad.is_empty() {
                    let bad_repr = py_repr_str_list(&bad);
                    errors.push(format!(
                        "generated effect '{effect_label}' sets params.allowed_commands to \
                         include {bad_repr} on a 'shell' tool, generated by \
                         {generating_effect_path}; the host config and this document's own \
                         shell effects don't already allow {bad_repr} -- add it to a literal \
                         shell effect's allowed_commands in the document, or to \
                         runtime.plugins.shell.allowed_commands."
                    ));
                }
            } else if provider_name.as_deref() == Some("service") && as_bool(get(params, "shell")) {
                errors.push(format!(
                    "generated effect '{effect_label}' sets params.shell: true on a \
                     'service' tool, generated by {generating_effect_path}; a document \
                     generated at run time can never do that."
                ));
            } else if let Some(provider_name) = provider_name {
                // A provider that isn't even a known plugin fails the usual way (an
                // "Unknown plugin" error at dispatch) instead of this one -- adding an
                // unknown name to allow_tools would itself be a check error, so this
                // refusal is only for a real plugin the document that generated it
                // simply doesn't use.
                let is_known =
                    known_providers.is_none_or(|known| known.contains(provider_name.as_str()));
                if let Some(allowed) = allowed_providers {
                    if is_known && !allowed.contains(provider_name.as_str()) {
                        if allow_tools_available {
                            errors.push(format!(
                                "generated effect '{effect_label}' uses tool provider \
                                 '{provider_name}', which the document that generated it \
                                 doesn't use; add '{provider_name}' to allow_tools on \
                                 {generating_effect_path} to allow it."
                            ));
                        } else {
                            errors.push(format!(
                                "generated effect '{effect_label}' uses tool provider \
                                 '{provider_name}', generated by {generating_effect_path}; it \
                                 isn't among the tool providers the document it serves already \
                                 uses, plus the fixed set a generated plan is always allowed \
                                 (json, regex, math, cel, clock, hash, base64, hex, uuid, csv, \
                                 validate_yaml)."
                            ));
                        }
                    }
                }
            }
        } else if etype == "dynamic" {
            walk_generated_errors(
                effects_of(effect),
                &effect_label,
                allowed_providers,
                known_providers,
                allowed_shell_commands,
                generating_effect_path,
                allow_tools_available,
                errors,
            );
            walk_generated_errors(
                get(effect, "finally"),
                &effect_label,
                allowed_providers,
                known_providers,
                allowed_shell_commands,
                generating_effect_path,
                allow_tools_available,
                errors,
            );
        } else if etype == "if" || etype == "conditional" {
            walk_generated_errors(
                get(effect, "then"),
                &effect_label,
                allowed_providers,
                known_providers,
                allowed_shell_commands,
                generating_effect_path,
                allow_tools_available,
                errors,
            );
            walk_generated_errors(
                get(effect, "else"),
                &effect_label,
                allowed_providers,
                known_providers,
                allowed_shell_commands,
                generating_effect_path,
                allow_tools_available,
                errors,
            );
        } else if etype == "loop" {
            walk_generated_errors(
                get(effect, "body"),
                &effect_label,
                allowed_providers,
                known_providers,
                allowed_shell_commands,
                generating_effect_path,
                allow_tools_available,
                errors,
            );
        } else if etype == "reflector" {
            walk_generated_errors(
                effects_of(effect),
                &effect_label,
                allowed_providers,
                known_providers,
                allowed_shell_commands,
                generating_effect_path,
                allow_tools_available,
                errors,
            );
        } else if etype == "use" {
            if let Some(inline) = get(effect, "inline").and_then(Value::as_str) {
                if let Some(parsed) = best_effort_yaml(inline) {
                    if let Some(parsed) = parsed.as_dict() {
                        walk_generated_errors(
                            effects_of(parsed),
                            &effect_label,
                            allowed_providers,
                            known_providers,
                            allowed_shell_commands,
                            generating_effect_path,
                            allow_tools_available,
                            errors,
                        );
                        walk_generated_errors(
                            get(parsed, "finally"),
                            &effect_label,
                            allowed_providers,
                            known_providers,
                            allowed_shell_commands,
                            generating_effect_path,
                            allow_tools_available,
                            errors,
                        );
                    }
                }
            }
        }
    }
}

/// Python's own `repr()` of a sorted list of plain strings -- single
/// quotes, exactly matching `f"{sorted(x)!r}"` for a `set[str]`/
/// `list[str]` of ASCII command names (every shell command name this
/// module ever sees).
fn py_repr_str_list(items: &BTreeSet<String>) -> String {
    let inner = items
        .iter()
        .map(|s| format!("'{s}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{inner}]")
}

/// `circuitry.generated_doc.generated_document_errors` -- every rule a
/// generated document violates (#772); empty means clean. `None` for
/// *allowed_providers* skips the provider check (rule 3) while still
/// enforcing the shell-specific rules (rule 2). *generating_effect_path*
/// is the dotted state path of the `use`/`reflector` effect that
/// produced this document, named in the provider-outside-the-allowed-set
/// message so the fix is actionable; ignored when *allowed_providers* is
/// `None`. *allow_tools_available* is `false` for a decomposition plan:
/// it has no generating `use`/`reflector` effect of its own to carry a
/// literal `allow_tools:`, so a provider-outside-the-ceiling refusal
/// there names the fixed safe set instead of a field that doesn't exist.
pub fn generated_document_errors(
    orch: &Value,
    allowed_providers: Option<&BTreeSet<String>>,
    generating_effect_path: &str,
) -> Vec<String> {
    generated_document_errors_with_shell(
        orch,
        allowed_providers,
        generating_effect_path,
        None,
        true,
    )
}

/// [`generated_document_errors`], with an explicit shell-allowlist
/// ceiling and `allow_tools_available` flag -- used directly by callers
/// that have already resolved [`resolved_shell_allowlist`] for the
/// enclosing document ([`crate::exec::use_`], [`crate::decompose`]);
/// [`generated_document_errors`] itself falls back to the bare host
/// default (no document-literal commands, no pin) when a caller has
/// none to give.
pub fn generated_document_errors_with_shell(
    orch: &Value,
    allowed_providers: Option<&BTreeSet<String>>,
    generating_effect_path: &str,
    allowed_shell_commands: Option<&BTreeSet<String>>,
    allow_tools_available: bool,
) -> Vec<String> {
    let mut errors = Vec::new();
    let known_providers: Option<BTreeSet<String>> = allowed_providers.map(|_| {
        electricity_tools::registry::supported_names()
            .into_iter()
            .collect()
    });
    let fallback = resolved_shell_allowlist(&BTreeSet::new(), None);
    let shell_commands = allowed_shell_commands.unwrap_or(&fallback);
    if let Some(dict) = orch.as_dict() {
        walk_generated_errors(
            effects_of(dict),
            "",
            allowed_providers,
            known_providers.as_ref(),
            shell_commands,
            generating_effect_path,
            allow_tools_available,
            &mut errors,
        );
        walk_generated_errors(
            get(dict, "finally"),
            "",
            allowed_providers,
            known_providers.as_ref(),
            shell_commands,
            generating_effect_path,
            allow_tools_available,
            &mut errors,
        );
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_yaml::load_yaml;

    fn parse(text: &str) -> Value {
        load_yaml(text).expect("valid yaml")
    }

    #[test]
    fn whole_template_as_one_tag_is_generated() {
        let rendered = parse("effects:\n  - type: tool\n    name: t\n    provider: shell\n");
        assert!(is_generated_inline("{{{prime.gen.value}}}", &rendered));
    }

    #[test]
    fn literal_structure_with_templated_scalar_is_not_generated() {
        let raw = "effects:\n  - type: tool\n    name: step1\n    provider: shell\n    params:\n      command: echo\n      args: ['{{msg}}']\n";
        let rendered = parse(
            "effects:\n  - type: tool\n    name: step1\n    provider: shell\n    params:\n      command: echo\n      args: ['hello']\n",
        );
        assert!(!is_generated_inline(raw, &rendered));
    }

    #[test]
    fn rendered_structure_with_an_extra_effect_is_generated() {
        let raw = "effects:\n  - type: prompt\n    name: a\n    template: hi\n";
        let rendered = parse(
            "effects:\n  - type: prompt\n    name: a\n    template: hi\n  - type: tool\n    name: b\n    provider: shell\n",
        );
        assert!(is_generated_inline(raw, &rendered));
    }

    #[test]
    fn a_templated_provider_is_generated_even_when_it_renders_to_the_same_name() {
        let raw = "effects:\n  - type: tool\n    name: t\n    provider: '{{prime.pick.value}}'\n    params:\n      command: touch\n      allowed_commands: [touch]\n";
        let rendered = parse(
            "effects:\n  - type: tool\n    name: t\n    provider: shell\n    params:\n      command: touch\n      allowed_commands: [touch]\n",
        );
        assert!(is_generated_inline(raw, &rendered));
    }

    #[test]
    fn a_templated_allowed_commands_entry_is_generated() {
        let raw = "effects:\n  - type: tool\n    name: t\n    provider: shell\n    params:\n      command: touch\n      allowed_commands: ['{{prime.pick.value}}']\n";
        let rendered = parse(
            "effects:\n  - type: tool\n    name: t\n    provider: shell\n    params:\n      command: touch\n      allowed_commands: [touch]\n",
        );
        assert!(is_generated_inline(raw, &rendered));
    }

    #[test]
    fn static_tool_providers_collects_direct_and_nested_inline() {
        let orch = parse(
            "effects:\n  - type: tool\n    name: a\n    provider: shell\n  - type: use\n    name: child\n    inline: |\n      effects:\n        - type: tool\n          name: x\n          provider: docker\n",
        );
        let providers = static_tool_providers(&orch);
        assert_eq!(
            providers,
            BTreeSet::from(["shell".to_string(), "docker".to_string()])
        );
    }

    #[test]
    fn allowed_commands_on_generated_shell_effect_is_always_refused() {
        let orch = parse(
            "effects:\n  - type: tool\n    name: t\n    provider: shell\n    params:\n      command: touch\n      allowed_commands: [touch]\n",
        );
        let allowed = BTreeSet::from(["shell".to_string()]);
        let errors = generated_document_errors(&orch, Some(&allowed), "prime.plan_it");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("allowed_commands"));
        assert!(errors[0].contains("prime.plan_it"));
    }

    #[test]
    fn service_shell_true_is_always_refused() {
        let orch = parse(
            "effects:\n  - type: tool\n    name: t\n    provider: service\n    params:\n      command: touch x\n      shell: true\n",
        );
        let allowed = BTreeSet::from(["service".to_string()]);
        let errors = generated_document_errors(&orch, Some(&allowed), "prime.plan_it");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("shell: true"));
        assert!(errors[0].contains("prime.plan_it"));
    }

    #[test]
    fn provider_outside_the_allowed_set_is_refused() {
        let orch = parse("effects:\n  - type: tool\n    name: t\n    provider: awk\n");
        let allowed = BTreeSet::from(["shell".to_string()]);
        let errors = generated_document_errors(&orch, Some(&allowed), "prime.plan_it");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("awk"));
        assert!(errors[0].contains("allow_tools"));
        assert!(errors[0].contains("prime.plan_it"));
        assert!(!errors[0].contains("ceiling"));
    }

    #[test]
    fn provider_outside_the_allowed_set_is_refused_when_allow_tools_is_unavailable() {
        let orch = parse("effects:\n  - type: tool\n    name: t\n    provider: awk\n");
        let allowed = decomposition_safe_providers();
        let errors = generated_document_errors_with_shell(&orch, Some(&allowed), "", None, false);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("awk"));
        assert!(!errors[0].contains("allow_tools"));
        assert!(errors[0].contains("json"));
    }

    #[test]
    fn an_unknown_provider_is_not_flagged_here() {
        // A provider that isn't a real plugin at all fails the usual way
        // ("Unknown plugin") at dispatch, never this check -- adding an
        // unknown name to allow_tools would itself be a check error.
        let orch = parse("effects:\n  - type: tool\n    name: t\n    provider: nonexistent_tool\n");
        let allowed = BTreeSet::from(["shell".to_string()]);
        assert!(generated_document_errors(&orch, Some(&allowed), "prime.plan_it").is_empty());
    }

    #[test]
    fn provider_within_the_allowed_set_is_clean() {
        let orch = parse(
            "effects:\n  - type: tool\n    name: t\n    provider: shell\n    params:\n      command: echo\n",
        );
        let allowed = BTreeSet::from(["shell".to_string()]);
        assert!(generated_document_errors(&orch, Some(&allowed), "prime.plan_it").is_empty());
    }

    #[test]
    fn no_allowed_set_skips_the_provider_check_but_not_shell_widening() {
        let orch = parse(
            "effects:\n  - type: tool\n    name: t\n    provider: shell\n    params:\n      command: touch\n      allowed_commands: [touch]\n",
        );
        let errors = generated_document_errors(&orch, None, "");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("allowed_commands"));
    }

    #[test]
    fn provider_name_is_matched_case_insensitively() {
        let orch = parse(
            "effects:\n  - type: tool\n    name: t\n    provider: Shell\n    params:\n      command: touch\n      allowed_commands: [touch]\n",
        );
        let allowed = BTreeSet::from(["shell".to_string()]);
        let errors = generated_document_errors(&orch, Some(&allowed), "prime.plan_it");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("allowed_commands"));
    }

    #[test]
    fn effect_type_is_matched_case_insensitively() {
        let orch = parse(
            "effects:\n  - type: Dynamic\n    name: d\n    effects:\n      - type: TOOL\n        name: t\n        provider: awk\n",
        );
        let allowed = BTreeSet::from(["shell".to_string()]);
        let errors = generated_document_errors(&orch, Some(&allowed), "prime.plan_it");
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("awk"));
    }

    #[test]
    fn a_generated_shell_command_within_the_document_literal_allowlist_is_accepted() {
        let orch = parse(
            "effects:\n  - type: tool\n    name: t\n    provider: shell\n    params:\n      command: node\n      allowed_commands: [node]\n",
        );
        let allowed = BTreeSet::from(["shell".to_string()]);
        let document_literal = BTreeSet::from(["node".to_string()]);
        let shell_commands = resolved_shell_allowlist(&document_literal, None);
        let errors = generated_document_errors_with_shell(
            &orch,
            Some(&allowed),
            "prime.plan_it",
            Some(&shell_commands),
            true,
        );
        assert!(errors.is_empty());
    }

    #[test]
    fn a_generated_shell_command_beyond_the_document_literal_allowlist_is_refused() {
        let orch = parse(
            "effects:\n  - type: tool\n    name: t\n    provider: shell\n    params:\n      command: rm\n      allowed_commands: [node, rm]\n",
        );
        let allowed = BTreeSet::from(["shell".to_string()]);
        let document_literal = BTreeSet::from(["node".to_string()]);
        let shell_commands = resolved_shell_allowlist(&document_literal, None);
        let errors = generated_document_errors_with_shell(
            &orch,
            Some(&allowed),
            "prime.plan_it",
            Some(&shell_commands),
            true,
        );
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("'rm'"));
        assert!(!errors[0].contains("node"));
        assert!(errors[0].contains("runtime.plugins.shell.allowed_commands"));
    }

    #[test]
    fn unknown_allow_tools_name_is_refused_after_compiling() {
        let doc = parse(
            "effects:\n  - type: use\n    name: child\n    allow_tools: [bogus_tool]\n    inline: 'effects: []'\n",
        );
        let program = electricity_compiler::compile_document(
            &doc,
            &electricity_compiler::DocumentOrigin::Generated,
        )
        .expect("compiles");
        let errors = unknown_allow_tools_errors(&program.root);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("bogus_tool"));
        assert!(errors[0].contains("allow_tools"));
    }

    #[test]
    fn a_known_allow_tools_name_is_not_flagged() {
        let doc = parse(
            "effects:\n  - type: use\n    name: child\n    allow_tools: [shell]\n    inline: 'effects: []'\n",
        );
        let program = electricity_compiler::compile_document(
            &doc,
            &electricity_compiler::DocumentOrigin::Generated,
        )
        .expect("compiles");
        assert!(unknown_allow_tools_errors(&program.root).is_empty());
    }

    #[test]
    fn resolved_shell_allowlist_intersects_with_host_pin() {
        let document_literal = BTreeSet::from(["node".to_string(), "jq".to_string()]);
        let pin = BTreeSet::from(["node".to_string(), "ls".to_string()]);
        let allowed = resolved_shell_allowlist(&document_literal, Some(&pin));
        assert_eq!(
            allowed,
            BTreeSet::from(["node".to_string(), "ls".to_string()])
        );
    }
}
