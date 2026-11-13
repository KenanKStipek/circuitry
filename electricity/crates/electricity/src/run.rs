//! `RunRequest`/`RunResult`: the shapes lane D's own `run_orchestration`
//! (issue #431's run-wiring steps 4-19) takes and returns, once it
//! replaces this crate's current preview-refusal [`crate::run_orchestration`].
//! Lane A stub: both types are this crate's final public shape for them;
//! neither is wired into anything yet.

use electricity_value::Value;
use std::path::PathBuf;

/// Everything `electricity <config.json> <doc> ...` parses off its
/// command line, bundled for `run_orchestration` the way `cli/app.py`
/// bundles its own `RunRequest` (issue #431's CLI section) -- one value
/// `electricity-cli`'s `main` builds once and passes through, rather
/// than every flag becoming its own function parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRequest {
    /// `None` for `electricity run` with no `--config` (issue #728) --
    /// this is the CLI flag's own raw value, not the config file
    /// discovery actually resolved (`--events`' own `run_start.config`
    /// mirrors `cli/runtime_shim.py::run`'s own `req.config_path.name
    /// if req.config_path is not None else None`, which is likewise
    /// the raw `--config` flag, never the resolved file). Always
    /// `Some` for the legacy `electricity <config.json> <doc>` form
    /// (its first positional).
    pub config_path: Option<PathBuf>,
    pub orchestration_path: PathBuf,
    /// `-e key=value`, in command-line order ([`crate::parse_inputs`]'s
    /// own output).
    pub inputs: indexmap::IndexMap<String, String>,
    /// `--out <path>` -- `None` prints the state to stdout instead
    /// (issue #431's "CLI output" decision).
    pub out_path: Option<PathBuf>,
    /// `--pretty` -- sorted keys, 2-space indent, vs. plain insertion-
    /// order `json.dumps` (`core/saved_state.py::dumps_saved_state`).
    pub pretty: bool,
    pub live_state_path: Option<PathBuf>,
    pub events_path: Option<PathBuf>,
    /// `--profile <path>` (issue #495, lane R) -- an explicit path
    /// only (DESIGN §1, §6.11: no name discovery, no
    /// `--profile-from-state`).
    pub profile_path: Option<PathBuf>,
    /// `--allow-capabilities a,b` (issue #728), already split and
    /// trimmed -- threaded into `runtime_config["_capability_allow"]`
    /// (`electricity_config::allowlist::capability_allow`) exactly as
    /// `cof run`'s own `RunRequest.allow_capabilities` is. Has no
    /// observable effect on any document electricity runs today (that
    /// module's own doc comment) -- ported for parity with `cof run`,
    /// not because electricity's own capability-consent gate needs it
    /// yet.
    pub allow_capabilities: Vec<String>,
    /// `--adapter <name>` (issue #728) -- this run's default adapter,
    /// ranked above a loaded `--profile`/the orchestration's own
    /// `adapter:`/the config's `default_adapter`, exactly as `cof
    /// run`'s own `--adapter`/`RunRequest.adapter_override` ranks
    /// (`cli` tier, `cli/effective_settings.py::
    /// resolve_effective_settings`).
    pub adapter_override: Option<String>,
    /// `--label TEXT` (#752, `run` only) -- literal text, never
    /// rendered, that wins over the document's own top-level `label:`
    /// template. Cleaned the same way either way
    /// (`crate::label::clean_label`) before it reaches `--events`'
    /// `run_start` or `runtime.last_run.label`, exactly as `cof run`'s
    /// own `RunRequest.label_override` does. `None` for the legacy
    /// `electricity <config.json> <doc>` form, which has no `--label`
    /// of its own.
    pub label_override: Option<String>,
    /// Warnings from resolving this run's own config, before
    /// `RunRequest` even existed (issue #728's config-discovery
    /// branch: a discovered-but-skipped project config's one line,
    /// `electricity::config_error_for_run`'s own return value) --
    /// seeded into `RunResult.warnings` ahead of anything this run
    /// itself adds, exactly as `cli/runtime_shim.py::run`'s own
    /// `warnings.extend(cfg.resolution_warnings())` runs ahead of
    /// every other `warnings.extend` call there. Always empty for the
    /// legacy explicit-`<config.json>` form, which never discovers
    /// anything to warn about.
    pub config_warnings: Vec<String>,
    /// `true` for a CLI-driven run (`ToolCall::armed`'s own doc
    /// comment, `electricity_tools::ToolCall`) -- a run option on
    /// *this* struct, not a literal `run_orchestration` hard-codes,
    /// so an embedding caller that isn't
    /// `electricity-cli` can pass `false`. `electricity-cli`'s own
    /// `main` always passes `true`: this crate has no embedding
    /// caller of its own yet (M1-A).
    pub armed: bool,
    /// `--state <path>` (issue #727) -- the whole saved state this run
    /// seeds its store from (every top-level key, not just `input`),
    /// relinking a `last` alias the same way [`electricity_vm::
    /// hydrate_resumed_state`] does for any loaded JSON object. `None`
    /// seeds the ordinary way ([`state::seed_state`]'s own `-e`-only
    /// seed).
    pub state_path: Option<PathBuf>,
    /// `--resume` (issue #727) -- with [`RunRequest::state_path`], a
    /// named effect whose saved node already finished without error is
    /// skipped and reused; every effect-type container's own partial-
    /// continue rule (a loop's first unfinished pass, a conditional's
    /// saved branch, a `use` with `runtime.state.record_children`)
    /// follows from there. Ignored with no `state_path` (cof's own
    /// `--resume last`/`--resume <run-id>` modes are out of this
    /// engine's scope: ruling 3, issue #727 -- `runtime.persistence`
    /// stays refused).
    pub resume: bool,
    /// `--force` (issue #727) -- with `resume`, continue even though
    /// *orchestration_path*'s own content changed since the saved run
    /// (`runtime.last_run.document_hash` mismatch) or it carries no
    /// recorded hash at all.
    pub force: bool,
}

/// `run_orchestration`'s own result -- success and failure both carry a
/// `state`, matching `cof run`'s own "`--out` is written on success *and*
/// failure" rule (issue #431's run-wiring step 20); `state` is `None`
/// only for a failure before any state exists at all (a config error,
/// step 1, or a bad `-e`, step 2 -- `cof run`'s own "A config error exits
/// 1 ... and writes no `--out`").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunResult {
    pub ok: bool,
    /// A loaded profile's own `out:` (issue #495, lane R) -- honoured
    /// by the caller (`electricity-cli`'s own `main`) only when no
    /// `--out` was given; `None` whenever this run never resolved a
    /// profile, or resolved one with no `out:` of its own, or failed
    /// before effective settings were ever computed (that failure's
    /// own `--out` is the plain `--out` only, DESIGN §6.11).
    pub out_path: Option<PathBuf>,
    /// The final saved state (`core/saved_state.py`'s own shape) --
    /// present whenever `--out`/stdout would have written one.
    pub state: Option<Value>,
    /// `None` on success; Circuitry's own exact failure text otherwise
    /// (a check failure, a run-time error, or the interrupt text --
    /// `Interrupted (Ctrl-C/SIGINT)`/`(SIGTERM)`/`(SIGHUP)`).
    pub error: Option<String>,
    /// `Warning: ...` lines (stderr, and the failure-JSON `warnings`
    /// field on stdout when stdout isn't a terminal).
    pub warnings: Vec<String>,
    /// The signal that ended the run, if any (SIGINT/SIGTERM/SIGHUP) --
    /// `--events`'s own `run_end.signal` and the process exit code
    /// (130/143/129) both come from this.
    pub signal: Option<Signal>,
}

/// The three signals `cof run`/`electricity` treat specially (DESIGN
/// §6.5/§6.9, issue #431's Signals section).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    Sigint,
    Sigterm,
    Sighup,
}

impl Signal {
    /// The process exit code `cof run`/`electricity` use for a run this
    /// signal ended (issue #431's run-wiring step 21).
    pub fn exit_code(self) -> i32 {
        match self {
            Signal::Sigint => 130,
            Signal::Sigterm => 143,
            Signal::Sighup => 129,
        }
    }

    /// `RunResult.error`'s own interrupt text.
    pub fn interrupt_text(self) -> &'static str {
        match self {
            Signal::Sigint => "Interrupted (Ctrl-C/SIGINT)",
            Signal::Sigterm => "Interrupted (SIGTERM)",
            Signal::Sighup => "Interrupted (SIGHUP)",
        }
    }

    /// `--events`'s own `run_end.signal` string (`cli/events.py`'s own
    /// `"SIGTERM"`/`"SIGHUP"`/`"SIGINT"`).
    pub fn events_name(self) -> &'static str {
        match self {
            Signal::Sigint => "SIGINT",
            Signal::Sigterm => "SIGTERM",
            Signal::Sighup => "SIGHUP",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_signal_has_its_own_exit_code() {
        assert_eq!(Signal::Sigint.exit_code(), 130);
        assert_eq!(Signal::Sigterm.exit_code(), 143);
        assert_eq!(Signal::Sighup.exit_code(), 129);
    }

    #[test]
    fn each_signal_has_its_own_interrupt_text() {
        assert_eq!(
            Signal::Sigint.interrupt_text(),
            "Interrupted (Ctrl-C/SIGINT)"
        );
        assert_eq!(Signal::Sigterm.interrupt_text(), "Interrupted (SIGTERM)");
        assert_eq!(Signal::Sighup.interrupt_text(), "Interrupted (SIGHUP)");
    }
}
