//! `electricity` — the CLI binary. This preview release has no VM: every
//! run request fails with a message pointing to `cof run`.
//! `--version`/`--help` succeed; `--dump-ir` runs the compiler's
//! `check_for_run` and prints its result (issue #408's CLI section) --
//! currently always a failure, since `electricity-compiler` is still a
//! lane A stub.

use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "\
Usage: electricity <config.json> <orchestration.yml> [-e key=value]... [--out state.json] [--profile <path>]
       electricity <config.json> <orchestration.yml> --dump-ir

electricity is a preview: this release cannot run orchestrations yet. Use
`cof run` instead. --version, --help and --dump-ir are the only supported
commands.

Options:
  -V, --version   Print the version and exit
  -h, --help      Print this message and exit
  --dump-ir       Print the compiled IR as JSON and exit. Unstable: this
                  format is a debugging aid and can change in any release.";

enum Action {
    Version,
    Help,
    Run,
    DumpIr(String),
    UsageError(String),
}

/// Run flags the usage text advertises (`-e key=value`, `--out`, `--profile`):
/// recognized in any position, each followed by its own value.
const KNOWN_RUN_FLAGS: &[&str] = &["-e", "--out", "--profile"];

fn classify(args: &[String]) -> Action {
    if args.iter().any(|a| a == "--version" || a == "-V") {
        return Action::Version;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Action::Help;
    }

    let dump_ir = args.iter().any(|a| a == "--dump-ir");
    let mut positionals: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--dump-ir" {
            i += 1;
        } else if KNOWN_RUN_FLAGS.contains(&arg) {
            i += 2; // the flag and its value
        } else if arg.starts_with('-') {
            return Action::UsageError(format!("unrecognized option '{arg}'"));
        } else {
            positionals.push(arg);
            i += 1;
        }
    }

    match positionals.last() {
        None => Action::UsageError("no config file or orchestration given".to_string()),
        Some(orchestration) if dump_ir => Action::DumpIr(orchestration.to_string()),
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
        Action::DumpIr(orchestration_path) => {
            match electricity::dump_ir(Path::new(&orchestration_path)) {
                Ok(json) => {
                    println!("{json}");
                    ExitCode::SUCCESS
                }
                Err(message) => {
                    eprintln!("{message}");
                    ExitCode::from(1)
                }
            }
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

    #[test]
    fn dump_ir_flag_selects_the_last_positional_as_the_orchestration() {
        match classify(&[
            "config.json".to_string(),
            "orchestration.yml".to_string(),
            "--dump-ir".to_string(),
        ]) {
            Action::DumpIr(path) => assert_eq!(path, "orchestration.yml"),
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
            Action::DumpIr(_)
        ));
    }

    #[test]
    fn dump_ir_with_no_positionals_is_a_usage_error() {
        assert!(matches!(
            classify(&["--dump-ir".to_string()]),
            Action::UsageError(_)
        ));
    }
}
