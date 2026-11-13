//! `electricity` — the CLI binary. `Action::Run` drives
//! `electricity::run_orchestration` (issue #431's run-wiring steps 1-19)
//! on a current-thread Tokio runtime, arming SIGINT/SIGTERM/SIGHUP
//! cancellation for the run alone (issue #431's Signals section),
//! writes `--out` on success *and* failure, and follows `cof`'s own
//! non-TTY stdout contract (issue #431's "CLI output" decision).
//! `--version`/`--help` succeed; `--dump-ir` runs the document check
//! alone and prints the result as JSON -- it never runs anything, and
//! so never refuses unsupported content either.
//!
//! Every flag issue #431's own usage line lists parses, in any position,
//! before or after the two positionals (`<config.json> <orchestration.yml>`)
//! -- `--out`/`--pretty`/`--live-state`/`--events` included. A value-taking
//! long flag also accepts `--flag=value`, not just `--flag value`
//! (`cof`'s own Click parser accepts both; ruling on PR
//! #432's review) -- `=` on a boolean flag (`--pretty=true`) is a usage
//! error, never silently accepted or ignored. An unknown flag is a usage
//! error (exit 2): issue #431's gate lane tightens this from the pre-#431
//! preview CLI, which let a trailing unknown flag fall through to the
//! (always-refusing) run path instead. `--profile <path>` (issue #495,
//! lane R) is passed straight through to `RunRequest::profile_path` --
//! an explicit path only, never discovered by name (DESIGN §1, §6.11).
//! A loaded profile's own `out:` (`RunResult::out_path`) is written only
//! when no `--out` was given.
//!
//! `electricity run <orchestration.yml> ...` (issue #728) is a second,
//! cof-named-option entry into `Action::Run` alongside the legacy
//! two-positional form above: `run` must be the very first token, and
//! everything after it parses with the same flag table (plus `--config`,
//! `--allow-capabilities`, `--adapter`, `--quiet`, none of which the
//! legacy form's own positional-config shape has room for). No
//! `--dump-ir` under `run` -- it has no `cof run` equivalent, and stays
//! reachable through the legacy form only. A missing orchestration
//! positional under `run` is `Action::MissingOrchestration` (exit 1,
//! cof's own text on stdout) only when `--last` is also absent, not
//! `Action::UsageError` (exit 2): cof's own positional is `Optional[str]`
//! precisely so `--last` (issue #760) can stand in for it, so Click never
//! raises a usage error for its absence either.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use electricity::CancellationToken;
use electricity::run::{RunRequest, RunResult, Signal};

mod user_env;

const USAGE: &str = "\
Usage: electricity <config.json> <orchestration.yml> [-e key=value]... [--out state.json]
                    [--pretty] [--live-state state.json] [--events events.jsonl]
                    [--profile <path>]
       electricity <config.json> <orchestration.yml> --dump-ir [-e key=value]...
       electricity run [orchestration.yml] [-e key=value]... [--config config.json]
                    [--out state.json] [--live-state state.json] [--events events.jsonl]
                    [--allow-capabilities cap,cap] [--adapter name] [--label TEXT] [--quiet]
                    [--pretty] [--profile <path>] [--state state.json] [--resume MODE] [--force]
                    [--last]

electricity is a preview: it runs tool/dynamic/if/finally/loop/use/yield/prompt/reflector documents
only (persistence and runtime plugins are refused with a
preview marker; use `cof run` for those). A prompt effect's model is
reached for real through a document's own `adapter:` or a prompt's own
`provider:`, including `ollama`, the OpenAI-compatible family, `openai`,
`anthropic`, `replicate`, `watsonx`, `cyberdiner`, `claude_code`, `pi`, and
the `scripted` adapter; `litellm` has no electricity builder and is
refused permanently.

`electricity run <doc>` accepts the same options `cof run` does, with the
same config discovery (`--config` optional: CIRCUITRY_CONFIG, then a
trusted project config in the working directory, then the global config)
and the same meaning for --allow-capabilities/--adapter/--quiet. The
legacy `electricity <config.json> <doc>` form (config always explicit,
never discovered) keeps working as an alias; --profile/--pretty/--dump-ir
are electricity-only additions, available under either form.

Options:
  -V, --version       Print the version and exit. Legacy form only -- `cof
                      run` has no `-V`/`--version` of its own, so neither
                      does `electricity run` (an unrecognized option there,
                      same as cof's).
  -h, --help          Print this message and exit. Legacy form only, same
                      as -V/--version above -- under `run`, use the long
                      --help below (cof run's own `-h` is likewise
                      unrecognized; only --help is).
  --help              Print this message and exit. Works under both forms.
  -e key=value        Pass an orchestration input, checked against its
                      declared interface.inputs the same way `cof run -e`
                      does. Repeatable; a later -e for the same key wins.
  --config <path>, -c `run` only: path to config JSON (or CIRCUITRY_CONFIG,
                      or discovery -- see above). The legacy form's first
                      positional is this value, always explicit; --config/-c
                      is rejected there as redundant.
  --out <path>, -o    Write the final state to <path> instead of stdout.
                      The -o alias is `run` only, matching cof's own.
  --pretty            Sort state keys and indent by 2 spaces -- the final
                      state, whether printed to stdout (no --out) or
                      written to --out's own file.
  --live-state <path> Mirror the running state to <path> as it changes.
  --events <path>     Write a JSONL event stream to <path>.
  --allow-capabilities cap,cap
                      Comma-separated capabilities to pre-approve for this
                      run (cof's own --allow-capabilities meaning and
                      escape hatch). Available under either form.
  --adapter <name>    This run's default adapter, beating a loaded
                      --profile, the orchestration and the config.
                      Available under either form.
  --label <text>      `run` only: literal text for this run's label --
                      beats the document's own top-level label: template.
                      Never rendered. Cleaned the same way either way:
                      whitespace collapsed to one space, control
                      characters and escape sequences dropped, trimmed,
                      cut at 64 characters; empty after cleaning means no
                      label.
  --quiet             Suppress non-essential output (accepted for parity;
                      electricity prints no such output today). Available
                      under either form.
  --profile <path>    Load a profile (adapter/model/inputs/persistence
                      overrides, routing pins and provider denials) from
                      an explicit path -- no name discovery.
  --dump-ir           Print the compiled IR as JSON and exit. Unstable: this
                      format is a debugging aid and can change in any release.
                      Legacy form only -- no `cof run` equivalent.
  --state <path>, -s  `run` only: resume from this saved state file --
                      every top-level key it carries, not just `input`.
                      A named effect whose saved node already finished
                      without error is skipped and reused only when
                      --resume is also given; named alone, --state
                      still seeds the whole run from the file, with no
                      skip. The -s alias is `run` only, matching cof's.
  --resume <mode>     `run` only: 'last' resumes from the most recent
                      stashed run's own --out (via the same --last
                      stash below), and --state <file> given alongside
                      any other value engages the skip-and-continue
                      behaviour above against that file directly. A
                      bare run-id (looked up via a runtime.persistence
                      backend) is not supported. Refuses a document
                      that changed since the saved run unless --force.
  --force             `run` only: with --resume, continue even though
                      the orchestration's content changed since the
                      saved run (or it carries no recorded digest at
                      all).
  --last              `run` only: replay the most recently stashed run's
                      own arguments (same file, keys and redaction as
                      cof's own --last stash) -- every other flag on the
                      line is overwritten from the stash, including the
                      orchestration positional, which may be omitted
                      entirely. Mutually exclusive with --resume.";

/// A successfully parsed run request -- every flag [`USAGE`] lists that
/// isn't `--dump-ir`/`--version`/`--help`, from either
/// `electricity run <doc> ...` or the legacy `electricity <config.json>
/// <doc> ...` form. `config` is always `Some` for the legacy form (its
/// first positional); for `run`, `None` means `--config` was omitted and
/// [`run_action`] must discover it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct RunArgs {
    config: Option<String>,
    orchestration: String,
    inputs: Vec<String>,
    out: Option<String>,
    pretty: bool,
    live_state: Option<String>,
    events: Option<String>,
    /// `--profile <path>` (issue #495, lane R): an explicit path
    /// only, passed through to `RunRequest::profile_path`.
    profile: Option<String>,
    /// `--allow-capabilities a,b` (issue #728, `run` only) -- raw text,
    /// split on `,` and trimmed in [`run_action`].
    allow_capabilities: Option<String>,
    /// `--adapter <name>` (issue #728, `run` only).
    adapter: Option<String>,
    /// `--label TEXT` (#752, `run` only) -- literal text, never
    /// rendered, that wins over the document's own top-level `label:`.
    label: Option<String>,
    /// `--quiet` (issue #728, `run` only) -- accepted for parity; has
    /// no observable effect today (USAGE's own note).
    quiet: bool,
    /// `--state <path>`/`-s` (issue #727, `run` only) -- the saved
    /// state this run resumes from (every top-level key, not just
    /// `input`), named explicitly.
    state: Option<String>,
    /// `--resume` (issue #727, `run` only) -- any value engages
    /// `--state`'s own resume skip logic (`cof run`'s own `last`/a
    /// run-id mode needs a persistence/stash lookup this engine never
    /// builds, so it is out of scope here); absent with `--state`
    /// given, the state file still seeds the run, just without the
    /// skip-and-continue behaviour.
    resume: Option<String>,
    /// `--force` (issue #727, `run` only).
    force: bool,
    /// `--last` (issue #760, `run` only) -- replay the most recently
    /// stashed run's own arguments, overwriting every other field this
    /// struct holds (`run_action`'s own `if run_args.last` block),
    /// exactly as `cof run`'s own `if last:` block does.
    last: bool,
    /// Whether this came from `electricity run <doc> ...` rather than
    /// the legacy `electricity <config.json> <doc> ...` form -- the one
    /// thing [`run_action`]'s own later usage-error site (a malformed
    /// `-e` entry) needs to pick [`print_run_usage_error`]'s plain
    /// frame over the legacy form's `USAGE` text, matching which of
    /// `cof run`'s own two argument shapes produced the same error.
    via_run: bool,
}

// `RunArgs` has grown past clippy's own size heuristic (issue #727's
// three new fields) -- a CLI parses exactly one command line per
// process and discards `Action` immediately after, so boxing the
// large variant to shrink every other one would only add an
// allocation with no actual memory-pressure benefit.
#[allow(clippy::large_enum_variant)]
enum Action {
    Version,
    Help,
    Run(RunArgs),
    /// `electricity run` with no orchestration positional and no
    /// `--last` either (issue #760) -- `cof run`'s own
    /// missing-orchestration check, which is not a usage error there
    /// either (the positional is `Optional[str]`, to support `--last`),
    /// so this is a distinct exit-1 outcome from
    /// [`Action::UsageError`]'s exit 2.
    MissingOrchestration,
    DumpIr(String, String, Vec<String>),
    UsageError(String),
    /// A usage error under `electricity run` specifically (issue #728)
    /// -- rendered in [`print_run_usage_error`]'s own plain frame
    /// (`Usage:`/`Try ... --help`/blank line/`Error: <message>`), not
    /// [`UsageError`]'s own legacy-form text, since `cof run`'s real
    /// errors arrive in a differently-shaped frame (a Rich panel with
    /// its own program name, close-match suggestions and width-
    /// dependent wrapping -- DESIGN.md's own row) that this plain
    /// frame approximates rather than reproduces byte for byte.
    RunUsageError(String),
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
    ("--quiet", Arity::Boolean),
    ("-e", Arity::Value),
    ("--out", Arity::Value),
    ("--live-state", Arity::Value),
    ("--events", Arity::Value),
    ("--profile", Arity::Value),
    ("--config", Arity::Value),
    ("--allow-capabilities", Arity::Value),
    ("--adapter", Arity::Value),
];

/// `electricity run <doc> ...`'s own flag table (issue #728) --
/// deliberately a *different* table from [`KNOWN_FLAGS`], not that one
/// filtered: `-V`/`--version`/`-h` have no `cof run` equivalent at all
/// (`cof run --version`/`-V`/`-h` are themselves unrecognized options,
/// unlike the bare `cof --version`/`-h` on the group), so under `run`
/// they must fail the exact same way an unknown flag does, not be
/// recognized as a boolean that happens to go unused -- [`classify`]'s
/// own `run` branch relies on that (an unrecognized `-V` is caught by
/// [`parse_flags`] before it could ever win over a usage error
/// elsewhere on the line, cof's own behavior -- see that branch's doc
/// comment). `--help` (long form only, no `-h`) is included: `cof run
/// --help` does succeed. `-c`/`-o` are `cof run`'s own short aliases
/// for `--config`/`--out`.
const RUN_FLAGS: &[(&str, Arity)] = &[
    ("--help", Arity::Boolean),
    ("--dump-ir", Arity::Boolean),
    ("--pretty", Arity::Boolean),
    ("--quiet", Arity::Boolean),
    ("-e", Arity::Value),
    ("--out", Arity::Value),
    ("-o", Arity::Value),
    ("--live-state", Arity::Value),
    ("--events", Arity::Value),
    ("--profile", Arity::Value),
    ("--config", Arity::Value),
    ("-c", Arity::Value),
    ("--allow-capabilities", Arity::Value),
    ("--adapter", Arity::Value),
    ("--label", Arity::Value),
    ("--state", Arity::Value),
    ("-s", Arity::Value),
    ("--resume", Arity::Value),
    ("--force", Arity::Boolean),
    ("--last", Arity::Boolean),
];

/// `PathBuf` -> `String` for a stash field (issue #760): the stash
/// itself stores every path as a plain JSON string (`str(path)`,
/// `cli/app.py::_save_last_run`'s own shape), so a path read back out
/// of one is always re-stringified before landing in a [`RunArgs`]
/// field, which holds the *unparsed* CLI value either way.
fn path_to_string(path: PathBuf) -> String {
    path.to_string_lossy().into_owned()
}

/// `cli/app.py::_save_last_run`'s own dict literal, in the exact same
/// key order (issue #760) -- a stash either engine wrote must read back
/// the same in the other, and `electricity-config::last_run::LastRun`'s
/// own accessors are keyed by name, not position, so this is purely for
/// a byte-for-byte-readable file, not a functional requirement. Every
/// key `cof run` writes that electricity's own CLI has no flag for
/// (`print_state`/`dry_run`/`json_out`/`verbose`/`tail`/`skip_preflight`/
/// `profile_from_state`/`model`/`explain_routing`/`decompose_out`/
/// `scoring`/`routing`/`decompose`/`split`) is written as the value an
/// invocation that never passed the reference's own equivalent flag
/// would stash -- `false`/`null` either way. `trust_document` is always
/// `true` and `remote_library_source` always `false`: electricity's own
/// positional orchestration argument is always a literal path, never a
/// library name (this module's own earlier doc comment).
fn last_run_stash_args(
    run_args: &RunArgs,
    resume_default_out_path: Option<&Path>,
) -> electricity_value::Dict {
    use electricity_value::{Dict, Value};

    fn opt_str(value: Option<&String>) -> Value {
        match value {
            Some(s) => Value::Str(s.clone()),
            None => Value::None,
        }
    }

    let mut args = Dict::new();
    args.insert(
        Value::Str("orchestration".to_string()),
        Value::Str(run_args.orchestration.clone()),
    );
    args.insert(
        Value::Str("config".to_string()),
        opt_str(run_args.config.as_ref()),
    );
    args.insert(
        Value::Str("state".to_string()),
        opt_str(run_args.state.as_ref()),
    );
    let out_value = run_args
        .out
        .clone()
        .or_else(|| resume_default_out_path.map(|p| path_to_string(p.to_path_buf())));
    args.insert(Value::Str("out".to_string()), opt_str(out_value.as_ref()));
    args.insert(
        Value::Str("pretty".to_string()),
        Value::Bool(run_args.pretty),
    );
    args.insert(Value::Str("print_state".to_string()), Value::Bool(false));
    args.insert(Value::Str("dry_run".to_string()), Value::Bool(false));
    args.insert(Value::Str("json_out".to_string()), Value::Bool(false));
    args.insert(Value::Str("quiet".to_string()), Value::Bool(run_args.quiet));
    args.insert(Value::Str("verbose".to_string()), Value::Bool(false));
    args.insert(
        Value::Str("live_state".to_string()),
        opt_str(run_args.live_state.as_ref()),
    );
    args.insert(
        Value::Str("events".to_string()),
        opt_str(run_args.events.as_ref()),
    );
    args.insert(
        Value::Str("env_vars".to_string()),
        Value::List(
            electricity_config::redacted_env_pairs(&run_args.inputs)
                .into_iter()
                .map(Value::Str)
                .collect(),
        ),
    );
    args.insert(
        Value::Str("label".to_string()),
        opt_str(run_args.label.as_ref()),
    );
    args.insert(Value::Str("tail".to_string()), Value::Bool(false));
    args.insert(Value::Str("skip_preflight".to_string()), Value::Bool(false));
    args.insert(
        Value::Str("allow_capabilities".to_string()),
        opt_str(run_args.allow_capabilities.as_ref()),
    );
    args.insert(
        Value::Str("profile".to_string()),
        opt_str(run_args.profile.as_ref()),
    );
    args.insert(Value::Str("profile_from_state".to_string()), Value::None);
    args.insert(
        Value::Str("adapter".to_string()),
        opt_str(run_args.adapter.as_ref()),
    );
    args.insert(Value::Str("model".to_string()), Value::None);
    args.insert(
        Value::Str("explain_routing".to_string()),
        Value::Bool(false),
    );
    args.insert(Value::Str("decompose_out".to_string()), Value::None);
    args.insert(Value::Str("scoring".to_string()), Value::None);
    args.insert(Value::Str("routing".to_string()), Value::None);
    args.insert(Value::Str("decompose".to_string()), Value::None);
    args.insert(Value::Str("split".to_string()), Value::None);
    args.insert(Value::Str("trust_document".to_string()), Value::Bool(true));
    args.insert(
        Value::Str("remote_library_source".to_string()),
        Value::Bool(false),
    );
    args
}

fn flag_arity<'a>(flags: &'a [(&'static str, Arity)], flag: &str) -> Option<&'a Arity> {
    flags
        .iter()
        .find(|(name, _)| *name == flag)
        .map(|(_, arity)| arity)
}

/// Every flag and positional in *args*, in one pass -- a token starting
/// with `-` that isn't exactly `-` (tolerated as a plain positional, the
/// conventional stdin/stdout placeholder, never a flag) and isn't in
/// *flags* ([`KNOWN_FLAGS`] for the legacy form, [`RUN_FLAGS`] under
/// `run`) is an `Err` naming it, worded `No such option: <flag>` --
/// `cof run`'s own Click-layer core text for the same case (issue
/// #728), minus the usage/help-hint lines and rich decorative panel
/// around it, which [`classify`]'s own `run` branch -- the only caller
/// this text is actually compared against -- renders itself (USAGE's
/// own electricity-cli DESIGN.md row). A value-taking flag with no
/// following argument is also an `Err` (this preview's own usage
/// error; cof's own Click-layer "Option '...' requires an argument."
/// is third-party CLI framework text, not Circuitry's own, so not
/// matched word for word).
struct ParsedFlags {
    version: bool,
    help: bool,
    dump_ir: bool,
    pretty: bool,
    quiet: bool,
    inputs: Vec<String>,
    out: Option<String>,
    live_state: Option<String>,
    events: Option<String>,
    profile: Option<String>,
    config: Option<String>,
    allow_capabilities: Option<String>,
    adapter: Option<String>,
    label: Option<String>,
    state: Option<String>,
    resume: Option<String>,
    force: bool,
    last: bool,
    positionals: Vec<String>,
}

fn parse_flags(
    args: &[String],
    flags: &'static [(&'static str, Arity)],
) -> Result<ParsedFlags, String> {
    let mut parsed = ParsedFlags {
        version: false,
        help: false,
        dump_ir: false,
        pretty: false,
        quiet: false,
        inputs: Vec::new(),
        out: None,
        live_state: None,
        events: None,
        profile: None,
        config: None,
        allow_capabilities: None,
        adapter: None,
        label: None,
        state: None,
        resume: None,
        force: false,
        last: false,
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
        let Some(arity) = flag_arity(flags, flag_name) else {
            if arg.starts_with('-') {
                return Err(format!("No such option: {arg}"));
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
                    "--quiet" => parsed.quiet = true,
                    "--force" => parsed.force = true,
                    "--last" => parsed.last = true,
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
                    "--out" | "-o" => parsed.out = Some(value),
                    "--live-state" => parsed.live_state = Some(value),
                    "--events" => parsed.events = Some(value),
                    "--profile" => parsed.profile = Some(value),
                    "--config" | "-c" => parsed.config = Some(value),
                    "--allow-capabilities" => parsed.allow_capabilities = Some(value),
                    "--adapter" => parsed.adapter = Some(value),
                    "--label" => parsed.label = Some(value),
                    "--state" | "-s" => parsed.state = Some(value),
                    "--resume" => parsed.resume = Some(value),
                    _ => unreachable!("every Arity::Value flag is handled above"),
                }
                i += if inline_value.is_some() { 1 } else { 2 };
            }
        }
    }
    Ok(parsed)
}

fn classify(args: &[String]) -> Action {
    // `electricity run <doc> ...` (issue #728): `run` must be the very
    // first token -- cof's own `run` is likewise the first word after
    // `cof`, never reachable after a flag -- so a config file actually
    // named `run` (no extension) stays reachable through the legacy
    // two-positional form below, just not as that form's *first*
    // argument. Routed *before* the legacy form's own `-V`/`--version`/
    // `-h`/`--help` pre-check below: unlike the legacy form (and unlike
    // the top-level `cof` group's own eager `--version`), `cof run`
    // itself has no `-V`/`--version`/`-h` of its own at all -- each is
    // itself an unrecognized option there -- and even `cof run --help`
    // loses to an unknown option anywhere else on the same line (cof's
    // own option parser raises on the first one it can't place while
    // scanning the whole line, before any eager callback, `--help`'s
    // included, ever runs), so `run`'s own flags are parsed with
    // [`RUN_FLAGS`] in full -- never short-circuited early -- before
    // `--help` is honoured.
    if args.first().map(String::as_str) == Some("run") {
        return classify_run(&args[1..]);
    }

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

    let parsed = match parse_flags(args, KNOWN_FLAGS) {
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

    // `--config` is `run`-only (issue #728's own config-discovery
    // seam): the legacy form's first positional already *is* the
    // config path, always explicit, so a second `--config` on the same
    // line is ambiguous rather than silently ignored -- USAGE's own
    // `` `run` only'' label for this flag is enforced here, not just
    // documented.
    if parsed.config.is_some() {
        return Action::UsageError(
            "--config is `run` only; the legacy form's first positional is the config path"
                .to_string(),
        );
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
            config: Some(config.clone()),
            orchestration: orchestration.clone(),
            inputs: parsed.inputs,
            out: parsed.out,
            pretty: parsed.pretty,
            live_state: parsed.live_state,
            events: parsed.events,
            profile: parsed.profile,
            allow_capabilities: parsed.allow_capabilities,
            adapter: parsed.adapter,
            label: None,
            quiet: parsed.quiet,
            state: None,
            resume: None,
            force: false,
            last: false,
            via_run: false,
        }),
        _ => Action::UsageError("no config file or orchestration given".to_string()),
    }
}

/// `electricity run <doc> ...` (issue #728), *args* being everything
/// after the literal `run` token -- [`RUN_FLAGS`], never [`KNOWN_FLAGS`]
/// ([`classify`]'s own doc comment on why `-V`/`--version`/`-h` are
/// excluded). Every usage error this returns is
/// [`Action::RunUsageError`], not [`Action::UsageError`] -- rendered in
/// `cof run`'s own plain-frame shape, not the legacy form's.
fn classify_run(args: &[String]) -> Action {
    let parsed = match parse_flags(args, RUN_FLAGS) {
        Ok(parsed) => parsed,
        Err(message) => return Action::RunUsageError(message),
    };
    if parsed.help {
        return Action::Help;
    }
    if parsed.dump_ir {
        return Action::RunUsageError(
            "--dump-ir is not available under `electricity run`; use `electricity \
             <config.json> <orchestration.yml> --dump-ir` instead"
                .to_string(),
        );
    }
    match parsed.positionals.as_slice() {
        // `cof run`'s own positional is `Optional[str]` precisely so
        // `--last` can stand in for it (issue #760) -- a bare `electricity
        // run --last` is `Action::Run` with an empty `orchestration`,
        // overwritten from the stash inside `run_action` before anything
        // reads it; only an *absent* `--last` still reaches
        // `MissingOrchestration`.
        [] if !parsed.last => Action::MissingOrchestration,
        [] => Action::Run(RunArgs {
            config: parsed.config,
            orchestration: String::new(),
            inputs: parsed.inputs,
            out: parsed.out,
            pretty: parsed.pretty,
            live_state: parsed.live_state,
            events: parsed.events,
            profile: parsed.profile,
            allow_capabilities: parsed.allow_capabilities,
            adapter: parsed.adapter,
            label: parsed.label,
            quiet: parsed.quiet,
            state: parsed.state,
            resume: parsed.resume,
            force: parsed.force,
            last: true,
            via_run: true,
        }),
        [orchestration] => Action::Run(RunArgs {
            config: parsed.config,
            // `cof run bogus.yml --last` ignores `bogus.yml` outright
            // (confirmed directly) -- `run_action`'s own `if
            // run_args.last` block overwrites this with the stash's own
            // value before anything reads it, same as the `[]` arm above,
            // so a positional given alongside `--last` here is harmless,
            // never validated against.
            orchestration: orchestration.clone(),
            inputs: parsed.inputs,
            out: parsed.out,
            pretty: parsed.pretty,
            live_state: parsed.live_state,
            events: parsed.events,
            profile: parsed.profile,
            allow_capabilities: parsed.allow_capabilities,
            adapter: parsed.adapter,
            label: parsed.label,
            quiet: parsed.quiet,
            state: parsed.state,
            resume: parsed.resume,
            force: parsed.force,
            last: parsed.last,
            via_run: true,
        }),
        // `click.exceptions.UsageError`'s own literal template keeps
        // `(s)` regardless of how many extras there are (Click 8.5's
        // `"Got unexpected extra argument{s} ({args})"`, `s="(s)"`
        // either way) -- every extra beyond the one orchestration
        // positional is named, space-joined, matching `cof run a.yml
        // b.yml` naming only `b.yml` (the first positional is consumed
        // as *orchestration*, same as cof's own single `[orchestration]`
        // argument).
        [_, extra, more @ ..] => {
            let mut extras = vec![extra.as_str()];
            extras.extend(more.iter().map(String::as_str));
            Action::RunUsageError(format!(
                "Got unexpected extra argument(s) ({})",
                extras.join(" ")
            ))
        }
    }
}

/// The POSIX signal number [`arm_signals`]'s own task calls
/// [`CancellationToken::request`] with for each signal it watches --
/// stable across Linux and macOS (`man 7 signal`), and the same three
/// numbers [`electricity::run::Signal::events_name`]/[`Signal::
/// exit_code`] key off of.
const SIGINT: i32 = 2;
const SIGHUP: i32 = 1;
const SIGTERM: i32 = 15;

/// Whether SIGHUP is already ignored (`SIG_IGN`) on entry -- a caller
/// that launched `electricity` under `nohup` means it, and this task
/// must never install its own handler over that (issue #431's Signals
/// section: "honours `SIG_IGN` at start"). Queried directly via
/// `sigaction`, never changing the disposition itself (the *old*
/// argument is `null`).
fn sighup_is_already_ignored() -> bool {
    #[cfg(unix)]
    unsafe {
        let mut current: libc::sigaction = std::mem::zeroed();
        if libc::sigaction(libc::SIGHUP, std::ptr::null(), &mut current) != 0 {
            return false;
        }
        current.sa_sigaction == libc::SIG_IGN
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// A dedicated OS thread, with its own minimal Tokio runtime, watching
/// SIGINT/SIGTERM/SIGHUP from the moment it's installed (PR #441) --
/// installed *before* config load, so this stays
/// responsive even while the main thread is deep in synchronous work
/// (config/document load, every pre-execution check) with no `await`
/// point of its own for a signal to be noticed at.
///
/// The first SIGINT/SIGTERM/SIGHUP calls [`CancellationToken::request`];
/// a second SIGINT/SIGTERM exits the whole process immediately with its
/// own conventional code (no `--out`, no `run_end` -- issue #431's
/// Signals section). SIGHUP is never treated as that second signal, and
/// is never even watched for at all when [`sighup_is_already_ignored`]
/// says it already is -- both rules live in
/// [`CancellationToken::request`]'s own "first signal wins" semantics
/// (any later call, same or different signum, returns `false`) plus
/// this function's own `continue` on a not-first SIGHUP, so a SIGHUP
/// arriving after an earlier signal already cancelled the run --
/// including one arriving after `execute_root` returns, while `--out`
/// is still being written -- is ignored the same way, with no separate
/// "are we past `--out` yet" check needed.
struct SignalGuard {
    stop: std::sync::Arc<tokio::sync::Notify>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl SignalGuard {
    /// Stops watching ("signals armed for the run only") and waits for
    /// the thread to actually exit, so a caller that's about to print
    /// this run's own result never races a signal thread still capable
    /// of calling `std::process::exit` out from under it. Called once
    /// `--out` has been written -- not merely once `run_orchestration`
    /// returns -- so a SIGHUP during that write is still ignored,
    /// matching [`arm_signals`]'s own doc comment.
    fn disarm(mut self) {
        self.stop.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn arm_signals(token: CancellationToken) -> SignalGuard {
    let stop = std::sync::Arc::new(tokio::sync::Notify::new());
    let stop_for_thread = stop.clone();
    // A rendezvous, not just a fire-and-forget spawn: the real
    // `sigaction` calls below only happen once the new thread's own
    // runtime schedules this async block, which the OS is free to
    // delay arbitrarily under load -- without waiting for `ready` here,
    // a signal arriving in that window would still hit the OS's
    // default (process-killing) disposition even though this function
    // had already returned, telling its caller the run was safe to
    // start. Confirmed by a real race on a loaded machine during this
    // function's own review (PR #441): a SIGINT sent as little as a
    // few hundred milliseconds after a plain, unguarded `spawn` could
    // still arrive before the handler was installed.
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    let thread = std::thread::Builder::new()
        .name("electricity-signals".to_string())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("could not start the signal-handling thread's own runtime");
            runtime.block_on(async move {
                use tokio::signal::unix::{SignalKind, signal};
                let mut sigint =
                    signal(SignalKind::interrupt()).expect("could not install a SIGINT handler");
                let mut sigterm =
                    signal(SignalKind::terminate()).expect("could not install a SIGTERM handler");
                let mut sighup = if sighup_is_already_ignored() {
                    None
                } else {
                    Some(signal(SignalKind::hangup()).expect("could not install a SIGHUP handler"))
                };
                // Every `sigaction` above has now run -- safe to let
                // `arm_signals` return.
                let _ = ready_tx.send(());
                loop {
                    let signum = match sighup.as_mut() {
                        Some(sighup) => {
                            tokio::select! {
                                _ = stop_for_thread.notified() => return,
                                _ = sigint.recv() => SIGINT,
                                _ = sigterm.recv() => SIGTERM,
                                _ = sighup.recv() => SIGHUP,
                            }
                        }
                        None => {
                            tokio::select! {
                                _ = stop_for_thread.notified() => return,
                                _ = sigint.recv() => SIGINT,
                                _ = sigterm.recv() => SIGTERM,
                            }
                        }
                    };
                    let first = token.request(signum);
                    if !first {
                        if signum == SIGHUP {
                            // Never a second signal (issue #431's Signals
                            // section) -- a closed terminal can deliver SIGHUP
                            // twice in quick succession, and the second must
                            // not race whichever signal actually cancelled this
                            // run to `std::process::exit`.
                            continue;
                        }
                        std::process::exit(match signum {
                            SIGINT => Signal::Sigint.exit_code(),
                            SIGTERM => Signal::Sigterm.exit_code(),
                            _ => unreachable!("only SIGINT/SIGTERM reach this branch"),
                        });
                    }
                }
            });
        })
        .expect("could not start the signal-handling thread");
    ready_rx
        .recv()
        .expect("the signal-handling thread's own ready signal");
    SignalGuard {
        stop,
        thread: Some(thread),
    }
}

/// Writes *text* to stdout, ignoring any error -- `print!`/`println!`
/// panic on a write error (a closed terminal after SIGHUP, a closed
/// pipe downstream), which would report this run's own exit code as
/// 101 instead of whichever one its own result earned. DESIGN.md §6.5:
/// a stdout/stderr write failure "never stops cleanup, `--out` or the
/// exit code" -- this is the one place that promise is kept, since
/// every other caller in this file goes through here rather than the
/// panicking macros directly.
fn write_stdout(text: &str) {
    let _ = std::io::stdout().lock().write_all(text.as_bytes());
}

/// [`write_stdout`]'s own stderr counterpart.
fn write_stderr(text: &str) {
    let _ = std::io::stderr().lock().write_all(text.as_bytes());
}

/// `cli/logging_setup.py::configure_cli_logging`'s own stderr handler --
/// WARNING and above (`cof run`'s default, never-`--verbose` level:
/// electricity has no `--verbose` flag of its own) formatted as
/// `{levelname}: {message}`, the same `logging.Formatter` the
/// reference's own CLI installs -- written straight through
/// [`write_stderr`], which never buffers across calls, so this is
/// flushed per line the same way Python's own StreamHandler is. Every
/// `log::warn!` this binary's own library dependencies call
/// (electricity-cel's absent-path warning; electricity-vm's
/// dynamic/conditional on_error degradation warnings;
/// electricity-config's "Unknown environment" warning; electricity's
/// own --live-state/--events mid-run write failures) reaches stderr
/// through this one sink (issue #442) -- a no-op until this is
/// installed, same as the reference's own NullHandler default for an
/// embedding caller that never calls configure_cli_logging. Filtered
/// to this workspace's own crates (`enabled`'s own `target`-prefix
/// check): `cli/logging_setup.py`'s own handler sits on Circuitry's
/// own `"circuitry"` logger alone, never the root logger, so a
/// dependency that happens to log doesn't reach `cof run`'s stderr
/// either -- `log`'s `target` defaults to the logging call site's own
/// module path, which for every crate in this workspace starts with
/// `electricity` (hyphens become underscores in a Rust module path),
/// so this is the narrowest prefix that admits all of them and
/// nothing outside this workspace.
struct StderrWarnLogger;

impl log::Log for StderrWarnLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Warn && metadata.target().starts_with("electricity")
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        // `Warn` is the only level any of this binary's own
        // dependencies ever actually emit today (`enabled` above
        // already excludes Info/Debug/Trace) -- Error is handled for
        // forward compatibility with a future log::error! site,
        // matching Python's own logging.WARNING-level threshold
        // admitting both.
        let levelname = match record.level() {
            log::Level::Error => "ERROR",
            log::Level::Warn => "WARNING",
            log::Level::Info => "INFO",
            log::Level::Debug | log::Level::Trace => "DEBUG",
        };
        write_stderr(&format!("{levelname}: {}\n", record.args()));
    }

    fn flush(&self) {}
}

static WARN_LOGGER: StderrWarnLogger = StderrWarnLogger;

/// Installs [`StderrWarnLogger`] as the `log` crate's global logger, at
/// WARNING and above -- called once, at the very start of [`main`],
/// before any config/document loading that could emit one of these
/// warnings runs.
fn install_warning_logger() {
    let _ = log::set_logger(&WARN_LOGGER);
    log::set_max_level(log::LevelFilter::Warn);
}

/// #471, belt and braces: `user_env::load_user_env`
/// already checks a key/value before ever calling `std::env::set_var`
/// (the one call in this binary's startup path that could panic with
/// a `.env` value in its own message, `failed to set environment
/// variable {key:?} to {value:?}`), so this should never actually
/// fire for that reason -- but a panic anywhere else on the startup
/// path, before `main` has done anything else, would otherwise print
/// through Rust's default hook, which includes the panic's own
/// payload verbatim. Installed first, before even
/// [`install_warning_logger`], so nothing between process start and
/// here can panic through the default hook instead. The payload is
/// never inspected, let alone printed, downcast to `&str` or not --
/// only the location, which is this binary's own source, never a
/// `.env` value.
fn install_secret_safe_panic_hook() {
    std::panic::set_hook(Box::new(|info| match info.location() {
        Some(location) => write_stderr(&format!("electricity panicked at {location}\n")),
        None => write_stderr("electricity panicked\n"),
    }));
}

/// `electricity`'s own non-TTY stdout contract (issue #431's "CLI
/// output" decision, "Same as `cof` when stdout is not a terminal"):
/// success with `--out` prints nothing; success without `--out` prints
/// the state JSON; a failure prints `{"ok": false, ...}` regardless of
/// `--out`. `--pretty` governs only the success-without-`--out` case --
/// `--out`'s own file is `electricity::out::write_out`'s job, not this
/// function's. A config error ([`run_action`]'s own early exit) never
/// reaches this function at all -- Circuitry's own `CircuitryGroup.
/// invoke` catches a `ConfigError` *around* the whole CLI command,
/// before `run()`'s own JSON-output logic is ever reached, so a config
/// error prints nothing on stdout, not even the failure payload.
///
/// Returns `Err` when a successful run's own state can't be serialized
/// for stdout (issue #512) -- a value `json_safe_
/// default`/`dumps_default_str` still can't encode (a non-string dict
/// key, ...), distinct from the `Date`/`DateTime`/`Bytes` fallback both
/// engines already handle. Nothing is written to stdout in that case;
/// the caller reports it on stderr and exits nonzero instead, the same
/// as `cof run --json` crashing uncaught on the same document rather
/// than printing a placeholder and exiting 0.
fn print_stdout_contract(
    result: &RunResult,
    out_path: Option<&Path>,
    pretty: bool,
) -> Result<(), String> {
    if !result.ok {
        // `state_out` names *out_path* only when a file was actually
        // written there -- a refusal (or any other `state: None`
        // failure) leaves `--out` unwritten even when `--out` was given
        // (PR #441), so this is `None` whenever
        // `result.state` is, regardless of what the caller asked for.
        let state_out = out_path.filter(|_| result.state.is_some());
        write_stdout(&electricity::out::failure_payload(
            result.error.as_deref().unwrap_or(""),
            &result.warnings,
            state_out,
        ));
        return Ok(());
    }
    if out_path.is_none() {
        if let Some(state) = &result.state {
            let rendered = electricity::out::render_state_for_stdout(state, pretty)
                .map_err(|err| err.to_string())?;
            write_stdout(&rendered);
            write_stdout("\n");
        }
    }
    Ok(())
}

/// `Warning: ...` lines, and nothing else -- issue #431's "CLI output"
/// decision's own stderr contract (PR #441): a
/// run failure's own error already went out on stdout, inside the
/// `{"ok": false, ...}` payload [`print_stdout_contract`] just wrote --
/// `cli/app.py::run`'s own non-TTY failure path (`json_out`) prints
/// nothing on stderr beyond `_print_run_warnings`'s own `Warning:`
/// lines, never a second `Error:` line too. A bare `Error: <text>` on
/// stderr stays reserved for the two checks that fail *before* `run()`
/// is ever reached at all -- a config error and a missing orchestration
/// (`run_action`'s own early-exit branches, both of which `return`
/// before this function is ever called) -- exactly where Circuitry's
/// own `CircuitryGroup.invoke`/CLI-layer checks report them, on stdout
/// or plain stderr text, never through this JSON-failure path at all.
///
/// `result.warnings` only ever holds this run's own `--events`/
/// `--live-state` write-failure warnings -- the mid-run `WARNING: ...`
/// lines Circuitry's own Python `logging` module emits
/// (`core/dynamic.py`'s/`core/conditional.py`'s own `on_error: skip`/
/// `continue` degradation, a `finally:` that fails after the body
/// already did, `core/tool.py`'s own invalid-timeout warning, ...) take
/// a different path to stderr: each site logs through `log::warn!` at
/// the point it happens (not collected into `RunResult.warnings` at
/// all), and [`StderrWarnLogger`] -- installed once, in [`main`],
/// before this function or [`run_orchestration_with_config`] ever runs
/// -- is what actually writes each one, as `WARNING: <text>`, the
/// moment it is logged (issue #442's "Warning lines on stderr" item).
/// `tests/warnings.rs` and `tests/conformance` both compare those
/// lines against a real `cof run`'s own stderr.
fn print_stderr_contract(result: &RunResult) {
    for warning in &result.warnings {
        write_stderr(&format!("Warning: {warning}\n"));
    }
}

/// The process exit code for *result* (issue #431's run-wiring step
/// 21): 0 on success; a signal's own conventional code when one ended
/// the run; 1 for every other failure.
fn exit_code_for(result: &RunResult) -> ExitCode {
    if result.ok {
        return ExitCode::SUCCESS;
    }
    match result.signal {
        Some(signal) => ExitCode::from(signal.exit_code() as u8),
        None => ExitCode::from(1),
    }
}

/// `cof run`'s own usage-error frame, plain: `Usage:`, `Try ... --help`,
/// a blank line, then `Error: <message>` -- `UsageError.show`'s own
/// shape minus two things DESIGN.md's own row says this build does not
/// reproduce: the invoking program's own name (`cof`'s own argv[0] --
/// `cof`, or `python -m circuitry.cli.app` under the test harness,
/// never fixed across environments either), and the Rich panel
/// (`typer`'s own `rich_markup_mode="rich"`) real `cof run` wraps
/// `Error: ...` in instead of a plain line -- a box whose own width
/// depends on the terminal/`COLUMNS`, so even two real `cof run`
/// invocations don't always render it identically, and a close-match
/// suggestion list (`Possible options: ...`) this hand-written parser
/// does not attempt. *message* is compared against the corresponding
/// substring of `cof run`'s own panel contents, not this whole frame.
fn print_run_usage_error(message: &str) {
    write_stderr(&format!(
        "Usage: electricity run [OPTIONS] [orchestration]\n\
         Try 'electricity run --help' for help.\n\
         \n\
         Error: {message}\n"
    ));
}

/// `--allow-capabilities a,b` -> `["a", "b"]` -- `cli/app.py::
/// _parse_allow_capabilities`'s own split/trim/drop-empty, minus its
/// `frozenset` dedup -- done instead by
/// `electricity_config::allowlist::capability_allow`, which both sorts
/// and deduplicates before this ever reaches `_capability_allow`/the
/// capability-consent gate, the same two-step split `cli/app.py` and
/// `cli/runtime_shim.py::run` make between them.
fn parse_allow_capabilities(value: Option<&str>) -> Vec<String> {
    let Some(value) = value else {
        return Vec::new();
    };
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

fn run_action(mut run_args: RunArgs) -> ExitCode {
    // `cof run`'s own very first check in `run_cmd`, before anything
    // else -- including config/orchestration resolution, both of which
    // `--last`'s own stash load below needs to run first (issue #760).
    if run_args.resume.is_some() && run_args.last {
        write_stdout("Error: --resume and --last are mutually exclusive.\n");
        return ExitCode::from(1);
    }

    // `--last` (issue #760): replay the most recent stashed run's own
    // arguments, the same file/keys `cof run` reads
    // (`cli/last_run.py::read_last_run`) -- before config/orchestration
    // resolution, since the stash itself names both (`cli/app.py::
    // run_cmd`'s own `if last:` block runs first, at ~:1124).
    if run_args.last {
        let home = std::env::home_dir();
        let stash = home
            .as_deref()
            .map(electricity_config::last_run_path)
            .and_then(|path| electricity_config::read_last_run(&path));
        let stash = match stash {
            Some(stash) => stash,
            None => {
                // `cof run --last` with no stash at all raises a bare
                // `typer.BadParameter` that nothing in `run_cmd` catches --
                // Click's own top-level handler renders it as a usage
                // error (exit 2), not the exit-1 `console.print`-plus-
                // `typer.Exit` shape every *other* refusal in this
                // function uses (confirmed directly against a real
                // `cof run --last`).
                print_run_usage_error(
                    "Invalid value: No previous run found. Run an orchestration first.",
                );
                return ExitCode::from(2);
            }
        };
        if !stash.ok() {
            // `cof run --last`'s own `_load_last_run` has no graceful
            // handling at all for a present-but-unreadable/malformed
            // stash (an uncaught `JSONDecodeError`/`KeyError`, a Python
            // traceback on stderr, exit 1) -- not one of issue #760's
            // own listed refusals (that list is `--resume last`'s,
            // which *does* read the stash gracefully), so this reports
            // it cleanly instead of reproducing a traceback.
            write_stdout(&format!("Error: {}\n", stash.error));
            return ExitCode::from(1);
        }
        if stash.has_redacted_secrets() {
            write_stdout("Error: previous run included redacted secrets in -e values.\n");
            write_stdout(
                "Re-run explicitly with the secret supplied via env var or config file \
                 (recommended), or via -e for this invocation.\n",
            );
            return ExitCode::from(1);
        }
        // `cof run`'s own `if last:` block overwrites every one of these
        // unconditionally from the stash -- including `orchestration`,
        // silently ignoring whatever positional this invocation itself
        // gave (confirmed directly: `cof run bogus.yml --last` replays
        // the stash, never refusing on `bogus.yml` not existing).
        run_args.orchestration = stash.orchestration();
        run_args.config = stash.config_path().map(path_to_string);
        run_args.state = stash.state_path().map(path_to_string);
        run_args.inputs = stash.env_pairs();
        run_args.out = stash.out_path().map(path_to_string);
        run_args.pretty = stash.pretty();
        run_args.live_state = stash.live_state_path().map(path_to_string);
        run_args.events = stash.events_path().map(path_to_string);
        run_args.label = stash.label();
        run_args.quiet = stash.quiet();
        run_args.allow_capabilities = stash.allow_capabilities();
        run_args.profile = stash.profile();
        run_args.adapter = stash.adapter();
    }

    // Armed *before* config load (issue
    // #431's own step 1), on a dedicated OS thread with its own Tokio
    // runtime -- responsive to a signal even while this thread is deep
    // in the synchronous config/document load and every pre-execution
    // check that follows, none of which ever awaits anything of its
    // own. Disarmed only once `--out` has actually been written, below.
    let token = CancellationToken::new();
    let signal_guard = arm_signals(token.clone());

    // Step 1 of issue #431's run-wiring table: a config error exits 1
    // with Circuitry's own text, on stderr alone -- Circuitry's
    // `CircuitryGroup.invoke` catches a `ConfigError` *around* the
    // whole CLI command, before `run()`'s own JSON-output logic is
    // ever reached, so unlike every other failure this prints no
    // stdout payload at all, `--out` or not (PR #441).
    // `config_error_for_run` resolves config exactly once for this
    // whole invocation, at `cof run`'s own point in its resolution
    // order (`cli/app.py` resolves config at ~:1127, before the
    // missing-orchestration/bad-`-e` checks below) -- so logging stays
    // on here: a resolve-time warning (electricity-config's own
    // "Unknown environment ...", issue #442) must print even when one
    // of those checks then ends the run early, exactly as it would for
    // `cof run`. The resolved config is threaded through to
    // `run_orchestration_with_config` below rather than resolved a
    // second time, so that warning never fires twice. `run_args.config`
    // is `None` for `electricity run` without `--config` (issue #728):
    // `config_error_for_run` then discovers it (`CIRCUITRY_CONFIG`, a
    // trusted project config in *cwd*, the global config), the same
    // branch the legacy two-positional form -- always `Some` -- never
    // takes.
    let explicit_config_path = run_args.config.as_ref().map(PathBuf::from);
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let (cfg, config_warnings) =
        match electricity::config_error_for_run(explicit_config_path.as_deref(), &cwd) {
            Ok(resolved) => resolved,
            Err(message) => {
                signal_guard.disarm();
                write_stderr(&format!("Error: {message}\n"));
                return ExitCode::from(1);
            }
        };

    // `cof`'s own "Orchestration not found" check (`_resolve_orchestration`)
    // happens in the CLI layer *before* `-e` is ever parsed
    // (`cli/app.py` resolves the document at ~:1150, `_parse_env_vars`
    // only at ~:1270) -- checked here, before step 2 below, for the
    // same reason (PR #441: `-e badtext` plus a
    // missing document must still exit 1, not 2). Unlike the config-
    // error check above, `cof` prints this one on *stdout*
    // (`console.print`, not `err_console.print` -- `app.py:1156`), so
    // this does too; electricity's own positional orchestration
    // argument is always a literal path (no library-name resolution,
    // unlike `cof run <name>`), so this is a plain existence check. The
    // accompanying `Tip: run cof list ...` line cof also prints is left
    // out on purpose: electricity has no `list` subcommand of its own
    // for it to name.
    let orchestration_path = PathBuf::from(&run_args.orchestration);
    if !orchestration_path.is_file() {
        signal_guard.disarm();
        // `cli/app.py::run_cmd`'s own `_print_run_warnings(cfg.
        // resolution_warnings())`, right before its own `Tip: ...` line
        // (electricity's own note just above on why that line itself
        // isn't printed): a skipped project config's warning must still
        // reach stderr here, not only on a run that gets far enough to
        // reach `print_stderr_contract`'s own copy of the same text.
        for warning in &config_warnings {
            write_stderr(&format!("Warning: {warning}\n"));
        }
        write_stdout(&format!(
            "Error: Orchestration not found: {}\n",
            run_args.orchestration
        ));
        return ExitCode::from(1);
    }

    // Step 2: `electricity::parse_inputs`'s own malformed-`-e` text,
    // Circuitry's own `BadParameter` message word for word (issue
    // #429): `parse_flags` only checks that `-e` has *some* value,
    // saying nothing about whether that value itself contains `=` -- a
    // value with no `=` (`-e badtext`) reaches this, `cli/app.py::
    // _parse_env_vars`'s own check proper.
    let inputs = match electricity::parse_inputs(&run_args.inputs) {
        Ok(inputs) => inputs,
        Err(message) => {
            signal_guard.disarm();
            if run_args.via_run {
                // `click.BadParameter`'s own `Invalid value: <message>`
                // prefix (`cof run`'s own panel contents for the same
                // malformed `-e` entry) -- `message` itself already
                // matches `_parse_env_vars`'s own text word for word.
                print_run_usage_error(&format!("Invalid value: {message}"));
            } else {
                write_stderr(&format!("electricity: {message}\n{USAGE}\n"));
            }
            return ExitCode::from(2);
        }
    };

    // Only the literal flag -- DESIGN.md §6.8's own "--resume with no
    // explicit --out writes back to the file it resumed from" default
    // ranks *below* a loaded `--profile`'s own `out:` (`cli/
    // effective_settings.py::resolve_effective_settings`'s own `cli >
    // profile > resume source > default` order), so it is passed to
    // `RunRequest::resume_default_out` instead and applied inside
    // `electricity` (lib)'s own effective-settings resolution, at that
    // tier, not folded into this CLI-level `out_path` (which ranks
    // above a profile unconditionally).
    let out_path = run_args.out.clone().map(PathBuf::from);

    // `--resume` (issue #727, extended by #760): `--state <file>` names
    // the saved state to resume from directly; `--resume last` resolves
    // it through the same `--last` stash `--last` itself reads
    // (`cli/last_run.py::read_last_run`) -- the prior run's own `--out`.
    // A bare run-id (`cof run`'s own `runtime.persistence` lookup) stays
    // refused. Every refusal below fails the same way `cof run`'s own
    // `_resolve_resume_state` does for all three sources -- a pre-run
    // `typer.BadParameter` the caller re-raises as `typer.Exit(code=1)`,
    // writing no state at all (DESIGN.md §6.9's own exit-code table) --
    // through the ordinary `RunResult`/`exit_code_for` path, exit 1,
    // never `ExitCode::from(2)`.
    let mut resume_state_path = run_args.state.clone().map(PathBuf::from);
    let mut resume_default_out_path: Option<PathBuf> = None;
    if run_args.resume.is_some() {
        let refusal = if run_args.state.is_some() {
            resume_default_out_path = run_args.state.clone().map(PathBuf::from);
            None
        } else if run_args.resume.as_deref() == Some("last") {
            let home = std::env::home_dir();
            let stash = home
                .as_deref()
                .map(electricity_config::last_run_path)
                .and_then(|path| electricity_config::read_last_run(&path));
            match &stash {
                None => Some("No previous run found \u{2014} nothing to resume from.".to_string()),
                Some(stash) if !stash.ok() => {
                    Some(format!("{} Nothing to resume from.", stash.error))
                }
                Some(stash) => {
                    let stashed_orchestration = stash.orchestration();
                    if stashed_orchestration != run_args.orchestration {
                        Some(format!(
                            "The last run was '{stashed_orchestration}', not {} \u{2014} run \
                             that orchestration again, or pass --state <file> to resume a \
                             specific saved state instead of 'last'.",
                            run_args.orchestration
                        ))
                    } else {
                        match stash.out_path() {
                            None => Some(
                                "The last run did not use --out, so it saved no state to \
                                 resume from."
                                    .to_string(),
                            ),
                            Some(stash_out_path) if !stash_out_path.exists() => Some(format!(
                                "The last run's --out file is gone: state file not found: {}",
                                stash_out_path.display()
                            )),
                            Some(stash_out_path) => {
                                resume_state_path = Some(stash_out_path.clone());
                                resume_default_out_path = Some(stash_out_path);
                                None
                            }
                        }
                    }
                }
            }
        } else {
            // `cof run --resume <run-id>` needs the orchestration's own
            // `runtime.persistence` backend, which stays refused on
            // electricity.
            Some(
                "--resume <run-id> needs this orchestration's runtime.persistence backend, \
                 which stays refused on electricity -- pass --state <file> naming a saved \
                 state directly instead."
                    .to_string(),
            )
        };
        if let Some(message) = refusal {
            signal_guard.disarm();
            let result = RunResult {
                ok: false,
                out_path: None,
                state: None,
                error: Some(message),
                warnings: config_warnings.clone(),
                signal: None,
            };
            if let Err(err) = print_stdout_contract(&result, None, run_args.pretty) {
                print_stderr_contract(&result);
                write_stderr(&format!(
                    "electricity: could not serialize run state: {err}\n"
                ));
                return ExitCode::from(1);
            }
            print_stderr_contract(&result);
            return exit_code_for(&result);
        }
    }

    let req = RunRequest {
        config_path: explicit_config_path,
        orchestration_path,
        inputs,
        out_path: out_path.clone(),
        pretty: run_args.pretty,
        live_state_path: run_args.live_state.clone().map(PathBuf::from),
        events_path: run_args.events.clone().map(PathBuf::from),
        profile_path: run_args.profile.clone().map(PathBuf::from),
        allow_capabilities: parse_allow_capabilities(run_args.allow_capabilities.as_deref()),
        config_warnings,
        adapter_override: run_args.adapter.clone(),
        label_override: run_args.label.clone(),
        armed: true,
        state_path: resume_state_path,
        resume: run_args.resume.is_some(),
        resume_default_out: resume_default_out_path.clone(),
        force: run_args.force,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("could not start the electricity async runtime");
    let result = runtime.block_on(electricity::run_orchestration_with_config(
        cfg, &req, &token,
    ));
    // Issue #721's own follow-up: a `service` teardown or stop a
    // cancelled run's own `select!` dropped mid-flight keeps running
    // on a task spawned off that drop, tracked for exactly this join --
    // on the same runtime the run itself used, so its own reactor
    // (timers, the state directory's lock) keeps driving it forward.
    // A no-op whenever nothing is outstanding, which is every run that
    // never touches the `service` tool, or finishes without being cut
    // short mid-signal.
    runtime.block_on(electricity::join_pending_teardowns());

    // `--out` outranks a loaded profile's own `out:` (DESIGN §6.11,
    // issue #495's own seam on `RunResult::out_path`) -- a profile's
    // `out:` is honoured only when no `--out` was given.
    let effective_out_path = out_path.or_else(|| result.out_path.clone());

    if let Some(path) = &effective_out_path {
        if let Some(state) = &result.state {
            if let Err(err) = electricity::out::write_out(path, state, run_args.pretty) {
                signal_guard.disarm();
                write_stderr(&format!(
                    "electricity: could not write --out {}: {err}\n",
                    path.display()
                ));
                return ExitCode::from(1);
            }
        }
    }

    // Stash for `--last`/`--resume last` (issue #760) -- success or
    // failure, same as `cof run`'s own `_save_last_run` (`cli/app.py`'s
    // own comment: "a failed run must leave a stash too, or `--resume
    // last` would have no record to find its --out through"), skipped
    // only when this run itself replayed one via `--last` (`cli/app.py`'s
    // own `if not last:` guard) -- a replay must never overwrite the
    // stash it just read, confirmed directly against a real `cof run
    // --last`. Best-effort: `electricity_config::write_last_run` already
    // swallows a write failure silently, exactly as the reference does.
    if !run_args.last {
        let home = std::env::home_dir();
        if let Some(path) = home.as_deref().map(electricity_config::last_run_path) {
            electricity_config::write_last_run(
                &path,
                last_run_stash_args(&run_args, resume_default_out_path.as_deref()),
            );
        }
    }

    // "Signals armed for the run only" (issue #431's Signals section):
    // stopped only now, after `--out` has actually been written, so a
    // SIGHUP during that write is still ignored rather than hitting
    // the OS's own default disposition (`arm_signals`'s own doc
    // comment).
    signal_guard.disarm();

    if let Err(err) = print_stdout_contract(&result, effective_out_path.as_deref(), run_args.pretty)
    {
        print_stderr_contract(&result);
        write_stderr(&format!(
            "electricity: could not serialize run state: {err}\n"
        ));
        return ExitCode::from(1);
    }
    print_stderr_contract(&result);
    exit_code_for(&result)
}

/// Issue #478's own exit-path wiring: no document can make a
/// `scripted` adapter load until lane G flips `prompt`, so `tests/
/// leftover_export.rs` has no other way to exercise
/// `electricity::write_leftover_export`'s own real call site end to
/// end. When `test-tools` is enabled (never in a plain build) and
/// `ELECTRICITY_TEST_SCRIPTED_REGISTER_REPLIES_FILE` names a replies
/// file, this builds a real `scripted` adapter over it and calls
/// `check()` -- the same lazy-load-then-register path `generate()`
/// takes (`ScriptedAdapter::ensure_loaded`), registering a full,
/// untouched instance for the leftover-replies export before anything
/// else in this process runs. A malformed replies file, or one this
/// process can't read, is silently skipped -- this hook exists only to
/// drive the export's own wiring, never to make the run itself fail on
/// a test fixture's own mistake.
#[cfg(feature = "test-tools")]
fn register_test_only_scripted_adapter() {
    const VAR: &str = "ELECTRICITY_TEST_SCRIPTED_REGISTER_REPLIES_FILE";
    let Some(replies_file) = std::env::var_os(VAR) else {
        return;
    };
    let mut cfg = electricity_value::Dict::new();
    cfg.insert(
        electricity_value::Value::Str("replies_file".to_string()),
        electricity_value::Value::Str(replies_file.to_string_lossy().into_owned()),
    );
    let Ok(adapter) = electricity_adapters::scripted::build_scripted(
        "scripted",
        &electricity_value::Value::Dict(cfg),
    ) else {
        return;
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("a tiny current-thread runtime for this test-only check() call");
    runtime.block_on(adapter.check());
}

#[cfg(not(feature = "test-tools"))]
fn register_test_only_scripted_adapter() {}

fn main() -> ExitCode {
    install_secret_safe_panic_hook();
    install_warning_logger();
    register_test_only_scripted_adapter();
    // `args_os` + lossy conversion instead of `args()`, which panics on a
    // non-UTF-8 argument.
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let action = classify(&args);
    // Issue #468: `cof`'s own root callback (`cli/app.py::_root`) loads
    // the user's private `.env` for every subcommand, *before* that
    // subcommand's own argument parsing can fail with a usage error --
    // Click has already run the group callback by the time a
    // subcommand's own missing/invalid argument raises (#471).
    // The one exception is `-V`/`-h`: `_root`'s `--version`
    // callback is `is_eager=True`, short-circuiting before `_root`'s
    // own body -- `cof --version` never loads the file or prints its
    // warnings, so neither does this. Loading here, once, before
    // `Action` is even matched on, also replaces the two separate
    // `user_env::load_user_env()` calls `run_action`/`Action::DumpIr`
    // used to make themselves -- still before the signal-handling
    // thread or the Tokio runtime [`run_action`] goes on to create,
    // the one place in this binary's lifetime mutating the process
    // environment from a single thread is always sound.
    if !matches!(action, Action::Version | Action::Help) {
        user_env::load_user_env();
    }
    let exit_code = match action {
        Action::Version => {
            write_stdout(&format!("{}\n", electricity::version_string()));
            ExitCode::SUCCESS
        }
        Action::Help => {
            write_stdout(&format!("{USAGE}\n"));
            ExitCode::SUCCESS
        }
        Action::Run(run_args) => run_action(run_args),
        // `cli/app.py::run_cmd`'s own check (issue #728): "Error:
        // Missing orchestration. Use --last or provide a path/name.",
        // on stdout (`console.print`, not `err_console.print`), exit
        // 1 -- not a usage error (exit 2), since cof's own positional
        // is `Optional[str]` precisely to let `--last` stand in for
        // it. electricity has no `--last` of its own (#727's own
        // scope) and omits cof's accompanying `cof list` tip, same as
        // the legacy form's own "Orchestration not found" message
        // does, for the same reason.
        Action::MissingOrchestration => {
            write_stdout("Error: Missing orchestration. Use --last or provide a path/name.\n");
            ExitCode::from(1)
        }
        Action::DumpIr(config_path, orchestration_path, raw_inputs) => {
            match electricity::parse_inputs(&raw_inputs) {
                Ok(inputs) => match electricity::dump_ir(
                    Path::new(&config_path),
                    Path::new(&orchestration_path),
                    &inputs,
                ) {
                    Ok(json) => {
                        write_stdout(&format!("{json}\n"));
                        ExitCode::SUCCESS
                    }
                    Err(message) => {
                        write_stderr(&format!("{message}\n"));
                        ExitCode::from(1)
                    }
                },
                Err(message) => {
                    write_stderr(&format!("electricity: {message}\n{USAGE}\n"));
                    ExitCode::from(2)
                }
            }
        }
        Action::UsageError(message) => {
            write_stderr(&format!("electricity: {message}\n{USAGE}\n"));
            ExitCode::from(2)
        }
        Action::RunUsageError(message) => {
            print_run_usage_error(&message);
            ExitCode::from(2)
        }
    };
    // The leftover-replies export (electricity/docs/spec/scripted-
    // replies.md §7, issue #478): Circuitry's own `atexit` hook runs
    // on every normal interpreter exit -- 0, 1, 2, and the *first*
    // signal's own handled exit (`cli/interrupts.py`'s cooperative
    // cancellation, which still reaches `sys.exit`) -- but never on a
    // *second* SIGINT/SIGTERM's `os._exit`, which skips `atexit`
    // entirely. This one call site covers the same set: every path
    // through `main` ends up here (including every signal exit
    // `run_action` itself returns for, via `exit_code_for`), while a
    // second signal's own hard exit
    // (`arm_signals`'s own `std::process::exit` call) never reaches
    // this function at all.
    electricity::write_leftover_export();
    exit_code
}

#[cfg(test)]
mod unit_tests {
    use super::*;
    use log::Log;

    fn warn_metadata(target: &str) -> log::Metadata<'_> {
        log::Metadata::builder()
            .level(log::Level::Warn)
            .target(target)
            .build()
    }

    #[test]
    fn the_warning_logger_admits_this_workspaces_own_crate_targets() {
        for target in [
            "electricity_vm::exec::dynamic",
            "electricity_config::config",
            "electricity",
        ] {
            assert!(
                StderrWarnLogger.enabled(&warn_metadata(target)),
                "{target} should be admitted"
            );
        }
    }

    #[test]
    fn the_warning_logger_ignores_a_record_from_outside_this_workspace() {
        assert!(!StderrWarnLogger.enabled(&warn_metadata("some_other_crate")));
    }

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

    /// Ties `USAGE`'s own "runs ... documents only" / "refused" lists
    /// to `Supported::current()`'s own flags, so the next lane that
    /// flips one (`loop_`/`use_` already did, this PR's own
    /// addition) can't leave this text stale the way it found it --
    /// each name in the mapping below must appear in exactly one of the
    /// two lists, matching whichever side its own flag says.
    #[test]
    fn usage_text_matches_supported_current_for_every_gated_effect_type() {
        let supported_list = USAGE
            .split("it runs ")
            .nth(1)
            .expect("USAGE names what it runs")
            .split(" documents")
            .next()
            .expect("USAGE's own \"documents only\" phrase")
            .split('/')
            .collect::<Vec<_>>();
        let refused_list = USAGE
            .split('(')
            .nth(1)
            .expect("USAGE names what it refuses, parenthesized")
            .split(',')
            .next()
            .expect("USAGE's own refused-list prefix, before the first comma")
            .split('/')
            .collect::<Vec<_>>();

        let supported = electricity_bytecode::Supported::current(&[]);
        let gated: [(&str, bool); 5] = [
            ("prompt", supported.prompt),
            ("loop", supported.loop_),
            ("use", supported.use_),
            ("yield", supported.yield_),
            ("reflector", supported.reflector),
        ];
        for (name, is_supported) in gated {
            if is_supported {
                assert!(
                    supported_list.contains(&name),
                    "{name} is supported (Supported::current) but missing from USAGE's own \
                     \"runs ... documents only\" list: {supported_list:?}"
                );
                assert!(
                    !refused_list.contains(&name),
                    "{name} is supported (Supported::current) but still named in USAGE's own \
                     refused list: {refused_list:?}"
                );
            } else {
                assert!(
                    refused_list.contains(&name),
                    "{name} is refused (Supported::current) but missing from USAGE's own \
                     refused list: {refused_list:?}"
                );
                assert!(
                    !supported_list.contains(&name),
                    "{name} is refused (Supported::current) but still named in USAGE's own \
                     \"runs ... documents only\" list: {supported_list:?}"
                );
            }
        }
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
                assert_eq!(run_args.config.as_deref(), Some("config.json"));
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
    fn osis_own_invocation_shape_still_parses() {
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
    fn osis_own_invocation_shape_still_parses_with_the_equals_form() {
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

    #[test]
    fn run_subcommand_with_a_single_positional_is_a_run_request_with_no_config() {
        match classify(&["run".to_string(), "doc.yml".to_string()]) {
            Action::Run(run_args) => {
                assert_eq!(run_args.orchestration, "doc.yml");
                assert_eq!(run_args.config, None);
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn run_subcommand_parses_every_new_flag() {
        match classify(&[
            "run".to_string(),
            "doc.yml".to_string(),
            "--config".to_string(),
            "config.json".to_string(),
            "--out".to_string(),
            "state.json".to_string(),
            "--live-state".to_string(),
            "live.json".to_string(),
            "--events".to_string(),
            "events.jsonl".to_string(),
            "--allow-capabilities".to_string(),
            "shell,network".to_string(),
            "--adapter".to_string(),
            "claude_code".to_string(),
            "--quiet".to_string(),
            "-e".to_string(),
            "name=World".to_string(),
        ]) {
            Action::Run(run_args) => {
                assert_eq!(run_args.orchestration, "doc.yml");
                assert_eq!(run_args.config.as_deref(), Some("config.json"));
                assert_eq!(run_args.out.as_deref(), Some("state.json"));
                assert_eq!(run_args.live_state.as_deref(), Some("live.json"));
                assert_eq!(run_args.events.as_deref(), Some("events.jsonl"));
                assert_eq!(
                    run_args.allow_capabilities.as_deref(),
                    Some("shell,network")
                );
                assert_eq!(run_args.adapter.as_deref(), Some("claude_code"));
                assert!(run_args.quiet);
                assert_eq!(run_args.inputs, vec!["name=World".to_string()]);
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn run_subcommand_with_no_positional_is_missing_orchestration_not_a_usage_error() {
        assert!(matches!(
            classify(&[
                "run".to_string(),
                "--config".to_string(),
                "c.json".to_string()
            ]),
            Action::MissingOrchestration
        ));
        assert!(matches!(
            classify(&["run".to_string()]),
            Action::MissingOrchestration
        ));
    }

    #[test]
    fn run_subcommand_last_flag_with_no_positional_is_a_run_request() {
        match classify(&["run".to_string(), "--last".to_string()]) {
            Action::Run(run_args) => {
                assert!(run_args.last);
                assert_eq!(run_args.orchestration, "");
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn run_subcommand_last_flag_with_a_positional_still_sets_last() {
        // `cof run bogus.yml --last` ignores `bogus.yml` outright
        // (confirmed directly against a real `cof run`) -- `run_action`
        // overwrites `orchestration` from the stash before anything
        // reads it, so the positional is merely parsed here, never
        // validated against.
        match classify(&[
            "run".to_string(),
            "bogus.yml".to_string(),
            "--last".to_string(),
        ]) {
            Action::Run(run_args) => {
                assert!(run_args.last);
                assert_eq!(run_args.orchestration, "bogus.yml");
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn run_subcommand_without_last_and_no_positional_is_still_missing_orchestration() {
        assert!(matches!(
            classify(&["run".to_string()]),
            Action::MissingOrchestration
        ));
    }

    #[test]
    fn run_subcommand_with_two_positionals_is_a_usage_error() {
        assert!(matches!(
            classify(&["run".to_string(), "a.yml".to_string(), "b.yml".to_string()]),
            Action::RunUsageError(_)
        ));
    }

    #[test]
    fn run_subcommand_extra_argument_names_every_extra_cofs_own_way() {
        match classify(&[
            "run".to_string(),
            "a.yml".to_string(),
            "b.yml".to_string(),
            "c.yml".to_string(),
        ]) {
            Action::RunUsageError(message) => {
                assert_eq!(message, "Got unexpected extra argument(s) (b.yml c.yml)");
            }
            _ => panic!("expected RunUsageError"),
        }
    }

    #[test]
    fn run_subcommand_rejects_dump_ir() {
        assert!(matches!(
            classify(&[
                "run".to_string(),
                "doc.yml".to_string(),
                "--dump-ir".to_string()
            ]),
            Action::RunUsageError(_)
        ));
    }

    #[test]
    fn run_subcommand_unknown_flag_is_a_usage_error() {
        match classify(&[
            "run".to_string(),
            "doc.yml".to_string(),
            "--bogus".to_string(),
        ]) {
            Action::RunUsageError(message) => assert_eq!(message, "No such option: --bogus"),
            _ => panic!("expected RunUsageError"),
        }
    }

    /// `cof run` has no `-V`/`--version`/`-h` of its own at all --
    /// each is itself an unrecognized option there (issue #728's own
    /// review), unlike the legacy form and the top-level group, where
    /// `-V`/`-h` still win over everything ([`version_flag_wins_over_
    /// everything`]/[`help_flag_is_recognized`] above, unchanged).
    /// `--help` (long form) still works under `run`.
    #[test]
    fn run_subcommand_has_no_short_version_or_help_flags_of_its_own() {
        for flag in ["-V", "--version", "-h"] {
            match classify(&["run".to_string(), flag.to_string()]) {
                Action::RunUsageError(message) => {
                    assert_eq!(message, format!("No such option: {flag}"))
                }
                _ => panic!("expected RunUsageError for {flag}"),
            }
        }
        assert!(matches!(
            classify(&["run".to_string(), "--help".to_string()]),
            Action::Help
        ));
    }

    /// An unknown option under `run` wins over `--help`, wherever it
    /// falls on the line -- `cof run`'s own option parser raises on
    /// the first unrecognized token while scanning the whole line,
    /// before any eager callback (`--help`'s included) ever runs.
    #[test]
    fn run_subcommand_unknown_option_wins_over_help_in_either_order() {
        assert!(matches!(
            classify(&[
                "run".to_string(),
                "--bogus".to_string(),
                "--help".to_string()
            ]),
            Action::RunUsageError(_)
        ));
        assert!(matches!(
            classify(&[
                "run".to_string(),
                "--help".to_string(),
                "--bogus".to_string()
            ]),
            Action::RunUsageError(_)
        ));
    }

    #[test]
    fn run_subcommand_accepts_the_short_config_and_out_aliases() {
        match classify(&[
            "run".to_string(),
            "doc.yml".to_string(),
            "-c".to_string(),
            "config.json".to_string(),
            "-o".to_string(),
            "state.json".to_string(),
        ]) {
            Action::Run(run_args) => {
                assert_eq!(run_args.config.as_deref(), Some("config.json"));
                assert_eq!(run_args.out.as_deref(), Some("state.json"));
            }
            _ => panic!("expected Run"),
        }
    }

    /// Every flag [`KNOWN_FLAGS`]/[`RUN_FLAGS`] recognizes is actually
    /// named in [`USAGE`]'s own Options list -- unlike `tests/cli.rs`'s
    /// own `--help`-output assertion (an external, binary-level check,
    /// kept as a second line of defense), this is tied directly to the
    /// parser's own source of truth, so a new flag added to either
    /// table without updating USAGE fails this immediately.
    #[test]
    fn usage_text_names_every_known_and_run_flag() {
        for (flag, _) in KNOWN_FLAGS.iter().chain(RUN_FLAGS.iter()) {
            assert!(USAGE.contains(flag), "USAGE never mentions {flag}");
        }
    }
}
