//! `electricity` — the CLI binary. This preview release has no compiler or
//! VM: every run request fails with a message pointing to `cof run`, and
//! `--version`/`--help` are the only two commands that succeed.

use std::process::ExitCode;

const USAGE: &str = "\
Usage: electricity <config.json> <orchestration.yml> [-e key=value]... [--out state.json] [--profile <path>]

electricity is a preview: this release cannot run orchestrations yet. Use
`cof run` instead. --version and --help are the only supported commands.

Options:
  -V, --version   Print the version and exit
  -h, --help      Print this message and exit";

enum Action {
    Version,
    Help,
    Run,
    UsageError(String),
}

/// Run flags the usage text advertises (`-e key=value`, `--out`, `--profile`):
/// recognized in any position, so a request using them is a run request
/// (exit 1), not a usage error (exit 2).
const KNOWN_RUN_FLAGS: &[&str] = &["-e", "--out", "--profile"];

fn classify(args: &[String]) -> Action {
    if args.iter().any(|a| a == "--version" || a == "-V") {
        return Action::Version;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Action::Help;
    }
    match args.first() {
        None => Action::UsageError("no config file or orchestration given".to_string()),
        Some(first) if first.starts_with('-') && !KNOWN_RUN_FLAGS.contains(&first.as_str()) => {
            Action::UsageError(format!("unrecognized option '{first}'"))
        }
        Some(_) => Action::Run,
    }
}

fn main() -> ExitCode {
    // `args_os` + lossy conversion instead of `args()`, which panics on a
    // non-UTF-8 argument.
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    match classify(&args) {
        Action::Version => {
            println!("{}", electricity::version_string());
            ExitCode::SUCCESS
        }
        Action::Help => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Action::Run => {
            eprintln!("{}", electricity::run_orchestration().unwrap_err());
            ExitCode::from(1)
        }
        Action::UsageError(message) => {
            eprintln!("electricity: {message}");
            eprintln!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn version_flag_wins_over_everything() {
        assert!(matches!(
            classify(&["--version".to_string(), "bogus".to_string()]),
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
    fn positional_args_are_a_run_request() {
        assert!(matches!(
            classify(&["config.json".to_string(), "orchestration.yml".to_string()]),
            Action::Run
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
            Action::Run
        ));
        assert!(matches!(
            classify(&[
                "--profile".to_string(),
                "p".to_string(),
                "config.json".to_string(),
                "orchestration.yml".to_string()
            ]),
            Action::Run
        ));
    }
}
