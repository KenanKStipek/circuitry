//! `electricity` — the CLI binary. This preview release has no VM: a run
//! request runs `electricity_compiler::check_for_run` first (issue #408's
//! CLI section) and, on success, still refuses with a message pointing to
//! `cof run` -- only a check failure prints that failure's own exact text
//! instead. `--version`/`--help` succeed; `--dump-ir` runs the same check
//! and prints the result as JSON.

use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "\
Usage: electricity <config.json> <orchestration.yml> [-e key=value]... [--out state.json] [--profile <path>]
       electricity <config.json> <orchestration.yml> --dump-ir [-e key=value]...

electricity is a preview: this release cannot run orchestrations yet. Use
`cof run` instead. --version, --help and --dump-ir are the only supported
commands.

Options:
  -V, --version   Print the version and exit
  -h, --help      Print this message and exit
  -e key=value    Pass an orchestration input, checked against its
                  declared interface.inputs the same way `cof run -e`
                  does. Repeatable; a later -e for the same key wins.
  --dump-ir       Print the compiled IR as JSON and exit. Unstable: this
                  format is a debugging aid and can change in any release.";

enum Action {
    Version,
    Help,
    Run(String, String, Vec<String>),
    DumpIr(String, String, Vec<String>),
    UsageError(String),
}

/// Every positional argument in *args*, skipping `--dump-ir` and each
/// known run flag's own value -- the one list both `Action::Run` and
/// `Action::DumpIr` (each its own first two: `<config.json>
/// <orchestration.yml>`) are built from.
fn positionals(args: &[String]) -> Vec<&str> {
    let mut result = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "--dump-ir" {
            i += 1;
        } else if KNOWN_RUN_FLAGS.contains(&arg) {
            i += 2; // the flag and its value
        } else {
            result.push(arg);
            i += 1;
        }
    }
    result
}

/// Every `-e` value in *args*, in order, duplicates included --
/// `electricity::parse_inputs`'s own input, built the same way
/// [`positionals`] walks the same argument list. `Err` is a trailing
/// `-e` with no value (this preview's own usage error; cof's own
/// Click-layer "Option '-e' requires an argument." is third-party CLI
/// framework text, not Circuitry's own, so not matched word for word).
fn e_entries(args: &[String]) -> Result<Vec<String>, String> {
    let mut result = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "-e" {
            match args.get(i + 1) {
                Some(value) => {
                    result.push(value.clone());
                    i += 2;
                }
                None => return Err("-e requires an argument".to_string()),
            }
        } else if arg == "--dump-ir" {
            i += 1;
        } else if KNOWN_RUN_FLAGS.contains(&arg) {
            i += 2;
        } else {
            i += 1;
        }
    }
    Ok(result)
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

    // A trailing `-e` with no value is this preview's own
    // structural-parsing error (cof's own Click layer reports it before
    // any positional validation too, confirmed directly: `cof run -e`
    // alone reports the missing `-e` value, not "Missing orchestration")
    // -- checked before the positional/unrecognized-option classification
    // below, same priority.
    let inputs = match e_entries(args) {
        Ok(inputs) => inputs,
        Err(message) => return Action::UsageError(message),
    };

    // Preserves the pre-#408 classification exactly ("Additive only"):
    // only the first argument decides Run vs. a usage error, so a
    // trailing unknown flag that used to fall through to the preview's
    // Run path -- e.g. `electricity c.json d.yml --pretty` -- still
    // does, and a bare `-` not in first position is still tolerated
    // the same way. `--dump-ir` is excluded here (handled below) so it
    // keeps working in first position too.
    match args.first() {
        None => return Action::UsageError("no config file or orchestration given".to_string()),
        Some(first)
            if first.starts_with('-')
                && first != "--dump-ir"
                && !KNOWN_RUN_FLAGS.contains(&first.as_str()) =>
        {
            return Action::UsageError(format!("unrecognized option '{first}'"));
        }
        _ => {}
    }

    if !args.iter().any(|a| a == "--dump-ir") {
        let found = positionals(args);
        return match (found.first(), found.get(1)) {
            (Some(config), Some(orchestration)) => {
                Action::Run(config.to_string(), orchestration.to_string(), inputs)
            }
            _ => Action::UsageError("no config file or orchestration given".to_string()),
        };
    }

    // The first two positionals, never a trailing one -- `electricity
    // <config.json> <orchestration.yml> --dump-ir` takes exactly two,
    // so a third stray positional must not silently become the
    // orchestration path.
    let found = positionals(args);

    match (found.first(), found.get(1)) {
        (Some(config), Some(orchestration)) => {
            Action::DumpIr(config.to_string(), orchestration.to_string(), inputs)
        }
        _ => Action::UsageError("--dump-ir requires <config.json> <orchestration.yml>".to_string()),
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
        Action::Run(config_path, orchestration_path, raw_inputs) => {
            // `electricity::parse_inputs`'s own malformed-`-e` text,
            // Circuitry's own `BadParameter` message word for word
            // (issue #429) -- not reached today, since `e_entries`
            // (every entry already has a value) says nothing about
            // whether that value itself contains `=`; this is the
            // `cli/app.py::_parse_env_vars` check proper.
            let inputs = match electricity::parse_inputs(&raw_inputs) {
                Ok(inputs) => inputs,
                Err(message) => {
                    eprintln!("electricity: {message}");
                    eprintln!("{USAGE}");
                    return ExitCode::from(2);
                }
            };
            // On failure: exactly the error text on stderr, exit 1
            // (issue #408's CLI section). On success: still exit 1
            // with the preview refusal -- there is no VM yet.
            eprintln!(
                "{}",
                electricity::run_orchestration(
                    Path::new(&config_path),
                    Path::new(&orchestration_path),
                    &inputs,
                )
            );
            ExitCode::from(1)
        }
        Action::DumpIr(config_path, orchestration_path, raw_inputs) => {
            let inputs = match electricity::parse_inputs(&raw_inputs) {
                Ok(inputs) => inputs,
                Err(message) => {
                    eprintln!("electricity: {message}");
                    eprintln!("{USAGE}");
                    return ExitCode::from(2);
                }
            };
            match electricity::dump_ir(
                Path::new(&config_path),
                Path::new(&orchestration_path),
                &inputs,
            ) {
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
            Action::Run(_, _, _)
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
            Action::Run(_, _, _)
        ));
        assert!(matches!(
            classify(&[
                "--profile".to_string(),
                "p".to_string(),
                "config.json".to_string(),
                "orchestration.yml".to_string()
            ]),
            Action::Run(_, _, _)
        ));
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
    fn trailing_unknown_flag_is_still_a_run_request_not_a_usage_error() {
        // Pre-#408 behaviour, preserved: only the first argument is
        // classified; a trailing unknown flag used to fall through to
        // the preview's Run path (exit 1), not a usage error (exit 2).
        assert!(matches!(
            classify(&[
                "config.json".to_string(),
                "orchestration.yml".to_string(),
                "--pretty".to_string(),
            ]),
            Action::Run(_, _, _)
        ));
    }

    #[test]
    fn bare_dash_not_in_first_position_is_still_a_run_request() {
        assert!(matches!(
            classify(&[
                "config.json".to_string(),
                "orchestration.yml".to_string(),
                "-".to_string(),
            ]),
            Action::Run(_, _, _)
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
            Action::Run(_, _, inputs) => assert_eq!(inputs, vec!["name=World".to_string()]),
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
}
