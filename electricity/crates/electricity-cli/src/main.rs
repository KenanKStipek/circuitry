//! `electricity` — the CLI binary. `Action::Run` drives
//! `electricity::run_orchestration` (issue #431's run-wiring steps 1-19)
//! on a current-thread Tokio runtime, arming SIGINT/SIGTERM/SIGHUP
//! cancellation for the run alone (issue #431's Signals section),
//! writes `--out` on success *and* failure, and follows `cof`'s own
//! non-TTY stdout contract (issue #431's "CLI output" decision).
//! `--version`/`--help` succeed; `--dump-ir` runs the document check
//! alone and prints the result as JSON -- it never runs anything, and
//! so never refuses unsupported content either.
//!
//! Every flag issue #431's own usage line lists parses, in any position,
//! before or after the two positionals (`<config.json> <orchestration.yml>`)
//! -- `--out`/`--pretty`/`--live-state`/`--events` included. A value-taking
//! long flag also accepts `--flag=value`, not just `--flag value`
//! (`cof`'s own Click parser accepts both; orchestrator ruling on PR
//! #432's review) -- `=` on a boolean flag (`--pretty=true`) is a usage
//! error, never silently accepted or ignored. An unknown flag is a usage
//! error (exit 2): issue #431's gate lane tightens this from the pre-#431
//! preview CLI, which let a trailing unknown flag fall through to the
//! (always-refusing) run path instead. `--profile` refuses with the
//! preview marker (profiles are M1-I) before `run_orchestration` is ever
//! called -- no state is written for it, same as any other refusal.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use electricity::CancellationToken;
use electricity::run::{RunRequest, RunResult, Signal};

const USAGE: &str = "\
Usage: electricity <config.json> <orchestration.yml> [-e key=value]... [--out state.json]
                    [--pretty] [--live-state state.json] [--events events.jsonl]
                    [--profile <path>]
       electricity <config.json> <orchestration.yml> --dump-ir [-e key=value]...

electricity is a preview: it runs tool/dynamic/if/finally documents only
(prompt/loop/use/reflector/yield, persistence and runtime plugins are
refused with a preview marker; use `cof run` for those).

Options:
  -V, --version       Print the version and exit
  -h, --help          Print this message and exit
  -e key=value        Pass an orchestration input, checked against its
                      declared interface.inputs the same way `cof run -e`
                      does. Repeatable; a later -e for the same key wins.
  --out <path>        Write the final state to <path> instead of stdout.
  --pretty            Sort state keys and indent by 2 spaces -- the final
                      state, whether printed to stdout (no --out) or
                      written to --out's own file.
  --live-state <path> Mirror the running state to <path> as it changes.
  --events <path>     Write a JSONL event stream to <path>.
  --profile <path>    Not supported in this preview (profiles land in a
                      later milestone).
  --dump-ir           Print the compiled IR as JSON and exit. Unstable: this
                      format is a debugging aid and can change in any release.";

/// A successfully parsed `electricity <config.json> <doc> ...` run
/// request -- every flag [`USAGE`] lists that isn't `--dump-ir`/
/// `--version`/`--help`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct RunArgs {
    config: String,
    orchestration: String,
    inputs: Vec<String>,
    out: Option<String>,
    pretty: bool,
    live_state: Option<String>,
    events: Option<String>,
    /// Parsed (and its own value consumed) but never acted on in this
    /// preview -- profiles refuse the same way every other `Run` does.
    profile: Option<String>,
}

enum Action {
    Version,
    Help,
    Run(RunArgs),
    DumpIr(String, String, Vec<String>),
    UsageError(String),
}

/// One recognized flag's own arity -- whether [`parse_flags`] consumes a
/// following value for it. `KNOWN_FLAGS` is this parser's single source
/// of truth for "is this token a flag at all", so a new flag only has to
/// be added here (and to the consuming match in [`parse_flags`]) to be
/// recognized in every position.
enum Arity {
    Boolean,
    Value,
}

const KNOWN_FLAGS: &[(&str, Arity)] = &[
    ("-V", Arity::Boolean),
    ("--version", Arity::Boolean),
    ("-h", Arity::Boolean),
    ("--help", Arity::Boolean),
    ("--dump-ir", Arity::Boolean),
    ("--pretty", Arity::Boolean),
    ("-e", Arity::Value),
    ("--out", Arity::Value),
    ("--live-state", Arity::Value),
    ("--events", Arity::Value),
    ("--profile", Arity::Value),
];

fn flag_arity(flag: &str) -> Option<&'static Arity> {
    KNOWN_FLAGS
        .iter()
        .find(|(name, _)| *name == flag)
        .map(|(_, arity)| arity)
}

/// Every flag and positional in *args*, in one pass -- a token starting
/// with `-` that isn't exactly `-` (tolerated as a plain positional, the
/// conventional stdin/stdout placeholder, never a flag) and isn't in
/// [`KNOWN_FLAGS`] is an `Err` naming it (issue #431's CLI section:
/// "An unknown flag is a usage error (exit 2)"). A value-taking flag
/// with no following argument is also an `Err` (this preview's own
/// usage error; cof's own Click-layer "Option '...' requires an
/// argument." is third-party CLI framework text, not Circuitry's own, so
/// not matched word for word).
struct ParsedFlags {
    version: bool,
    help: bool,
    dump_ir: bool,
    pretty: bool,
    inputs: Vec<String>,
    out: Option<String>,
    live_state: Option<String>,
    events: Option<String>,
    profile: Option<String>,
    positionals: Vec<String>,
}

fn parse_flags(args: &[String]) -> Result<ParsedFlags, String> {
    let mut parsed = ParsedFlags {
        version: false,
        help: false,
        dump_ir: false,
        pretty: false,
        inputs: Vec::new(),
        out: None,
        live_state: None,
        events: None,
        profile: None,
        positionals: Vec::new(),
    };
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "-" {
            parsed.positionals.push(arg.to_string());
            i += 1;
            continue;
        }
        // `--name=value` (a long flag only -- getopt/Click's own
        // convention never extends this to a short flag like `-e`,
        // which instead always takes its value as a separate token):
        // split *once*, so a value that itself contains `=` (`--out
        // =a=b`, however unlikely) stays whole.
        let (flag_name, inline_value): (&str, Option<&str>) = if arg.starts_with("--") {
            match arg.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (arg, None),
            }
        } else {
            (arg, None)
        };
        let Some(arity) = flag_arity(flag_name) else {
            if arg.starts_with('-') {
                return Err(format!("unrecognized option '{arg}'"));
            }
            parsed.positionals.push(arg.to_string());
            i += 1;
            continue;
        };
        match arity {
            Arity::Boolean => {
                if inline_value.is_some() {
                    return Err(format!("{flag_name} does not take a value"));
                }
                match flag_name {
                    "-V" | "--version" => parsed.version = true,
                    "-h" | "--help" => parsed.help = true,
                    "--dump-ir" => parsed.dump_ir = true,
                    "--pretty" => parsed.pretty = true,
                    _ => unreachable!("every Arity::Boolean flag is handled above"),
                }
                i += 1;
            }
            Arity::Value => {
                let value = match inline_value {
                    Some(value) => value.to_string(),
                    None => args
                        .get(i + 1)
                        .ok_or_else(|| format!("{flag_name} requires an argument"))?
                        .clone(),
                };
                match flag_name {
                    "-e" => parsed.inputs.push(value),
                    "--out" => parsed.out = Some(value),
                    "--live-state" => parsed.live_state = Some(value),
                    "--events" => parsed.events = Some(value),
                    "--profile" => parsed.profile = Some(value),
                    _ => unreachable!("every Arity::Value flag is handled above"),
                }
                i += if inline_value.is_some() { 1 } else { 2 };
            }
        }
    }
    Ok(parsed)
}

fn classify(args: &[String]) -> Action {
    // `-V`/`--version` and `-h`/`--help` win over everything, including a
    // usage error elsewhere in the same command line -- checked directly
    // against the raw tokens, before flag validation, so `electricity
    // --bogus --version` still prints the version (pre-#431 behavior,
    // preserved).
    if args.iter().any(|a| a == "--version" || a == "-V") {
        return Action::Version;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Action::Help;
    }

    let parsed = match parse_flags(args) {
        Ok(parsed) => parsed,
        Err(message) => return Action::UsageError(message),
    };
    // `parse_flags` only ever sets `parsed.version`/`.help` via the exact
    // same tokens already handled above, so this is unreachable in
    // practice; kept for defense-in-depth if a future flag reuses them.
    if parsed.version {
        return Action::Version;
    }
    if parsed.help {
        return Action::Help;
    }

    if parsed.dump_ir {
        return match (parsed.positionals.first(), parsed.positionals.get(1)) {
            (Some(config), Some(orchestration)) => {
                Action::DumpIr(config.clone(), orchestration.clone(), parsed.inputs)
            }
            _ => Action::UsageError(
                "--dump-ir requires <config.json> <orchestration.yml>".to_string(),
            ),
        };
    }

    match (parsed.positionals.first(), parsed.positionals.get(1)) {
        (Some(config), Some(orchestration)) => Action::Run(RunArgs {
            config: config.clone(),
            orchestration: orchestration.clone(),
            inputs: parsed.inputs,
            out: parsed.out,
            pretty: parsed.pretty,
            live_state: parsed.live_state,
            events: parsed.events,
            profile: parsed.profile,
        }),
        _ => Action::UsageError("no config file or orchestration given".to_string()),
    }
}

/// The POSIX signal number [`arm_signals`]'s own task calls
/// [`CancellationToken::request`] with for each signal it watches --
/// stable across Linux and macOS (`man 7 signal`), and the same three
/// numbers [`electricity::run::Signal::events_name`]/[`Signal::
/// exit_code`] key off of.
const SIGINT: i32 = 2;
const SIGHUP: i32 = 1;
const SIGTERM: i32 = 15;

/// Whether SIGHUP is already ignored (`SIG_IGN`) on entry -- a caller
/// that launched `electricity` under `nohup` means it, and this task
/// must never install its own handler over that (issue #431's Signals
/// section: "honours `SIG_IGN` at start"). Queried directly via
/// `sigaction`, never changing the disposition itself (the *old*
/// argument is `null`).
fn sighup_is_already_ignored() -> bool {
    #[cfg(unix)]
    unsafe {
        let mut current: libc::sigaction = std::mem::zeroed();
        if libc::sigaction(libc::SIGHUP, std::ptr::null(), &mut current) != 0 {
            return false;
        }
        current.sa_sigaction == libc::SIG_IGN
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// A dedicated OS thread, with its own minimal Tokio runtime, watching
/// SIGINT/SIGTERM/SIGHUP from the moment it's installed (PR #441 review
/// finding 5) -- installed *before* config load, so this stays
/// responsive even while the main thread is deep in synchronous work
/// (config/document load, every pre-execution check) with no `await`
/// point of its own for a signal to be noticed at.
///
/// The first SIGINT/SIGTERM/SIGHUP calls [`CancellationToken::request`];
/// a second SIGINT/SIGTERM exits the whole process immediately with its
/// own conventional code (no `--out`, no `run_end` -- issue #431's
/// Signals section). SIGHUP is never treated as that second signal, and
/// is never even watched for at all when [`sighup_is_already_ignored`]
/// says it already is -- both rules live in
/// [`CancellationToken::request`]'s own "first signal wins" semantics
/// (any later call, same or different signum, returns `false`) plus
/// this function's own `continue` on a not-first SIGHUP, so a SIGHUP
/// arriving after an earlier signal already cancelled the run --
/// including one arriving after `execute_root` returns, while `--out`
/// is still being written -- is ignored the same way, with no separate
/// "are we past `--out` yet" check needed.
struct SignalGuard {
    stop: std::sync::Arc<tokio::sync::Notify>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SignalGuard {
    /// Stops watching ("signals armed for the run only") and waits for
    /// the thread to actually exit, so a caller that's about to print
    /// this run's own result never races a signal thread still capable
    /// of calling `std::process::exit` out from under it. Called once
    /// `--out` has been written -- not merely once `run_orchestration`
    /// returns -- so a SIGHUP during that write is still ignored,
    /// matching [`arm_signals`]'s own doc comment.
    fn disarm(mut self) {
        self.stop.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn arm_signals(token: CancellationToken) -> SignalGuard {
    let stop = std::sync::Arc::new(tokio::sync::Notify::new());
    let stop_for_thread = stop.clone();
    // A rendezvous, not just a fire-and-forget spawn: the real
    // `sigaction` calls below only happen once the new thread's own
    // runtime schedules this async block, which the OS is free to
    // delay arbitrarily under load -- without waiting for `ready` here,
    // a signal arriving in that window would still hit the OS's
    // default (process-killing) disposition even though this function
    // had already returned, telling its caller the run was safe to
    // start. Confirmed by a real race on a loaded machine during this
    // function's own review (PR #441): a SIGINT sent as little as a
    // few hundred milliseconds after a plain, unguarded `spawn` could
    // still arrive before the handler was installed.
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    let thread = std::thread::Builder::new()
        .name("electricity-signals".to_string())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("could not start the signal-handling thread's own runtime");
            runtime.block_on(async move {
                use tokio::signal::unix::{SignalKind, signal};
                let mut sigint =
                    signal(SignalKind::interrupt()).expect("could not install a SIGINT handler");
                let mut sigterm =
                    signal(SignalKind::terminate()).expect("could not install a SIGTERM handler");
                let mut sighup = if sighup_is_already_ignored() {
                    None
                } else {
                    Some(signal(SignalKind::hangup()).expect("could not install a SIGHUP handler"))
                };
                // Every `sigaction` above has now run -- safe to let
                // `arm_signals` return.
                let _ = ready_tx.send(());
                loop {
                    let signum = match sighup.as_mut() {
                        Some(sighup) => {
                            tokio::select! {
                                _ = stop_for_thread.notified() => return,
                                _ = sigint.recv() => SIGINT,
                                _ = sigterm.recv() => SIGTERM,
                                _ = sighup.recv() => SIGHUP,
                            }
                        }
                        None => {
                            tokio::select! {
                                _ = stop_for_thread.notified() => return,
                                _ = sigint.recv() => SIGINT,
                                _ = sigterm.recv() => SIGTERM,
                            }
                        }
                    };
                    let first = token.request(signum);
                    if !first {
                        if signum == SIGHUP {
                            // Never a second signal (issue #431's Signals
                            // section) -- a closed terminal can deliver SIGHUP
                            // twice in quick succession, and the second must
                            // not race whichever signal actually cancelled this
                            // run to `std::process::exit`.
                            continue;
                        }
                        std::process::exit(match signum {
                            SIGINT => Signal::Sigint.exit_code(),
                            SIGTERM => Signal::Sigterm.exit_code(),
                            _ => unreachable!("only SIGINT/SIGTERM reach this branch"),
                        });
                    }
                }
            });
        })
        .expect("could not start the signal-handling thread");
    ready_rx
        .recv()
        .expect("the signal-handling thread's own ready signal");
    SignalGuard {
        stop,
        thread: Some(thread),
    }
}

/// The preview marker refusal for a flag this preview doesn't support
/// at all (`--profile`; profiles are M1-I) -- refused before
/// `run_orchestration` is ever called, so no state exists to write
/// (issue #431's "Refused before the run starts" list).
fn profile_refusal() -> RunResult {
    RunResult {
        ok: false,
        state: None,
        error: Some(format!(
            "electricity {} is a preview and cannot run orchestrations yet: --profile is not \
             supported until a later milestone; use `cof run` instead",
            electricity::VERSION
        )),
        warnings: Vec::new(),
        signal: None,
    }
}

/// Writes *text* to stdout, ignoring any error -- `print!`/`println!`
/// panic on a write error (a closed terminal after SIGHUP, a closed
/// pipe downstream), which would report this run's own exit code as
/// 101 instead of whichever one its own result earned. DESIGN.md §6.5:
/// a stdout/stderr write failure "never stops cleanup, `--out` or the
/// exit code" -- this is the one place that promise is kept, since
/// every other caller in this file goes through here rather than the
/// panicking macros directly.
fn write_stdout(text: &str) {
    let _ = std::io::stdout().lock().write_all(text.as_bytes());
}

/// [`write_stdout`]'s own stderr counterpart.
fn write_stderr(text: &str) {
    let _ = std::io::stderr().lock().write_all(text.as_bytes());
}

/// `cli/logging_setup.py::configure_cli_logging`'s own stderr handler --
/// WARNING and above (`cof run`'s default, never-`--verbose` level:
/// electricity has no `--verbose` flag of its own) formatted as
/// `{levelname}: {message}`, the same `logging.Formatter` the
/// reference's own CLI installs -- written straight through
/// [`write_stderr`], which never buffers across calls, so this is
/// flushed per line the same way Python's own StreamHandler is. Every
/// `log::warn!` this binary's own library dependencies call
/// (electricity-cel's absent-path warning; electricity-vm's
/// dynamic/conditional on_error degradation warnings;
/// electricity-config's "Unknown environment" warning; electricity's
/// own --live-state/--events mid-run write failures) reaches stderr
/// through this one sink (issue #442) -- a no-op until this is
/// installed, same as the reference's own NullHandler default for an
/// embedding caller that never calls configure_cli_logging. Filtered
/// to this workspace's own crates (`enabled`'s own `target`-prefix
/// check): `cli/logging_setup.py`'s own handler sits on Circuitry's
/// own `"circuitry"` logger alone, never the root logger, so a
/// dependency that happens to log doesn't reach `cof run`'s stderr
/// either -- `log`'s `target` defaults to the logging call site's own
/// module path, which for every crate in this workspace starts with
/// `electricity` (hyphens become underscores in a Rust module path),
/// so this is the narrowest prefix that admits all of them and
/// nothing outside this workspace.
struct StderrWarnLogger;

impl log::Log for StderrWarnLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Warn && metadata.target().starts_with("electricity")
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        // `Warn` is the only level any of this binary's own
        // dependencies ever actually emit today (`enabled` above
        // already excludes Info/Debug/Trace) -- Error is handled for
        // forward compatibility with a future log::error! site,
        // matching Python's own logging.WARNING-level threshold
        // admitting both.
        let levelname = match record.level() {
            log::Level::Error => "ERROR",
            log::Level::Warn => "WARNING",
            log::Level::Info => "INFO",
            log::Level::Debug | log::Level::Trace => "DEBUG",
        };
        write_stderr(&format!("{levelname}: {}\n", record.args()));
    }

    fn flush(&self) {}
}

static WARN_LOGGER: StderrWarnLogger = StderrWarnLogger;

/// Installs [`StderrWarnLogger`] as the `log` crate's global logger, at
/// WARNING and above -- called once, at the very start of [`main`],
/// before any config/document loading that could emit one of these
/// warnings runs.
fn install_warning_logger() {
    let _ = log::set_logger(&WARN_LOGGER);
    log::set_max_level(log::LevelFilter::Warn);
}

/// `electricity`'s own non-TTY stdout contract (issue #431's "CLI
/// output" decision, "Same as `cof` when stdout is not a terminal"):
/// success with `--out` prints nothing; success without `--out` prints
/// the state JSON; a failure prints `{"ok": false, ...}` regardless of
/// `--out`. `--pretty` governs only the success-without-`--out` case --
/// `--out`'s own file is `electricity::out::write_out`'s job, not this
/// function's. A config error ([`run_action`]'s own early exit) never
/// reaches this function at all -- Circuitry's own `CircuitryGroup.
/// invoke` catches a `ConfigError` *around* the whole CLI command,
/// before `run()`'s own JSON-output logic is ever reached, so a config
/// error prints nothing on stdout, not even the failure payload.
fn print_stdout_contract(result: &RunResult, out_path: Option<&Path>, pretty: bool) {
    if !result.ok {
        // `state_out` names *out_path* only when a file was actually
        // written there -- a refusal (or any other `state: None`
        // failure) leaves `--out` unwritten even when `--out` was given
        // (PR #441 review finding 8), so this is `None` whenever
        // `result.state` is, regardless of what the caller asked for.
        let state_out = out_path.filter(|_| result.state.is_some());
        write_stdout(&electricity::out::failure_payload(
            result.error.as_deref().unwrap_or(""),
            &result.warnings,
            state_out,
        ));
        return;
    }
    if out_path.is_none() {
        if let Some(state) = &result.state {
            write_stdout(&electricity::out::render_state_for_stdout(state, pretty));
            write_stdout("\n");
        }
    }
}

/// `Warning: ...` lines, and nothing else -- issue #431's "CLI output"
/// decision's own stderr contract (PR #441 review finding 4, K1): a
/// run failure's own error already went out on stdout, inside the
/// `{"ok": false, ...}` payload [`print_stdout_contract`] just wrote --
/// `cli/app.py::run`'s own non-TTY failure path (`json_out`) prints
/// nothing on stderr beyond `_print_run_warnings`'s own `Warning:`
/// lines, never a second `Error:` line too. A bare `Error: <text>` on
/// stderr stays reserved for the two checks that fail *before* `run()`
/// is ever reached at all -- a config error and a missing orchestration
/// (`run_action`'s own early-exit branches, both of which `return`
/// before this function is ever called) -- exactly where Circuitry's
/// own `CircuitryGroup.invoke`/CLI-layer checks report them, on stdout
/// or plain stderr text, never through this JSON-failure path at all.
///
/// Known gap (#442): `result.warnings` only ever holds this run's own
/// `--events`/`--live-state` write-failure warnings -- it never carries
/// the `WARNING: ...` lines Circuitry's own Python `logging` module
/// emits mid-run (`core/dynamic.py`'s/`core/conditional.py`'s own
/// `on_error: skip`/`continue` degradation, a `finally:` that fails
/// after the body already did), since there is no VM observer hook yet
/// for them. State, stdout, and `--events`/`--live-state` are all
/// unaffected -- the conformance suite never compares stderr -- but a
/// human watching the terminal sees fewer `Warning:` lines from
/// electricity than from `cof run` for the same document today.
fn print_stderr_contract(result: &RunResult) {
    for warning in &result.warnings {
        write_stderr(&format!("Warning: {warning}\n"));
    }
}

/// The process exit code for *result* (issue #431's run-wiring step
/// 21): 0 on success; a signal's own conventional code when one ended
/// the run; 1 for every other failure.
fn exit_code_for(result: &RunResult) -> ExitCode {
    if result.ok {
        return ExitCode::SUCCESS;
    }
    match result.signal {
        Some(signal) => ExitCode::from(signal.exit_code() as u8),
        None => ExitCode::from(1),
    }
}

fn run_action(run_args: RunArgs) -> ExitCode {
    // PR #441 review finding 5: armed *before* config load (issue
    // #431's own step 1), on a dedicated OS thread with its own Tokio
    // runtime -- responsive to a signal even while this thread is deep
    // in the synchronous config/document load and every pre-execution
    // check that follows, none of which ever awaits anything of its
    // own. Disarmed only once `--out` has actually been written, below.
    let token = CancellationToken::new();
    let signal_guard = arm_signals(token.clone());

    // Step 1 of issue #431's run-wiring table: a config error exits 1
    // with Circuitry's own text, on stderr alone -- Circuitry's
    // `CircuitryGroup.invoke` catches a `ConfigError` *around* the
    // whole CLI command, before `run()`'s own JSON-output logic is
    // ever reached, so unlike every other failure this prints no
    // stdout payload at all, `--out` or not (PR #441 review finding 8).
    // `config_error` resolves *config_path* only to check for an
    // error -- `run_orchestration` below resolves it again as its own
    // step 1 (that function's own doc comment). A log::warn! a
    // resolve triggers (electricity-config's own "Unknown environment"
    // warning) must fire exactly once per invocation, as it would for
    // a single `cof run`, so logging is silenced for this throwaway
    // first resolve and restored right after (issue #442) -- a config
    // error, below, ends the process before the real resolve would
    // ever run, so no warning is lost by silencing this one.
    let config_path = PathBuf::from(&run_args.config);
    log::set_max_level(log::LevelFilter::Off);
    let config_check = electricity::config_error(&config_path);
    log::set_max_level(log::LevelFilter::Warn);
    if let Some(message) = config_check {
        signal_guard.disarm();
        write_stderr(&format!("Error: {message}\n"));
        return ExitCode::from(1);
    }

    // `cof`'s own "Orchestration not found" check (`_resolve_orchestration`)
    // happens in the CLI layer *before* `-e` is ever parsed
    // (`cli/app.py` resolves the document at ~:1150, `_parse_env_vars`
    // only at ~:1270) -- checked here, before step 2 below, for the
    // same reason (PR #441 review finding 5: `-e badtext` plus a
    // missing document must still exit 1, not 2). Unlike the config-
    // error check above, `cof` prints this one on *stdout*
    // (`console.print`, not `err_console.print` -- `app.py:1156`), so
    // this does too; electricity's own positional orchestration
    // argument is always a literal path (no library-name resolution,
    // unlike `cof run <name>`), so this is a plain existence check. The
    // accompanying `Tip: run cof list ...` line cof also prints is left
    // out on purpose: electricity has no `list` subcommand of its own
    // for it to name.
    let orchestration_path = PathBuf::from(&run_args.orchestration);
    if !orchestration_path.is_file() {
        signal_guard.disarm();
        write_stdout(&format!(
            "Error: Orchestration not found: {}\n",
            run_args.orchestration
        ));
        return ExitCode::from(1);
    }

    // Step 2: `electricity::parse_inputs`'s own malformed-`-e` text,
    // Circuitry's own `BadParameter` message word for word (issue
    // #429): `parse_flags` only checks that `-e` has *some* value,
    // saying nothing about whether that value itself contains `=` -- a
    // value with no `=` (`-e badtext`) reaches this, `cli/app.py::
    // _parse_env_vars`'s own check proper.
    let inputs = match electricity::parse_inputs(&run_args.inputs) {
        Ok(inputs) => inputs,
        Err(message) => {
            signal_guard.disarm();
            write_stderr(&format!("electricity: {message}\n{USAGE}\n"));
            return ExitCode::from(2);
        }
    };

    let out_path = run_args.out.map(PathBuf::from);

    // `--profile` refuses with the preview marker before anything else
    // runs (issue #431's CLI section) -- profiles are M1-I.
    let result = if run_args.profile.is_some() {
        profile_refusal()
    } else {
        let req = RunRequest {
            config_path,
            orchestration_path,
            inputs,
            out_path: out_path.clone(),
            pretty: run_args.pretty,
            live_state_path: run_args.live_state.map(PathBuf::from),
            events_path: run_args.events.map(PathBuf::from),
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("could not start the electricity async runtime");
        runtime.block_on(electricity::run_orchestration(&req, &token))
    };

    if let Some(path) = &out_path {
        if let Some(state) = &result.state {
            if let Err(err) = electricity::out::write_out(path, state, run_args.pretty) {
                signal_guard.disarm();
                write_stderr(&format!(
                    "electricity: could not write --out {}: {err}\n",
                    path.display()
                ));
                return ExitCode::from(1);
            }
        }
    }

    // "Signals armed for the run only" (issue #431's Signals section):
    // stopped only now, after `--out` has actually been written, so a
    // SIGHUP during that write is still ignored rather than hitting
    // the OS's own default disposition (`arm_signals`'s own doc
    // comment).
    signal_guard.disarm();

    print_stdout_contract(&result, out_path.as_deref(), run_args.pretty);
    print_stderr_contract(&result);
    exit_code_for(&result)
}

fn main() -> ExitCode {
    install_warning_logger();
    // `args_os` + lossy conversion instead of `args()`, which panics on a
    // non-UTF-8 argument.
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    match classify(&args) {
        Action::Version => {
            write_stdout(&format!("{}\n", electricity::version_string()));
            ExitCode::SUCCESS
        }
        Action::Help => {
            write_stdout(&format!("{USAGE}\n"));
            ExitCode::SUCCESS
        }
        Action::Run(run_args) => run_action(run_args),
        Action::DumpIr(config_path, orchestration_path, raw_inputs) => {
            let inputs = match electricity::parse_inputs(&raw_inputs) {
                Ok(inputs) => inputs,
                Err(message) => {
                    write_stderr(&format!("electricity: {message}\n{USAGE}\n"));
                    return ExitCode::from(2);
                }
            };
            match electricity::dump_ir(
                Path::new(&config_path),
                Path::new(&orchestration_path),
                &inputs,
            ) {
                Ok(json) => {
                    write_stdout(&format!("{json}\n"));
                    ExitCode::SUCCESS
                }
                Err(message) => {
                    write_stderr(&format!("{message}\n"));
                    ExitCode::from(1)
                }
            }
        }
        Action::UsageError(message) => {
            write_stderr(&format!("electricity: {message}\n{USAGE}\n"));
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;
    use log::Log;

    fn warn_metadata(target: &str) -> log::Metadata<'_> {
        log::Metadata::builder()
            .level(log::Level::Warn)
            .target(target)
            .build()
    }

    #[test]
    fn the_warning_logger_admits_this_workspaces_own_crate_targets() {
        for target in [
            "electricity_vm::exec::dynamic",
            "electricity_config::config",
            "electricity",
        ] {
            assert!(
                StderrWarnLogger.enabled(&warn_metadata(target)),
                "{target} should be admitted"
            );
        }
    }

    #[test]
    fn the_warning_logger_ignores_a_record_from_outside_this_workspace() {
        assert!(!StderrWarnLogger.enabled(&warn_metadata("some_other_crate")));
    }

    #[test]
    fn version_flag_wins_over_everything() {
        assert!(matches!(
            classify(&["--version".to_string(), "bogus".to_string()]),
            Action::Version
        ));
    }

    #[test]
    fn version_flag_wins_even_over_an_unknown_flag() {
        assert!(matches!(
            classify(&["--bogus".to_string(), "--version".to_string()]),
            Action::Version
        ));
    }

    #[test]
    fn help_flag_is_recognized() {
        assert!(matches!(classify(&["--help".to_string()]), Action::Help));
        assert!(matches!(classify(&["-h".to_string()]), Action::Help));
    }

    #[test]
    fn no_args_is_a_usage_error() {
        assert!(matches!(classify(&[]), Action::UsageError(_)));
    }

    #[test]
    fn unknown_flag_is_a_usage_error() {
        assert!(matches!(
            classify(&["--bogus".to_string()]),
            Action::UsageError(_)
        ));
    }

    #[test]
    fn unknown_flag_after_the_positionals_is_also_a_usage_error() {
        // Issue #431's gate lane tightens this from the pre-#431 preview
        // CLI, which let a trailing unknown flag fall through to the
        // (always-refusing) run path instead.
        assert!(matches!(
            classify(&[
                "config.json".to_string(),
                "orchestration.yml".to_string(),
                "--bogus".to_string(),
            ]),
            Action::UsageError(_)
        ));
    }

    #[test]
    fn positional_args_are_a_run_request() {
        assert!(matches!(
            classify(&["config.json".to_string(), "orchestration.yml".to_string()]),
            Action::Run(_)
        ));
    }

    #[test]
    fn known_run_flag_in_first_position_is_a_run_request() {
        assert!(matches!(
            classify(&[
                "-e".to_string(),
                "k=v".to_string(),
                "config.json".to_string(),
                "orchestration.yml".to_string()
            ]),
            Action::Run(_)
        ));
        assert!(matches!(
            classify(&[
                "--profile".to_string(),
                "p".to_string(),
                "config.json".to_string(),
                "orchestration.yml".to_string()
            ]),
            Action::Run(_)
        ));
    }

    #[test]
    fn every_new_run_flag_parses_in_any_position() {
        match classify(&[
            "--out".to_string(),
            "state.json".to_string(),
            "config.json".to_string(),
            "orchestration.yml".to_string(),
            "--pretty".to_string(),
            "--live-state".to_string(),
            "live.json".to_string(),
            "--events".to_string(),
            "events.jsonl".to_string(),
        ]) {
            Action::Run(run_args) => {
                assert_eq!(run_args.config, "config.json");
                assert_eq!(run_args.orchestration, "orchestration.yml");
                assert_eq!(run_args.out.as_deref(), Some("state.json"));
                assert!(run_args.pretty);
                assert_eq!(run_args.live_state.as_deref(), Some("live.json"));
                assert_eq!(run_args.events.as_deref(), Some("events.jsonl"));
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn a_value_flag_with_no_following_argument_is_a_usage_error() {
        for flag in ["--out", "--live-state", "--events", "--profile"] {
            assert!(
                matches!(
                    classify(&[
                        "config.json".to_string(),
                        "orchestration.yml".to_string(),
                        flag.to_string(),
                    ]),
                    Action::UsageError(_)
                ),
                "{flag} with no value should be a usage error"
            );
        }
    }

    #[test]
    fn dump_ir_flag_selects_the_first_two_positionals_as_config_and_orchestration() {
        match classify(&[
            "config.json".to_string(),
            "orchestration.yml".to_string(),
            "--dump-ir".to_string(),
        ]) {
            Action::DumpIr(config, orchestration, _) => {
                assert_eq!(config, "config.json");
                assert_eq!(orchestration, "orchestration.yml");
            }
            _ => panic!("expected DumpIr"),
        }
    }

    #[test]
    fn dump_ir_flag_recognized_in_first_position_too() {
        assert!(matches!(
            classify(&[
                "--dump-ir".to_string(),
                "config.json".to_string(),
                "orchestration.yml".to_string(),
            ]),
            Action::DumpIr(_, _, _)
        ));
    }

    #[test]
    fn dump_ir_with_no_positionals_is_a_usage_error() {
        assert!(matches!(
            classify(&["--dump-ir".to_string()]),
            Action::UsageError(_)
        ));
    }

    #[test]
    fn dump_ir_picks_the_first_two_positionals_not_the_last() {
        match classify(&[
            "config.json".to_string(),
            "orchestration.yml".to_string(),
            "extra.yml".to_string(),
            "--dump-ir".to_string(),
        ]) {
            Action::DumpIr(config, orchestration, _) => {
                assert_eq!(config, "config.json");
                assert_eq!(orchestration, "orchestration.yml");
            }
            _ => panic!("expected DumpIr"),
        }
    }

    #[test]
    fn bare_dash_not_in_first_position_is_still_a_run_request() {
        assert!(matches!(
            classify(&[
                "config.json".to_string(),
                "orchestration.yml".to_string(),
                "-".to_string(),
            ]),
            Action::Run(_)
        ));
    }

    #[test]
    fn e_values_are_collected_in_order_for_a_run_request() {
        match classify(&[
            "config.json".to_string(),
            "orchestration.yml".to_string(),
            "-e".to_string(),
            "name=World".to_string(),
        ]) {
            Action::Run(run_args) => assert_eq!(run_args.inputs, vec!["name=World".to_string()]),
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn e_values_are_collected_for_a_dump_ir_request_too() {
        match classify(&[
            "config.json".to_string(),
            "orchestration.yml".to_string(),
            "--dump-ir".to_string(),
            "-e".to_string(),
            "name=World".to_string(),
        ]) {
            Action::DumpIr(_, _, inputs) => assert_eq!(inputs, vec!["name=World".to_string()]),
            _ => panic!("expected DumpIr"),
        }
    }

    #[test]
    fn trailing_e_with_no_value_is_a_usage_error() {
        assert!(matches!(
            classify(&[
                "config.json".to_string(),
                "orchestration.yml".to_string(),
                "-e".to_string(),
            ]),
            Action::UsageError(_)
        ));
    }

    #[test]
    fn osps_own_invocation_shape_still_parses() {
        // oscilloscope's `ElectricityEngine` (`oscilloscope/crates/
        // oscilloscope-core/src/engine.rs`) passes `<config> <doc> -e
        // k=v... --out <dir>/state.json` -- issue #431's CLI section:
        // "That must keep working."
        match classify(&[
            "config.json".to_string(),
            "doc.yml".to_string(),
            "-e".to_string(),
            "name=World".to_string(),
            "--out".to_string(),
            "run-dir/state.json".to_string(),
        ]) {
            Action::Run(run_args) => {
                assert_eq!(run_args.out.as_deref(), Some("run-dir/state.json"));
                assert_eq!(run_args.inputs, vec!["name=World".to_string()]);
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn every_long_value_flag_accepts_the_equals_form() {
        match classify(&[
            "config.json".to_string(),
            "orchestration.yml".to_string(),
            "--out=state.json".to_string(),
            "--live-state=live.json".to_string(),
            "--events=events.jsonl".to_string(),
            "--profile=p.json".to_string(),
        ]) {
            Action::Run(run_args) => {
                assert_eq!(run_args.out.as_deref(), Some("state.json"));
                assert_eq!(run_args.live_state.as_deref(), Some("live.json"));
                assert_eq!(run_args.events.as_deref(), Some("events.jsonl"));
                assert_eq!(run_args.profile.as_deref(), Some("p.json"));
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn the_equals_form_works_before_the_positionals_too() {
        match classify(&[
            "--out=state.json".to_string(),
            "config.json".to_string(),
            "orchestration.yml".to_string(),
        ]) {
            Action::Run(run_args) => assert_eq!(run_args.out.as_deref(), Some("state.json")),
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn a_value_containing_an_equals_sign_is_kept_whole() {
        match classify(&[
            "config.json".to_string(),
            "orchestration.yml".to_string(),
            "--out=a=b.json".to_string(),
        ]) {
            Action::Run(run_args) => assert_eq!(run_args.out.as_deref(), Some("a=b.json")),
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn a_boolean_flag_with_an_equals_value_is_a_usage_error() {
        assert!(matches!(
            classify(&[
                "config.json".to_string(),
                "orchestration.yml".to_string(),
                "--pretty=true".to_string(),
            ]),
            Action::UsageError(_)
        ));
        assert!(matches!(
            classify(&[
                "config.json".to_string(),
                "orchestration.yml".to_string(),
                "--dump-ir=1".to_string(),
            ]),
            Action::UsageError(_)
        ));
    }

    #[test]
    fn an_unknown_flag_with_an_equals_value_is_still_a_usage_error() {
        assert!(matches!(
            classify(&[
                "config.json".to_string(),
                "orchestration.yml".to_string(),
                "--bogus=1".to_string(),
            ]),
            Action::UsageError(_)
        ));
    }

    #[test]
    fn osps_own_invocation_shape_still_parses_with_the_equals_form() {
        match classify(&[
            "config.json".to_string(),
            "doc.yml".to_string(),
            "-e".to_string(),
            "name=World".to_string(),
            "--out=run-dir/state.json".to_string(),
        ]) {
            Action::Run(run_args) => {
                assert_eq!(run_args.out.as_deref(), Some("run-dir/state.json"));
            }
            _ => panic!("expected Run"),
        }
    }
}
