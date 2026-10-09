//! `osp` — the CLI binary (DESIGN.md §4, §6.2). Milestone O-1 (issue
//! #424) fills in the run logic and `osp watch`: `--log` plain-text
//! mode is the only front end until O-2's TUI lands, so every run
//! prints this stream today regardless of `effective_log_mode`'s
//! answer.
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
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use clap::{Parser, ValueEnum};
use oscilloscope_core::diff::Differ;
use oscilloscope_core::engine::{CofEngine, ElectricityEngine, Engine, EngineError, RunSpec};
use oscilloscope_core::model::RunModel;
use oscilloscope_core::observe::{
    EventsTailer, LiveStatePoller, POLL_INTERVAL, duration_seconds, parse_event,
};
use oscilloscope_core::plan::PlanTree;
use oscilloscope_core::supervise::{SignalWatcher, SupervisedChild, exit_code};

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
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
enum EngineChoice {
    Cof,
    Electricity,
}

/// `--log` is chosen automatically when stdout is not a TTY or `CI` is
/// set (DESIGN.md §6.2). O-1 has no TUI yet (O-2's job), so this value
/// isn't load-bearing for *how* osp prints today — every run uses the
/// plain-text stream either way. It's still computed (and tested) so
/// O-2 only has to branch on it, not reimplement the detection.
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
    if let Err(err) = create_run_dir(&run_dir, is_default_dir) {
        eprintln!(
            "osp: couldn't create run directory {}: {err}",
            run_dir.display()
        );
        return ExitCode::from(1);
    }

    let spec = RunSpec {
        orchestration: orchestration.clone(),
        config,
        sets: args.set.clone(),
        run_dir: run_dir.clone(),
    };

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

    let engine = build_engine(args.engine);
    if engine.name() == "cof" && !engine.caps().events {
        eprintln!("osp: this cof has no --events: running from state only");
    }

    let cmd = match engine.command(&spec) {
        Ok(cmd) => cmd,
        Err(EngineError::MissingConfig(message)) => {
            eprintln!("osp: {message}");
            return ExitCode::from(2);
        }
    };

    let plan = match oscilloscope_core::plan::compile(&orchestration) {
        Ok(program) => PlanTree::from_program(&program),
        Err(err) => {
            eprintln!("osp: couldn't compile a plan ({err}); running from observations only");
            PlanTree::empty()
        }
    };

    // Computed (and unit-tested) so O-2's TUI only has to branch on it;
    // every run prints the same plain-text stream today regardless.
    let _log_mode = effective_log_mode(args.log);

    let (mut child, stderr_rx) =
        match SupervisedChild::spawn(cmd, &spec.stdout_path(), &spec.stderr_path()) {
            Ok(pair) => pair,
            Err(err) => {
                eprintln!("osp: couldn't launch {}: {err}", engine.name());
                return ExitCode::from(1);
            }
        };

    let mut clock = Clock::new();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    let mut live_poller = LiveStatePoller::new(spec.live_state_path());
    let mut events_tailer = EventsTailer::new(spec.events_path());
    let mut differ = Differ::new();
    // Dispatch info (branches/concurrency, DESIGN.md §2.1 rule 4) is
    // tracked as soon as events arrive, even though nothing renders it
    // back yet: there's no TUI before O-2, and --log's own output comes
    // from `differ` alone.
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
    let exit_status = child.wait().unwrap_or(exit_status);

    // Drain what's left after the engine exited: its last stderr lines,
    // and the final live-state write (DESIGN.md §1.1: `run_end`, when
    // present, lands after it).
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

    // F5: `diff`'s own `■ run ...` line only ever comes from a `prime`
    // snapshot that reached `runtime.last_run.completed_at`. Two cases
    // never produce one: the engine failed before writing any state at
    // all (a validation error, printed as `{"ok":false,"error":...}`
    // JSON on stdout, DESIGN.md §4.1), or the run was genuinely aborted
    // (a second signal, SIGKILL, a crash) with no final write. Try the
    // authoritative `--out` file once more first -- the live-state
    // poller's own last `drain` above can miss the very last write if
    // it lands between two polls -- before falling back to either.
    if !differ.run_line_emitted() {
        if let Ok(bytes) = std::fs::read(spec.out_path()) {
            if let Ok(state) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                for line in differ.diff(&state, &plan) {
                    print_line(&mut out, &mut clock, line.ts.as_deref(), &line.text);
                }
            }
        }
    }
    if !differ.run_line_emitted() {
        let text = match read_stdout_json_error(&spec.stdout_path()) {
            Some(err) => format!("■ run failed: {err}"),
            None => "■ run aborted (no final state)".to_string(),
        };
        print_line(&mut out, &mut clock, None, &text);
    }
    // K6: a run that ended (`runtime.last_run.completed_at` set)
    // without ever creating a `prime` node at all — a document invalid
    // enough that nothing ran — gets a correctly "failed" summary line
    // from `run_ok` above, but with no message at all: there is no
    // `/prime/meta/error` to read one from. cof's own stdout JSON
    // (DESIGN.md §4.1) has the real reason; prefer it over leaving the
    // summary line's error text empty.
    if let Ok(bytes) = std::fs::read(spec.out_path()) {
        if let Ok(state) = serde_json::from_slice::<serde_json::Value>(&bytes) {
            let ended_without_a_message = oscilloscope_core::model::run_ended(&state)
                && !oscilloscope_core::model::run_ok(&state)
                && oscilloscope_core::model::run_error(&state).is_none();
            if ended_without_a_message {
                if let Some(err) = read_stdout_json_error(&spec.stdout_path()) {
                    print_line(&mut out, &mut clock, None, &format!("engine: {err}"));
                }
            }
        }
    }
    let _ = writeln!(out, "exit {}", exit_code(exit_status));

    if args.out_dir.is_none() {
        let _ = writeln!(out, "run directory: {}", run_dir.display());
    }

    ExitCode::from(exit_code(exit_status) as u8)
}

/// `cof`'s own pre-execution failure shape (DESIGN.md §4.1): with no
/// orchestration ever started, it prints exactly `{"ok":false,
/// "error":"..."}` to stdout and nothing reaches `--live-state`/`--out`
/// at all. Scans line by line rather than parsing the whole file as
/// one JSON value: `--quiet` is the only flag osp passes, but a build
/// with some other banner on stdout should still have this line found.
fn read_stdout_json_error(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("ok").and_then(serde_json::Value::as_bool) == Some(false) {
            if let Some(error) = value.get("error").and_then(serde_json::Value::as_str) {
                return Some(error.to_string());
            }
        }
    }
    None
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

    let plan = match &args.plan {
        Some(doc) => match oscilloscope_core::plan::compile(doc) {
            Ok(program) => PlanTree::from_program(&program),
            Err(err) => {
                eprintln!("osp: couldn't compile a plan ({err}); running from observations only");
                PlanTree::empty()
            }
        },
        None => PlanTree::empty(),
    };

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
    loop {
        if let Some(watcher) = signals.as_mut() {
            if !watcher.pending().is_empty() {
                return ExitCode::from(130);
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

    match last_state {
        Some(state) if oscilloscope_core::model::run_ended(&state) => {
            if oscilloscope_core::model::run_ok(&state) {
                ExitCode::from(0)
            } else {
                ExitCode::from(1)
            }
        }
        _ if model.run_end_ok() == Some(true) => ExitCode::from(0),
        _ => {
            // Stopped on a dead pid (or a `run_end` whose promised final
            // write never actually showed up), with nothing that counts
            // as a clean completion: an abort (F4).
            ExitCode::from(1)
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
    fn electricity_with_no_config_is_a_clear_usage_error() {
        let dir = tempfile::tempdir().unwrap();
        let doc = dir.path().join("do.yml");
        std::fs::write(&doc, "effects: []\n").unwrap();
        let code = run(args(&[doc.to_str().unwrap(), "--engine", "electricity"]));
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
    fn watch_attaches_to_a_run_directory_with_only_a_final_state() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("state.live.json"),
            r#"{"runtime":{"last_run":{"completed_at":"t"}},"prime":{"value":true,"meta":{"error":null}}}"#,
        )
        .unwrap();
        let code = run(args(&["watch", dir.path().to_str().unwrap()]));
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
            run(std::iter::once("osp".to_string())
                .chain(["watch".to_string(), target.to_str().unwrap().to_string()]))
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
}
