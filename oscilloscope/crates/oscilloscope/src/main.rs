//! `osp` — the CLI binary (DESIGN.md §4, §6.2). Milestone O-1 (issue
//! #424) fills in the run logic and `osp watch`: `--log` plain-text
//! mode is the only front end until O-2's TUI lands, so every run
//! prints this stream today regardless of `effective_log_mode`'s
//! answer.
//!
//! The two forms share one top-level command: `watch` is a real
//! `clap` subcommand, and anything else is captured by
//! `#[command(external_subcommand)]` and reparsed as [`RunArgs`] — so
//! `-e key=value` and the other run flags are only recognized *after*
//! the orchestration positional, matching the usage synopsis in
//! [`ABOUT`] (and `cof run`'s own flags-after-positionals order,
//! DESIGN.md §4.1).

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand, ValueEnum};
use oscilloscope_core::diff::Differ;
use oscilloscope_core::engine::{CofEngine, ElectricityEngine, Engine, EngineError, RunSpec};
use oscilloscope_core::observe::{EventsTailer, LiveStatePoller, POLL_INTERVAL, duration_seconds};
use oscilloscope_core::plan::PlanTree;
use oscilloscope_core::supervise::{SignalWatcher, SupervisedChild, exit_code};

const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (preview)");

const ABOUT: &str = "\
osp (oscilloscope) launches a Circuitry orchestration on cof or electricity \
and shows it running.

  osp <orchestration> [config.json] [-e key=value]... [--engine cof|electricity] [--out-dir DIR] [--log]
  osp watch <dir | state.live.json> [--plan doc.yml]";

#[derive(Parser)]
#[command(name = "osp", version = VERSION, about = ABOUT)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Attach to a run osp did not start.
    Watch(WatchArgs),
    #[command(external_subcommand)]
    Run(Vec<String>),
}

#[derive(Parser, Debug)]
#[command(name = "osp")]
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
}

impl Clock {
    fn new() -> Self {
        Clock {
            start: Instant::now(),
            anchor_ts: None,
        }
    }

    fn elapsed_for(&mut self, ts: Option<&str>) -> f64 {
        match (&self.anchor_ts, ts) {
            (None, Some(t)) => {
                self.anchor_ts = Some(t.to_string());
                0.0
            }
            (Some(anchor), Some(t)) => {
                duration_seconds(anchor, t).unwrap_or_else(|| self.start.elapsed().as_secs_f64())
            }
            _ => self.start.elapsed().as_secs_f64(),
        }
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

fn build_engine(choice: EngineChoice) -> Box<dyn Engine + Send + Sync> {
    match choice {
        EngineChoice::Cof => Box::new(CofEngine::detect("cof")),
        EngineChoice::Electricity => Box::new(ElectricityEngine::new("electricity")),
    }
}

fn do_run(args: RunArgs) -> ExitCode {
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

    let run_dir = args.out_dir.clone().unwrap_or_else(unique_run_dir);
    if let Err(err) = std::fs::create_dir_all(&run_dir) {
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

    let mut signals = SignalWatcher::new().ok();
    let mut signal_count: u32 = 0;
    let mut kill_deadline: Option<Instant> = None;

    let drain = |out: &mut std::io::StdoutLock<'_>,
                 clock: &mut Clock,
                 live_poller: &mut LiveStatePoller,
                 differ: &mut Differ,
                 plan: &PlanTree| {
        if let Some(state) = live_poller.poll() {
            for line in differ.diff(&state, plan) {
                print_line(out, clock, line.ts.as_deref(), &line.text);
            }
        }
    };

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
        drain(&mut out, &mut clock, &mut live_poller, &mut differ, &plan);
        for _event in events_tailer.poll() {
            // Event-driven log lines land once `cof run --events` is on
            // PATH in the field; state-only diffing already covers
            // every case reachable without it (engine.caps().events
            // gates whether osp even asked for the stream).
        }

        if let Ok(Some(status)) = child.try_wait() {
            break status;
        }
        std::thread::sleep(POLL_INTERVAL);
    };

    // Drain what's left after the engine exited: its last stderr lines,
    // and the final live-state write (DESIGN.md §1.1: `run_end`, when
    // present, lands after it).
    for line in stderr_rx.try_iter() {
        print_line(&mut out, &mut clock, None, &format!("engine: {line}"));
    }
    drain(&mut out, &mut clock, &mut live_poller, &mut differ, &plan);

    if args.out_dir.is_none() {
        let _ = writeln!(out, "run directory: {}", run_dir.display());
    }

    ExitCode::from(exit_code(exit_status) as u8)
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
    let mut signals = SignalWatcher::new().ok();

    let final_state = loop {
        if let Some(watcher) = signals.as_mut() {
            if !watcher.pending().is_empty() {
                return ExitCode::from(130);
            }
        }
        if let Some(state) = live_poller.poll() {
            for line in differ.diff(&state, &plan) {
                print_line(&mut out, &mut clock, line.ts.as_deref(), &line.text);
            }
            if oscilloscope_core::model::run_ended(&state) {
                break state;
            }
        }
        for _event in events_tailer.poll() {}
        std::thread::sleep(POLL_INTERVAL);
    };

    if oscilloscope_core::model::run_ok(&final_state) {
        ExitCode::from(0)
    } else {
        ExitCode::from(1)
    }
}

fn run(args: impl IntoIterator<Item = String>) -> ExitCode {
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(err) => {
            // clap's own exit codes: 0 for --help/--version, 2 for a
            // usage error (`clap::error::ErrorKind` maps to `Error::exit_code()`).
            print!("{err}");
            return ExitCode::from(err.exit_code() as u8);
        }
    };
    match cli.command {
        Commands::Watch(args) => do_watch(args),
        Commands::Run(raw) => {
            match RunArgs::try_parse_from(std::iter::once("osp".to_string()).chain(raw)) {
                Ok(args) => do_run(args),
                Err(err) => {
                    print!("{err}");
                    ExitCode::from(err.exit_code() as u8)
                }
            }
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
    fn unknown_flag_before_the_orchestration_is_a_usage_error() {
        assert_eq!(run(args(&["-e", "k=v", "do-thing.yml"])), ExitCode::from(2));
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
    fn effective_log_mode_is_true_when_explicitly_requested() {
        assert!(effective_log_mode(true));
    }
}
