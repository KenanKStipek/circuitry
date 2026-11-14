//! `osi man [watch] [--roff]` (issue #764): builds osi(1)/osi-watch(1)
//! directly from the same `RunArgs`/`WatchArgs` clap already defines
//! (`clap_mangen` renders NAME/SYNOPSIS/OPTIONS/DESCRIPTION from
//! them), with hand-written EXAMPLES/ENVIRONMENT/FILES/EXIT STATUS/SEE
//! ALSO appended -- so `--help` and the man page can't disagree about
//! what an option does, and `scripts/generate-man-pages.py` only ever
//! captures this binary's own `osi man --roff` output, never
//! reimplements it.

use clap::CommandFactory;

use crate::{RunArgs, WatchArgs};

/// The two pages `osi man` knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Run,
    Watch,
}

/// Word-wraps *text* to at most *width* columns -- `mandoc -Tlint`/
/// `groff -ww` lint the source file's own line lengths, not just the
/// rendered page.
fn wrap_width(text: &str, width: usize) -> String {
    let mut out = String::new();
    let mut line_len = 0usize;
    for word in text.split_whitespace() {
        let word_len = word.chars().count();
        if line_len == 0 {
            out.push_str(word);
            line_len = word_len;
        } else if line_len + 1 + word_len > width {
            out.push('\n');
            out.push_str(word);
            line_len = word_len;
        } else {
            out.push(' ');
            out.push_str(word);
            line_len += 1 + word_len;
        }
    }
    out
}

const MAN_WIDTH: usize = 78;

/// A fixed `.TH` date (issue #764): never the build date, so `osi
/// man`'s output -- and `man/man1/osi*.1` -- don't change on every
/// build.
const MAN_DATE: &str = "2026-11-13";

/// Fixes up clap_mangen's own raw roff (issue #764): wraps every
/// plain-text line (never a macro line, and never inside a `.nf`/
/// `.fi` block) to [`MAN_WIDTH`], and drops the empty `.br`/`.sp`
/// pair clap_mangen emits around a `value_enum` field's own Possible
/// values list -- `mandoc -Tlint`'s only two complaints about
/// clap_mangen's own output, neither under this binary's control
/// otherwise.
/// Turns a markdown code span (`` `foo` ``) into roff bold (`\fBfoo\fR`)
/// (issue #764) -- `RunArgs`'/`WatchArgs`' own doc comments use
/// backticks for an option/path/env-var name, which `clap_mangen`
/// passes straight through as a literal character.
fn code_spans_to_bold(text: &str) -> String {
    let mut out = String::new();
    let mut in_code = false;
    for part in text.split('`') {
        if in_code {
            out.push_str("\\fB");
            out.push_str(part);
            out.push_str("\\fR");
        } else {
            out.push_str(part);
        }
        in_code = !in_code;
    }
    out
}

/// A roff request mid-paragraph, even right after wrapping put it at
/// the start of a line -- `\&` is a zero-width roff escape that
/// neutralizes a leading `.`/`'` without changing how the line prints.
fn guard_leading_control_char(line: &str) -> std::borrow::Cow<'_, str> {
    if line.starts_with('.') || line.starts_with('\'') {
        std::borrow::Cow::Owned(format!("\\&{line}"))
    } else {
        std::borrow::Cow::Borrowed(line)
    }
}

fn postprocess_roff(text: &str) -> String {
    let text = text.replace(".br\n\n.br\n", "\n");
    let mut out = String::new();
    let mut in_no_fill = false;
    for raw_line in text.lines() {
        let line = raw_line.trim_end();
        if line == ".nf" {
            in_no_fill = true;
        } else if line == ".fi" {
            in_no_fill = false;
        }
        if in_no_fill || line.is_empty() || line.starts_with('.') || line.starts_with('\'') {
            out.push_str(line);
        } else {
            let bold = code_spans_to_bold(line);
            for (i, wrapped_line) in wrap_width(&bold, MAN_WIDTH).lines().enumerate() {
                if i > 0 {
                    out.push('\n');
                }
                out.push_str(&guard_leading_control_char(wrapped_line));
            }
        }
        out.push('\n');
    }
    out
}

/// Creates *name*'s own temp roff file with an unpredictable name,
/// atomically (`create_new`) so a symlink planted ahead of time at a
/// guessed path can't be written through -- unlike a plain
/// `{name}.{pid}.1` path, a process id is not a secret.
pub fn create_temp_man_file(name: &str, text: &str) -> std::io::Result<std::path::PathBuf> {
    use std::io::Write as _;
    let dir = std::env::temp_dir();
    for attempt in 0..100u32 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let unique = nanos ^ u128::from(std::process::id()) ^ u128::from(attempt);
        let path = dir.join(format!("{name}.{unique:x}.1"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(text.as_bytes())?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not create a temp man file",
    ))
}

/// Renders a `.TP` section from (heading, body) rows: *bold_heading*
/// picks `.B` (an env var/exit code) over `.I` (a file path).
fn render_tp_section(rows: &[(&str, &str)], bold_heading: bool) -> String {
    let macro_ = if bold_heading { ".B" } else { ".I" };
    let mut out = String::new();
    for (heading, body) in rows {
        out.push_str(".TP\n");
        out.push_str(macro_);
        out.push(' ');
        out.push_str(heading);
        out.push('\n');
        out.push_str(&wrap_width(body, MAN_WIDTH));
        out.push('\n');
    }
    out
}

const RUN_EXAMPLES: &[(&str, &str)] = &[
    (
        "osi orchestration.yml -e name=world",
        "Launches a document on cof, with one input, and watches it.",
    ),
    (
        "osi orchestration.yml --engine electricity --out-dir ./run",
        "Launches a document on electricity instead, keeping its run \
         directory (state, events, stdout/stderr) at ./run.",
    ),
    (
        "osi --log",
        "Without an orchestration: lists the orchestrations already \
         running on this machine and exits, instead of opening the \
         Select screen.",
    ),
];

const WATCH_EXAMPLES: &[(&str, &str)] = &[
    (
        "osi watch ./run",
        "Attaches to the run directory a prior osi orchestration.yml \
         --out-dir ./run launch wrote.",
    ),
    (
        "osi watch ./run/state.live.json --plan orchestration.yml",
        "Watches a live state file directly, compiling orchestration.yml \
         into the same plan the live run started from.",
    ),
    (
        "osi watch ./run --glance --once",
        "Prints the current header line for a tmux status line and exits, \
         instead of watching.",
    ),
];

const ENVIRONMENT: &[(&str, &str)] = &[
    (
        "OSI_THEME",
        "The colour theme, when --theme isn't given. terminal (today's \
         look) otherwise.",
    ),
    (
        "OSI_HISTORY",
        "OSI_HISTORY=off turns run history off, the same as --no-history: \
         nothing is read or written, and no history directory is created.",
    ),
    (
        "OSI_HISTORY_DIR",
        "Overrides osi's history directory (see FILES below).",
    ),
    (
        "OSI_RUNS_DIR",
        "Overrides osi's run registry directory (see FILES below).",
    ),
    (
        "OSI_PROCESS_SCAN",
        "OSI_PROCESS_SCAN=0 limits bare osi's discovery (no \
         orchestration, no TTY, or --log) to the run registry, skipping \
         the process scan and any state.live.json in the current \
         directory.",
    ),
    (
        "XDG_STATE_HOME",
        "The base osi's history and run registry directories are \
         rooted under, when OSI_HISTORY_DIR/OSI_RUNS_DIR aren't set.",
    ),
    (
        "XDG_RUNTIME_DIR",
        "Preferred over XDG_STATE_HOME for the run registry alone (a \
         live run's pid file), when it's set.",
    ),
    ("CI", "Forces the plain-text stream, the same as --log."),
];

const FILES: &[(&str, &str)] = &[
    (
        "~/.local/state/circuitry/osi/history/",
        "Per-document run history: one record per finished run, read \
         back for the Recent Runs table and ETA estimates. \
         OSI_HISTORY_DIR overrides this directory; OSI_HISTORY=off or \
         --no-history disables it entirely.",
    ),
    (
        "~/.local/state/circuitry/osi/runs/",
        "The run registry: one file per run osi launched or is watching, \
         used by bare osi's discovery and pruned once a run's pid is \
         no longer alive. OSI_RUNS_DIR overrides this directory; \
         $XDG_RUNTIME_DIR/circuitry/osi/runs is tried first when set.",
    ),
];

const EXIT_STATUS: &[(&str, &str)] = &[
    ("0", "The watched or launched run finished without error."),
    (
        "1 and other",
        "osi exits with the engine's own exit code, passed through \
         unchanged -- a child process's own code if it had one, or \
         128 plus the signal number that killed it. 1 covers an \
         ordinary failure, or osi being unable to start or attach to \
         the run at all.",
    ),
    (
        "2",
        "A usage error -- an unrecognized option, a missing required \
         argument, or an unknown osi man page. Never from the watched \
         or launched engine itself.",
    ),
    ("129", "The run ended on SIGHUP (128 + 1)."),
    ("130", "The run ended on SIGINT, Ctrl-C (128 + 2)."),
    ("143", "The run ended on SIGTERM (128 + 15)."),
];

const SEE_ALSO: &str =
    ".BR osi (1),\n.BR osi\\-watch (1),\n.BR cof (1),\n.BR cof\\-run (1),\n.BR electricity (1)\n";

/// Builds *page*'s own full roff man page: `clap_mangen`'s own
/// rendering of `RunArgs`/`WatchArgs` for NAME/SYNOPSIS/OPTIONS/
/// DESCRIPTION, with the hand-written sections above appended.
pub fn build_man_page(page: Page) -> String {
    let source = format!("osi {}", env!("CARGO_PKG_VERSION"));
    let mut base = Vec::new();
    let (examples, see_also) = match page {
        Page::Run => {
            clap_mangen::Man::new(RunArgs::command())
                .title("OSI")
                .date(MAN_DATE)
                .source(&source)
                .render(&mut base)
                .expect("rendering to a Vec<u8> never fails");
            (RUN_EXAMPLES, SEE_ALSO)
        }
        Page::Watch => {
            clap_mangen::Man::new(WatchArgs::command())
                .title("OSI-WATCH")
                .date(MAN_DATE)
                .source(&source)
                .render(&mut base)
                .expect("rendering to a Vec<u8> never fails");
            (WATCH_EXAMPLES, SEE_ALSO)
        }
    };
    let mut base = String::from_utf8(base).expect("clap_mangen renders valid UTF-8");
    if page == Page::Watch {
        // clap_mangen's own NAME line uses the Command's name verbatim
        // ("osi watch", with a space -- `WatchArgs`'s own program name
        // for its SYNOPSIS/USAGE), not the hyphenated `osi-watch(1)`
        // man-page convention `osi-watch.1`'s filename already follows.
        base = base.replacen("osi watch \\-", "osi-watch \\-", 1);
    }
    let full = format!(
        "{base}.SH EXAMPLES\n{examples}.SH ENVIRONMENT\n{environment}.SH FILES\n{files}\
         .SH EXIT STATUS\n{exit_status}.SH SEE ALSO\n{see_also}",
        examples = render_tp_section(examples, true),
        environment = render_tp_section(ENVIRONMENT, true),
        files = render_tp_section(FILES, false),
        exit_status = render_tp_section(EXIT_STATUS, true),
    );
    postprocess_roff(&full)
}
