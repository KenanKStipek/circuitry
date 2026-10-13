//! The electricity library crate: [`run_orchestration`] (issue #431's
//! run-wiring steps 1-19) is the real VM run `electricity-cli`'s own
//! `Run` action drives -- resolve the config, seed and check the
//! document, refuse unsupported content, then execute
//! ([`electricity_vm::execute_root`], lanes B/C) with `--events`/
//! `--live-state` observing it. `--out`/stdout ([`out`]), `--events`
//! ([`events`]) and `--live-state` ([`live_state`]) are this crate's own
//! writers; [`state`] seeds a fresh run's store. `../../DESIGN.md` and
//! `../../docs/spec/vm-lanes.md` have the full M0-H lane ownership map
//! for this crate and its sibling VM-lane crates. [`dump_ir`] is a
//! separate, unstable debugging aid that runs the document check alone
//! and prints the result as JSON -- it never runs anything, so it has
//! no refusal of its own either.

use std::path::Path;

pub mod events;
pub mod live_state;
pub mod out;
pub mod preflight;
pub mod run;
pub mod state;

/// `electricity_adapters::write_leftover_export` -- re-exported so
/// `electricity-cli`'s own exit path (the only caller) doesn't need
/// its own direct dependency on `electricity-adapters` just for this
/// one function (issue #478, lane F1).
pub use electricity_adapters::write_leftover_export;
pub use electricity_vm::CancellationToken;
pub use run::{RunRequest, RunResult, Signal};

/// The crate's version, taken from the workspace's `Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// `electricity <version> (preview)`, the string printed by `electricity --version`.
pub fn version_string() -> String {
    format!("electricity {VERSION} (preview)")
}

/// Parses the CLI's `-e key=value` entries, in order, into
/// [`electricity_compiler::CheckOptions`]'s own `inputs` shape --
/// `cli/app.py::_parse_env_vars`'s own `"=" not in entry` check and its
/// exact `BadParameter` text (issue #429: Circuitry's own message, not
/// third-party, so matched word for word), and its own `result[key] =
/// ...` dict-assignment semantics for a repeated key: an `IndexMap`'s
/// `insert` keeps a repeated key at its *first* occurrence's position
/// while taking the *new* value, exactly like a Python `dict`'s own
/// `__setitem__` does -- so `-e name=A -e name=B` behaves like cof's
/// own `-e name=A -e name=B` (`B` wins, in `name`'s original position).
pub fn parse_inputs(entries: &[String]) -> Result<indexmap::IndexMap<String, String>, String> {
    let mut result = indexmap::IndexMap::new();
    for entry in entries {
        match entry.split_once('=') {
            Some((key, value)) => {
                result.insert(key.to_string(), value.to_string());
            }
            None => {
                return Err(format!(
                    "Invalid -e format: {} (expected KEY=VALUE)",
                    python_repr_str(entry)
                ));
            }
        }
    }
    Ok(result)
}

/// Python `repr(s)` of a plain Rust `&str` -- [`parse_inputs`]'s own
/// malformed-entry message quotes the offending text the same way
/// Python's `{entry!r}` f-string interpolation does.
fn python_repr_str(s: &str) -> String {
    electricity_value::Value::Str(s.to_string()).py_repr()
}

/// The [`electricity_compiler::CheckOptions`] an `electricity` run of
/// *orchestration_path* against *config_path* checks against: trusting
/// the document and skipping preflight (issue #408's CLI section --
/// `first_unsupported` refuses a document naming a top-level `adapter:`
/// outright instead, exactly because this crate never runs the check
/// that flag would otherwise skip),
/// *config_path*'s own fully-resolved `runtime:` block --
/// `SANE_DEFAULTS`, the file deep-merged on top, then `CIRCUITRY_*`
/// env overlays, exactly [`run_orchestration`]'s own step 1
/// ([`config_runtime_block`], `None` only when resolving it fails
/// outright, lenient on purpose: this function has no error return of
/// its own to report that failure through, unlike `run_orchestration`
/// -- a caller that cares (this crate's own [`dump_ir`]) sees it
/// surface instead as whatever document/structural error a document
/// relying on that config would then fail with, same as the pre-#431
/// preview build's own latitude here) -- and *inputs* (the CLI's own
/// `-e key=value` pairs, [`parse_inputs`]'s own output -- issue #429)
/// passed straight through as `CheckOptions.inputs`. What another tool
/// in this workspace (`oscilloscope`, which links `electricity_
/// compiler` directly) should call to compile a document with exactly
/// the options a real `electricity` run would use for it, rather than
/// reimplementing this merge itself (PR #441 review finding 14: this
/// function's own `config_runtime_block` and `run_orchestration`'s own
/// step 1 now both go through `electricity_config::resolve_config`, so
/// `--dump-ir`/osp's own plan can no longer drift from what a real run
/// sees just because `CIRCUITRY_MODEL`-style env overlays, or a
/// config-file key `SANE_DEFAULTS` itself supplies, were never once
/// part of this function's own, narrower raw-file read).
pub fn check_options(
    config_path: &Path,
    inputs: &indexmap::IndexMap<String, String>,
) -> electricity_compiler::CheckOptions {
    electricity_compiler::CheckOptions {
        skip_preflight: true,
        trust_document: true,
        config_runtime: config_runtime_block(config_path),
        inputs: inputs.clone(),
        profile_inputs: indexmap::IndexMap::new(),
    }
}

/// *config_path*'s own fully-resolved `runtime:` block ([`check_options`]'s
/// own doc comment), or `None` when resolving the config fails outright.
fn config_runtime_block(config_path: &Path) -> Option<electricity_value::Value> {
    electricity_config::resolve_config(config_path, &current_env_vars())
        .ok()?
        .runtime
}

/// `{"ir_version": "unstable", "program": ...}`, 2-space indented, for
/// `electricity --dump-ir` (issue #408's CLI section). Not a contract:
/// nothing in this workspace reads this shape back, and it may change in
/// any release -- see `electricity_bytecode`'s crate docs.
///
/// Runs `electricity_compiler::check_for_run` first, with *config_path*'s
/// own `runtime:` block merged under the document's own the same way
/// [`run_orchestration`] does -- so a `--dump-ir` of a document that
/// relies on a config-defined `runtime.concurrency_groups`/
/// `max_concurrency` checks out the same way a `cof run`/plain run of it
/// would, rather than failing without the config file's own settings.
/// Its `Err` becomes this function's `Err`, with the exact text
/// `--dump-ir` writes to stderr on failure (the same text a plain run of
/// the same document would report).
///
/// *inputs* ([`parse_inputs`]'s own output, issue #429) is passed
/// through as `CheckOptions.inputs`, same as [`run_orchestration`]'s own
/// document check -- so a document with a required input needs `-e` on
/// `--dump-ir` too, exactly as it does on a plain run.
pub fn dump_ir(
    config_path: &Path,
    orchestration_path: &Path,
    inputs: &indexmap::IndexMap<String, String>,
) -> Result<String, String> {
    let options = check_options(config_path, inputs);
    let program = electricity_compiler::check_for_run(orchestration_path, &options)
        .map_err(|err| err.to_string())?;
    // `serde_json::json!` would `.unwrap()` internally on a `Program`
    // that fails to serialize (e.g. a non-UTF-8 `PathBuf` in
    // `DocumentInfo`) -- reporting that as an `Err` on stderr instead
    // of panicking matters here specifically, since a `--dump-ir`
    // failure is this crate's one promise never to crash.
    let program_value = serde_json::to_value(&program).map_err(|err| err.to_string())?;
    let mut wrapper = serde_json::Map::with_capacity(2);
    wrapper.insert(
        "ir_version".to_string(),
        serde_json::Value::from("unstable"),
    );
    wrapper.insert("program".to_string(), program_value);
    serde_json::to_string_pretty(&serde_json::Value::Object(wrapper)).map_err(|err| err.to_string())
}

// ---------------------------------------------------------------------
// run_orchestration -- issue #431's run-wiring steps 1-19
// ---------------------------------------------------------------------

use electricity_bytecode::{EffectPath, PREVIEW_MARKER, first_unsupported, format_refusal};
use electricity_config::CircuitryConfig;
use electricity_value::{Dict, Value};
use electricity_vm::{Limiter, RunContext, RunObserver, Store};
use std::cell::Cell;
use std::rc::Rc;
use std::time::Instant;

/// Extra tool providers this crate's own [`tool_registry`] dispatches
/// on top of `json` (always on -- [`electricity_bytecode::refusal::
/// Supported::allows_provider`]) -- just the `test-tools` cargo
/// feature's own `sleep`/`fail` when that feature is compiled in
/// (`electricity_bytecode::refusal::Supported::providers`). Empty in
/// every ordinary build -- `electricity-cli`'s own `Cargo.toml` never
/// enables this feature (that crate's own doc comment). M1 gate seam
/// round 2, item S3: this function used to return the walker's *entire*
/// provider allow-list (`&["json"]`); `shell`/`awk`/`imagemagick`/
/// `ffmpeg` (lane C), `fs`/`env_vars`/`hash`/`uuid`/`clock` (lane D1),
/// `regex` (lane D2) and `http` (lane E) are now each their own
/// capability flag on [`electricity_bytecode::refusal::Supported`]
/// instead, flipped in [`electricity_bytecode::refusal::Supported::
/// current`], so no lane needs to touch this slice at all.
#[cfg(feature = "test-tools")]
fn extra_supported_providers() -> &'static [&'static str] {
    &["sleep", "fail"]
}

#[cfg(not(feature = "test-tools"))]
fn extra_supported_providers() -> &'static [&'static str] {
    &[]
}

/// A fresh tool registry: `json` (M0-H's only real provider), plus
/// `sleep`/`fail` when the `test-tools` feature is enabled -- the one
/// place both are ever registered, so [`extra_supported_providers`]
/// (the refusal walker's own extra allow-list) never drifts from what
/// the registry actually dispatches.
/// `json`, like every other provider this crate can build, now goes
/// through [`electricity_vm::exec::tool::resolve_plugin`]'s own
/// per-attempt [`electricity_tools::build_plugin`] fallback instead of
/// a long-lived singleton here (review finding 6 on PR #454) -- a
/// production run's own [`electricity_vm::RunContext::registry`] is
/// left with nothing registered at all through M1-A; `test-tools`'
/// `sleep`/`fail` still register as singletons (stateless, so there is
/// nothing per-attempt for either to restart).
fn tool_registry() -> electricity_tools::ToolRegistry {
    // `mut` is only exercised under `test-tools` -- a plain release
    // build's own registry is never mutated after construction, now
    // that `json` no longer registers a singleton here.
    #[allow(unused_mut)]
    let mut registry = electricity_tools::ToolRegistry::new();
    #[cfg(feature = "test-tools")]
    {
        registry.register(Box::new(electricity_tools::test_tools::SleepTool));
        registry.register(Box::new(electricity_tools::test_tools::FailTool));
    }
    registry
}

/// Whether *runtime* (the already-merged, effective `runtime:` block)
/// configures persistence -- `runtime.persistence.enabled` is Python-
/// truthy (`electricity_config::persistence_enabled`, PR #441 review
/// finding 2: `1`/`"yes"` turn persistence on just as `true` does, not
/// only a literal boolean). `electricity_config::validate_persistence`,
/// called inside `pre_state_checks`, has already rejected anything
/// malformed by the time a caller ever reaches this; this only asks
/// whether the now-valid block actually turns the backend on,
/// Circuitry's own `build_persistence_backend`'s one truthy gate.
fn persistence_is_configured(runtime: Option<&Value>) -> bool {
    electricity_config::persistence_enabled(runtime)
}

/// `cli/runtime_shim.py::_has_prompt_effects`, ported over a compiled
/// [`electricity_bytecode::Program`]'s own root [`electricity_bytecode::Op`]
/// instead of a raw `*Definition` tree -- orchestrator P1 (reachable
/// now that `use` runs): a document containing one of these needs the
/// *real* `effective.adapter`/`effective.model` names (`runtime.
/// effective_settings` and every container's own `meta.adapter`), not
/// the `_noop` sentinel every other document gets, even though M0-H
/// still refuses every one of these capabilities before ever building
/// a real adapter object -- a `use` child's own content is never
/// walked (Python's own check is conservative: *any* `use` effect
/// counts, since its content might not even exist yet at this point --
/// an inline child renders at run time). A disabled effect (`enabled:
/// false`) never runs, so it and everything nested under it are
/// skipped entirely, matching Python's own `is_enabled` guard ahead of
/// every other check in the function this ports.
fn has_prompt_effects(op: &electricity_bytecode::Op) -> bool {
    use electricity_bytecode::{Condition, ExpectCondition, LeafKind, LoopSpec, NodeKind, Region};

    fn region_has_prompt_effects(region: &Region) -> bool {
        match region {
            Region::Block { ops, .. } => ops.iter().any(has_prompt_effects),
            Region::Parallel { branches, .. } => branches.iter().any(has_prompt_effects),
            Region::TryFinally { body, finally } => {
                region_has_prompt_effects(body) || region_has_prompt_effects(finally)
            }
            Region::If {
                cond, then_, else_, ..
            } => {
                matches!(cond, Condition::Model { .. })
                    || region_has_prompt_effects(then_)
                    || else_.as_deref().is_some_and(region_has_prompt_effects)
            }
            Region::Loop { spec, body, .. } => {
                matches!(spec, LoopSpec::While(Condition::Model { .. }))
                    || region_has_prompt_effects(body)
            }
        }
    }

    if !op.enabled {
        return false;
    }
    match &op.kind {
        NodeKind::Leaf(leaf) => match leaf.as_ref() {
            LeafKind::Prompt(_) => true,
            LeafKind::Use(_) => true,
            LeafKind::Tool(tool) => {
                matches!(&tool.expect, Some(ExpectCondition::Model { .. }))
            }
            LeafKind::Reflector(reflector) => region_has_prompt_effects(&reflector.inner),
            LeafKind::Yield(_) => false,
        },
        NodeKind::Control(region) => region_has_prompt_effects(region),
    }
}

/// Settled ruling 3, widened for lane G (issue #498, #448): whether the
/// *root* document itself (never a `use` child's own content, which
/// isn't expanded inline here -- a `use` leaf never recurses) has an
/// effect that would call *this run's own default adapter* directly --
/// a `prompt` (regardless of its own `provider:`, conservatively: a
/// provider override still falls back to the default on
/// `provider_fallbacks` exhaustion), a model-mode `if`/`while`
/// (`core/conditional.py`'s own mode-model branch always calls
/// `generate()` on the document's default adapter, never a per-
/// condition override), or a model-mode `expect:` on a `tool`/`use`
/// effect. A `use` effect's own body never counts on its own (the
/// placeholder below exists precisely for a default reached *only*
/// through one) -- only its own `expect:`, when present and model-mode,
/// does. Lane G (#498) flips `Supported::prompt` to `true`, so a
/// `prompt` effect itself no longer refuses outright under this
/// build's own `Supported` -- this predicate still matters for it,
/// now for a different reason: catching an *unbuildable* default
/// adapter before a `prompt` ever dispatches to it (this module's own
/// up-front refusal, below). Lane G2 (#513) flips `Supported::
/// model_condition`/`model_expect` to `true`, so a model-mode
/// `if`/`while`/`expect:` no longer refuses outright either -- this
/// predicate already matched `Condition::Model`/`ExpectCondition::
/// Model` from the start (lane R, #495), so it needed no change of
/// its own once G2 landed. This predicate's own unit tests exercise
/// it directly, against a hand-built `Op`, rather than through a real
/// run.
fn default_adapter_called_directly(op: &electricity_bytecode::Op) -> bool {
    use electricity_bytecode::{Condition, ExpectCondition, LeafKind, LoopSpec, NodeKind, Region};

    fn region_calls_default_adapter(region: &Region) -> bool {
        match region {
            Region::Block { ops, .. } => ops.iter().any(default_adapter_called_directly),
            Region::Parallel { branches, .. } => {
                branches.iter().any(default_adapter_called_directly)
            }
            Region::TryFinally { body, finally } => {
                region_calls_default_adapter(body) || region_calls_default_adapter(finally)
            }
            Region::If {
                cond, then_, else_, ..
            } => {
                matches!(cond, Condition::Model { .. })
                    || region_calls_default_adapter(then_)
                    || else_.as_deref().is_some_and(region_calls_default_adapter)
            }
            Region::Loop { spec, body, .. } => {
                matches!(spec, LoopSpec::While(Condition::Model { .. }))
                    || region_calls_default_adapter(body)
            }
        }
    }

    if !op.enabled {
        return false;
    }
    match &op.kind {
        NodeKind::Leaf(leaf) => match leaf.as_ref() {
            LeafKind::Prompt(_) => true,
            LeafKind::Use(use_op) => {
                matches!(&use_op.expect, Some(ExpectCondition::Model { .. }))
            }
            LeafKind::Tool(tool) => {
                matches!(&tool.expect, Some(ExpectCondition::Model { .. }))
            }
            LeafKind::Reflector(reflector) => region_calls_default_adapter(&reflector.inner),
            LeafKind::Yield(_) => false,
        },
        NodeKind::Control(region) => region_calls_default_adapter(region),
    }
}

/// Issue #518 (lane J2), Scope §2: the first (in document order)
/// *enabled* prompt under *op* whose own `provider:` names an adapter
/// Circuitry's own registry knows (`electricity_vm::adapter_names::
/// ADAPTER_NAMES`) but this build has no builder for. A document can
/// also set a prompt's `provider:` directly (`compile/leaves.rs`'s own
/// `PromptOp::provider`), not only a profile override -- but that case
/// is already caught earlier, by `unbuildable_known_adapter` walking
/// the raw document (`allowlist.rs::walk_effects` collects a prompt's
/// own `provider:` the same way it collects the document's top-level
/// `adapter:`), so by the time this function runs on the overlaid
/// *program*, the only way an unbuildable name reaches it unrefused is
/// a profile override replacing a buildable (or absent) document value
/// with an unbuildable one. Never recurses into a `use` leaf (its
/// child is compiled, and so overlaid, independently at run time -- out
/// of scope here, same as every other static walk in this file) nor
/// into a disabled subtree (`op.enabled == false`): a disabled prompt's
/// provider, overridden or not, is never dispatched.
fn profile_provider_unbuildable(
    op: &electricity_bytecode::Op,
    adapters: &electricity_vm::adapter::AdapterRegistry,
) -> Option<String> {
    use electricity_bytecode::{LeafKind, NodeKind, Region};

    fn region_find(
        region: &Region,
        adapters: &electricity_vm::adapter::AdapterRegistry,
    ) -> Option<String> {
        match region {
            Region::Block { ops, .. } => ops
                .iter()
                .find_map(|op| profile_provider_unbuildable(op, adapters)),
            Region::Parallel { branches, .. } => branches
                .iter()
                .find_map(|op| profile_provider_unbuildable(op, adapters)),
            Region::TryFinally { body, finally } => {
                region_find(body, adapters).or_else(|| region_find(finally, adapters))
            }
            Region::If { then_, else_, .. } => region_find(then_, adapters)
                .or_else(|| else_.as_deref().and_then(|r| region_find(r, adapters))),
            Region::Loop { body, .. } => region_find(body, adapters),
        }
    }

    if !op.enabled {
        return None;
    }
    match &op.kind {
        NodeKind::Leaf(leaf) => match leaf.as_ref() {
            LeafKind::Prompt(prompt_op) => {
                let provider = prompt_op.provider.as_deref()?;
                let adapter_name = electricity_config::provider_token_to_adapter(provider)?;
                let normalized =
                    electricity_value::pycompat::py_strip(&adapter_name).to_lowercase();
                let buildable = adapters.supported_names();
                if electricity_vm::adapter_names::ADAPTER_NAMES.contains(&normalized.as_str())
                    && !buildable.contains(&normalized)
                {
                    Some(normalized)
                } else {
                    None
                }
            }
            LeafKind::Reflector(reflector) => region_find(&reflector.inner, adapters),
            LeafKind::Tool(_) | LeafKind::Use(_) | LeafKind::Yield(_) => None,
        },
        NodeKind::Control(region) => region_find(region, adapters),
    }
}

/// The preview-marker refusal for a document/config that configures
/// persistence or runtime plugins (issue #431's "Out of scope"
/// section: "refused with the preview marker, after the step-6/9
/// validation" -- PR #441 review finding 4) -- *reason* names what was
/// configured ("runtime.persistence"/"runtime plugins (...)"). No
/// `EffectPath` to report, unlike [`format_refusal`]'s own per-effect
/// refusals: this is a run-level setting, not a node in the compiled
/// tree.
fn format_config_refusal(reason: &str) -> String {
    format!(
        "electricity {VERSION} {PREVIEW_MARKER}: {reason} is not \
         supported until a later milestone; use `cof run` instead"
    )
}

/// *runtime*'s own `max_concurrency`/`concurrency_groups`, already
/// validated (the caller only ever reaches this after `electricity_
/// compiler::pre_state_checks` -- which runs the same parse internally
/// for its own config-error check -- has returned `Ok`) -- parsed again
/// here, directly, since that validation is `electricity-compiler`'s
/// own private helper, not a public seam this crate can call to get the
/// numbers back out. `electricity_vm::Limiter::with_limits` takes it
/// from here.
fn concurrency_limits(runtime: Option<&Value>) -> (Option<usize>, Vec<(String, usize)>) {
    let dict = runtime.and_then(Value::as_dict);
    let max_concurrency = dict
        .and_then(|d| d.get(&Value::Str("max_concurrency".to_string())))
        .and_then(Value::as_int)
        .map(|n| n.to_f64().max(0.0) as usize);
    let groups = dict
        .and_then(|d| d.get(&Value::Str("concurrency_groups".to_string())))
        .and_then(Value::as_dict)
        .map(|groups| {
            groups
                .iter()
                .filter_map(|(name, limit)| {
                    let name = name.as_str()?.to_string();
                    let limit = limit.as_int()?.to_f64().max(0.0) as usize;
                    Some((name, limit))
                })
                .collect()
        })
        .unwrap_or_default();
    (max_concurrency, groups)
}

/// *path*'s own directory, resolved (symlinks followed) the same way
/// Python's `orchestration_path.resolve().parent` is -- falling back to
/// the unresolved, merely-absolute form on an `io::Error` (a path that
/// no longer exists between the earlier load and here), since this
/// string only ever lands in `runtime.effective_settings.runtime.
/// _orchestration_dir` metadata, never anything this crate's own checks
/// rely on.
fn orchestration_dir_string(path: &std::path::Path) -> String {
    let resolved = path
        .canonicalize()
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default().join(path));
    resolved.parent().unwrap_or(&resolved).display().to_string()
}

/// *effective_runtime* (already deep-merged config+document), with the
/// three private keys `cli/runtime_shim.py::run` installs into its own
/// `runtime_config` bag before any tool dispatches -- `_orchestration_
/// dir`, `_allowlists`, `_capability_allow` (issue #431's run-wiring
/// step 8; `electricity-config`'s own `vm-lanes.md` row names this as
/// this lane's first caller). The same `Value` this function returns is
/// both [`RunContext::runtime_config`] (what a tool dispatch actually
/// reads) and, after [`electricity_redaction::redact`], `state.runtime.
/// effective_settings.runtime` (confirmed byte-for-byte against the
/// golden run corpus's own `json_parse_failure` case) -- one value, not
/// two that could drift apart.
fn build_runtime_config(
    effective_runtime: Option<&Value>,
    cfg: &CircuitryConfig,
    orchestration_dir: &str,
) -> Value {
    let mut dict = effective_runtime
        .and_then(Value::as_dict)
        .cloned()
        .unwrap_or_default();
    dict.insert(
        Value::Str("_orchestration_dir".to_string()),
        Value::Str(orchestration_dir.to_string()),
    );
    let allowlists = electricity_config::allowlists(cfg);
    let mut allowlists_dict = Dict::new();
    allowlists_dict.insert(
        Value::Str("adapters".to_string()),
        match allowlists.adapters {
            Some(names) => Value::List(names.into_iter().map(Value::Str).collect()),
            None => Value::None,
        },
    );
    allowlists_dict.insert(
        Value::Str("tools".to_string()),
        match allowlists.tools {
            Some(names) => Value::List(names.into_iter().map(Value::Str).collect()),
            None => Value::None,
        },
    );
    dict.insert(
        Value::Str("_allowlists".to_string()),
        Value::Dict(allowlists_dict),
    );
    dict.insert(
        Value::Str("_capability_allow".to_string()),
        Value::List(
            electricity_config::capability_allow()
                .into_iter()
                .map(Value::Str)
                .collect(),
        ),
    );
    Value::Dict(dict)
}

/// `runtime.plugins` metadata (issue #431's run-wiring step 13):
/// `contract_version`/`configured` are real (the merged plugin-name
/// list `electricity_config::effective_settings` already resolved);
/// `loaded`/`events` are always empty -- M0-H has no plugin loader of
/// its own (`electricity-config`'s own crate docs: a document that
/// configures runtime plugins is refused with the preview marker, after
/// this validation, so nothing here is ever actually instantiated).
fn plugins_meta_value(configured: &[Value]) -> Value {
    let mut dict = Dict::new();
    dict.insert(
        Value::Str("contract_version".to_string()),
        Value::Str("1".to_string()),
    );
    dict.insert(
        Value::Str("configured".to_string()),
        Value::List(configured.to_vec()),
    );
    dict.insert(Value::Str("loaded".to_string()), Value::List(Vec::new()));
    dict.insert(Value::Str("events".to_string()), Value::List(Vec::new()));
    Value::Dict(dict)
}

/// Wall-clock UTC, second precision, no offset suffix --
/// `cli/runtime_shim.py::_now_iso`'s own `datetime.now(timezone.utc)
/// .isoformat()`-shaped timestamp (`started_at`/`completed_at`).
fn now_iso() -> String {
    chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.6f+00:00")
        .to_string()
}

/// `state.runtime.last_run.totals` (issue #431's run-wiring step 17/18):
/// `effects_run` counts every completed node, root and containers
/// included, whose own `meta.completed_at` is truthy -- `Observer::
/// effect_complete`'s own *completed_at_is_truthy* parameter
/// ([`electricity_vm::RunObserver::effect_complete`]'s own doc
/// comment), matching `_TotalsAccumulator.observe`'s identical check
/// exactly. Issue #506's lane L1, review round 2: that parameter used
/// to be re-derived here by walking *this run's own root* `Store` from
/// *path* alone, which could never see a node still isolated in its
/// own `use` child's or tree branch's own `Store` at the moment its
/// own `effect_complete` fired -- silently over-counting a successful
/// `reflector` inside either container shape (the one effect type
/// that never writes `completed_at` on success at all, so it's the one
/// call `Totals::observe_complete` below must not count). Every
/// caller now decides the parameter itself, from whichever store/node
/// it already has in scope.
/// `tokens_sent`/`tokens_received`/`cost_usd` (issue #498 ruling 7) are
/// real now -- `cli/runtime_shim.py::_TotalsAccumulator`'s own
/// `_int_field(meta, "<f>_total", "<f>")`/cost-sum rules, applied to
/// every [`RunObserver::record_totals`] call (`exec::prompt::
/// execute_prompt`'s own call, immediately before `effect_complete`,
/// on every pass that writes a `meta` at all -- the reused-node case,
/// where a render failure keeps an earlier pass's `tokens_sent_total`,
/// is exactly why this reads *meta* fresh each call rather than
/// tracking its own running delta); still zero/zero/`null` for every
/// run with no prompt effect, since nothing else calls it yet.
struct Totals {
    effects_run: Cell<u64>,
    tokens_sent: Cell<i64>,
    tokens_received: Cell<i64>,
    cost_usd: Cell<Option<f64>>,
}

impl Totals {
    fn new() -> Self {
        Totals {
            effects_run: Cell::new(0),
            tokens_sent: Cell::new(0),
            tokens_received: Cell::new(0),
            cost_usd: Cell::new(None),
        }
    }

    fn observe_complete(&self) {
        self.effects_run.set(self.effects_run.get() + 1);
    }

    /// `_int_field(meta, *keys)`: the first of *keys* present on *meta*
    /// as a real (non-`bool`) int, else `0` -- `<field>_total` tried
    /// before the plain field, so a prompt's own run total (every
    /// attempt, not just the one that answered) counts when present.
    fn int_field(meta: &electricity_value::Dict, keys: &[&str]) -> i64 {
        for key in keys {
            if let Some(Value::Int(electricity_value::IntValue::Small(n))) =
                meta.get(&Value::Str((*key).to_string()))
            {
                return *n;
            }
        }
        0
    }

    /// `_TotalsAccumulator.observe`'s own tokens/cost half -- *meta* is
    /// a completing effect's own `meta` snapshot, already known to have
    /// a truthy `completed_at` by the caller (every [`RunObserver::
    /// record_totals`] call site only ever fires alongside its own
    /// `effect_complete`, which only ever happens once `meta.
    /// completed_at` is set).
    fn observe_meta(&self, meta: &Value) {
        let Some(meta) = meta.as_dict() else {
            return;
        };
        self.tokens_sent.set(
            self.tokens_sent.get() + Self::int_field(meta, &["tokens_sent_total", "tokens_sent"]),
        );
        self.tokens_received.set(
            self.tokens_received.get()
                + Self::int_field(meta, &["tokens_received_total", "tokens_received"]),
        );
        let cost = match meta.get(&Value::Str("cost_usd".to_string())) {
            Some(Value::Int(electricity_value::IntValue::Small(n))) => Some(*n as f64),
            Some(Value::Float(f)) => Some(*f),
            _ => None,
        };
        if let Some(cost) = cost {
            self.cost_usd
                .set(Some(self.cost_usd.get().unwrap_or(0.0) + cost));
        }
    }

    fn value(&self, wall_time_s: f64) -> Value {
        let mut dict = Dict::new();
        dict.insert(
            Value::Str("wall_time_s".to_string()),
            Value::from(wall_time_s),
        );
        dict.insert(
            Value::Str("effects_run".to_string()),
            Value::from(self.effects_run.get() as i64),
        );
        dict.insert(
            Value::Str("tokens_sent".to_string()),
            Value::from(self.tokens_sent.get()),
        );
        dict.insert(
            Value::Str("tokens_received".to_string()),
            Value::from(self.tokens_received.get()),
        );
        dict.insert(
            Value::Str("cost_usd".to_string()),
            self.cost_usd.get().map(Value::from).unwrap_or(Value::None),
        );
        Value::Dict(dict)
    }
}

/// Fans every [`RunObserver`] hook out to `--events`/`--live-state`
/// (issue #431's run-wiring step 16) and [`Totals`] -- the one place
/// `execute_root`'s three observer hooks (`effect_start`/
/// `effect_complete`/`dispatch`) and its `write` hook (DESIGN.md's own
/// "coalesced by the caller" rule) are composed, since `RunObserver`
/// itself has no `&mut self` of its own to fan out with (every method
/// takes `&self`; this struct's own interior state -- [`Totals`],
/// `EventLog`, `LiveStateMirror` -- is each already `Cell`/`RefCell`-
/// based for exactly that reason).
struct Observer<'a> {
    totals: &'a Totals,
    events: Option<&'a events::EventLog>,
    live_state: Option<&'a live_state::LiveStateMirror>,
    /// Issue #449's gate lane item 7's own per-instance `--events`
    /// pairing seam -- the fallback id source when `--events` itself
    /// is off, so `effect_start` always has *some* value to hand back
    /// regardless. When `--events` is on, [`events::EventLog::on_start`]'s
    /// own id is used instead (review finding 3 on PR #454: a *second*,
    /// independent counter here could never line up with `EventLog`'s
    /// own per-path stack, which is keyed by the ids `EventLog::on_start`
    /// itself hands out).
    next_instance: std::cell::Cell<electricity_vm::observer::InstanceId>,
}

impl RunObserver for Observer<'_> {
    fn effect_start(&self, path: &EffectPath) -> electricity_vm::observer::InstanceId {
        if let Some(log) = self.events {
            return log.on_start(&path.to_string());
        }
        let instance = self.next_instance.get() + 1;
        self.next_instance.set(instance);
        instance
    }

    fn effect_complete(
        &self,
        path: &EffectPath,
        instance: electricity_vm::observer::InstanceId,
        error: Option<&str>,
        completed_at_is_truthy: bool,
    ) {
        if completed_at_is_truthy {
            self.totals.observe_complete();
        }
        if let Some(log) = self.events {
            log.on_complete(&path.to_string(), instance, error);
        }
    }

    fn dispatch(&self, path: &EffectPath, branches: usize, concurrency: usize) {
        if let Some(log) = self.events {
            log.on_dispatch(&path.to_string(), branches, concurrency);
        }
    }

    fn write(&self) {
        // Finding 7's own fix: never take a store snapshot from inside
        // this hook, which fires synchronously from inside whatever
        // just wrote to the store and may still hold a borrow a frame
        // or two up its own call stack -- only ever record that a
        // change happened. `run_orchestration`'s own select loop is
        // the one place that ever turns this into a real snapshot and
        // a write (`LiveStateMirror::flush_if_due`'s own doc comment).
        if let Some(mirror) = self.live_state {
            mirror.mark_pending();
        }
    }

    fn graft_child_write(&self, node_path: &EffectPath, snapshot: &Value) {
        if let Some(mirror) = self.live_state {
            mirror.graft_child_write(node_path, snapshot);
        }
    }

    fn clear_graft(&self, node_path: &EffectPath) {
        if let Some(mirror) = self.live_state {
            mirror.clear_graft(node_path);
        }
    }

    // R4's own refinement on PR #472's review: `graft_child_write`
    // above is a no-op without a mirror, so a caller that would
    // otherwise snapshot a child store purely to call it should never
    // do so either -- true exactly when a `--live-state` mirror is
    // actually running.
    fn wants_child_snapshots(&self) -> bool {
        self.live_state.is_some()
    }

    fn record_totals(&self, meta: &Value) {
        self.totals.observe_meta(meta);
    }
}

/// `state.setdefault("runtime", {}).setdefault("last_run", {})`, then
/// `["completed_at"] = ...; ["totals"] = ...` -- issue #431's run-wiring
/// steps 17/18, applied uniformly whether `runtime.last_run` already
/// carries the full shape step 12 wrote (a failure after that point:
/// only `completed_at`/`totals` are overwritten, every other key kept)
/// or doesn't exist at all yet (a failure before it: a sparse `{
/// completed_at, totals }` is all this ever produces, matching Python's
/// own `setdefault`-only behaviour exactly -- `run_id`/`orchestration_
/// path`/... are never backfilled for a failure that happened before
/// they were ever assigned).
fn finalize_last_run(store: &Store, totals: &Totals, wall_time_s: f64) {
    let runtime = store
        .ensure_dict(&store.root, Value::Str("runtime".to_string()))
        .expect("Store::ensure_dict never fails");
    let last_run = store
        .ensure_dict(&runtime, Value::Str("last_run".to_string()))
        .expect("Store::ensure_dict never fails");
    store.set_leaf(
        &last_run,
        Value::Str("completed_at".to_string()),
        Value::Str(now_iso()),
    );
    store.set_leaf(
        &last_run,
        Value::Str("totals".to_string()),
        totals.value(wall_time_s),
    );
}

/// `state.setdefault("runtime", {}).setdefault("plugins", {})`, then
/// default `configured`/`loaded`/`events` to `[]` unless each is
/// already a list -- issue #431's run-wiring step 18's own plugins-meta
/// setdefault, applied the same way regardless of how early the failure
/// happened (see [`finalize_last_run`]'s own doc comment).
fn finalize_plugins_meta(store: &Store) {
    let runtime = store
        .ensure_dict(&store.root, Value::Str("runtime".to_string()))
        .expect("Store::ensure_dict never fails");
    let plugins = store
        .ensure_dict(&runtime, Value::Str("plugins".to_string()))
        .expect("Store::ensure_dict never fails");
    for key in ["configured", "loaded", "events"] {
        let already_list = matches!(
            plugins.borrow().get(&Value::Str(key.to_string())),
            Some(electricity_vm::store::Slot::Value(Value::List(_)))
        );
        if !already_list {
            store.set_leaf(
                &plugins,
                Value::Str(key.to_string()),
                Value::List(Vec::new()),
            );
        }
    }
}

/// The signal `token` was first cancelled with, if any -- POSIX's own
/// stable `SIGHUP`/`SIGINT`/`SIGTERM` numbers (1/2/15, identical on
/// Linux and macOS), matching the raw signum `electricity-cli`'s own
/// signal-handling task calls [`CancellationToken::request`] with.
fn signal_from_token(token: &CancellationToken) -> Option<Signal> {
    match token.signum() {
        Some(2) => Some(Signal::Sigint),
        Some(15) => Some(Signal::Sigterm),
        Some(1) => Some(Signal::Sighup),
        _ => None,
    }
}

/// The current process environment, as [`electricity_config::resolve_config`]'s
/// own `CIRCUITRY_*`-overlay parameter -- the one place this crate
/// reads the process environment for that, so [`config_error`] and
/// [`run_orchestration`] always resolve the exact same config for the
/// exact same *config_path*, never drifting apart on what counts as
/// "the environment" between the two.
///
/// Reads `std::env::vars_os()`, not `std::env::vars()` (issue #468's
/// startup-crash comment): `vars()` panics on any variable whose name
/// or value isn't valid Unicode, so a single stray non-UTF-8 variable
/// anywhere in the environment -- unrelated to this run, and not even
/// one `resolve_config` would ever look at -- would exit every
/// `electricity` run with code 101 before it did anything at all. `cof`
/// reads the same environment without trouble, because Python decodes
/// `os.environ` with `surrogateescape`, never raising. Only `CIRCUITRY_*`
/// names matter to `resolve_config`, so a non-UTF-8 name or value is
/// simply dropped here rather than surrogate-escaped to match Python
/// byte for byte -- a `CIRCUITRY_*` override is always meant to be read
/// back as text, so a non-UTF-8 one could never have been a real config
/// override to begin with.
fn current_env_vars() -> std::collections::HashMap<String, String> {
    std::env::vars_os()
        .filter_map(|(key, value)| {
            let key = key.into_string().ok()?;
            let value = value.into_string().ok()?;
            Some((key, value))
        })
        .collect()
}

/// *config_path*'s own resolved config, or its config error text on
/// failure. `electricity-cli`'s own "a config error exits 1 with
/// Circuitry's own text and writes no `--out`" special case (issue
/// #431's run-wiring step 1): Circuitry's `CircuitryGroup.invoke`
/// catches a `ConfigError` *around* the whole CLI command, before
/// `run()`'s own JSON-output logic (`json_out`/`console.print_json`)
/// is ever reached, so a config error prints nothing on stdout at
/// all -- unlike every other failure [`run_orchestration`] itself
/// reports, which always goes through that logic. A caller that wants
/// this distinction checks this function *before* building a
/// [`RunRequest`] -- resolving config here, first, is also `cof run`'s
/// own resolution order (`cli/app.py` resolves config at ~:1127,
/// before `_resolve_orchestration`/`_parse_env_vars`), so this is
/// where `electricity_config::resolve_config`'s own `log::warn!`
/// ("Unknown environment ...", issue #442) belongs for
/// `electricity-cli`'s own caller. On success, pass the `Ok` value to
/// [`run_orchestration_with_config`] rather than building a
/// [`RunRequest`] and calling [`run_orchestration`] (which resolves
/// config again, independently -- fine for a direct caller like this
/// crate's own tests, but it would log that same warning a second
/// time for a caller that already has it from here).
pub fn config_error(config_path: &Path) -> Result<CircuitryConfig, String> {
    electricity_config::resolve_config(config_path, &current_env_vars()).map_err(|err| err.0)
}

/// Runs *req* the way `electricity <config.json> <doc> ...` does (issue
/// #431's run-wiring table, steps 1-19) -- resolving the config,
/// seeding and checking the document, refusing unsupported content,
/// then executing it ([`electricity_vm::execute_root`]) with
/// `--events`/`--live-state` observing it, every error path producing
/// the same [`RunResult`] shape a success does. *token* is armed for
/// the duration of this call only -- `electricity-cli`'s own job (issue
/// #431's Signals section), not this function's.
///
/// `RunResult::state` is `None` for exactly two outcomes, both before
/// any effect could possibly have run: a config error (step 1, this
/// function's very first fallible step) and a refusal of unsupported
/// content (wired in right after every check error and before any file
/// is written, issue #408's gate lane) -- the refusal is the one place
/// after state-seeding that deliberately *discards* whatever this
/// function already wrote to *store* rather than reporting it, so a
/// refused run leaves no `--out`/`--events`/`--live-state` trace at all.
pub async fn run_orchestration(req: &RunRequest, token: &CancellationToken) -> RunResult {
    // Step 1: config -- a config error exits 1 with Circuitry's own
    // text and writes no --out (no state exists yet to write). Resolved
    // here, and only here, for a caller that hasn't already resolved it
    // itself (`electricity-cli`'s own entry point has -- see
    // [`run_orchestration_with_config`]'s own doc comment) -- this is
    // every direct caller in this crate's own tests, so this is where
    // `electricity_config::resolve_config`'s own `log::warn!` ("Unknown
    // environment ...", issue #442) fires for them.
    let env_vars = current_env_vars();
    match electricity_config::resolve_config(&req.config_path, &env_vars) {
        Ok(cfg) => run_orchestration_with_config(cfg, req, token).await,
        Err(err) => RunResult {
            ok: false,
            out_path: None,
            state: None,
            error: Some(err.0),
            warnings: Vec::new(),
            signal: None,
        },
    }
}

/// [`run_orchestration`]'s own step 1 already done by *cfg* -- the one
/// `electricity-cli` itself calls, with the `CircuitryConfig`
/// [`config_error`]'s own resolve (run first, before even the
/// orchestration-path-exists check below, to match `cof run`'s own
/// resolution order -- `cli/app.py` resolves config at ~:1127, before
/// `_resolve_orchestration` at ~:1150 and `_parse_env_vars` at ~:1270)
/// already produced. A second call to `electricity_config::resolve_config`
/// for the same invocation would log "Unknown environment ..." (issue
/// #442) a second time, where a single `cof run` only ever logs it once
/// -- so this function trusts *cfg* instead of resolving it again, and
/// is the only way to reach the rest of this run without doing so.
pub async fn run_orchestration_with_config(
    cfg: CircuitryConfig,
    req: &RunRequest,
    token: &CancellationToken,
) -> RunResult {
    let run_t0 = Instant::now();
    let mut warnings: Vec<String> = Vec::new();

    // Step 4: seed state (`input` namespace, from the CLI's own `-e`
    // entries) -- every failure from here on reports *some* state,
    // even if sparse, and carries the same `input` a `cof run` of the
    // same `-e` values would (PR #441 review finding 3).
    let store = Store::new();
    state::seed_state(&store, &req.inputs);

    // A loaded `--profile`'s own `out:` (issue #495, lane R) -- `None`
    // until a profile actually resolves one; every `fail!` from that
    // point on reports it, matching DESIGN §6.11's "a profile failure's
    // `--out` is the plain `--out` only" (a failure *before* this is
    // ever set, including the profile load itself, never has one to
    // report).
    let mut resolved_out_path: Option<std::path::PathBuf> = None;

    // A pre-execution check failure (every `fail!` call site below)
    // reports the interrupt text/signal instead of the check's own real
    // error whenever *token* was already cancelled by the time the
    // check failed -- `cli/interrupts.py`'s own handler raises
    // `KeyboardInterrupt` (or a subclass) in the main thread the moment
    // a signal arrives, at whatever bytecode boundary Python happens to
    // be at; `runtime_shim.run`'s own broad `except (Exception,
    // KeyboardInterrupt)` never distinguishes where that boundary fell,
    // so a SIGINT arriving mid-check in cof reports `Interrupted
    // (Ctrl-C/SIGINT)`/exit 130 there too, not the check's own text
    // (PR #441 review finding 8 -- this file's own earlier comment here
    // claimed the opposite, which was never actually true of cof).
    // electricity has no equivalent mid-check interrupt point of its
    // own (every check here runs to completion once started, unlike
    // Python's per-bytecode-instruction signal delivery), so this is
    // the next best thing: ask *once*, right when a check has already
    // failed, whether a signal got there first.
    macro_rules! fail {
        ($message:expr) => {{
            let totals = Totals::new();
            finalize_last_run(&store, &totals, run_t0.elapsed().as_secs_f64());
            finalize_plugins_meta(&store);
            let signal = signal_from_token(token);
            let error = match signal {
                Some(signal) => signal.interrupt_text().to_string(),
                None => $message,
            };
            return RunResult {
                ok: false,
                out_path: resolved_out_path.clone(),
                state: Some(store.saved(&store.root)),
                error: Some(error),
                warnings,
                signal,
            };
        }};
    }

    // Step 5: load the document -- a load error still writes --out.
    let mut check_options = electricity_compiler::CheckOptions {
        skip_preflight: true,
        trust_document: true,
        config_runtime: cfg.runtime.clone(),
        inputs: req.inputs.clone(),
        profile_inputs: indexmap::IndexMap::new(),
    };
    let loaded =
        match electricity_compiler::prepare_document(&req.orchestration_path, &check_options) {
            Ok(loaded) => loaded,
            Err(err) => fail!(err.to_string()),
        };

    // PR #441 review finding 1c: re-seed `state["input"]` now that the
    // real document is loaded, restoring a declared `type: string`
    // input's own raw `-e` text the same way `cli/app.py::
    // _restore_raw_text_for_string_inputs` does before `run()` is ever
    // called -- every failure from here on (allowlist, effective
    // settings, complexity, concurrency, persistence) must report that
    // restoration too, not only a failure that happens to reach
    // `build_input_namespace` (step 10) itself. Never `apply_declared_
    // inputs`'d yet (that's step 10's own job, below) -- this is still
    // just the sniffed-and-string-restored seed.
    let (restored_input, _) =
        electricity_compiler::seeded_input_namespace(&loaded.document, &check_options);
    state::replace_input_namespace(&store, restored_input);

    // `electricity-config`'s own first caller (vm-lanes.md's own row for
    // this module): between resolve_config and effective_settings, the
    // same position `runtime_shim.run` checks it in. Lane I
    // prerequisites (issue #448, item U4(b)): `electricity_compiler::
    // use_children::allowlist_denials` walks every literal `use` child
    // reachable from the document too (path and inline alike),
    // `cli/allowlist.py::check_allowlist`'s own shape -- a document
    // whose own text is clean but whose `use` child names a denied
    // adapter/tool is rejected here, by this allowlist's own denial
    // text, rather than only later by the capability refusal below.
    let allow_errors = electricity_compiler::use_children::allowlist_denials(
        &loaded.document,
        Some(&req.orchestration_path),
        cfg.enabled_adapters.as_deref(),
        cfg.enabled_tools.as_deref(),
    );
    if !allow_errors.is_empty() {
        fail!(format!(
            "Allowlist enforcement failed: {}",
            allow_errors.join("; ")
        ));
    }

    // `--profile <path>` (issue #495, lane R): loaded right after the
    // allowlist check and before effective settings (DESIGN §6.11,
    // `electricity_config::profile`'s own vm-lanes.md row) -- an
    // explicit path only, never discovered by name. A load/schema/
    // effect-path-validation failure is the run error, with state
    // (`fail!`): it has a direct Python equivalent, unlike the two
    // refusals above.
    let profile = match &req.profile_path {
        Some(path) => match electricity_compiler::profile::load_profile(path, &loaded.document) {
            Ok(profile) => Some(profile),
            Err(err) => fail!(err.to_string()),
        },
        None => None,
    };
    if let Some(profile) = &profile {
        // Profile `inputs:` become the base layer under `-e` (DESIGN
        // §6.11; `electricity_compiler::CheckOptions::profile_inputs`'s
        // own seam) -- re-seeding `state["input"]` again now, the same
        // way the step-5 reseed above already does, so every failure
        // from here on reports the profile-layered namespace too, not
        // only the one `build_input_namespace` (step 10) itself
        // produces.
        check_options.profile_inputs = profile
            .inputs
            .iter()
            .filter_map(|(k, v)| k.as_str().map(|k| (k.to_string(), v.clone())))
            .collect();
        let (restored_input, _) =
            electricity_compiler::seeded_input_namespace(&loaded.document, &check_options);
        state::replace_input_namespace(&store, restored_input);
    }

    // Step 6 (part 1): effective settings -- model/adapter/plugins/
    // runtime merge and `sources`, *document_name* naming the "Applied
    // host settings" notice exactly as `resolve_effective_settings`'s
    // own parameter of the same name does. `effective_settings_with_
    // profile` layers a loaded profile's own `model`/`adapter`/`out`/
    // `persistence` in at Python's own precedence (issue #495).
    let document_name = req.orchestration_path.file_name().and_then(|n| n.to_str());
    let mut effective = match electricity_config::effective_settings_with_profile(
        &cfg,
        &loaded.document,
        document_name,
        profile.as_ref(),
    ) {
        Ok(effective) => effective,
        Err(err) => fail!(err.0),
    };
    warnings.extend(effective.warnings.clone());
    // A loaded profile's own `out:` (DESIGN §6.11) -- the plain `--out`
    // still outranks it (set below, at the same point the existing
    // `sources["out"] = "cli"` override already runs); every `fail!`
    // from here on reports whichever of the two actually applies.
    resolved_out_path = effective.out.clone();

    // Step 6 (part 2)/7/9: complexity validation (before the limiter),
    // the concurrency-config-error check, persistence validation
    // (after the limiter) -- everything `build_input_namespace` (step
    // 10) itself needs to *not* have run yet (PR #441 review finding
    // 1b): a failure here means `check_interface_inputs` never got a
    // chance to touch `state["input"]` at all, so it must stay exactly
    // what the step-5 reseed above just left it as -- never the
    // partial, best-effort coercion pass that's only ever correct for
    // a step-10 failure itself.
    if let Err(err) = electricity_compiler::pre_input_checks(&loaded, effective.runtime.as_ref()) {
        fail!(err.to_string());
    }

    // Profile routing pins and provider denials (issue #495, lane R):
    // run only when `profile.effects` is non-empty, after concurrency
    // and persistence validation (`pre_input_checks` bundles complexity,
    // concurrency and persistence into one call -- not this lane's own
    // seam to split, so this sits after all three rather than strictly
    // between concurrency and persistence as `cli/runtime_shim.py` has
    // it; the PR body records this divergence, visible only when a
    // document fails both at once).
    if let Some(profile) = &profile {
        if !profile.effects.is_empty() {
            let routing =
                electricity_config::resolve_complexity_settings(effective.runtime.as_ref())
                    .map(|c| c.routing)
                    .unwrap_or_default();
            if let Err(err) = electricity_config::validate_profile_routing_pins(
                &profile.effects,
                &routing,
                &profile.name,
            ) {
                fail!(err.to_string());
            }
            let profile_denials = electricity_config::profile_provider_denials(
                &profile.effects,
                cfg.enabled_adapters.as_deref(),
            );
            if !profile_denials.is_empty() {
                fail!(format!(
                    "Allowlist enforcement failed: {}",
                    profile_denials.join("; ")
                ));
            }
        }
    }

    // After step 9's own validation: a document/config that actually
    // *configures* persistence or runtime plugins is refused with the
    // preview marker, exactly like any other unsupported content --
    // `validate_persistence`/`validate_persistence_block` (already run
    // inside `pre_input_checks`) only ever rejects a malformed block,
    // never a well-formed one, since M0-H has no real backend/loader
    // of its own to refuse through *that* path (issue #431's "Out of
    // scope" section; PR #441 review finding 4). No state at all is
    // written for this refusal, same as [`first_unsupported`]'s own --
    // and, now that this runs *before* step 10, `build_input_namespace`
    // is never even called for a run this refuses.
    if persistence_is_configured(effective.runtime.as_ref()) {
        return RunResult {
            ok: false,
            out_path: None,
            state: None,
            error: Some(format_config_refusal("runtime.persistence")),
            warnings,
            signal: None,
        };
    }
    if !effective.plugins.is_empty() {
        let names = effective
            .plugins
            .iter()
            .map(Value::py_str)
            .collect::<Vec<_>>()
            .join(", ");
        return RunResult {
            ok: false,
            out_path: None,
            state: None,
            error: Some(format_config_refusal(&format!("runtime plugins ({names})"))),
            warnings,
            signal: None,
        };
    }

    // Step 10: check_interface_inputs -- replaces `state["input"]`
    // wholesale with the coerced/defaulted namespace (PR #441 review
    // finding 1a: a key the final namespace doesn't carry forward,
    // e.g. an optional input whose seed was `null`, must not survive
    // from the step-5 reseed above).
    match electricity_compiler::build_input_namespace(&loaded.document, &check_options) {
        Ok(namespace) => state::replace_input_namespace(&store, namespace),
        Err(err) => {
            // PR #441 review finding 3: step 10 itself failed partway
            // through -- mirror Python's own in-place mutation of
            // `state["input"]` as far as it got before the first
            // violation (`build_input_namespace_best_effort`'s own doc
            // comment), replacing wholesale same as the success arm.
            let partial = electricity_compiler::build_input_namespace_best_effort(
                &loaded.document,
                &check_options,
            );
            state::replace_input_namespace(&store, partial);
            fail!(err)
        }
    }

    // Step 5 (limiter build): already-validated counts straight into
    // `Limiter::with_limits` (concurrency_limits' own doc comment).
    let (max_concurrency, groups) = concurrency_limits(effective.runtime.as_ref());
    let limiter = Limiter::with_limits(max_concurrency, groups);

    // Step 8: runtime_config -- the merged runtime block plus the three
    // private keys a tool dispatch and this run's own metadata both
    // read off the identical value.
    let orchestration_dir = orchestration_dir_string(&req.orchestration_path);
    let runtime_config_value =
        build_runtime_config(effective.runtime.as_ref(), &cfg, &orchestration_dir);

    // Step 12 (part 1): `state.setdefault("runtime", {})` -- Python's
    // own order has this *before* `_run_id`/`_timestamp` are assigned
    // (`cli/runtime_shim.py::run`, ~:651-660), so the top-level key
    // order this run's own state ends up in is `input, runtime,
    // _run_id, _timestamp, prime` (confirmed against the golden run
    // corpus's own `json_parse_failure` case).
    let runtime_node = store
        .ensure_dict(&store.root, Value::Str("runtime".to_string()))
        .expect("Store::ensure_dict never fails");

    // Step 11: run_id/timestamp.
    let run_id = uuid::Uuid::new_v4().to_string();
    let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S").to_string();
    let started_at = now_iso();
    store.set_leaf(
        &store.root,
        Value::Str("_run_id".to_string()),
        Value::Str(run_id.clone()),
    );
    store.set_leaf(
        &store.root,
        Value::Str("_timestamp".to_string()),
        Value::Str(timestamp),
    );

    // Step 12 (part 2): runtime.last_run -- `document_hash` is computed
    // independently of compilation, exactly as `cli/runtime_shim.py::run`
    // does (its own `document_content_digest` call, ~:666-683, happens
    // *before* the structural checks at ~:758 -- never gated on them
    // succeeding): a document that fails `post_state_checks` below still
    // gets a real digest here, not `null`. `None` only on an `OSError`-
    // equivalent (the file vanished between the earlier load and now).
    let document_hash = electricity_compiler::document_content_digest(
        &loaded.path,
        &loaded.document,
        &loaded.confinement_root,
    )
    .ok();

    let last_run_node = store
        .ensure_dict(&runtime_node, Value::Str("last_run".to_string()))
        .expect("Store::ensure_dict never fails");
    store.set_leaf(
        &last_run_node,
        Value::Str("run_id".to_string()),
        Value::Str(run_id.clone()),
    );
    store.set_leaf(
        &last_run_node,
        Value::Str("orchestration_path".to_string()),
        Value::Str(out::python_path_str(&req.orchestration_path)),
    );
    store.set_leaf(
        &last_run_node,
        Value::Str("document_hash".to_string()),
        document_hash.map(Value::Str).unwrap_or(Value::None),
    );
    store.set_leaf(
        &last_run_node,
        Value::Str("dry_run".to_string()),
        Value::Bool(false),
    );
    store.set_leaf(
        &last_run_node,
        Value::Str("validate_only".to_string()),
        Value::Bool(false),
    );
    store.set_leaf(
        &last_run_node,
        Value::Str("verbose".to_string()),
        Value::Bool(false),
    );
    store.set_leaf(
        &last_run_node,
        Value::Str("started_at".to_string()),
        Value::Str(started_at),
    );
    store.set_leaf(
        &last_run_node,
        Value::Str("completed_at".to_string()),
        Value::None,
    );

    // Step 13: runtime.effective_settings (redacted, limiter dropped --
    // there never was one in this Value to begin with, unlike Python's
    // own dict; `sources["out"]` is "cli" exactly when --out was given,
    // the one override this crate makes to `effective_settings`'s own
    // always-"default" entry), runtime.plugins.
    if req.out_path.is_some() {
        effective
            .sources
            .insert("out".to_string(), "cli".to_string());
    }
    let redacted_runtime = electricity_redaction::redact(runtime_config_value.clone());
    let mut effective_settings_dict = Dict::new();
    effective_settings_dict.insert(
        Value::Str("model".to_string()),
        effective
            .model
            .clone()
            .map(Value::Str)
            .unwrap_or(Value::None),
    );
    effective_settings_dict.insert(
        Value::Str("adapter".to_string()),
        effective
            .adapter
            .clone()
            .map(Value::Str)
            .unwrap_or(Value::None),
    );
    effective_settings_dict.insert(
        Value::Str("out".to_string()),
        req.out_path
            .clone()
            .or_else(|| effective.out.clone())
            .map(|p| Value::Str(out::python_path_str(&p)))
            .unwrap_or(Value::None),
    );
    effective_settings_dict.insert(
        Value::Str("plugins".to_string()),
        Value::List(effective.plugins.clone()),
    );
    effective_settings_dict.insert(Value::Str("runtime".to_string()), redacted_runtime);
    effective_settings_dict.insert(
        Value::Str("sources".to_string()),
        Value::Dict(
            effective
                .sources
                .iter()
                .map(|(k, v)| (Value::Str(k.clone()), Value::Str(v.clone())))
                .collect(),
        ),
    );
    // A loaded `--profile`'s own record (DESIGN §6.11), added after
    // `sources` (`cli/runtime_shim.py:714-718`) -- never present at all
    // for a run with no profile, matching Python's own conditional
    // `if profile is not None` key insertion exactly (never a `null`
    // placeholder entry).
    if let Some(profile) = &profile {
        let mut profile_dict = Dict::new();
        profile_dict.insert(
            Value::Str("name".to_string()),
            Value::Str(profile.name.clone()),
        );
        profile_dict.insert(
            Value::Str("content".to_string()),
            electricity_redaction::redact(profile.raw.clone()),
        );
        effective_settings_dict
            .insert(Value::Str("profile".to_string()), Value::Dict(profile_dict));
    }
    store.set_leaf(
        &runtime_node,
        Value::Str("effective_settings".to_string()),
        Value::Dict(effective_settings_dict),
    );
    store.set_leaf(
        &runtime_node,
        Value::Str("plugins".to_string()),
        plugins_meta_value(&effective.plugins),
    );

    // Step 14: structural checks, compile, groups, cycles -- `post_
    // state_checks`'s own composition (errors formatted exactly as
    // `check_for_run` already does), run only now that steps 11-13
    // have fully written `_run_id`/`_timestamp`/`runtime.last_run`/
    // `effective_settings`/`plugins` -- a failure here only overwrites
    // `last_run.completed_at`/`totals` (`fail!`'s own job), keeping
    // every field already written intact, matching `run()`'s own
    // `setdefault`-only except-block behaviour for a failure this late.
    let mut program =
        match electricity_compiler::post_state_checks(&loaded, effective.runtime.as_ref()) {
            Ok(program) => program,
            Err(err) => fail!(err.to_string()),
        };

    // Lane J2's own per-effect profile overlay (issue #518, settled
    // ruling 1): applied directly onto the compiled IR, right after
    // `post_state_checks` and before the refusal walker below -- so
    // that walker, `has_prompt_effects` and `default_adapter_called_
    // directly` (further down) all see the overlaid program, never
    // the raw one. A no-op when the loaded profile has no `effects:`
    // entries at all (including no `--profile` at all).
    if let Some(profile) = &profile {
        if !profile.effects.is_empty() {
            electricity_compiler::overrides::apply_effect_overrides(&mut program, &profile.effects);
        }
    }

    // Resolved before the refusal walker below needs it (review P2-10
    // on PR #454's second review: whether this run's own effective
    // config enables complexity decomposition isn't a property of
    // *program* itself, so the walker takes it as an argument rather
    // than re-deriving it -- narrowed to decomposition alone by issue
    // #516 §4(b), once K2 wired scoring/routing into `prompt`) -- the
    // same value `run_ctx.complexity` below reuses, resolved only
    // once.
    let complexity = electricity_config::resolve_complexity_settings(effective.runtime.as_ref())
        .unwrap_or_default();

    // This build's own capability set -- resolved once, here, so
    // `run_ctx.supported` below carries the exact same value the
    // refusal walker just checked the top-level document against,
    // rather than a second, possibly-drifting call to `Supported::
    // current` at the `RunContext` construction site.
    let supported = electricity_bytecode::Supported::current(extra_supported_providers());

    // Lane A's own refusal walker (issue #408's gate lane), wired in
    // after every check error above and before any file is written --
    // no state is written for a refusal at all (this function's own
    // doc comment).
    if let Some(refusal) = first_unsupported(&program, complexity.decomposition.enabled, &supported)
    {
        return RunResult {
            ok: false,
            out_path: None,
            state: None,
            error: Some(format_refusal(&refusal)),
            warnings,
            signal: None,
        };
    }

    // Lane I prerequisites (issue #448, item U4(a)): the same walker,
    // now also run against every literal `use` path child statically
    // reachable from the document -- a path-`use` child is itself
    // compiled and checked at run time (lane I), so this is a static
    // *prediction* of that check, not a replacement for it; inert
    // under M0 (`use` is refused first, above, before this is ever
    // reached). No state is written for this refusal either, same as
    // the top-level one just above.
    if let Some(refusal) = electricity_compiler::use_children::first_refused_child(
        &loaded.document,
        Some(&req.orchestration_path),
        complexity.decomposition.enabled,
        &supported,
    ) {
        return RunResult {
            ok: false,
            out_path: None,
            state: None,
            error: Some(refusal),
            warnings,
            signal: None,
        };
    }

    // Step 12: `needs_real_adapter`/`resolved_adapter` -- a *lenient*
    // name only (`effective.adapter or ""`, Python's own `:842-856`):
    // the strict check (`_require_resolved_settings`) is deferred past
    // preflight below, matching Python's own order, so an unknown
    // document-level `adapter:` fails with preflight's text, not an
    // early "no adapter resolved" error it would never actually hit
    // either (the name *is* present, just unbuildable -- issue #235).
    let needs_real_adapter = has_prompt_effects(&program.root);
    let resolved_adapter_name: String = if needs_real_adapter {
        effective.adapter.clone().unwrap_or_default()
    } else {
        "_noop".to_string()
    };

    // Review P2-3 on PR #454's second review: `electricity_adapters::
    // build_registry()` replaces a direct `AdapterRegistry::new()`
    // call -- lane F1/F2's own PR only ever adds a builder line
    // inside `build_registry()`'s own crate, never touches this file.
    // Lane F1 (issue #478) has registered `scripted` and `host_claude`
    // there; moved up from its old position (next to `RunContext`'s
    // own construction) so preflight and the real adapter build below
    // can both use it.
    let adapters = electricity_adapters::build_registry();

    // Settled ruling 3 (issue #495): refuse the whole run, no state,
    // for any adapter name preflight would check (the document's own
    // top-level `adapter:`/prompt `provider:`) that Circuitry itself
    // supports but this build cannot build at all -- never an ordinary
    // preflight failure. The config's own `default_adapter` is never
    // walked here (preflight never checks it either); that case is
    // [`preflight::PlaceholderAdapter`]'s own job, below.
    if let Some(name) = preflight::unbuildable_known_adapter(&loaded.document, &adapters) {
        return RunResult {
            ok: false,
            out_path: None,
            state: None,
            error: Some(format_config_refusal(&format!("adapter '{name}'"))),
            warnings,
            signal: None,
        };
    }

    // Review finding 1 on PR #526's own review: the same refusal as
    // just above, but for a *tool* plugin Circuitry knows
    // (`electricity_tools::plugin_names::PLUGIN_NAMES`) but this build
    // has no builder for -- read off the raw document, so a profile
    // that disables the tool still refuses it, matching the same
    // document-level `UnsupportedToolProvider` refusal an *enabled*
    // instance of the same tool would already get from the walker
    // below. Without this, a disabled tool naming such a plugin would
    // reach preflight's own "Unknown plugin" failure instead, once the
    // refusal walker started skipping disabled subtrees for issue #518.
    if let Some(name) = preflight::unbuildable_known_tool_plugin(&loaded.document) {
        return RunResult {
            ok: false,
            out_path: None,
            state: None,
            error: Some(format_config_refusal(&format!("tool provider '{name}'"))),
            warnings,
            signal: None,
        };
    }

    // Issue #518 (lane J2), Scope §2/settled ruling 6: the same refusal
    // as just above, but for a profile `provider:` override landing on
    // an *enabled* prompt -- read off the already-overlaid *program*,
    // never the raw document, since the override itself only exists
    // once the overlay above has applied it. Only a prompt is checked
    // here: a tool's own swapped `provider:` is already caught by the
    // refusal walker's `UnsupportedToolProvider` check (settled ruling
    // 6's own second sentence), which walks the overlaid IR too
    // (`first_unsupported` above). Without this check,
    // `generate_with_fallbacks` would instead refuse the run partway
    // through, after any earlier effect had already run.
    if let Some(name) = profile_provider_unbuildable(&program.root, &adapters) {
        return RunResult {
            ok: false,
            out_path: None,
            state: None,
            error: Some(format_config_refusal(&format!("adapter '{name}'"))),
            warnings,
            signal: None,
        };
    }

    // Step 13: preflight -- always runs (settled ruling 1: no
    // `--skip-preflight`/`--dry-run` in electricity's own CLI shape).
    // `config.runtime`, never the merged `effective.runtime` (a
    // document-level `runtime.adapters.scripted.replies_file` is
    // invisible to preflight but used by the run).
    let preflight_results =
        preflight::run_preflight(&loaded.document, cfg.runtime.as_ref(), &adapters).await;
    let (hard_results, soft_results) = electricity_config::preflight::classify_preflight_results(
        &loaded.document,
        preflight_results,
        electricity_vm::adapter_names::ADAPTER_NAMES,
    );
    let preflight_errors = electricity_config::preflight::format_preflight_errors(&hard_results);
    if !preflight_errors.is_empty() {
        fail!(format!(
            "Preflight failed: {}. Re-run with --skip-preflight to bypass.",
            preflight_errors.join("; ")
        ));
    }
    warnings.extend(electricity_config::preflight::format_preflight_warnings(
        &soft_results,
    ));
    warnings.extend(preflight::image_asset_warnings(
        &loaded.document,
        &resolved_adapter_name,
        effective.runtime.as_ref(),
        &adapters,
    ));

    // Step 14: the adapter build. `cli/runtime_shim.py::run`'s own
    // `_require_resolved_settings`, deferred to this same relative
    // position (right before the adapter is actually built) -- Python
    // checks `effective.adapter` first, so a document missing both
    // reports only the adapter error. `fail!` (not a bare `state: None`
    // `RunResult`, unlike every refusal above, which are electricity-
    // only preview concepts with no Python equivalent): this failure
    // has a direct one, and `cof run` writes `--out` with whatever
    // state existed when it raised.
    let (default_adapter, adapter_timeout_seconds): (
        Rc<dyn electricity_vm::adapter::Adapter>,
        u32,
    ) = if needs_real_adapter {
        let Some(adapter_name) = effective.adapter.as_deref().filter(|s| !s.is_empty()) else {
            fail!(format!(
                "No adapter resolved for orchestration {}. Set 'adapter' in the orchestration \
                     or 'default_adapter' in config.json. (adapter source: {})",
                out::python_path_str(&req.orchestration_path),
                effective
                    .sources
                    .get("adapter")
                    .map(String::as_str)
                    .unwrap_or("None")
            ));
        };
        if effective
            .model
            .as_deref()
            .filter(|s| !s.is_empty())
            .is_none()
        {
            fail!(format!(
                "No model resolved for orchestration {}. Set 'model' in the orchestration or \
                     'default_model' in config.json. (model source: {})",
                out::python_path_str(&req.orchestration_path),
                effective
                    .sources
                    .get("model")
                    .map(String::as_str)
                    .unwrap_or("None")
            ));
        }
        if let Err(denial) =
            electricity_config::require_adapter(adapter_name, cfg.enabled_adapters.as_deref())
        {
            fail!(denial);
        }
        let adapter: Rc<dyn electricity_vm::adapter::Adapter> = match adapters
            .build(adapter_name, &runtime_config_value)
        {
            Ok(built) => Rc::from(built),
            Err(err) => {
                let normalized = electricity_value::pycompat::py_strip(adapter_name).to_lowercase();
                let known_but_unbuildable = electricity_vm::adapter_names::ADAPTER_NAMES
                    .contains(&normalized.as_str())
                    && !adapters.supported_names().contains(&normalized);
                if known_but_unbuildable && default_adapter_called_directly(&program.root) {
                    // Settled ruling 3, widened for lane G (issue
                    // #498): the root document itself, outside
                    // any `use` child, has an effect that would
                    // call this adapter directly (a `prompt`, a
                    // model-mode `if`/`while`, or a model-mode
                    // `expect:`) -- refuse the whole run up
                    // front, statelessly, the same preview text
                    // as the document-referenced-name refusal
                    // above, rather than install a placeholder
                    // that would just defer the identical failure
                    // to the first `generate` call.
                    return RunResult {
                        ok: false,
                        out_path: None,
                        state: None,
                        error: Some(format_config_refusal(&format!("adapter '{normalized}'"))),
                        warnings,
                        signal: None,
                    };
                } else if known_but_unbuildable {
                    // Reachable only when a `use` effect alone
                    // makes `needs_real_adapter` true -- `generate`
                    // is never actually called on this placeholder
                    // in that case (checked just above).
                    Rc::new(preflight::PlaceholderAdapter::new(normalized))
                } else {
                    fail!(err.message);
                }
            }
        };
        let timeout = electricity_config::configured_timeout_seconds(
            adapter_name,
            effective.runtime.as_ref(),
        )
        .and_then(|seconds| u32::try_from(seconds).ok())
        .unwrap_or(120);
        (adapter, timeout)
    } else {
        (Rc::new(electricity_vm::adapter::NoopAdapter), 120)
    };
    let model_locked = effective.model_locked;

    // Step 16: the live-state mirror (first write synchronous and
    // fatal), then EventLog + run_start.
    let live_mirror = match &req.live_state_path {
        Some(path) => {
            let mirror = live_state::LiveStateMirror::new(path.clone());
            if let Err(err) = mirror.write_initial(&store.saved(&store.root)) {
                fail!(format!(
                    "Could not write --live-state {}: {err}",
                    path.display()
                ));
            }
            Some(mirror)
        }
        None => None,
    };
    let event_log = req.events_path.as_ref().map(|path| {
        let log = events::EventLog::open(path);
        log.run_start(
            &run_id,
            req.orchestration_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(""),
        );
        log
    });

    let totals = Totals::new();
    let observer = Observer {
        totals: &totals,
        events: event_log.as_ref(),
        live_state: live_mirror.as_ref(),
        next_instance: std::cell::Cell::new(0),
    };

    let model = effective.model.clone().unwrap_or_default();
    let registry = tool_registry();
    let orchestration_dir: &Path = program
        .document
        .as_ref()
        .map(|doc| doc.resolved_directory.as_path())
        .unwrap_or_else(|| Path::new("."));
    let run_ctx = RunContext {
        registry: &registry,
        limiter: &limiter,
        model: &model,
        // Step 15: `resolved_adapter_name` -- `_noop` unless
        // `has_prompt_effects` found a real need for one, matching
        // `cof run`'s own `prime.meta.adapter` for a `use`/`prompt`/
        // model-condition document even though M0-H still refuses
        // every one of those before building a real adapter *object*
        // (`default_adapter` below stays the no-op one regardless).
        adapter: &resolved_adapter_name,
        runtime_config: &runtime_config_value,
        dry_run: false,
        armed: req.armed,
        adapters: &adapters,
        default_adapter,
        model_locked,
        adapter_timeout_seconds,
        complexity: &complexity,
        decomposition_depth: 0,
        orchestration_dir,
        declared_prompts: &program.prompts,
        effect_names: &program.effect_names,
        supported,
    };

    // Step 17/18: execute the root, then the success or failure tail.
    // With `--live-state`, `tokio::select!`s this future against a
    // plain poll tick rather than just `.await`ing it directly, so its
    // own pending-change flush (finding 7: see `live_state.rs`'s own
    // doc comment) gets a chance to run between `execute_root`'s own
    // polls -- never while it's still holding control, so never while
    // any node's own store borrow from that exact poll could still be
    // live. The tick itself is far shorter than `LIVE_STATE_INTERVAL`
    // (`flush_if_due` is the one place that actually enforces that
    // interval; this is only how often this loop gets to ask). Without
    // `--live-state` there is nothing for that tick to ever do, so this
    // just `.await`s *exec_fut* directly instead (PR #441 review
    // finding 10) -- `tokio::time::sleep` needs a timer driver
    // (`Builder::enable_time`/`enable_all`), which a caller embedding
    // this crate's own public `run_orchestration` on a runtime built
    // without one (this crate's own tests included, when they build a
    // bare `rt` runtime rather than `electricity-cli`'s `enable_all`
    // one) would otherwise panic on, for a feature it never asked for.
    let ctx_snapshot = store.snapshot(&store.root);
    let mut exec_fut = std::pin::pin!(electricity_vm::execute_root(
        &program,
        &store,
        &ctx_snapshot,
        &run_ctx,
        &observer,
        token,
    ));
    let exec_result = match &live_mirror {
        Some(mirror) => {
            const LIVE_STATE_POLL_TICK: std::time::Duration = std::time::Duration::from_millis(50);
            loop {
                tokio::select! {
                    result = &mut exec_fut => break result,
                    () = tokio::time::sleep(LIVE_STATE_POLL_TICK) => {
                        mirror.flush_if_due(|| store.saved(&store.root));
                    }
                }
            }
        }
        None => exec_fut.await,
    };

    let wall_time_s = run_t0.elapsed().as_secs_f64();
    let (ok, error, signal) = match exec_result {
        Ok(()) => {
            let last_run_node = store
                .ensure_dict(&runtime_node, Value::Str("last_run".to_string()))
                .expect("Store::ensure_dict never fails");
            store.set_leaf(
                &last_run_node,
                Value::Str("completed_at".to_string()),
                Value::Str(now_iso()),
            );
            store.set_leaf(
                &last_run_node,
                Value::Str("totals".to_string()),
                totals.value(wall_time_s),
            );
            (true, None, None)
        }
        // Only `VmError::Cancelled` -- the VM's own cancellation signal,
        // not merely "the token happens to be set" -- ever becomes the
        // interrupt text or a non-`None` `RunResult.signal` (PR #441
        // review finding 13): that variant's own doc comment names
        // this exact caller as the one place its token/signum gets
        // turned into `RunResult`'s own wording. Any other `VmError`
        // keeps its own real message, exit 1, even if a signal arrived
        // during this run (DESIGN §6.5's cancellation never masks an
        // unrelated failure).
        Err(err @ electricity_vm::VmError::Cancelled) => {
            finalize_last_run(&store, &totals, wall_time_s);
            finalize_plugins_meta(&store);
            let signal = signal_from_token(token);
            let message = signal
                .map(|s| s.interrupt_text().to_string())
                .unwrap_or_else(|| err.to_string());
            (false, Some(message), signal)
        }
        Err(err) => {
            finalize_last_run(&store, &totals, wall_time_s);
            finalize_plugins_meta(&store);
            (false, Some(err.to_string()), None)
        }
    };

    // Step 19: live-state final write, then run_end (with signal),
    // then close -- a failed write to either folds into one warning,
    // never changing the run's own result.
    let final_state = store.saved(&store.root);
    if let Some(mirror) = &live_mirror {
        if mirror.close(&final_state) {
            warnings.push(format!(
                "Could not keep --live-state {} in sync with the run; see the log for details.",
                req.live_state_path.as_ref().unwrap().display()
            ));
        }
    }
    if let Some(log) = &event_log {
        log.run_end(ok, error.as_deref(), signal.map(Signal::events_name));
        if log.close() {
            warnings.push(format!(
                "Could not keep --events {} in sync with the run; see the log for details.",
                req.events_path.as_ref().unwrap().display()
            ));
        }
    }

    RunResult {
        ok,
        out_path: resolved_out_path,
        state: Some(final_state),
        error,
        warnings,
        signal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_bytecode::effects::{
        PromptContent, PromptOp, PromptType, RetryPolicy, ToolOp, UseOp, UseSource,
    };
    use electricity_bytecode::region::{Condition, ExpectCondition, LoopFlow, LoopSpec, Region};
    use electricity_bytecode::template::{Escape, TemplateText};
    use electricity_bytecode::{EffectPath, LeafKind, NodeKind, OnError};

    fn leaf(kind: LeafKind) -> electricity_bytecode::Op {
        electricity_bytecode::Op {
            path: EffectPath::root().push_name("n"),
            name: Some("n".to_string()),
            kind: NodeKind::Leaf(Box::new(kind)),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        }
    }

    fn block(ops: Vec<electricity_bytecode::Op>) -> electricity_bytecode::Op {
        electricity_bytecode::Op {
            path: EffectPath::root(),
            name: Some("prime".to_string()),
            kind: NodeKind::Control(Region::Block {
                ops,
                overlay: false,
            }),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        }
    }

    fn tool_op() -> ToolOp {
        ToolOp {
            provider: "json".to_string(),
            params: electricity_bytecode::ParamNode::Map(indexmap::IndexMap::new()),
            params_json: None,
            prompt: None,
            model: None,
            timeout_ms: None,
            retries: RetryPolicy::default(),
            expect: None,
            description: None,
            group: None,
        }
    }

    fn prompt_op() -> PromptOp {
        PromptOp {
            content: PromptContent::Template(TemplateText::new(
                "hi".to_string(),
                false,
                Escape::None,
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

    fn use_op() -> UseOp {
        UseOp {
            source: UseSource::Path("child.yml".to_string()),
            inputs: None,
            outputs: None,
            validate: true,
            retries: RetryPolicy::default(),
            expect: None,
            description: None,
        }
    }

    #[test]
    fn a_prompt_effect_calls_the_default_adapter_directly() {
        let op = block(vec![leaf(LeafKind::Prompt(prompt_op()))]);
        assert!(default_adapter_called_directly(&op));
    }

    #[test]
    fn a_plain_use_effect_never_calls_the_default_adapter_on_its_own() {
        let op = block(vec![leaf(LeafKind::Use(use_op()))]);
        assert!(!default_adapter_called_directly(&op));
    }

    #[test]
    fn a_use_effects_own_model_mode_expect_calls_the_default_adapter() {
        let op = block(vec![leaf(LeafKind::Use(UseOp {
            expect: Some(ExpectCondition::Model {
                template: TemplateText::new("ok?".to_string(), false, Escape::None),
            }),
            ..use_op()
        }))]);
        assert!(default_adapter_called_directly(&op));
    }

    #[test]
    fn a_tools_own_model_mode_expect_calls_the_default_adapter() {
        let op = block(vec![leaf(LeafKind::Tool(ToolOp {
            expect: Some(ExpectCondition::Model {
                template: TemplateText::new("ok?".to_string(), false, Escape::None),
            }),
            ..tool_op()
        }))]);
        assert!(default_adapter_called_directly(&op));
    }

    #[test]
    fn a_tools_own_cel_expect_never_calls_the_default_adapter() {
        let op = block(vec![leaf(LeafKind::Tool(ToolOp {
            expect: Some(ExpectCondition::Cel {
                expr: "true".to_string(),
            }),
            ..tool_op()
        }))]);
        assert!(!default_adapter_called_directly(&op));
    }

    #[test]
    fn a_model_mode_if_calls_the_default_adapter() {
        let op = electricity_bytecode::Op {
            path: EffectPath::root(),
            name: Some("prime".to_string()),
            kind: NodeKind::Control(Region::If {
                cond: Condition::Model {
                    template: TemplateText::new("ok?".to_string(), false, Escape::None),
                },
                then_: Box::new(Region::Block {
                    ops: vec![],
                    overlay: false,
                }),
                else_: None,
                threshold: 0.5,
            }),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        assert!(default_adapter_called_directly(&op));
    }

    #[test]
    fn a_cel_mode_if_never_calls_the_default_adapter() {
        let op = electricity_bytecode::Op {
            path: EffectPath::root(),
            name: Some("prime".to_string()),
            kind: NodeKind::Control(Region::If {
                cond: Condition::Cel {
                    expr: "true".to_string(),
                    strict: false,
                },
                then_: Box::new(Region::Block {
                    ops: vec![leaf(LeafKind::Use(use_op()))],
                    overlay: false,
                }),
                else_: None,
                threshold: 0.5,
            }),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        assert!(!default_adapter_called_directly(&op));
    }

    #[test]
    fn a_model_mode_while_loop_calls_the_default_adapter() {
        let op = electricity_bytecode::Op {
            path: EffectPath::root(),
            name: Some("prime".to_string()),
            kind: NodeKind::Control(Region::Loop {
                name: None,
                spec: LoopSpec::While(Condition::Model {
                    template: TemplateText::new("ok?".to_string(), false, Escape::None),
                }),
                body: Box::new(Region::Block {
                    ops: vec![],
                    overlay: false,
                }),
                flow: LoopFlow::Chain,
                max_concurrency: None,
                max_iterations: None,
                min_iterations: 0,
                collect: None,
            }),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        assert!(default_adapter_called_directly(&op));
    }

    #[test]
    fn a_disabled_prompt_effect_never_calls_the_default_adapter() {
        let mut prompt_leaf = leaf(LeafKind::Prompt(prompt_op()));
        prompt_leaf.enabled = false;
        assert!(!default_adapter_called_directly(&prompt_leaf));
    }

    #[test]
    fn version_string_has_preview_notice() {
        let s = version_string();
        assert!(s.starts_with("electricity "));
        assert!(s.ends_with("(preview)"));
        assert!(s.contains(VERSION));
    }

    #[test]
    fn check_options_merges_config_runtime_and_inputs() {
        let dir = std::env::temp_dir().join(format!(
            "electricity-check-options-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("config.json");
        std::fs::write(&config, r#"{"runtime": {"max_concurrency": 3}}"#).unwrap();

        let mut inputs = indexmap::IndexMap::new();
        inputs.insert("name".to_string(), "World".to_string());
        let options = check_options(&config, &inputs);

        assert!(options.skip_preflight);
        assert!(options.trust_document);
        assert_eq!(
            options.inputs.get("name").map(String::as_str),
            Some("World")
        );
        // PR #441 review finding 14: `config_runtime` is the fully
        // resolved runtime (`SANE_DEFAULTS` deep-merged with the file),
        // not just the file's own raw `runtime:` key -- `max_concurrency`
        // (this file's own) and `adapters` (only ever a `SANE_DEFAULTS`
        // key) are both present together.
        let runtime = options.config_runtime.as_ref().unwrap().as_dict().unwrap();
        assert_eq!(
            runtime.get(&electricity_value::Value::Str(
                "max_concurrency".to_string()
            )),
            Some(&electricity_value::Value::from(3i64))
        );
        assert!(
            runtime.contains_key(&electricity_value::Value::Str("adapters".to_string())),
            "expected SANE_DEFAULTS' own adapters key to still be merged in: {runtime:?}"
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn check_options_tolerates_a_missing_config_file() {
        let options = check_options(
            Path::new("/does/not/exist/config.json"),
            &indexmap::IndexMap::new(),
        );
        assert_eq!(options.config_runtime, None);
    }

    #[test]
    fn parse_inputs_rejects_an_entry_with_no_equals_sign() {
        assert_eq!(
            parse_inputs(&["badtext".to_string()]),
            Err("Invalid -e format: 'badtext' (expected KEY=VALUE)".to_string())
        );
    }

    #[test]
    fn parse_inputs_keeps_a_repeated_key_at_its_first_position_with_the_last_value() {
        let inputs = parse_inputs(&[
            "name=Alice".to_string(),
            "age=30".to_string(),
            "name=Bob".to_string(),
        ])
        .unwrap();
        assert_eq!(inputs.keys().collect::<Vec<_>>(), vec!["name", "age"]);
        assert_eq!(inputs["name"], "Bob");
    }
}
