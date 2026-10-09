//! `electricity` — the CLI binary. This preview release has no VM: a run
//! request runs `electricity_compiler::check_for_run` first (issue #408's
//! CLI section) and, on success, still refuses with a message pointing to
//! `cof run` -- only a check failure prints that failure's own exact text
//! instead. `--version`/`--help` succeed; `--dump-ir` runs the same check
//! and prints the result as JSON.
//!
//! Every flag issue #431's own usage line lists parses, in any position,
//! before or after the two positionals (`<config.json> <orchestration.yml>`)
//! -- `--out`/`--pretty`/`--live-state`/`--events` included. A value-taking
//! long flag also accepts `--flag=value`, not just `--flag value`
//! (`cof`'s own Click parser accepts both; orchestrator ruling on PR
//! #432's review) -- `=` on a boolean flag (`--pretty=true`) is a usage
//! error, never silently accepted or ignored. An unknown flag is a usage
//! error (exit 2): issue #431's gate lane tightens this from the pre-#431
//! preview CLI, which let a trailing unknown flag fall through to the
//! (always-refusing) run path instead. None of the four
//! new flags changes this release's own behavior yet -- each still routes
//! to the same preview refusal / check-failure text `Action::Run` always
//! produced, so the conformance runner's existing "refusal keeps the
//! preview marker" skip rule keeps working; lane D wires their real
//! output contract in together with that runner's own update.

use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "\
Usage: electricity <config.json> <orchestration.yml> [-e key=value]... [--out state.json]
                    [--pretty] [--live-state state.json] [--events events.jsonl]
                    [--profile <path>]
       electricity <config.json> <orchestration.yml> --dump-ir [-e key=value]...

electricity is a preview: this release cannot run orchestrations yet. Use
`cof run` instead. --version, --help and --dump-ir are the only supported
commands.

Options:
  -V, --version       Print the version and exit
  -h, --help          Print this message and exit
  -e key=value        Pass an orchestration input, checked against its
                      declared interface.inputs the same way `cof run -e`
                      does. Repeatable; a later -e for the same key wins.
  --out <path>        Write the final state to <path> instead of stdout.
  --pretty            Sort state keys and indent by 2 spaces (only with
                      --out).
  --live-state <path> Mirror the running state to <path> as it changes.
  --events <path>     Write a JSONL event stream to <path>.
  --profile <path>    Not supported in this preview (profiles land in a
                      later milestone).
  --dump-ir           Print the compiled IR as JSON and exit. Unstable: this
                      format is a debugging aid and can change in any release.";

/// A successfully parsed `electricity <config.json> <doc> ...` run
/// request -- every flag [`USAGE`] lists that isn't `--dump-ir`/
/// `--version`/`--help`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct RunArgs {
    config: String,
    orchestration: String,
    inputs: Vec<String>,
    out: Option<String>,
    pretty: bool,
    live_state: Option<String>,
    events: Option<String>,
    /// Parsed (and its own value consumed) but never acted on in this
    /// preview -- profiles refuse the same way every other `Run` does.
    profile: Option<String>,
}

enum Action {
    Version,
    Help,
    Run(RunArgs),
    DumpIr(String, String, Vec<String>),
    UsageError(String),
}

/// One recognized flag's own arity -- whether [`parse_flags`] consumes a
/// following value for it. `KNOWN_FLAGS` is this parser's single source
/// of truth for "is this token a flag at all", so a new flag only has to
/// be added here (and to the consuming match in [`parse_flags`]) to be
/// recognized in every position.
enum Arity {
    Boolean,
    Value,
}

const KNOWN_FLAGS: &[(&str, Arity)] = &[
    ("-V", Arity::Boolean),
    ("--version", Arity::Boolean),
    ("-h", Arity::Boolean),
    ("--help", Arity::Boolean),
    ("--dump-ir", Arity::Boolean),
    ("--pretty", Arity::Boolean),
    ("-e", Arity::Value),
    ("--out", Arity::Value),
    ("--live-state", Arity::Value),
    ("--events", Arity::Value),
    ("--profile", Arity::Value),
];

fn flag_arity(flag: &str) -> Option<&'static Arity> {
    KNOWN_FLAGS
        .iter()
        .find(|(name, _)| *name == flag)
        .map(|(_, arity)| arity)
}

/// Every flag and positional in *args*, in one pass -- a token starting
/// with `-` that isn't exactly `-` (tolerated as a plain positional, the
/// conventional stdin/stdout placeholder, never a flag) and isn't in
/// [`KNOWN_FLAGS`] is an `Err` naming it (issue #431's CLI section:
/// "An unknown flag is a usage error (exit 2)"). A value-taking flag
/// with no following argument is also an `Err` (this preview's own
/// usage error; cof's own Click-layer "Option '...' requires an
/// argument." is third-party CLI framework text, not Circuitry's own, so
/// not matched word for word).
struct ParsedFlags {
    version: bool,
    help: bool,
    dump_ir: bool,
    pretty: bool,
    inputs: Vec<String>,
    out: Option<String>,
    live_state: Option<String>,
    events: Option<String>,
    profile: Option<String>,
    positionals: Vec<String>,
}

fn parse_flags(args: &[String]) -> Result<ParsedFlags, String> {
    let mut parsed = ParsedFlags {
        version: false,
        help: false,
        dump_ir: false,
        pretty: false,
        inputs: Vec::new(),
        out: None,
        live_state: None,
        events: None,
        profile: None,
        positionals: Vec::new(),
    };
    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        if arg == "-" {
            parsed.positionals.push(arg.to_string());
            i += 1;
            continue;
        }
        // `--name=value` (a long flag only -- getopt/Click's own
        // convention never extends this to a short flag like `-e`,
        // which instead always takes its value as a separate token):
        // split *once*, so a value that itself contains `=` (`--out
        // =a=b`, however unlikely) stays whole.
        let (flag_name, inline_value): (&str, Option<&str>) = if arg.starts_with("--") {
            match arg.split_once('=') {
                Some((name, value)) => (name, Some(value)),
                None => (arg, None),
            }
        } else {
            (arg, None)
        };
        let Some(arity) = flag_arity(flag_name) else {
            if arg.starts_with('-') {
                return Err(format!("unrecognized option '{arg}'"));
            }
            parsed.positionals.push(arg.to_string());
            i += 1;
            continue;
        };
        match arity {
            Arity::Boolean => {
                if inline_value.is_some() {
                    return Err(format!("{flag_name} does not take a value"));
                }
                match flag_name {
                    "-V" | "--version" => parsed.version = true,
                    "-h" | "--help" => parsed.help = true,
                    "--dump-ir" => parsed.dump_ir = true,
                    "--pretty" => parsed.pretty = true,
                    _ => unreachable!("every Arity::Boolean flag is handled above"),
                }
                i += 1;
            }
            Arity::Value => {
                let value = match inline_value {
                    Some(value) => value.to_string(),
                    None => args
                        .get(i + 1)
                        .ok_or_else(|| format!("{flag_name} requires an argument"))?
                        .clone(),
                };
                match flag_name {
                    "-e" => parsed.inputs.push(value),
                    "--out" => parsed.out = Some(value),
                    "--live-state" => parsed.live_state = Some(value),
                    "--events" => parsed.events = Some(value),
                    "--profile" => parsed.profile = Some(value),
                    _ => unreachable!("every Arity::Value flag is handled above"),
                }
                i += if inline_value.is_some() { 1 } else { 2 };
            }
        }
    }
    Ok(parsed)
}

fn classify(args: &[String]) -> Action {
    // `-V`/`--version` and `-h`/`--help` win over everything, including a
    // usage error elsewhere in the same command line -- checked directly
    // against the raw tokens, before flag validation, so `electricity
    // --bogus --version` still prints the version (pre-#431 behavior,
    // preserved).
    if args.iter().any(|a| a == "--version" || a == "-V") {
        return Action::Version;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Action::Help;
    }

    let parsed = match parse_flags(args) {
        Ok(parsed) => parsed,
        Err(message) => return Action::UsageError(message),
    };
    // `parse_flags` only ever sets `parsed.version`/`.help` via the exact
    // same tokens already handled above, so this is unreachable in
    // practice; kept for defense-in-depth if a future flag reuses them.
    if parsed.version {
        return Action::Version;
    }
    if parsed.help {
        return Action::Help;
    }

    if parsed.dump_ir {
        return match (parsed.positionals.first(), parsed.positionals.get(1)) {
            (Some(config), Some(orchestration)) => {
                Action::DumpIr(config.clone(), orchestration.clone(), parsed.inputs)
            }
            _ => Action::UsageError(
                "--dump-ir requires <config.json> <orchestration.yml>".to_string(),
            ),
        };
    }

    match (parsed.positionals.first(), parsed.positionals.get(1)) {
        (Some(config), Some(orchestration)) => Action::Run(RunArgs {
            config: config.clone(),
            orchestration: orchestration.clone(),
            inputs: parsed.inputs,
            out: parsed.out,
            pretty: parsed.pretty,
            live_state: parsed.live_state,
            events: parsed.events,
            profile: parsed.profile,
        }),
        _ => Action::UsageError("no config file or orchestration given".to_string()),
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
        Action::Run(run_args) => {
            // `electricity::parse_inputs`'s own malformed-`-e` text,
            // Circuitry's own `BadParameter` message word for word
            // (issue #429): `parse_flags` only checks that `-e` has
            // *some* value, saying nothing about whether that value
            // itself contains `=` -- a value with no `=` (`-e badtext`)
            // reaches this, `cli/app.py::_parse_env_vars`'s own check
            // proper.
            let inputs = match electricity::parse_inputs(&run_args.inputs) {
                Ok(inputs) => inputs,
                Err(message) => {
                    eprintln!("electricity: {message}");
                    eprintln!("{USAGE}");
                    return ExitCode::from(2);
                }
            };
            // `--out`/`--pretty`/`--live-state`/`--events`/`--profile`
            // are fully parsed above (consuming their own value, and
            // never themselves a usage error) but not yet acted on --
            // lane D wires the real `--out`/`--live-state`/`--events`
            // writers and `--profile`'s own preview refusal in together
            // with `run_orchestration`'s own replacement (issue #431's
            // CLI section). Until then every `Run` ends the same way it
            // always has: the check failure's own text, or this
            // preview's one unconditional refusal.
            let _ = (
                run_args.out,
                run_args.pretty,
                run_args.live_state,
                run_args.events,
                run_args.profile,
            );
            // On failure: exactly the error text on stderr, exit 1
            // (issue #408's CLI section). On success: still exit 1
            // with the preview refusal -- there is no VM yet.
            eprintln!(
                "{}",
                electricity::run_orchestration(
                    Path::new(&run_args.config),
                    Path::new(&run_args.orchestration),
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
    fn version_flag_wins_even_over_an_unknown_flag() {
        assert!(matches!(
            classify(&["--bogus".to_string(), "--version".to_string()]),
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
    fn unknown_flag_after_the_positionals_is_also_a_usage_error() {
        // Issue #431's gate lane tightens this from the pre-#431 preview
        // CLI, which let a trailing unknown flag fall through to the
        // (always-refusing) run path instead.
        assert!(matches!(
            classify(&[
                "config.json".to_string(),
                "orchestration.yml".to_string(),
                "--bogus".to_string(),
            ]),
            Action::UsageError(_)
        ));
    }

    #[test]
    fn positional_args_are_a_run_request() {
        assert!(matches!(
            classify(&["config.json".to_string(), "orchestration.yml".to_string()]),
            Action::Run(_)
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
            Action::Run(_)
        ));
        assert!(matches!(
            classify(&[
                "--profile".to_string(),
                "p".to_string(),
                "config.json".to_string(),
                "orchestration.yml".to_string()
            ]),
            Action::Run(_)
        ));
    }

    #[test]
    fn every_new_run_flag_parses_in_any_position() {
        match classify(&[
            "--out".to_string(),
            "state.json".to_string(),
            "config.json".to_string(),
            "orchestration.yml".to_string(),
            "--pretty".to_string(),
            "--live-state".to_string(),
            "live.json".to_string(),
            "--events".to_string(),
            "events.jsonl".to_string(),
        ]) {
            Action::Run(run_args) => {
                assert_eq!(run_args.config, "config.json");
                assert_eq!(run_args.orchestration, "orchestration.yml");
                assert_eq!(run_args.out.as_deref(), Some("state.json"));
                assert!(run_args.pretty);
                assert_eq!(run_args.live_state.as_deref(), Some("live.json"));
                assert_eq!(run_args.events.as_deref(), Some("events.jsonl"));
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn a_value_flag_with_no_following_argument_is_a_usage_error() {
        for flag in ["--out", "--live-state", "--events", "--profile"] {
            assert!(
                matches!(
                    classify(&[
                        "config.json".to_string(),
                        "orchestration.yml".to_string(),
                        flag.to_string(),
                    ]),
                    Action::UsageError(_)
                ),
                "{flag} with no value should be a usage error"
            );
        }
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
    fn bare_dash_not_in_first_position_is_still_a_run_request() {
        assert!(matches!(
            classify(&[
                "config.json".to_string(),
                "orchestration.yml".to_string(),
                "-".to_string(),
            ]),
            Action::Run(_)
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
            Action::Run(run_args) => assert_eq!(run_args.inputs, vec!["name=World".to_string()]),
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

    #[test]
    fn osps_own_invocation_shape_still_parses() {
        // oscilloscope's `ElectricityEngine` (`oscilloscope/crates/
        // oscilloscope-core/src/engine.rs`) passes `<config> <doc> -e
        // k=v... --out <dir>/state.json` -- issue #431's CLI section:
        // "That must keep working."
        match classify(&[
            "config.json".to_string(),
            "doc.yml".to_string(),
            "-e".to_string(),
            "name=World".to_string(),
            "--out".to_string(),
            "run-dir/state.json".to_string(),
        ]) {
            Action::Run(run_args) => {
                assert_eq!(run_args.out.as_deref(), Some("run-dir/state.json"));
                assert_eq!(run_args.inputs, vec!["name=World".to_string()]);
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn every_long_value_flag_accepts_the_equals_form() {
        match classify(&[
            "config.json".to_string(),
            "orchestration.yml".to_string(),
            "--out=state.json".to_string(),
            "--live-state=live.json".to_string(),
            "--events=events.jsonl".to_string(),
            "--profile=p.json".to_string(),
        ]) {
            Action::Run(run_args) => {
                assert_eq!(run_args.out.as_deref(), Some("state.json"));
                assert_eq!(run_args.live_state.as_deref(), Some("live.json"));
                assert_eq!(run_args.events.as_deref(), Some("events.jsonl"));
                assert_eq!(run_args.profile.as_deref(), Some("p.json"));
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn the_equals_form_works_before_the_positionals_too() {
        match classify(&[
            "--out=state.json".to_string(),
            "config.json".to_string(),
            "orchestration.yml".to_string(),
        ]) {
            Action::Run(run_args) => assert_eq!(run_args.out.as_deref(), Some("state.json")),
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn a_value_containing_an_equals_sign_is_kept_whole() {
        match classify(&[
            "config.json".to_string(),
            "orchestration.yml".to_string(),
            "--out=a=b.json".to_string(),
        ]) {
            Action::Run(run_args) => assert_eq!(run_args.out.as_deref(), Some("a=b.json")),
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn a_boolean_flag_with_an_equals_value_is_a_usage_error() {
        assert!(matches!(
            classify(&[
                "config.json".to_string(),
                "orchestration.yml".to_string(),
                "--pretty=true".to_string(),
            ]),
            Action::UsageError(_)
        ));
        assert!(matches!(
            classify(&[
                "config.json".to_string(),
                "orchestration.yml".to_string(),
                "--dump-ir=1".to_string(),
            ]),
            Action::UsageError(_)
        ));
    }

    #[test]
    fn an_unknown_flag_with_an_equals_value_is_still_a_usage_error() {
        assert!(matches!(
            classify(&[
                "config.json".to_string(),
                "orchestration.yml".to_string(),
                "--bogus=1".to_string(),
            ]),
            Action::UsageError(_)
        ));
    }

    #[test]
    fn osps_own_invocation_shape_still_parses_with_the_equals_form() {
        match classify(&[
            "config.json".to_string(),
            "doc.yml".to_string(),
            "-e".to_string(),
            "name=World".to_string(),
            "--out=run-dir/state.json".to_string(),
        ]) {
            Action::Run(run_args) => {
                assert_eq!(run_args.out.as_deref(), Some("run-dir/state.json"));
            }
            _ => panic!("expected Run"),
        }
    }
}
