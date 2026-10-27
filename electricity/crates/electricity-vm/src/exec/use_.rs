//! `use` effect execution (`core/use.py::UseRuntime.execute`) -- M1
//! lane I (issue #469). A child orchestration, loaded from a `path:`
//! or rendered from `inline:`, runs isolated ([`run_isolated`], shared
//! with M1-L2's decomposition lane) and its outputs are mapped back
//! under the parent node in one of three modes (explicit `outputs:`,
//! auto-generated from the child's own `interface.outputs`, or full
//! passthrough of the child's own `prime` children).
//!
//! **Child observability.** [`run_isolated_inner`] (this file's own
//! private half of [`run_isolated`], not just a thin pass-through --
//! see that function's own doc comment for why) wraps whatever
//! [`RunObserver`] it is handed in a [`ChildObserver`] before ever
//! calling [`crate::exec::dynamic::execute_dynamic`] with it: every
//! child effect path the child's own execution reports is already
//! correctly namespaced under the parent (`Frame::nested`/
//! `Frame::effect_path`, threaded through *frame* -- no path rewriting
//! of its own is needed here), but the child *root*'s own
//! `effect_start`/`effect_complete` pair resolves to exactly the same
//! path the `use` effect's own node already represents
//! (`frame.effect_path(&EffectPath::root())`, the same composition
//! `Frame::nested`'s own doc comment and tests pin), so forwarding it
//! too would duplicate that node's own start/end pair
//! (`core/use.py:43-56`'s own `_relative_child_path`/
//! `_namespaced_effect_cb`, `None` for the child root specifically).
//! [`ChildObserver`] drops exactly that one pair and forwards every
//! other hook unchanged -- including a nested child's own
//! `graft_child_write`/`clear_graft` calls, which already compose
//! correctly without any extra work here, since each nests at its own,
//! deeper `Frame::effect_path`.
//!
//! The same wrapper also turns every `write()` signal from inside the
//! child's own execution into a [`RunObserver::graft_child_write`]
//! call against the *parent* observer, carrying the child's own
//! current [`Store::saved`] snapshot -- `core/use.py::_child_on_write`/
//! `_grafted_snapshot`'s own seam (#462's `graft_child_write`/
//! `clear_graft` hooks) -- and clears that graft
//! ([`RunObserver::clear_graft`]) the moment the child's own execution
//! finishes, success or failure, before [`run_isolated`] ever returns
//! to its caller -- so [`execute_use`]'s own merge write always lands
//! after the graft is gone, never racing it. This is generic to *any*
//! [`run_isolated`] caller (this lane's own `use`, and M1-L2's
//! decomposition): both need the identical "a child root's own pair is
//! a duplicate; the parent should see every child write" behaviour, so
//! it lives here rather than being re-derived once per caller.
//!
//! [`run_isolated_inner`] (not [`run_isolated`] itself) is what
//! [`execute_use`] actually calls: it hands back the child [`Store`]
//! *and* the run's own verdict as two separate values, rather than
//! losing the (possibly partially populated) store the moment
//! execution fails the way [`run_isolated`]'s own `Result<Store,
//! VmError>` return type would. `core/use.py`'s own `child_store`
//! local variable keeps pointing at the same, now partially-written
//! `Store` object regardless of whether `DynamicRuntime.execute`
//! raised (Python's references survive an exception; Rust's owned
//! return value does not) -- `meta.child_errors`/`record_children`'s
//! own "graft whatever the child produced" rule (both of which recover
//! state from a *failed* child run, not just a successful one) need
//! exactly that same partial store back, so [`execute_use`] calls the
//! private two-value helper directly instead of the public,
//! single-value wrapper it hands to every other caller (M1-L2
//! included). [`run_isolated`]'s own public signature is unchanged
//! either way -- this is purely an internal split of its own body.
//!
//! **Seam note.** `electricity_compiler::pipeline::apply_declared_inputs`
//! (the top-level `-e` path's own default-fill/require/coerce mutation,
//! `core/interface_inputs.py::check_interface_inputs`'s real body) was
//! `fn`, not `pub fn` -- widened to `pub` (and re-exported) rather than
//! duplicated a second time here, since this lane's own `interface.
//! inputs` check against a `use` child's rendered inputs is exactly the
//! same mutation against a different *namespace* (rendered inputs, not
//! the CLI `-e` text-only one `build_input_namespace`'s own preamble
//! builds) -- a minimal, additive visibility change to a file no other
//! M1 lane edits, not a reshape of its behaviour.

use crate::exec::live_scope::LiveRoot;
use crate::exec::{CtxChain, store_err};
use crate::observer::InstanceId;
use crate::params::ParamsCtx;
use crate::store::Slot;
use crate::{CancellationToken, Frame, NodeRef, RunContext, RunObserver, Store, VmError};
use electricity_bytecode::{EffectPath, ExpectCondition, OnError, Op, Program, UseOp, UseSource};
use electricity_value::pycompat::PyExc;
use electricity_value::{Dict, Value};
use indexmap::IndexMap;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// `core/use.py`'s own `_CHILD_ROOT`.
const CHILD_ROOT: &str = "prime";

fn now_iso() -> Value {
    use chrono::SecondsFormat;
    Value::Str(chrono::Utc::now().to_rfc3339_opts(SecondsFormat::Micros, false))
}

/// Python truthiness, for the handful of raw config/interface values
/// this file reads directly off an uncompiled [`Value`] (`runtime.
/// state.record_children`, a `required:`/interface-outputs entry, a
/// child's own `meta.error`).
fn is_truthy(value: Option<&Value>) -> bool {
    match value {
        None => false,
        Some(Value::None) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Int(i)) => !i.is_zero(),
        Some(Value::Float(f)) => *f != 0.0,
        Some(Value::Str(s)) => !s.is_empty(),
        Some(Value::Bytes(b)) => !b.is_empty(),
        Some(Value::List(items)) => !items.is_empty(),
        Some(Value::Dict(d)) => !d.is_empty(),
        Some(Value::Date(_)) | Some(Value::DateTime(..)) => true,
    }
}

fn pop_leaf(node: &NodeRef, key: &str) {
    node.borrow_mut().shift_remove(&Value::Str(key.to_string()));
}

fn set_leaf(store: &Store, node: &NodeRef, key: &str, value: Value) {
    store.set_leaf(node, Value::Str(key.to_string()), value);
}

/// *node*'s own *key*, materialized (`Value::None` when absent) --
/// `core/use.py`'s own plain `meta.get(key)`/`node[key]` reads, for
/// building a `meta.attempts` entry off keys [`attempt_once`] already
/// wrote onto *node* through the ordinary [`set_leaf`] path.
fn read_key(store: &Store, node: &NodeRef, key: &str) -> Value {
    let slot = node.borrow().get(&Value::Str(key.to_string())).cloned();
    match slot {
        None => Value::None,
        Some(Slot::Value(v)) => v,
        Some(Slot::Node(child)) => store.snapshot(&child),
    }
}

/// `core/use.py::_take_child_record`, used at the *start* of every
/// attempt but the first: drops every child effect a previous attempt
/// left under *node* -- everything but `value`/`meta` -- so this
/// attempt's own child run starts from a clean node rather than mixing
/// with whatever the last one left (issue #659). Safe against
/// [`crate::store::Node::watermark`]'s own invariant (`store.rs`'s own
/// doc comment): that invariant guards a `prime`/loop-name *container*
/// node some sibling step's own chain-flow overlay watermarks by entry
/// count, never an individual effect's own node like this one -- no
/// overlay is ever built with a `use` effect's own node as its
/// `fallback`.
fn clear_child_record(node: &NodeRef) {
    node.borrow_mut()
        .retain(|key, _| matches!(key, Value::Str(s) if s == "value" || s == "meta"));
}

/// `core/use.py::_child_record_copy` -- the child's effects as one
/// attempt left them, in the same saved form [`Store::saved`] gives a
/// live node (a named loop's own `last` as its `{"$ref": "iter_<N>"}`
/// reference, not the live node it aliases): a snapshot, not a
/// [`Slot::Node`] alias, so nothing a *later* attempt does to the real
/// child tree reaches an already-recorded entry.
fn child_record_value(store: &Store, child_prime: &NodeRef) -> Value {
    let mut saved = store.saved(child_prime);
    if let Value::Dict(dict) = &mut saved {
        dict.shift_remove(&Value::Str("value".to_string()));
        dict.shift_remove(&Value::Str("meta".to_string()));
    }
    saved
}

/// One `meta.attempts` entry -- `core/use.py::_attempt_entry`, same
/// keys, same order.
#[allow(clippy::too_many_arguments)]
fn attempt_entry(
    store: &Store,
    effect_node: &NodeRef,
    meta_node: &NodeRef,
    created_at: Value,
    completed_at: Value,
    error: Option<String>,
    children: Option<Value>,
) -> Value {
    let mut entry = Dict::new();
    entry.insert(Value::from("created_at"), created_at);
    entry.insert(Value::from("completed_at"), completed_at);
    entry.insert(Value::from("value"), read_key(store, effect_node, "value"));
    entry.insert(
        Value::from("error"),
        error.map(Value::Str).unwrap_or(Value::None),
    );
    entry.insert(
        Value::from("child_errors"),
        read_key(store, meta_node, "child_errors"),
    );
    entry.insert(Value::from("expect"), read_key(store, meta_node, "expect"));
    if let Some(children) = children {
        entry.insert(Value::from("children"), children);
    }
    Value::Dict(entry)
}

/// `core/use.py::_write_attempts` -- `meta.attempts`, always written as
/// `meta`'s last key: both engines compare state by insertion order, so
/// this pops before it sets, exactly like [`pop_leaf`]+[`set_leaf`]
/// everywhere else in this module that must stay last (`retries_used`).
fn write_attempts(store: &Store, meta_node: &NodeRef, attempts: &[Value]) {
    pop_leaf(meta_node, "attempts");
    set_leaf(store, meta_node, "attempts", Value::List(attempts.to_vec()));
}

// `core/use.py::_resolve_reference` -- walks a dotted path through
// *ctx*. `crate::params::ParamsCtx::resolve_from` is the exact same
// walk (a mapping segment by exact key, a list segment by Python-
// style, negative-wrapping index, `None` on any missing key/out-of-
// range index/non-container segment) against any `ParamsCtx` -- a
// plain already-materialized `Value` or, since issue #463 (PR #482
// review), a live `LiveRoot` -- so this module calls that shared
// primitive directly rather than keeping its own second copy of it;
// only the *caller's* contract around a `None` result (no `default:`,
// passed through rather than raised -- `crate::params`'s own doc
// comment) stays `use_.rs`'s own.

/// `"Orchestration {label} validation failed:\n  - ..."` -- the shape
/// both a file/legacy child's own structural errors and an unknown
/// `group:` reference share (`core/use.py:498-502`/`:898-902`). Not
/// `electricity_compiler::RunCheckError`'s own `Display` (that
/// variant's text has no *label* of its own -- the top-level document
/// is always "the" document being checked, where a `use` child's own
/// message must name which child).
fn validation_failed_text(label: &str, errors: &[String]) -> String {
    let mut text = format!("Orchestration {label} validation failed:");
    for error in errors {
        text.push_str(&format!("\n  - {error}"));
    }
    text
}

/// `core/use.py::_clean_yaml_fences` -- moved to `electricity-yaml`
/// (issue #521, §2): `electricity-tools`'s own `validate_yaml` tool
/// needs it too, and that crate cannot depend on this one. Every
/// existing caller of `exec::use_::clean_yaml_fences` (this file's own
/// `resolve_path_child`, `tests/use_corpus.rs`) is unaffected.
pub use electricity_yaml::clean_yaml_fences;

/// `core/use.py::_validate_inline_yaml` -- `Ok(())` once *cleaned* (as
/// a parsed [`Value`]) is confirmed a mapping with an `effects` key and
/// passes `structural_errors`; `Err` the first violation's own text(s).
pub fn validate_inline_yaml(parsed: &Value) -> Result<(), Vec<String>> {
    let Some(dict) = parsed.as_dict() else {
        return Err(vec![
            "Inline orchestration must be a YAML mapping with an 'effects' key.".to_string(),
        ]);
    };
    if !dict.contains_key(&Value::Str("effects".to_string())) {
        return Err(vec![
            "Inline orchestration is missing required 'effects' key.".to_string(),
        ]);
    }
    let errors = electricity_compiler::structural_errors(parsed);
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// `hashlib.sha256(cleaned.encode("utf-8")).hexdigest()` -- the full
/// lowercase hex digest; [`Frame::push_use`]'s own identity is this
/// string's first 16 characters, applied by the caller.
/// `core/expect.py::expect_failure_summary`'s own model-mode half --
/// the CEL half is just `defn.expr`, used verbatim everywhere above.
/// Whitespace-collapsed and capped at 80 characters (Python's own
/// `collapsed[:80]` is a codepoint slice, matched here with `chars()`
/// rather than a byte slice).
fn expect_model_summary(source: &str) -> String {
    let collapsed = source.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= 80 {
        collapsed
    } else {
        let truncated: String = collapsed.chars().take(80).collect();
        format!("{truncated}...")
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

/// `core/outputs.py::normalize_one_output`, against a raw interface-
/// outputs *spec* value directly (not nested under an effect's own
/// `outputs:` key the way `electricity-compiler::compile::leaves`'s
/// own private copy is) -- this crate reads a *child*'s declared
/// `interface.outputs` at run time, long after that document compiled.
fn normalize_one_output_runtime(spec: &Value, context: &str, key: &str) -> Result<String, String> {
    match spec {
        Value::Str(s) => {
            let path = s.trim();
            if path.is_empty() {
                Err(format!(
                    "{context}: output '{key}' is an empty string. \
                     Give it a state path — {key}: {{path: prime.<effect>.value}}."
                ))
            } else {
                Ok(path.to_string())
            }
        }
        Value::Dict(d) => {
            let declared = d
                .get(&Value::Str("path".to_string()))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty());
            match declared {
                Some(declared) => Ok(declared.to_string()),
                None => Err(format!(
                    "{context}: output '{key}' has no 'path'. \
                     Canonical form is {key}: {{path: prime.<effect>.value}} \
                     (the bare string {key}: prime.<effect>.value is also accepted)."
                )),
            }
        }
        other => Err(format!(
            "{context}: output '{key}' must be an object with a 'path' key \
             (canonical) or a bare state-path string (shorthand); got {}.",
            other.type_name()
        )),
    }
}

/// `core/outputs.py::normalize_outputs`, against the raw
/// `interface.outputs` value directly -- `None` for an absent/falsy
/// value, matching Python's own `if not raw: return None`.
pub fn normalize_outputs_runtime(
    raw: Option<&Value>,
    context: &str,
) -> Result<Option<IndexMap<String, String>>, String> {
    let Some(raw) = raw.filter(|v| is_truthy(Some(v))) else {
        return Ok(None);
    };
    let Value::Dict(dict) = raw else {
        return Err(format!(
            "{context}: outputs must be a mapping of name -> {{path: <state path>}}, not {}.",
            raw.type_name()
        ));
    };
    let mut normalized = IndexMap::new();
    for (key, spec) in dict {
        let key_str = match key {
            Value::Str(s) => s.clone(),
            other => other.py_str(),
        };
        normalized.insert(
            key_str.clone(),
            normalize_one_output_runtime(spec, context, &key_str)?,
        );
    }
    Ok(Some(normalized))
}

/// `core/use.py::_resolve_dot_path` -- a dict-only walk (never a list
/// index, unlike [`crate::params::ParamsCtx::resolve_from`]'s own
/// by-reference-input walk):
/// `Value::None` the moment any segment is missing or the current
/// value isn't a dict.
pub fn resolve_dot_path(state: &Value, dot_path: &str) -> Value {
    let mut current = state.clone();
    for part in dot_path.split('.') {
        let Some(next) = current
            .as_dict()
            .and_then(|d| d.get(&Value::Str(part.to_string())))
        else {
            return Value::None;
        };
        current = next.clone();
    }
    current
}

/// `core/use.py::_collect_child_errors` -- `{path, error}` for every
/// node anywhere under *node* whose own `meta.error` is truthy, walked
/// recursively through every nested container. *prefix* is the dotted
/// path built up so far; the child root itself reports as
/// [`CHILD_ROOT`] (`"prime"`), matching Python's own `prefix or
/// _CHILD_ROOT`.
pub fn collect_child_errors(node: &Value, prefix: &str) -> Vec<(String, Value)> {
    let mut errors = Vec::new();
    let Some(dict) = node.as_dict() else {
        return errors;
    };
    if let Some(meta) = dict
        .get(&Value::Str("meta".to_string()))
        .and_then(Value::as_dict)
    {
        let error = meta.get(&Value::Str("error".to_string()));
        if is_truthy(error) {
            let path = if prefix.is_empty() {
                CHILD_ROOT.to_string()
            } else {
                prefix.to_string()
            };
            errors.push((path, error.cloned().unwrap_or(Value::None)));
        }
    }
    for (key, value) in dict {
        let Value::Str(key_str) = key else { continue };
        if key_str == "value" || key_str == "meta" {
            continue;
        }
        if value.as_dict().is_some() {
            let child_prefix = if prefix.is_empty() {
                key_str.clone()
            } else {
                format!("{prefix}.{key_str}")
            };
            errors.extend(collect_child_errors(value, &child_prefix));
        }
    }
    errors
}

/// `core/use.py::_graft_child_record` -- every key of *child_prime*
/// except `value`/`meta` copied onto *node*, preserving node identity
/// (a [`Slot::Node`] copies as an `Rc` clone of the exact same live
/// child, not a flattened snapshot -- the same aliasing [`Store::
/// merge`]'s own doc comment explains matters for a loop's `last`).
pub fn graft_child_record(node: &NodeRef, child_prime: &NodeRef) {
    let entries: Vec<(Value, Slot)> = child_prime
        .borrow()
        .iter()
        .filter(|(key, _)| !matches!(key, Value::Str(s) if s == "value" || s == "meta"))
        .map(|(key, slot)| (key.clone(), slot.clone()))
        .collect();
    let mut node_map = node.borrow_mut();
    for (key, slot) in entries {
        node_map.insert(key, slot);
    }
}

/// `allowlist_gate.py::allowed_adapters`/`allowed_tools`, against
/// `runtime_config._allowlists.{adapters,tools}` -- `None` (default
/// open) unless that entry is a list. `pub(crate)` (issue #521, §5):
/// `decompose::validate_plan`'s own allowlist check reads the same
/// `runtime_config` entry `_check_allowlists` does.
pub(crate) fn allowlist_names(runtime_config: &Value, key: &str) -> Option<Vec<String>> {
    let entry = runtime_config
        .as_dict()?
        .get(&Value::Str("_allowlists".to_string()))?
        .as_dict()?
        .get(&Value::Str(key.to_string()))?;
    match entry {
        Value::List(items) => Some(
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
        ),
        _ => None,
    }
}

/// `core/concurrency.py::parse_concurrency_groups`'s own known-name
/// set, read off the run's already-merged `runtime_config` (the same
/// value [`RunContext::runtime_config`] carries, which still holds the
/// document+config `runtime.concurrency_groups` block verbatim --
/// `electricity` (lib)'s own `build_runtime_config` only ever *adds*
/// `_orchestration_dir`/`_allowlists`/`_capability_allow` to it, never
/// removes an existing key).
fn known_concurrency_group_names(runtime_config: &Value) -> BTreeSet<String> {
    runtime_config
        .as_dict()
        .and_then(|d| d.get(&Value::Str("concurrency_groups".to_string())))
        .and_then(Value::as_dict)
        .map(|d| {
            d.keys()
                .filter_map(|k| k.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// `runtime.state.record_children` (config), off the same
/// `runtime_config` value.
fn record_children_enabled(runtime_config: &Value) -> bool {
    runtime_config
        .as_dict()
        .and_then(|d| d.get(&Value::Str("state".to_string())))
        .and_then(Value::as_dict)
        .map(|d| is_truthy(d.get(&Value::Str("record_children".to_string()))))
        .unwrap_or(false)
}

/// `core/use.py::UseRuntime._resolve_orchestration`'s own two
/// filesystem steps -- absolute/cwd-relative, then
/// parent-orchestration-relative -- shared by a `path:` and the
/// deprecated `orchestration:` field (electricity has no library, so
/// neither ever reaches a third, library-lookup step; a documented
/// divergence for the legacy field, which Python's own fallback chain
/// tries third). Returns the path exactly as resolved (never
/// canonicalized) -- `meta.resolved_path`'s own value.
fn resolve_path_child(
    name: &str,
    raw: &str,
    orchestration_dir: &Path,
    legacy: bool,
) -> Result<PathBuf, String> {
    let candidate = PathBuf::from(raw);
    if candidate.is_file() {
        return Ok(candidate);
    }
    let relative = orchestration_dir.join(raw);
    if relative.is_file() {
        return Ok(relative);
    }
    if legacy {
        Err(format!(
            "Use effect '{name}': orchestration '{raw}' not found. Provide a valid file path \
             or a bundled orchestration name (run `cof list` to see available names)."
        ))
    } else {
        Err(format!(
            "Use effect '{name}': path '{raw}' not found (checked absolute, cwd-relative, \
             and parent-orchestration-relative)."
        ))
    }
}

/// A loaded, validated, parsed child document, ready to compile --
/// [`load_child`]'s own result.
struct LoadedChild {
    document: Value,
    /// The path as resolved/displayed (`meta.resolved_path`'s own
    /// value for a file child), or `"inline"`.
    label: String,
    /// [`Frame::push_use`]'s own cycle-detection key.
    identity: String,
    /// The full hex digest (`meta.orchestration_sha256`).
    digest: String,
    document_dir: PathBuf,
    origin: electricity_compiler::DocumentOrigin,
    /// `None` for an inline child (`meta.resolved_path` stays `null`).
    resolved_path: Option<String>,
}

/// `core/use.py::UseRuntime._load_child_orch`, *validate*'s own
/// structural check included (Python's own `if self.defn.validate:
/// errors = structural_errors(...)` sits inside the same function for
/// both source kinds).
async fn load_child<R: ParamsCtx>(
    name: &str,
    source: &UseSource,
    validate: bool,
    parent_trusted: bool,
    ctx: &R,
    run_ctx: &RunContext<'_>,
) -> Result<LoadedChild, String> {
    match source {
        UseSource::Inline(text) => {
            let rendered = crate::compose::render_with_composition(
                &text.source,
                ctx,
                run_ctx.declared_prompts,
                run_ctx.effect_names,
                "inline",
                &electricity_template::PlainCtx,
                matches!(text.escape, electricity_bytecode::Escape::Html),
            )?;
            let cleaned = clean_yaml_fences(&rendered);
            // `core/use.py::_resolve_orchestration`'s own inline branch
            // (`use.py:566-577`): with `validate: true`, *every* failure
            // -- a YAML parse error included -- goes through
            // `_validate_inline_yaml`'s own `(ok, errors)` and is wrapped
            // in the "Inline orchestration validation failed:" text; with
            // `validate: false`, a parse error propagates with no prefix
            // at all (`str(yaml_error)`), and a non-mapping result gives
            // the bare "...must be a YAML mapping..." text with no
            // wrapper, since `_validate_inline_yaml` is never called.
            let document = if validate {
                // A calendar-invalid timestamp's bare `ValueError`
                // (issue #528's own comment) is never a `yaml.YAMLError`
                // `_validate_inline_yaml`'s own `except` could catch, so
                // it escapes uncaught -- no "Inline orchestration
                // validation failed:" wrapper, no `errors` list, just
                // the message alone, matched *before* any other parse
                // error is turned into an ordinary error-list entry.
                //
                // Parsed once: the inline text's own cost of parsing
                // doesn't need paying three times over to tell these
                // three outcomes apart.
                match electricity_yaml::load_yaml(&cleaned) {
                    Err(e @ electricity_yaml::YamlError::InvalidTimestampValue { .. }) => {
                        return Err(e.cof_message());
                    }
                    Err(e) => {
                        return Err(format!(
                            "Inline orchestration validation failed:\n  - YAML parse error: {e}"
                        ));
                    }
                    Ok(parsed) => {
                        if let Err(errors) = validate_inline_yaml(&parsed) {
                            return Err(format!(
                                "Inline orchestration validation failed:\n{}",
                                errors
                                    .iter()
                                    .map(|e| format!("  - {e}"))
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            ));
                        }
                        parsed
                    }
                }
            } else {
                let parsed = electricity_yaml::load_yaml(&cleaned).map_err(|e| e.cof_message())?;
                if parsed.as_dict().is_none() {
                    return Err(
                        "Inline orchestration must be a YAML mapping with an 'effects' key."
                            .to_string(),
                    );
                }
                parsed
            };
            let digest = sha256_hex(cleaned.as_bytes());
            let identity = format!("inline:{}", &digest[..16]);
            // Inline children inherit the current `orchestration_dir`
            // unchanged (`core/use.py::_load_child_orch`'s own
            // inline branch never re-roots it) -- a `{file:}` prompt
            // source is refused outright for `DocumentOrigin::
            // Generated` regardless, so no confinement root is ever
            // actually consulted for this origin.
            let document_dir = run_ctx.orchestration_dir.to_path_buf();
            Ok(LoadedChild {
                document,
                label: "inline".to_string(),
                identity,
                digest,
                document_dir,
                origin: electricity_compiler::DocumentOrigin::Generated,
                resolved_path: None,
            })
        }
        UseSource::Path(raw) | UseSource::Orchestration(raw) => {
            let legacy = matches!(source, UseSource::Orchestration(_));
            let resolved = resolve_path_child(name, raw, run_ctx.orchestration_dir, legacy)?;
            let identity_path =
                std::fs::canonicalize(&resolved).unwrap_or_else(|_| resolved.clone());
            let identity = identity_path.display().to_string();
            let document_dir = identity_path
                .parent()
                .unwrap_or(&identity_path)
                .to_path_buf();
            let confinement_root =
                electricity_compiler::project_root::default_project_root(&document_dir);
            let document = electricity_compiler::load_document(&resolved).map_err(|e| e.0)?;
            if validate {
                let errors = electricity_compiler::structural_errors(&document);
                if !errors.is_empty() {
                    return Err(validation_failed_text(
                        &resolved.display().to_string(),
                        &errors,
                    ));
                }
            }
            let digest = electricity_compiler::document_content_digest(
                &resolved,
                &document,
                &confinement_root,
            )
            .map_err(|e| e.0)?;
            let label = resolved.display().to_string();
            // A child named by path is part of the code its caller runs, so
            // it gets its parent's trust: a prompt's `file: {from: ...}`
            // `within` is then free (`File`) or stays inside the child's
            // project (`LimitedFile`), issue #578. Electricity has no
            // library, so no child is ever limited by being resolved through
            // one (`core/use.py` limits those).
            let origin = if parent_trusted {
                electricity_compiler::DocumentOrigin::File {
                    document_dir: document_dir.clone(),
                    confinement_root,
                }
            } else {
                electricity_compiler::DocumentOrigin::LimitedFile {
                    document_dir: document_dir.clone(),
                    confinement_root,
                }
            };
            Ok(LoadedChild {
                document,
                label: label.clone(),
                identity,
                digest,
                document_dir: document_dir.clone(),
                origin,
                resolved_path: Some(label),
            })
        }
    }
}

/// Everything [`attempt_once`] can fail with that the retry loop in
/// [`execute_use`] needs to classify -- `core/use.py`'s own `except
/// Exception`/`except (ValueError, RecursionError)` split, plus the
/// two electricity-only control errors ([`VmError::Cancelled`]/
/// [`VmError::Refused`]) that bypass this classification entirely
/// (`VmError::is_fatal()`).
enum AttemptProblem {
    /// `VmError::Cancelled` or `VmError::Refused`, already fully
    /// formed -- propagated immediately by [`execute_use`].
    Fatal(VmError),
    /// A `ValueError`/`RecursionError`-class failure -- never retried,
    /// whatever attempt this is.
    NotRetried { message: String, py_class: PyExc },
    /// Anything else (the child's own propagated failure, a failed
    /// `expect:`) -- retried while attempts remain. Always a
    /// `RuntimeError` class, same as Python's own wrap. *child_record*
    /// is the child's own `prime` [`NodeRef`] (`record_children` only,
    /// and only for a failure the child's own run raised), carried out
    /// so [`execute_use`] can graft it once -- never twice, never for a
    /// retried attempt -- only if this attempt turns out to be the
    /// last one.
    Retryable {
        message: String,
        child_record: Option<NodeRef>,
    },
}

fn not_retried(message: String) -> AttemptProblem {
    AttemptProblem::NotRetried {
        message,
        py_class: PyExc::ValueError,
    }
}

/// One full attempt: load, allowlist, cycle, compile, render inputs,
/// check interface, run the child isolated, map outputs, evaluate
/// `expect:` -- `core/use.py::UseRuntime.execute`'s own per-attempt
/// try block. *label* is updated in place the moment a load succeeds,
/// mirroring Python's own reassignment of its local `label` variable
/// (read by [`execute_use`]'s own final `on_error: fail` wrap even when
/// a *later* step in the same attempt fails).
#[allow(clippy::too_many_arguments)]
async fn attempt_once<R: ParamsCtx>(
    op: &Op,
    use_op: &UseOp,
    name: &str,
    effect_node: &NodeRef,
    meta_node: &NodeRef,
    ctx: &R,
    frame: &Frame,
    run_ctx: &RunContext<'_>,
    observer: &dyn RunObserver,
    token: &CancellationToken,
    store: &Store,
    label: &mut String,
    record_children: bool,
    attempt_children: &mut Option<Value>,
) -> Result<(), AttemptProblem> {
    let loaded = load_child(
        name,
        &use_op.source,
        use_op.validate,
        use_op.parent_trusted,
        ctx,
        run_ctx,
    )
    .await
    .map_err(not_retried)?;
    *label = loaded.label.clone();
    set_leaf(
        store,
        meta_node,
        "orchestration_sha256",
        Value::Str(loaded.digest.clone()),
    );
    if let Some(resolved_path) = &loaded.resolved_path {
        set_leaf(
            store,
            meta_node,
            "resolved_path",
            Value::Str(resolved_path.clone()),
        );
    }

    // `_check_allowlists` -- the child's own adapter/tool references
    // against this run's allowlists, templated references and the
    // child's own top-level `adapter:` both skipped.
    let denials = electricity_config::orchestration_denials(
        &loaded.document,
        allowlist_names(run_ctx.runtime_config, "adapters").as_deref(),
        allowlist_names(run_ctx.runtime_config, "tools").as_deref(),
        true,
        false,
    );
    if !denials.is_empty() {
        return Err(not_retried(format!(
            "use '{name}': child {} failed allowlist enforcement: {}",
            loaded.label,
            denials.join("; ")
        )));
    }

    // Cycle detection -- the stack is per call path (`Frame::
    // push_use`'s own fresh `Vec`), checked after the allowlist check,
    // matching Python's own order.
    let parent_stack = frame.use_call_stack();
    if parent_stack.contains(&loaded.identity) {
        let mut chain: Vec<&str> = parent_stack.iter().map(String::as_str).collect();
        chain.push(&loaded.identity);
        return Err(AttemptProblem::NotRetried {
            message: format!("use '{name}': cycle detected — {}", chain.join(" → ")),
            py_class: PyExc::RecursionError,
        });
    }
    // `Frame::nested` composes the child's own root path onto the
    // use effect's own path in the *parent* -- the one thing that
    // actually namespaces every effect the child runs under this
    // node; `Frame::push_use` only ever extends the cycle-detection
    // stack, never the path prefix.
    // Issue #516 §4(c): the child document's own root `dynamic` is one
    // display level deeper than the `use` effect that spawned it,
    // whatever ambient depth *this* frame already carries.
    let child_frame = frame
        .nested(&op.path)
        .with_display_depth(crate::complexity::depth::use_child_root(
            frame.display_depth(),
        ))
        .push_use(loaded.identity.clone());

    let child_program = electricity_compiler::compile_document(&loaded.document, &loaded.origin)
        .map_err(|e| not_retried(e.0))?;

    let known_groups = known_concurrency_group_names(run_ctx.runtime_config);
    let group_errors =
        electricity_compiler::groups::unknown_group_errors(&child_program, &known_groups);
    if !group_errors.is_empty() {
        return Err(not_retried(validation_failed_text(
            &loaded.label,
            &group_errors,
        )));
    }

    // Render `inputs:` -- a top-level `{from:}`/template/literal walk
    // (`core/use.py::_render_inputs`), never `crate::params`'s own
    // tool-shaped walk (this module's own doc comment).
    let mut rendered_inputs: IndexMap<Value, Value> = IndexMap::new();
    let mut unresolved: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    if let Some(electricity_bytecode::ParamNode::Map(entries)) = &use_op.inputs {
        for (key, node) in entries {
            let value = match node {
                electricity_bytecode::ParamNode::From { path, as_json, .. } => {
                    let resolved = ctx.resolve_from(path).unwrap_or(Value::None);
                    if matches!(resolved, Value::None) {
                        if let Value::Str(key_str) = key {
                            unresolved.insert(key_str.clone(), path.clone());
                        }
                        resolved
                    } else if *as_json {
                        // `core/use.py::_render_inputs`: `as: json` only
                        // serializes a value that resolved; an unresolved
                        // one stays `None`.
                        crate::params::serialize_as_json(&resolved)
                            .map(Value::Str)
                            .map_err(|reason| {
                                not_retried(
                                    crate::params::RenderParamsError::NotSerializable {
                                        label: format!("inputs.{}", key.py_str()),
                                        reference_path: path.clone(),
                                        reason,
                                    }
                                    .to_string(),
                                )
                            })?
                    } else {
                        resolved
                    }
                }
                electricity_bytecode::ParamNode::Template(text) => {
                    let label_for_render = format!("inputs.{}", key.py_str());
                    crate::compose::render_with_composition(
                        &text.source,
                        ctx,
                        run_ctx.declared_prompts,
                        run_ctx.effect_names,
                        &label_for_render,
                        &electricity_template::PlainCtx,
                        matches!(text.escape, electricity_bytecode::Escape::Html),
                    )
                    .map(Value::Str)
                    .map_err(not_retried)?
                }
                electricity_bytecode::ParamNode::Literal(value) => value.clone(),
                electricity_bytecode::ParamNode::Map(_)
                | electricity_bytecode::ParamNode::List(_) => {
                    unreachable!(
                        "electricity_compiler::compile::params::build_use_inputs only ever \
                         produces From/Template/Literal at a use effect's own top level"
                    )
                }
            };
            rendered_inputs.insert(key.clone(), value);
        }
    }

    // The interface check, with the unresolved-by-reference-required
    // pre-check ahead of it (checked first: `rendered_inputs` always
    // carries the key as `Value::None` in this case, so the shared
    // required-input check below never sees it as merely missing).
    let interface = child_program.interface.as_ref().and_then(Value::as_dict);
    let iface_inputs = interface
        .and_then(|d| d.get(&Value::Str("inputs".to_string())))
        .and_then(Value::as_dict);
    if let Some(iface_inputs) = iface_inputs {
        for (key, spec) in iface_inputs {
            let Value::Str(key_str) = key else { continue };
            let Some(spec_dict) = spec.as_dict() else {
                continue;
            };
            if is_truthy(spec_dict.get(&Value::Str("required".to_string()))) {
                if let Some(from_path) = unresolved.get(key_str) {
                    return Err(not_retried(format!(
                        "Use effect '{name}': required input '{key_str}' resolved to nothing \
                         from '{from_path}'."
                    )));
                }
            }
        }
        electricity_compiler::apply_declared_inputs(&mut rendered_inputs, iface_inputs)
            .map_err(|e| not_retried(format!("Use effect '{name}': {e}")))?;
    }

    let auto_outputs = if use_op.outputs.is_none() {
        let outputs_raw = interface.and_then(|d| d.get(&Value::Str("outputs".to_string())));
        normalize_outputs_runtime(
            outputs_raw,
            &format!("Use effect '{name}': child interface.outputs"),
        )
        .map_err(not_retried)?
    } else {
        None
    };
    let effective_outputs: Option<&IndexMap<String, String>> =
        use_op.outputs.as_ref().or(auto_outputs.as_ref());

    set_leaf(
        store,
        meta_node,
        "inputs",
        Value::Dict(rendered_inputs.clone()),
    );

    // Refused before the child ever runs -- a freshly compiled child
    // program the parent's own up-front walker could never have seen
    // (lane A's `first_unsupported`, rechecked here against the exact
    // `Supported` value the parent run was already checked against).
    if let Some(refusal) = electricity_bytecode::first_unsupported(
        &child_program,
        run_ctx.complexity.decomposition.enabled,
        &run_ctx.supported,
    ) {
        return Err(AttemptProblem::Fatal(VmError::Refused {
            refusal,
            child_label: Some(loaded.label.clone()),
        }));
    }

    let mut initial_state = Dict::new();
    initial_state.insert(
        Value::Str("input".to_string()),
        Value::Dict(rendered_inputs),
    );
    let document_dir = loaded.document_dir.clone();
    let child_run_ctx = RunContext {
        registry: run_ctx.registry,
        limiter: run_ctx.limiter,
        model: run_ctx.model,
        adapter: run_ctx.adapter,
        runtime_config: run_ctx.runtime_config,
        dry_run: run_ctx.dry_run,
        armed: run_ctx.armed,
        adapters: run_ctx.adapters,
        default_adapter: std::rc::Rc::clone(&run_ctx.default_adapter),
        model_locked: run_ctx.model_locked,
        adapter_timeout_seconds: run_ctx.adapter_timeout_seconds,
        complexity: run_ctx.complexity,
        // `core/use.py::_run_isolated_child`'s child `RunConfig` copies
        // `_decomposition_depth` unchanged (`use.py:877-879`) -- only
        // decomposition's own child increments it (`decompose.py:
        // 694-698`), so a `use` child must not count as one more level.
        decomposition_depth: run_ctx.decomposition_depth,
        orchestration_dir: &document_dir,
        declared_prompts: &child_program.prompts,
        effect_names: &child_program.effect_names,
        supported: run_ctx.supported,
    };

    let (child_store, run_result) = run_isolated_inner(
        &child_program,
        Value::Dict(initial_state),
        &child_frame,
        &child_run_ctx,
        observer,
        token,
    )
    .await;

    // `meta.attempts[].children` (issue #659): whenever this attempt
    // actually ran the child and `record_children` is on, whatever it
    // got to -- a success, an `expect` failure (grafted again below,
    // identically), or the child's own run raising mid-way. Taken here
    // once, regardless of outcome, rather than re-derived at each of
    // the three return points below.
    if record_children {
        if let Ok(Some(child_prime_node)) =
            child_store.child(&child_store.root, &Value::Str(CHILD_ROOT.to_string()))
        {
            *attempt_children = Some(child_record_value(&child_store, &child_prime_node));
        }
    }

    let child_snapshot = child_store.snapshot(&child_store.root);
    let child_prime_snapshot = child_snapshot
        .as_dict()
        .and_then(|d| d.get(&Value::Str(CHILD_ROOT.to_string())))
        .cloned()
        .unwrap_or(Value::None);
    let child_errors = collect_child_errors(&child_prime_snapshot, "");
    if !child_errors.is_empty() {
        let errors_value: Vec<Value> = child_errors
            .into_iter()
            .map(|(path, error)| {
                let mut d = Dict::new();
                d.insert(Value::Str("path".to_string()), Value::Str(path));
                d.insert(Value::Str("error".to_string()), error);
                Value::Dict(d)
            })
            .collect();
        set_leaf(store, meta_node, "child_errors", Value::List(errors_value));
    }

    match run_result {
        Err(VmError::Cancelled) => return Err(AttemptProblem::Fatal(VmError::Cancelled)),
        Err(VmError::Refused {
            refusal,
            child_label,
        }) => {
            // A refusal already labeled (a deeper nested `use` child's
            // own static check, or this same check a level up) keeps
            // its own label unchanged; one still `None` (`exec::prompt::
            // execute_prompt`'s own dispatch-time `UnsupportedAdapter`
            // check, which has no child document of its own to name)
            // is claimed by *this* level -- the nearest enclosing `use`
            // that actually ran the effect that raised it (issue #498
            // ruling 4).
            return Err(AttemptProblem::Fatal(VmError::Refused {
                refusal,
                child_label: child_label.or_else(|| Some(loaded.label.clone())),
            }));
        }
        Err(other) => {
            // `core/use.py`'s own `record_children` graft for *this*
            // failure mode -- the child's own run raising mid-way --
            // only ever happens from the outer `except`, once this
            // attempt's failure has already been decided final
            // (`use.py:1121-1127`); a retryable attempt's own `except`
            // (`use.py:1082-1094`) sets `meta["error"]` and loops again
            // with no graft at all. Grafting here unconditionally, before
            // that attempt's failure is known final, would let a retried
            // attempt's own record appear on the node mid-backoff, for
            // `--live-state` to see -- `clear_child_record`'s per-attempt
            // clear (issue #659) already drops a prior attempt's stale
            // keys at the next attempt's start, but that is a different
            // moment than this one. So the record is only *collected*
            // here (a live `NodeRef`, not a `Value` snapshot, to keep
            // `graft_child_record`'s own node-identity preservation) and
            // [`execute_use`] grafts it, only once it knows this attempt
            // will not be retried.
            let child_record = if record_children {
                child_store
                    .child(&child_store.root, &Value::Str(CHILD_ROOT.to_string()))
                    .ok()
                    .flatten()
            } else {
                None
            };
            return Err(AttemptProblem::Retryable {
                message: other.to_string(),
                child_record,
            });
        }
        Ok(()) => {}
    }

    match effective_outputs {
        Some(outputs) => {
            let mut result = Dict::new();
            for (output_key, child_path) in outputs {
                result.insert(
                    Value::Str(output_key.clone()),
                    resolve_dot_path(&child_snapshot, child_path),
                );
            }
            set_leaf(store, effect_node, "value", Value::Dict(result));
            if record_children {
                if let Ok(Some(child_prime_node)) =
                    child_store.child(&child_store.root, &Value::Str(CHILD_ROOT.to_string()))
                {
                    graft_child_record(effect_node, &child_prime_node);
                }
            }
        }
        None => {
            if let Ok(Some(child_prime_node)) =
                child_store.child(&child_store.root, &Value::Str(CHILD_ROOT.to_string()))
            {
                graft_child_record(effect_node, &child_prime_node);
            }
        }
    }

    // `meta.expect` is written on every attempt that reaches a
    // successful child run, pass or fail -- `core/use.py::UseRuntime.
    // execute` sets it unconditionally before deciding whether the
    // outcome fails the attempt (`use.py:1036-1047`), mirroring `core/
    // tool.py`'s own `expect_meta` block (`tool.rs`'s own CEL/model
    // arms). A CEL evaluation error and a model-mode answer-parse
    // failure are both ordinary attempt failures (retried, never
    // fatal); a `Raised` failure not in [`PyExc::is_value_error_class`]
    // or `RecursionError` is retried too (issue #513 settled question
    // 5), and only a fatal `VmError` out of `condition_model::
    // evaluate_model_expect` (a cancellation or a refusal reached
    // while rendering/dispatching) propagates as fatal.
    if let Some(expect) = &use_op.expect {
        let value_snapshot = store.snapshot(effect_node);
        let value_for_expect = value_snapshot
            .as_dict()
            .and_then(|d| d.get(&Value::Str("value".to_string())))
            .cloned()
            .unwrap_or(Value::None);
        let meta_snapshot = store.snapshot(meta_node);
        // Deliberately *not* narrowed through `exec::live_scope::
        // build_cel_state` -- same call, same reasoning as `exec::
        // tool`'s own `expect:` state (that module's own doc comment):
        // `evaluate_expect` converts its whole `state` with no internal
        // `project` of its own, so narrowing here would silently change
        // behaviour issue #463 is not about.
        let state = ctx.whole();
        match expect {
            ExpectCondition::Cel { expr } => {
                let mut expect_meta = IndexMap::new();
                expect_meta.insert(Value::from("mode"), Value::from("cel"));
                expect_meta.insert(Value::from("expr"), Value::from(expr.as_str()));
                match electricity_cel::evaluate_expect(
                    expr,
                    &value_for_expect,
                    &meta_snapshot,
                    &state,
                ) {
                    Ok(passed) => {
                        expect_meta.insert(Value::from("result"), Value::Bool(passed));
                        set_leaf(store, meta_node, "expect", Value::Dict(expect_meta));
                        if !passed {
                            return Err(AttemptProblem::Retryable {
                                message: format!("expect failed: {expr}"),
                                child_record: None,
                            });
                        }
                    }
                    Err(cel_err) if cel_err.is_overflow() => {
                        // `core/cel_eval.py::evaluate_cel_expect` converts
                        // `value`/`meta`/`state` before its own `try`/
                        // `except`, so the matching `ValueError("overflow")`
                        // reaches `core/use.py` uncaught -- never a
                        // `CelEvaluationError`, never a failed expectation.
                        // `meta.expect` is never set (the escape happens
                        // before `core/expect.py` would set it), and
                        // `core/use.py`'s own `except (ValueError,
                        // RecursionError)` re-raises immediately, with no
                        // retry, regardless of which attempt this is.
                        return Err(AttemptProblem::NotRetried {
                            message: "overflow".to_string(),
                            py_class: PyExc::ValueError,
                        });
                    }
                    Err(cel_err) => {
                        expect_meta.insert(
                            Value::from("error"),
                            Value::from(cel_err.to_string().as_str()),
                        );
                        expect_meta.insert(Value::from("result"), Value::Bool(false));
                        set_leaf(store, meta_node, "expect", Value::Dict(expect_meta));
                        return Err(AttemptProblem::Retryable {
                            message: format!("expect failed: {expr}"),
                            child_record: None,
                        });
                    }
                }
            }
            // `condition_model::evaluate_model_expect` -- issue
            // #513. *timeout_seconds* here is the run adapter budget
            // (`run_ctx.adapter_timeout_seconds`), never a resolved
            // tool timeout -- a `use` effect has none of its own.
            // `Raised`'s own *py_class* decides the split `core/
            // use.py`'s `except (ValueError, RecursionError)` makes,
            // via `isinstance` -- so every `ValueError` subclass
            // counts, not just the bare class ([`PyExc::
            // is_value_error_class`]'s own doc comment; PR #514
            // named the real adapters'
            // `UnicodeDecodeError` for a non-UTF-8 response body as
            // the subclass this used to miss): not retried for that
            // set plus `RecursionError`, retried for everything else
            // (an adapter failure, always a `RuntimeError`/[`PyExc::
            // AdapterCallError`] class here, even a non-retryable
            // one).
            ExpectCondition::Model { template } => {
                let effect_path = frame.effect_path(&op.path);
                let summary = expect_model_summary(&template.source);
                match crate::exec::condition_model::evaluate_model_expect(
                    template,
                    &value_for_expect,
                    &meta_snapshot,
                    &state,
                    &effect_path,
                    run_ctx.adapter_timeout_seconds,
                    run_ctx,
                    token,
                )
                .await
                {
                    Ok(outcome) => {
                        set_leaf(store, meta_node, "expect", outcome.meta);
                        if !outcome.passed {
                            return Err(AttemptProblem::Retryable {
                                message: format!("expect failed: {summary}"),
                                child_record: None,
                            });
                        }
                    }
                    Err(crate::exec::condition_model::ExpectCallError::Fatal(e)) => {
                        return Err(AttemptProblem::Fatal(e));
                    }
                    Err(crate::exec::condition_model::ExpectCallError::Raised {
                        message,
                        py_class,
                    }) => {
                        if py_class.is_value_error_class() || py_class == PyExc::RecursionError {
                            return Err(AttemptProblem::NotRetried { message, py_class });
                        }
                        return Err(AttemptProblem::Retryable {
                            message,
                            child_record: None,
                        });
                    }
                }
            }
        }
    }

    set_leaf(store, meta_node, "completed_at", now_iso());
    Ok(())
}

/// `core/use.py::UseRuntime.execute` -- see this module's own doc
/// comment for the overall design. No longer a `live_ctx` snapshot
/// taken eagerly here (issue #463, PR #482's review): a `use`
/// effect's own input render/`expect:` call sites resolve
/// `live_scope::LiveRoot` lazily, the same way `exec::tool` already
/// does, so a `use` effect inside a loop is O(N) total, not O(N^2).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_use(
    op: &Op,
    use_op: &UseOp,
    store: &Store,
    parent: &NodeRef,
    ctx_chain: &CtxChain,
    frame: &Frame,
    run_ctx: &RunContext<'_>,
    observer: &dyn RunObserver,
    token: &CancellationToken,
) -> Result<(), VmError> {
    let live_root = LiveRoot::new(ctx_chain, store);
    execute_use_with_ctx(
        op, use_op, store, parent, &live_root, frame, run_ctx, observer, token,
    )
    .await
}

/// [`execute_use`]'s own real body, generic over *ctx*'s own
/// [`ParamsCtx`] rather than requiring an already-resolved [`Value`]
/// (`R = Value` is still exactly that, so every existing caller --
/// `tests/use_corpus.rs`'s golden-corpus replay, from PR #472's review,
/// which cannot name a [`CtxChain`]/[`CtxSource`] of its own:
/// both are `pub(crate)` to `exec/mod.rs` -- keeps compiling and
/// behaving unchanged). `core/use.py`'s own `UseRuntime.execute(store,
/// ctx)` takes *ctx* the exact same reference way; this function is
/// the closer match to replay the corpus against, not a test-only
/// detour.
#[allow(clippy::too_many_arguments)]
pub async fn execute_use_with_ctx<R: ParamsCtx>(
    op: &Op,
    use_op: &UseOp,
    store: &Store,
    parent: &NodeRef,
    ctx: &R,
    frame: &Frame,
    run_ctx: &RunContext<'_>,
    observer: &dyn RunObserver,
    token: &CancellationToken,
) -> Result<(), VmError> {
    let name = op
        .name
        .as_deref()
        .expect("a use effect's own Op always carries a name");

    let effect_node = store
        .ensure_dict(parent, Value::Str(name.to_string()))
        .map_err(store_err)?;
    if !effect_node
        .borrow()
        .contains_key(&Value::Str("value".to_string()))
    {
        set_leaf(store, &effect_node, "value", Value::None);
    }
    let meta_node = store
        .ensure_dict(&effect_node, Value::Str("meta".to_string()))
        .map_err(store_err)?;

    let legacy_ref = match &use_op.source {
        UseSource::Path(s) | UseSource::Orchestration(s) => Some(s.clone()),
        UseSource::Inline(_) => None,
    };
    let is_inline = matches!(use_op.source, UseSource::Inline(_));
    let mut label = legacy_ref.clone().unwrap_or_else(|| "inline".to_string());

    set_leaf(store, &meta_node, "created_at", now_iso());
    set_leaf(store, &meta_node, "completed_at", Value::None);
    set_leaf(
        store,
        &meta_node,
        "orchestration",
        legacy_ref.clone().map(Value::Str).unwrap_or(Value::None),
    );
    set_leaf(store, &meta_node, "inline", Value::Bool(is_inline));
    set_leaf(store, &meta_node, "resolved_path", Value::None);
    set_leaf(store, &meta_node, "validation_errors", Value::None);
    set_leaf(store, &meta_node, "error", Value::None);
    set_leaf(store, &meta_node, "child_errors", Value::None);
    set_leaf(store, &meta_node, "inputs", Value::None);
    set_leaf(store, &meta_node, "orchestration_sha256", Value::None);
    pop_leaf(&meta_node, "expect");
    pop_leaf(&meta_node, "retries_used");
    pop_leaf(&meta_node, "attempts");

    let effect_path = frame.effect_path(&op.path);
    let instance: InstanceId = observer.effect_start(&effect_path);

    if run_ctx.dry_run {
        set_leaf(store, &meta_node, "completed_at", now_iso());
        set_leaf(store, &meta_node, "dry_run", Value::Bool(true));
        observer.effect_complete(
            &effect_path,
            instance,
            None,
            super::completed_at_is_truthy(store, &meta_node),
        );
        observer.write();
        return Ok(());
    }

    let record_children = record_children_enabled(run_ctx.runtime_config);

    let max_attempts = use_op.retries.max_attempts.max(1);
    let base_backoff_ms = use_op.retries.backoff_ms;
    let mut next_delay_ms = base_backoff_ms;
    let mut attempts: Vec<Value> = Vec::new();

    for attempt_index in 0..max_attempts {
        if attempt_index > 0 {
            if token.is_set() {
                return Err(VmError::Cancelled);
            }
            tokio::select! {
                () = tokio::time::sleep(std::time::Duration::from_millis(u64::from(next_delay_ms))) => {}
                () = token.cancelled() => return Err(VmError::Cancelled),
            }
        }

        set_leaf(store, &effect_node, "value", Value::None);
        if attempt_index > 0 {
            // The previous attempt's child effects: kept in its own
            // `meta.attempts` entry below when `record_children` asks,
            // never left under the node for this attempt's to mix with
            // (issue #659).
            clear_child_record(&effect_node);
        }
        let attempt_started_at = now_iso();
        if attempt_index == 0 {
            set_leaf(store, &meta_node, "created_at", attempt_started_at.clone());
        }
        set_leaf(store, &meta_node, "completed_at", Value::None);
        set_leaf(store, &meta_node, "error", Value::None);
        set_leaf(store, &meta_node, "child_errors", Value::None);
        set_leaf(store, &meta_node, "inputs", Value::None);
        set_leaf(store, &meta_node, "orchestration_sha256", Value::None);
        pop_leaf(&meta_node, "expect");
        pop_leaf(&meta_node, "retries_used");
        observer.write();

        let mut attempt_children: Option<Value> = None;
        let attempt_result = attempt_once(
            op,
            use_op,
            name,
            &effect_node,
            &meta_node,
            ctx,
            frame,
            run_ctx,
            observer,
            token,
            store,
            &mut label,
            record_children,
            &mut attempt_children,
        )
        .await;

        let entry_error = match &attempt_result {
            Ok(()) => None,
            Err(AttemptProblem::Fatal(e)) => Some(e.to_string()),
            Err(AttemptProblem::NotRetried { message, .. })
            | Err(AttemptProblem::Retryable { message, .. }) => Some(message.clone()),
        };
        let entry_completed_at = if attempt_result.is_ok() {
            read_key(store, &meta_node, "completed_at")
        } else {
            now_iso()
        };
        let entry_children = if record_children {
            Some(
                attempt_children
                    .take()
                    .unwrap_or_else(|| Value::Dict(Dict::new())),
            )
        } else {
            None
        };
        attempts.push(attempt_entry(
            store,
            &effect_node,
            &meta_node,
            attempt_started_at,
            entry_completed_at,
            entry_error,
            entry_children,
        ));

        let (message, py_class, child_record) = match attempt_result {
            Ok(()) => {
                if attempt_index > 0 {
                    set_leaf(
                        store,
                        &meta_node,
                        "retries_used",
                        Value::from(i64::from(attempt_index)),
                    );
                    write_attempts(store, &meta_node, &attempts);
                }
                observer.effect_complete(
                    &effect_path,
                    instance,
                    None,
                    super::completed_at_is_truthy(store, &meta_node),
                );
                observer.write();
                return Ok(());
            }
            Err(AttemptProblem::Fatal(VmError::Cancelled)) => return Err(VmError::Cancelled),
            Err(AttemptProblem::Fatal(refused @ VmError::Refused { .. })) => {
                let text = refused.to_string();
                set_leaf(store, &meta_node, "error", Value::Str(text.clone()));
                let completed_at = now_iso();
                set_leaf(store, &meta_node, "completed_at", completed_at.clone());
                if attempt_index > 0 {
                    set_leaf(
                        store,
                        &meta_node,
                        "retries_used",
                        Value::from(i64::from(attempt_index)),
                    );
                    if let Some(Value::Dict(last)) = attempts.last_mut() {
                        last.insert(Value::from("completed_at"), completed_at);
                    }
                    write_attempts(store, &meta_node, &attempts);
                }
                observer.effect_complete(
                    &effect_path,
                    instance,
                    Some(&text),
                    super::completed_at_is_truthy(store, &meta_node),
                );
                observer.write();
                return Err(refused);
            }
            Err(AttemptProblem::Fatal(other)) => return Err(other),
            Err(AttemptProblem::NotRetried { message, py_class }) => (message, py_class, None),
            Err(AttemptProblem::Retryable {
                message,
                child_record,
            }) => (message, PyExc::RuntimeError, child_record),
        };

        set_leaf(store, &meta_node, "error", Value::Str(message.clone()));
        let is_last_attempt = attempt_index + 1 >= max_attempts;
        // [`PyExc::is_value_error_class`] covers every `ValueError`
        // subclass Python's own `isinstance` check would (issue
        // #513; PR #514), not just the bare
        // class -- `RecursionError` itself is a `RuntimeError`
        // subclass, checked separately.
        let retryable = !(py_class.is_value_error_class() || py_class == PyExc::RecursionError);
        if !is_last_attempt && retryable {
            // The finished attempts, readable (and on --live-state)
            // before the next one starts (issue #659).
            write_attempts(store, &meta_node, &attempts);
            observer.write();
            next_delay_ms = crate::retry::next_backoff_delay_ms(
                &crate::retry::RetryInfo::retryable(),
                attempt_index,
                base_backoff_ms,
            );
            continue;
        }

        // `record_children`'s own failure-path graft lands here, never
        // above the `continue` -- `core/use.py`'s own outer `except`
        // (`use.py:1121-1127`) only ever reaches `_graft_child_record`
        // once an attempt's failure is final, exactly this point.
        if let Some(child_record) = &child_record {
            graft_child_record(&effect_node, child_record);
        }

        let completed_at = now_iso();
        set_leaf(store, &meta_node, "completed_at", completed_at.clone());
        if attempt_index > 0 {
            set_leaf(
                store,
                &meta_node,
                "retries_used",
                Value::from(i64::from(attempt_index)),
            );
            if let Some(Value::Dict(last)) = attempts.last_mut() {
                last.insert(Value::from("completed_at"), completed_at);
            }
            write_attempts(store, &meta_node, &attempts);
        }
        if message.to_lowercase().contains("validation failed") {
            set_leaf(
                store,
                &meta_node,
                "validation_errors",
                Value::Str(message.clone()),
            );
        }

        return match op.on_error {
            OnError::Fail => {
                let wrapped = format!("use '{name}' -> {label}: {message}");
                observer.effect_complete(
                    &effect_path,
                    instance,
                    Some(&message),
                    super::completed_at_is_truthy(store, &meta_node),
                );
                observer.write();
                Err(VmError::Raised {
                    py_class: PyExc::RuntimeError,
                    message: wrapped,
                    cause: electricity_tools::ErrorCause::None,
                    cause_class: Some(py_class),
                })
            }
            OnError::Skip | OnError::Continue => {
                set_leaf(store, &effect_node, "value", Value::None);
                observer.effect_complete(
                    &effect_path,
                    instance,
                    Some(&message),
                    super::completed_at_is_truthy(store, &meta_node),
                );
                observer.write();
                Ok(())
            }
            OnError::Break => unreachable!("a use effect's own on_error is never Break"),
        };
    }
    unreachable!("the attempt loop always returns on its last iteration")
}

/// Wraps whatever [`RunObserver`] [`run_isolated_inner`] was handed:
/// drops the child root's own `effect_start`/`effect_complete` pair
/// (this module's own doc comment) and turns every `write()` signal
/// into a [`RunObserver::graft_child_write`] call carrying the child's
/// own current [`Store::saved`] snapshot. Every other hook forwards
/// unchanged.
struct ChildObserver<'a> {
    parent: &'a dyn RunObserver,
    root_path: EffectPath,
    child_store: &'a Store,
    /// Throttles [`RunObserver::write`]'s own snapshot-and-graft pair
    /// (PR #472): a long loop inside the child
    /// calls `write()` once per leaf write, and a full [`Store::saved`]
    /// snapshot is `O(size of the child tree so far)` -- taking one on
    /// *every* call would make a child with an `N`-pass loop pay
    /// `O(N^2)` overall, for live-state content nothing ever reads
    /// faster than [`CHILD_GRAFT_INTERVAL`] anyway (the real mirror's
    /// own flush is throttled the same way: `electricity`'s own
    /// `LiveStateMirror::flush_if_due`). `None` until the first call,
    /// which always grafts immediately.
    last_graft: std::cell::Cell<Option<std::time::Instant>>,
}

/// [`ChildObserver::write`]'s own throttle window -- deliberately not
/// shared with `electricity` (lib)'s own `LiveStateMirror::
/// LIVE_STATE_INTERVAL` (that crate isn't a dependency of this one);
/// a different, shorter value is fine, since both ends only promise
/// "eventually catches up", never a fixed cadence of their own.
const CHILD_GRAFT_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

impl RunObserver for ChildObserver<'_> {
    fn effect_start(&self, path: &EffectPath) -> InstanceId {
        if *path == self.root_path {
            return 0;
        }
        self.parent.effect_start(path)
    }

    fn effect_complete(
        &self,
        path: &EffectPath,
        instance: InstanceId,
        error: Option<&str>,
        completed_at_is_truthy: bool,
    ) {
        if *path == self.root_path {
            return;
        }
        self.parent
            .effect_complete(path, instance, error, completed_at_is_truthy);
    }

    // A child loop's `dispatch` must carry the same absolute path its
    // own `start`/`end` events use (#483, matching `core/use.py`'s
    // `_namespaced_dispatch_cb`) -- and like every other hook here,
    // *path* already is that path: it comes from this crate's own
    // child [`Frame`], which composes every path through
    // [`Frame::effect_path`], already namespaced under the `use` node.
    // No rewriting needed; forward it unchanged.
    fn dispatch(&self, path: &EffectPath, branches: usize, concurrency: usize) {
        self.parent.dispatch(path, branches, concurrency);
    }

    fn write(&self) {
        // Ruling R4 on PR #472's review: without a `--live-state`
        // mirror (or any other caller that actually reads a graft),
        // `graft_child_write` is a no-op, so the `Store::saved` snapshot
        // below -- an `O(size of the child tree so far)` recursive
        // clone -- is never worth taking at all, not even throttled.
        if !self.parent.wants_child_snapshots() {
            return;
        }
        let now = std::time::Instant::now();
        let due = match self.last_graft.get() {
            None => true,
            Some(last) => now.duration_since(last) >= CHILD_GRAFT_INTERVAL,
        };
        if !due {
            return;
        }
        self.last_graft.set(Some(now));
        let snapshot = self.child_store.saved(&self.child_store.root);
        self.parent.graft_child_write(&self.root_path, &snapshot);
    }

    fn warning(&self, message: &str) {
        self.parent.warning(message);
    }

    // A report's own path is already namespaced under the `use` node
    // the same way `dispatch`'s own path is (`Frame::effect_path`) --
    // no rewriting needed, same as `dispatch`/`warning` above.
    fn report(
        &self,
        path: &EffectPath,
        level: electricity_bytecode::ReportLevel,
        message: &str,
        data: Option<&Value>,
    ) {
        self.parent.report(path, level, message, data);
    }

    fn record_totals(&self, meta: &Value) {
        self.parent.record_totals(meta);
    }

    fn graft_child_write(&self, node_path: &EffectPath, snapshot: &Value) {
        self.parent.graft_child_write(node_path, snapshot);
    }

    fn clear_graft(&self, node_path: &EffectPath) {
        self.parent.clear_graft(node_path);
    }

    // Issue #465: a tree nested inside this `use` child registers its
    // own branches at its own, already-namespaced path (same reasoning
    // as `dispatch`, above) -- the branch `NodeRef`s themselves belong
    // to `self.child_store`, not the parent run's store, but
    // `RunObserver::register_tree_branches`'s own contract only ever
    // reads one lazily, through whichever `Store` the caller happens
    // to resolve it with (`Store::saved` doesn't depend on `self`),
    // so forwarding the `NodeRef`s unchanged is correct regardless of
    // which store they came from.
    fn register_tree_branches(&self, node_path: &EffectPath, branches: &[NodeRef]) {
        self.parent.register_tree_branches(node_path, branches);
    }

    fn clear_tree_branches(&self, node_path: &EffectPath, branches: &[NodeRef]) {
        self.parent.clear_tree_branches(node_path, branches);
    }
}

/// [`run_isolated`]'s own private body -- see this module's own doc
/// comment for why *execute_use* calls this directly instead of the
/// public, signature-fixed wrapper.
async fn run_isolated_inner(
    program: &Program,
    initial_state: Value,
    frame: &Frame,
    run_ctx: &RunContext<'_>,
    observer: &dyn RunObserver,
    token: &CancellationToken,
) -> (Store, Result<(), VmError>) {
    let child_store = Store::new();
    let mut initial_state = initial_state;
    if let Value::Dict(dict) = &mut initial_state {
        for (key, value) in std::mem::take(dict) {
            child_store.set_leaf(&child_store.root, key, value);
        }
    }
    let root_path = frame.effect_path(&EffectPath::root());
    let wrapped = ChildObserver {
        parent: observer,
        root_path: root_path.clone(),
        child_store: &child_store,
        last_graft: std::cell::Cell::new(None),
    };
    let result = crate::exec::dynamic::execute_dynamic(
        &program.root,
        &child_store,
        &child_store.root,
        &Vec::new(),
        frame,
        run_ctx,
        &wrapped,
        token,
    )
    .await;
    observer.clear_graft(&root_path);
    (child_store, result)
}

/// Runs *program* against a fresh, isolated [`Store`] seeded with
/// *initial_state* (never the parent's own store) -- there is no
/// `core/use.py::_run_isolated` to port this from: `UseRuntime.execute`
/// builds its own child `Store` inline, with no separate function of
/// its own. Shared by `use` (this lane) and decomposition (lane L2).
pub async fn run_isolated(
    program: &Program,
    initial_state: Value,
    frame: &Frame,
    run_ctx: &RunContext<'_>,
    observer: &dyn RunObserver,
    token: &CancellationToken,
) -> Result<Store, VmError> {
    let (store, result) =
        run_isolated_inner(program, initial_state, frame, run_ctx, observer, token).await;
    result.map(|()| store)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::CtxSource;
    use crate::observer::NullObserver;
    use electricity_bytecode::{LeafKind, NodeKind};
    use electricity_config::ComplexitySettings;
    use std::collections::BTreeSet;
    use std::rc::Rc;

    /// PR #472 (ruling R5): a
    /// retryable child-run failure must *collect* its own
    /// `record_children` record, never apply it -- only [`execute_use`]
    /// applies it, and only once it knows this attempt is the last one.
    /// This white-box test calls [`attempt_once`] directly (its own
    /// private items aren't reachable from the `tests/` integration
    /// suite) against an inline child that always fails deterministically
    /// (`json` tool, `mode: parse` on malformed input, `on_error: fail`
    /// so the child's own run itself raises) and checks both halves of
    /// the contract: the failure is retryable and carries the child's
    /// own record, and *effect_node* -- the use node [`execute_use`]
    /// would later graft onto -- was not mutated by `attempt_once`
    /// itself.
    #[tokio::test]
    async fn a_retryable_child_failure_collects_its_own_record_children_without_applying_it() {
        let inline_yaml = "\
effects:\n  - type: tool\n    name: inner\n    provider: json\n    params: {mode: parse, input: \"not json\"}\n";
        let use_op = UseOp {
            parent_trusted: false,
            source: UseSource::Inline(electricity_bytecode::TemplateText::new(
                inline_yaml.to_string(),
                true,
                electricity_bytecode::Escape::Html,
            )),
            inputs: None,
            outputs: None,
            validate: true,
            retries: electricity_bytecode::RetryPolicy {
                max_attempts: 2,
                backoff_ms: 0,
            },
            expect: None,
            description: None,
        };
        let op = Op {
            path: EffectPath::root().push_name("step"),
            name: Some("step".to_string()),
            kind: NodeKind::Leaf(Box::new(LeafKind::Use(use_op.clone()))),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };

        let store = Store::new();
        let effect_node = store
            .ensure_dict(&store.root, Value::Str("step".to_string()))
            .unwrap();
        set_leaf(&store, &effect_node, "value", Value::None);
        let meta_node = store
            .ensure_dict(&effect_node, Value::Str("meta".to_string()))
            .unwrap();

        let mut registry = electricity_tools::ToolRegistry::new();
        registry.register(Box::new(electricity_tools::json::JsonTool));
        let limiter = crate::Limiter::new();
        let runtime_config = Value::None;
        let adapters = crate::adapter::AdapterRegistry::new();
        let complexity = ComplexitySettings::default();
        let orchestration_dir = std::path::Path::new(".");
        let declared_prompts = indexmap::IndexMap::new();
        let effect_names = BTreeSet::new();
        let run_ctx = RunContext {
            registry: &registry,
            limiter: &limiter,
            model: "",
            adapter: "_noop",
            runtime_config: &runtime_config,
            dry_run: false,
            armed: false,
            adapters: &adapters,
            default_adapter: Rc::new(crate::adapter::NoopAdapter),
            model_locked: false,
            adapter_timeout_seconds: 120,
            complexity: &complexity,
            decomposition_depth: 0,
            orchestration_dir,
            declared_prompts: &declared_prompts,
            effect_names: &effect_names,
            supported: electricity_bytecode::Supported::m0(&[]),
        };
        let frame = Frame::root();
        let observer = NullObserver;
        let token = CancellationToken::new();
        let mut label = "inline".to_string();

        let result = attempt_once(
            &op,
            &use_op,
            "step",
            &effect_node,
            &meta_node,
            &Value::None,
            &frame,
            &run_ctx,
            &observer,
            &token,
            &store,
            &mut label,
            true, // record_children
            &mut None,
        )
        .await;

        let AttemptProblem::Retryable { child_record, .. } = result.expect_err(
            "a child whose own run fails must be a retryable AttemptProblem, not applied here",
        ) else {
            panic!("expected AttemptProblem::Retryable");
        };
        assert!(
            child_record.is_some(),
            "record_children was true and the child did run, so a record must be collected"
        );
        assert!(
            effect_node
                .borrow()
                .get(&Value::Str("inner".to_string()))
                .is_none(),
            "attempt_once must never graft record_children itself -- only execute_use, \
             once it knows this attempt is final, may"
        );

        // The collected record is still real, live child data -- grafting
        // it by hand (exactly what `execute_use` does once an attempt is
        // final) does carry the child's own content.
        graft_child_record(&effect_node, &child_record.unwrap());
        assert!(
            effect_node
                .borrow()
                .contains_key(&Value::Str("inner".to_string())),
            "the collected record must be the real child node, graftable by the caller"
        );
    }

    /// Issue #400: `value.result` is too large for CEL's 64-bit `int`
    /// -- `evaluate_cel_expect`'s own `_to_cel` conversion raises a bare
    /// `ValueError("overflow")` before Circuitry's own `evaluate_expect`
    /// ever gets a `CelEvaluationError` to catch, and `core/use.py`'s
    /// own `except (ValueError, RecursionError)` re-raises it
    /// immediately, with no retry regardless of `retries.max_attempts` --
    /// unlike `core/tool.py`'s attempt loop, which retries the same
    /// exception like any other.
    #[tokio::test]
    async fn an_overflowing_cel_expect_is_not_retried() {
        let inline_yaml = "effects:\n  - type: tool\n    name: inner\n    provider: json\n    params: {mode: parse, input: \"1180591620717411303424\"}\n";
        let mut outputs = indexmap::IndexMap::new();
        outputs.insert("result".to_string(), "prime.inner.value".to_string());
        let use_op = UseOp {
            parent_trusted: false,
            source: UseSource::Inline(electricity_bytecode::TemplateText::new(
                inline_yaml.to_string(),
                true,
                electricity_bytecode::Escape::Html,
            )),
            inputs: None,
            outputs: Some(outputs),
            validate: true,
            retries: electricity_bytecode::RetryPolicy {
                max_attempts: 3,
                backoff_ms: 0,
            },
            expect: Some(ExpectCondition::Cel {
                expr: "value.result == value.result".to_string(),
            }),
            description: None,
        };
        let op = Op {
            path: EffectPath::root().push_name("step"),
            name: Some("step".to_string()),
            kind: NodeKind::Leaf(Box::new(LeafKind::Use(use_op.clone()))),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };

        let store = Store::new();
        let effect_node = store
            .ensure_dict(&store.root, Value::Str("step".to_string()))
            .unwrap();
        set_leaf(&store, &effect_node, "value", Value::None);
        let meta_node = store
            .ensure_dict(&effect_node, Value::Str("meta".to_string()))
            .unwrap();

        let mut registry = electricity_tools::ToolRegistry::new();
        registry.register(Box::new(electricity_tools::json::JsonTool));
        let limiter = crate::Limiter::new();
        let runtime_config = Value::None;
        let adapters = crate::adapter::AdapterRegistry::new();
        let complexity = ComplexitySettings::default();
        let orchestration_dir = std::path::Path::new(".");
        let declared_prompts = indexmap::IndexMap::new();
        let effect_names = BTreeSet::new();
        let run_ctx = RunContext {
            registry: &registry,
            limiter: &limiter,
            model: "",
            adapter: "_noop",
            runtime_config: &runtime_config,
            dry_run: false,
            armed: false,
            adapters: &adapters,
            default_adapter: Rc::new(crate::adapter::NoopAdapter),
            model_locked: false,
            adapter_timeout_seconds: 120,
            complexity: &complexity,
            decomposition_depth: 0,
            orchestration_dir,
            declared_prompts: &declared_prompts,
            effect_names: &effect_names,
            supported: electricity_bytecode::Supported::m0(&[]),
        };
        let frame = Frame::root();
        let observer = NullObserver;
        let token = CancellationToken::new();
        let mut label = "inline".to_string();

        let result = attempt_once(
            &op,
            &use_op,
            "step",
            &effect_node,
            &meta_node,
            &Value::None,
            &frame,
            &run_ctx,
            &observer,
            &token,
            &store,
            &mut label,
            false,
            &mut None,
        )
        .await;

        let AttemptProblem::NotRetried { message, py_class } =
            result.expect_err("an overflowing expect must be an AttemptProblem")
        else {
            panic!("expected AttemptProblem::NotRetried");
        };
        assert_eq!(message, "overflow");
        assert_eq!(py_class, electricity_tools::PyExc::ValueError);
        assert!(
            !meta_node
                .borrow()
                .contains_key(&Value::Str("expect".to_string())),
            "meta.expect must never be set for an overflowing expect"
        );
    }

    /// A fixed-error adapter for the model-expect classification tests
    /// below -- `AdapterError::py_class` is the only thing those tests
    /// care about.
    struct FailingAdapter {
        py_class: electricity_tools::PyExc,
    }

    #[async_trait::async_trait(?Send)]
    impl crate::adapter::Adapter for FailingAdapter {
        fn name(&self) -> &str {
            "fake"
        }

        async fn generate(
            &self,
            _model: &str,
            _prompt: &str,
            _options: &crate::adapter::GenerateOptions,
            _call: &crate::adapter::CallContext<'_>,
        ) -> Result<crate::adapter::GenerateResult, crate::adapter::AdapterError> {
            Err(crate::adapter::AdapterError {
                message: "boom".to_string(),
                retry: crate::retry::RetryInfo::not_retryable(),
                py_class: self.py_class,
            })
        }

        async fn check(&self) -> crate::adapter::CheckResult {
            crate::adapter::CheckResult {
                ok: true,
                ..Default::default()
            }
        }
    }

    /// Captures the *timeout_seconds* a `use` effect's own model
    /// `expect:` dispatches with, replying `"yes"` so the attempt
    /// succeeds -- issue #513's review: a `use` effect has no
    /// timeout of its own, so this call site must forward
    /// `run_ctx.adapter_timeout_seconds` verbatim, never a tool-shaped
    /// resolved value (that's `tool.rs`'s own call site instead).
    #[derive(Default)]
    struct TimeoutCapturingAdapter {
        seen: std::cell::Cell<Option<u32>>,
    }

    #[async_trait::async_trait(?Send)]
    impl crate::adapter::Adapter for TimeoutCapturingAdapter {
        fn name(&self) -> &str {
            "fake"
        }

        async fn generate(
            &self,
            _model: &str,
            _prompt: &str,
            _options: &crate::adapter::GenerateOptions,
            call: &crate::adapter::CallContext<'_>,
        ) -> Result<crate::adapter::GenerateResult, crate::adapter::AdapterError> {
            self.seen.set(Some(call.timeout_seconds));
            Ok(crate::adapter::GenerateResult {
                text: "yes".to_string(),
                raw: Value::None,
                tokens_sent: Some(1),
                tokens_received: Some(1),
                finish_reason: None,
                warnings: Vec::new(),
                cost_usd: None,
            })
        }

        async fn check(&self) -> crate::adapter::CheckResult {
            crate::adapter::CheckResult {
                ok: true,
                ..Default::default()
            }
        }
    }

    /// Issue #513's review: the outer `execute_use_with_ctx`
    /// retry check, not just `attempt_once`'s own `Raised`
    /// classification, must not retry a `TemplateError` -- a render
    /// failure in the expect template itself (subscripting an `int`
    /// like a list, the same trigger `exec::prompt`'s own render-
    /// failure tests use), with `retries.max_attempts: 2` so a retry
    /// would be observable as a second attempt (`meta.retries_used`
    /// set).
    #[tokio::test]
    async fn a_uses_model_expect_template_error_is_not_retried_by_the_outer_loop() {
        let inline_yaml = "effects:\n  - type: tool\n    name: inner\n    provider: json\n    params: {mode: stringify, input: 1}\n";
        let use_op = UseOp {
            parent_trusted: false,
            source: UseSource::Inline(electricity_bytecode::TemplateText::new(
                inline_yaml.to_string(),
                true,
                electricity_bytecode::Escape::Html,
            )),
            inputs: None,
            outputs: None,
            validate: true,
            retries: electricity_bytecode::RetryPolicy {
                max_attempts: 2,
                backoff_ms: 0,
            },
            expect: Some(ExpectCondition::Model {
                template: electricity_bytecode::TemplateText::new(
                    "{{value.0}} ok?",
                    false,
                    electricity_bytecode::Escape::Html,
                ),
            }),
            description: None,
        };
        let op = Op {
            path: EffectPath::root().push_name("step"),
            name: Some("step".to_string()),
            kind: NodeKind::Leaf(Box::new(LeafKind::Use(use_op.clone()))),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };

        let store = Store::new();
        let mut registry = electricity_tools::ToolRegistry::new();
        registry.register(Box::new(electricity_tools::json::JsonTool));
        let limiter = crate::Limiter::new();
        let runtime_config = Value::None;
        let adapters = crate::adapter::AdapterRegistry::new();
        let complexity = ComplexitySettings::default();
        let orchestration_dir = std::path::Path::new(".");
        let declared_prompts = indexmap::IndexMap::new();
        let effect_names = BTreeSet::new();
        let run_ctx = RunContext {
            registry: &registry,
            limiter: &limiter,
            model: "m",
            adapter: "fake",
            runtime_config: &runtime_config,
            dry_run: false,
            armed: false,
            adapters: &adapters,
            default_adapter: Rc::new(crate::adapter::NoopAdapter),
            model_locked: false,
            adapter_timeout_seconds: 120,
            complexity: &complexity,
            decomposition_depth: 0,
            orchestration_dir,
            declared_prompts: &declared_prompts,
            effect_names: &effect_names,
            supported: electricity_bytecode::Supported::m0(&[]),
        };
        let frame = Frame::root();
        let observer = NullObserver;
        let token = CancellationToken::new();

        let err = execute_use_with_ctx(
            &op,
            &use_op,
            &store,
            &store.root,
            &Value::None,
            &frame,
            &run_ctx,
            &observer,
            &token,
        )
        .await
        .expect_err("a TemplateError-class expect failure must still fail the use effect");

        let VmError::Raised { cause_class, .. } = err else {
            panic!("expected VmError::Raised, got {err:?}")
        };
        assert_eq!(
            cause_class,
            Some(PyExc::TemplateError),
            "the outer loop must classify the final failure as TemplateError"
        );

        let snapshot = store.snapshot(&store.root);
        let step = snapshot
            .as_dict()
            .unwrap()
            .get(&Value::from("step"))
            .unwrap()
            .as_dict()
            .unwrap();
        let meta = step.get(&Value::from("meta")).unwrap().as_dict().unwrap();
        assert_eq!(
            meta.get(&Value::from("retries_used")),
            None,
            "a TemplateError must not be retried, so there is never a second attempt"
        );
    }

    #[tokio::test]
    async fn a_uses_model_expect_dispatches_with_the_run_adapter_budget() {
        let inline_yaml = "effects:\n  - type: tool\n    name: inner\n    provider: json\n    params: {mode: stringify, input: 1}\n";
        let use_op = UseOp {
            parent_trusted: false,
            source: UseSource::Inline(electricity_bytecode::TemplateText::new(
                inline_yaml.to_string(),
                true,
                electricity_bytecode::Escape::Html,
            )),
            inputs: None,
            outputs: None,
            validate: true,
            retries: electricity_bytecode::RetryPolicy {
                max_attempts: 1,
                backoff_ms: 0,
            },
            expect: Some(ExpectCondition::Model {
                template: electricity_bytecode::TemplateText::new(
                    "ok?",
                    false,
                    electricity_bytecode::Escape::Html,
                ),
            }),
            description: None,
        };
        let op = Op {
            path: EffectPath::root().push_name("step"),
            name: Some("step".to_string()),
            kind: NodeKind::Leaf(Box::new(LeafKind::Use(use_op.clone()))),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };

        let store = Store::new();
        let effect_node = store
            .ensure_dict(&store.root, Value::Str("step".to_string()))
            .unwrap();
        set_leaf(&store, &effect_node, "value", Value::None);
        let meta_node = store
            .ensure_dict(&effect_node, Value::Str("meta".to_string()))
            .unwrap();

        let mut registry = electricity_tools::ToolRegistry::new();
        registry.register(Box::new(electricity_tools::json::JsonTool));
        let limiter = crate::Limiter::new();
        let runtime_config = Value::None;
        let adapters = crate::adapter::AdapterRegistry::new();
        let complexity = ComplexitySettings::default();
        let orchestration_dir = std::path::Path::new(".");
        let declared_prompts = indexmap::IndexMap::new();
        let effect_names = BTreeSet::new();
        let adapter = Rc::new(TimeoutCapturingAdapter::default());
        let run_ctx = RunContext {
            registry: &registry,
            limiter: &limiter,
            model: "m",
            adapter: "fake",
            runtime_config: &runtime_config,
            dry_run: false,
            armed: false,
            adapters: &adapters,
            default_adapter: adapter.clone(),
            model_locked: false,
            adapter_timeout_seconds: 77,
            complexity: &complexity,
            decomposition_depth: 0,
            orchestration_dir,
            declared_prompts: &declared_prompts,
            effect_names: &effect_names,
            supported: electricity_bytecode::Supported::m0(&[]),
        };
        let frame = Frame::root();
        let observer = NullObserver;
        let token = CancellationToken::new();
        let mut label = "inline".to_string();

        let result = attempt_once(
            &op,
            &use_op,
            "step",
            &effect_node,
            &meta_node,
            &Value::None,
            &frame,
            &run_ctx,
            &observer,
            &token,
            &store,
            &mut label,
            false,
            &mut None,
        )
        .await;
        assert!(result.is_ok(), "a model expect answered 'yes' must succeed");

        assert_eq!(
            adapter.seen.get(),
            Some(77),
            "a use effect's own model expect must dispatch with run_ctx.adapter_timeout_seconds, \
             never a tool-shaped resolved timeout"
        );
    }

    /// Issue #513's settled question 5: `core/use.py`'s own `except
    /// (ValueError, RecursionError)` split -- a raised model-expect
    /// failure is retried unless its own Python class is one of that
    /// pair. An `AdapterCallError` (what electricity's own real
    /// adapters raise for a transport-level failure, e.g.
    /// `electricity_adapters::openai`'s own non-zero-exit curl
    /// outcome) is retried even though that failure's own `RetryInfo`
    /// can be `not_retryable()` (`classify_curl_outcome`) -- the
    /// retry/fail split for an `expect:`'s own raised failure is
    /// decided by exception class alone, never by `RetryInfo`. A
    /// malformed response body (`non_json_error` and its siblings)
    /// raises plain `RuntimeError` instead, also retried here, since
    /// neither class is in [`PyExc::is_value_error_class`]'s set.
    #[tokio::test]
    async fn a_model_expects_adaptercallerror_failure_is_retried_despite_not_retryable() {
        let problem = model_expect_attempt_problem(electricity_tools::PyExc::AdapterCallError)
            .await
            .expect_err("an AdapterCallError-class failure must be an AttemptProblem");
        assert!(
            matches!(problem, AttemptProblem::Retryable { .. }),
            "expected Retryable"
        );
    }

    /// The other half of the same split: a `ValueError`-class failure
    /// (a template render failure's own `TemplateError` counts as one,
    /// that variant's own doc comment) is never retried.
    #[tokio::test]
    async fn a_model_expects_valueerror_failure_is_not_retried() {
        let problem = model_expect_attempt_problem(electricity_tools::PyExc::ValueError)
            .await
            .expect_err("a ValueError-class failure must be an AttemptProblem");
        assert!(
            matches!(problem, AttemptProblem::NotRetried { .. }),
            "expected NotRetried"
        );
    }

    /// PR #514: `UnicodeDecodeError` is a
    /// `ValueError` subclass in Python
    /// ([`PyExc::is_value_error_class`]'s own doc comment) -- the
    /// class electricity's real adapters raise for a response body
    /// that isn't valid UTF-8 (`transport_failure_error` in
    /// `electricity_adapters::openai`/`openai_compat`/`ollama`/
    /// `anthropic`/`replicate`/`watsonx`). Not retried, like the
    /// bare `ValueError` case above.
    #[tokio::test]
    async fn a_model_expects_unicodedecodeerror_failure_is_not_retried() {
        let problem = model_expect_attempt_problem(electricity_tools::PyExc::UnicodeDecodeError)
            .await
            .expect_err("a UnicodeDecodeError-class failure must be an AttemptProblem");
        assert!(
            matches!(problem, AttemptProblem::NotRetried { .. }),
            "expected NotRetried"
        );
    }

    /// [`attempt_once`] against an inline child with a model `expect:`,
    /// whose own default adapter always raises with *py_class* --
    /// shared by the two tests above.
    async fn model_expect_attempt_problem(
        py_class: electricity_tools::PyExc,
    ) -> Result<(), AttemptProblem> {
        let inline_yaml = "effects:\n  - type: tool\n    name: inner\n    provider: json\n    params: {mode: stringify, input: 1}\n";
        let use_op = UseOp {
            parent_trusted: false,
            source: UseSource::Inline(electricity_bytecode::TemplateText::new(
                inline_yaml.to_string(),
                true,
                electricity_bytecode::Escape::Html,
            )),
            inputs: None,
            outputs: None,
            validate: true,
            retries: electricity_bytecode::RetryPolicy {
                max_attempts: 1,
                backoff_ms: 0,
            },
            expect: Some(ExpectCondition::Model {
                template: electricity_bytecode::TemplateText::new(
                    "ok?",
                    false,
                    electricity_bytecode::Escape::Html,
                ),
            }),
            description: None,
        };
        let op = Op {
            path: EffectPath::root().push_name("step"),
            name: Some("step".to_string()),
            kind: NodeKind::Leaf(Box::new(LeafKind::Use(use_op.clone()))),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };

        let store = Store::new();
        let effect_node = store
            .ensure_dict(&store.root, Value::Str("step".to_string()))
            .unwrap();
        set_leaf(&store, &effect_node, "value", Value::None);
        let meta_node = store
            .ensure_dict(&effect_node, Value::Str("meta".to_string()))
            .unwrap();

        let mut registry = electricity_tools::ToolRegistry::new();
        registry.register(Box::new(electricity_tools::json::JsonTool));
        let limiter = crate::Limiter::new();
        let runtime_config = Value::None;
        let adapters = crate::adapter::AdapterRegistry::new();
        let complexity = ComplexitySettings::default();
        let orchestration_dir = std::path::Path::new(".");
        let declared_prompts = indexmap::IndexMap::new();
        let effect_names = BTreeSet::new();
        let run_ctx = RunContext {
            registry: &registry,
            limiter: &limiter,
            model: "m",
            adapter: "fake",
            runtime_config: &runtime_config,
            dry_run: false,
            armed: false,
            adapters: &adapters,
            default_adapter: Rc::new(FailingAdapter { py_class }),
            model_locked: false,
            adapter_timeout_seconds: 120,
            complexity: &complexity,
            decomposition_depth: 0,
            orchestration_dir,
            declared_prompts: &declared_prompts,
            effect_names: &effect_names,
            supported: electricity_bytecode::Supported::m0(&[]),
        };
        let frame = Frame::root();
        let observer = NullObserver;
        let token = CancellationToken::new();
        let mut label = "inline".to_string();

        attempt_once(
            &op,
            &use_op,
            "step",
            &effect_node,
            &meta_node,
            &Value::None,
            &frame,
            &run_ctx,
            &observer,
            &token,
            &store,
            &mut label,
            false,
            &mut None,
        )
        .await
    }

    /// A minimal, leaked `RunContext` carrying *declared_prompts*/
    /// *effect_names* -- the two inputs this module's own composition
    /// tests (`{{> name}}` through `use.inline`/`use.inputs`, issue
    /// #492's review) actually vary; every other field is the
    /// same inert M0-H shape `attempt_once`'s own existing test above
    /// builds by hand.
    fn run_ctx_for_composition<'a>(
        declared_prompts: &'a indexmap::IndexMap<String, String>,
        effect_names: &'a BTreeSet<String>,
    ) -> RunContext<'a> {
        let registry: &'a electricity_tools::ToolRegistry =
            Box::leak(Box::new(electricity_tools::ToolRegistry::new()));
        let limiter: &'a crate::Limiter = Box::leak(Box::new(crate::Limiter::new()));
        let runtime_config: &'a Value = Box::leak(Box::new(Value::None));
        let adapters: &'a crate::adapter::AdapterRegistry =
            Box::leak(Box::new(crate::adapter::AdapterRegistry::new()));
        let complexity: &'a ComplexitySettings = Box::leak(Box::new(ComplexitySettings::default()));
        let dir: &'static std::path::PathBuf = Box::leak(Box::new(std::path::PathBuf::from(".")));
        RunContext {
            registry,
            limiter,
            model: "",
            adapter: "_noop",
            runtime_config,
            dry_run: false,
            armed: false,
            adapters,
            default_adapter: Rc::new(crate::adapter::NoopAdapter),
            model_locked: false,
            adapter_timeout_seconds: 120,
            complexity,
            decomposition_depth: 0,
            orchestration_dir: dir.as_path(),
            declared_prompts,
            effect_names,
            supported: electricity_bytecode::Supported::m0(&[]),
        }
    }

    #[tokio::test]
    async fn load_child_inline_composes_with_the_inline_label_and_escapes_only_its_own_tags() {
        // `voice`'s own tags are rewritten to no-escape (`{{&input.x}}`)
        // before splicing, so they stay unescaped under `use.inline`'s
        // own `escape: true` (runtime-semantics.md §3.4) -- but a
        // *direct* tag in the same template (`{{input.y}}`, never
        // wrapped in a declared prompt) still escapes under that same
        // flag.
        let mut declared_prompts = indexmap::IndexMap::new();
        declared_prompts.insert("voice".to_string(), "<b>{{input.x}}</b>".to_string());
        let effect_names = BTreeSet::new();
        let run_ctx = run_ctx_for_composition(&declared_prompts, &effect_names);

        let mut input = Dict::new();
        input.insert(Value::from("x"), Value::from("<i>hi</i>"));
        input.insert(Value::from("y"), Value::from("<i>hi2</i>"));
        let mut top = Dict::new();
        top.insert(Value::from("input"), Value::Dict(input));
        top.insert(Value::from("prime"), Value::Dict(Dict::new()));
        let ctx = Value::Dict(top);

        let source = UseSource::Inline(electricity_bytecode::TemplateText::new(
            "description: \"{{> voice}}|{{input.y}}\"\neffects: []\n".to_string(),
            true,
            electricity_bytecode::Escape::Html,
        ));
        let loaded = match load_child("step", &source, true, false, &ctx, &run_ctx).await {
            Ok(loaded) => loaded,
            Err(e) => panic!("expected load_child to succeed, got: {e}"),
        };
        let description = loaded
            .document
            .as_dict()
            .and_then(|d| d.get(&Value::from("description")))
            .and_then(|v| match v {
                Value::Str(s) => Some(s.as_str()),
                _ => None,
            })
            .unwrap();
        assert_eq!(description, "<b><i>hi</i></b>|&lt;i&gt;hi2&lt;/i&gt;");
    }

    #[tokio::test]
    async fn load_child_inline_attributes_a_composition_error_to_the_inline_label() {
        let declared_prompts = indexmap::IndexMap::new();
        let effect_names = BTreeSet::new();
        let run_ctx = run_ctx_for_composition(&declared_prompts, &effect_names);
        let ctx = Value::Dict({
            let mut top = Dict::new();
            top.insert(Value::from("prime"), Value::Dict(Dict::new()));
            top
        });
        let source = UseSource::Inline(electricity_bytecode::TemplateText::new(
            "description: \"{{> nope}}\"\neffects: []\n".to_string(),
            true,
            electricity_bytecode::Escape::Html,
        ));
        let err = match load_child("step", &source, true, false, &ctx, &run_ctx).await {
            Err(e) => e,
            Ok(_) => panic!("expected load_child to fail"),
        };
        assert_eq!(
            err,
            "inline: '{{> nope}}' does not name a declared prompt or effect."
        );
    }

    #[tokio::test]
    async fn attempt_once_renders_inputs_with_a_declared_prompt_and_an_effect_reference() {
        // `voice`'s own `{{input.x}}` tag stays unescaped (spliced in as
        // `{{&input.x}}`); `already_ran`'s own resolved value is spliced
        // through a no-escape hidden binding (`{{{__circuitry_partial_
        // N__}}}`) and is *also* never re-rendered or escaped -- both
        // true regardless of `use.inputs`'s own `escape: true`
        // (runtime-semantics.md §3.4).
        let mut declared_prompts = indexmap::IndexMap::new();
        declared_prompts.insert("voice".to_string(), "<b>{{input.x}}</b>".to_string());
        let mut effect_names = BTreeSet::new();
        effect_names.insert("already_ran".to_string());
        let run_ctx = run_ctx_for_composition(&declared_prompts, &effect_names);

        let store = Store::new();
        let prime = store
            .ensure_dict(&store.root, Value::from("prime"))
            .unwrap();
        let already_ran = store
            .ensure_dict(&prime, Value::from("already_ran"))
            .unwrap();
        store.set_leaf(&already_ran, Value::from("value"), Value::from("<i>v</i>"));
        store.set_leaf(&already_ran, Value::from("meta"), Value::Dict(Dict::new()));
        let mut input = Dict::new();
        input.insert(Value::from("x"), Value::from("<i>hi</i>"));
        store.set_leaf(&store.root, Value::from("input"), Value::Dict(input));
        let chain: CtxChain = vec![CtxSource::Live(Rc::clone(&store.root))];
        let ctx = LiveRoot::new(&chain, &store);

        let mut inputs = indexmap::IndexMap::new();
        inputs.insert(
            Value::from("greeting"),
            electricity_bytecode::ParamNode::Template(electricity_bytecode::TemplateText::new(
                "{{> voice}}|{{> already_ran}}".to_string(),
                true,
                electricity_bytecode::Escape::Html,
            )),
        );
        let use_op = UseOp {
            parent_trusted: false,
            source: UseSource::Inline(electricity_bytecode::TemplateText::new(
                "effects: []\n".to_string(),
                true,
                electricity_bytecode::Escape::Html,
            )),
            inputs: Some(electricity_bytecode::ParamNode::Map(inputs)),
            outputs: None,
            validate: true,
            retries: electricity_bytecode::RetryPolicy {
                max_attempts: 1,
                backoff_ms: 0,
            },
            expect: None,
            description: None,
        };
        let op = Op {
            path: EffectPath::root().push_name("step"),
            name: Some("step".to_string()),
            kind: NodeKind::Leaf(Box::new(LeafKind::Use(use_op.clone()))),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        let effect_node = store
            .ensure_dict(&prime, Value::Str("step".to_string()))
            .unwrap();
        set_leaf(&store, &effect_node, "value", Value::None);
        let meta_node = store
            .ensure_dict(&effect_node, Value::Str("meta".to_string()))
            .unwrap();
        let frame = Frame::root();
        let observer = NullObserver;
        let token = CancellationToken::new();
        let mut label = "inline".to_string();

        match attempt_once(
            &op,
            &use_op,
            "step",
            &effect_node,
            &meta_node,
            &ctx,
            &frame,
            &run_ctx,
            &observer,
            &token,
            &store,
            &mut label,
            false,
            &mut None,
        )
        .await
        {
            Ok(()) => {}
            Err(AttemptProblem::NotRetried { message, .. }) => {
                panic!("expected attempt_once to succeed, got: {message}")
            }
            Err(_) => panic!("expected attempt_once to succeed"),
        }

        let rendered_inputs = match meta_node.borrow().get(&Value::Str("inputs".to_string())) {
            Some(Slot::Value(Value::Dict(d))) => d.clone(),
            other => panic!("expected a dict leaf, got {other:?}"),
        };
        assert_eq!(
            rendered_inputs.get(&Value::from("greeting")),
            Some(&Value::from("<b><i>hi</i></b>|<i>v</i>"))
        );
    }

    #[tokio::test]
    async fn attempt_once_attributes_an_input_composition_error_to_its_own_dotted_label() {
        let declared_prompts = indexmap::IndexMap::new();
        let effect_names = BTreeSet::new();
        let run_ctx = run_ctx_for_composition(&declared_prompts, &effect_names);

        let store = Store::new();
        let ctx = Value::Dict({
            let mut top = Dict::new();
            top.insert(Value::from("prime"), Value::Dict(Dict::new()));
            top
        });

        let mut inputs = indexmap::IndexMap::new();
        inputs.insert(
            Value::from("greeting"),
            electricity_bytecode::ParamNode::Template(electricity_bytecode::TemplateText::new(
                "{{> nope}}".to_string(),
                true,
                electricity_bytecode::Escape::Html,
            )),
        );
        let use_op = UseOp {
            parent_trusted: false,
            source: UseSource::Inline(electricity_bytecode::TemplateText::new(
                "effects: []\n".to_string(),
                true,
                electricity_bytecode::Escape::Html,
            )),
            inputs: Some(electricity_bytecode::ParamNode::Map(inputs)),
            outputs: None,
            validate: true,
            retries: electricity_bytecode::RetryPolicy {
                max_attempts: 1,
                backoff_ms: 0,
            },
            expect: None,
            description: None,
        };
        let op = Op {
            path: EffectPath::root().push_name("step"),
            name: Some("step".to_string()),
            kind: NodeKind::Leaf(Box::new(LeafKind::Use(use_op.clone()))),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        let effect_node = store
            .ensure_dict(&store.root, Value::Str("step".to_string()))
            .unwrap();
        set_leaf(&store, &effect_node, "value", Value::None);
        let meta_node = store
            .ensure_dict(&effect_node, Value::Str("meta".to_string()))
            .unwrap();
        let frame = Frame::root();
        let observer = NullObserver;
        let token = CancellationToken::new();
        let mut label = "inline".to_string();

        let message = match attempt_once(
            &op,
            &use_op,
            "step",
            &effect_node,
            &meta_node,
            &ctx,
            &frame,
            &run_ctx,
            &observer,
            &token,
            &store,
            &mut label,
            false,
            &mut None,
        )
        .await
        {
            Err(AttemptProblem::NotRetried { message, .. }) => message,
            Err(_) => panic!("expected AttemptProblem::NotRetried"),
            Ok(()) => panic!("expected attempt_once to fail"),
        };
        assert_eq!(
            message,
            "inputs.greeting: '{{> nope}}' does not name a declared prompt or effect."
        );
    }

    #[test]
    fn resolve_reference_walks_a_mapping_by_key() {
        let mut inner = Dict::new();
        inner.insert(Value::Str("y".to_string()), Value::from(5i64));
        let mut outer = Dict::new();
        outer.insert(Value::Str("x".to_string()), Value::Dict(inner));
        let ctx = Value::Dict(outer);
        assert_eq!(ctx.resolve_from("x.y"), Some(Value::from(5i64)));
        assert_eq!(ctx.resolve_from("x.z"), None);
    }

    #[test]
    fn resolve_reference_walks_a_list_by_python_index() {
        let ctx = Value::List(vec![
            Value::from(1i64),
            Value::from(2i64),
            Value::from(3i64),
        ]);
        assert_eq!(ctx.resolve_from("-1"), Some(Value::from(3i64)));
        assert_eq!(ctx.resolve_from("5"), None);
    }

    #[test]
    fn resolve_dot_path_is_dict_only_never_a_list_index() {
        let state = Value::List(vec![Value::from(1i64)]);
        assert_eq!(resolve_dot_path(&state, "0"), Value::None);
    }

    #[test]
    fn collect_child_errors_recurses_and_labels_the_root_prime() {
        let mut step_meta = Dict::new();
        step_meta.insert(Value::Str("error".to_string()), Value::from("boom"));
        let mut step = Dict::new();
        step.insert(Value::Str("meta".to_string()), Value::Dict(step_meta));
        let mut root_meta = Dict::new();
        root_meta.insert(Value::Str("error".to_string()), Value::None);
        let mut root = Dict::new();
        root.insert(Value::Str("meta".to_string()), Value::Dict(root_meta));
        root.insert(Value::Str("step".to_string()), Value::Dict(step));
        let errors = collect_child_errors(&Value::Dict(root), "");
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].0, "step");
    }

    #[test]
    fn validation_failed_text_names_the_label_and_lists_every_error() {
        let text = validation_failed_text(
            "child.yml",
            &["bad thing one".to_string(), "bad thing two".to_string()],
        );
        assert_eq!(
            text,
            "Orchestration child.yml validation failed:\n  - bad thing one\n  - bad thing two"
        );
    }

    /// A temporary project (`config.json` marks it) holding a `child.yml`,
    /// removed on drop, for the origin a path child is loaded with.
    struct ChildProject(std::path::PathBuf);

    impl ChildProject {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "electricity-use-trust-test-{tag}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            let dir = std::fs::canonicalize(&dir).unwrap();
            std::fs::write(dir.join("config.json"), "{}").unwrap();
            std::fs::write(dir.join("child.yml"), "effects: []\n").unwrap();
            ChildProject(dir)
        }
    }

    impl Drop for ChildProject {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    async fn loaded_path_child_origin(
        project: &ChildProject,
        parent_trusted: bool,
    ) -> electricity_compiler::DocumentOrigin {
        let declared_prompts = indexmap::IndexMap::new();
        let effect_names = BTreeSet::new();
        let run_ctx = run_ctx_for_composition(&declared_prompts, &effect_names);
        let source = UseSource::Path(project.0.join("child.yml").display().to_string());
        let ctx = Value::Dict(Dict::new());
        match load_child("step", &source, true, parent_trusted, &ctx, &run_ctx).await {
            Ok(loaded) => loaded.origin,
            Err(e) => panic!("expected load_child to succeed, got: {e}"),
        }
    }

    /// Issue #578: a child named by `path:` gets its parent's trust, so a
    /// prompt's `file: {from: ...}` in it may name any `within`.
    #[tokio::test]
    async fn a_path_child_of_a_trusted_parent_is_trusted() {
        let project = ChildProject::new("trusted");
        let origin = loaded_path_child_origin(&project, true).await;
        assert_eq!(
            origin,
            electricity_compiler::DocumentOrigin::File {
                document_dir: project.0.clone(),
                confinement_root: project.0.clone(),
            }
        );
    }

    /// Issue #578: a path child of a limited parent is limited, confined to
    /// its own project.
    #[tokio::test]
    async fn a_path_child_of_a_limited_parent_is_limited() {
        let project = ChildProject::new("limited");
        let origin = loaded_path_child_origin(&project, false).await;
        assert_eq!(
            origin,
            electricity_compiler::DocumentOrigin::LimitedFile {
                document_dir: project.0.clone(),
                confinement_root: project.0.clone(),
            }
        );
    }

    /// Issue #578: an inline child is generated, so it is never trusted,
    /// whatever its parent is.
    #[tokio::test]
    async fn an_inline_child_is_generated_whatever_its_parent_is() {
        let declared_prompts = indexmap::IndexMap::new();
        let effect_names = BTreeSet::new();
        let run_ctx = run_ctx_for_composition(&declared_prompts, &effect_names);
        let source = UseSource::Inline(electricity_bytecode::TemplateText::new(
            "effects: []\n".to_string(),
            true,
            electricity_bytecode::Escape::Html,
        ));
        let ctx = Value::Dict(Dict::new());
        let loaded = match load_child("step", &source, true, true, &ctx, &run_ctx).await {
            Ok(loaded) => loaded,
            Err(e) => panic!("expected load_child to succeed, got: {e}"),
        };
        assert_eq!(
            loaded.origin,
            electricity_compiler::DocumentOrigin::Generated
        );
    }

    #[test]
    fn sha256_hex_matches_a_known_vector() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
