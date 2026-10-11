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
pub mod run;
pub mod state;

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

use electricity_bytecode::{EffectPath, Refusal, RefusalReason, first_unsupported};
use electricity_config::CircuitryConfig;
use electricity_value::{Dict, Value};
use electricity_vm::{Limiter, RunContext, RunObserver, Store};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

/// Tool providers this crate's own [`tool_registry`] actually
/// dispatches -- `json`, plus the `test-tools` cargo feature's own
/// `sleep`/`fail` when that feature is compiled in
/// (`electricity_bytecode::refusal::Supported::providers`, issue #449's
/// gate lane). Just `&["json"]` in every ordinary build --
/// `electricity-cli`'s own `Cargo.toml` never enables this feature
/// (that crate's own doc comment).
#[cfg(feature = "test-tools")]
fn supported_providers() -> &'static [&'static str] {
    &["json", "sleep", "fail"]
}

#[cfg(not(feature = "test-tools"))]
fn supported_providers() -> &'static [&'static str] {
    &["json"]
}

/// A fresh tool registry: `json` (M0-H's only real provider), plus
/// `sleep`/`fail` when the `test-tools` feature is enabled -- the one
/// place both are ever registered, so [`supported_providers`] (the
/// refusal walker's own allow-list) never drifts from what the registry
/// actually dispatches.
fn tool_registry() -> electricity_tools::ToolRegistry {
    let mut registry = electricity_tools::ToolRegistry::new();
    registry.register(Box::new(electricity_tools::json::JsonTool));
    #[cfg(feature = "test-tools")]
    {
        registry.register(Box::new(electricity_tools::test_tools::SleepTool));
        registry.register(Box::new(electricity_tools::test_tools::FailTool));
    }
    registry
}

/// `electricity_bytecode::RefusalReason` -> the human-readable half of
/// the refusal message [`format_refusal`] builds (issue #431's gate
/// lane: "the refusal message keeps the existing preview marker phrase
/// and adds the effect path and the reason").
fn refusal_reason_text(reason: &RefusalReason) -> String {
    match reason {
        RefusalReason::Prompt => "a prompt effect".to_string(),
        RefusalReason::Loop => "a loop effect".to_string(),
        RefusalReason::Use => "a use effect".to_string(),
        RefusalReason::Reflector => "a reflector effect".to_string(),
        RefusalReason::Yield => "a yield effect".to_string(),
        RefusalReason::ModelCondition => "an if/while condition in mode: model".to_string(),
        RefusalReason::ModelExpect => "an expect: in mode: model".to_string(),
        RefusalReason::DeclaredPrompts => "a document that declares prompts:".to_string(),
        RefusalReason::UnsupportedToolProvider(provider) => {
            format!("a tool effect with provider {provider:?}")
        }
        RefusalReason::PartialReference => "an unexpanded {{> name}} partial reference".to_string(),
        RefusalReason::DocumentAdapter(adapter) => format!(
            "a document that names adapter {adapter:?}, and electricity does not run preflight checks yet"
        ),
    }
}

/// The exact text a refusal reports: the preview marker phrase (kept so
/// the conformance runner's "refusal keeps the preview marker" skip
/// rule keeps working), the effect path, and the reason.
fn format_refusal(refusal: &Refusal) -> String {
    format!(
        "electricity {VERSION} is a preview and cannot run orchestrations yet: {} is {}; use `cof run` instead",
        refusal.path,
        refusal_reason_text(&refusal.reason)
    )
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
        "electricity {VERSION} is a preview and cannot run orchestrations yet: {reason} is not \
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
/// included (`RunObserver::effect_complete`'s own call count) --
/// `tokens_sent`/`tokens_received`/`cost_usd` stay at Circuitry's own
/// zero/zero/`null` defaults through M0-H, since no prompt effect (the
/// only thing that ever sets them) exists yet to report anything else.
struct Totals {
    effects_run: Cell<u64>,
}

impl Totals {
    fn new() -> Self {
        Totals {
            effects_run: Cell::new(0),
        }
    }

    fn observe_complete(&self) {
        self.effects_run.set(self.effects_run.get() + 1);
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
        dict.insert(Value::Str("tokens_sent".to_string()), Value::from(0i64));
        dict.insert(Value::Str("tokens_received".to_string()), Value::from(0i64));
        dict.insert(Value::Str("cost_usd".to_string()), Value::None);
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
    /// pairing seam -- a plain incrementing counter is enough here:
    /// M0-H never runs two overlapping calls at the same
    /// [`EffectPath`] (no `loop`, and a tree's own branches are each
    /// at their *own* concretized path), so `--events` output stays
    /// byte-identical either way; this only has to hand back a value
    /// `effect_complete` can be given.
    next_instance: std::cell::Cell<electricity_vm::observer::InstanceId>,
}

impl RunObserver for Observer<'_> {
    fn effect_start(&self, path: &EffectPath) -> electricity_vm::observer::InstanceId {
        if let Some(log) = self.events {
            log.on_start(&path.to_string());
        }
        let instance = self.next_instance.get() + 1;
        self.next_instance.set(instance);
        instance
    }

    fn effect_complete(
        &self,
        path: &EffectPath,
        _instance: electricity_vm::observer::InstanceId,
        error: Option<&str>,
    ) {
        self.totals.observe_complete();
        if let Some(log) = self.events {
            log.on_complete(&path.to_string(), error);
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
/// reads `std::env::vars()` for that, so [`config_error`] and
/// [`run_orchestration`] always resolve the exact same config for the
/// exact same *config_path*, never drifting apart on what counts as
/// "the environment" between the two.
fn current_env_vars() -> std::collections::HashMap<String, String> {
    std::env::vars().collect()
}

/// *config_path*'s own config error, if resolving it fails -- `None` on
/// success. `electricity-cli`'s own "a config error exits 1 with
/// Circuitry's own text and writes no `--out`" special case (issue
/// #431's run-wiring step 1): Circuitry's `CircuitryGroup.invoke`
/// catches a `ConfigError` *around* the whole CLI command, before
/// `run()`'s own JSON-output logic (`json_out`/`console.print_json`)
/// is ever reached, so a config error prints nothing on stdout at
/// all -- unlike every other failure [`run_orchestration`] itself
/// reports, which always goes through that logic. A caller that wants
/// this distinction checks this function *before* building a
/// [`RunRequest`] and calling [`run_orchestration`] (which still
/// resolves the same config again, internally, as step 1 of its own
/// run-wiring table -- this is deliberately not threaded through as a
/// parameter, so a direct [`run_orchestration`] caller, like this
/// crate's own tests, never has to resolve a config twice itself just
/// to get a [`RunResult`]).
pub fn config_error(config_path: &Path) -> Option<String> {
    electricity_config::resolve_config(config_path, &current_env_vars())
        .err()
        .map(|err| err.0)
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
    let run_t0 = Instant::now();
    let mut warnings: Vec<String> = Vec::new();

    // Step 1: config -- a config error exits 1 with Circuitry's own
    // text and writes no --out (no state exists yet to write).
    let env_vars = current_env_vars();
    let cfg = match electricity_config::resolve_config(&req.config_path, &env_vars) {
        Ok(cfg) => cfg,
        Err(err) => {
            return RunResult {
                ok: false,
                state: None,
                error: Some(err.0),
                warnings,
                signal: None,
            };
        }
    };

    // Step 4: seed state (`input` namespace, from the CLI's own `-e`
    // entries) -- every failure from here on reports *some* state,
    // even if sparse, and carries the same `input` a `cof run` of the
    // same `-e` values would (PR #441 review finding 3).
    let store = Store::new();
    state::seed_state(&store, &req.inputs);

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
                state: Some(store.saved(&store.root)),
                error: Some(error),
                warnings,
                signal,
            };
        }};
    }

    // Step 5: load the document -- a load error still writes --out.
    let check_options = electricity_compiler::CheckOptions {
        skip_preflight: true,
        trust_document: true,
        config_runtime: cfg.runtime.clone(),
        inputs: req.inputs.clone(),
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
    // same position `runtime_shim.run` checks it in.
    let allow_errors = electricity_config::check_allowlist(&loaded.document, &cfg);
    if !allow_errors.is_empty() {
        fail!(format!(
            "Allowlist enforcement failed: {}",
            allow_errors.join("; ")
        ));
    }

    // Step 6 (part 1): effective settings -- model/adapter/plugins/
    // runtime merge and `sources`, *document_name* naming the "Applied
    // host settings" notice exactly as `resolve_effective_settings`'s
    // own parameter of the same name does.
    let document_name = req.orchestration_path.file_name().and_then(|n| n.to_str());
    let mut effective =
        match electricity_config::effective_settings(&cfg, &loaded.document, document_name) {
            Ok(effective) => effective,
            Err(err) => fail!(err.0),
        };
    warnings.extend(effective.warnings.clone());

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
            .as_ref()
            .map(|p| Value::Str(out::python_path_str(p)))
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
    let program = match electricity_compiler::post_state_checks(&loaded, effective.runtime.as_ref())
    {
        Ok(program) => program,
        Err(err) => fail!(err.to_string()),
    };

    // Lane A's own refusal walker (issue #408's gate lane), wired in
    // after every check error above and before any file is written --
    // no state is written for a refusal at all (this function's own
    // doc comment).
    if let Some(refusal) = first_unsupported(
        &program,
        &electricity_bytecode::Supported::m0(supported_providers()),
    ) {
        return RunResult {
            ok: false,
            state: None,
            error: Some(format_refusal(&refusal)),
            warnings,
            signal: None,
        };
    }

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
    let adapters = electricity_vm::adapter::AdapterRegistry::new();
    let complexity = electricity_config::resolve_complexity_settings(effective.runtime.as_ref())
        .unwrap_or_default();
    let use_call_stack: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let orchestration_dir: &Path = program
        .document
        .as_ref()
        .map(|doc| doc.resolved_directory.as_path())
        .unwrap_or_else(|| Path::new("."));
    let run_ctx = RunContext {
        registry: &registry,
        limiter: &limiter,
        model: &model,
        // Step 15: no prompt effects exist in M0-H, so this is always
        // the no-op adapter sentinel -- `effective.adapter` (the real
        // resolved name) is still what `runtime.effective_settings.
        // adapter` reports, a separate, metadata-only field above.
        adapter: "_noop",
        runtime_config: &runtime_config_value,
        dry_run: false,
        // This function is `electricity-cli`'s own entry point -- always
        // an armed, CLI-driven run (M1-A has no embedding caller of its
        // own yet).
        armed: true,
        adapters: &adapters,
        default_adapter: Rc::new(electricity_vm::adapter::NoopAdapter),
        model_locked: false,
        adapter_timeout_seconds: 120,
        complexity: &complexity,
        decomposition_depth: 0,
        use_call_stack: &use_call_stack,
        orchestration_dir,
        declared_prompts: &program.prompts,
        effect_names: &program.effect_names,
        display_depth: 0,
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
        state: Some(final_state),
        error,
        warnings,
        signal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
