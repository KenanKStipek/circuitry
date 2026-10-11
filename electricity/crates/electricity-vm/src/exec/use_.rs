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

use crate::exec::{CtxChain, live_ctx, store_err};
use crate::observer::InstanceId;
use crate::store::Slot;
use crate::{CancellationToken, Frame, NodeRef, RunContext, RunObserver, Store, VmError};
use electricity_bytecode::{
    EffectPath, ExpectCondition, OnError, Op, PathSegment, Program, UseOp, UseSource,
};
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

/// `core/use.py::_resolve_reference` -- walks *path* (dot-separated)
/// through *ctx*: a mapping segment looks the next part up by exact
/// key; a list segment parses the next part as a Python-style
/// (negative-wraps) index. `None` on any missing key, out-of-range
/// index, or a segment that indexes into a non-container.
///
/// Not [`crate::params`]'s own `resolve_reference`: that module's doc
/// comment says a `use` effect's own `inputs:` follow a different
/// contract (no `default:`, a `{from:}` that resolves to `None` passes
/// through as `None` rather than raising) and isn't rendered there --
/// this is `use_.rs`'s own copy of the walk, not a reused one.
fn resolve_reference(ctx: &Value, path: &str) -> Option<Value> {
    let mut current = ctx.clone();
    for part in path.split('.') {
        current = match &current {
            Value::Dict(dict) => dict.get(&Value::Str(part.to_string()))?.clone(),
            Value::List(items) => {
                let index: i64 = part.parse().ok()?;
                let len = items.len() as i64;
                let normalized = if index < 0 { index + len } else { index };
                if normalized < 0 || normalized >= len {
                    return None;
                }
                items[normalized as usize].clone()
            }
            _ => return None,
        };
    }
    Some(current)
}

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

/// `core/use.py::_clean_yaml_fences`.
pub fn clean_yaml_fences(text: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    for line in text.lines() {
        let stripped = line.trim();
        if stripped.starts_with("```") || stripped == "---" {
            continue;
        }
        lines.push(line);
    }
    lines.join("\n").trim().to_string()
}

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
/// index, unlike [`resolve_reference`]'s own by-reference-input walk):
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
/// open) unless that entry is a list.
fn allowlist_names(runtime_config: &Value, key: &str) -> Option<Vec<String>> {
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
async fn load_child(
    name: &str,
    source: &UseSource,
    validate: bool,
    ctx: &Value,
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
                let errors: Vec<String> = match electricity_yaml::load_yaml(&cleaned) {
                    Err(e) => vec![format!("YAML parse error: {e}")],
                    Ok(parsed) => match validate_inline_yaml(&parsed) {
                        Ok(()) => Vec::new(),
                        Err(errors) => errors,
                    },
                };
                if !errors.is_empty() {
                    return Err(format!(
                        "Inline orchestration validation failed:\n{}",
                        errors
                            .iter()
                            .map(|e| format!("  - {e}"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    ));
                }
                electricity_yaml::load_yaml(&cleaned)
                    .map_err(|e| format!("YAML parse error: {e}"))?
            } else {
                let parsed = electricity_yaml::load_yaml(&cleaned).map_err(|e| e.to_string())?;
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
            Ok(LoadedChild {
                document,
                label: label.clone(),
                identity,
                digest,
                document_dir: document_dir.clone(),
                origin: electricity_compiler::DocumentOrigin::File {
                    document_dir,
                    confinement_root,
                },
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
async fn attempt_once(
    op: &Op,
    use_op: &UseOp,
    name: &str,
    effect_node: &NodeRef,
    meta_node: &NodeRef,
    ctx: &Value,
    frame: &Frame,
    run_ctx: &RunContext<'_>,
    observer: &dyn RunObserver,
    token: &CancellationToken,
    store: &Store,
    label: &mut String,
    record_children: bool,
) -> Result<(), AttemptProblem> {
    let loaded = load_child(name, &use_op.source, use_op.validate, ctx, run_ctx)
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
    let child_frame = frame.nested(&op.path).push_use(loaded.identity.clone());

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
                electricity_bytecode::ParamNode::From { path, .. } => {
                    let resolved = resolve_reference(ctx, path).unwrap_or(Value::None);
                    if matches!(resolved, Value::None) {
                        if let Value::Str(key_str) = key {
                            unresolved.insert(key_str.clone(), path.clone());
                        }
                    }
                    resolved
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
        run_ctx.complexity.enabled(),
        &run_ctx.supported,
    ) {
        return Err(AttemptProblem::Fatal(VmError::Refused {
            refusal,
            child_label: loaded.label.clone(),
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
        display_depth: run_ctx.display_depth + 1,
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
            return Err(AttemptProblem::Fatal(VmError::Refused {
                refusal,
                child_label,
            }));
        }
        Err(other) => {
            // `core/use.py`'s own `record_children` graft for *this*
            // failure mode -- the child's own run raising mid-way --
            // only ever happens from the outer `except`, once this
            // attempt's failure has already been decided final
            // (`use.py:1121-1127`); a retryable attempt's own `except`
            // (`use.py:1082-1094`) sets `meta["error"]` and loops again
            // with no graft at all. Grafting unconditionally here, on
            // every attempt, would let a key only a now-discarded
            // attempt's own child ever wrote survive a later successful
            // retry (review finding 8 on PR #472) -- so the record is
            // only *collected* here (a live `NodeRef`, not a `Value`
            // snapshot, to keep `graft_child_record`'s own node-identity
            // preservation) and [`execute_use`] grafts it, only once it
            // knows this attempt will not be retried.
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
    // fatal); only a fatal `VmError` out of the model-mode stub (a
    // cancellation or a refusal reached while rendering/dispatching)
    // propagates as fatal.
    if let Some(expect) = &use_op.expect {
        let value_snapshot = store.snapshot(effect_node);
        let value_for_expect = value_snapshot
            .as_dict()
            .and_then(|d| d.get(&Value::Str("value".to_string())))
            .cloned()
            .unwrap_or(Value::None);
        let meta_snapshot = store.snapshot(meta_node);
        match expect {
            ExpectCondition::Cel { expr } => {
                let mut expect_meta = IndexMap::new();
                expect_meta.insert(Value::from("mode"), Value::from("cel"));
                expect_meta.insert(Value::from("expr"), Value::from(expr.as_str()));
                match electricity_cel::evaluate_expect(expr, &value_for_expect, &meta_snapshot, ctx)
                {
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
            ExpectCondition::Model { template } => {
                let effect_path = frame.effect_path(&op.path);
                let summary = expect_model_summary(&template.source);
                match crate::exec::condition_model::evaluate_model_expect(
                    template,
                    &value_for_expect,
                    ctx,
                    &effect_path,
                    frame,
                    run_ctx,
                    token,
                )
                .await
                {
                    Ok(outcome) => {
                        let mut expect_meta = IndexMap::new();
                        expect_meta.insert(Value::from("mode"), Value::from("model"));
                        expect_meta.insert(Value::from("template"), Value::from(summary.as_str()));
                        expect_meta.insert(
                            Value::from("answer"),
                            Value::from(outcome.generated.text.as_str()),
                        );
                        expect_meta.insert(
                            Value::from("tokens_sent"),
                            outcome
                                .generated
                                .tokens_sent
                                .map(Value::from)
                                .unwrap_or(Value::None),
                        );
                        expect_meta.insert(
                            Value::from("tokens_received"),
                            outcome
                                .generated
                                .tokens_received
                                .map(Value::from)
                                .unwrap_or(Value::None),
                        );
                        expect_meta.insert(Value::from("result"), Value::Bool(outcome.passed));
                        set_leaf(store, meta_node, "expect", Value::Dict(expect_meta));
                        if !outcome.passed {
                            return Err(AttemptProblem::Retryable {
                                message: format!("expect failed: {summary}"),
                                child_record: None,
                            });
                        }
                    }
                    Err(e) if e.is_fatal() => return Err(AttemptProblem::Fatal(e)),
                    Err(e) => {
                        return Err(AttemptProblem::Retryable {
                            message: e.to_string(),
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
/// comment for the overall design.
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
    let ctx = live_ctx(ctx_chain, store);
    execute_use_with_ctx(
        op, use_op, store, parent, &ctx, frame, run_ctx, observer, token,
    )
    .await
}

/// [`execute_use`]'s own real body, taking an already-resolved *ctx*
/// directly rather than a [`CtxChain`] -- [`CtxChain`]/[`CtxSource`]
/// are `pub(crate)` (lane B2/H's own `exec/mod.rs`, not this lane's to
/// widen), so an external integration test (`tests/use_corpus.rs`,
/// the golden-corpus replay review finding 5 on PR #472 asked for)
/// cannot name one to call [`execute_use`] itself with. `core/use.py`'s
/// own `UseRuntime.execute(store, ctx)` takes *ctx* the exact same
/// way -- this function is the closer match to replay the corpus
/// against, not a test-only detour.
#[allow(clippy::too_many_arguments)]
pub async fn execute_use_with_ctx(
    op: &Op,
    use_op: &UseOp,
    store: &Store,
    parent: &NodeRef,
    ctx: &Value,
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

    let effect_path = frame.effect_path(&op.path);
    let instance: InstanceId = observer.effect_start(&effect_path);

    if run_ctx.dry_run {
        set_leaf(store, &meta_node, "completed_at", now_iso());
        set_leaf(store, &meta_node, "dry_run", Value::Bool(true));
        observer.effect_complete(&effect_path, instance, None);
        observer.write();
        return Ok(());
    }

    let record_children = record_children_enabled(run_ctx.runtime_config);

    let max_attempts = use_op.retries.max_attempts.max(1);
    let base_backoff_ms = use_op.retries.backoff_ms;
    let mut next_delay_ms = base_backoff_ms;

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
        if attempt_index == 0 {
            set_leaf(store, &meta_node, "created_at", now_iso());
        }
        set_leaf(store, &meta_node, "completed_at", Value::None);
        set_leaf(store, &meta_node, "error", Value::None);
        set_leaf(store, &meta_node, "child_errors", Value::None);
        set_leaf(store, &meta_node, "inputs", Value::None);
        set_leaf(store, &meta_node, "orchestration_sha256", Value::None);
        pop_leaf(&meta_node, "expect");
        pop_leaf(&meta_node, "retries_used");
        observer.write();

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
        )
        .await;

        let (message, py_class, child_record) = match attempt_result {
            Ok(()) => {
                if attempt_index > 0 {
                    set_leaf(
                        store,
                        &meta_node,
                        "retries_used",
                        Value::from(i64::from(attempt_index)),
                    );
                }
                observer.effect_complete(&effect_path, instance, None);
                observer.write();
                return Ok(());
            }
            Err(AttemptProblem::Fatal(VmError::Cancelled)) => return Err(VmError::Cancelled),
            Err(AttemptProblem::Fatal(refused @ VmError::Refused { .. })) => {
                let text = refused.to_string();
                set_leaf(store, &meta_node, "error", Value::Str(text.clone()));
                set_leaf(store, &meta_node, "completed_at", now_iso());
                if attempt_index > 0 {
                    set_leaf(
                        store,
                        &meta_node,
                        "retries_used",
                        Value::from(i64::from(attempt_index)),
                    );
                }
                observer.effect_complete(&effect_path, instance, Some(&text));
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
        let retryable = !matches!(py_class, PyExc::ValueError | PyExc::RecursionError);
        if !is_last_attempt && retryable {
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

        set_leaf(store, &meta_node, "completed_at", now_iso());
        if attempt_index > 0 {
            set_leaf(
                store,
                &meta_node,
                "retries_used",
                Value::from(i64::from(attempt_index)),
            );
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
                observer.effect_complete(&effect_path, instance, Some(&message));
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
                observer.effect_complete(&effect_path, instance, Some(&message));
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
    /// (review finding 7 on PR #472): a long loop inside the child
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

    fn effect_complete(&self, path: &EffectPath, instance: InstanceId, error: Option<&str>) {
        if *path == self.root_path {
            return;
        }
        self.parent.effect_complete(path, instance, error);
    }

    // `core/use.py`'s own child `Store` is handed `concurrent_dispatch`
    // straight from the parent, un-namespaced (`use.py:969`); the path a
    // dispatch event carries comes from that child store's own
    // `effect_path`, always `prime.<name>` relative to the child's own
    // root, never aware of the `use` node it runs under
    // (`effect_identity.py::nested_container` only ever sets the
    // call-path contextvar, not `concurrent_dispatch`'s own path
    // argument). This crate's own child [`Frame`] composes every path
    // through [`Frame::effect_path`] instead (so every *other* hook
    // forwards correctly namespaced) -- *dispatch* alone has to be
    // rewritten back to what the child's own unnamespaced `prime`-
    // relative path would have been, matching on whole
    // [`electricity_bytecode::PathSegment`]s rather than string surgery.
    fn dispatch(&self, path: &EffectPath, branches: usize, concurrency: usize) {
        self.parent
            .dispatch(&self.unnamespace(path), branches, concurrency);
    }

    fn write(&self) {
        // Orchestrator R4 on PR #472's review: without a `--live-state`
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

    fn graft_child_write(&self, node_path: &EffectPath, snapshot: &Value) {
        self.parent.graft_child_write(node_path, snapshot);
    }

    fn clear_graft(&self, node_path: &EffectPath) {
        self.parent.clear_graft(node_path);
    }
}

impl ChildObserver<'_> {
    /// *path* with this child's own [`ChildObserver::root_path`] prefix
    /// replaced by a bare [`EffectPath::root`] -- a no-op for a path
    /// that doesn't start with it (a dispatch from a *nested* `use`'s
    /// own, already-unnamespaced child, forwarded through unchanged).
    fn unnamespace(&self, path: &EffectPath) -> EffectPath {
        let prefix_len = self.root_path.0.len();
        if path.0.len() >= prefix_len && path.0[..prefix_len] == self.root_path.0[..] {
            let mut segments = vec![PathSegment::Name("prime".to_string())];
            segments.extend_from_slice(&path.0[prefix_len..]);
            EffectPath(segments)
        } else {
            path.clone()
        }
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
    use crate::observer::NullObserver;
    use electricity_bytecode::{LeafKind, NodeKind};
    use electricity_config::ComplexitySettings;
    use std::collections::BTreeSet;
    use std::rc::Rc;

    /// Review finding 8 on PR #472 (orchestrator ruling R5): a
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
            display_depth: 0,
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

    #[test]
    fn clean_yaml_fences_drops_code_fences_and_document_separators() {
        let text = "```yaml\neffects: []\n---\n```";
        assert_eq!(clean_yaml_fences(text), "effects: []");
    }

    #[test]
    fn clean_yaml_fences_keeps_original_indentation() {
        let text = "effects:\n  - type: tool\n```\n";
        assert_eq!(clean_yaml_fences(text), "effects:\n  - type: tool");
    }

    #[test]
    fn resolve_reference_walks_a_mapping_by_key() {
        let mut inner = Dict::new();
        inner.insert(Value::Str("y".to_string()), Value::from(5i64));
        let mut outer = Dict::new();
        outer.insert(Value::Str("x".to_string()), Value::Dict(inner));
        let ctx = Value::Dict(outer);
        assert_eq!(resolve_reference(&ctx, "x.y"), Some(Value::from(5i64)));
        assert_eq!(resolve_reference(&ctx, "x.z"), None);
    }

    #[test]
    fn resolve_reference_walks_a_list_by_python_index() {
        let ctx = Value::List(vec![
            Value::from(1i64),
            Value::from(2i64),
            Value::from(3i64),
        ]);
        assert_eq!(resolve_reference(&ctx, "-1"), Some(Value::from(3i64)));
        assert_eq!(resolve_reference(&ctx, "5"), None);
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

    #[test]
    fn sha256_hex_matches_a_known_vector() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
