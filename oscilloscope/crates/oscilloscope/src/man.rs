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
            out.push_str(&wrap_width(line, MAN_WIDTH));
        }
        out.push('\n');
    }
    out
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
        "Overrides osi's own history directory (see FILES below).",
    ),
    (
        "OSI_RUNS_DIR",
        "Overrides osi's own run registry directory (see FILES below).",
    ),
    (
        "OSI_PROCESS_SCAN",
        "OSI_PROCESS_SCAN=0 limits bare osi's own discovery (no \
         orchestration, no TTY, or --log) to the run registry, skipping \
         the process scan and any state.live.json in the current \
         directory.",
    ),
    (
        "XDG_STATE_HOME",
        "The base osi's own history and run registry directories are \
         rooted under, when OSI_HISTORY_DIR/OSI_RUNS_DIR aren't set.",
    ),
    (
        "XDG_RUNTIME_DIR",
        "Preferred over XDG_STATE_HOME for the run registry alone (a \
         live run's own pid file), when it's set.",
    ),
    ("CI", "Forces the plain-text stream, the same as --log."),
];

const FILES: &[(&str, &str)] = &[
    (
        "~/.local/state/circuitry/osi/history/",
        "Per-document run history (issue #649): one record per finished \
         run, read back for the Recent Runs table and ETA estimates. \
         OSI_HISTORY_DIR overrides this directory; OSI_HISTORY=off or \
         --no-history disables it entirely.",
    ),
    (
        "~/.local/state/circuitry/osi/runs/",
        "The run registry: one file per run osi launched or is watching, \
         used by bare osi's own discovery and pruned once a run's pid is \
         no longer alive. OSI_RUNS_DIR overrides this directory; \
         $XDG_RUNTIME_DIR/circuitry/osi/runs is tried first when set.",
    ),
];

const EXIT_STATUS: &[(&str, &str)] = &[
    ("0", "The watched or launched run finished without error."),
    (
        "1",
        "The run finished with an error, or osi could not start or attach \
         to it at all.",
    ),
    (
        "2",
        "A usage error -- an unrecognized option, a missing required \
         argument, or an unknown osi man page.",
    ),
    ("129", "The run ended on SIGHUP."),
    ("130", "The run ended on SIGINT (Ctrl-C)."),
    ("143", "The run ended on SIGTERM."),
];

const SEE_ALSO: &str =
    ".BR osi (1),\n.BR osi\\-watch (1),\n.BR cof (1),\n.BR cof\\-run (1),\n.BR electricity (1)\n";

/// Builds *page*'s own full roff man page: `clap_mangen`'s own
/// rendering of `RunArgs`/`WatchArgs` for NAME/SYNOPSIS/OPTIONS/
/// DESCRIPTION, with the hand-written sections above appended.
pub fn build_man_page(page: Page) -> String {
    let mut base = Vec::new();
    let (examples, see_also) = match page {
        Page::Run => {
            clap_mangen::Man::new(RunArgs::command())
                .title("OSI")
                .date(MAN_DATE)
                .render(&mut base)
                .expect("rendering to a Vec<u8> never fails");
            (RUN_EXAMPLES, SEE_ALSO)
        }
        Page::Watch => {
            clap_mangen::Man::new(WatchArgs::command())
                .title("OSI-WATCH")
                .date(MAN_DATE)
                .render(&mut base)
                .expect("rendering to a Vec<u8> never fails");
            (WATCH_EXAMPLES, SEE_ALSO)
        }
    };
    let base = String::from_utf8(base).expect("clap_mangen renders valid UTF-8");
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
