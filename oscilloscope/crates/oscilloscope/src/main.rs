//! `osp` — the CLI binary (DESIGN.md §4, §6.2–§6.3). Milestone O-0
//! (issue #420) wires up argument parsing only: `--version` and
//! `--help` work, and both commands below exit 2 with a "not
//! implemented yet" message. The run logic lands in O-1, the TUI in
//! O-2.
//!
//! The two forms share one top-level command: `watch` is a real
//! `clap` subcommand, and anything else is captured by
//! `#[command(external_subcommand)]` and reparsed as [`RunArgs`] — so
//! `-e key=value` and the other run flags are only recognized *after*
//! the orchestration positional, matching the usage synopsis in
//! [`ABOUT`] (and `cof run`'s own flags-after-positionals order,
//! DESIGN.md §4.1).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};

const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), " (preview)");

const ABOUT: &str = "\
osp (oscilloscope) launches a Circuitry orchestration on cof or electricity \
and shows it running. This preview (milestone O-0) has no run logic yet.

Planned interface (milestone O-1):
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
    #[arg(long, value_enum)]
    engine: Option<Engine>,
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

#[derive(ValueEnum, Clone, Debug)]
enum Engine {
    Cof,
    Electricity,
}

const NOT_IMPLEMENTED: &str = "osp: not implemented yet (milestone O-1)";

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
        Commands::Watch(_args) => {
            eprintln!("{NOT_IMPLEMENTED}");
            ExitCode::from(2)
        }
        Commands::Run(raw) => {
            match RunArgs::try_parse_from(std::iter::once("osp".to_string()).chain(raw)) {
                Ok(_args) => {
                    eprintln!("{NOT_IMPLEMENTED}");
                    ExitCode::from(2)
                }
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
    fn run_form_is_not_implemented() {
        assert_eq!(
            run(args(&["do-thing.yml", "config.json"])),
            ExitCode::from(2)
        );
    }

    #[test]
    fn run_form_with_flags_after_the_positional_is_not_implemented() {
        assert_eq!(
            run(args(&[
                "do-thing.yml",
                "config.json",
                "-e",
                "k=v",
                "--engine",
                "electricity",
                "--out-dir",
                "/tmp/osp-out",
                "--log",
            ])),
            ExitCode::from(2)
        );
    }

    #[test]
    fn watch_form_is_not_implemented() {
        assert_eq!(
            run(args(&["watch", "/tmp/some-run-dir"])),
            ExitCode::from(2)
        );
    }

    #[test]
    fn watch_form_with_plan_is_not_implemented() {
        assert_eq!(
            run(args(&["watch", "state.live.json", "--plan", "doc.yml"])),
            ExitCode::from(2)
        );
    }

    #[test]
    fn unknown_flag_before_the_orchestration_is_a_usage_error() {
        assert_eq!(run(args(&["-e", "k=v", "do-thing.yml"])), ExitCode::from(2));
    }

    #[test]
    fn no_args_is_a_usage_error() {
        assert_eq!(run(args(&[])), ExitCode::from(2));
    }
}
