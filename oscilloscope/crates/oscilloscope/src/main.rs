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
    // Dispatch info (branches/concurrency, DESIGN.md §2.1 rule 4) is
    // tracked as soon as events arrive, even though nothing renders it
    // back yet: there's no TUI before O-2, and --log's own output comes
    // from `differ` alone.
    let mut model = RunModel::new();

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
        for raw_event in events_tailer.poll() {
            if let Some(event) = parse_event(&raw_event) {
                model.observe_event(&event);
            }
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
    let mut model = RunModel::new();
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
        for raw_event in events_tailer.poll() {
            if let Some(event) = parse_event(&raw_event) {
                model.observe_event(&event);
            }
        }
        std::thread::sleep(POLL_INTERVAL);
    };

    if oscilloscope_core::model::run_ok(&final_state) {
        ExitCode::from(0)
    } else {
        ExitCode::from(1)
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
        // appear before the positionals." `do-thing.yml` here doesn't
        // exist, so this still exits non-zero (a couldn't-launch-cof or
        // compile-failure path) — the point is that `-e`/`--engine`
        // *parse* before the positional rather than erroring as unknown.
        let code = run(args(&["-e", "k=v", "--log", "do-thing.yml"]));
        assert_ne!(
            code,
            ExitCode::from(2),
            "flags before the positional should parse"
        );
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
