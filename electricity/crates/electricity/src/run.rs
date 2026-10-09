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
    pub config_path: PathBuf,
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
