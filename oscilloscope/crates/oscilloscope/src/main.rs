//! `osi` — the CLI binary (DESIGN.md §4, §6.2, §6.3). `--log`, or
//! any non-TTY stdout, keeps O-1's plain-text stream unchanged; a TTY
//! gets O-2's ratatui/crossterm TUI (issue #434) instead, in
//! `run_tui`/`run_tui_watch`, `tui.rs` and `keys.rs`.
//!
//! The two forms share no top-level `clap` command: `osi watch ...` is
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

mod highlight;
mod history_io;
mod icon;
mod keys;
mod terminal;
mod theme;
mod tui;

use clap::{Parser, ValueEnum};
use crossterm::event::{self, Event as CEvent, KeyEventKind};
use oscilloscope_core::diff::Differ;
use oscilloscope_core::engine::{
    CofEngine, ElectricityEngine, Engine, EngineCaps, EngineError, RunSpec,
};
use oscilloscope_core::model::{ProcessState, RunModel, RunStatus, run_status};
use oscilloscope_core::observe::{
    EventsTailer, LiveStatePoller, POLL_INTERVAL, TRANSCRIPT_TAIL_BYTES, now_rfc3339, parse_event,
    tail_transcript_lines,
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
osi (oscilloscope) launches a Circuitry orchestration on cof or electricity \
and shows it running.

  osi <orchestration> [config.json] [-e key=value]... [--engine cof|electricity] [--out-dir DIR] [--log]
  osi watch <dir | state.live.json> [--plan doc.yml]";

#[derive(Parser, Debug)]
#[command(name = "osi", version = VERSION, about = ABOUT)]
struct RunArgs {
    /// The orchestration document to run (YAML or JSON).
    orchestration: String,
    /// The engine's own config (JSON), passed through unchanged. With
    /// none, the engine resolves its own default (DESIGN.md §4).
    config: Option<String>,
    /// `-e key=value` inputs, passed through to the engine unchanged.
    #[arg(short = 'e', value_name = "key=value")]
    set: Vec<String>,
    /// Which engine runs the orchestration.
    #[arg(long, value_enum, default_value = "cof")]
    engine: EngineChoice,
    /// Puts the run directory (state, events, stdout/stderr) here
    /// instead of a temporary one -- which osi never removes either
    /// way; it prints where that one ended up once the run ends.
    #[arg(long, value_name = "DIR")]
    out_dir: Option<PathBuf>,
    /// Forces the plain-text stream (DESIGN.md §6.2) even on a TTY.
    #[arg(long)]
    log: bool,
    /// Shows only the header line (issue #626), without the tabs,
    /// rewritten in place about once a second -- for a tmux status
    /// line. Can't be combined with `--log`.
    #[arg(long)]
    glance: bool,
    /// Switches off the update highlights and the scope icon's
    /// trace: both draw a still frame instead (DESIGN.md §6.3).
    #[arg(long)]
    no_animation: bool,
    /// The colour theme (DESIGN.md §6.3). `OSI_THEME` is used when
    /// this isn't given; `terminal` (today's look) otherwise.
    #[arg(long, value_enum)]
    theme: Option<theme::ThemeName>,
    /// Turns run history off (issue #649): nothing is read or written,
    /// and no history directory is created. `OSI_HISTORY=off` does the
    /// same.
    #[arg(long)]
    no_history: bool,
}

#[derive(Parser, Debug)]
#[command(name = "osi watch")]
struct WatchArgs {
    /// A run directory, or a `state.live.json` file directly.
    target: String,
    /// Compiles this document into a plan, the same one a live `osi
    /// <doc>` run of it would have from the start (DESIGN.md §5) --
    /// without it, container detection (an `if`/loop vs. a leaf) can
    /// briefly fall back to a snapshot it hasn't seen yet instead.
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
    /// the TUI's own default otherwise, the same split `osi <doc>`
    /// makes.
    #[arg(long)]
    log: bool,
    /// Shows only the header line (issue #626), without the tabs,
    /// rewritten in place about once a second -- for a tmux status
    /// line. Can't be combined with `--log`.
    #[arg(long)]
    glance: bool,
    /// With `--glance`: prints the current line once and exits 0,
    /// rather than watching -- for a tmux status line that runs its
    /// own command on a timer instead of leaving one process attached.
    #[arg(long)]
    once: bool,
    /// Switches off the update highlights and the scope icon's
    /// trace: both draw a still frame instead (DESIGN.md §6.3).
    #[arg(long)]
    no_animation: bool,
    /// The colour theme (DESIGN.md §6.3). `OSI_THEME` is used when
    /// this isn't given; `terminal` (today's look) otherwise.
    #[arg(long, value_enum)]
    theme: Option<theme::ThemeName>,
    /// Turns run history off (issue #649) -- see `osi --help`'s own
    /// `--no-history`.
    #[arg(long)]
    no_history: bool,
}

/// `--theme`, else `OSI_THEME`, else `terminal` (DESIGN.md §6.3) --
/// shared by both `osi <doc>` and `osi watch`, so the two can never
/// resolve the same environment differently. `osi_theme_env` is the
/// caller's own read of `OSI_THEME` (finding 13: kept pure, like
/// `theme::ColorMode::detect`, rather than reading the process
/// environment itself -- a test can then exercise every branch
/// without mutating process-wide state at all).
fn resolve_theme_choice(
    cli_theme: Option<theme::ThemeName>,
    osi_theme_env: Option<&str>,
) -> Result<theme::ThemeName, String> {
    if let Some(t) = cli_theme {
        return Ok(t);
    }
    match osi_theme_env {
        Some(v) if !v.is_empty() => theme::ThemeName::parse(v),
        _ => Ok(theme::ThemeName::Terminal),
    }
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

/// The real system zone (issue #613: "the binary uses the local
/// zone"), resolved per timestamp (through [`chrono::Local`], which
/// itself resolves the offset for whichever instant it's given) so
/// DST is right for the timestamp being formatted, not just for
/// whenever `osi` happened to start.
struct LocalZone;

impl oscilloscope_core::format::ClockZone for LocalZone {
    fn local(&self, utc: chrono::DateTime<chrono::Utc>) -> chrono::DateTime<chrono::FixedOffset> {
        utc.with_timezone(&chrono::Local).fixed_offset()
    }
}

/// `HH:MM:SS` clock-time prefix (DESIGN.md §6.2, issue #613: "log
/// lines ... start with the line's own local clock time"), from the
/// line's own timestamp when it has one, else osi's own wall clock at
/// print time (a line with no `ts`, like a forwarded-signal notice).
struct Clock {
    zone: Box<dyn oscilloscope_core::format::ClockZone>,
    /// The highest clock time printed so far (K5): a line's own `ts`
    /// can legitimately be *earlier* than one already printed (a
    /// coalesced state write backdating a node's `▶` line to its real
    /// `created_at`, DESIGN.md §2.4's "new node that is already
    /// complete" row, or events and state simply landing a tick apart)
    /// — printing that line's own true clock time anyway would make
    /// the stream's own prefixes visibly run backwards. Clamping the
    /// *displayed* time to this high-water mark leaves sorting (by
    /// the real `ts`) and duration math untouched; only the printed
    /// prefix never regresses.
    max_displayed: Option<chrono::DateTime<chrono::FixedOffset>>,
}

impl Clock {
    fn new() -> Self {
        Clock::new_with_zone(Box::new(LocalZone))
    }

    fn new_with_zone(zone: Box<dyn oscilloscope_core::format::ClockZone>) -> Self {
        Clock {
            zone,
            max_displayed: None,
        }
    }

    fn clock_time_for(&mut self, ts: Option<&str>) -> chrono::DateTime<chrono::FixedOffset> {
        let utc = ts
            .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .unwrap_or_else(chrono::Utc::now);
        let local = self.zone.local(utc);
        let clamped = match self.max_displayed {
            Some(prev) if local < prev => prev,
            _ => local,
        };
        self.max_displayed = Some(clamped);
        clamped
    }

    fn format(dt: chrono::DateTime<chrono::FixedOffset>) -> String {
        dt.format("%H:%M:%S").to_string()
    }
}

fn print_line(out: &mut impl Write, clock: &mut Clock, ts: Option<&str>, text: &str) {
    let dt = clock.clock_time_for(ts);
    let _ = writeln!(out, "{} {text}", Clock::format(dt));
}

/// Prints the `exit <code>` line every path through `do_run` ends
/// with (P2-1) — including a usage-style failure before the engine
/// was ever spawned, which used to print nothing on stdout at all.
fn exit_early(code: u8) -> ExitCode {
    println!("exit {code}");
    ExitCode::from(code)
}

/// The exit code osi should use right now, without ever spawning the
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
    std::env::temp_dir().join(format!("osi-{}-{nanos}", std::process::id()))
}

/// Creates the run directory private (`0700`): it can hold `state.live
/// .json`/`state.json`, which carry prompt and tool output (F2).
///
/// The default directory's name is a predictable `osi-<pid>-<nanos>`
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
/// real, unrequested change to something outside osi's own run
/// directory, and silently chmod'ing *someone else's* directory (one
/// this process doesn't own) can even fail outright with `EPERM`. A
/// pre-existing directory that already was group- or world-accessible
/// instead gets a warning on stderr — loud enough that a caller who
/// reuses one on purpose notices, without osi quietly changing it out
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
                "osi: run directory {} is group- or world-accessible (mode {:o}); \
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

fn build_engine(
    choice: EngineChoice,
    signals: &mut Option<SignalWatcher>,
) -> Box<dyn Engine + Send + Sync> {
    match choice {
        EngineChoice::Cof => Box::new(CofEngine::detect("cof", signals.as_mut())),
        EngineChoice::Electricity => {
            Box::new(ElectricityEngine::detect("electricity", signals.as_mut()))
        }
    }
}

/// The notice `do_run` prints right after `build_engine`'s own probe,
/// when it has one (M4/#433) -- pulled out of `do_run` itself (review
/// finding 6 on #458) so the exact wording for each of `caps`'s own
/// cases is unit-testable without a real engine binary on `PATH`.
fn engine_capability_notice(caps: &EngineCaps, engine_name: &str) -> Option<String> {
    if caps.timed_out {
        Some(format!(
            "osi: {engine_name} --help didn't answer in time; running from state only"
        ))
    } else if !caps.events && !caps.missing {
        Some(format!(
            "osi: this {engine_name} has no --events: running from state only"
        ))
    } else {
        None
    }
}

/// `oscilloscope_core::plan::compile`, mirroring `cof`'s own config
/// discovery (issue #433) when `config` is `None`: `$HOME` and osi's
/// own current directory are read here, at this one real call site,
/// and handed down as plain parameters from here on — discovery
/// itself never touches the process environment directly (see
/// `config_discovery`'s own module doc), so a test exercising it
/// supplies both explicitly instead. Either one being unavailable
/// (`$HOME` unset, the cwd gone) just skips discovery, the same as
/// nothing being found by it. No named return type (and so no direct
/// `electricity`/`electricity-bytecode` dependency of this binary
/// crate) — both branches already return exactly the same type
/// `oscilloscope_core::plan`'s own two functions do, inferred from
/// this macro's own expansion at each call site: `(Program,
/// Option<DiscoveredConfig>)`, so a caller can hand the *same*
/// effective config this compile actually used on to
/// `PlanTree::from_program_with_options` for every `use: path:` child
/// (review finding 3 on #458) instead of the caller's own original,
/// possibly-absent `--config` argument.
///
/// An empty `CIRCUITRY_CONFIG` (`CIRCUITRY_CONFIG=`) is treated the
/// same as unset, matching `cli/config.py::resolve_config`'s own
/// `os.getenv("CIRCUITRY_CONFIG")` truthiness check (review finding 8
/// on #458) — `PathBuf::from("")` would otherwise name a real,
/// non-empty path here (`""`, read as the current directory) and skip
/// discovery silently instead of falling through to it.
macro_rules! compile_plan {
    ($orchestration:expr, $config:expr, $inputs:expr) => {
        match (std::env::var_os("HOME"), std::env::current_dir()) {
            (Some(home), Ok(cwd)) => {
                let trust_env =
                    std::env::var(oscilloscope_core::config_discovery::TRUST_PROJECT_CONFIG_ENV)
                        .ok();
                let env_config = std::env::var_os("CIRCUITRY_CONFIG")
                    .filter(|v| !v.is_empty())
                    .map(PathBuf::from);
                oscilloscope_core::plan::compile_with_discovery(
                    $orchestration,
                    $config,
                    $inputs,
                    Path::new(&home),
                    &cwd,
                    trust_env.as_deref(),
                    env_config.as_deref(),
                )
            }
            _ => {
                oscilloscope_core::plan::compile($orchestration, $config, $inputs).map(|program| {
                    let config: Option<&Path> = $config;
                    let discovered = config
                        .map(|p| oscilloscope_core::plan::DiscoveredConfig::Path(p.to_path_buf()));
                    (program, discovered)
                })
            }
        }
    };
}

/// `do_run`'s own plain supervise loop, shared with `run_tui`'s own
/// TUI-unavailable fallback (review finding 14): forwards every
/// signal osi receives to the engine's process group, escalates to
/// `SIGKILL` 10s after a second one, drains its stderr and
/// observations every tick, and returns once it has exited. Before
/// this was shared, that fallback just blocked on `child.wait()` with
/// no signal forwarding at all — a SIGINT/TERM/HUP to osi did nothing
/// until the engine ended on its own.
#[allow(clippy::too_many_arguments)]
fn supervise_plain(
    out: &mut impl Write,
    clock: &mut Clock,
    child: &mut SupervisedChild,
    stderr_rx: &std::sync::mpsc::Receiver<String>,
    live_poller: &mut LiveStatePoller,
    events_tailer: &mut EventsTailer,
    differ: &mut Differ,
    model: &mut RunModel,
    plan: &PlanTree,
    signals: &mut Option<SignalWatcher>,
) -> std::process::ExitStatus {
    let mut signal_count: u32 = 0;
    let mut kill_deadline: Option<Instant> = None;
    loop {
        if let Some(watcher) = signals.as_mut() {
            for sig in watcher.pending() {
                signal_count += 1;
                child.forward(sig);
                if signal_count == 1 {
                    print_line(out, clock, None, "cancelling, finally running...");
                } else if signal_count == 2 {
                    kill_deadline = Some(Instant::now() + std::time::Duration::from_secs(10));
                }
            }
        }
        if let Some(deadline) = kill_deadline {
            if Instant::now() >= deadline && matches!(child.try_wait(), Ok(None)) {
                print_line(
                    out,
                    clock,
                    None,
                    "engine still running 10s after the second signal; sending SIGKILL",
                );
                child.kill_group();
                kill_deadline = None;
            }
        }

        for line in stderr_rx.try_iter() {
            print_line(out, clock, None, &format!("engine: {line}"));
        }
        drain_observations(out, clock, live_poller, events_tailer, differ, model, plan);

        if let Ok(Some(status)) = child.try_wait() {
            return status;
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// `--glance` rewrites the header line about once a second (issue
/// #626) -- any wider than the real terminal would wrap in a tmux
/// status line, so this reads the controlling terminal's own width
/// when there is one, falling back to a generous default when stdout
/// isn't a TTY at all (a tmux status line's own capture, say).
fn glance_width() -> u16 {
    crossterm::terminal::size().map(|(w, _)| w).unwrap_or(120)
}

/// Builds and writes one `--glance` line (issue #626) -- `header_
/// line`'s own segments, as plain text, with no tabs (`current:
/// None`). An intermediate line is rewritten in place (`\r`, then
/// clears to the end of the line so a shorter new line doesn't leave
/// a stale tail behind); the final line, once the run has ended,
/// stays on screen with its own trailing newline instead -- and is
/// the only line ever written at all when stdout isn't a terminal.
#[allow(clippy::too_many_arguments)]
fn write_glance_line(
    out: &mut impl Write,
    document: &str,
    plan: &PlanTree,
    model: &mut RunModel,
    state: Option<&serde_json::Value>,
    process: ProcessState,
    elapsed_s: f64,
    zone: &dyn oscilloscope_core::format::ClockZone,
    no_animation: bool,
    is_tty: bool,
    final_line: bool,
    engine_alive_override: Option<bool>,
    history: &history_io::HistoryHandle,
) {
    if !is_tty && !final_line {
        // DESIGN.md: "when stdout isn't a terminal, it prints one
        // line when the run ends" -- no intermediate rewrites at all.
        return;
    }
    let now = now_rfc3339();
    let mut rendered = oscilloscope_core::render::build(
        document,
        "",
        plan,
        model,
        state,
        process,
        elapsed_s,
        &now,
        &[],
    );
    history_io::apply_history_eta(history, &mut rendered.header, state, plan, &now);
    // `osi watch` owns no process of its own for `render::build`'s
    // own `engine_alive` (derived from `process`, which `watch_
    // glance` always passes as `Running` while it's still watching)
    // to read -- `♥` has to come from the watched run's own pid in
    // `--events`' `run_start` instead, same as the TUI's watch loop.
    if let Some(alive) = engine_alive_override {
        rendered.header.engine_alive = alive;
    }
    let icon_frame = icon_frame_for(no_animation, Duration::from_secs_f64(elapsed_s));
    let segments = oscilloscope_core::screens::header_line(
        &rendered.header,
        &now,
        zone,
        glance_width(),
        !no_animation,
        icon_frame,
        None,
    );
    let text = oscilloscope_core::screens::header_line_text(&segments);
    if is_tty {
        // Rewritten in place: `\r` returns to the start of the line,
        // then `\x1b[K` clears to its end so a shorter new line
        // doesn't leave a stale tail of the previous, longer one
        // behind.
        if final_line {
            let _ = writeln!(out, "\r{text}\x1b[K");
        } else {
            let _ = write!(out, "\r{text}\x1b[K");
        }
    } else {
        // Not a terminal (a pipe, a file, a tmux `#(...)` capture):
        // one plain line, no `\r`/`\x1b[K` -- there's no previous
        // line on screen to overwrite, and a control code there would
        // just be bytes in whatever's reading it.
        debug_assert!(
            final_line,
            "the only line ever written when stdout isn't a terminal"
        );
        let _ = writeln!(out, "{text}");
    }
    let _ = out.flush();
}

/// How often `--glance` rewrites its own line (DESIGN.md: "about
/// once a second").
const GLANCE_INTERVAL: Duration = Duration::from_secs(1);

/// `osi <doc> --glance` (issue #626): the engine runs exactly as it
/// does under `--log`/the TUI, but the only thing ever written to
/// stdout is the header line, rewritten in place. Signals and exit
/// codes work exactly as `--log`'s own `supervise_plain`/`finish_run`
/// pair already do -- this mirrors their forwarding and kill-deadline
/// logic, without any of their per-line printing.
#[allow(clippy::too_many_arguments)]
fn run_glance(
    mut child: SupervisedChild,
    stderr_rx: std::sync::mpsc::Receiver<String>,
    spec: RunSpec,
    plan: PlanTree,
    run_dir: PathBuf,
    document: String,
    mut signals: Option<SignalWatcher>,
    no_animation: bool,
    history: &history_io::HistoryHandle,
    engine_name: &str,
) -> ExitCode {
    let start = Instant::now();
    let zone = LocalZone;
    let is_tty = std::io::stdout().is_terminal();
    let mut live_poller = LiveStatePoller::new(spec.live_state_path());
    let mut events_tailer = EventsTailer::new(spec.events_path());
    let mut differ = Differ::new();
    let mut model = RunModel::new();
    let mut current_state: Option<serde_json::Value> = None;
    let mut signal_count: u32 = 0;
    let mut kill_deadline: Option<Instant> = None;
    let mut last_write = Instant::now() - GLANCE_INTERVAL;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    let exit_status = loop {
        if let Some(watcher) = signals.as_mut() {
            for sig in watcher.pending() {
                signal_count += 1;
                child.forward(sig);
                if signal_count == 2 {
                    kill_deadline = Some(Instant::now() + std::time::Duration::from_secs(10));
                }
            }
        }
        if let Some(deadline) = kill_deadline {
            if Instant::now() >= deadline && matches!(child.try_wait(), Ok(None)) {
                child.kill_group();
                kill_deadline = None;
            }
        }
        // `--glance` shows only the header line -- the engine's own
        // stderr is still drained (so the pipe never blocks it), just
        // never printed.
        for _ in stderr_rx.try_iter() {}
        let (_, state) = observe_tick(
            &mut live_poller,
            &mut events_tailer,
            &mut differ,
            &mut model,
            &plan,
        );
        if let Some(state) = state {
            current_state = Some(state);
        }

        if let Ok(Some(status)) = child.try_wait() {
            break status;
        }
        if last_write.elapsed() >= GLANCE_INTERVAL {
            write_glance_line(
                &mut out,
                &document,
                &plan,
                &mut model,
                current_state.as_ref(),
                ProcessState::Running,
                start.elapsed().as_secs_f64(),
                &zone,
                no_animation,
                is_tty,
                false,
                None,
                history,
            );
            last_write = Instant::now();
        }
        std::thread::sleep(POLL_INTERVAL);
    };

    let exit_status = child.wait().unwrap_or(exit_status);
    for _ in stderr_rx.try_iter() {}
    let (_, final_state, _) = drain_until_ended(
        &spec.out_path(),
        &mut live_poller,
        &mut events_tailer,
        &mut differ,
        &mut model,
        &plan,
    );
    // The authoritative final state once a run truly ended with one
    // (DESIGN.md §2.1 rule 7) -- `None` both before any write and
    // after a second-signal abort with no final write at all, in
    // which case the last live snapshot stays, same as `finish_run`'s
    // own summary falls back to it.
    if final_state.is_some() {
        current_state = final_state;
    }

    let process = ProcessState::Exited {
        interrupted: signal_count > 0,
        likely_failed: exit_status.code() == Some(1),
        has_failure_reason: failure_reason_fallback(&run_dir, &model).is_some(),
        exited_at: Some(now_rfc3339()),
    };
    history.write(Some(engine_name), current_state.as_ref(), &model, &plan);
    write_glance_line(
        &mut out,
        &document,
        &plan,
        &mut model,
        current_state.as_ref(),
        process,
        start.elapsed().as_secs_f64(),
        &zone,
        no_animation,
        is_tty,
        true,
        None,
        history,
    );
    flush_history_notices(history);

    ExitCode::from(exit_code(exit_status) as u8)
}

/// Every notice [`history_io::HistoryHandle`] has queued rather than
/// printed right away (issue #649) -- called once a loop that reads
/// history on every redraw is done, never from inside one: the TUI's
/// own raw-mode, alternate-screen loop, and `--glance`'s own
/// rewritten-in-place line, would both show a bare `eprintln!` as a
/// stray line over whatever's actually drawn.
fn flush_history_notices(history: &history_io::HistoryHandle) {
    for notice in history.take_notices() {
        let _ = writeln!(std::io::stderr(), "{notice}");
    }
}

/// `osi watch`'s own document name (issue #626): the watched run's
/// own `runtime.last_run.orchestration_path` once a live-state write
/// has it, else whatever the caller was pointed at.
fn watch_document_name(state: Option<&serde_json::Value>, fallback: &str) -> String {
    state
        .and_then(oscilloscope_core::model::orchestration_path)
        .map(str::to_string)
        .unwrap_or_else(|| fallback.to_string())
}

/// `osi watch <dir> --glance` (issue #626): the same stop conditions
/// `supervise_plain_watch` already uses (a completed live-state
/// write, an events `run_end`, or the watched pid going away), but
/// rewriting the header line instead of printing diff lines --
/// `osi watch` owns no engine to forward a signal to, so a local one
/// just ends the watch, same as `q` does in the TUI (detaching, never
/// an abort). `--once` skips the loop entirely: one read, one line,
/// exit 0, for a tmux status line that runs its own command on a
/// timer instead.
fn watch_glance(
    run_dir: PathBuf,
    live_state_path: PathBuf,
    plan: PlanTree,
    fallback_document: String,
    no_animation: bool,
    once: bool,
    history: &history_io::HistoryHandle,
) -> ExitCode {
    let start = Instant::now();
    let zone = LocalZone;
    let is_tty = std::io::stdout().is_terminal();
    let mut live_poller = LiveStatePoller::new(&live_state_path);
    let mut events_tailer = EventsTailer::new(run_dir.join("events.jsonl"));
    let mut differ = Differ::new();
    let mut model = RunModel::new();
    let mut last_state: Option<serde_json::Value> = None;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    if once {
        let (_, state) = observe_tick(
            &mut live_poller,
            &mut events_tailer,
            &mut differ,
            &mut model,
            &plan,
        );
        last_state = state;
        let document = watch_document_name(last_state.as_ref(), &fallback_document);
        let process = if last_state
            .as_ref()
            .is_some_and(oscilloscope_core::model::run_ended)
        {
            ProcessState::Exited {
                interrupted: false,
                likely_failed: false,
                has_failure_reason: false,
                exited_at: None,
            }
        } else {
            ProcessState::Running
        };
        let engine_alive = Some(
            model
                .run_start_pid()
                .is_some_and(oscilloscope_core::supervise::process_alive),
        );
        write_glance_line(
            &mut out,
            &document,
            &plan,
            &mut model,
            last_state.as_ref(),
            process,
            start.elapsed().as_secs_f64(),
            &zone,
            no_animation,
            is_tty,
            true,
            engine_alive,
            history,
        );
        // Issue #649: "`osi watch --glance --once` only reads" --
        // never a write, even when the one state it happened to read
        // already shows the run ended.
        flush_history_notices(history);
        return ExitCode::from(0);
    }

    let mut signals = SignalWatcher::new().ok();
    let mut last_write = Instant::now() - GLANCE_INTERVAL;
    let mut local_signal: Option<ForwardSignal> = None;
    loop {
        if let Some(watcher) = signals.as_mut() {
            if let Some(sig) = watcher.pending().into_iter().next() {
                local_signal = Some(sig);
                break;
            }
        }
        let (_, state) = observe_tick(
            &mut live_poller,
            &mut events_tailer,
            &mut differ,
            &mut model,
            &plan,
        );
        if state.is_some() {
            last_state = state;
        }
        if last_state
            .as_ref()
            .is_some_and(oscilloscope_core::model::run_ended)
        {
            break;
        }
        if model.run_ended_by_events() {
            let (_, state) = observe_tick(
                &mut live_poller,
                &mut events_tailer,
                &mut differ,
                &mut model,
                &plan,
            );
            if state.is_some() {
                last_state = state;
            }
            break;
        }
        if let Some(pid) = model.run_start_pid() {
            if !oscilloscope_core::supervise::process_alive(pid) {
                break;
            }
        }
        if last_write.elapsed() >= GLANCE_INTERVAL {
            let document = watch_document_name(last_state.as_ref(), &fallback_document);
            let engine_alive = Some(
                model
                    .run_start_pid()
                    .is_some_and(oscilloscope_core::supervise::process_alive),
            );
            write_glance_line(
                &mut out,
                &document,
                &plan,
                &mut model,
                last_state.as_ref(),
                ProcessState::Running,
                start.elapsed().as_secs_f64(),
                &zone,
                no_animation,
                is_tty,
                false,
                engine_alive,
                history,
            );
            last_write = Instant::now();
        }
        std::thread::sleep(POLL_INTERVAL);
    }

    let document = watch_document_name(last_state.as_ref(), &fallback_document);
    // A voluntary detach from a run that's still going is not an
    // abort -- shown as still running, exactly as the TUI's own
    // finished screen would leave it, rather than a synthetic
    // "aborted" ending it never had.
    let process = if local_signal.is_some() {
        ProcessState::Running
    } else {
        ProcessState::Exited {
            interrupted: false,
            likely_failed: model.run_end_ok() == Some(false),
            has_failure_reason: failure_reason_fallback(&run_dir, &model).is_some(),
            exited_at: None,
        }
    };
    let engine_alive = Some(
        model
            .run_start_pid()
            .is_some_and(oscilloscope_core::supervise::process_alive),
    );
    // Issue #649: "`osi watch` writes only when the watched run
    // really ended" -- `HistoryHandle::write` itself checks
    // `run_ended`, so a voluntary detach or a dead-pid abort (neither
    // of which ever sets that on `last_state`) writes nothing.
    history.write(model.run_start_engine(), last_state.as_ref(), &model, &plan);
    write_glance_line(
        &mut out,
        &document,
        &plan,
        &mut model,
        last_state.as_ref(),
        process,
        start.elapsed().as_secs_f64(),
        &zone,
        no_animation,
        is_tty,
        true,
        engine_alive,
        history,
    );
    flush_history_notices(history);
    // Signals and exit codes work exactly as `--log`'s own do
    // (DESIGN.md) -- the same rule `finish_watch` uses, not a second
    // copy of it.
    ExitCode::from(watch_exit_code(local_signal, &model, &last_state))
}

fn do_run(args: RunArgs) -> ExitCode {
    // Issue #535: resolved before anything else runs, same spirit as
    // the signal watcher just below -- a bad `OSI_THEME` is a usage
    // error and should exit 2 before this function does anything
    // else, exactly as an invalid `--theme` already does via clap.
    let osi_theme_env = std::env::var("OSI_THEME").ok();
    let theme_name = match resolve_theme_choice(args.theme, osi_theme_env.as_deref()) {
        Ok(name) => name,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    if args.log && args.glance {
        eprintln!("osi: --log and --glance can't be combined");
        return ExitCode::from(2);
    }

    // Registered before *anything* else in this function, not just
    // before spawning the engine (F13): `build_engine`'s own `cof run
    // --help` detection, and compiling the plan, both run a blocking
    // subprocess/compile step before the engine is ever spawned, and
    // each can legitimately take longer than a human's first Ctrl-C
    // takes to arrive (a cold Python interpreter start under load is
    // not rare on a shared, busy machine). A signal landing in either
    // window, with no handler installed yet, used to hit the OS
    // default disposition and kill osi outright — with the engine
    // either not yet started, or started and now unsupervised.
    let mut signals = SignalWatcher::new().ok();

    let orchestration = PathBuf::from(&args.orchestration);
    let config = args.config.as_ref().map(PathBuf::from);
    let history = history_io::HistoryHandle::for_run(
        history_io::resolve_history_dir(args.no_history),
        history_io::resolve_run_document_path(&orchestration),
    );

    if args.engine == EngineChoice::Electricity {
        if let Some(cfg) = &config {
            if oscilloscope_core::engine::looks_swapped(&orchestration, cfg) {
                eprintln!(
                    "osi: '{}' and '{}' look swapped — usage is `osi <orchestration> [config.json]`",
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

    let engine = build_engine(args.engine, &mut signals);
    // Review finding 2 on #458: a Ctrl-C landing during the probe
    // `build_engine` just ran is cancelled promptly (`run_probe`'s own
    // signal check), but is still only *pending*, not yet acted on --
    // checked here, before the probe's own notice below, so a
    // cancelled-not-really-timed-out probe never prints a misleading
    // "didn't answer in time" notice right before exiting anyway. The
    // run directory doesn't exist yet at this point (created below),
    // so there's nothing to clean up either way.
    if let Some(code) = pending_signal_exit_code(&mut signals) {
        return exit_early(code);
    }
    if let Some(notice) = engine_capability_notice(&engine.caps(), engine.name()) {
        eprintln!("{notice}");
    }

    let cmd = match engine.command(&spec) {
        Ok(cmd) => cmd,
        Err(EngineError::MissingConfig(message)) => {
            eprintln!("osi: {message}");
            return exit_early(2);
        }
    };

    // K4: the exact options a real `electricity <config> <doc> -e
    // k=v...` run would check this document against (DESIGN.md §5
    // via `electricity::check_options`), so the plan can't drift from
    // what the engine itself would check. With no config argument,
    // `compile_plan` also mirrors `cof`'s own config discovery (the
    // global/project `circuitry.config.json` layers a bare `cof run`
    // resolves with no `--config`, issue #433), so a `group:` defined
    // only in one of those layers is no longer lost here either, on
    // either engine. A malformed `-e` is never osi's own error (the
    // engine reports that itself once it actually runs) — just the
    // same no-plan fallback as a compile failure.
    let plan = match oscilloscope_core::plan::parse_inputs(&args.set) {
        Ok(inputs) => match compile_plan!(&orchestration, spec.config.as_deref(), &inputs) {
            Ok((program, discovered_config)) => {
                let effective_config = discovered_config.as_ref().map(|d| d.path());
                PlanTree::from_program_with_options(&program, effective_config, &inputs)
            }
            Err(err) => {
                eprintln!("osi: couldn't compile a plan ({err}); running from observations only");
                PlanTree::empty()
            }
        },
        Err(err) => {
            eprintln!("osi: couldn't compile a plan ({err}); running from observations only");
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
            "osi: couldn't create run directory {}: {err}",
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
    // one (F3). osi made this directory (or it's the default, always
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
                eprintln!("osi: couldn't launch {}: {err}", engine.name());
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

    // Issue #626: `--glance` wins over both the TUI and `--log` --
    // it shows only the header line, on any stdout, TTY or not.
    if args.glance {
        return run_glance(
            child,
            stderr_rx,
            spec,
            plan,
            run_dir,
            args.orchestration.clone(),
            signals,
            args.no_animation,
            &history,
            engine.name(),
        );
    }

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
            args.no_animation,
            theme_name,
            &history,
        );
    }

    let mut clock = Clock::new();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    let mut live_poller = LiveStatePoller::new(spec.live_state_path());
    let mut events_tailer = EventsTailer::new(spec.events_path());
    let mut differ = Differ::new();
    let mut model = RunModel::new();

    let exit_status = supervise_plain(
        &mut out,
        &mut clock,
        &mut child,
        &stderr_rx,
        &mut live_poller,
        &mut events_tailer,
        &mut differ,
        &mut model,
        &plan,
        &mut signals,
    );

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
        &history,
        engine.name(),
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
    out: &mut impl Write,
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
    history: &history_io::HistoryHandle,
    engine_name: &str,
) -> u8 {
    let exit_status = child.wait().unwrap_or(exit_status);

    // Drain what's left after the engine exited: its last stderr
    // lines, then the same shared final drain every path that
    // reaches a run's end now takes (#494) -- keeps tailing events
    // and the live state until a pass yields nothing new, then reads
    // the authoritative `--out` file once more, which also covers the
    // two cases that read alone can't: the engine failed before
    // writing any state at all (a validation error, printed as cof's
    // own pre-execution JSON on stdout, DESIGN.md §4.1), or the run
    // was genuinely aborted (a second signal, SIGKILL, a crash) with
    // no final write.
    for line in stderr_rx.try_iter() {
        print_line(out, clock, None, &format!("engine: {line}"));
    }
    let (lines, final_state, _) = drain_until_ended(
        &spec.out_path(),
        live_poller,
        events_tailer,
        differ,
        model,
        plan,
    );
    for line in lines {
        print_line(out, clock, line.ts.as_deref(), &line.text);
    }
    // Issue #649: every ending writes the record, once the final
    // state is in hand -- `HistoryHandle::write` itself is a no-op
    // for a run that never truly ended (an abort with no final
    // write), a dry run, a validate-only run, or one with no
    // `document_hash`.
    history.write(Some(engine_name), final_state.as_ref(), model, plan);

    // M1: the run's own summary line is computed exactly once, here,
    // after every other line this run will ever produce has already
    // been printed -- `Differ::finish` never emits it mid-run, and a
    // per-tick heuristic no longer exists to race state and events
    // (two independently polled files) against each other.
    let summary = final_state
        .as_ref()
        .and_then(|state| differ.finish(state, failure_reason_fallback(run_dir, model).as_deref()))
        .or_else(|| {
            if differ.run_line_emitted() {
                return None;
            }
            Some(oscilloscope_core::diff::LogLine {
                ts: None,
                text: unended_run_summary_text(
                    final_state.as_ref(),
                    // DESIGN.md §2.1 rule 7: the real exit code being
                    // exactly 1 is what cof exits with for a
                    // pre-execution validation failure (§4.1).
                    exit_status.code() == Some(1),
                    run_dir,
                    model,
                ),
                kind: oscilloscope_core::diff::LogKind::RunEnd,
                path: None,
            })
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

/// The scope icon's own frame number (issue #535): one `REDRAW_
/// INTERVAL` tick is one frame while animated, pinned to 0 (the
/// still frame) under `--no-animation` -- a plain function, not
/// `Instant::now()` inline at each call site, so a unit test can
/// pin both branches without a real clock.
fn icon_frame_for(no_animation: bool, elapsed: Duration) -> u64 {
    if no_animation {
        0
    } else {
        (elapsed.as_millis() / REDRAW_INTERVAL.as_millis()) as u64
    }
}

/// How long one loop tick waits for a key before giving the rest of
/// the loop (signals, observation polling, the redraw check) another
/// turn — short enough that navigation feels immediate, long enough
/// not to spin a whole CPU core on a shared machine.
const KEY_POLL_INTERVAL: Duration = Duration::from_millis(30);

/// How many of the TUI's own log lines are kept at most (review
/// finding 13): unlike `--log`'s own stdout stream, the TUI's log
/// pane buffer lives in memory for as long as osi does, and a long-
/// running orchestration never had anything bounding it at all.
const MAX_LOG_LINES: usize = 10_000;

/// Pushes one line onto the TUI's own log buffer, then trims it back
/// to `MAX_LOG_LINES` (finding 13) — the oldest lines go first, same
/// as a terminal's own scrollback eventually would. `next_seq` is a
/// plain running count of every line ever pushed, bumped here and
/// never reset or shifted by the trim above (finding 4) -- the log
/// pane's own highlight key for a line is this sequence number, not
/// its position in `log_lines`, which the trim would otherwise shift
/// out from under an already-marked key.
fn push_log_line(log_lines: &mut Vec<String>, next_seq: &mut u64, line: String) {
    log_lines.push(line);
    *next_seq += 1;
    if log_lines.len() > MAX_LOG_LINES {
        log_lines.drain(0..log_lines.len() - MAX_LOG_LINES);
    }
}

/// The Flow screen's own story buffer (issue #635: "it keeps at most
/// 2,000 lines") -- a separate, bounded buffer from the TUI's own
/// `log_lines` (`MAX_LOG_LINES`), since the story is a small, curated
/// subset (run start/end, decision, report, failure) a much longer
/// run could otherwise scroll a 10,000-line log buffer past.
const MAX_STORY_LINES: usize = 2_000;

/// Pushes one line onto the story buffer when its own `kind` is one
/// of the story's (issue #635), then trims it back to
/// `MAX_STORY_LINES` -- every other line (`Start`/`End`/`Progress`/
/// `Retry`) is silently dropped here, never entering the story at
/// all, though it still went into `--log`/`log_lines` same as always.
fn push_story_line(
    story: &mut Vec<oscilloscope_core::diff::LogLine>,
    line: oscilloscope_core::diff::LogLine,
) {
    use oscilloscope_core::diff::LogKind;
    if !matches!(
        line.kind,
        LogKind::RunStart
            | LogKind::RunEnd
            | LogKind::Decision
            | LogKind::Report
            | LogKind::Failure
    ) {
        return;
    }
    story.push(line);
    if story.len() > MAX_STORY_LINES {
        story.drain(0..story.len() - MAX_STORY_LINES);
    }
}

/// The run-end line, as soon as the TUI itself can show it (issue
/// #635: the Flow screen's story has "run end with its totals") --
/// `finish` is called on a *clone* of `differ`, never the real one,
/// so this never trips its one-shot `run_line_emitted` latch ahead of
/// `--log`'s own authoritative call in `finish_run`/`finish_watch`,
/// after the TUI has already closed. Cheap to call on every poll
/// that just saw the engine exit: `finish` itself is a no-op once
/// the state doesn't yet read as ended.
#[allow(clippy::too_many_arguments)]
fn push_run_end_story_line(
    differ: &Differ,
    state: Option<&serde_json::Value>,
    run_dir: &Path,
    model: &RunModel,
    clock: &mut Clock,
    log_lines: &mut Vec<String>,
    next_log_seq: &mut u64,
    story: &mut Vec<oscilloscope_core::diff::LogLine>,
) {
    let Some(state) = state else {
        return;
    };
    let Some(line) = differ
        .clone()
        .finish(state, failure_reason_fallback(run_dir, model).as_deref())
    else {
        return;
    };
    push_log_line(
        log_lines,
        next_log_seq,
        format_log_line(clock, line.ts.as_deref(), &line.text),
    );
    push_story_line(story, line);
}

/// Update highlights (DESIGN.md §6.3, issue #535): computes what
/// changed between the previous render state and this one
/// (`oscilloscope_core::changes::diff`, front-end independent) and
/// marks each changed key's fade as starting now. The first
/// observation (`prev` still `None`) always reports no changes, so
/// this never needs a separate "is this the first redraw" check of
/// its own. `--no-animation` skips both the diff and the mark
/// entirely -- `prev` is still kept up to date either way, so turning
/// animation on mid-run (there's no key for that today, but a future
/// one would) picks up from a consistent baseline rather than a stale
/// one.
fn mark_highlights(
    highlights: &mut highlight::Tracker,
    prev: &mut Option<oscilloscope_core::render::RenderState>,
    new: &oscilloscope_core::render::RenderState,
    no_animation: bool,
) {
    if !no_animation {
        let now = Instant::now();
        for key in oscilloscope_core::changes::diff(prev.as_ref(), new) {
            highlights.mark(&key, now);
        }
    }
    *prev = Some(new.clone());
}

/// Marks `log:<seq>` for every line pushed since the last rebuild
/// (finding 4): `next_seq` is `push_log_line`'s own running count of
/// every line ever pushed, immune to `MAX_LOG_LINES`' own trim, so a
/// line's key stays stable even once the trim has dropped it from
/// `log_lines` itself. `prev_total` is the same count as of the
/// previous rebuild -- everything from there up to now is new.
///
/// `baseline_exists` is the same "does `prev_render_state` already
/// hold a real build" condition `mark_highlights` tests via its own
/// `prev` (issue #535's log-pane half of finding 3): on the loop's
/// first real rebuild there is no baseline yet, so every line pushed
/// by the catch-up drain that ran before it -- replaying what an
/// already-progressed or already-finished run did before `osi
/// watch`/`osi <doc>` ever attached -- is caught up here silently,
/// same as the render-state diff is. `prev_total` still advances
/// either way, so nothing from before this call is ever marked
/// retroactively once a real baseline does exist.
fn mark_new_log_lines(
    highlights: &mut highlight::Tracker,
    prev_total: &mut u64,
    next_seq: u64,
    no_animation: bool,
    baseline_exists: bool,
) {
    if !no_animation && baseline_exists {
        let now = Instant::now();
        for seq in *prev_total..next_seq {
            highlights.mark(&format!("log:{seq}"), now);
        }
    }
    *prev_total = next_seq;
}

/// Marks `details:sent`/`details:received` when the *currently*
/// selected path's own blocks changed since the last rebuild (finding
/// 5) -- `prev` only ever compares against the same path: switching
/// the selection to a different row, or to none, never counts as a
/// change on its own, it just restarts the comparison from that new
/// path's own first observation.
fn mark_details_highlights(
    highlights: &mut highlight::Tracker,
    prev: &mut Option<(
        String,
        Option<oscilloscope_core::render::TokenBlock>,
        Option<oscilloscope_core::render::TokenBlock>,
    )>,
    selected_path: Option<&str>,
    plan: &PlanTree,
    model: &oscilloscope_core::model::RunModel,
    state: Option<&serde_json::Value>,
    no_animation: bool,
) {
    let Some(path) = selected_path else {
        *prev = None;
        return;
    };
    let details = oscilloscope_core::render::details_for(path, plan, model, state);
    if !no_animation {
        if let Some((prev_path, prev_sent, prev_received)) = prev.as_ref() {
            if prev_path == path {
                let now = Instant::now();
                if *prev_sent != details.sent {
                    highlights.mark("details:sent", now);
                }
                if *prev_received != details.received {
                    highlights.mark("details:received", now);
                }
            }
        }
    }
    *prev = Some((path.to_string(), details.sent, details.received));
}

/// Sends one cancelling signal to the engine's process group and
/// applies the same escalation `do_run`'s plain loop applies to a real
/// forwarded signal (DESIGN.md §4.1) — shared by both, since `c`/a
/// confirmed `q` (§6.3: "cancel, with confirm (= Ctrl-C)") must behave
/// exactly like Ctrl-C itself, never a separate path that could drift
/// from it. `sig` is `Int` for a key (Ctrl-C always means SIGINT) but
/// the real signal osi itself received for an external one (K2): the
/// plain loop above already forwards *that* signal unchanged, and the
/// TUI must too, rather than silently turning every SIGTERM/SIGHUP
/// into a SIGINT and leaving osi exiting 130 for either.
fn cancel_once(
    child: &SupervisedChild,
    sig: ForwardSignal,
    signal_count: &mut u32,
    kill_deadline: &mut Option<Instant>,
    log_lines: &mut Vec<String>,
    next_log_seq: &mut u64,
    clock: &mut Clock,
) {
    *signal_count += 1;
    child.forward(sig);
    if *signal_count == 1 {
        push_log_line(
            log_lines,
            next_log_seq,
            format_log_line(clock, None, "cancelling, finally running..."),
        );
    } else if *signal_count == 2 {
        *kill_deadline = Some(Instant::now() + Duration::from_secs(10));
    }
}

/// A pty whose master end is gone,
/// but which was never osi's own *controlling* terminal (so no
/// SIGHUP ever arrives for it), keeps answering `poll`/`read` with an
/// immediate error and `draw` with a failed write, forever -- with no
/// blocking read left to cap the loop's own pace, that span a whole
/// CPU core for as long as the engine itself kept running, ignoring
/// every further signal in the process (it never actually stopped
/// reaching the top of the loop; it just had nothing left to *show*
/// for any of it). Treated exactly like a real SIGHUP
/// (`cancel_once`'s own `Hup` arm): the engine is cancelled if osi
/// started it and hasn't already exited, and the caller leaves the
/// TUI loop at once -- never waiting around for the engine to exit on
/// its own the way a *real* signal's "finished screen" rule does,
/// since there is no terminal left to show one on, and continuing to
/// poll/read/draw against the same dead one is exactly what spun
/// before.
fn cancel_for_lost_terminal(
    child: &SupervisedChild,
    exit_status: Option<std::process::ExitStatus>,
    signal_count: &mut u32,
    kill_deadline: &mut Option<Instant>,
    log_lines: &mut Vec<String>,
    next_log_seq: &mut u64,
    clock: &mut Clock,
) {
    if exit_status.is_none() {
        cancel_once(
            child,
            ForwardSignal::Hup,
            signal_count,
            kill_deadline,
            log_lines,
            next_log_seq,
            clock,
        );
    }
}

/// A follow-up finding: a pty slave whose master
/// has closed, but which was never osi's own *controlling* terminal
/// (no `setsid`-then-`TIOCSCTTY` acquisition, as a driver using
/// `start_new_session=True` alone never gives it one) raises no
/// SIGHUP at all -- yet `event::poll`/`event::read` (checked above)
/// never error for it either. The reason lives in crossterm's own
/// event source (`event::source::unix::tty::try_read`, crossterm
/// 0.29): it `poll(2)`s the tty fd checking only `revents & POLLIN`,
/// never `POLLHUP`/`POLLERR`/`POLLNVAL` -- and a hung-up pty slave
/// sets `POLLIN` right alongside `POLLHUP` (EOF counts as
/// "readable"), so crossterm keeps calling a `read()` that returns 0
/// bytes in a tight inner loop until its own timeout budget runs out,
/// then returns cleanly with nothing to show for it. Every
/// `KEY_POLL_INTERVAL` tick spends its *entire* budget spinning on
/// that 0-byte read instead of actually blocking for up to it, which
/// is the real source of the CPU this bug burns -- `event::poll`
/// itself never seeing an error is just a symptom of that inner loop
/// having already given up by the time it returns.
///
/// A direct, non-blocking `poll(2)` of our own on the same fd, before
/// ever calling into crossterm at all, sees the `POLLHUP` crossterm's
/// own check throws away -- `fd: libc::STDIN_FILENO` matches
/// crossterm's own `tty_fd()` exactly: it uses stdin directly
/// whenever stdin is itself a tty (`isatty(STDIN_FILENO)`, never a
/// separate `/dev/tty` open), which is always true for an `osi`
/// spawned the way every caller here does, with its own stdin
/// already pointed at the pty slave.
fn stdin_hung_up() -> bool {
    let mut fds = [libc::pollfd {
        fd: libc::STDIN_FILENO,
        events: libc::POLLIN,
        revents: 0,
    }];
    // `poll` itself erroring on fd 0 (EBADF, most plausibly) means
    // there is no usable terminal fd left either, same conclusion as
    // a hang-up bit actually being set.
    if unsafe { libc::poll(fds.as_mut_ptr(), 1, 0) } < 0 {
        return true;
    }
    fds[0].revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
}

/// A third finding, from a real `sample` of a
/// still-spinning `osi`: `stdin_hung_up`'s own pre-check, run between
/// crossterm calls, never gets another turn once the main thread is
/// actually *inside* `event::poll`/`event::read` when the hang-up
/// happens -- the overwhelmingly common case, since that call blocks
/// for up to `KEY_POLL_INTERVAL` out of every tick. Crossterm 0.29's
/// own mio-based event source (`event::source::unix::mio::try_read`)
/// has an inner `read()` loop with no `Ok(0)` (EOF) arm at all: once
/// a hung-up pty slave starts returning zero bytes, every attempt
/// takes the same `Ok(read_count)` branch forever (`read_count == 0`
/// skips the parser, finds nothing new, and the `loop` just tries
/// `read()` again) -- it never reaches the outer poll-timeout check
/// again, so `try_read`, and therefore `event::poll`, never returns
/// at all once this state is hit. No pre-check on the *same* thread
/// can ever get a chance to run again either.
///
/// The only fix is not letting the main thread sit inside crossterm
/// in the first place: this spawns crossterm's own blocking
/// `event::read()` loop onto its own thread, forwarding whatever it
/// eventually produces (or never does, if it gets stuck exactly as
/// above) over a channel the main loop only ever waits on with its
/// own bounded `recv_timeout`, checking `stdin_hung_up()` itself on
/// every one of *its own* ticks regardless of whether the reader
/// thread ever answers again. A reader thread stuck forever inside
/// crossterm after a hang-up is harmless: nothing ever joins it, and
/// every caller of this function ends the whole process outright
/// (`std::process::exit`, after its own normal cleanup) the moment
/// its own main loop decides to leave, taking the stuck thread down
/// with it without needing any cooperation from it at all.
fn spawn_event_reader() -> std::sync::mpsc::Receiver<std::io::Result<CEvent>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        loop {
            let event = event::read();
            let stop = event.is_err();
            if tx.send(event).is_err() || stop {
                break;
            }
        }
    });
    rx
}

/// Which row's own details the details pane shows next redraw
/// (#610): the state pane's own selection, when it was the one that
/// last actually changed (`App::details_source`, joined back onto
/// `prime.`, since `StateRow::path` is relative to it) -- the
/// orchestration tree's otherwise, exactly as before #610. The two
/// selections are deliberately independent
/// (`keys::tests::the_state_panes_own_selection_never_moves_the_
/// orchestration_selection`); `details_source`, not `app.pane`, is
/// what lets the details pane keep following whichever one the user
/// is actually looking at once they tab onto `Pane::Details` itself
/// to scroll it -- including a named report's own row, which never
/// has an orchestration-tree row of its own at all to select instead.
/// Screen 0's own model (issue #626), built only while it's actually
/// on display -- the performance rule ("models for screens not on
/// display aren't built"). `flatten_state` runs again here rather
/// than threading `RenderState`'s own internal `flat` map out of
/// `render::build` just for this -- it's the same O(state size) walk
/// `build` itself already does on every rebuild, and this only adds
/// a second one while the overview is the screen being shown.
fn build_overview_if_active(
    app: &App,
    render_state: &oscilloscope_core::render::RenderState,
    plan: &oscilloscope_core::plan::PlanTree,
    model: &oscilloscope_core::model::RunModel,
    state: Option<&serde_json::Value>,
    log_lines: &[String],
    now: &str,
) -> Option<oscilloscope_core::screens::Overview> {
    if app.screen != oscilloscope_core::screens::Screen::Overview {
        return None;
    }
    let empty = serde_json::Value::Null;
    let flat = oscilloscope_core::model::flatten_state(state.unwrap_or(&empty));
    Some(oscilloscope_core::screens::build_overview(
        &render_state.rows,
        &render_state.unfolded_rows,
        &render_state.header,
        &flat,
        plan,
        model,
        state,
        &render_state.token_bar,
        log_lines,
        app.selected_path.as_deref(),
        now,
    ))
}

/// Screen 3's own model (issue #632), built only while it's actually
/// on display -- the same performance rule `build_overview_if_active`
/// follows, and rebuilt at the same points that one is (a rebuild, or
/// a key that could have changed the screen, the measure or the
/// zoom). `statuses` comes straight from `render_state.unfolded_rows`
/// rather than a second `RunModel::observe` call: every path the
/// by-store tree could ever ask about already has a row (and so a
/// status) there, since `render::build_rows` walks the identical
/// path universe.
fn build_cost_screen_if_active(
    app: &App,
    render_state: &oscilloscope_core::render::RenderState,
    state: Option<&serde_json::Value>,
    now: &str,
) -> Option<oscilloscope_core::screens::CostScreen> {
    if app.screen != oscilloscope_core::screens::Screen::Cost {
        return None;
    }
    let empty = serde_json::Value::Null;
    let flat = oscilloscope_core::model::flatten_state(state.unwrap_or(&empty));
    let statuses: std::collections::BTreeMap<String, oscilloscope_core::model::RowStatus> =
        render_state
            .unfolded_rows
            .iter()
            .map(|r| (r.path.clone(), r.status.clone()))
            .collect();
    let kinds: std::collections::BTreeMap<String, oscilloscope_core::render::RowKind> =
        render_state
            .unfolded_rows
            .iter()
            .map(|r| (r.path.clone(), r.kind))
            .collect();
    Some(oscilloscope_core::screens::build_cost_screen(
        &render_state.rows,
        &render_state.header,
        &flat,
        &statuses,
        &kinds,
        state,
        app.cost_measure,
        now,
    ))
}

/// Screen 2's own model (issue #635), built only while it's actually
/// on display -- the same performance rule `build_overview_if_active`/
/// `build_cost_screen_if_active` already follow.
#[allow(clippy::too_many_arguments)]
fn build_flow_screen_if_active(
    app: &App,
    render_state: &oscilloscope_core::render::RenderState,
    plan: &oscilloscope_core::plan::PlanTree,
    model: &oscilloscope_core::model::RunModel,
    state: Option<&serde_json::Value>,
    story: &[oscilloscope_core::diff::LogLine],
    now: &str,
) -> Option<oscilloscope_core::screens::FlowScreen> {
    if app.screen != oscilloscope_core::screens::Screen::Flow {
        return None;
    }
    let empty = serde_json::Value::Null;
    let flat = oscilloscope_core::model::flatten_state(state.unwrap_or(&empty));
    let selected_path = details_selection_path(app);
    Some(oscilloscope_core::screens::build_flow_screen(
        &render_state.rows,
        &render_state.unfolded_rows,
        &flat,
        plan,
        model,
        state,
        story,
        selected_path.as_deref(),
        &LocalZone,
        now,
    ))
}

/// Screen 1's own model (issue #650), built only while it's actually
/// on display -- the same performance rule the other three screens'
/// own `_if_active` builders follow.
#[allow(clippy::too_many_arguments)]
fn build_time_screen_if_active(
    app: &App,
    render_state: &oscilloscope_core::render::RenderState,
    plan: &oscilloscope_core::plan::PlanTree,
    model: &oscilloscope_core::model::RunModel,
    state: Option<&serde_json::Value>,
    story: &[oscilloscope_core::diff::LogLine],
    now: &str,
) -> Option<oscilloscope_core::screens::TimeScreen> {
    if app.screen != oscilloscope_core::screens::Screen::Time {
        return None;
    }
    let empty = serde_json::Value::Null;
    let flat = oscilloscope_core::model::flatten_state(state.unwrap_or(&empty));
    let selected_path = details_selection_path(app);
    Some(oscilloscope_core::screens::build_time_screen(
        &render_state.rows,
        &flat,
        plan,
        model,
        &render_state.header,
        state,
        story,
        selected_path.as_deref(),
        &app.time_expanded_loops,
        now,
    ))
}

/// Issue #637 (extended by #650 to screen 1): the transcript path for
/// whichever step is actually on screen right now -- the overview's
/// own focused step (running, last-finished, or an ended run's own
/// stopped agent) on screen 0, Flow's and Time's own selected step on
/// screens 2 and 1 -- `None` on every other screen. Tails a finished
/// agent's own `meta.raw.transcript` too, not just a running one's
/// `meta.live.transcript` -- the Time screen's zoom pane (issue #650)
/// is the first thing that ever shows a finished agent's own bands,
/// which need the transcript the same way a running one's do.
fn focused_agent_transcript_path(
    app: &App,
    overview: Option<&oscilloscope_core::screens::Overview>,
    flow_screen: Option<&oscilloscope_core::screens::FlowScreen>,
    time_screen: Option<&oscilloscope_core::screens::TimeScreen>,
) -> Option<String> {
    use oscilloscope_core::screens::{FocusDetail, NowFocus, Screen};
    let step = match app.screen {
        Screen::Overview => overview.and_then(|o| match &o.now_pane.focus {
            NowFocus::Running(step, _) | NowFocus::LastFinished(step) => Some(step),
            NowFocus::Ended(summary) => summary.stopped_agent.as_ref(),
            _ => None,
        }),
        Screen::Flow => flow_screen.and_then(|f| f.selected.as_ref()),
        Screen::Time => time_screen.and_then(|t| t.selected.as_ref()),
        _ => None,
    }?;
    match &step.detail {
        FocusDetail::Agent {
            live: Some(live), ..
        } => live.transcript.clone(),
        // Issue #650: once the session has finished, `meta.live` is
        // gone, but `meta.raw.transcript` is the same file -- the
        // zoom pane's own bands for a finished agent read it the
        // same way a running one's `meta.live.transcript` already
        // does.
        FocusDetail::Agent {
            live: None,
            raw_transcript: Some(path),
            ..
        } => Some(path.clone()),
        _ => None,
    }
}

/// A tail's own result, kept across calls so two tail calls in the
/// same tick (the key handler's own same-frame rebuild, immediately
/// followed by the top-of-loop rebuild `needs_rebuild` triggers for
/// it) read the file once, not twice, whenever nothing about it has
/// actually changed between them -- `len` alone is enough to tell:
/// growing is the only way a transcript changes while it's tailed,
/// bar the one file-replaced-mid-run case DESIGN.md already asks
/// osi to tolerate as stale for up to one more poll, same as any
/// other observation here.
#[derive(Default)]
struct TranscriptTailCache {
    path: Option<String>,
    len: Option<u64>,
    view: Option<oscilloscope_core::transcript::TranscriptView>,
}

/// Tails the one step's transcript this frame actually needs
/// (`focused_agent_transcript_path`), bounded to the last 64 KiB
/// (DESIGN.md, issue #637) -- switching focus to a different step
/// (or off a screen that shows one at all) costs nothing more than
/// this call simply returning `None` next time; no file is ever
/// opened for a step that isn't the one on screen.
fn tail_focused_transcript(
    app: &App,
    overview: Option<&oscilloscope_core::screens::Overview>,
    flow_screen: Option<&oscilloscope_core::screens::FlowScreen>,
    time_screen: Option<&oscilloscope_core::screens::TimeScreen>,
    cache: &mut TranscriptTailCache,
) -> Option<oscilloscope_core::transcript::TranscriptView> {
    let path = focused_agent_transcript_path(app, overview, flow_screen, time_screen)?;
    let len = std::fs::metadata(&path).ok().map(|m| m.len());
    if cache.path.as_deref() == Some(path.as_str()) && cache.len == len {
        return cache.view.clone();
    }
    let lines = tail_transcript_lines(Path::new(&path), TRANSCRIPT_TAIL_BYTES);
    let truncated = oscilloscope_core::observe::transcript_tail_truncated(
        Path::new(&path),
        TRANSCRIPT_TAIL_BYTES,
    );
    let view = oscilloscope_core::transcript::build_transcript_view(&lines, truncated);
    cache.path = Some(path);
    cache.len = len;
    cache.view = Some(view.clone());
    Some(view)
}

fn details_selection_path(app: &App) -> Option<String> {
    if app.details_source == keys::Pane::State {
        if let Some(p) = &app.state_selected_path {
            return Some(format!("prime.{p}"));
        }
    }
    app.selected_path.clone()
}

/// `print_line`'s own formatting, into the TUI's own log buffer
/// instead of straight to stdout (DESIGN.md §6.3's log pane: "the
/// same lines as `--log`").
fn format_log_line(clock: &mut Clock, ts: Option<&str>, text: &str) -> String {
    let dt = clock.clock_time_for(ts);
    format!("{} {text}", Clock::format(dt))
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
/// else dirty, same as `--log`'s own clock-time prefix.
const RENDER_REBUILD_INTERVAL: Duration = Duration::from_secs(1);

/// `osi <doc>`'s own TUI loop (DESIGN.md §6.3): supervises the engine
/// exactly like `do_run`'s plain loop (the same signal forwarding,
/// kill-deadline escalation, and observation polling), but renders a
/// `RenderState` and reads keys instead of printing a plain-text
/// stream.
///
/// Review finding K1: the engine exiting does not, on its own, end
/// this loop — only three things do, and only once it has (DESIGN.md
/// §6.3's final-state screen): a confirmed `q` (`App::
/// quit_when_finished`), a Ctrl-C cancel, or any external INT/TERM/
/// HUP osi itself received (`leave_once_exited`, set by all three).
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
    no_animation: bool,
    mut theme_name: theme::ThemeName,
    history: &history_io::HistoryHandle,
) -> ExitCode {
    let mut live_poller = LiveStatePoller::new(spec.live_state_path());
    let mut events_tailer = EventsTailer::new(spec.events_path());
    let mut differ = Differ::new();
    let mut model = RunModel::new();
    let mut clock = Clock::new();
    let mut log_lines: Vec<String> = Vec::new();
    // Finding 4: a plain running count of every line ever pushed,
    // never reset or shifted by `MAX_LOG_LINES`' own trim -- a log
    // line's own highlight key is this sequence number, never its
    // position in `log_lines` itself.
    let mut next_log_seq: u64 = 0;
    // The Flow screen's own story (issue #635), bounded by
    // `MAX_STORY_LINES` -- a separate buffer from `log_lines`, kept
    // in `diff::LogLine`'s own raw-timestamp shape so the story can
    // merge in an inferred decision at the right clock time.
    let mut story: Vec<oscilloscope_core::diff::LogLine> = Vec::new();
    let mut current_state: Option<serde_json::Value> = None;
    let start = Instant::now();

    let mut app = App::new(false);
    let mut cost_layout = tui::CostLayoutCache::default();
    let mut signal_count: u32 = 0;
    let mut kill_deadline: Option<Instant> = None;

    // Issue #535: the colour mode is detected once at startup, never
    // per redraw, and is shared by every theme choice for this run --
    // `t` only ever changes which theme is resolved against it, never
    // the mode itself.
    let color_mode = theme::ColorMode::from_env();
    let mut resolved_theme = theme::resolve(theme_name, color_mode);
    if let Some(note) = resolved_theme.downgrade_note.take() {
        push_log_line(
            &mut log_lines,
            &mut next_log_seq,
            format_log_line(&mut clock, None, &note),
        );
    }
    let mut highlights = highlight::Tracker::new();
    let mut prev_render_state: Option<oscilloscope_core::render::RenderState> = None;
    // Finding 4/5: both start out already caught up with whatever
    // the startup lines above just did, so the loop's own first real
    // rebuild is the one that establishes the first real baseline --
    // nothing from before the loop started ever counts as "new".
    let mut prev_log_total: u64 = next_log_seq;
    let mut prev_details_highlight: Option<(
        String,
        Option<oscilloscope_core::render::TokenBlock>,
        Option<oscilloscope_core::render::TokenBlock>,
    )> = None;
    let mut theme_banner_until: Option<Instant> = None;

    let guard_and_terminal = TerminalGuard::enter()
        .and_then(|g| Terminal::new(CrosstermBackend::new(std::io::stdout())).map(|t| (g, t)));
    let (_guard, mut terminal) = match guard_and_terminal {
        Ok(pair) => pair,
        Err(err) => {
            // DESIGN.md's own TTY check (`effective_log_mode`) said
            // this should be a real terminal; if it still isn't one
            // underneath (an exotic CI pty, a stdout swapped out from
            // under osi), fall back to the plain supervise loop
            // (finding 14) rather than a broken half-raw-mode session
            // — a bare `child.wait()` here left SIGINT/TERM/HUP to osi
            // doing nothing at all until the engine ended on its own.
            eprintln!("osi: couldn't start the TUI ({err}); falling back to the plain stream");
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            let exit_status = supervise_plain(
                &mut out,
                &mut clock,
                &mut child,
                &stderr_rx,
                &mut live_poller,
                &mut events_tailer,
                &mut differ,
                &mut model,
                &plan,
                &mut signals,
            );
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
                history,
                &engine_name,
            );
            return ExitCode::from(code);
        }
    };

    // Design: crossterm's own blocking
    // `event::read()` runs on its own thread from here on, never
    // this one -- see `spawn_event_reader`'s own doc comment.
    let event_rx = spawn_event_reader();

    let mut last_draw = Instant::now() - REDRAW_INTERVAL;
    let mut last_rebuild = Instant::now() - RENDER_REBUILD_INTERVAL;
    // Finding 1: a *rebuild* (`render::build` plus `mark_highlights`'
    // own `changes::diff`) and a *draw* are no longer the same flag --
    // `needs_rebuild` is only ever set by something that can actually
    // change what gets built (an observation, a key, a resize, the 1s
    // timer); the redraw rule's own animation/fade tick below sets
    // only `needs_draw`, so a live run with nothing else happening no
    // longer rebuilds on every ~30ms key-poll tick the way a single
    // `dirty` flag did (K4's own regression -- about 20 rebuilds a
    // second instead of about 1, since `needs_rebuild` used to stay
    // true, re-triggering the rebuild block, for every tick between
    // two actual draws, not just reset once a real draw happened).
    let mut needs_rebuild = true;
    let mut needs_draw = true;
    let mut exit_status: Option<std::process::ExitStatus> = None;
    // The instant this loop itself saw the engine process exit --
    // captured once, never resampled, so a run that ends with no
    // other known instant (DESIGN.md's "one clock") freezes its own
    // elapsed time here instead of ticking on to `now` forever.
    let mut exit_status_ts: Option<String> = None;
    // K1: set once a confirmed `q`, a Ctrl-C cancel, or an external
    // signal has happened — the only three things that make this loop
    // leave the instant the engine exits, rather than staying open on
    // its final state until the user explicitly asks to leave.
    let mut leave_once_exited = false;
    let mut now_str = now_rfc3339();
    let mut render_state = oscilloscope_core::render::build_folded(
        &document,
        &engine_name,
        &plan,
        &mut model,
        current_state.as_ref(),
        ProcessState::Running,
        start.elapsed().as_secs_f64(),
        &now_str,
        &[],
        app.fold_enabled,
        &app.expanded_quick_steps,
    );
    history_io::apply_history_eta(
        history,
        &mut render_state.header,
        current_state.as_ref(),
        &plan,
        &now_str,
    );
    let mut overview = build_overview_if_active(
        &app,
        &render_state,
        &plan,
        &model,
        current_state.as_ref(),
        &log_lines,
        &now_str,
    );
    let mut cost_screen =
        build_cost_screen_if_active(&app, &render_state, current_state.as_ref(), &now_str);
    let mut flow_screen = build_flow_screen_if_active(
        &app,
        &render_state,
        &plan,
        &model,
        current_state.as_ref(),
        &story,
        &now_str,
    );
    let mut time_screen = build_time_screen_if_active(
        &app,
        &render_state,
        &plan,
        &model,
        current_state.as_ref(),
        &story,
        &now_str,
    );
    let mut transcript_cache = TranscriptTailCache::default();
    let mut transcript_view = tail_focused_transcript(
        &app,
        overview.as_ref(),
        flow_screen.as_ref(),
        time_screen.as_ref(),
        &mut transcript_cache,
    );
    // Finding 3: no `mark_highlights` call here -- `prev_render_state`
    // stays `None` through this placeholder build (always built with
    // `current_state: None`, before the loop has polled anything),
    // so the loop's own first real rebuild below is genuinely the
    // first observation (`changes::diff(None, _)`, which reports
    // nothing by construction), not a diff against this empty one.
    if app.selected_path.is_none() {
        app.selected_path = render_state.rows.first().map(|r| r.path.clone());
    }

    // Set when the terminal itself went away (see `stdin_hung_up`):
    // ratatui's own `Terminal::drop` must not run then (below).
    let mut lost_terminal = false;
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
                        &mut next_log_seq,
                        &mut clock,
                    );
                }
                leave_once_exited = true;
                needs_rebuild = true;
                needs_draw = true;
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
                push_log_line(
                    &mut log_lines,
                    &mut next_log_seq,
                    format_log_line(
                        &mut clock,
                        None,
                        "engine still running 10s after the second signal; sending SIGKILL",
                    ),
                );
                child.kill_group();
                kill_deadline = None;
                needs_rebuild = true;
                needs_draw = true;
            }
        }

        for line in stderr_rx.try_iter() {
            push_log_line(
                &mut log_lines,
                &mut next_log_seq,
                format_log_line(&mut clock, None, &format!("engine: {line}")),
            );
            needs_rebuild = true;
            needs_draw = true;
        }

        if exit_status.is_none() {
            let (lines, state) = observe_tick(
                &mut live_poller,
                &mut events_tailer,
                &mut differ,
                &mut model,
                &plan,
            );
            needs_rebuild |= !lines.is_empty();
            needs_draw |= !lines.is_empty();
            for line in &lines {
                push_log_line(
                    &mut log_lines,
                    &mut next_log_seq,
                    format_log_line(&mut clock, line.ts.as_deref(), &line.text),
                );
                push_story_line(&mut story, line.clone());
            }
            if let Some(state) = state {
                current_state = Some(state);
                needs_rebuild = true;
                needs_draw = true;
            }
        }

        // K4: a full rebuild is forced at least once a second even
        // with nothing else dirty, so the header's own elapsed time
        // still visibly ticks while the run is otherwise quiet --
        // once the engine process has exited, that elapsed time is
        // frozen at its own final value, so forcing a rebuild with
        // nothing changed would only cost cycles (the Flow screen's
        // own model among them) for no visible difference at all.
        if exit_status.is_none() && last_rebuild.elapsed() >= RENDER_REBUILD_INTERVAL {
            needs_rebuild = true;
            needs_draw = true;
        }

        if needs_rebuild {
            let process = match exit_status {
                None => ProcessState::Running,
                // DESIGN.md §2.1 rule 7: `likely_failed` is the real
                // exit code being exactly 1 (what cof exits with for
                // a pre-execution validation failure, §4.1), never
                // `signal_count` -- a run cancelled by a forwarded
                // signal exits 130/143/129, not 1.
                Some(status) => ProcessState::Exited {
                    interrupted: signal_count > 0,
                    likely_failed: status.code() == Some(1),
                    has_failure_reason: failure_reason_fallback(&run_dir, &model).is_some(),
                    exited_at: exit_status_ts.clone(),
                },
            };
            now_str = now_rfc3339();
            render_state = oscilloscope_core::render::build_folded(
                &document,
                &engine_name,
                &plan,
                &mut model,
                current_state.as_ref(),
                process,
                start.elapsed().as_secs_f64(),
                &now_str,
                &[],
                app.fold_enabled,
                &app.expanded_quick_steps,
            );
            history_io::apply_history_eta(
                history,
                &mut render_state.header,
                current_state.as_ref(),
                &plan,
                &now_str,
            );
            let baseline_exists = prev_render_state.is_some();
            mark_highlights(
                &mut highlights,
                &mut prev_render_state,
                &render_state,
                no_animation,
            );
            mark_new_log_lines(
                &mut highlights,
                &mut prev_log_total,
                next_log_seq,
                no_animation,
                baseline_exists,
            );
            mark_details_highlights(
                &mut highlights,
                &mut prev_details_highlight,
                details_selection_path(&app).as_deref(),
                &plan,
                &model,
                current_state.as_ref(),
                no_animation,
            );
            last_rebuild = Instant::now();
            needs_rebuild = false;
            if app.follow {
                if let Some(target) = App::follow_target(&render_state.rows) {
                    app.select_tree_path_keeping_details(Some(target.to_string()));
                }
            }
            app.reconcile_folding(&render_state.rows);
            if app.selected_path.is_none() {
                let first = render_state.rows.first().map(|r| r.path.clone());
                app.select_tree_path_keeping_details(first);
            }
            // Built after follow (and the first-selection fallback)
            // updated `app.selected_path`, not before -- the now
            // pane's own focus reads that selection, so building it
            // earlier left it a tick stale on the very frame follow
            // first lands on a target.
            overview = build_overview_if_active(
                &app,
                &render_state,
                &plan,
                &model,
                current_state.as_ref(),
                &log_lines,
                &now_str,
            );
            cost_screen =
                build_cost_screen_if_active(&app, &render_state, current_state.as_ref(), &now_str);
            flow_screen = build_flow_screen_if_active(
                &app,
                &render_state,
                &plan,
                &model,
                current_state.as_ref(),
                &story,
                &now_str,
            );
            time_screen = build_time_screen_if_active(
                &app,
                &render_state,
                &plan,
                &model,
                current_state.as_ref(),
                &story,
                &now_str,
            );
            transcript_view = tail_focused_transcript(
                &app,
                overview.as_ref(),
                flow_screen.as_ref(),
                time_screen.as_ref(),
                &mut transcript_cache,
            );
        }
        // K4: computed every tick regardless (collapse/filter/errors-
        // only and the selection itself can change on a key alone),
        // but cheaply — against whatever `render_state.rows` the last
        // rebuild above left cached, not a fresh one every tick.
        // Issue #635: on the Flow screen, movement must land only
        // on a row the Flow pane actually draws, never on a step
        // hidden under an untaken branch or merged into its own
        // single-pass loop row -- `visible_rows_for_flow` narrows the
        // same real-`Row` list every other screen moves over to
        // exactly those. Issue #650: the same rule, for the Time
        // screen's own collapsed loop rows.
        let visible = match (&flow_screen, &time_screen) {
            (Some(screen), _) if app.screen == oscilloscope_core::screens::Screen::Flow => {
                keys::visible_rows_for_flow(&render_state.rows, &screen.rows, &app)
            }
            (_, Some(screen)) if app.screen == oscilloscope_core::screens::Screen::Time => {
                keys::visible_rows_for_time(&render_state.rows, &screen.rows, &app)
            }
            _ => keys::visible_rows(&render_state.rows, &app),
        };
        let state_visible = keys::visible_state_rows(&render_state.state_rows, &app);

        // Third finding: never call into
        // crossterm directly here at all any more -- see
        // `spawn_event_reader`'s own doc comment for why a pre-check
        // between crossterm calls, on this same thread, can't work.
        // `stdin_hung_up()` is still checked every tick regardless of
        // whether the reader thread answered this time.
        let mut terminal_gone = stdin_hung_up();
        if !terminal_gone {
            match event_rx.recv_timeout(KEY_POLL_INTERVAL) {
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => terminal_gone = true,
                Ok(Err(_)) => terminal_gone = true,
                Ok(Ok(ev)) => match ev {
                    CEvent::Key(key) if key.kind == KeyEventKind::Press => {
                        let running = exit_status.is_none();
                        let has_prompt_sent = details_selection_path(&app).is_some_and(|p| {
                            oscilloscope_core::render::full_value(
                                &p,
                                oscilloscope_core::render::FullValueField::PromptSent,
                                current_state.as_ref(),
                            )
                            .is_some()
                        });
                        let cost_tree: &[oscilloscope_core::treemap::TreemapNode] = cost_screen
                            .as_ref()
                            .map(|c| c.tree.as_slice())
                            .unwrap_or(&[]);
                        match app.handle_key(
                            key,
                            &visible,
                            &state_visible,
                            running,
                            has_prompt_sent,
                            cost_tree,
                        ) {
                            keys::Action::Cancel => {
                                cancel_once(
                                    &child,
                                    ForwardSignal::Int,
                                    &mut signal_count,
                                    &mut kill_deadline,
                                    &mut log_lines,
                                    &mut next_log_seq,
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
                                        &mut next_log_seq,
                                        &mut clock,
                                    );
                                    leave_once_exited = true;
                                } else {
                                    // K3: "with nothing running: quit".
                                    break 'tui;
                                }
                            }
                            keys::Action::CycleTheme => {
                                // Finding 19: only logs the fallback
                                // note on an actual transition into
                                // (or out of) it -- on a 16-colour
                                // terminal, `t` cycling past several
                                // unsupported themes in a row used to
                                // re-log the same note on every single
                                // press, since every one of them
                                // individually falls back. The banner/
                                // log line both name the theme actually
                                // applied, not whatever was requested,
                                // so a fallback reads as "terminal",
                                // never the unsupported name that can't
                                // show.
                                let previously_applied = resolved_theme.theme.name;
                                theme_name = theme_name.next();
                                resolved_theme = theme::resolve(theme_name, color_mode);
                                if resolved_theme.theme.name != previously_applied {
                                    if let Some(note) = resolved_theme.downgrade_note.take() {
                                        push_log_line(
                                            &mut log_lines,
                                            &mut next_log_seq,
                                            format_log_line(&mut clock, None, &note),
                                        );
                                    }
                                }
                                push_log_line(
                                    &mut log_lines,
                                    &mut next_log_seq,
                                    format_log_line(
                                        &mut clock,
                                        None,
                                        &format!("theme: {}", resolved_theme.theme.name.as_str()),
                                    ),
                                );
                                theme_banner_until =
                                    Some(Instant::now() + std::time::Duration::from_secs(2));
                            }
                            keys::Action::Quit => break 'tui,
                            keys::Action::None => {}
                        }
                        needs_rebuild = true;
                        needs_draw = true;
                        // A key can change `app.screen` or
                        // `app.selected_path` (a screen switch, a
                        // move) right here, but the next rebuild
                        // (above, at the top of the loop) doesn't run
                        // until next iteration -- without this, the
                        // frame this same key press draws still has
                        // the *previous* screen's overview (`None`
                        // right after switching onto it), one blank
                        // redraw wide. `render_state` itself needs no
                        // refresh here: nothing a key does changes
                        // what it holds.
                        overview = build_overview_if_active(
                            &app,
                            &render_state,
                            &plan,
                            &model,
                            current_state.as_ref(),
                            &log_lines,
                            &now_str,
                        );
                        cost_screen = build_cost_screen_if_active(
                            &app,
                            &render_state,
                            current_state.as_ref(),
                            &now_str,
                        );
                        flow_screen = build_flow_screen_if_active(
                            &app,
                            &render_state,
                            &plan,
                            &model,
                            current_state.as_ref(),
                            &story,
                            &now_str,
                        );
                        time_screen = build_time_screen_if_active(
                            &app,
                            &render_state,
                            &plan,
                            &model,
                            current_state.as_ref(),
                            &story,
                            &now_str,
                        );
                        transcript_view = tail_focused_transcript(
                            &app,
                            overview.as_ref(),
                            flow_screen.as_ref(),
                            time_screen.as_ref(),
                            &mut transcript_cache,
                        );
                    }
                    CEvent::Resize(_, _) => {
                        needs_rebuild = true;
                        needs_draw = true;
                    }
                    _ => {}
                },
            }
        }
        if terminal_gone {
            cancel_for_lost_terminal(
                &child,
                exit_status,
                &mut signal_count,
                &mut kill_deadline,
                &mut log_lines,
                &mut next_log_seq,
                &mut clock,
            );
            lost_terminal = true;
            break 'tui;
        }

        // Issue #535's own redraw rule: while any highlight is
        // fading, or the run is live with animation on (so the scope
        // icon's own trace keeps moving), mark the frame dirty every
        // tick -- the existing `REDRAW_INTERVAL` cap still paces the
        // actual draw. With neither, this adds no redraws beyond what
        // was already dirty. Finding 1: this sets only `needs_draw`,
        // never `needs_rebuild` -- an animation-only tick has nothing
        // new to rebuild from.
        let highlight_now = Instant::now();
        if !no_animation {
            highlights.prune(highlight_now);
            if highlights.has_active(highlight_now) || exit_status.is_none() {
                needs_draw = true;
            }
        }

        if needs_draw && last_draw.elapsed() >= REDRAW_INTERVAL {
            let details = details_selection_path(&app).map(|p| {
                oscilloscope_core::render::details_for(&p, &plan, &model, current_state.as_ref())
            });
            let icon_frame = icon_frame_for(no_animation, start.elapsed());
            let banner = theme_banner_until
                .filter(|&until| highlight_now < until)
                .map(|_| resolved_theme.theme.name.as_str());
            let ctx = tui::DrawCtx {
                theme: &resolved_theme.theme,
                highlights: &highlights,
                now: highlight_now,
                color_mode,
                theme_banner: banner,
                zone: &LocalZone,
                no_animation,
            };
            let drew = terminal.draw(|f| {
                tui::draw(
                    f,
                    &render_state,
                    details.as_ref(),
                    &log_lines,
                    next_log_seq,
                    &mut app,
                    current_state.as_ref(),
                    &ctx,
                    icon_frame,
                    &now_str,
                    overview.as_ref(),
                    cost_screen.as_ref(),
                    flow_screen.as_ref(),
                    time_screen.as_ref(),
                    transcript_view.as_ref(),
                    &mut cost_layout,
                );
            });
            last_draw = Instant::now();
            needs_draw = false;
            if drew.is_err() {
                cancel_for_lost_terminal(
                    &child,
                    exit_status,
                    &mut signal_count,
                    &mut kill_deadline,
                    &mut log_lines,
                    &mut next_log_seq,
                    &mut clock,
                );
                lost_terminal = true;
                break 'tui;
            }
        }

        if exit_status.is_none() {
            if let Ok(Some(status)) = child.try_wait() {
                exit_status = Some(status);
                exit_status_ts = Some(now_rfc3339());
                // #494: the engine already exited, but its last
                // events and its final live-state write (DESIGN.md
                // §1.1) can still land after the poll above that just
                // caught `try_wait`'s own exit -- drain once more,
                // before the next render rebuild, through the same
                // final read `finish_run` takes, so this loop's own
                // post-exit screen can't show the run `aborted` with
                // every write already sitting on disk.
                let (lines, out_state, last_live) = drain_until_ended(
                    &spec.out_path(),
                    &mut live_poller,
                    &mut events_tailer,
                    &mut differ,
                    &mut model,
                    &plan,
                );
                for line in &lines {
                    push_log_line(
                        &mut log_lines,
                        &mut next_log_seq,
                        format_log_line(&mut clock, line.ts.as_deref(), &line.text),
                    );
                    push_story_line(&mut story, line.clone());
                }
                if let Some(state) = out_state.or(last_live) {
                    current_state = Some(state);
                }
                push_run_end_story_line(
                    &differ,
                    current_state.as_ref(),
                    &run_dir,
                    &model,
                    &mut clock,
                    &mut log_lines,
                    &mut next_log_seq,
                    &mut story,
                );
                needs_rebuild = true;
                needs_draw = true;
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
        // own dialog logic means by "nothing running"), so neither
        // reaches here with the engine still going — but a lost
        // terminal (`cancel_for_lost_terminal`'s own `break 'tui`)
        // can, with the engine's own cancellation only just sent: a
        // real, blocking wait for however long that takes, not a
        // busy one, and the only safety net if the first invariant
        // were ever wrong too.
        None => child.wait().expect("waiting for the engine's own exit"),
    };

    // ratatui's own `Terminal::drop` shows the cursor again and reports
    // a failure with `eprintln!`, which panics (exit 101) when stderr is
    // the same dead terminal. With the terminal gone there is nothing
    // left to restore on it, so skip that drop.
    if lost_terminal {
        std::mem::forget(terminal);
    } else {
        drop(terminal);
    }
    drop(_guard);
    flush_history_notices(history);

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
        history,
        &engine_name,
    );
    // Design: `spawn_event_reader`'s own
    // background thread may still be stuck forever inside crossterm
    // if the terminal hung up (its own doc comment) -- a plain
    // `return`/falling off the end here would already exit the
    // process just as fast regardless (detached threads never block
    // it), but ending it explicitly, after this function's own
    // cleanup and final output, removes any doubt that anything else
    // could ever end up waiting on that thread first.
    let _ = out.flush();
    std::process::exit(i32::from(code));
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

/// The plain `--log` summary's own synthetic line for a run that
/// never reached `differ.finish` at all (DESIGN.md §2.1 rule 7's
/// "process exited with no ended state" branch): shared by
/// `finish_run` (which has a real exit code) and `finish_watch`
/// (which has none, and passes `likely_failed` from `run_end`'s own
/// `ok` instead, K4). Goes through the same `run_status` the TUI
/// header and `Differ::finish` use, so the three surfaces can't
/// label the same run differently -- this is the K6 fix: an invalid
/// document (`likely_failed`, with cof's own stdout JSON as the
/// reason) must read "failed", not "aborted".
fn unended_run_summary_text(
    state: Option<&serde_json::Value>,
    likely_failed: bool,
    run_dir: &Path,
    model: &RunModel,
) -> String {
    let reason = failure_reason_fallback(run_dir, model).filter(|r| !r.is_empty());
    let process = ProcessState::Exited {
        interrupted: false,
        likely_failed,
        has_failure_reason: reason.is_some(),
        exited_at: None,
    };
    match run_status(state, process) {
        RunStatus::Failed => format!(
            "■ run failed: {}",
            reason
                .as_deref()
                .map(oscilloscope_core::diff::format_reason_for_summary)
                .unwrap_or_default()
        ),
        _ => "■ run aborted (no final state)".to_string(),
    }
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
    // The state poll happens first, purely so `diff_event` below has
    // this tick's own freshest snapshot to fall back on for container
    // detection (`note_preview`, issue #433's "osi watch without
    // --plan" item) — events are still processed *before* `diff`
    // itself: `diff_event` marks a path event-sourced as it goes, and
    // `diff` needs that already set for *this* tick to skip its own
    // line for a path whose state write landed in the very same tick
    // as its event — this real race only starts mattering once
    // `--events` is actually flowing (#423), which is when it first
    // showed up as a genuine duplicate line.
    let state = live_poller.poll();
    if let Some(state) = &state {
        differ.note_preview(state);
    }
    let mut lines = Vec::new();
    for raw_event in events_tailer.poll() {
        if let Some(event) = parse_event(&raw_event) {
            model.observe_event(&event);
            lines.extend(differ.diff_event(&event, plan, model));
        }
    }
    if let Some(state) = &state {
        lines.extend(differ.diff(state, plan));
    }
    // Ages every held `End` line one tick, whether or not this tick's
    // own poll/events had anything new (#545) — a leaf whose state
    // never catches up within the wait bound still gets a line,
    // rather than staying silent for the rest of the run.
    lines.extend(differ.settle_pending_ends(Instant::now()));
    oscilloscope_core::diff::sort_log_lines(&mut lines);
    (lines, state)
}

fn drain_observations(
    out: &mut impl Write,
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

/// The drain every path that reaches a run's end takes, once, before
/// it ever rebuilds a render state or prints a final summary (#494):
/// keeps calling [`observe_tick`] until a pass yields no new lines —
/// the engine has already exited (or, for `osi watch`, gone away) by
/// the time a caller gets here, so its last events and its final
/// live-state write are already on disk; this just keeps polling
/// until nothing new turns up, rather than stopping after the single
/// pass that used to miss them when they all landed between two
/// polls. Then takes the same authoritative final read `finish_run`
/// and `finish_watch` always took straight from `out_path` (DESIGN.md
/// §1.1: `--out`/`state.json`, written once, after the live-state
/// poller's own last write) — `differ.diff` is idempotent against a
/// snapshot it has already seen in full, so re-diffing it here even
/// when the live-state poller already caught the very same write
/// produces no duplicate lines. `run_tui`'s own loop used to keep
/// polling only while `exit_status.is_none()`, so a fast-exiting
/// engine's last events and final write landed after the last poll
/// that would ever read them, and the TUI kept showing the run
/// `aborted` with the engine already gone; calling this the moment
/// each caller first observes the run has ended closes that gap for
/// every one of them, through the one read they all now share.
///
/// Also returns the last live-state snapshot the drain loop itself
/// polled, separately from `out_path`'s own read: `LiveStatePoller::
/// poll` only reports a snapshot once, the first time it sees it
/// change, so a caller that only kept the diff lines and dropped the
/// snapshot could never get it back if `out_path` then turned out to
/// have nothing to read (`osi watch` on a target with no sibling
/// `state.json`, DESIGN.md Q5) — `finish_run`/`finish_watch` still
/// read only `out_path`'s own return value, exactly as before; only
/// the TUI loops that can outlive that file's absence fall back to
/// this one.
fn drain_until_ended(
    out_path: &Path,
    live_poller: &mut LiveStatePoller,
    events_tailer: &mut EventsTailer,
    differ: &mut Differ,
    model: &mut RunModel,
    plan: &PlanTree,
) -> (
    Vec<oscilloscope_core::diff::LogLine>,
    Option<serde_json::Value>,
    Option<serde_json::Value>,
) {
    let mut lines = Vec::new();
    let mut last_live = None;
    loop {
        let (tick_lines, live_state) =
            observe_tick(live_poller, events_tailer, differ, model, plan);
        let done = tick_lines.is_empty();
        if live_state.is_some() {
            last_live = live_state;
        }
        lines.extend(tick_lines);
        if done {
            break;
        }
    }
    let final_state = std::fs::read(out_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    if let Some(state) = &final_state {
        lines.extend(differ.diff(state, plan));
    }
    // A held `End` line (#545) still waiting on its node's state must
    // not be abandoned here: there is no further tick coming once
    // this loop has stopped and the authoritative final state has
    // already been diffed above, so this resolves each one now,
    // against that same final state, with no more ageing.
    lines.extend(differ.flush_pending_ends());
    (lines, final_state, last_live)
}

/// `do_watch`'s own plain stop-condition loop, shared with `run_tui_
/// watch`'s own TUI-unavailable fallback (review finding 14): the
/// same three ways a watched run ends (DESIGN.md §6.3, F4) — a
/// completed live-state write, an events `run_end`, or the engine's
/// own pid (from a `run_start` event) going away — plus a local
/// signal (P2-7). Before this was shared, that fallback reported the
/// run aborted at once, with no actual watching at all.
#[allow(clippy::too_many_arguments)]
fn supervise_plain_watch(
    out: &mut impl Write,
    clock: &mut Clock,
    live_poller: &mut LiveStatePoller,
    events_tailer: &mut EventsTailer,
    differ: &mut Differ,
    model: &mut RunModel,
    plan: &PlanTree,
    run_dir: &Path,
    signals: &mut Option<SignalWatcher>,
) -> (Option<serde_json::Value>, Option<ForwardSignal>) {
    let mut last_state: Option<serde_json::Value> = None;
    let mut warned_no_events = false;
    let events_path = run_dir.join("events.jsonl");
    let mut local_signal: Option<ForwardSignal> = None;
    loop {
        if let Some(watcher) = signals.as_mut() {
            if let Some(sig) = watcher.pending().into_iter().next() {
                local_signal = Some(sig);
                break;
            }
        }
        if let Some(state) =
            drain_observations(out, clock, live_poller, events_tailer, differ, model, plan)
        {
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
                "osi: this run has no --events stream; an aborted run can't be \
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
            if let Some(state) =
                drain_observations(out, clock, live_poller, events_tailer, differ, model, plan)
            {
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
    (last_state, local_signal)
}

fn do_watch(args: WatchArgs) -> ExitCode {
    let osi_theme_env = std::env::var("OSI_THEME").ok();
    let theme_name = match resolve_theme_choice(args.theme, osi_theme_env.as_deref()) {
        Ok(name) => name,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    if args.log && args.glance {
        eprintln!("osi: --log and --glance can't be combined");
        return ExitCode::from(2);
    }
    if args.once && !args.glance {
        eprintln!("osi: --once needs --glance");
        return ExitCode::from(2);
    }

    let target = PathBuf::from(&args.target);
    // P2-4: a typo'd path used to be silently treated as a direct
    // `state.live.json` target and waited on forever -- with no
    // staleness timeout (N3's own decision) there was nothing to ever
    // notice it, let alone say so.
    if !target.exists() {
        eprintln!("osi: watch target {} does not exist", target.display());
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
        eprintln!("osi: waiting for {}", live_state_path.display());
    }

    // K4: the same options check -- `--config`/`-e`, when given --
    // `osi <doc>` itself uses, so `osi watch --plan doc.yml` compiles
    // the same plan a live `osi run` of that document would.
    let plan = match &args.plan {
        Some(doc) => match oscilloscope_core::plan::parse_inputs(&args.set) {
            Ok(inputs) => match compile_plan!(doc, args.config.as_deref(), &inputs) {
                Ok((program, discovered_config)) => {
                    let effective_config = discovered_config.as_ref().map(|d| d.path());
                    PlanTree::from_program_with_options(&program, effective_config, &inputs)
                }
                Err(err) => {
                    eprintln!(
                        "osi: couldn't compile a plan ({err}); running from observations only"
                    );
                    PlanTree::empty()
                }
            },
            Err(err) => {
                eprintln!("osi: couldn't compile a plan ({err}); running from observations only");
                PlanTree::empty()
            }
        },
        None => PlanTree::empty(),
    };
    let history = history_io::HistoryHandle::for_watch(
        history_io::resolve_history_dir(args.no_history),
        args.plan.clone(),
    );

    // Issue #626: `--glance` wins over both the TUI and `--log`,
    // same as `osi <doc>`.
    if args.glance {
        return watch_glance(
            run_dir,
            live_state_path,
            plan,
            args.target.clone(),
            args.no_animation,
            args.once,
            &history,
        );
    }

    // DESIGN.md §6.2/§6.3: same TTY/—log split as `osi <doc>`; `q` in
    // this one just detaches (§6.3), since watch owns no engine of
    // its own to ever cancel.
    if !effective_log_mode(args.log) {
        return run_tui_watch(
            run_dir,
            live_state_path,
            plan,
            args.target.clone(),
            args.no_animation,
            theme_name,
            &history,
        );
    }

    let mut clock = Clock::new();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    let mut live_poller = LiveStatePoller::new(&live_state_path);
    let mut events_tailer = EventsTailer::new(run_dir.join("events.jsonl"));
    let mut differ = Differ::new();
    let mut model = RunModel::new();
    let mut signals = SignalWatcher::new().ok();

    // F4/N3: `osi watch` owns no `Child` for a run it didn't start,
    // so `supervise_plain_watch` stops on a completed live-state
    // write, an events `run_end`, the engine's own pid (from a
    // `run_start` event) going away, or a local signal (P2-7) —
    // whichever comes first.
    let (mut last_state, local_signal) = supervise_plain_watch(
        &mut out,
        &mut clock,
        &mut live_poller,
        &mut events_tailer,
        &mut differ,
        &mut model,
        &plan,
        &run_dir,
        &mut signals,
    );

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
        false,
        &history,
    );
    let _ = writeln!(out, "exit {code}");
    ExitCode::from(code as u8)
}

/// `osi watch`'s own TUI loop (DESIGN.md §6.3): the same render/key
/// loop `run_tui` gives a live run, but with none of its own engine
/// to supervise or cancel — `q` always just detaches (§6.3: "In `osi
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
    fallback_document: String,
    no_animation: bool,
    mut theme_name: theme::ThemeName,
    history: &history_io::HistoryHandle,
) -> ExitCode {
    let mut live_poller = LiveStatePoller::new(&live_state_path);
    let mut events_tailer = EventsTailer::new(run_dir.join("events.jsonl"));
    let mut differ = Differ::new();
    let mut model = RunModel::new();
    let mut clock = Clock::new();
    let mut log_lines: Vec<String> = Vec::new();
    // Finding 4: see `run_tui`'s own `next_log_seq`.
    let mut next_log_seq: u64 = 0;
    // See `run_tui`'s own `story` (issue #635).
    let mut story: Vec<oscilloscope_core::diff::LogLine> = Vec::new();
    let mut last_state: Option<serde_json::Value> = None;
    let mut warned_no_events = false;
    let events_path = run_dir.join("events.jsonl");
    let mut signals = SignalWatcher::new().ok();
    let mut local_signal: Option<ForwardSignal> = None;
    // Finding 15: `q` on a still-live run is a voluntary detach, not
    // an abort.
    let mut detached_while_live = false;
    let start = Instant::now();

    let mut app = App::new(true);
    let mut cost_layout = tui::CostLayoutCache::default();

    let color_mode = theme::ColorMode::from_env();
    let mut resolved_theme = theme::resolve(theme_name, color_mode);
    if let Some(note) = resolved_theme.downgrade_note.take() {
        push_log_line(
            &mut log_lines,
            &mut next_log_seq,
            format_log_line(&mut clock, None, &note),
        );
    }
    let mut highlights = highlight::Tracker::new();
    let mut prev_render_state: Option<oscilloscope_core::render::RenderState> = None;
    // Finding 4/5: see `run_tui`'s own comment above.
    let mut prev_log_total: u64 = next_log_seq;
    let mut prev_details_highlight: Option<(
        String,
        Option<oscilloscope_core::render::TokenBlock>,
        Option<oscilloscope_core::render::TokenBlock>,
    )> = None;
    let mut theme_banner_until: Option<Instant> = None;

    let guard_and_terminal = TerminalGuard::enter()
        .and_then(|g| Terminal::new(CrosstermBackend::new(std::io::stdout())).map(|t| (g, t)));
    let (_guard, mut terminal) = match guard_and_terminal {
        Ok(pair) => pair,
        Err(err) => {
            // Finding 14: actually watches via the same stop-
            // condition loop `do_watch` uses, rather than reporting
            // the run aborted at once with no watching at all.
            eprintln!("osi: couldn't start the TUI ({err}); falling back to the plain stream");
            let stdout = std::io::stdout();
            let mut out = stdout.lock();
            let (mut last_state, local_signal) = supervise_plain_watch(
                &mut out,
                &mut clock,
                &mut live_poller,
                &mut events_tailer,
                &mut differ,
                &mut model,
                &plan,
                &run_dir,
                &mut signals,
            );
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
                false,
                history,
            );
            let _ = writeln!(out, "exit {code}");
            return ExitCode::from(code as u8);
        }
    };

    // Design: crossterm's own blocking
    // `event::read()` runs on its own thread from here on, never
    // this one -- see `spawn_event_reader`'s own doc comment.
    let event_rx = spawn_event_reader();

    let mut last_draw = Instant::now() - REDRAW_INTERVAL;
    let mut last_rebuild = Instant::now() - RENDER_REBUILD_INTERVAL;
    // Finding 1: see `run_tui`'s own comment above.
    let mut needs_rebuild = true;
    let mut needs_draw = true;
    // K1: once the watched run has ended (by state, by events, or by
    // its pid going away), this loop keeps running — redrawing the
    // final state and reading keys — rather than detaching on its
    // own; only `ended` stops `observe_tick` from polling a run
    // directory that has nothing left to say.
    let mut ended = false;
    let mut now_str = now_rfc3339();
    // Issue #626: `osi watch` names the document from the run's own
    // `runtime.last_run.orchestration_path` once a live-state write
    // has it, not from the directory/file it was pointed at.
    let mut document = watch_document_name(last_state.as_ref(), &fallback_document);
    let mut render_state = oscilloscope_core::render::build_folded(
        &document,
        "watch",
        &plan,
        &mut model,
        last_state.as_ref(),
        ProcessState::Running,
        start.elapsed().as_secs_f64(),
        &now_str,
        &[],
        app.fold_enabled,
        &app.expanded_quick_steps,
    );
    // `osi watch` owns no process of its own to read `ProcessState`
    // from (DESIGN.md §4.1): the header's own `♥` (issue #626) reads
    // the watched run's own pid from `--events`' `run_start` instead.
    render_state.header.engine_alive = model
        .run_start_pid()
        .is_some_and(oscilloscope_core::supervise::process_alive);
    history_io::apply_history_eta(
        history,
        &mut render_state.header,
        last_state.as_ref(),
        &plan,
        &now_str,
    );
    let mut overview = build_overview_if_active(
        &app,
        &render_state,
        &plan,
        &model,
        last_state.as_ref(),
        &log_lines,
        &now_str,
    );
    let mut cost_screen =
        build_cost_screen_if_active(&app, &render_state, last_state.as_ref(), &now_str);
    let mut flow_screen = build_flow_screen_if_active(
        &app,
        &render_state,
        &plan,
        &model,
        last_state.as_ref(),
        &story,
        &now_str,
    );
    let mut time_screen = build_time_screen_if_active(
        &app,
        &render_state,
        &plan,
        &model,
        last_state.as_ref(),
        &story,
        &now_str,
    );
    let mut transcript_cache = TranscriptTailCache::default();
    let mut transcript_view = tail_focused_transcript(
        &app,
        overview.as_ref(),
        flow_screen.as_ref(),
        time_screen.as_ref(),
        &mut transcript_cache,
    );
    // Finding 3: no `mark_highlights` call here -- see `run_tui`'s
    // own comment above (this placeholder build always has
    // `last_state: None`; the loop's own first real rebuild is the
    // genuine first observation, including when `osi watch` attaches
    // to a run that had already finished).
    if app.selected_path.is_none() {
        app.selected_path = render_state.rows.first().map(|r| r.path.clone());
    }

    // Set when the terminal itself went away (see `stdin_hung_up`):
    // ratatui's own `Terminal::drop` must not run then (below).
    let mut lost_terminal = false;
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
            needs_rebuild |= !lines.is_empty();
            needs_draw |= !lines.is_empty();
            for line in &lines {
                push_log_line(
                    &mut log_lines,
                    &mut next_log_seq,
                    format_log_line(&mut clock, line.ts.as_deref(), &line.text),
                );
                push_story_line(&mut story, line.clone());
            }
            if let Some(state) = state {
                last_state = Some(state);
                needs_rebuild = true;
                needs_draw = true;
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
                push_log_line(
                    &mut log_lines,
                    &mut next_log_seq,
                    format_log_line(
                        &mut clock,
                        None,
                        "this run has no --events stream; an aborted run can't be detected \
                     without one. q detaches.",
                    ),
                );
                warned_no_events = true;
                needs_rebuild = true;
                needs_draw = true;
            }
            if !ended && model.run_ended_by_events() {
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
                // #494: the same shared final drain every path that
                // reaches a run's end takes, regardless of which of
                // the three stop conditions above just tripped --
                // drains events and the live state until a pass
                // yields nothing new, then reads `state.json` once
                // more as the authoritative final write, so this
                // loop's own post-exit screen can't show a dead-pid
                // or events-only ending as `aborted` with every write
                // already sitting on disk.
                let (more_lines, out_state, last_live) = drain_until_ended(
                    &run_dir.join("state.json"),
                    &mut live_poller,
                    &mut events_tailer,
                    &mut differ,
                    &mut model,
                    &plan,
                );
                for line in &more_lines {
                    push_log_line(
                        &mut log_lines,
                        &mut next_log_seq,
                        format_log_line(&mut clock, line.ts.as_deref(), &line.text),
                    );
                    push_story_line(&mut story, line.clone());
                }
                if let Some(state) = out_state.or(last_live) {
                    last_state = Some(state);
                }
                push_run_end_story_line(
                    &differ,
                    last_state.as_ref(),
                    &run_dir,
                    &model,
                    &mut clock,
                    &mut log_lines,
                    &mut next_log_seq,
                    &mut story,
                );
                needs_rebuild = true;
                needs_draw = true;
            }
        }

        // K4, same as `run_tui`: forced only while the run itself
        // is still going, since a finished run's own elapsed time is
        // frozen and a forced rebuild past that point would only
        // cost cycles (the Flow screen's own model among them) for no
        // visible difference at all.
        if !ended && last_rebuild.elapsed() >= RENDER_REBUILD_INTERVAL {
            needs_rebuild = true;
            needs_draw = true;
        }

        if needs_rebuild {
            let process = if ended {
                // DESIGN.md §2.1 rule 7: `osi watch` owns no process
                // of its own, so `run_end`'s own `ok` stands in for a
                // real exit code of 1.
                ProcessState::Exited {
                    interrupted: false,
                    likely_failed: model.run_end_ok() == Some(false),
                    has_failure_reason: failure_reason_fallback(&run_dir, &model).is_some(),
                    exited_at: None,
                }
            } else {
                ProcessState::Running
            };
            now_str = now_rfc3339();
            document = watch_document_name(last_state.as_ref(), &fallback_document);
            render_state = oscilloscope_core::render::build_folded(
                &document,
                "watch",
                &plan,
                &mut model,
                last_state.as_ref(),
                process,
                start.elapsed().as_secs_f64(),
                &now_str,
                &[],
                app.fold_enabled,
                &app.expanded_quick_steps,
            );
            render_state.header.engine_alive = model
                .run_start_pid()
                .is_some_and(oscilloscope_core::supervise::process_alive);
            history_io::apply_history_eta(
                history,
                &mut render_state.header,
                last_state.as_ref(),
                &plan,
                &now_str,
            );
            let baseline_exists = prev_render_state.is_some();
            mark_highlights(
                &mut highlights,
                &mut prev_render_state,
                &render_state,
                no_animation,
            );
            mark_new_log_lines(
                &mut highlights,
                &mut prev_log_total,
                next_log_seq,
                no_animation,
                baseline_exists,
            );
            mark_details_highlights(
                &mut highlights,
                &mut prev_details_highlight,
                details_selection_path(&app).as_deref(),
                &plan,
                &model,
                last_state.as_ref(),
                no_animation,
            );
            last_rebuild = Instant::now();
            needs_rebuild = false;
            if app.follow {
                if let Some(target) = App::follow_target(&render_state.rows) {
                    app.select_tree_path_keeping_details(Some(target.to_string()));
                }
            }
            app.reconcile_folding(&render_state.rows);
            if app.selected_path.is_none() {
                let first = render_state.rows.first().map(|r| r.path.clone());
                app.select_tree_path_keeping_details(first);
            }
            // See `run_tui`'s own comment above -- built after
            // follow updated the selection, not before.
            overview = build_overview_if_active(
                &app,
                &render_state,
                &plan,
                &model,
                last_state.as_ref(),
                &log_lines,
                &now_str,
            );
            cost_screen =
                build_cost_screen_if_active(&app, &render_state, last_state.as_ref(), &now_str);
            flow_screen = build_flow_screen_if_active(
                &app,
                &render_state,
                &plan,
                &model,
                last_state.as_ref(),
                &story,
                &now_str,
            );
            time_screen = build_time_screen_if_active(
                &app,
                &render_state,
                &plan,
                &model,
                last_state.as_ref(),
                &story,
                &now_str,
            );
            transcript_view = tail_focused_transcript(
                &app,
                overview.as_ref(),
                flow_screen.as_ref(),
                time_screen.as_ref(),
                &mut transcript_cache,
            );
        }
        // Issue #635/#650: same narrowing as `osi <doc>`'s own main
        // loop -- see the comment there.
        let visible = match (&flow_screen, &time_screen) {
            (Some(screen), _) if app.screen == oscilloscope_core::screens::Screen::Flow => {
                keys::visible_rows_for_flow(&render_state.rows, &screen.rows, &app)
            }
            (_, Some(screen)) if app.screen == oscilloscope_core::screens::Screen::Time => {
                keys::visible_rows_for_time(&render_state.rows, &screen.rows, &app)
            }
            _ => keys::visible_rows(&render_state.rows, &app),
        };
        let state_visible = keys::visible_state_rows(&render_state.state_rows, &app);

        // Third finding: never call into
        // crossterm directly here at all any more -- see
        // `spawn_event_reader`'s own doc comment for why a pre-check
        // between crossterm calls, on this same thread, can't work.
        // `stdin_hung_up()` is still checked every tick regardless of
        // whether the reader thread answered this time.
        let mut terminal_gone = stdin_hung_up();
        if !terminal_gone {
            match event_rx.recv_timeout(KEY_POLL_INTERVAL) {
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => terminal_gone = true,
                Ok(Err(_)) => terminal_gone = true,
                Ok(Ok(ev)) => match ev {
                    CEvent::Key(key) if key.kind == KeyEventKind::Press => {
                        // Watch owns no engine to cancel (DESIGN.md
                        // §6.3), so `running` is always false here:
                        // `c` does nothing, and `q` detaches at once,
                        // with no confirm, whether the watched run is
                        // still going or not.
                        let has_prompt_sent = details_selection_path(&app).is_some_and(|p| {
                            oscilloscope_core::render::full_value(
                                &p,
                                oscilloscope_core::render::FullValueField::PromptSent,
                                last_state.as_ref(),
                            )
                            .is_some()
                        });
                        let cost_tree: &[oscilloscope_core::treemap::TreemapNode] = cost_screen
                            .as_ref()
                            .map(|c| c.tree.as_slice())
                            .unwrap_or(&[]);
                        match app.handle_key(
                            key,
                            &visible,
                            &state_visible,
                            false,
                            has_prompt_sent,
                            cost_tree,
                        ) {
                            keys::Action::Quit => {
                                // Finding 15: `q` while the watched
                                // run is still going is a voluntary
                                // detach, not an abort — the run
                                // already having ended (K1's own
                                // finished screen) still gets the
                                // normal ok/failed/aborted summary.
                                detached_while_live = !ended;
                                break 'tui;
                            }
                            // K3: Ctrl-C in `osi watch` always just
                            // detaches too, same as `q` — but reports
                            // the same 130 a real forwarded SIGINT
                            // would, not the final state's own code.
                            keys::Action::CtrlC => {
                                local_signal = Some(ForwardSignal::Int);
                                break 'tui;
                            }
                            keys::Action::CycleTheme => {
                                // Finding 19: see `run_tui`'s own
                                // comment above.
                                let previously_applied = resolved_theme.theme.name;
                                theme_name = theme_name.next();
                                resolved_theme = theme::resolve(theme_name, color_mode);
                                if resolved_theme.theme.name != previously_applied {
                                    if let Some(note) = resolved_theme.downgrade_note.take() {
                                        push_log_line(
                                            &mut log_lines,
                                            &mut next_log_seq,
                                            format_log_line(&mut clock, None, &note),
                                        );
                                    }
                                }
                                push_log_line(
                                    &mut log_lines,
                                    &mut next_log_seq,
                                    format_log_line(
                                        &mut clock,
                                        None,
                                        &format!("theme: {}", resolved_theme.theme.name.as_str()),
                                    ),
                                );
                                theme_banner_until =
                                    Some(Instant::now() + std::time::Duration::from_secs(2));
                            }
                            keys::Action::Cancel | keys::Action::None => {}
                        }
                        needs_rebuild = true;
                        needs_draw = true;
                        // See `run_tui`'s own comment above --
                        // refreshes the overview for this same
                        // frame's draw, not next iteration's.
                        overview = build_overview_if_active(
                            &app,
                            &render_state,
                            &plan,
                            &model,
                            last_state.as_ref(),
                            &log_lines,
                            &now_str,
                        );
                        cost_screen = build_cost_screen_if_active(
                            &app,
                            &render_state,
                            last_state.as_ref(),
                            &now_str,
                        );
                        flow_screen = build_flow_screen_if_active(
                            &app,
                            &render_state,
                            &plan,
                            &model,
                            last_state.as_ref(),
                            &story,
                            &now_str,
                        );
                        time_screen = build_time_screen_if_active(
                            &app,
                            &render_state,
                            &plan,
                            &model,
                            last_state.as_ref(),
                            &story,
                            &now_str,
                        );
                        transcript_view = tail_focused_transcript(
                            &app,
                            overview.as_ref(),
                            flow_screen.as_ref(),
                            time_screen.as_ref(),
                            &mut transcript_cache,
                        );
                    }
                    CEvent::Resize(_, _) => {
                        needs_rebuild = true;
                        needs_draw = true;
                    }
                    _ => {}
                },
            }
        }
        if terminal_gone {
            // `osi watch`'s own shape
            // of it: watch owns no engine to cancel, so this is
            // exactly the real-SIGHUP arm above, just detected from a
            // dead terminal instead of a real signal.
            local_signal = Some(ForwardSignal::Hup);
            lost_terminal = true;
            break 'tui;
        }

        // Finding 1: this sets only `needs_draw`, same as `run_tui`.
        let highlight_now = Instant::now();
        if !no_animation {
            highlights.prune(highlight_now);
            if highlights.has_active(highlight_now) || !ended {
                needs_draw = true;
            }
        }

        if needs_draw && last_draw.elapsed() >= REDRAW_INTERVAL {
            let details = details_selection_path(&app).map(|p| {
                oscilloscope_core::render::details_for(&p, &plan, &model, last_state.as_ref())
            });
            let icon_frame = icon_frame_for(no_animation, start.elapsed());
            let banner = theme_banner_until
                .filter(|&until| highlight_now < until)
                .map(|_| resolved_theme.theme.name.as_str());
            let ctx = tui::DrawCtx {
                theme: &resolved_theme.theme,
                highlights: &highlights,
                now: highlight_now,
                color_mode,
                theme_banner: banner,
                zone: &LocalZone,
                no_animation,
            };
            let drew = terminal.draw(|f| {
                tui::draw(
                    f,
                    &render_state,
                    details.as_ref(),
                    &log_lines,
                    next_log_seq,
                    &mut app,
                    last_state.as_ref(),
                    &ctx,
                    icon_frame,
                    &now_str,
                    overview.as_ref(),
                    cost_screen.as_ref(),
                    flow_screen.as_ref(),
                    time_screen.as_ref(),
                    transcript_view.as_ref(),
                    &mut cost_layout,
                );
            });
            last_draw = Instant::now();
            needs_draw = false;
            if drew.is_err() {
                local_signal = Some(ForwardSignal::Hup);
                lost_terminal = true;
                break 'tui;
            }
        }
    }

    // ratatui's own `Terminal::drop` shows the cursor again and reports
    // a failure with `eprintln!`, which panics (exit 101) when stderr is
    // the same dead terminal. With the terminal gone there is nothing
    // left to restore on it, so skip that drop.
    if lost_terminal {
        std::mem::forget(terminal);
    } else {
        drop(terminal);
    }
    drop(_guard);
    flush_history_notices(history);

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
        detached_while_live,
        history,
    );
    let _ = writeln!(out, "exit {code}");
    // Design -- see `run_tui`'s own matching
    // tail for why this ends the process explicitly rather than just
    // returning.
    let _ = out.flush();
    std::process::exit(i32::from(code));
}

/// The common ending every `osi watch` session reaches, however its
/// own loop stopped (a completed run, a local Ctrl-C, or a dead pid
/// with nothing left to watch): one more drain for anything that
/// landed between the loop's last check and now, the same single
/// final-summary computation `do_run`'s own `finish_run` uses (M1),
/// and the exit code `osi run` of the same document would have used
/// — from a signal `run_end` named, or from watch's own local Ctrl-C,
/// rather than only ok-vs-failed. Returns that exit code; the caller
/// prints it and `ExitCode::from`s it, the same shape `finish_run`
/// leaves to its own callers.
#[allow(clippy::too_many_arguments)]
fn finish_watch(
    out: &mut impl Write,
    clock: &mut Clock,
    live_poller: &mut LiveStatePoller,
    events_tailer: &mut EventsTailer,
    differ: &mut Differ,
    model: &mut RunModel,
    plan: &PlanTree,
    run_dir: &Path,
    last_state: &mut Option<serde_json::Value>,
    local_signal: Option<ForwardSignal>,
    detached_while_live: bool,
    history: &history_io::HistoryHandle,
) -> u8 {
    if let Some(state) =
        drain_observations(out, clock, live_poller, events_tailer, differ, model, plan)
    {
        *last_state = Some(state);
    }

    // Finding 15: a voluntary `q` detach from a run that was still
    // going is not an abort — reported distinctly, and exits 0,
    // rather than falling into the generic "no final state" ending
    // below (which this would otherwise always hit: the run, by
    // definition, hasn't written one yet).
    if detached_while_live {
        print_line(out, clock, None, "■ detached (the run is still going)");
        return 0;
    }

    // #494: the same shared final drain every path that reaches a
    // run's end takes, before the one authoritative read every
    // ending (osi run's `finish_run` included) takes from its own
    // `--out`/`state.json`. By this point the run is either already
    // ended, by one of `run_tui_watch`/`supervise_plain_watch`'s three
    // stop conditions, or `local_signal` is set (a local Ctrl-C, or an
    // INT/TERM/HUP forwarded to `osi watch` itself) and the watched
    // run may still be live -- but there's no spin risk either way:
    // one tailer poll reads whatever's on disk to EOF, so the loop
    // always ends on its first quiet pass, whether that's because the
    // engine has nothing left to say or because it's still running
    // and simply hasn't written anything new yet.
    let (lines, final_state, _) = drain_until_ended(
        &run_dir.join("state.json"),
        live_poller,
        events_tailer,
        differ,
        model,
        plan,
    );
    for line in lines {
        print_line(out, clock, line.ts.as_deref(), &line.text);
    }
    if let Some(state) = &final_state {
        *last_state = Some(state.clone());
    }
    // Issue #649: only ever reached once `detached_while_live` has
    // already returned above, so this is exactly "the watched run
    // really ended" -- `HistoryHandle::write` itself checks
    // `run_ended` too, so a dead-pid abort with no final write still
    // writes nothing.
    history.write(model.run_start_engine(), final_state.as_ref(), model, plan);
    let summary = final_state
        .as_ref()
        .and_then(|state| differ.finish(state, failure_reason_fallback(run_dir, model).as_deref()))
        .or_else(|| {
            if differ.run_line_emitted() {
                return None;
            }
            Some(oscilloscope_core::diff::LogLine {
                ts: None,
                text: unended_run_summary_text(
                    final_state.as_ref(),
                    // DESIGN.md §2.1 rule 7: `osi watch` owns no
                    // process of its own, so `run_end`'s own `ok`
                    // stands in for a real exit code of 1.
                    model.run_end_ok() == Some(false),
                    run_dir,
                    model,
                ),
                kind: oscilloscope_core::diff::LogKind::RunEnd,
                path: None,
            })
        });
    if let Some(line) = summary {
        print_line(out, clock, line.ts.as_deref(), &line.text);
    }

    watch_exit_code(local_signal, model, last_state)
}

/// `osi watch`'s own exit code (DESIGN.md §4.1), shared by `finish_
/// watch`'s own `--log`/TUI ending above and `watch_glance`'s --
/// "signals and exit codes work as with `--log`" means the exact
/// same rule, not a second copy of it that can drift: a local signal
/// (Ctrl-C, or one `osi watch` itself received) first, the watched
/// run's own `run_end` signal next, then whether the last state it
/// saw actually ended ok, falling back to `model.run_end_ok()` for an
/// events-only run with no live-state write at all, and finally an
/// abort (a dead pid with no clean completion either way).
fn watch_exit_code(
    local_signal: Option<ForwardSignal>,
    model: &RunModel,
    last_state: &Option<serde_json::Value>,
) -> u8 {
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
/// stderr, same as every other error `osi` reports (#422 review note).
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
        let watch_args = std::iter::once("osi watch".to_string()).chain(args.into_iter().skip(2));
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

    fn agent_running_state_for_transcript_path_test() -> serde_json::Value {
        serde_json::json!({"prime": {"value": null, "meta": {"completed_at": null},
            "fix": {"value": null, "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": null,
                "provider": "agent",
                "live": {"transcript": "/tmp/agent-live-path-test.jsonl", "updated_at": "2026-01-01T00:00:00Z"}
            }}
        }})
    }

    /// Issue #637 (extended by #650 to screen 1): transcript tailing
    /// only ever happens for the one step actually on screen -- the
    /// overview's own focused step on screen 0, Flow's selected step
    /// on screen 2, Time's selected step on screen 1, and nothing at
    /// all on screen 3, even though the very same agent step exists
    /// in state either way.
    #[test]
    fn focused_agent_transcript_path_is_none_off_the_overview_flow_and_time_screens() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = agent_running_state_for_transcript_path_test();
        let flat = oscilloscope_core::model::flatten_state(&state);
        let rendered = oscilloscope_core::render::build_folded(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            1.0,
            "2026-01-01T00:00:01Z",
            &[],
            true,
            &Default::default(),
        );
        let overview = oscilloscope_core::screens::build_overview(
            &rendered.rows,
            &rendered.unfolded_rows,
            &rendered.header,
            &flat,
            &plan,
            &model,
            Some(&state),
            &rendered.token_bar,
            &[],
            None,
            "2026-01-01T00:00:01Z",
        );
        let flow_screen = oscilloscope_core::screens::build_flow_screen(
            &rendered.rows,
            &rendered.unfolded_rows,
            &flat,
            &plan,
            &model,
            Some(&state),
            &[],
            Some("prime.fix"),
            &LocalZone,
            "2026-01-01T00:00:01Z",
        );
        let time_screen = oscilloscope_core::screens::build_time_screen(
            &rendered.rows,
            &flat,
            &plan,
            &model,
            &rendered.header,
            Some(&state),
            &[],
            Some("prime.fix"),
            &Default::default(),
            "2026-01-01T00:00:01Z",
        );

        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Overview;
        assert_eq!(
            focused_agent_transcript_path(
                &app,
                Some(&overview),
                Some(&flow_screen),
                Some(&time_screen)
            ),
            Some("/tmp/agent-live-path-test.jsonl".to_string())
        );

        app.screen = oscilloscope_core::screens::Screen::Flow;
        assert_eq!(
            focused_agent_transcript_path(
                &app,
                Some(&overview),
                Some(&flow_screen),
                Some(&time_screen)
            ),
            Some("/tmp/agent-live-path-test.jsonl".to_string())
        );

        app.screen = oscilloscope_core::screens::Screen::Time;
        assert_eq!(
            focused_agent_transcript_path(
                &app,
                Some(&overview),
                Some(&flow_screen),
                Some(&time_screen)
            ),
            Some("/tmp/agent-live-path-test.jsonl".to_string())
        );

        app.screen = oscilloscope_core::screens::Screen::Cost;
        assert_eq!(
            focused_agent_transcript_path(
                &app,
                Some(&overview),
                Some(&flow_screen),
                Some(&time_screen)
            ),
            None,
            "no screen but overview/flow/time ever names a transcript to tail"
        );
    }

    /// Issue #637's own bound, checked at this function's own level
    /// too (`observe::tail_transcript_lines` already has its own byte
    /// -level test): a step whose own `meta.live` has no `transcript`
    /// at all, or isn't an agent, never names a path to tail either.
    #[test]
    fn focused_agent_transcript_path_is_none_for_a_finished_step_with_no_live_left() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = serde_json::json!({"prime": {"value": null, "meta": {"completed_at": "t"},
            "fix": {"value": "done", "meta": {"created_at": "t0", "completed_at": "t1", "provider": "agent"}}
        }});
        let flat = oscilloscope_core::model::flatten_state(&state);
        let rendered = oscilloscope_core::render::build_folded(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(false),
            1.0,
            "t2",
            &[],
            true,
            &Default::default(),
        );
        let overview = oscilloscope_core::screens::build_overview(
            &rendered.rows,
            &rendered.unfolded_rows,
            &rendered.header,
            &flat,
            &plan,
            &model,
            Some(&state),
            &rendered.token_bar,
            &[],
            None,
            "t2",
        );
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Overview;
        assert_eq!(
            focused_agent_transcript_path(&app, Some(&overview), None, None),
            None
        );
    }

    /// Issue #635: "it keeps at most 2,000 lines" -- pushing well
    /// past that cap must leave the buffer at exactly
    /// `MAX_STORY_LINES`, having dropped the *oldest* lines first,
    /// never the newest.
    #[test]
    fn push_story_line_caps_the_buffer_at_2000_lines_dropping_the_oldest() {
        let mut story = Vec::new();
        for i in 0..MAX_STORY_LINES + 10 {
            push_story_line(
                &mut story,
                oscilloscope_core::diff::LogLine {
                    ts: None,
                    text: format!("▸ report {i}"),
                    kind: oscilloscope_core::diff::LogKind::Report,
                    path: None,
                },
            );
        }
        assert_eq!(story.len(), MAX_STORY_LINES);
        assert_eq!(story.first().unwrap().text, "▸ report 10");
        assert_eq!(
            story.last().unwrap().text,
            format!("▸ report {}", MAX_STORY_LINES + 9)
        );
    }

    /// A line whose own `kind` isn't one of the story's
    /// (`Start`/`End`/`Progress`/`Retry`, DESIGN.md) never enters the
    /// buffer at all.
    #[test]
    fn push_story_line_drops_a_non_story_kind() {
        let mut story = Vec::new();
        push_story_line(
            &mut story,
            oscilloscope_core::diff::LogLine {
                ts: None,
                text: "start".to_string(),
                kind: oscilloscope_core::diff::LogKind::Start,
                path: None,
            },
        );
        assert!(story.is_empty());
    }

    /// Issue #635: the live story gets the run-end line as soon as
    /// state reads as ended, through a *clone* of `differ` -- the
    /// real one's own one-shot `run_line_emitted` latch must stay
    /// untripped, so `--log`'s own authoritative call (`finish_run`/
    /// `finish_watch`, after the TUI has closed) still prints it
    /// exactly once of its own.
    #[test]
    fn push_run_end_story_line_leaves_the_real_differ_to_still_print_once_after() {
        let mut differ = oscilloscope_core::diff::Differ::new();
        let model = RunModel::new();
        let state = serde_json::json!({
            "runtime": {"last_run": {"completed_at": "t1"}},
            "prime": {"value": "ok", "meta": {"created_at": "t0", "completed_at": "t1", "error": null}}
        });
        let mut clock = Clock::new();
        let mut log_lines = Vec::new();
        let mut next_log_seq = 0u64;
        let mut story = Vec::new();
        let run_dir = Path::new("/does/not/exist");
        push_run_end_story_line(
            &differ,
            Some(&state),
            run_dir,
            &model,
            &mut clock,
            &mut log_lines,
            &mut next_log_seq,
            &mut story,
        );
        assert!(
            !story.is_empty(),
            "the live story must get the run-end line while still running"
        );
        assert!(
            !differ.run_line_emitted(),
            "the real differ's own latch must stay untripped"
        );
        let final_line = differ.finish(&state, None);
        assert!(
            final_line.is_some(),
            "--log's own authoritative call must still print exactly once"
        );
    }

    fn args(words: &[&str]) -> Vec<String> {
        std::iter::once("osi".to_string())
            .chain(words.iter().map(|w| w.to_string()))
            .collect()
    }

    /// Issue #626: `osi watch` names the document from the watched
    /// run's own `runtime.last_run.orchestration_path` once a
    /// live-state write has it, not from whatever directory or file
    /// it was pointed at -- the only document name `osi watch` ever
    /// actually has of its own.
    #[test]
    fn watch_document_name_prefers_the_runs_own_orchestration_path() {
        let state = serde_json::json!({
            "runtime": {"last_run": {"orchestration_path": "do-thing.yml"}}
        });
        assert_eq!(
            watch_document_name(Some(&state), "fallback.yml"),
            "do-thing.yml"
        );
    }

    #[test]
    fn watch_document_name_falls_back_with_no_state_or_orchestration_path_yet() {
        assert_eq!(watch_document_name(None, "fallback.yml"), "fallback.yml");
        let state = serde_json::json!({"runtime": {"last_run": {}}});
        assert_eq!(
            watch_document_name(Some(&state), "fallback.yml"),
            "fallback.yml"
        );
    }

    /// #610: a state-pane selection must stay what `Details` shows
    /// even once the user tabs away from `Pane::State` onto `Pane::
    /// Details` itself to scroll it -- the only way to ever actually
    /// scroll a state selection's own details at all, since `Pane::
    /// State` has no scroll-details key of its own.
    #[test]
    fn details_selection_path_follows_whichever_selection_changed_last() {
        let mut app = App::new(false);
        assert_eq!(details_selection_path(&app), None);

        app.select_tree_path(Some("prime.a".to_string()));
        assert_eq!(details_selection_path(&app).as_deref(), Some("prime.a"));

        // Tab to `Details`: the tree selection is still the source.
        app.pane = keys::Pane::Details;
        assert_eq!(details_selection_path(&app).as_deref(), Some("prime.a"));

        // Tab to `State` and pick a row there -- `pane` is `State`
        // now, not `Details`, but this is the only state the test can
        // reach through real key handling before tabbing onward.
        app.pane = keys::Pane::State;
        let status_row = oscilloscope_core::render::StateRow {
            path: "status".to_string(),
            depth: 0,
            key: "status".to_string(),
            has_children: false,
            status: oscilloscope_core::model::RowStatus {
                kind: oscilloscope_core::model::StatusKind::Done,
                skip_reason: None,
                retrying: false,
            },
            dim: None,
            tokens: None,
        };
        app.handle_key(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Down,
                crossterm::event::KeyModifiers::NONE,
            ),
            &[],
            &[&status_row],
            true,
            false,
            &[],
        );
        assert_eq!(
            details_selection_path(&app).as_deref(),
            Some("prime.status"),
            "a state selection must win over a stale tree selection"
        );

        // Tab away to `Tree`, then to `Details`: the state selection
        // is still the source, because the tree selection never
        // changed again in between.
        app.pane = keys::Pane::Tree;
        app.pane = keys::Pane::Details;
        assert_eq!(
            details_selection_path(&app).as_deref(),
            Some("prime.status"),
            "tabbing through other panes must not reset the details source on its own"
        );

        // A fresh tree selection wins back.
        app.select_tree_path(Some("prime.b".to_string()));
        assert_eq!(details_selection_path(&app).as_deref(), Some("prime.b"));
    }

    /// Issue #535's own `--no-animation` acceptance item: a unit test
    /// showing it turns the tracker off -- a real status change
    /// between two render states marks a highlight when animation is
    /// on, and marks nothing at all when it's off.
    #[test]
    fn no_animation_turns_the_highlight_tracker_off() {
        let plan = PlanTree::empty();
        let running = serde_json::json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": null, "meta": {"created_at": "t0", "completed_at": null}}
        }});
        let done = serde_json::json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "x", "meta": {"created_at": "t0", "completed_at": "t1"}}
        }});
        let mut model = RunModel::new();
        let old = oscilloscope_core::render::build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&running),
            ProcessState::Running,
            0.0,
            "t0",
            &[],
        );
        let new = oscilloscope_core::render::build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&done),
            ProcessState::Running,
            1.0,
            "t1",
            &[],
        );

        let mut prev = Some(old.clone());
        let mut tracker = highlight::Tracker::new();
        mark_highlights(&mut tracker, &mut prev, &new, false);
        assert!(
            tracker.has_active(Instant::now()),
            "animation on: the change should be marked"
        );

        let mut prev2 = Some(old);
        let mut tracker2 = highlight::Tracker::new();
        mark_highlights(&mut tracker2, &mut prev2, &new, true);
        assert!(
            !tracker2.has_active(Instant::now()),
            "--no-animation: nothing should ever be marked"
        );
    }

    /// Finding 3: the real call sequence both TUI loops take now --
    /// `osi watch` attaching to a run that's already well under way
    /// (full state from the very first real rebuild, nothing like
    /// `run_tui`/`run_tui_watch`'s own placeholder build with `state:
    /// None` ever gets diffed against). `prev_render_state` starts at
    /// `None`, same as both functions leave it before their loop, and
    /// the first `mark_highlights` call is the loop's first real
    /// rebuild, not a second one against an empty placeholder.
    #[test]
    fn the_first_real_rebuild_is_silent_even_with_a_run_already_well_under_way() {
        let plan = PlanTree::empty();
        let already_progressed = serde_json::json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "x", "meta": {"created_at": "t0", "completed_at": "t1"}},
            "b": {"value": null, "meta": {"created_at": "t1", "completed_at": null}}
        }});
        let mut model = RunModel::new();
        let first_real_build = oscilloscope_core::render::build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&already_progressed),
            ProcessState::Running,
            1.0,
            "t1",
            &[],
        );

        // No placeholder build is ever fed through `mark_highlights`
        // in `run_tui`/`run_tui_watch` any more (finding 3) --
        // `prev_render_state` starts exactly like this.
        let mut prev_render_state = None;
        let mut highlights = highlight::Tracker::new();
        mark_highlights(
            &mut highlights,
            &mut prev_render_state,
            &first_real_build,
            false,
        );
        assert!(
            !highlights.has_active(Instant::now()),
            "attaching to an already-progressed run must open on a calm screen"
        );

        // A real, later change against that first real baseline does
        // still mark -- the fix isn't "nothing ever highlights".
        let next_state = serde_json::json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "x", "meta": {"created_at": "t0", "completed_at": "t1"}},
            "b": {"value": "y", "meta": {"created_at": "t1", "completed_at": "t2"}}
        }});
        let second_real_build = oscilloscope_core::render::build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&next_state),
            ProcessState::Running,
            2.0,
            "t2",
            &[],
        );
        mark_highlights(
            &mut highlights,
            &mut prev_render_state,
            &second_real_build,
            false,
        );
        assert!(
            highlights.has_active(Instant::now()),
            "a real change against the first real baseline must still mark"
        );
    }

    /// Issue #535: the real call sequence `osi watch` takes attaching
    /// to a run that's already finished -- a catch-up drain pushes
    /// every log line the run ever produced before the loop's first
    /// real rebuild runs at all, so `prev_log_total` is still 0 (its
    /// own startup value) when `next_log_seq` already reflects the
    /// whole backlog. `mark_new_log_lines` must take its
    /// `baseline_exists` from the same `prev_render_state.is_some()`
    /// check made *before* `mark_highlights` runs, same as the real
    /// loops do, or the backlog highlights on open.
    #[test]
    fn watching_a_finished_run_opens_calm_even_with_lines_already_in_the_log() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let finished = serde_json::json!({"prime": {"value": "x", "meta": {"completed_at": "t0"}}});
        let render_state = oscilloscope_core::render::build(
            "do-thing.yml",
            "watch",
            &plan,
            &mut model,
            Some(&finished),
            ProcessState::Exited {
                interrupted: false,
                likely_failed: false,
                has_failure_reason: false,
                exited_at: None,
            },
            1.0,
            "t1",
            &[],
        );

        let mut highlights = highlight::Tracker::new();
        let mut prev_render_state = None;
        let mut prev_log_total = 0u64;
        // The catch-up drain's own three lines, already pushed before
        // this first rebuild ever runs.
        let next_log_seq = 3u64;

        let baseline_exists = prev_render_state.is_some();
        mark_highlights(
            &mut highlights,
            &mut prev_render_state,
            &render_state,
            false,
        );
        mark_new_log_lines(
            &mut highlights,
            &mut prev_log_total,
            next_log_seq,
            false,
            baseline_exists,
        );
        let now = Instant::now();
        assert!(
            !highlights.has_active(now),
            "a finished run's own replayed log lines must not highlight on open"
        );
        assert_eq!(prev_log_total, 3);

        // A line pushed after the first frame still highlights.
        let baseline_exists = prev_render_state.is_some();
        mark_highlights(
            &mut highlights,
            &mut prev_render_state,
            &render_state,
            false,
        );
        mark_new_log_lines(
            &mut highlights,
            &mut prev_log_total,
            4,
            false,
            baseline_exists,
        );
        assert!(
            highlights
                .style_for(
                    "log:3",
                    now,
                    &theme::Theme::for_name(theme::ThemeName::Terminal),
                    theme::ColorMode::TrueColor
                )
                .is_some(),
            "a line pushed after the first frame must still highlight"
        );
    }

    /// Finding 4: `mark_new_log_lines` marks every sequence number
    /// from the previous rebuild's total up to the new one, and
    /// nothing retroactively once a key has already been marked.
    /// `baseline_exists` is `true` throughout here -- a later rebuild
    /// against an already-established baseline; the catch-up case
    /// (`baseline_exists: false`, issue #535) has its own test below.
    #[test]
    fn mark_new_log_lines_marks_only_the_newly_pushed_sequence_numbers() {
        let mut highlights = highlight::Tracker::new();
        let mut prev_total = 0u64;
        mark_new_log_lines(&mut highlights, &mut prev_total, 3, false, true);
        let now = Instant::now();
        assert!(
            highlights
                .style_for(
                    "log:0",
                    now,
                    &theme::Theme::for_name(theme::ThemeName::Terminal),
                    theme::ColorMode::TrueColor
                )
                .is_some()
        );
        assert!(
            highlights
                .style_for(
                    "log:2",
                    now,
                    &theme::Theme::for_name(theme::ThemeName::Terminal),
                    theme::ColorMode::TrueColor
                )
                .is_some()
        );
        assert_eq!(prev_total, 3);

        // A second call only marks what's new since the last one.
        mark_new_log_lines(&mut highlights, &mut prev_total, 5, false, true);
        assert!(
            highlights
                .style_for(
                    "log:3",
                    now,
                    &theme::Theme::for_name(theme::ThemeName::Terminal),
                    theme::ColorMode::TrueColor
                )
                .is_some()
        );
        assert_eq!(prev_total, 5);

        // `--no-animation` still advances `prev_total`, marks nothing.
        let mut highlights_off = highlight::Tracker::new();
        let mut prev_total_off = 0u64;
        mark_new_log_lines(&mut highlights_off, &mut prev_total_off, 2, true, true);
        assert!(!highlights_off.has_active(now));
        assert_eq!(prev_total_off, 2);
    }

    /// Issue #535: with no baseline yet (the loop's own first real
    /// rebuild, same condition `mark_highlights` tests via its own
    /// `prev`), `mark_new_log_lines` still advances `prev_total` but
    /// marks nothing -- the catch-up drain that ran before this first
    /// rebuild (an already-progressed or already-finished run's own
    /// backlog) must open calm, same as the render-state diff does.
    #[test]
    fn mark_new_log_lines_is_silent_on_the_first_rebuild_with_no_baseline_yet() {
        let mut highlights = highlight::Tracker::new();
        let mut prev_total = 0u64;
        mark_new_log_lines(&mut highlights, &mut prev_total, 5, false, false);
        let now = Instant::now();
        assert!(
            !highlights.has_active(now),
            "no baseline yet: the catch-up backlog must not mark"
        );
        assert_eq!(prev_total, 5);

        // Once a baseline exists, a line pushed after it still marks.
        mark_new_log_lines(&mut highlights, &mut prev_total, 6, false, true);
        assert!(
            highlights
                .style_for(
                    "log:5",
                    now,
                    &theme::Theme::for_name(theme::ThemeName::Terminal),
                    theme::ColorMode::TrueColor
                )
                .is_some(),
            "a line pushed after the first frame must still highlight"
        );
        assert_eq!(prev_total, 6);
    }

    /// Finding 5: `details:sent`/`details:received` mark only when
    /// the *same* selected path's blocks actually changed, never on
    /// a bare selection change to a different path.
    #[test]
    fn mark_details_highlights_ignores_a_bare_selection_change() {
        let plan = PlanTree::empty();
        let ask_running = serde_json::json!({"prime": {"value": null, "meta": {"completed_at": null},
            "ask": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "prompt_sent": "hello"}},
            "other": {"value": "x", "meta": {"created_at": "t0", "completed_at": "t1"}}
        }});
        let ask_done = serde_json::json!({"prime": {"value": null, "meta": {"completed_at": null},
            "ask": {"value": "hi", "meta": {"created_at": "t0", "completed_at": "t1", "prompt_sent": "hello"}},
            "other": {"value": "x", "meta": {"created_at": "t0", "completed_at": "t1"}}
        }});
        let mut highlights = highlight::Tracker::new();
        let mut prev = None;
        mark_details_highlights(
            &mut highlights,
            &mut prev,
            Some("prime.ask"),
            &plan,
            &RunModel::new(),
            Some(&ask_running),
            false,
        );
        assert!(
            !highlights.has_active(Instant::now()),
            "the first observation never marks"
        );

        // Selecting a *different* path never compares against `ask`'s
        // own previous blocks, even though they did change underneath.
        mark_details_highlights(
            &mut highlights,
            &mut prev,
            Some("prime.other"),
            &plan,
            &RunModel::new(),
            Some(&ask_done),
            false,
        );
        assert!(
            !highlights.has_active(Instant::now()),
            "a bare selection change must never mark details:sent/received"
        );

        // Selecting `ask` again, now that it finished, does mark --
        // the path matches the one just tracked for it below.
        mark_details_highlights(
            &mut highlights,
            &mut prev,
            Some("prime.ask"),
            &plan,
            &RunModel::new(),
            Some(&ask_running),
            false,
        );
        mark_details_highlights(
            &mut highlights,
            &mut prev,
            Some("prime.ask"),
            &plan,
            &RunModel::new(),
            Some(&ask_done),
            false,
        );
        assert!(
            highlights.has_active(Instant::now()),
            "the same selected path's own changed blocks must mark"
        );
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
    fn run_args_help_describes_every_argument() {
        // Issue #433: `osi <doc>`'s own arguments had no --help
        // descriptions at all, unlike `osi watch`'s.
        let err = RunArgs::try_parse_from(args(&["--help"])).unwrap_err();
        let rendered = err.to_string();
        assert!(rendered.contains("The orchestration document to run"));
        assert!(rendered.contains("passed through to the engine unchanged"));
        assert!(rendered.contains("Which engine runs the orchestration"));
        assert!(rendered.contains("Puts the run directory"));
        assert!(rendered.contains("Forces the plain-text stream"));
        assert!(rendered.contains("Shows only the header line"));
        assert!(rendered.contains("Switches off the update highlights"));
        assert!(rendered.contains("The colour theme"));
        assert!(rendered.contains("Turns run history off"));
        // Finding 10: `--theme`'s possible values (a derived
        // `ValueEnum`) are listed in `--help`, not just its own
        // description text.
        assert!(rendered.contains("dracula"), "{rendered}");
    }

    #[test]
    fn watch_args_help_describes_glance_and_once() {
        let err = WatchArgs::try_parse_from(
            std::iter::once("osi watch".to_string()).chain(["--help".to_string()]),
        )
        .unwrap_err();
        let rendered = err.to_string();
        assert!(rendered.contains("Shows only the header line"));
        assert!(rendered.contains("prints the current line once and exits 0"));
        assert!(rendered.contains("Turns run history off"));
    }

    #[test]
    fn no_animation_parses_on_both_run_and_watch_args() {
        let parsed = RunArgs::try_parse_from(args(&["--no-animation", "do-thing.yml"]))
            .expect("--no-animation should parse");
        assert!(parsed.no_animation);

        let parsed = WatchArgs::try_parse_from(
            std::iter::once("osi watch".to_string())
                .chain(["--no-animation".to_string(), "dir".to_string()]),
        )
        .expect("--no-animation should parse on watch too");
        assert!(parsed.no_animation);
    }

    /// Finding 15: an explicit icon test for `--no-animation` -- the
    /// still frame (0) regardless of how long the run's been live,
    /// versus a real frame number once animation is on.
    #[test]
    fn no_animation_pins_the_icon_at_frame_zero() {
        assert_eq!(icon_frame_for(true, Duration::from_secs(5)), 0);
        assert_eq!(icon_frame_for(true, Duration::from_secs(50)), 0);
        assert!(icon_frame_for(false, Duration::from_millis(500)) > 0);
    }

    #[test]
    fn theme_flag_parses_a_known_name_and_rejects_an_unknown_one() {
        let parsed = RunArgs::try_parse_from(args(&["--theme", "dracula", "do-thing.yml"]))
            .expect("a known theme name should parse");
        assert_eq!(parsed.theme, Some(theme::ThemeName::Dracula));

        let err = RunArgs::try_parse_from(args(&["--theme", "bogus", "do-thing.yml"]))
            .expect_err("an unknown theme name is a usage error");
        // Finding 10: `--theme` is now a derived `ValueEnum`
        // (clap's own usage error, not `ThemeName::parse`'s), so
        // every valid name is listed right there in the error too.
        assert!(err.to_string().contains("dracula"), "{err}");
    }

    #[test]
    fn resolve_theme_choice_prefers_the_cli_flag_over_osi_theme() {
        // Finding 13: `resolve_theme_choice` takes its own
        // `OSI_THEME` reading as a plain argument now, so this test
        // never touches the process environment at all.
        assert_eq!(
            resolve_theme_choice(Some(theme::ThemeName::Dracula), Some("nord")),
            Ok(theme::ThemeName::Dracula)
        );
        assert_eq!(
            resolve_theme_choice(None, Some("nord")),
            Ok(theme::ThemeName::Nord)
        );
        assert_eq!(
            resolve_theme_choice(None, None),
            Ok(theme::ThemeName::Terminal)
        );
        assert_eq!(
            resolve_theme_choice(None, Some("")),
            Ok(theme::ThemeName::Terminal)
        );
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
        // then leaves an `osi-*` temp directory behind.
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
        // osi-* directories into a shared /tmp on every cargo test.
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
        // becomes true. `osi watch` must still notice, from the
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
            run(std::iter::once("osi".to_string()).chain([
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
                "osi watch hung instead of noticing the dead pid"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(handle.join().unwrap(), ExitCode::from(1));
    }

    #[test]
    fn watch_reports_the_signal_run_end_named_not_just_ok_vs_failed() {
        // P2-7: a run_end event names the real signal that ended the
        // run (DESIGN.md §3's format table); osi watch should report
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
    fn finish_watch_reports_a_distinct_detached_ending_for_a_voluntary_q_on_a_live_run() {
        // Finding 15: `q` on a run that's still going is a voluntary
        // detach, not an abort — reported distinctly, and exits 0,
        // rather than the generic "no final state" abort every other
        // ending without one falls into.
        let dir = tempfile::tempdir().unwrap();
        let mut live_poller = LiveStatePoller::new(dir.path().join("state.live.json"));
        let mut events_tailer = EventsTailer::new(dir.path().join("events.jsonl"));
        let mut differ = Differ::new();
        let mut model = RunModel::new();
        let plan = PlanTree::empty();
        let mut last_state: Option<serde_json::Value> = None;
        let mut clock = Clock::new();
        let mut buf: Vec<u8> = Vec::new();

        let code = finish_watch(
            &mut buf,
            &mut clock,
            &mut live_poller,
            &mut events_tailer,
            &mut differ,
            &mut model,
            &plan,
            dir.path(),
            &mut last_state,
            None,
            true,
            &history_io::HistoryHandle::for_watch(None, None),
        );
        assert_eq!(code, 0);
        let text = String::from_utf8(buf).unwrap();
        assert!(
            text.contains("■ detached (the run is still going)"),
            "{text:?}"
        );
    }

    #[test]
    fn drain_until_ended_shows_ok_with_every_effect_counted_after_a_fast_exit() {
        // #494: an engine double that writes its last events and its
        // final state only *after* the last observation this test
        // takes before calling the fix under test -- exactly the
        // shape DESIGN.md §1.1 describes for a real engine ("writes
        // its last events and its final state and exits at once").
        // `run_tui`'s own loop used to never observe anything again
        // once it had seen the engine's exit; `drain_until_ended` is
        // the one read every path that reaches a run's end now takes
        // instead.
        let dir = tempfile::tempdir().unwrap();
        let plan = PlanTree::empty();
        let mut live_poller = LiveStatePoller::new(dir.path().join("state.live.json"));
        let mut events_tailer = EventsTailer::new(dir.path().join("events.jsonl"));
        let mut differ = Differ::new();
        let mut model = RunModel::new();

        // A tick taken while the run directory is still empty -- the
        // same as every tick `run_tui`'s own loop takes while the
        // engine is still running.
        let (lines, state) = observe_tick(
            &mut live_poller,
            &mut events_tailer,
            &mut differ,
            &mut model,
            &plan,
        );
        assert!(lines.is_empty());
        assert!(state.is_none());

        // The engine writes everything and exits, strictly after
        // that last observation -- nothing else polls these files
        // again in between, exactly like the real gap (#494).
        std::fs::write(
            dir.path().join("events.jsonl"),
            "{\"v\":1,\"seq\":0,\"ts\":\"t\",\"ev\":\"run_start\",\"run_id\":\"r\"}\n\
             {\"v\":1,\"seq\":1,\"ts\":\"t\",\"ev\":\"start\",\"id\":1,\"path\":\"prime.hello\"}\n\
             {\"v\":1,\"seq\":2,\"ts\":\"t\",\"ev\":\"end\",\"id\":1,\"path\":\"prime.hello\",\"ok\":true,\"ms\":1}\n\
             {\"v\":1,\"seq\":3,\"ts\":\"t\",\"ev\":\"run_end\",\"ok\":true}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("state.json"),
            r#"{"runtime":{"last_run":{"completed_at":"t","totals":{"wall_time_s":0.01}}},"prime":{"value":true,"meta":{"completed_at":"t","error":null},"hello":{"value":"hi\n","meta":{"completed_at":"t","error":null,"exit_code":0,"provider":"shell"}}}}"#,
        )
        .unwrap();

        let spec = RunSpec {
            orchestration: dir.path().join("do.yml"),
            config: None,
            sets: vec![],
            run_dir: dir.path().to_path_buf(),
        };

        let (_lines, final_state, _) = drain_until_ended(
            &spec.out_path(),
            &mut live_poller,
            &mut events_tailer,
            &mut differ,
            &mut model,
            &plan,
        );
        let state = final_state.expect("drain_until_ended should read the final --out write");

        let render_state = oscilloscope_core::render::build(
            "do.yml",
            "electricity",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Exited {
                interrupted: false,
                likely_failed: false,
                has_failure_reason: false,
                exited_at: None,
            },
            0.1,
            "2024-01-01T00:00:00Z",
            &[],
        );
        assert_eq!(render_state.header.status, RunStatus::Ok);
        // The root counts too (issue #620), alongside `hello`.
        assert_eq!(render_state.header.effects_done, 2);
    }

    #[test]
    fn drain_until_ended_shows_failed_with_the_error_after_a_fast_exit() {
        // [`drain_until_ended_shows_ok_with_every_effect_counted_after_a_fast_exit`],
        // for a run that failed instead.
        let dir = tempfile::tempdir().unwrap();
        let plan = PlanTree::empty();
        let mut live_poller = LiveStatePoller::new(dir.path().join("state.live.json"));
        let mut events_tailer = EventsTailer::new(dir.path().join("events.jsonl"));
        let mut differ = Differ::new();
        let mut model = RunModel::new();

        let (lines, state) = observe_tick(
            &mut live_poller,
            &mut events_tailer,
            &mut differ,
            &mut model,
            &plan,
        );
        assert!(lines.is_empty());
        assert!(state.is_none());

        std::fs::write(
            dir.path().join("events.jsonl"),
            "{\"v\":1,\"seq\":0,\"ts\":\"t\",\"ev\":\"run_start\",\"run_id\":\"r\"}\n\
             {\"v\":1,\"seq\":1,\"ts\":\"t\",\"ev\":\"start\",\"id\":1,\"path\":\"prime.boom\"}\n\
             {\"v\":1,\"seq\":2,\"ts\":\"t\",\"ev\":\"end\",\"id\":1,\"path\":\"prime.boom\",\"ok\":false,\"ms\":1,\"error\":\"boom failed\"}\n\
             {\"v\":1,\"seq\":3,\"ts\":\"t\",\"ev\":\"run_end\",\"ok\":false,\"error\":\"boom: boom failed\"}\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("state.json"),
            r#"{"runtime":{"last_run":{"completed_at":"t","totals":{"wall_time_s":0.01}}},"prime":{"value":false,"meta":{"completed_at":"t","error":"boom: boom failed"},"boom":{"value":null,"meta":{"completed_at":"t","error":"boom failed","provider":"shell"}}}}"#,
        )
        .unwrap();

        let spec = RunSpec {
            orchestration: dir.path().join("do.yml"),
            config: None,
            sets: vec![],
            run_dir: dir.path().to_path_buf(),
        };

        let (lines, final_state, _) = drain_until_ended(
            &spec.out_path(),
            &mut live_poller,
            &mut events_tailer,
            &mut differ,
            &mut model,
            &plan,
        );
        let state = final_state.expect("drain_until_ended should read the final --out write");

        // Review finding 3 (PR #496): the drain's own diff lines must
        // carry the failure, not just the fixture this test wrote by
        // hand straight into `state.json` -- a check against that
        // fixture alone would pass even with no drain at all.
        assert!(
            lines.iter().any(|l| l.text.contains("boom failed")),
            "{lines:?}"
        );

        let render_state = oscilloscope_core::render::build(
            "do.yml",
            "electricity",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Exited {
                interrupted: false,
                likely_failed: true,
                has_failure_reason: true,
                exited_at: None,
            },
            0.1,
            "2024-01-01T00:00:00Z",
            &[],
        );
        assert_eq!(render_state.header.status, RunStatus::Failed);
        // The root counts too (issue #620), alongside `boom`.
        assert_eq!(render_state.header.effects_done, 2);

        let details =
            oscilloscope_core::render::details_for("prime.boom", &plan, &model, Some(&state));
        assert_eq!(details.error.as_deref(), Some("boom failed"));
    }

    #[test]
    fn drain_until_ended_never_drops_a_held_end_line() {
        // #545's review finding 3: a held `End` line must not be
        // abandoned just because the drain loop itself reached a
        // quiet pass before the token counts ever showed up over
        // `--live-state` -- the run's authoritative final `--out`
        // read, taken right after this loop stops, has them, and
        // `flush_pending_ends` must resolve the line from that read
        // rather than letting it fall back to `ok` or vanish.
        let dir = tempfile::tempdir().unwrap();
        let plan = PlanTree::empty();
        let mut live_poller = LiveStatePoller::new(dir.path().join("state.live.json"));
        let mut events_tailer = EventsTailer::new(dir.path().join("events.jsonl"));
        let mut differ = Differ::new();
        let mut model = RunModel::new();

        // The engine's `end` event for `prime.ask` lands with no
        // `--live-state` ever written for it at all: `known_node`
        // has nothing, so `diff_event` holds the line.
        std::fs::write(
            dir.path().join("events.jsonl"),
            "{\"v\":1,\"seq\":0,\"ts\":\"t\",\"ev\":\"run_start\",\"run_id\":\"r\"}\n\
             {\"v\":1,\"seq\":1,\"ts\":\"t\",\"ev\":\"start\",\"id\":1,\"path\":\"prime.ask\"}\n\
             {\"v\":1,\"seq\":2,\"ts\":\"t\",\"ev\":\"end\",\"id\":1,\"path\":\"prime.ask\",\"ok\":true,\"ms\":100}\n\
             {\"v\":1,\"seq\":3,\"ts\":\"t\",\"ev\":\"run_end\",\"ok\":true}\n",
        )
        .unwrap();
        // Only the final `--out` ever carries `prime.ask`'s tokens --
        // no `state.live.json` is written at all in this fixture, the
        // sharpest form of the gap: the drain loop's own poll never
        // sees the path either.
        std::fs::write(
            dir.path().join("state.json"),
            r#"{"runtime":{"last_run":{"completed_at":"t","totals":{"wall_time_s":0.1}}},"prime":{"value":true,"meta":{"completed_at":"t","error":null},"ask":{"value":"hello back","meta":{"created_at":"t","completed_at":"t","error":null,"tokens_sent":3,"tokens_received":5}}}}"#,
        )
        .unwrap();

        let spec = RunSpec {
            orchestration: dir.path().join("do.yml"),
            config: None,
            sets: vec![],
            run_dir: dir.path().to_path_buf(),
        };

        let (lines, final_state, _) = drain_until_ended(
            &spec.out_path(),
            &mut live_poller,
            &mut events_tailer,
            &mut differ,
            &mut model,
            &plan,
        );
        assert!(final_state.is_some());
        assert!(!differ.has_pending_ends());
        assert!(
            lines
                .iter()
                .any(|l| l.text == "✓ prime.ask  0.1s  ↑3 t ↓5 t"),
            "held line must resolve with its real tokens, not ok or nothing: {lines:?}"
        );
    }

    #[test]
    fn drain_until_ended_falls_back_to_the_live_snapshot_it_polled_when_out_path_has_nothing() {
        // Review finding 1 on PR #496: `osi watch <dir | live.json>`
        // (DESIGN.md Q5) can point at a run osi didn't start, where
        // `out_path` (`run_dir.join("state.json")`) is never written
        // at all -- `drain_until_ended` must still surface the final
        // live write its own loop just polled, rather than discarding
        // it the moment the loop ends, which is what left a run like
        // this stuck showing `aborted` forever.
        let dir = tempfile::tempdir().unwrap();
        let plan = PlanTree::empty();
        let mut live_poller = LiveStatePoller::new(dir.path().join("state.live.json"));
        let mut events_tailer = EventsTailer::new(dir.path().join("events.jsonl"));
        let mut differ = Differ::new();
        let mut model = RunModel::new();

        let (lines, state) = observe_tick(
            &mut live_poller,
            &mut events_tailer,
            &mut differ,
            &mut model,
            &plan,
        );
        assert!(lines.is_empty());
        assert!(state.is_none());

        std::fs::write(
            dir.path().join("events.jsonl"),
            "{\"v\":1,\"seq\":0,\"ts\":\"t\",\"ev\":\"run_start\",\"run_id\":\"r\"}\n\
             {\"v\":1,\"seq\":1,\"ts\":\"t\",\"ev\":\"start\",\"id\":1,\"path\":\"prime.hello\"}\n\
             {\"v\":1,\"seq\":2,\"ts\":\"t\",\"ev\":\"end\",\"id\":1,\"path\":\"prime.hello\",\"ok\":true,\"ms\":1}\n\
             {\"v\":1,\"seq\":3,\"ts\":\"t\",\"ev\":\"run_end\",\"ok\":true}\n",
        )
        .unwrap();
        // Only `state.live.json` is ever written -- no `state.json`
        // sibling, exactly the case `out_path` can't read.
        std::fs::write(
            dir.path().join("state.live.json"),
            r#"{"runtime":{"last_run":{"completed_at":"t","totals":{"wall_time_s":0.01}}},"prime":{"value":true,"meta":{"completed_at":"t","error":null},"hello":{"value":"hi\n","meta":{"completed_at":"t","error":null,"exit_code":0,"provider":"shell"}}}}"#,
        )
        .unwrap();

        let (_lines, out_state, last_live) = drain_until_ended(
            &dir.path().join("state.json"),
            &mut live_poller,
            &mut events_tailer,
            &mut differ,
            &mut model,
            &plan,
        );
        assert!(out_state.is_none(), "no state.json exists for this run");
        let state = last_live.expect("the drain should have kept its own polled live snapshot");

        let render_state = oscilloscope_core::render::build(
            "do.yml",
            "electricity",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Exited {
                interrupted: false,
                likely_failed: false,
                has_failure_reason: false,
                exited_at: None,
            },
            0.1,
            "2024-01-01T00:00:00Z",
            &[],
        );
        assert_eq!(render_state.header.status, RunStatus::Ok);
        // The root counts too (issue #620), alongside `hello`.
        assert_eq!(render_state.header.effects_done, 2);
    }

    #[test]
    fn push_log_line_keeps_only_the_most_recent_max_log_lines() {
        // Finding 13: the TUI's own log buffer has nothing else
        // bounding it for as long as osi keeps running.
        let mut log_lines: Vec<String> = Vec::new();
        let mut next_log_seq: u64 = 0;
        for n in 0..MAX_LOG_LINES + 5 {
            push_log_line(&mut log_lines, &mut next_log_seq, format!("line {n}"));
        }
        assert_eq!(log_lines.len(), MAX_LOG_LINES);
        assert_eq!(log_lines.first().unwrap(), "line 5");
        assert_eq!(
            log_lines.last().unwrap(),
            &format!("line {}", MAX_LOG_LINES + 4)
        );
        // Finding 4: the sequence count keeps going regardless of
        // the trim -- it's the key's own stable identity, not the
        // buffer's own length.
        assert_eq!(next_log_seq, (MAX_LOG_LINES + 5) as u64);
    }

    fn utc_zone() -> Box<dyn oscilloscope_core::format::ClockZone> {
        Box::new(chrono::FixedOffset::east_opt(0).unwrap())
    }

    #[test]
    fn clock_formats_hh_mm_ss() {
        let utc = chrono::FixedOffset::east_opt(0).unwrap();
        let dt = chrono::DateTime::parse_from_rfc3339("2026-10-08T19:56:22Z")
            .unwrap()
            .with_timezone(&utc);
        assert_eq!(Clock::format(dt), "19:56:22");
    }

    #[test]
    fn clock_time_for_uses_the_lines_own_timestamp_in_the_given_zone() {
        let mut clock = Clock::new_with_zone(utc_zone());
        let dt = clock.clock_time_for(Some("2026-10-08T19:56:22.000Z"));
        assert_eq!(Clock::format(dt), "19:56:22");
    }

    #[test]
    fn clock_never_prints_an_earlier_clock_time_than_it_already_has() {
        // K5: a backdated line (DESIGN.md §2.4's "new node that is
        // already complete" row, or events and state simply landing a
        // tick apart) can carry a `ts` earlier than one the clock has
        // already shown — the printed `HH:MM:SS` must never visibly
        // run backwards even then.
        let mut clock = Clock::new_with_zone(utc_zone());
        let first = clock.clock_time_for(Some("2026-10-08T19:56:25.000Z"));
        assert_eq!(Clock::format(first), "19:56:25");
        let later = clock.clock_time_for(Some("2026-10-08T19:56:28.000Z"));
        assert_eq!(Clock::format(later), "19:56:28");
        let backdated = clock.clock_time_for(Some("2026-10-08T19:56:22.000Z"));
        assert_eq!(
            backdated, later,
            "a backdated ts must not move the display backwards"
        );
    }

    #[test]
    fn clock_time_for_a_nonzero_offset_resolves_the_local_calendar_time() {
        let plus_530 = chrono::FixedOffset::east_opt(5 * 3600 + 30 * 60).unwrap();
        let mut clock = Clock::new_with_zone(Box::new(plus_530));
        let dt = clock.clock_time_for(Some("2026-10-08T19:56:22Z"));
        assert_eq!(Clock::format(dt), "01:26:22");
    }

    #[test]
    fn effective_log_mode_is_true_when_explicitly_requested() {
        assert!(effective_log_mode(true));
    }

    #[test]
    fn engine_capability_notice_reports_a_timed_out_probe_distinctly_from_no_events() {
        // Review finding 6 on #458: the timed-out probe's own notice
        // text was untested.
        let timed_out = EngineCaps {
            events: false,
            live_state: true,
            missing: false,
            timed_out: true,
        };
        assert_eq!(
            engine_capability_notice(&timed_out, "cof").as_deref(),
            Some("osi: cof --help didn't answer in time; running from state only")
        );

        let no_events = EngineCaps {
            events: false,
            live_state: true,
            missing: false,
            timed_out: false,
        };
        assert_eq!(
            engine_capability_notice(&no_events, "cof").as_deref(),
            Some("osi: this cof has no --events: running from state only")
        );

        let fully_capable = EngineCaps {
            events: true,
            live_state: true,
            missing: false,
            timed_out: false,
        };
        assert_eq!(engine_capability_notice(&fully_capable, "cof"), None);

        let missing = EngineCaps {
            events: false,
            live_state: false,
            missing: true,
            timed_out: false,
        };
        assert_eq!(
            engine_capability_notice(&missing, "cof"),
            None,
            "a missing binary gets its own, more specific error once the spawn fails"
        );
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
        // The default directory's name (`osi-<pid>-<nanos>`) is
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
        // an unrequested change to something outside osi's own run
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
