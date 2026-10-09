//! `osp` — the CLI binary (DESIGN.md §4, §6.2, §6.3). `--log`, or
//! any non-TTY stdout, keeps O-1's plain-text stream unchanged; a TTY
//! gets O-2's ratatui/crossterm TUI (issue #434) instead, in
//! `run_tui`/`run_tui_watch`, `tui.rs` and `keys.rs`.
//!
//! The two forms share no top-level `clap` command: `osp watch ...` is
//! dispatched by a plain string check on the first argument (`watch`
//! is therefore reserved — an orchestration literally named `watch`
//! needs a `./watch` or `watch.yml` path instead), and everything else
//! parses directly as [`RunArgs`]. A `clap` derive otherwise mixes
//! flags and positionals in any order on its own; the #422 review's
//! "flags work before the positionals" only broke under the O-0
//! stub's `#[command(external_subcommand)]` dispatch, which matched
//! the *first* token against the top level's own (empty) flag set
//! before ever reaching `RunArgs` — this dispatch avoids that entirely.

use std::io::{IsTerminal, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod keys;
mod terminal;
mod tui;

use clap::{Parser, ValueEnum};
use crossterm::event::{self, Event as CEvent, KeyEventKind};
use oscilloscope_core::diff::Differ;
use oscilloscope_core::engine::{CofEngine, ElectricityEngine, Engine, EngineError, RunSpec};
use oscilloscope_core::model::{ProcessState, RunModel};
use oscilloscope_core::observe::{
    EventsTailer, LiveStatePoller, POLL_INTERVAL, duration_seconds, parse_event,
};
use oscilloscope_core::plan::PlanTree;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use keys::App;
use oscilloscope_core::supervise::{
    ForwardSignal, SignalWatcher, SupervisedChild, exit_code, exit_code_for_signal_name,
};
use terminal::TerminalGuard;

const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (preview)");

const ABOUT: &str = "\
osp (oscilloscope) launches a Circuitry orchestration on cof or electricity \
and shows it running.

  osp <orchestration> [config.json] [-e key=value]... [--engine cof|electricity] [--out-dir DIR] [--log]
  osp watch <dir | state.live.json> [--plan doc.yml]";

#[derive(Parser, Debug)]
#[command(name = "osp", version = VERSION, about = ABOUT)]
struct RunArgs {
    orchestration: String,
    config: Option<String>,
    #[arg(short = 'e', value_name = "key=value")]
    set: Vec<String>,
    #[arg(long, value_enum, default_value = "cof")]
    engine: EngineChoice,
    #[arg(long, value_name = "DIR")]
    out_dir: Option<PathBuf>,
    #[arg(long)]
    log: bool,
}

#[derive(Parser, Debug)]
#[command(name = "osp watch")]
struct WatchArgs {
    /// A run directory, or a `state.live.json` file directly.
    target: String,
    #[arg(long, value_name = "doc.yml")]
    plan: Option<PathBuf>,
    /// The config `--plan`'s own document should be checked against
    /// (K4) — the same `config.json` the watched run itself used, if
    /// any. Needed for a document whose plan depends on a config-
    /// defined `concurrency_groups`/`runtime:` block; without it,
    /// `--plan` still compiles, just without that merge.
    #[arg(long, value_name = "config.json")]
    config: Option<PathBuf>,
    /// `-e key=value` inputs `--plan`'s own document needs to compile
    /// (K4) — the same ones the watched run itself was given, if any.
    #[arg(short = 'e', value_name = "key=value")]
    set: Vec<String>,
    /// Forces the plain-text stream (DESIGN.md §6.2) even on a TTY —
    /// the TUI's own default otherwise, the same split `osp <doc>`
    /// makes.
    #[arg(long)]
    log: bool,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum EngineChoice {
    Cof,
    Electricity,
}

/// `--log` is chosen automatically when stdout is not a TTY or `CI` is
/// set (DESIGN.md §6.2). `true` keeps the plain-text stream; `false`
/// means `do_run`/`do_watch` hand off to the TUI instead (§6.3).
fn effective_log_mode(explicit: bool) -> bool {
    explicit || !std::io::stdout().is_terminal() || std::env::var_os("CI").is_some()
}

/// `mm:ss.s` elapsed time (DESIGN.md §6.2), anchored to the first
/// event timestamp osp observes so a log line's elapsed time tracks
/// the engine's own clock rather than polling latency; falls back to
/// osp's own wall clock before the first timestamped observation (and
/// for lines with none, like a forwarded-signal notice).
struct Clock {
    start: Instant,
    anchor_ts: Option<String>,
    /// The highest elapsed time printed so far (K5): a line's own `ts`
    /// can legitimately be *earlier* than one already printed (a
    /// coalesced state write backdating a node's `▶` line to its real
    /// `created_at`, DESIGN.md §2.4's "new node that is already
    /// complete" row, or events and state simply landing a tick apart)
    /// — printing that line's own true elapsed time anyway would make
    /// the stream's own `mm:ss.s` prefixes visibly run backwards.
    /// Clamping the *displayed* time to this high-water mark leaves
    /// sorting (by the real `ts`) and duration math untouched; only the
    /// elapsed-time prefix never regresses.
    max_elapsed: f64,
}

impl Clock {
    fn new() -> Self {
        Clock {
            start: Instant::now(),
            anchor_ts: None,
            max_elapsed: 0.0,
        }
    }

    fn elapsed_for(&mut self, ts: Option<&str>) -> f64 {
        let computed = match (&self.anchor_ts, ts) {
            (None, Some(t)) => {
                self.anchor_ts = Some(t.to_string());
                0.0
            }
            (Some(anchor), Some(t)) => {
                duration_seconds(anchor, t).unwrap_or_else(|| self.start.elapsed().as_secs_f64())
            }
            _ => self.start.elapsed().as_secs_f64(),
        };
        self.max_elapsed = self.max_elapsed.max(computed);
        self.max_elapsed
    }

    fn format(secs: f64) -> String {
        let tenths = (secs.max(0.0) * 10.0).round() as i64;
        let whole = tenths / 10;
        let tenth = tenths % 10;
        format!("{:02}:{:02}.{tenth}", whole / 60, whole % 60)
    }
}

fn print_line(out: &mut impl Write, clock: &mut Clock, ts: Option<&str>, text: &str) {
    let elapsed = clock.elapsed_for(ts);
    let _ = writeln!(out, "{} {text}", Clock::format(elapsed));
}

/// Prints the `exit <code>` line every path through `do_run` ends
/// with (P2-1) — including a usage-style failure before the engine
/// was ever spawned, which used to print nothing on stdout at all.
fn exit_early(code: u8) -> ExitCode {
    println!("exit {code}");
    ExitCode::from(code)
}

/// The exit code osp should use right now, without ever spawning the
/// engine, if a signal already arrived (P2-10) — the same 128+signum
/// codes `cof` itself uses. `None` means nothing is pending yet.
fn pending_signal_exit_code(signals: &mut Option<SignalWatcher>) -> Option<u8> {
    let watcher = signals.as_mut()?;
    let sig = watcher.pending().into_iter().next()?;
    Some(match sig {
        ForwardSignal::Int => 130,
        ForwardSignal::Term => 143,
        ForwardSignal::Hup => 129,
    })
}

fn unique_run_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("osp-{}-{nanos}", std::process::id()))
}

/// Creates the run directory private (`0700`): it can hold `state.live
/// .json`/`state.json`, which carry prompt and tool output (F2).
///
/// The default directory's name is a predictable `osp-<pid>-<nanos>`
/// under a shared `/tmp`, so it is created non-recursively and must
/// not already exist — a pre-existing entry there (a collision, or
/// something planted ahead of time) is refused rather than reused.
/// `--out-dir` may be a path the caller wants created in full (missing
/// parents and all) and may legitimately already exist from an
/// earlier run (F3 clears its stale observation files, not the
/// directory itself), so it uses `create_dir_all`'s own semantics
/// instead — but only ever chmods the leaf directory when *this call*
/// is the one that created it (K7): forcing `0700` onto a directory
/// the caller already had, for whatever reason of their own, is a
/// real, unrequested change to something outside osp's own run
/// directory, and silently chmod'ing *someone else's* directory (one
/// this process doesn't own) can even fail outright with `EPERM`. A
/// pre-existing directory that already was group- or world-accessible
/// instead gets a warning on stderr — loud enough that a caller who
/// reuses one on purpose notices, without osp quietly changing it out
/// from under them.
fn create_run_dir(path: &Path, is_default: bool) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    if is_default {
        return std::fs::DirBuilder::new().mode(0o700).create(path);
    }
    let already_existed = path.exists();
    std::fs::create_dir_all(path)?;
    if already_existed {
        let mode = std::fs::metadata(path)?.permissions().mode();
        if mode & 0o077 != 0 {
            eprintln!(
                "osp: run directory {} is group- or world-accessible (mode {:o}); \
                 it can hold prompt and tool output",
                path.display(),
                mode & 0o777
            );
        }
        Ok(())
    } else {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
    }
}

fn build_engine(choice: EngineChoice) -> Box<dyn Engine + Send + Sync> {
    match choice {
        EngineChoice::Cof => Box::new(CofEngine::detect("cof")),
        EngineChoice::Electricity => Box::new(ElectricityEngine::new("electricity")),
    }
}

fn do_run(args: RunArgs) -> ExitCode {
    // Registered before *anything* else in this function, not just
    // before spawning the engine (F13): `build_engine`'s own `cof run
    // --help` detection, and compiling the plan, both run a blocking
    // subprocess/compile step before the engine is ever spawned, and
    // each can legitimately take longer than a human's first Ctrl-C
    // takes to arrive (a cold Python interpreter start under load is
    // not rare on a shared, busy machine). A signal landing in either
    // window, with no handler installed yet, used to hit the OS
    // default disposition and kill osp outright — with the engine
    // either not yet started, or started and now unsupervised.
    let mut signals = SignalWatcher::new().ok();

    let orchestration = PathBuf::from(&args.orchestration);
    let config = args.config.as_ref().map(PathBuf::from);

    if args.engine == EngineChoice::Electricity {
        if let Some(cfg) = &config {
            if oscilloscope_core::engine::looks_swapped(cfg, &orchestration) {
                eprintln!(
                    "osp: '{}' and '{}' look swapped — usage is `osp <orchestration> [config.json]`",
                    cfg.display(),
                    orchestration.display()
                );
            }
        }
    }

    let is_default_dir = args.out_dir.is_none();
    let run_dir = args.out_dir.clone().unwrap_or_else(unique_run_dir);
    let spec = RunSpec {
        orchestration: orchestration.clone(),
        config,
        sets: args.set.clone(),
        run_dir: run_dir.clone(),
    };

    let engine = build_engine(args.engine);
    // M4: a missing engine binary gets its own, more specific error
    // once the later spawn fails -- this notice is only useful (and
    // only true) when `cof` was actually found and simply predates
    // `--events`.
    if engine.name() == "cof" && !engine.caps().events && !engine.caps().missing {
        eprintln!("osp: this cof has no --events: running from state only");
    }

    let cmd = match engine.command(&spec) {
        Ok(cmd) => cmd,
        Err(EngineError::MissingConfig(message)) => {
            eprintln!("osp: {message}");
            return exit_early(2);
        }
    };

    // K4: the exact options a real `electricity <config> <doc> -e
    // k=v...` run would check this document against (DESIGN.md §5
    // via `electricity::check_options`), so the plan can't drift from
    // what the engine itself would check. osp does not reproduce
    // `cof`'s own config discovery (the global/project
    // `circuitry.config.json` layers a bare `cof run` resolves with
    // no `--config`), so a `group:` defined only in one of those
    // layers still falls back to no plan here, on either engine. A
    // malformed `-e` is never osp's own error (the engine reports
    // that itself once it actually runs) — just the same no-plan
    // fallback as a compile failure.
    let plan = match oscilloscope_core::plan::parse_inputs(&args.set) {
        Ok(inputs) => {
            match oscilloscope_core::plan::compile(&orchestration, spec.config.as_deref(), &inputs)
            {
                Ok(program) => {
                    PlanTree::from_program_with_options(&program, spec.config.as_deref(), &inputs)
                }
                Err(err) => {
                    eprintln!(
                        "osp: couldn't compile a plan ({err}); running from observations only"
                    );
                    PlanTree::empty()
                }
            }
        }
        Err(err) => {
            eprintln!("osp: couldn't compile a plan ({err}); running from observations only");
            PlanTree::empty()
        }
    };

    // The run directory is created only right here, immediately before
    // the spawn it exists for (P2-1): every check above (the config,
    // the plan compile) can fail without ever needing it on disk at
    // all, and a failure there used to leave a freshly made, empty
    // directory behind with no "exit" line printed either.
    if let Err(err) = create_run_dir(&run_dir, is_default_dir) {
        eprintln!(
            "osp: couldn't create run directory {}: {err}",
            run_dir.display()
        );
        return exit_early(1);
    }

    // A reused `--out-dir` can hold `state.live.json`/`state.json`/
    // `events.jsonl` from an earlier run: left alone, the first poll
    // right after spawn would read *that* run's old final state —
    // printing its whole log (including its own `■ run` line) before
    // this run has written anything, and suppressing this run's own
    // summary line since the differ would already think it had seen
    // one (F3). osp made this directory (or it's the default, always
    // fresh), so clearing stale observation files here can't lose
    // anything the caller put there on purpose.
    for stale in [spec.live_state_path(), spec.out_path(), spec.events_path()] {
        let _ = std::fs::remove_file(&stale);
    }

    // P2-10: a signal that arrived any time after `SignalWatcher::new`
    // above -- during `build_engine`'s own `cof run --help` detection,
    // or the plan compile, both blocking steps that run before any of
    // this -- must not go on to spawn the engine at all: forwarding it
    // to a child started *after* the signal already arrived would
    // still leave a brief, real window where the engine ran
    // unsupervised before the first poll loop iteration ever checked.
    if let Some(code) = pending_signal_exit_code(&mut signals) {
        if is_default_dir {
            let _ = std::fs::remove_dir_all(&run_dir);
        }
        return exit_early(code);
    }

    let (mut child, stderr_rx) =
        match SupervisedChild::spawn(cmd, &spec.stdout_path(), &spec.stderr_path()) {
            Ok(pair) => pair,
            Err(err) => {
                eprintln!("osp: couldn't launch {}: {err}", engine.name());
                // The directory was just created for this spawn alone
                // (P2-1): the engine never started, so a default one
                // shouldn't be left behind empty. A caller's own
                // `--out-dir` is never removed, same as every other
                // path through this function.
                if is_default_dir {
                    let _ = std::fs::remove_dir_all(&run_dir);
                }
                return exit_early(1);
            }
        };

    // DESIGN.md §6.2/§6.3: a TTY gets the interactive TUI; `--log` or
    // any non-TTY stdout keeps this function's own plain-text stream
    // below, byte for byte.
    if !effective_log_mode(args.log) {
        return run_tui(
            child,
            stderr_rx,
            spec,
            plan,
            run_dir,
            is_default_dir,
            args.orchestration.clone(),
            engine.name().to_string(),
            signals,
        );
    }

    let mut clock = Clock::new();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    let mut live_poller = LiveStatePoller::new(spec.live_state_path());
    let mut events_tailer = EventsTailer::new(spec.events_path());
    let mut differ = Differ::new();
    let mut model = RunModel::new();

    let mut signal_count: u32 = 0;
    let mut kill_deadline: Option<Instant> = None;

    let exit_status = loop {
        if let Some(watcher) = signals.as_mut() {
            for sig in watcher.pending() {
                signal_count += 1;
                child.forward(sig);
                if signal_count == 1 {
                    print_line(&mut out, &mut clock, None, "cancelling, finally running...");
                } else if signal_count == 2 {
                    kill_deadline = Some(Instant::now() + std::time::Duration::from_secs(10));
                }
            }
        }
        if let Some(deadline) = kill_deadline {
            if Instant::now() >= deadline && matches!(child.try_wait(), Ok(None)) {
                print_line(
                    &mut out,
                    &mut clock,
                    None,
                    "engine still running 10s after the second signal; sending SIGKILL",
                );
                child.kill_group();
                kill_deadline = None;
            }
        }

        for line in stderr_rx.try_iter() {
            print_line(&mut out, &mut clock, None, &format!("engine: {line}"));
        }
        drain_observations(
            &mut out,
            &mut clock,
            &mut live_poller,
            &mut events_tailer,
            &mut differ,
            &mut model,
            &plan,
        );

        if let Ok(Some(status)) = child.try_wait() {
            break status;
        }
        std::thread::sleep(POLL_INTERVAL);
    };

    // Join the tee threads before draining anything further (F6): the
    // engine already exited (`try_wait` above returned `Some`), so this
    // reaps a already-reaped child again -- safe, and how `Drop`'s own
    // kill-then-wait does it too -- but it also blocks until every
    // stdout/stderr byte buffered in the pipe has actually been read
    // and queued, which plain `try_iter()` right after `try_wait()`
    // does not: a line the tee thread hadn't gotten to yet was silently
    // dropped, and `stdout.txt`/`stderr.txt` could be cut off
    // mid-write.
    let code = finish_run(
        &mut out,
        &mut clock,
        &mut child,
        &stderr_rx,
        &mut live_poller,
        &mut events_tailer,
        &mut differ,
        &mut model,
        &plan,
        &spec,
        &run_dir,
        exit_status,
        args.out_dir.is_none(),
    );
    ExitCode::from(code)
}

/// The common ending every run reaches, however it got there (the
/// engine simply exiting, in `do_run`'s own plain loop; the TUI's own
/// loop breaking the same way, once its terminal session is already
/// torn down and a plain writer is all that's left): reaps the
/// engine, drains whatever's left of its stderr and the final
/// live-state write, computes the run's own summary line exactly
/// once (M1: never mid-run), and prints the exit code and (for the
/// default run directory only) where its files ended up. Returns the
/// engine's own exit code, the same number every path through here
/// has always used to build its `ExitCode` from.
#[allow(clippy::too_many_arguments)]
fn finish_run(
    out: &mut std::io::StdoutLock<'_>,
    clock: &mut Clock,
    child: &mut SupervisedChild,
    stderr_rx: &std::sync::mpsc::Receiver<String>,
    live_poller: &mut LiveStatePoller,
    events_tailer: &mut EventsTailer,
    differ: &mut Differ,
    model: &mut RunModel,
    plan: &PlanTree,
    spec: &RunSpec,
    run_dir: &Path,
    exit_status: std::process::ExitStatus,
    print_run_dir: bool,
) -> u8 {
    let exit_status = child.wait().unwrap_or(exit_status);

    // Drain what's left after the engine exited: its last stderr lines,
    // and the final live-state write (DESIGN.md §1.1: `run_end`, when
    // present, lands after it).
    for line in stderr_rx.try_iter() {
        print_line(out, clock, None, &format!("engine: {line}"));
    }
    drain_observations(out, clock, live_poller, events_tailer, differ, model, plan);

    // M1: the run's own summary line is computed exactly once, here,
    // after every other line this run will ever produce has already
    // been printed -- `Differ::finish` never emits it mid-run, and a
    // per-tick heuristic no longer exists to race state and events
    // (two independently polled files) against each other. Try the
    // authoritative `--out` file once more first -- the live-state
    // poller's own last `drain` above can miss the very last write if
    // it lands between two polls -- which also covers the two cases
    // `finish` itself can't: the engine failed before writing any
    // state at all (a validation error, printed as cof's own
    // pre-execution JSON on stdout, DESIGN.md §4.1), or the run was
    // genuinely aborted (a second signal, SIGKILL, a crash) with no
    // final write.
    let final_state = std::fs::read(spec.out_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    if let Some(state) = &final_state {
        for line in differ.diff(state, plan) {
            print_line(out, clock, line.ts.as_deref(), &line.text);
        }
    }
    let summary = final_state
        .as_ref()
        .and_then(|state| differ.finish(state, failure_reason_fallback(run_dir, model).as_deref()))
        .or_else(|| {
            if differ.run_line_emitted() {
                return None;
            }
            let text = match failure_reason_fallback(run_dir, model) {
                Some(err) => format!(
                    "■ run failed: {}",
                    oscilloscope_core::diff::format_reason_for_summary(&err)
                ),
                None => "■ run aborted (no final state)".to_string(),
            };
            Some(oscilloscope_core::diff::LogLine { ts: None, text })
        });
    if let Some(line) = summary {
        print_line(out, clock, line.ts.as_deref(), &line.text);
    }
    let _ = writeln!(out, "exit {}", exit_code(exit_status));

    if print_run_dir {
        let _ = writeln!(out, "run directory: {}", run_dir.display());
    }

    exit_code(exit_status) as u8
}

/// How often the TUI redraws at most (DESIGN.md §6.3: "Redraws are
/// capped (for example 10 per second) and happen only when something
/// changed").
const REDRAW_INTERVAL: Duration = Duration::from_millis(100);
/// How long one loop tick waits for a key before giving the rest of
/// the loop (signals, observation polling, the redraw check) another
/// turn — short enough that navigation feels immediate, long enough
/// not to spin a whole CPU core on a shared machine.
const KEY_POLL_INTERVAL: Duration = Duration::from_millis(30);

/// Sends one cancelling signal to the engine's process group and
/// applies the same escalation `do_run`'s plain loop applies to a real
/// forwarded signal (DESIGN.md §4.1) — shared by both, since `c`/a
/// confirmed `q` (§6.3: "cancel, with confirm (= Ctrl-C)") must behave
/// exactly like Ctrl-C itself, never a separate path that could drift
/// from it. `sig` is `Int` for a key (Ctrl-C always means SIGINT) but
/// the real signal osp itself received for an external one (K2): the
/// plain loop above already forwards *that* signal unchanged, and the
/// TUI must too, rather than silently turning every SIGTERM/SIGHUP
/// into a SIGINT and leaving osp exiting 130 for either.
fn cancel_once(
    child: &SupervisedChild,
    sig: ForwardSignal,
    signal_count: &mut u32,
    kill_deadline: &mut Option<Instant>,
    log_lines: &mut Vec<String>,
    clock: &mut Clock,
) {
    *signal_count += 1;
    child.forward(sig);
    if *signal_count == 1 {
        log_lines.push(format_log_line(
            clock,
            None,
            "cancelling, finally running...",
        ));
    } else if *signal_count == 2 {
        *kill_deadline = Some(Instant::now() + Duration::from_secs(10));
    }
}

/// `print_line`'s own formatting, into the TUI's own log buffer
/// instead of straight to stdout (DESIGN.md §6.3's log pane: "the
/// same lines as `--log`").
fn format_log_line(clock: &mut Clock, ts: Option<&str>, text: &str) -> String {
    let elapsed = clock.elapsed_for(ts);
    format!("{} {text}", Clock::format(elapsed))
}

/// How often the TUI's own render state (the plan tree's rows, the
/// header's effect counts, everything `oscilloscope_core::render::
/// build` computes) is allowed to be rebuilt from scratch (review
/// finding K4) — `REDRAW_INTERVAL` caps how often a frame is drawn,
/// but every ~30ms key-poll tick used to rebuild this *and* recompute
/// `keys::visible_rows` regardless of whether either observation,
/// key or resize had actually changed anything since the last one,
/// which alone cost +0.52s of CPU per wall second on a 400-item tree
/// loop while nothing was happening at all. The header's own elapsed
/// time is the one field that's always moving regardless, so a
/// rebuild is still forced at least this often even with nothing
/// else dirty, same as `--log`'s own `mm:ss.s` prefix.
const RENDER_REBUILD_INTERVAL: Duration = Duration::from_secs(1);

/// `osp <doc>`'s own TUI loop (DESIGN.md §6.3): supervises the engine
/// exactly like `do_run`'s plain loop (the same signal forwarding,
/// kill-deadline escalation, and observation polling), but renders a
/// `RenderState` and reads keys instead of printing a plain-text
/// stream.
///
/// Review finding K1: the engine exiting does not, on its own, end
/// this loop — only three things do, and only once it has (DESIGN.md
/// §6.3's final-state screen): a confirmed `q` (`App::
/// quit_when_finished`), a Ctrl-C cancel, or any external INT/TERM/
/// HUP osp itself received (`leave_once_exited`, set by all three).
/// Short of one of those, a run that simply finishes on its own —
/// ok, failed, or otherwise — leaves this loop showing that final
/// state (the header's own ok/failed/cancelled/aborted, every row's
/// final status, `v` and the details pane all still live) until the
/// user presses `q` (no confirm, nothing left to cancel) or Ctrl-C.
/// `finish_run`'s plain summary still prints once this loop is well
/// and truly done, exactly as it already does for `--log`.
#[allow(clippy::too_many_arguments)]
fn run_tui(
    mut child: SupervisedChild,
    stderr_rx: std::sync::mpsc::Receiver<String>,
    spec: RunSpec,
    plan: PlanTree,
    run_dir: PathBuf,
    print_run_dir: bool,
    document: String,
    engine_name: String,
    mut signals: Option<SignalWatcher>,
) -> ExitCode {
    let mut live_poller = LiveStatePoller::new(spec.live_state_path());
    let mut events_tailer = EventsTailer::new(spec.events_path());
    let mut differ = Differ::new();
    let mut model = RunModel::new();
    let mut clock = Clock::new();
    let mut log_lines: Vec<String> = Vec::new();
    let mut current_state: Option<serde_json::Value> = None;
    let start = Instant::now();

    let mut app = App::new(false);
    let mut signal_count: u32 = 0;
    let mut kill_deadline: Option<Instant> = None;

    let guard_and_terminal = TerminalGuard::enter()
        .and_then(|g| Terminal::new(CrosstermBackend::new(std::io::stdout())).map(|t| (g, t)));
    let (_guard, mut terminal) = match guard_and_terminal {
        Ok(pair) => pair,
        Err(err) => {
            // DESIGN.md's own TTY check (`effective_log_mode`) said
            // this should be a real terminal; if it still isn't one
            // underneath (an exotic CI pty, a stdout swapped out from
            // under osp), fall back to a quiet wait rather than a
            // broken half-raw-mode session.
            eprintln!("osp: couldn't start the TUI ({err}); waiting for the engine to finish");
            let exit_status = child.wait().expect("waiting on a freshly spawned child");
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            let code = finish_run(
                &mut out,
                &mut clock,
                &mut child,
                &stderr_rx,
                &mut live_poller,
                &mut events_tailer,
                &mut differ,
                &mut model,
                &plan,
                &spec,
                &run_dir,
                exit_status,
                print_run_dir,
            );
            return ExitCode::from(code);
        }
    };

    let mut last_draw = Instant::now() - REDRAW_INTERVAL;
    let mut last_rebuild = Instant::now() - RENDER_REBUILD_INTERVAL;
    let mut dirty = true;
    let mut exit_status: Option<std::process::ExitStatus> = None;
    // K1: set once a confirmed `q`, a Ctrl-C cancel, or an external
    // signal has happened — the only three things that make this loop
    // leave the instant the engine exits, rather than staying open on
    // its final state until the user explicitly asks to leave.
    let mut leave_once_exited = false;
    let mut render_state = oscilloscope_core::render::build(
        &document,
        &engine_name,
        &plan,
        &mut model,
        current_state.as_ref(),
        ProcessState::Running,
        start.elapsed().as_secs_f64(),
        &[],
    );
    if app.selected_path.is_none() {
        app.selected_path = render_state.rows.first().map(|r| r.path.clone());
    }

    'tui: loop {
        if let Some(watcher) = signals.as_mut() {
            for sig in watcher.pending() {
                if exit_status.is_none() {
                    cancel_once(
                        &child,
                        sig,
                        &mut signal_count,
                        &mut kill_deadline,
                        &mut log_lines,
                        &mut clock,
                    );
                }
                leave_once_exited = true;
                dirty = true;
            }
        }
        // The engine already exited on its own (the finished screen is
        // up) and a fresh signal just arrived — K1's "any external
        // signal" trigger applies even then (a SIGHUP in particular
        // must never wait on a key with the terminal already gone).
        if leave_once_exited && exit_status.is_some() {
            break;
        }
        if let Some(deadline) = kill_deadline {
            if Instant::now() >= deadline && matches!(child.try_wait(), Ok(None)) {
                log_lines.push(format_log_line(
                    &mut clock,
                    None,
                    "engine still running 10s after the second signal; sending SIGKILL",
                ));
                child.kill_group();
                kill_deadline = None;
                dirty = true;
            }
        }

        for line in stderr_rx.try_iter() {
            log_lines.push(format_log_line(
                &mut clock,
                None,
                &format!("engine: {line}"),
            ));
            dirty = true;
        }

        if exit_status.is_none() {
            let (lines, state) = observe_tick(
                &mut live_poller,
                &mut events_tailer,
                &mut differ,
                &mut model,
                &plan,
            );
            dirty |= !lines.is_empty();
            for line in &lines {
                log_lines.push(format_log_line(&mut clock, line.ts.as_deref(), &line.text));
            }
            if let Some(state) = state {
                current_state = Some(state);
                dirty = true;
            }
        }

        // K4: a full rebuild is forced at least once a second even
        // with nothing else dirty, so the header's own elapsed time
        // still visibly ticks while the run is otherwise quiet.
        if last_rebuild.elapsed() >= RENDER_REBUILD_INTERVAL {
            dirty = true;
        }

        if dirty {
            let process = match exit_status {
                None => ProcessState::Running,
                Some(_) => ProcessState::Exited {
                    interrupted: signal_count > 0,
                },
            };
            render_state = oscilloscope_core::render::build(
                &document,
                &engine_name,
                &plan,
                &mut model,
                current_state.as_ref(),
                process,
                start.elapsed().as_secs_f64(),
                &[],
            );
            last_rebuild = Instant::now();
            if app.follow {
                if let Some(target) = App::follow_target(&render_state.rows) {
                    app.selected_path = Some(target.to_string());
                }
            }
            if app.selected_path.is_none() {
                app.selected_path = render_state.rows.first().map(|r| r.path.clone());
            }
        }
        // K4: computed every tick regardless (collapse/filter/errors-
        // only and the selection itself can change on a key alone),
        // but cheaply — against whatever `render_state.rows` the last
        // rebuild above left cached, not a fresh one every tick.
        let visible = keys::visible_rows(&render_state.rows, &app);

        if event::poll(KEY_POLL_INTERVAL).unwrap_or(false) {
            if let Ok(ev) = event::read() {
                match ev {
                    CEvent::Key(key) if key.kind == KeyEventKind::Press => {
                        let running = exit_status.is_none();
                        let has_prompt_sent = app.selected_path.as_deref().is_some_and(|p| {
                            oscilloscope_core::render::full_value(
                                p,
                                oscilloscope_core::render::FullValueField::PromptSent,
                                current_state.as_ref(),
                            )
                            .is_some()
                        });
                        match app.handle_key(key, &visible, running, has_prompt_sent) {
                            keys::Action::Cancel => {
                                cancel_once(
                                    &child,
                                    ForwardSignal::Int,
                                    &mut signal_count,
                                    &mut kill_deadline,
                                    &mut log_lines,
                                    &mut clock,
                                );
                                // K1: only a *confirmed* `q` leaves on
                                // its own once the engine exits — a
                                // plain `c` cancels but still shows
                                // the finished screen afterwards.
                                if app.quit_when_finished {
                                    leave_once_exited = true;
                                }
                            }
                            keys::Action::CtrlC => {
                                if running {
                                    cancel_once(
                                        &child,
                                        ForwardSignal::Int,
                                        &mut signal_count,
                                        &mut kill_deadline,
                                        &mut log_lines,
                                        &mut clock,
                                    );
                                    leave_once_exited = true;
                                } else {
                                    // K3: "with nothing running: quit".
                                    break 'tui;
                                }
                            }
                            keys::Action::Quit => break 'tui,
                            keys::Action::None => {}
                        }
                        dirty = true;
                    }
                    CEvent::Resize(_, _) => dirty = true,
                    _ => {}
                }
            }
        }

        if dirty && last_draw.elapsed() >= REDRAW_INTERVAL {
            let details = app
                .selected_path
                .as_deref()
                .map(|p| oscilloscope_core::render::details_for(p, &plan, current_state.as_ref()));
            let _ = terminal.draw(|f| {
                tui::draw(
                    f,
                    &render_state,
                    details.as_ref(),
                    &log_lines,
                    &app,
                    current_state.as_ref(),
                );
            });
            last_draw = Instant::now();
            dirty = false;
        }

        if exit_status.is_none() {
            if let Ok(Some(status)) = child.try_wait() {
                exit_status = Some(status);
                dirty = true;
                if leave_once_exited {
                    break;
                }
            }
        }
    }

    let exit_status = match exit_status {
        Some(status) => status,
        // `Action::Quit`/`Action::CtrlC`'s own "nothing running" arms
        // are only ever reachable once `exit_status` is already
        // `Some` (that's exactly what `running`/`App::handle_key`'s
        // own dialog logic means by "nothing running") — but the
        // engine is still osp's own child, and must never be left
        // behind regardless; one more wait costs nothing when it has
        // already exited, and is the only safety net if that
        // invariant were ever wrong.
        None => child.wait().expect("the engine has already exited"),
    };

    drop(terminal);
    drop(_guard);

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let code = finish_run(
        &mut out,
        &mut clock,
        &mut child,
        &stderr_rx,
        &mut live_poller,
        &mut events_tailer,
        &mut differ,
        &mut model,
        &plan,
        &spec,
        &run_dir,
        exit_status,
        print_run_dir,
    );
    ExitCode::from(code)
}

/// `cof`'s own pre-execution failure shape (DESIGN.md §4.1): with no
/// orchestration ever started, it prints exactly `{"ok":false,
/// "error":"..."}` to stdout and nothing reaches `--live-state`/`--out`
/// at all. Parsed as a stream of whole JSON values, not split by line
/// (M2): stdout is a pipe, so a real `cof` pretty-prints that object
/// across several lines (`console.print_json`'s default), and a
/// line-by-line scan never finds it there -- only against a fixture
/// someone had already flattened to one line by hand.
fn read_stdout_json_error(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut found = None;
    for value in serde_json::Deserializer::from_str(&text)
        .into_iter::<serde_json::Value>()
        .filter_map(Result::ok)
    {
        if value.get("ok").and_then(serde_json::Value::as_bool) == Some(false) {
            if let Some(error) = value.get("error").and_then(serde_json::Value::as_str) {
                found = Some(error.to_string());
            }
        }
    }
    found
}

/// The engine's own last word when neither state nor stdout said
/// anything at all (M2's last-resort reason): a trimmed, non-empty
/// line from the very end of its stderr tee.
fn last_nonempty_stderr_line(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    text.lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

/// The failed run's own reason, when `prime.meta.error` has none to
/// give (M2) -- in priority order: a `run_end` event's own `error`
/// (DESIGN.md §3's format table always carries one for a failing
/// run), then cof's pre-execution stdout JSON (DESIGN.md §4.1), then
/// its last stderr line.
fn failure_reason_fallback(run_dir: &Path, model: &RunModel) -> Option<String> {
    model
        .run_end_error()
        .map(str::to_string)
        .or_else(|| read_stdout_json_error(&run_dir.join("stdout.txt")))
        .or_else(|| last_nonempty_stderr_line(&run_dir.join("stderr.txt")))
}

/// One observation tick's lines, from whichever of state and events
/// had something new, merged and sorted by their own timestamps before
/// anything is printed (DESIGN.md §2: never by which source noticed
/// first) — shared between `do_run` and `do_watch`. Returns the
/// freshly polled live-state snapshot, if there was one, so a caller
/// that needs to check `run_ended` doesn't have to poll a second time.
/// One tick's lines, from whichever of events and state had something
/// new, merged and sorted by their own timestamps — split out from
/// [`drain_observations`] so it can be unit-tested without a real
/// `stdout` lock.
fn observe_tick(
    live_poller: &mut LiveStatePoller,
    events_tailer: &mut EventsTailer,
    differ: &mut Differ,
    model: &mut RunModel,
    plan: &PlanTree,
) -> (
    Vec<oscilloscope_core::diff::LogLine>,
    Option<serde_json::Value>,
) {
    // Events first, then the state diff (not the order either was
    // originally polled in): `diff_event` marks a path event-sourced
    // as it goes, and `diff` needs that already set for *this* tick to
    // skip its own line for a path whose state write landed in the
    // very same tick as its event — this real race only starts
    // mattering once `--events` is actually flowing (#423), which is
    // when it first showed up as a genuine duplicate line.
    let mut lines = Vec::new();
    for raw_event in events_tailer.poll() {
        if let Some(event) = parse_event(&raw_event) {
            model.observe_event(&event);
            lines.extend(differ.diff_event(&event, plan, model));
        }
    }
    let state = live_poller.poll();
    if let Some(state) = &state {
        lines.extend(differ.diff(state, plan));
    }
    oscilloscope_core::diff::sort_log_lines(&mut lines);
    (lines, state)
}

fn drain_observations(
    out: &mut std::io::StdoutLock<'_>,
    clock: &mut Clock,
    live_poller: &mut LiveStatePoller,
    events_tailer: &mut EventsTailer,
    differ: &mut Differ,
    model: &mut RunModel,
    plan: &PlanTree,
) -> Option<serde_json::Value> {
    let (lines, state) = observe_tick(live_poller, events_tailer, differ, model, plan);
    for line in lines {
        print_line(out, clock, line.ts.as_deref(), &line.text);
    }
    state
}

fn do_watch(args: WatchArgs) -> ExitCode {
    let target = PathBuf::from(&args.target);
    // P2-4: a typo'd path used to be silently treated as a direct
    // `state.live.json` target and waited on forever -- with no
    // staleness timeout (N3's own decision) there was nothing to ever
    // notice it, let alone say so.
    if !target.exists() {
        eprintln!("osp: watch target {} does not exist", target.display());
        return exit_early(2);
    }
    let (run_dir, live_state_path): (PathBuf, PathBuf) = if target.is_dir() {
        (target.clone(), target.join("state.live.json"))
    } else {
        (
            target
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from(".")),
            target.clone(),
        )
    };

    // P2-4: says once, up front, what this watch is actually waiting
    // on, rather than leaving a user looking at a silent, running
    // process with no clue what it's for -- the live-state file for a
    // run that simply hasn't started yet (or never will) looks
    // identical to one that's already hung.
    if !live_state_path.exists() {
        eprintln!("osp: waiting for {}", live_state_path.display());
    }

    // K4: the same options check -- `--config`/`-e`, when given --
    // `osp <doc>` itself uses, so `osp watch --plan doc.yml` compiles
    // the same plan a live `osp run` of that document would.
    let plan = match &args.plan {
        Some(doc) => match oscilloscope_core::plan::parse_inputs(&args.set) {
            Ok(inputs) => {
                match oscilloscope_core::plan::compile(doc, args.config.as_deref(), &inputs) {
                    Ok(program) => PlanTree::from_program_with_options(
                        &program,
                        args.config.as_deref(),
                        &inputs,
                    ),
                    Err(err) => {
                        eprintln!(
                            "osp: couldn't compile a plan ({err}); running from observations only"
                        );
                        PlanTree::empty()
                    }
                }
            }
            Err(err) => {
                eprintln!("osp: couldn't compile a plan ({err}); running from observations only");
                PlanTree::empty()
            }
        },
        None => PlanTree::empty(),
    };

    // DESIGN.md §6.2/§6.3: same TTY/—log split as `osp <doc>`; `q` in
    // this one just detaches (§6.3), since watch owns no engine of
    // its own to ever cancel.
    if !effective_log_mode(args.log) {
        return run_tui_watch(run_dir, live_state_path, plan, args.target.clone());
    }

    let mut clock = Clock::new();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    let mut live_poller = LiveStatePoller::new(&live_state_path);
    let mut events_tailer = EventsTailer::new(run_dir.join("events.jsonl"));
    let mut differ = Differ::new();
    let mut model = RunModel::new();
    let mut signals = SignalWatcher::new().ok();

    // F4: `osp watch` owns no `Child` for a run it didn't start, so it
    // cannot simply wait on it — and the one state-only stop condition
    // this loop used to have, a completed live-state write, never
    // happens for a run that aborts (a second signal, SIGKILL, a
    // crash: DESIGN.md §1.1's "no final write and no --out"), which
    // made watch loop forever. It now also stops on an events `run_end`
    // (DESIGN.md §3: the final live-state write is already on disk by
    // then) and, failing both, once the engine's own pid — from a
    // `run_start` event, when the stream has one — is confirmed dead.
    let mut last_state: Option<serde_json::Value> = None;
    // N3: a run supervised by a bare `cof run --live-state ... --out
    // ...` (no `--events`) never gives this loop a pid to check, and
    // an aborted run (a second signal, SIGKILL, a crash) never writes
    // a completed state either, so neither of this loop's other two
    // stop conditions can ever fire for it. The orchestrator's own
    // decision on this finding: no staleness timeout (a quiet run can
    // legitimately stay quiet for a long time) — just tell the user,
    // once, what watch is relying on instead: Ctrl-C.
    let mut warned_no_events = false;
    let events_path = run_dir.join("events.jsonl");
    // P2-7: a local Ctrl-C is recorded rather than returned on the
    // spot, so watch still reaches the same ending every other stop
    // condition does — draining whatever's left, then the same
    // summary and `exit` lines `osp`'s own run prints.
    let mut local_signal: Option<ForwardSignal> = None;
    loop {
        if let Some(watcher) = signals.as_mut() {
            if let Some(sig) = watcher.pending().into_iter().next() {
                local_signal = Some(sig);
                break;
            }
        }
        if let Some(state) = drain_observations(
            &mut out,
            &mut clock,
            &mut live_poller,
            &mut events_tailer,
            &mut differ,
            &mut model,
            &plan,
        ) {
            last_state = Some(state);
        }
        if last_state
            .as_ref()
            .is_some_and(oscilloscope_core::model::run_ended)
        {
            break;
        }
        if !warned_no_events
            && last_state.is_some()
            && model.run_start_pid().is_none()
            && !model.run_ended_by_events()
            && !events_path.exists()
        {
            eprintln!(
                "osp: this run has no --events stream; an aborted run can't be \
                 detected without one. Ctrl-C stops this watch."
            );
            warned_no_events = true;
        }
        if model.run_ended_by_events() {
            // DESIGN.md §3's own ordering guarantee: the final
            // live-state write already landed before `run_end` did, so
            // one more poll picks it up for the exit-code check below
            // even if this tick's `drain_observations` read the events
            // file first.
            if let Some(state) = drain_observations(
                &mut out,
                &mut clock,
                &mut live_poller,
                &mut events_tailer,
                &mut differ,
                &mut model,
                &plan,
            ) {
                last_state = Some(state);
            }
            break;
        }
        if let Some(pid) = model.run_start_pid() {
            if !oscilloscope_core::supervise::process_alive(pid) {
                break;
            }
        }
        std::thread::sleep(POLL_INTERVAL);
    }

    let code = finish_watch(
        &mut out,
        &mut clock,
        &mut live_poller,
        &mut events_tailer,
        &mut differ,
        &mut model,
        &plan,
        &run_dir,
        &mut last_state,
        local_signal,
    );
    let _ = writeln!(out, "exit {code}");
    ExitCode::from(code as u8)
}

/// `osp watch`'s own TUI loop (DESIGN.md §6.3): the same render/key
/// loop `run_tui` gives a live run, but with none of its own engine
/// to supervise or cancel — `q` always just detaches (§6.3: "In `osp
/// watch`, `q` just detaches"), with no confirm, whether or not the
/// watched run is still going.
///
/// Review finding K1: watching a run that's already finished (or one
/// that finishes while this is open) stays on that final state until
/// the user presses `q` or Ctrl-C, same as `run_tui` — it does not
/// detach back to the shell the instant `run_ended`/a dead pid is
/// first noticed, which used to show the finished screen for about
/// one redraw before leaving on its own.
fn run_tui_watch(
    run_dir: PathBuf,
    live_state_path: PathBuf,
    plan: PlanTree,
    document: String,
) -> ExitCode {
    let mut live_poller = LiveStatePoller::new(&live_state_path);
    let mut events_tailer = EventsTailer::new(run_dir.join("events.jsonl"));
    let mut differ = Differ::new();
    let mut model = RunModel::new();
    let mut clock = Clock::new();
    let mut log_lines: Vec<String> = Vec::new();
    let mut last_state: Option<serde_json::Value> = None;
    let mut warned_no_events = false;
    let events_path = run_dir.join("events.jsonl");
    let mut signals = SignalWatcher::new().ok();
    let mut local_signal: Option<ForwardSignal> = None;
    let start = Instant::now();

    let mut app = App::new(true);

    let guard_and_terminal = TerminalGuard::enter()
        .and_then(|g| Terminal::new(CrosstermBackend::new(std::io::stdout())).map(|t| (g, t)));
    let (_guard, mut terminal) = match guard_and_terminal {
        Ok(pair) => pair,
        Err(err) => {
            eprintln!("osp: couldn't start the TUI ({err}); falling back to waiting quietly");
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            let code = finish_watch(
                &mut out,
                &mut clock,
                &mut live_poller,
                &mut events_tailer,
                &mut differ,
                &mut model,
                &plan,
                &run_dir,
                &mut last_state,
                None,
            );
            let _ = writeln!(out, "exit {code}");
            return ExitCode::from(code as u8);
        }
    };

    let mut last_draw = Instant::now() - REDRAW_INTERVAL;
    let mut last_rebuild = Instant::now() - RENDER_REBUILD_INTERVAL;
    let mut dirty = true;
    // K1: once the watched run has ended (by state, by events, or by
    // its pid going away), this loop keeps running — redrawing the
    // final state and reading keys — rather than detaching on its
    // own; only `ended` stops `observe_tick` from polling a run
    // directory that has nothing left to say.
    let mut ended = false;
    let mut render_state = oscilloscope_core::render::build(
        &document,
        "watch",
        &plan,
        &mut model,
        last_state.as_ref(),
        ProcessState::Running,
        start.elapsed().as_secs_f64(),
        &[],
    );
    if app.selected_path.is_none() {
        app.selected_path = render_state.rows.first().map(|r| r.path.clone());
    }

    'tui: loop {
        if let Some(watcher) = signals.as_mut() {
            if let Some(sig) = watcher.pending().into_iter().next() {
                local_signal = Some(sig);
                break;
            }
        }

        if !ended {
            let (lines, state) = observe_tick(
                &mut live_poller,
                &mut events_tailer,
                &mut differ,
                &mut model,
                &plan,
            );
            dirty |= !lines.is_empty();
            for line in &lines {
                log_lines.push(format_log_line(&mut clock, line.ts.as_deref(), &line.text));
            }
            if let Some(state) = state {
                last_state = Some(state);
                dirty = true;
            }

            if last_state
                .as_ref()
                .is_some_and(oscilloscope_core::model::run_ended)
            {
                ended = true;
            }
            if !warned_no_events
                && last_state.is_some()
                && model.run_start_pid().is_none()
                && !model.run_ended_by_events()
                && !events_path.exists()
            {
                log_lines.push(format_log_line(
                    &mut clock,
                    None,
                    "this run has no --events stream; an aborted run can't be detected \
                     without one. q detaches.",
                ));
                warned_no_events = true;
                dirty = true;
            }
            if !ended && model.run_ended_by_events() {
                let (more_lines, state) = observe_tick(
                    &mut live_poller,
                    &mut events_tailer,
                    &mut differ,
                    &mut model,
                    &plan,
                );
                for line in &more_lines {
                    log_lines.push(format_log_line(&mut clock, line.ts.as_deref(), &line.text));
                }
                if let Some(state) = state {
                    last_state = Some(state);
                }
                ended = true;
            }
            if !ended {
                if let Some(pid) = model.run_start_pid() {
                    if !oscilloscope_core::supervise::process_alive(pid) {
                        ended = true;
                    }
                }
            }
            if ended {
                dirty = true;
            }
        }

        // K4, same as `run_tui`.
        if last_rebuild.elapsed() >= RENDER_REBUILD_INTERVAL {
            dirty = true;
        }

        if dirty {
            let process = if ended {
                ProcessState::Exited { interrupted: false }
            } else {
                ProcessState::Running
            };
            render_state = oscilloscope_core::render::build(
                &document,
                "watch",
                &plan,
                &mut model,
                last_state.as_ref(),
                process,
                start.elapsed().as_secs_f64(),
                &[],
            );
            last_rebuild = Instant::now();
            if app.follow {
                if let Some(target) = App::follow_target(&render_state.rows) {
                    app.selected_path = Some(target.to_string());
                }
            }
            if app.selected_path.is_none() {
                app.selected_path = render_state.rows.first().map(|r| r.path.clone());
            }
        }
        let visible = keys::visible_rows(&render_state.rows, &app);

        if event::poll(KEY_POLL_INTERVAL).unwrap_or(false) {
            if let Ok(ev) = event::read() {
                match ev {
                    CEvent::Key(key) if key.kind == KeyEventKind::Press => {
                        // Watch owns no engine to cancel (DESIGN.md
                        // §6.3), so `running` is always false here:
                        // `c` does nothing, and `q` detaches at once,
                        // with no confirm, whether the watched run is
                        // still going or not.
                        let has_prompt_sent = app.selected_path.as_deref().is_some_and(|p| {
                            oscilloscope_core::render::full_value(
                                p,
                                oscilloscope_core::render::FullValueField::PromptSent,
                                last_state.as_ref(),
                            )
                            .is_some()
                        });
                        match app.handle_key(key, &visible, false, has_prompt_sent) {
                            keys::Action::Quit => break 'tui,
                            // K3: Ctrl-C in `osp watch` always just
                            // detaches too, same as `q` — but reports
                            // the same 130 a real forwarded SIGINT
                            // would, not the final state's own code.
                            keys::Action::CtrlC => {
                                local_signal = Some(ForwardSignal::Int);
                                break 'tui;
                            }
                            keys::Action::Cancel | keys::Action::None => {}
                        }
                        dirty = true;
                    }
                    CEvent::Resize(_, _) => dirty = true,
                    _ => {}
                }
            }
        }

        if dirty && last_draw.elapsed() >= REDRAW_INTERVAL {
            let details = app
                .selected_path
                .as_deref()
                .map(|p| oscilloscope_core::render::details_for(p, &plan, last_state.as_ref()));
            let _ = terminal.draw(|f| {
                tui::draw(
                    f,
                    &render_state,
                    details.as_ref(),
                    &log_lines,
                    &app,
                    last_state.as_ref(),
                );
            });
            last_draw = Instant::now();
            dirty = false;
        }
    }

    drop(terminal);
    drop(_guard);

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let code = finish_watch(
        &mut out,
        &mut clock,
        &mut live_poller,
        &mut events_tailer,
        &mut differ,
        &mut model,
        &plan,
        &run_dir,
        &mut last_state,
        local_signal,
    );
    let _ = writeln!(out, "exit {code}");
    ExitCode::from(code as u8)
}

/// The common ending every `osp watch` session reaches, however its
/// own loop stopped (a completed run, a local Ctrl-C, or a dead pid
/// with nothing left to watch): one more drain for anything that
/// landed between the loop's last check and now, the same single
/// final-summary computation `do_run`'s own `finish_run` uses (M1),
/// and the exit code `osp run` of the same document would have used
/// — from a signal `run_end` named, or from watch's own local Ctrl-C,
/// rather than only ok-vs-failed. Returns that exit code; the caller
/// prints it and `ExitCode::from`s it, the same shape `finish_run`
/// leaves to its own callers.
#[allow(clippy::too_many_arguments)]
fn finish_watch(
    out: &mut std::io::StdoutLock<'_>,
    clock: &mut Clock,
    live_poller: &mut LiveStatePoller,
    events_tailer: &mut EventsTailer,
    differ: &mut Differ,
    model: &mut RunModel,
    plan: &PlanTree,
    run_dir: &Path,
    last_state: &mut Option<serde_json::Value>,
    local_signal: Option<ForwardSignal>,
) -> u8 {
    if let Some(state) =
        drain_observations(out, clock, live_poller, events_tailer, differ, model, plan)
    {
        *last_state = Some(state);
    }

    let final_state = std::fs::read(run_dir.join("state.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    if let Some(state) = &final_state {
        for line in differ.diff(state, plan) {
            print_line(out, clock, line.ts.as_deref(), &line.text);
        }
        *last_state = Some(state.clone());
    }
    let summary = final_state
        .as_ref()
        .and_then(|state| differ.finish(state, failure_reason_fallback(run_dir, model).as_deref()))
        .or_else(|| {
            if differ.run_line_emitted() {
                return None;
            }
            Some(oscilloscope_core::diff::LogLine {
                ts: None,
                text: "■ run aborted (no final state)".to_string(),
            })
        });
    if let Some(line) = summary {
        print_line(out, clock, line.ts.as_deref(), &line.text);
    }

    if let Some(sig) = local_signal {
        match sig {
            ForwardSignal::Int => 130,
            ForwardSignal::Term => 143,
            ForwardSignal::Hup => 129,
        }
    } else if let Some(signal) = model.run_end_signal() {
        exit_code_for_signal_name(signal).unwrap_or(1) as u8
    } else {
        match last_state {
            Some(state) if oscilloscope_core::model::run_ended(state) => {
                if oscilloscope_core::model::run_ok(state) {
                    0
                } else {
                    1
                }
            }
            _ if model.run_end_ok() == Some(true) => 0,
            _ => {
                // Stopped on a dead pid, with nothing that counts as a
                // clean completion: an abort (F4).
                1
            }
        }
    }
}

/// Prints a clap parse error to the right stream and returns its exit
/// code: `--help`/`--version` (exit 0) print to stdout, same as any
/// other successful output; an actual usage error (exit 2) prints to
/// stderr, same as every other error `osp` reports (#422 review note).
fn report_clap_error(err: clap::Error) -> ExitCode {
    if err.exit_code() == 0 {
        print!("{err}");
    } else {
        eprint!("{err}");
    }
    ExitCode::from(err.exit_code() as u8)
}

fn run(args: impl IntoIterator<Item = String>) -> ExitCode {
    let args: Vec<String> = args.into_iter().collect();
    if args.get(1).map(String::as_str) == Some("watch") {
        let watch_args = std::iter::once("osp watch".to_string()).chain(args.into_iter().skip(2));
        match WatchArgs::try_parse_from(watch_args) {
            Ok(args) => do_watch(args),
            Err(err) => report_clap_error(err),
        }
    } else {
        match RunArgs::try_parse_from(args) {
            Ok(args) => do_run(args),
            Err(err) => report_clap_error(err),
        }
    }
}

fn main() -> ExitCode {
    // `args_os` + lossy conversion instead of `args()`, which panics on
    // a non-UTF-8 argument (same reasoning as electricity-cli's `main`).
    let args = std::env::args_os().map(|a| a.to_string_lossy().into_owned());
    run(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(words: &[&str]) -> Vec<String> {
        std::iter::once("osp".to_string())
            .chain(words.iter().map(|w| w.to_string()))
            .collect()
    }

    #[test]
    fn version_flag_exits_zero() {
        assert_eq!(run(args(&["--version"])), ExitCode::SUCCESS);
    }

    #[test]
    fn help_flag_exits_zero() {
        assert_eq!(run(args(&["--help"])), ExitCode::SUCCESS);
    }

    #[test]
    fn an_actually_unknown_flag_is_a_usage_error() {
        assert_eq!(
            run(args(&["--bogus-flag", "do-thing.yml"])),
            ExitCode::from(2)
        );
    }

    #[test]
    fn recognized_flags_work_before_the_orchestration_positional() {
        // DESIGN.md §4.1/issue #424's own synopsis: "Flags may also
        // appear before the positionals." Asserted at the `clap` parse
        // alone (`RunArgs::try_parse_from`, never `run`/`do_run`): F11 —
        // `do_run` reaches `CofEngine::detect("cof")`, which runs the
        // real `cof run --help` under this process's own real `HOME`
        // and credentials whenever `cof` is on `PATH`, in *every*
        // `cargo test` run, not just the gated end-to-end suite — and
        // then leaves an `osp-*` temp directory behind.
        let parsed = RunArgs::try_parse_from(args(&["-e", "k=v", "--log", "do-thing.yml"]));
        let parsed = parsed.expect("flags before the positional should parse");
        assert_eq!(parsed.orchestration, "do-thing.yml");
        assert_eq!(parsed.set, vec!["k=v".to_string()]);
        assert!(parsed.log);
    }

    #[test]
    fn no_args_is_a_usage_error() {
        assert_eq!(run(args(&[])), ExitCode::from(2));
    }

    #[test]
    fn a_signal_already_pending_before_the_spawn_gives_its_exit_code_with_no_spawn() {
        // P2-10: registering *this test's own* `SignalWatcher` first,
        // then sending the signal to this very process, exercises the
        // exact pre-spawn race deterministically -- no engine process
        // and no wall-clock delay involved, so the kernel's own
        // default SIGINT disposition (process death) is never a risk
        // the way it would be sending a signal *before* any watcher is
        // registered at all.
        let mut signals = SignalWatcher::new().ok();
        assert_eq!(pending_signal_exit_code(&mut signals), None);

        unsafe {
            libc::kill(std::process::id() as i32, libc::SIGINT);
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut code = None;
        while code.is_none() && std::time::Instant::now() < deadline {
            code = pending_signal_exit_code(&mut signals);
        }
        assert_eq!(code, Some(130));
    }

    #[test]
    fn electricity_with_no_config_is_a_clear_usage_error() {
        let dir = tempfile::tempdir().unwrap();
        let doc = dir.path().join("do.yml");
        std::fs::write(&doc, "effects: []\n").unwrap();
        // P2-1: an explicit --out-dir in this test's own TempDir, not
        // the default temp directory -- this error path returns
        // before the run directory is ever created (P2-1), but every
        // test that reaches do_run uses its own --out-dir regardless,
        // so a future change along this path can't start leaking
        // osp-* directories into a shared /tmp on every cargo test.
        let out_dir = dir.path().join("out");
        let code = run(args(&[
            doc.to_str().unwrap(),
            "--engine",
            "electricity",
            "--out-dir",
            out_dir.to_str().unwrap(),
        ]));
        assert_eq!(code, ExitCode::from(2));
    }

    #[test]
    fn watch_is_dispatched_to_watch_args_not_run_args() {
        // "watch" with no target is a WatchArgs usage error (missing
        // the required `target` positional) — proving dispatch reached
        // WatchArgs, not RunArgs (which has no required `target` field).
        assert_eq!(run(args(&["watch"])), ExitCode::from(2));
    }

    #[test]
    fn watch_exits_2_on_a_target_that_does_not_exist() {
        // P2-4: a typo'd path must not be silently treated as a live
        // -state file and waited on forever.
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let code = run(args(&["watch", missing.to_str().unwrap()]));
        assert_eq!(code, ExitCode::from(2));
    }

    #[test]
    fn watch_attaches_to_a_run_directory_with_only_a_final_state() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("state.live.json"),
            r#"{"runtime":{"last_run":{"completed_at":"t"}},"prime":{"value":true,"meta":{"error":null}}}"#,
        )
        .unwrap();
        // Finding 7: `--log` forces the plain stream even on this
        // test runner's own TTY (an interactive `cargo test` run), so
        // this never enters the real TUI's raw mode / alternate screen
        // — and, since K1, never waits on a key nothing here will send.
        let code = run(args(&["watch", dir.path().to_str().unwrap(), "--log"]));
        assert_eq!(code, ExitCode::from(0));
    }

    #[test]
    fn watch_stops_on_a_dead_run_start_pid_instead_of_hanging_forever() {
        // F4: an aborted run (a second signal, SIGKILL, a crash) never
        // writes a final snapshot, so `run_ended(state)` alone never
        // becomes true. `osp watch` must still notice, from the
        // engine's own pid (announced by a `run_start` event) going
        // away, rather than looping forever.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("state.live.json"),
            r#"{"runtime":{"last_run":{"completed_at":null}},"prime":{"value":null,"meta":{"completed_at":null}}}"#,
        )
        .unwrap();
        let mut dead = std::process::Command::new("true").spawn().unwrap();
        let dead_pid = dead.id();
        dead.wait().unwrap();
        std::fs::write(
            dir.path().join("events.jsonl"),
            format!(
                "{{\"v\":1,\"seq\":0,\"ts\":\"t\",\"ev\":\"run_start\",\"run_id\":\"r\",\"pid\":{dead_pid}}}\n"
            ),
        )
        .unwrap();

        let target = dir.path().to_path_buf();
        let handle = std::thread::spawn(move || {
            // Finding 7: `--log`, same reason as the test above.
            run(std::iter::once("osp".to_string()).chain([
                "watch".to_string(),
                target.to_str().unwrap().to_string(),
                "--log".to_string(),
            ]))
        });
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if handle.is_finished() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "osp watch hung instead of noticing the dead pid"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(handle.join().unwrap(), ExitCode::from(1));
    }

    #[test]
    fn watch_reports_the_signal_run_end_named_not_just_ok_vs_failed() {
        // P2-7: a run_end event names the real signal that ended the
        // run (DESIGN.md §3's format table); osp watch should report
        // that same exit code (130 for SIGINT), not just 1.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("state.live.json"),
            r#"{"runtime":{"last_run":{"completed_at":null}},"prime":{"value":null,"meta":{"completed_at":null}}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("events.jsonl"),
            "{\"v\":1,\"seq\":0,\"ts\":\"t\",\"ev\":\"run_end\",\"ok\":false,\"error\":\"Interrupted (Ctrl-C/SIGINT)\",\"signal\":\"SIGINT\"}\n",
        )
        .unwrap();

        // Finding 7: `--log`, same reason as the first watch test above.
        let code = run(args(&["watch", dir.path().to_str().unwrap(), "--log"]));
        assert_eq!(code, ExitCode::from(130));
    }

    #[test]
    fn clock_formats_elapsed_as_mm_ss_tenths() {
        assert_eq!(Clock::format(0.0), "00:00.0");
        assert_eq!(Clock::format(2.94), "00:02.9");
        assert_eq!(Clock::format(65.0), "01:05.0");
    }

    #[test]
    fn clock_anchors_to_the_first_timestamp_then_tracks_event_time() {
        let mut clock = Clock::new();
        assert_eq!(clock.elapsed_for(Some("2026-10-08T19:56:22.000Z")), 0.0);
        let elapsed = clock.elapsed_for(Some("2026-10-08T19:56:24.500Z"));
        assert!((elapsed - 2.5).abs() < 1e-9);
    }

    #[test]
    fn clock_never_prints_an_earlier_elapsed_time_than_it_already_has() {
        // K5: a backdated line (DESIGN.md §2.4's "new node that is
        // already complete" row, or events and state simply landing a
        // tick apart) can carry a `ts` earlier than one the clock has
        // already shown — the printed mm:ss.s must never visibly run
        // backwards even then.
        let mut clock = Clock::new();
        assert_eq!(clock.elapsed_for(Some("2026-10-08T19:56:25.000Z")), 0.0);
        let later = clock.elapsed_for(Some("2026-10-08T19:56:28.000Z"));
        assert!((later - 3.0).abs() < 1e-9);
        let backdated = clock.elapsed_for(Some("2026-10-08T19:56:22.000Z"));
        assert_eq!(
            backdated, later,
            "a backdated ts must not move the display backwards"
        );
    }

    #[test]
    fn effective_log_mode_is_true_when_explicitly_requested() {
        assert!(effective_log_mode(true));
    }

    #[test]
    fn a_leaf_completing_in_the_same_tick_as_its_event_prints_once_not_twice() {
        // Once `--events` is actually flowing (#423), a fast effect's
        // `start`/`end` and its own state write can land in the same
        // 100ms poll tick. `diff_event` must run first and mark the
        // path event-sourced before `diff` looks at the same state, or
        // both emit the same ✓ line.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("state.live.json"),
            r#"{"prime": {"value": true, "meta": {"completed_at": "t1", "error": null, "flow": "chain"},
                "hello": {"value": "hi\n", "meta": {"created_at": "t0", "completed_at": "t1", "error": null, "provider": "shell", "stdout": "hi\n"}}
            }}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("events.jsonl"),
            "{\"v\":1,\"seq\":0,\"ts\":\"t0\",\"ev\":\"start\",\"id\":1,\"path\":\"prime.hello\"}\n\
             {\"v\":1,\"seq\":1,\"ts\":\"t1\",\"ev\":\"end\",\"id\":1,\"path\":\"prime.hello\",\"ok\":true,\"ms\":5}\n",
        )
        .unwrap();

        let mut live_poller = LiveStatePoller::new(dir.path().join("state.live.json"));
        let mut events_tailer = EventsTailer::new(dir.path().join("events.jsonl"));
        let mut differ = Differ::new();
        let mut model = RunModel::new();
        let (lines, _state) = observe_tick(
            &mut live_poller,
            &mut events_tailer,
            &mut differ,
            &mut model,
            &PlanTree::empty(),
        );
        let done: Vec<_> = lines
            .iter()
            .filter(|l| l.text.starts_with("✓ prime.hello"))
            .collect();
        assert_eq!(done.len(), 1, "{lines:?}");
    }

    #[test]
    fn create_run_dir_is_always_private() {
        // F2: a run directory can hold state with prompt and tool
        // output, so it must never be left group/world-readable.
        let parent = tempfile::tempdir().unwrap();

        let default_dir = parent.path().join("default");
        create_run_dir(&default_dir, true).unwrap();
        let mode = std::fs::metadata(&default_dir)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);

        let out_dir = parent.path().join("nested").join("out");
        create_run_dir(&out_dir, false).unwrap();
        let mode = std::fs::metadata(&out_dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[test]
    fn create_run_dir_refuses_a_default_path_that_already_exists() {
        // The default directory's name (`osp-<pid>-<nanos>`) is
        // predictable; refusing an existing entry there rather than
        // reusing it is cheap insurance against a planted/colliding
        // directory.
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join("taken");
        std::fs::create_dir(&path).unwrap();
        assert!(create_run_dir(&path, true).is_err());
    }

    #[test]
    fn create_run_dir_never_chmods_an_out_dir_it_did_not_create() {
        // K7: forcing 0700 onto a directory the caller already had is
        // an unrequested change to something outside osp's own run
        // directory -- it must be left exactly as the caller made it,
        // not quietly locked down.
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join("reused");
        {
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .mode(0o755)
                .create(&path)
                .unwrap();
        }
        create_run_dir(&path, false).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o755,
            "a pre-existing out-dir's mode must be left untouched"
        );
    }

    #[test]
    fn a_reused_out_dir_does_not_replay_the_previous_runs_final_state() {
        // F3: the previous run's `state.live.json`/`state.json`/
        // `events.jsonl` must be gone before the engine is spawned into
        // a reused `--out-dir`, or the first poll reads the *old* run's
        // final snapshot and prints its whole log (including its own
        // `■ run` line) before this run has written anything of its
        // own.
        let dir = tempfile::tempdir().unwrap();
        let run_dir = dir.path().join("run");
        create_run_dir(&run_dir, false).unwrap();
        let spec = oscilloscope_core::engine::RunSpec {
            orchestration: PathBuf::from("do.yml"),
            config: None,
            sets: vec![],
            run_dir: run_dir.clone(),
        };
        std::fs::write(
            spec.live_state_path(),
            r#"{"runtime":{"last_run":{"completed_at":"t"}},"prime":{"value":true,"meta":{"error":null}}}"#,
        )
        .unwrap();
        std::fs::write(spec.out_path(), "{}").unwrap();
        std::fs::write(spec.events_path(), "").unwrap();

        for stale in [spec.live_state_path(), spec.out_path(), spec.events_path()] {
            let _ = std::fs::remove_file(&stale);
        }

        assert!(!spec.live_state_path().exists());
        assert!(!spec.out_path().exists());
        assert!(!spec.events_path().exists());
    }

    const ONE_EFFECT_FINAL_STATE: &str = r#"{"runtime": {"last_run": {"completed_at": "2026-10-08T23:53:11.508492+00:00", "totals": {"wall_time_s": 0.1}}},
        "prime": {"value": true, "meta": {"completed_at": "2026-10-08T23:53:11.508492+00:00", "error": null, "flow": "chain"},
            "hello": {"value": "hi\n", "meta": {"created_at": "2026-10-08T23:53:11.500000+00:00", "completed_at": "2026-10-08T23:53:11.508360+00:00", "error": null, "provider": "shell", "stdout": "hi\n"}}
        }}"#;

    const ONE_EFFECT_EVENTS_COMPLETE: &str = "{\"v\":1,\"seq\":0,\"ts\":\"2026-10-08T23:53:11.497Z\",\"ev\":\"run_start\",\"run_id\":\"r\",\"orchestration\":\"d\",\"engine\":\"cof\",\"pid\":1}\n\
         {\"v\":1,\"seq\":1,\"ts\":\"2026-10-08T23:53:11.500Z\",\"ev\":\"start\",\"id\":1,\"path\":\"prime.hello\"}\n\
         {\"v\":1,\"seq\":2,\"ts\":\"2026-10-08T23:53:11.508Z\",\"ev\":\"end\",\"id\":1,\"path\":\"prime.hello\",\"ok\":true,\"ms\":8}\n\
         {\"v\":1,\"seq\":3,\"ts\":\"2026-10-08T23:53:11.509Z\",\"ev\":\"run_end\",\"ok\":true}\n";

    const ONE_EFFECT_EVENTS_START_ONLY: &str = "{\"v\":1,\"seq\":0,\"ts\":\"2026-10-08T23:53:11.497Z\",\"ev\":\"run_start\",\"run_id\":\"r\",\"orchestration\":\"d\",\"engine\":\"cof\",\"pid\":1}\n\
         {\"v\":1,\"seq\":1,\"ts\":\"2026-10-08T23:53:11.500Z\",\"ev\":\"start\",\"id\":1,\"path\":\"prime.hello\"}\n";

    #[test]
    fn the_summary_line_is_last_when_the_events_stream_is_already_complete() {
        // M1, case A: the events writer has already caught up (its own
        // `end`/`run_end` already on disk) by the same tick the state
        // write lands -- the exact race the measured bug hit, since
        // the event's own millisecond-plus-`Z` timestamp and the
        // state's microsecond-plus-offset one for the same instant
        // are not comparable as strings at all.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("state.live.json"), ONE_EFFECT_FINAL_STATE).unwrap();
        std::fs::write(dir.path().join("state.json"), ONE_EFFECT_FINAL_STATE).unwrap();
        std::fs::write(dir.path().join("events.jsonl"), ONE_EFFECT_EVENTS_COMPLETE).unwrap();

        let mut live_poller = LiveStatePoller::new(dir.path().join("state.live.json"));
        let mut events_tailer = EventsTailer::new(dir.path().join("events.jsonl"));
        let mut differ = Differ::new();
        let mut model = RunModel::new();
        let plan = PlanTree::empty();

        let mut buf: Vec<u8> = Vec::new();
        let mut clock = Clock::new();
        let (lines, _state) = observe_tick(
            &mut live_poller,
            &mut events_tailer,
            &mut differ,
            &mut model,
            &plan,
        );
        for l in lines {
            print_line(&mut buf, &mut clock, l.ts.as_deref(), &l.text);
        }
        let final_state: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("state.json")).unwrap()).unwrap();
        for l in differ.diff(&final_state, &plan) {
            print_line(&mut buf, &mut clock, l.ts.as_deref(), &l.text);
        }
        if let Some(l) = differ.finish(&final_state, None) {
            print_line(&mut buf, &mut clock, l.ts.as_deref(), &l.text);
        }

        let text = String::from_utf8(buf).unwrap();
        let printed: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
        assert!(printed.last().unwrap().contains("■ run ok"), "{printed:?}");
        assert!(
            printed[printed.len() - 2].contains("✓ prime.hello"),
            "{printed:?}"
        );
    }

    #[test]
    fn the_summary_line_is_last_when_the_state_write_arrives_before_the_events_stream_does() {
        // M1, case B: the reverse race -- the live-state file already
        // shows the run complete while the events writer is still
        // catching up (only `start` on disk so far), the end event
        // and the final `--out` write both landing only on the next
        // tick (`do_run`'s own final drain, right after the engine
        // exits).
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("state.live.json"), ONE_EFFECT_FINAL_STATE).unwrap();
        std::fs::write(
            dir.path().join("events.jsonl"),
            ONE_EFFECT_EVENTS_START_ONLY,
        )
        .unwrap();

        let mut live_poller = LiveStatePoller::new(dir.path().join("state.live.json"));
        let mut events_tailer = EventsTailer::new(dir.path().join("events.jsonl"));
        let mut differ = Differ::new();
        let mut model = RunModel::new();
        let plan = PlanTree::empty();

        let mut buf: Vec<u8> = Vec::new();
        let mut clock = Clock::new();
        let (lines, _state) = observe_tick(
            &mut live_poller,
            &mut events_tailer,
            &mut differ,
            &mut model,
            &plan,
        );
        for l in lines {
            print_line(&mut buf, &mut clock, l.ts.as_deref(), &l.text);
        }

        std::fs::write(dir.path().join("events.jsonl"), ONE_EFFECT_EVENTS_COMPLETE).unwrap();
        std::fs::write(dir.path().join("state.json"), ONE_EFFECT_FINAL_STATE).unwrap();

        let (lines2, _state2) = observe_tick(
            &mut live_poller,
            &mut events_tailer,
            &mut differ,
            &mut model,
            &plan,
        );
        for l in lines2 {
            print_line(&mut buf, &mut clock, l.ts.as_deref(), &l.text);
        }
        let final_state: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.path().join("state.json")).unwrap()).unwrap();
        for l in differ.diff(&final_state, &plan) {
            print_line(&mut buf, &mut clock, l.ts.as_deref(), &l.text);
        }
        if let Some(l) = differ.finish(&final_state, None) {
            print_line(&mut buf, &mut clock, l.ts.as_deref(), &l.text);
        }

        let text = String::from_utf8(buf).unwrap();
        let printed: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
        assert!(printed.last().unwrap().contains("■ run ok"), "{printed:?}");
        assert!(
            printed[printed.len() - 2].contains("✓ prime.hello"),
            "{printed:?}"
        );
    }

    #[test]
    fn read_stdout_json_error_parses_a_pretty_printed_object() {
        // M2: a real `cof`, with stdout as a pipe, pretty-prints its
        // pre-execution failure object across several lines -- a
        // line-by-line scan never finds it there, only a fixture
        // already flattened to one line by hand.
        let dir = tempfile::tempdir().unwrap();
        let stdout_path = dir.path().join("stdout.txt");
        std::fs::write(
            &stdout_path,
            "{\n  \"ok\": false,\n  \"error\": \"Orchestration validation failed: bad type\",\n  \"warnings\": []\n}\n",
        )
        .unwrap();
        assert_eq!(
            read_stdout_json_error(&stdout_path),
            Some("Orchestration validation failed: bad type".to_string())
        );
    }

    #[test]
    fn a_validation_failure_puts_cofs_own_reason_on_the_summary_line() {
        // M2: a run that ended with no `prime` node at all (a document
        // invalid enough that nothing ran) has no `prime.meta.error` of
        // its own -- cof's own pre-execution stdout JSON is the only
        // place the real reason lives.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("stdout.txt"),
            "{\n  \"ok\": false,\n  \"error\": \"Orchestration validation failed: bad type\",\n  \"warnings\": []\n}\n",
        )
        .unwrap();
        let state: serde_json::Value = serde_json::from_str(
            r#"{"runtime": {"last_run": {"completed_at": "t9", "totals": {"wall_time_s": 0.0}}}}"#,
        )
        .unwrap();
        let mut differ = Differ::new();
        let model = RunModel::new();
        let reason = failure_reason_fallback(dir.path(), &model);
        let line = differ
            .finish(&state, reason.as_deref())
            .expect("a summary line");
        assert_eq!(
            line.text,
            "■ run failed: Orchestration validation failed: bad type  0.0s"
        );
    }
}
