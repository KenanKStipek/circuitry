//! `trait Engine { fn command(&RunSpec) -> Command; fn caps() }` and its
//! two implementations, `CofEngine` and `ElectricityEngine` (DESIGN.md
//! §4 and §6.1).

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// What one run needs, independent of which engine runs it (DESIGN.md
/// §4): the document, an optional config, inline `-e key=value` sets
/// passed through untouched, and the run directory that holds
/// `state.live.json`/`state.json`/`events.jsonl`/`stdout.txt`/`stderr.txt`.
#[derive(Debug, Clone)]
pub struct RunSpec {
    pub orchestration: PathBuf,
    pub config: Option<PathBuf>,
    pub sets: Vec<String>,
    pub run_dir: PathBuf,
}

impl RunSpec {
    pub fn live_state_path(&self) -> PathBuf {
        self.run_dir.join("state.live.json")
    }

    pub fn out_path(&self) -> PathBuf {
        self.run_dir.join("state.json")
    }

    pub fn events_path(&self) -> PathBuf {
        self.run_dir.join("events.jsonl")
    }

    pub fn stdout_path(&self) -> PathBuf {
        self.run_dir.join("stdout.txt")
    }

    pub fn stderr_path(&self) -> PathBuf {
        self.run_dir.join("stderr.txt")
    }
}

/// What this engine binary can do, detected once at startup (the
/// orchestrator decision behind issue #424: detect `--events` support
/// from `cof run --help` rather than assuming a version; issue #431's
/// lane E2 detects `--live-state` the same way from `electricity
/// --help`, since an older electricity built before M0-H has neither
/// flag).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EngineCaps {
    pub events: bool,
    /// `cof run` has had `--live-state` unconditionally since before
    /// capability detection existed at all (`CofEngine::command` below
    /// always passes it), so `CofEngine`'s own caps hardcode this
    /// `true` rather than detecting it; electricity detects it for
    /// real, since M0-H added both flags together and an older build
    /// has neither.
    pub live_state: bool,
    /// Set when the `--help` probe itself couldn't find the binary at
    /// all (M4): lets a caller skip the "this cof has no --events"
    /// notice in that case, which otherwise printed -- wrongly --
    /// ahead of the real "couldn't launch cof" error the later spawn
    /// goes on to report for the exact same reason.
    pub missing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineError {
    /// electricity has no config discovery (DESIGN.md §4.2): osp
    /// refuses rather than inventing an empty `{}` config, which would
    /// silently drop settings `cof` would have applied.
    MissingConfig(String),
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EngineError::MissingConfig(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for EngineError {}

pub trait Engine {
    fn name(&self) -> &'static str;
    fn caps(&self) -> EngineCaps;
    fn command(&self, spec: &RunSpec) -> Result<Command, EngineError>;
}

/// `cof run <doc> [--config <cfg>] --quiet --live-state <dir>/state.live.json
/// --out <dir>/state.json [--events <dir>/events.jsonl] [-e k=v]...`
/// (DESIGN.md §4.1).
pub struct CofEngine {
    binary: String,
    caps: EngineCaps,
}

impl CofEngine {
    /// Detects `--events` support by running `<binary> run --help` once
    /// and looking for the flag's own name in its output — the same
    /// detection serves a `cof` built before #419 lands and any older
    /// release a user still has on `PATH`.
    pub fn detect(binary: impl Into<String>) -> Self {
        use std::os::unix::process::CommandExt;

        let binary = binary.into();
        let mut cmd = std::process::Command::new(&binary);
        cmd.args(["run", "--help"]);
        // P2-10: its own process group, same as the real engine spawn
        // (supervise.rs), so a terminal Ctrl-C landing while this
        // probe is still running doesn't kill it directly — that used
        // to make `.output()` return early/incomplete, which
        // `.unwrap_or(false)` then read as "no --events support" and
        // silently ran the real engine state-only even on a `cof` that
        // actually has the flag.
        cmd.process_group(0);
        let output = cmd.output();
        let missing = matches!(&output, Err(e) if e.kind() == std::io::ErrorKind::NotFound);
        let events = output
            .map(|out| {
                let text = String::from_utf8_lossy(&out.stdout);
                let err_text = String::from_utf8_lossy(&out.stderr);
                text.contains("--events") || err_text.contains("--events")
            })
            .unwrap_or(false);
        CofEngine {
            binary,
            caps: EngineCaps {
                events,
                live_state: true,
                missing,
            },
        }
    }

    pub fn with_caps(binary: impl Into<String>, caps: EngineCaps) -> Self {
        CofEngine {
            binary: binary.into(),
            caps,
        }
    }
}

impl Engine for CofEngine {
    fn name(&self) -> &'static str {
        "cof"
    }

    fn caps(&self) -> EngineCaps {
        self.caps
    }

    fn command(&self, spec: &RunSpec) -> Result<Command, EngineError> {
        let mut cmd = Command::new(&self.binary);
        cmd.arg("run").arg(&spec.orchestration);
        if let Some(config) = &spec.config {
            cmd.arg("--config").arg(config);
        }
        cmd.arg("--quiet");
        cmd.arg("--live-state").arg(spec.live_state_path());
        cmd.arg("--out").arg(spec.out_path());
        if self.caps.events {
            cmd.arg("--events").arg(spec.events_path());
        }
        for kv in &spec.sets {
            cmd.arg("-e").arg(kv);
        }
        Ok(cmd)
    }
}

/// `electricity <config.json> <orchestration.yml> -e k=v... --out
/// <dir>/state.json [--events <dir>/events.jsonl] [--live-state
/// <dir>/state.live.json]` (DESIGN.md §4.2, issue #431's lane E2):
/// M0-H gave electricity both flags together, so an older build on
/// `PATH` from before that lane has neither — osp falls back to
/// supervising it with no observation at all in that case, the same
/// no-events path `CofEngine` already falls back to, rather than
/// passing a flag the binary doesn't understand.
pub struct ElectricityEngine {
    binary: String,
    caps: EngineCaps,
}

impl ElectricityEngine {
    /// No capability detection: every caller that cares which flags
    /// this binary supports uses [`ElectricityEngine::detect`]
    /// instead. Kept for a caller that only needs `command`'s own
    /// argument shape (e.g. `looks_swapped`'s own tests), with every
    /// flag left off, same as the pre-M0-H behaviour this replaces.
    pub fn new(binary: impl Into<String>) -> Self {
        ElectricityEngine {
            binary: binary.into(),
            caps: EngineCaps::default(),
        }
    }

    /// Detects `--events`/`--live-state` support by running
    /// `<binary> --help` once and looking for each flag's own name in
    /// its output — the same probe shape as [`CofEngine::detect`],
    /// including its own process group (so a terminal Ctrl-C landing
    /// mid-probe doesn't kill it directly and get misread as "no
    /// support").
    pub fn detect(binary: impl Into<String>) -> Self {
        use std::os::unix::process::CommandExt;

        let binary = binary.into();
        let mut cmd = std::process::Command::new(&binary);
        cmd.arg("--help");
        cmd.process_group(0);
        let output = cmd.output();
        let missing = matches!(&output, Err(e) if e.kind() == std::io::ErrorKind::NotFound);
        let (events, live_state) = output
            .map(|out| {
                let text = String::from_utf8_lossy(&out.stdout);
                let err_text = String::from_utf8_lossy(&out.stderr);
                let has = |flag: &str| text.contains(flag) || err_text.contains(flag);
                (has("--events"), has("--live-state"))
            })
            .unwrap_or((false, false));
        ElectricityEngine {
            binary,
            caps: EngineCaps {
                events,
                live_state,
                missing,
            },
        }
    }

    pub fn with_caps(binary: impl Into<String>, caps: EngineCaps) -> Self {
        ElectricityEngine {
            binary: binary.into(),
            caps,
        }
    }
}

impl Engine for ElectricityEngine {
    fn name(&self) -> &'static str {
        "electricity"
    }

    fn caps(&self) -> EngineCaps {
        self.caps
    }

    fn command(&self, spec: &RunSpec) -> Result<Command, EngineError> {
        let Some(config) = &spec.config else {
            return Err(EngineError::MissingConfig(
                "electricity needs a config: osp <orchestration> <config.json> --engine electricity"
                    .to_string(),
            ));
        };
        let mut cmd = Command::new(&self.binary);
        cmd.arg(config).arg(&spec.orchestration);
        for kv in &spec.sets {
            cmd.arg("-e").arg(kv);
        }
        cmd.arg("--out").arg(spec.out_path());
        if self.caps.events {
            cmd.arg("--events").arg(spec.events_path());
        }
        if self.caps.live_state {
            cmd.arg("--live-state").arg(spec.live_state_path());
        }
        Ok(cmd)
    }
}

/// `osp <orchestration> [config]`'s own argument-order hint (DESIGN.md
/// §4.2): neither file can be told apart by extension (an orchestration
/// may be `.json`), so osp keeps the order strict and only hints when
/// the first argument looks like a config and the second like a
/// document (an `effects` key present only on the second).
pub fn looks_swapped(first: &Path, second: &Path) -> bool {
    !looks_like_a_document(first) && looks_like_a_document(second)
}

/// A best-effort, parser-free guess (compiling is the real check): does
/// this file have a top-level `effects` key, YAML or JSON? Good enough
/// for a hint, never for validation.
fn looks_like_a_document(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    text.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("effects:") || trimmed.starts_with("\"effects\":")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_swapped_is_false_for_the_documented_order() {
        // `osp <orchestration> [config.json]` (DESIGN.md §4.2): the
        // orchestrator's own regression -- `do_run` used to call this
        // with the two arguments flipped, warning on every correctly
        // ordered invocation and staying silent on a really swapped
        // one.
        let dir = tempfile::tempdir().unwrap();
        let orchestration = dir.path().join("do.yml");
        let config = dir.path().join("config.json");
        std::fs::write(&orchestration, "effects:\n  - name: hello\n").unwrap();
        std::fs::write(&config, "{}").unwrap();
        assert!(!looks_swapped(&orchestration, &config));
    }

    #[test]
    fn looks_swapped_is_true_for_a_really_swapped_pair() {
        let dir = tempfile::tempdir().unwrap();
        let orchestration = dir.path().join("do.yml");
        let config = dir.path().join("config.json");
        std::fs::write(&orchestration, "effects:\n  - name: hello\n").unwrap();
        std::fs::write(&config, "{}").unwrap();
        // The caller passed `config` first, `orchestration` second --
        // exactly the swapped-argument case this hint exists for.
        assert!(looks_swapped(&config, &orchestration));
    }

    #[test]
    fn cof_command_includes_live_state_out_and_events_when_supported() {
        let engine = CofEngine::with_caps(
            "cof",
            EngineCaps {
                events: true,
                live_state: true,
                missing: false,
            },
        );
        let spec = RunSpec {
            orchestration: PathBuf::from("do.yml"),
            config: None,
            sets: vec!["k=v".to_string()],
            run_dir: PathBuf::from("/tmp/run"),
        };
        let cmd = engine.command(&spec).unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(args.contains(&"--live-state".to_string()));
        assert!(args.contains(&"/tmp/run/state.live.json".to_string()));
        assert!(args.contains(&"--events".to_string()));
        assert!(args.contains(&"-e".to_string()));
        assert!(args.contains(&"k=v".to_string()));
    }

    #[test]
    fn cof_command_omits_events_when_unsupported() {
        let engine = CofEngine::with_caps(
            "cof",
            EngineCaps {
                events: false,
                live_state: true,
                missing: false,
            },
        );
        let spec = RunSpec {
            orchestration: PathBuf::from("do.yml"),
            config: None,
            sets: vec![],
            run_dir: PathBuf::from("/tmp/run"),
        };
        let cmd = engine.command(&spec).unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(!args.contains(&"--events".to_string()));
    }

    #[test]
    fn electricity_requires_a_config() {
        let engine = ElectricityEngine::new("electricity");
        let spec = RunSpec {
            orchestration: PathBuf::from("do.yml"),
            config: None,
            sets: vec![],
            run_dir: PathBuf::from("/tmp/run"),
        };
        assert!(matches!(
            engine.command(&spec),
            Err(EngineError::MissingConfig(_))
        ));
    }

    #[test]
    fn electricity_command_puts_config_before_orchestration() {
        let engine = ElectricityEngine::new("electricity");
        let spec = RunSpec {
            orchestration: PathBuf::from("do.yml"),
            config: Some(PathBuf::from("config.json")),
            sets: vec![],
            run_dir: PathBuf::from("/tmp/run"),
        };
        let cmd = engine.command(&spec).unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args[0], "config.json");
        assert_eq!(args[1], "do.yml");
    }

    #[test]
    fn electricity_command_adds_events_and_live_state_when_supported() {
        let engine = ElectricityEngine::with_caps(
            "electricity",
            EngineCaps {
                events: true,
                live_state: true,
                missing: false,
            },
        );
        let spec = RunSpec {
            orchestration: PathBuf::from("do.yml"),
            config: Some(PathBuf::from("config.json")),
            sets: vec!["k=v".to_string()],
            run_dir: PathBuf::from("/tmp/run"),
        };
        let cmd = engine.command(&spec).unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        // DESIGN.md §4.2's own argv shape: `<config> <doc> -e k=v...
        // --out <dir>/state.json [--events ...] [--live-state ...]`.
        assert_eq!(
            args,
            vec![
                "config.json",
                "do.yml",
                "-e",
                "k=v",
                "--out",
                "/tmp/run/state.json",
                "--events",
                "/tmp/run/events.jsonl",
                "--live-state",
                "/tmp/run/state.live.json",
            ]
        );
    }

    #[test]
    fn electricity_command_omits_events_and_live_state_when_unsupported() {
        let engine = ElectricityEngine::with_caps(
            "electricity",
            EngineCaps {
                events: false,
                live_state: false,
                missing: false,
            },
        );
        let spec = RunSpec {
            orchestration: PathBuf::from("do.yml"),
            config: Some(PathBuf::from("config.json")),
            sets: vec![],
            run_dir: PathBuf::from("/tmp/run"),
        };
        let cmd = engine.command(&spec).unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(!args.contains(&"--events".to_string()));
        assert!(!args.contains(&"--live-state".to_string()));
    }

    #[test]
    fn electricity_detect_reports_missing_for_an_absent_binary() {
        let engine = ElectricityEngine::detect("osp-test-nonexistent-electricity-binary");
        assert!(engine.caps().missing);
        assert!(!engine.caps().events);
        assert!(!engine.caps().live_state);
    }
}
