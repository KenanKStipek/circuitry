"""`cof man [COMMAND]` and `man/man1/cof*.1`'s own source (issue #764).

NAME/SYNOPSIS/OPTIONS are walked straight off the live Click command tree
(the same one Typer builds `cof`'s own `--help` from), so they can never
drift from what the CLI actually accepts. DESCRIPTION/EXAMPLES/ENVIRONMENT/
FILES/EXIT STATUS/SEE ALSO are hand-written, in :data:`PAGE_EXTRAS` below,
and merged in by :func:`render_page`.

Both `cof man` and `scripts/generate-man-pages.py` call :func:`render_page`
directly -- there is no separate "installed page" this binary reads back;
regenerating is simply re-running this same code, so the two can never
disagree, and `cof man` keeps working in a source checkout with no
`man/man1/` at all.
"""

from __future__ import annotations

import re
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path
from typing import TYPE_CHECKING, Any

import click
import typer

from .console import console

if TYPE_CHECKING:
    # Typer wraps a forked copy of click's own types (`typer._click`), so
    # a plain `click.Command`/`click.Context` annotation doesn't always
    # line up with what `typer.main.get_command` actually returns --
    # `Any` here is deliberate, not a shortcut.
    Command = Any
    Context = Any

MAN_WIDTH = 78

#: A fixed `.TH` date (issue #764): never the build date, so a page's
#: content doesn't change on every regeneration unless it actually did.
MAN_DATE = "2026-11-13"

#: Canonical page names, in `cof man`'s own listing order -- every
#: top-level Click command name, plus `check` (`validate`'s alias) and
#: `man` itself. `library`'s own subcommand (`refresh`) is documented
#: on the `library` page, not as a page of its own (issue #764 rule 1:
#: "one page covering the group's subcommands").
COMMAND_ORDER: tuple[str, ...] = (
    "run",
    "validate",
    "check",
    "inspect",
    "list",
    "info",
    "eject",
    "fetch",
    "run-library",
    "library",
    "gen",
    "wizard",
    "init",
    "doctor",
    "score",
    "setup",
    "trust",
    "untrust",
    "version",
    "mcp",
    "tui",
    "man",
)

#: `cof-check(1)` points to `cof-validate(1)` (issue #764 rule 1) --
#: `render_page` answers an aliased name with a tiny `.so` page instead
#: of walking a Click command for it, and `cof man check` shows the
#: real page's content directly.
ALIASES: dict[str, str] = {"check": "validate"}


def _guard_leading_control_char(line: str) -> str:
    r"""A line starting with `.` or `'` is a roff request, even mid-
    paragraph -- wrapping can land either character at the start of a
    line that was never meant as one. `\&` is a zero-width roff escape
    that neutralizes it without changing how the line prints."""
    if line.startswith((".", "'")):
        return "\\&" + line
    return line


def _wrap(text: str, width: int = MAN_WIDTH) -> str:
    """Word-wraps *text* to at most *width* columns, one output line per
    input line it produces -- `mandoc -Tlint`/`groff -ww` lint the
    *source* file's own line lengths, not just the rendered page."""
    out: list[str] = []
    line: list[str] = []
    line_len = 0
    for word in text.split():
        # Byte length, not character count: mandoc/groff's own line-
        # length lint counts bytes, and an em dash or similar costs
        # more than one.
        word_len = len(word.encode("utf-8"))
        if line and line_len + 1 + word_len > width:
            out.append(" ".join(line))
            line = [word]
            line_len = word_len
        else:
            if line:
                line_len += 1 + word_len
            else:
                line_len = word_len
            line.append(word)
    if line:
        out.append(" ".join(line))
    return "\n".join(_guard_leading_control_char(line) for line in out)


def _wrap_paragraphs(text: str, width: int = MAN_WIDTH) -> str:
    """Wraps each blank-line-separated paragraph of *text* independently,
    rejoining them with `.PP` -- `-man`'s own paragraph break, since a
    blank input line alone is not one."""
    paragraphs = [p.strip() for p in text.strip().split("\n\n") if p.strip()]
    return "\n.PP\n".join(_wrap(_format_text(" ".join(p.split())), width) for p in paragraphs)


def _roff_escape(text: str) -> str:
    """Escapes a literal backslash, then every non-ASCII character --
    plain `groff -man -Tutf8` (no `-k`/preconv) reads raw UTF-8 bytes as
    invalid input, so an em dash or similar needs roff's own escape
    (`\\(em`, or `\\[uXXXX]` for anything without a named glyph) instead
    of the literal character."""
    text = text.replace("\\", "\\e")
    text = text.replace("—", "\\(em")
    return "".join(c if ord(c) < 128 else f"\\[u{ord(c):04X}]" for c in text)


def _code_spans_to_bold(text: str) -> str:
    """Turns a markdown code span (`` `foo` ``) into roff bold (``\\fBfoo\\fR``)
    -- hand-written help text uses backticks for an option or path name, and
    a plain backtick would otherwise render literally on the page."""
    return re.sub(r"`([^`]+)`", r"\\fB\1\\fR", text)


def _format_text(text: str) -> str:
    """Escapes roff-sensitive characters, then turns a markdown code span
    into roff bold -- the one pass every hand-written or CLI-sourced
    string takes before it reaches the page."""
    return _code_spans_to_bold(_roff_escape(text))


def _tp(heading: str, body: str, *, bold: bool = True, raw_prefix: str = "") -> str:
    """*raw_prefix* -- a metavar's own `\\fI...\\fR` italics -- is inserted
    after *body* is escaped, never before: it is already roff, not prose."""
    macro = ".B" if bold else ".I"
    text = raw_prefix + _format_text(body)
    return f".TP\n{macro} {_format_text(heading)}\n{_wrap(text)}\n"


def _tp_section(rows: tuple[tuple[str, str], ...], *, bold: bool = True) -> str:
    return "".join(_tp(heading, body, bold=bold) for heading, body in rows)


@dataclass(frozen=True)
class PageExtra:
    """The hand-written sections of one page -- everything
    NAME/SYNOPSIS/OPTIONS can't answer on their own."""

    description: str
    examples: tuple[tuple[str, str], ...] = ()
    environment: tuple[tuple[str, str], ...] = ()
    files: tuple[tuple[str, str], ...] = ()
    exit_status: tuple[tuple[str, str], ...] = (
        ("0", "Success."),
        ("1", "The command failed, or the input was invalid."),
        ("2", "A usage error -- an unrecognized option or missing argument."),
    )
    see_also: tuple[str, ...] = field(default_factory=tuple)


#: Shared by every command that resolves a config the way `resolve_config`
#: does (issue #764: "the config discovery order and CIRCUITRY_CONFIG").
CONFIG_ENVIRONMENT: tuple[tuple[str, str], ...] = (
    (
        "CIRCUITRY_CONFIG",
        ("An explicit config JSON path (same as --config). When set, replaces "
        "the global config and any project config entirely, rather than "
        "layering on top of them."),
    ),
    ("CIRCUITRY_MODEL", "Overrides the resolved config's default_model."),
    ("CIRCUITRY_ADAPTER", "Overrides the resolved config's default_adapter."),
    (
        "CIRCUITRY_ADAPTER_URL",
        "Overrides the resolved default adapter's base_url.",
    ),
    ("CIRCUITRY_COMFYUI_URL", "Overrides the comfyui plugin's base_url."),
    ("CIRCUITRY_SYSTEMONE_URL", "Overrides the systemone plugin's base_url."),
    (
        "CIRCUITRY_ENABLED_ADAPTERS",
        ("Comma-separated allowlist, narrowing (never widening) the "
        "resolved config's enabled_adapters."),
    ),
    (
        "CIRCUITRY_ENABLED_PLUGINS",
        ("Comma-separated allowlist, narrowing (never widening) the "
        "resolved config's enabled_plugins."),
    ),
    (
        "CIRCUITRY_ENABLED_TOOLS",
        ("Comma-separated allowlist, narrowing (never widening) the "
        "resolved config's enabled_tools."),
    ),
    (
        "CIRCUITRY_ENVIRONMENT",
        "Overrides the resolved config's environment (dev, prod, or test).",
    ),
    (
        "CIRCUITRY_TRUST_PROJECT_CONFIG",
        ("Trusts a project config in the current directory without "
        "running cof trust first."),
    ),
)

#: Shared FILES, for the same commands as :data:`CONFIG_ENVIRONMENT`.
CONFIG_FILES: tuple[tuple[str, str], ...] = (
    (
        "~/.config/circuitry/config.json",
        "The global config, applied first; a trusted project config layers on top of it.",
    ),
    (
        "~/.config/circuitry/.env",
        ("Private environment variables, loaded before config resolution "
        "(written by cof setup)."),
    ),
    (
        "~/.config/circuitry/trusted.json",
        ("Project configs trusted by cof trust; config resolution consults "
        "it before applying a discovered project config."),
    ),
    (
        "./circuitry.config.json, ./config.json",
        ("A project config in the working directory, layered on top of the "
        "global config once trusted."),
    ),
)

#: :data:`CONFIG_ENVIRONMENT` plus the one extra variable `fetch`/
#: `run-library` consult for a shared library asset.
LIBRARY_ENVIRONMENT: tuple[tuple[str, str], ...] = (
    *CONFIG_ENVIRONMENT,
    (
        "CIRCUITRY_LIBRARY_TOKEN",
        "Shared library auth token, when the configured backend requires one; overridden by --auth-token.",
    ),
)

SEE_ALSO_CORE: tuple[str, ...] = ("cof(1)",)


def _page(name: str) -> str:
    return f"cof-{name}(1)" if name != "cof" else "cof(1)"


#: Hand-written sections for every page `COMMAND_ORDER` names, plus `cof`
#: itself. A page not listed here (there should be none) falls back to a
#: minimal, still-accurate stub in :func:`_extra_for`.
PAGE_EXTRAS: dict[str, PageExtra] = {
    "cof": PageExtra(
        description=(
            "cof is Circuitry's command-line interface: it runs, "
            "validates, inspects, generates, lists, and manages "
            "orchestration documents and their configuration."
        ),
        environment=CONFIG_ENVIRONMENT,
        files=(*CONFIG_FILES, ("~/.config/circuitry/services/", "Background service state (cof's service tooling).")),
        see_also=(*tuple(_page(n) for n in COMMAND_ORDER), "electricity(1)", "osi(1)"),
    ),
    "run": PageExtra(
        description=(
            "Executes an orchestration document: a bundled name (cof list), "
            "a local path, or (with --last) the most recently run "
            "orchestration's saved arguments. Config resolution runs first "
            "(the global config, then a trusted project config layered on "
            "top of it; CIRCUITRY_CONFIG or --config, if set, replaces both "
            "instead), then the document's effects run in order."
        ),
        examples=(
            ("cof run hello-world", "Runs a bundled orchestration by name."),
            (
                "cof run my-flow.yml -e name=world --out state.json",
                ("Runs a local document with one input, writing the final "
                "state to a file."),
            ),
            (
                "cof run --last",
                ("Re-runs the most recent orchestration with the same "
                "arguments, from ~/.config/circuitry/last-run.json."),
            ),
            (
                "cof run my-flow.yml --resume last",
                ("Resumes the most recent interrupted or failed run of the "
                "same orchestration from its saved state."),
            ),
        ),
        environment=CONFIG_ENVIRONMENT,
        files=(*CONFIG_FILES, ("~/.config/circuitry/last-run.json", "This run's arguments, for a later --last or --resume last. Secret-looking values are redacted before being written, and --last refuses to replay a redacted run.")),
        exit_status=(
            ("0", "The run finished without error."),
            ("1", "The run failed, or the orchestration could not be found."),
            ("2", "A usage error -- an unrecognized option or missing argument."),
            ("129", "The run was interrupted by SIGHUP."),
            ("130", "The run was interrupted by SIGINT (Ctrl-C)."),
            ("143", "The run was interrupted by SIGTERM."),
        ),
        see_also=(*SEE_ALSO_CORE, "cof-validate(1)", "cof-list(1)", "electricity-run(1)"),
    ),
    "validate": PageExtra(
        description=(
            "Checks an orchestration file against Circuitry's schema and, "
            "unless --skip-preflight is given, every effect's "
            "dependency-readiness check -- without running anything."
        ),
        examples=(
            ("cof validate my-flow.yml", "Validates a document, printing any error."),
            ("cof check my-flow.yml --json", "Same check, as the check alias, machine-readable."),
        ),
        environment=CONFIG_ENVIRONMENT,
        files=CONFIG_FILES,
        see_also=(*SEE_ALSO_CORE, "cof-inspect(1)", "cof-run(1)"),
    ),
    "inspect": PageExtra(
        description="Prints an orchestration document's path, format, size, model, adapter, and effect names and count, without running or validating it.",
        examples=(("cof inspect my-flow.yml", "Shows a document's summary fields."),),
        see_also=(*SEE_ALSO_CORE, "cof-validate(1)"),
    ),
    "list": PageExtra(
        description=(
            "Lists bundled orchestrations from the curated index, "
            "optionally filtered by category. --extensions lists compiled-"
            "in tool/runtime/adapter extensions instead; --models <adapter> "
            "lists that adapter's available models."
        ),
        examples=(
            ("cof list", "Lists every bundled orchestration."),
            ("cof list --category research", "Filters by category."),
            ("cof list --extensions", "Lists compiled-in extensions instead."),
        ),
        environment=CONFIG_ENVIRONMENT,
        files=CONFIG_FILES,
        see_also=(*SEE_ALSO_CORE, "cof-info(1)", "cof-run(1)"),
    ),
    "info": PageExtra(
        description="Shows a bundled orchestration's details from the curated index: description, category, complexity, and required extensions.",
        examples=(("cof info hello-world", "Shows one bundled orchestration's details."),),
        see_also=(*SEE_ALSO_CORE, "cof-list(1)", "cof-eject(1)"),
    ),
    "eject": PageExtra(
        description="Copies a bundled orchestration's file into the current directory, for local editing -- the bundled copy itself is never writable.",
        examples=(("cof eject hello-world", "Copies the bundled hello-world.yml here."),),
        see_also=(*SEE_ALSO_CORE, "cof-info(1)", "cof-run(1)"),
    ),
    "fetch": PageExtra(
        description=(
            "Fetches a shared library asset from the configured filesystem "
            "backend (runtime.library.local_root), writing it to --out, "
            "without running it."
        ),
        environment=LIBRARY_ENVIRONMENT,
        files=CONFIG_FILES,
        see_also=(*SEE_ALSO_CORE, "cof-library(1)", "cof-run-library(1)"),
    ),
    "run-library": PageExtra(
        description="Fetches a shared library asset and runs it in one step -- the same fetch cof fetch does, followed by the same execution cof run does.",
        environment=LIBRARY_ENVIRONMENT,
        files=(*CONFIG_FILES, ("~/.config/circuitry/last-run.json", "This run's arguments, for a later cof run --last.")),
        exit_status=(
            ("0", "The run finished without error."),
            ("1", "The fetch or the run failed."),
            ("2", "A usage error -- an unrecognized option or missing argument."),
            ("129", "The run was interrupted by SIGHUP."),
            ("130", "The run was interrupted by SIGINT (Ctrl-C)."),
            ("143", "The run was interrupted by SIGTERM."),
        ),
        see_also=(*SEE_ALSO_CORE, "cof-fetch(1)", "cof-run(1)", "cof-library(1)"),
    ),
    "library": PageExtra(
        description=(
            "Manages runtime.library.sources: remote sources a bare-name "
            "cof run argument or an orchestration's use: ref: resolves "
            "against, in precedence order. cof fetch/run-library pull from "
            "a separate filesystem backend (runtime.library.local_root) "
            "instead, by asset id. The only subcommand, refresh, is the "
            "only thing that touches the network for a library source -- "
            "it fetches every configured source (or just the one named) "
            "into the local cache."
        ),
        examples=(
            ("cof library refresh", "Refreshes every configured library source."),
            ("cof library refresh my-source", "Refreshes just the named source."),
        ),
        environment=CONFIG_ENVIRONMENT,
        files=CONFIG_FILES,
        see_also=(*SEE_ALSO_CORE, "cof-fetch(1)", "cof-run-library(1)"),
    ),
    "gen": PageExtra(
        description=(
            "Generates a new orchestration from a natural-language prompt, "
            "single-shot, by driving the bundled agents/meta_orchestrator.yml "
            "orchestration. For a multi-turn, clarifying-questions build "
            "instead, see cof wizard."
        ),
        examples=(
            (
                "cof gen my-flow 'summarize a URL and post it to slack'",
                "Generates my-flow.yml from a prompt.",
            ),
        ),
        environment=CONFIG_ENVIRONMENT,
        files=CONFIG_FILES,
        see_also=(*SEE_ALSO_CORE, "cof-wizard(1)", "cof-run(1)"),
    ),
    "wizard": PageExtra(
        description=(
            "Builds an orchestration headlessly by talking to the bundled "
            "wizard orchestration, from a one-line --goal, rather than a "
            "single prompt -- a multi-turn, clarifying-questions build. Not "
            "cof gen, which drives a different, single-shot orchestration."
        ),
        examples=(
            (
                "cof wizard --goal 'summarize a URL and post it to slack'",
                "Builds an orchestration through the wizard's questions.",
            ),
        ),
        environment=CONFIG_ENVIRONMENT,
        files=CONFIG_FILES,
        see_also=(*SEE_ALSO_CORE, "cof-gen(1)", "cof-run(1)"),
    ),
    "init": PageExtra(
        description="Initializes a new Circuitry project in the current directory: a starter config and example orchestration.",
        examples=(("cof init", "Scaffolds a new project in the current directory."),),
        see_also=(*SEE_ALSO_CORE, "cof-setup(1)"),
    ),
    "doctor": PageExtra(
        description=(
            "Runs system diagnostics: config resolution, the resolved "
            "adapter's connectivity (and, with --generate, a tiny "
            "real generate call), and every enabled extension's "
            "dependency check."
        ),
        examples=(
            ("cof doctor", "Checks config, adapter connectivity, and extensions."),
            ("cof doctor --generate", "Also makes one small real generate call."),
        ),
        environment=CONFIG_ENVIRONMENT,
        files=CONFIG_FILES,
        see_also=(*SEE_ALSO_CORE, "cof-setup(1)"),
    ),
    "score": PageExtra(
        description="Previews an orchestration's per-effect complexity scores, bands, and model routing decisions, without running it.",
        examples=(("cof score my-flow.yml", "Shows per-effect complexity scores."),),
        environment=CONFIG_ENVIRONMENT,
        files=CONFIG_FILES,
        see_also=(*SEE_ALSO_CORE, "cof-run(1)"),
    ),
    "setup": PageExtra(
        description=(
            "Interactive setup wizard: detects reachable backends "
            "(Ollama, ComfyUI), picks a default adapter and model, and "
            "writes ~/.config/circuitry/config.json (and, for a detected "
            "API key, ~/.config/circuitry/.env)."
        ),
        examples=(
            ("cof setup", "Runs the interactive setup wizard."),
            ("cof setup --json", "Detects backends and prints the result as JSON, non-interactively."),
        ),
        environment=(("CIRCUITRY_CONFIG", "Not consulted by setup itself; setup always writes the global config."),),
        files=(
            ("~/.config/circuitry/config.json", "Written with the detected/chosen adapter and model."),
            ("~/.config/circuitry/.env", "Written with a detected API key, when setup finds one."),
        ),
        see_also=(*SEE_ALSO_CORE, "cof-doctor(1)", "cof-init(1)"),
    ),
    "trust": PageExtra(
        description=(
            "Trusts a project config file, or consents to an orchestration "
            "document's capabilities. A .yml/.yaml PATH is a document "
            "(shows the capabilities it needs and asks); anything else is "
            "a project config file (shows what it sets and asks). Trust "
            "applies until the file's content changes."
        ),
        examples=(
            ("cof trust", "Trusts circuitry.config.json or config.json in the current directory."),
            ("cof trust my-flow.yml", "Shows and consents to a document's capabilities."),
            ("cof trust --list", "Lists every trusted file and whether it still matches."),
        ),
        environment=(("CIRCUITRY_CONFIG", "Used to resolve a document's use: ref: library sources, when trusting a document."),),
        files=(("~/.config/circuitry/trusted.json", "Where every trust decision is recorded."),),
        see_also=(*SEE_ALSO_CORE, "cof-untrust(1)", "cof-run(1)"),
    ),
    "untrust": PageExtra(
        description="Removes a project config file from ~/.config/circuitry/trusted.json, so it is no longer applied automatically.",
        examples=(("cof untrust", "Untrusts circuitry.config.json or config.json in the current directory."),),
        files=(("~/.config/circuitry/trusted.json", "Where every trust decision is recorded."),),
        see_also=(*SEE_ALSO_CORE, "cof-trust(1)"),
    ),
    "version": PageExtra(
        description="Prints the installed circuitry-cof package version (also: cof --version).",
        see_also=SEE_ALSO_CORE,
    ),
    "mcp": PageExtra(
        description="Runs the Circuitry MCP server on the stdio transport, for an MCP client to launch as a subprocess.",
        see_also=(*SEE_ALSO_CORE, "cof-run(1)"),
    ),
    "tui": PageExtra(
        description="Launches the terminal UI (requires the tui extra: pip install 'circuitry-cof[tui]').",
        see_also=(*SEE_ALSO_CORE, "osi(1)"),
    ),
    "man": PageExtra(
        description=(
            "Shows a cof man page through man on a terminal, or prints its "
            "roff when piped or given --roff. Works even where MANPATH "
            "doesn't reach the installed pages: the pages are generated "
            "from the same code this command itself runs."
        ),
        examples=(
            ("cof man run", "Shows cof-run(1) through man."),
            ("cof man run --roff | groff -man -Tutf8", "Renders cof-run(1)'s roff directly."),
        ),
        exit_status=(
            ("0", "The page was shown or printed."),
            ("2", "COMMAND doesn't name a page; the available pages are listed."),
        ),
        see_also=SEE_ALSO_CORE,
    ),
}


def _name_blurb(short: str) -> str:
    """The NAME line's own one-clause blurb: everything up to an em
    dash or ` - `, so a long help sentence (which may also carry a
    multi-byte em dash) doesn't push NAME past the line-length lint."""
    return re.split(r"\s[—-]\s", short, maxsplit=1)[0].rstrip(".")


def _get_root_command() -> Command:
    from .app import app

    return typer.main.get_command(app)


def _ctx_for(cmd: Command, info_name: str, parent: Context | None = None) -> Context:
    return click.Context(cmd, info_name=info_name, parent=parent)


def _is_argument(param: click.Parameter) -> bool:
    """Whether *param* is a positional argument rather than an option.

    Checked by Click's own ``param_type_name``, not ``isinstance``: Typer's
    ``TyperArgument``/``TyperOption`` subclass a forked copy of
    ``click.Parameter`` (``typer._click``), so ``isinstance(param,
    click.Argument)`` is always ``False`` against the real click -- every
    param on a Typer-built command would otherwise read as an option.
    """
    return getattr(param, "param_type_name", None) == "argument"


def _metavar(param: click.Parameter, ctx: click.Context) -> str | None:
    # Typer's forked `Parameter` (`typer._click`/`typer.core`) has no
    # `make_metavar` at all, under either form -- an argument's metavar
    # falls back to its own uppercased name the same way click's own
    # `make_metavar` would with nothing fancier (brackets, ellipsis) set.
    if _is_argument(param):
        return param.metavar or ((param.name or "").upper() or None)
    if getattr(param, "is_flag", False) or getattr(param, "count", False):
        return None
    return param.metavar or param.name.upper() if param.name else None


def _flag_text(param: click.Parameter) -> str:
    opts = list(param.opts) + list(getattr(param, "secondary_opts", None) or [])
    return ", ".join(opts)


def _option_help(param: click.Parameter) -> str:
    help_text = getattr(param, "help", None) or ""
    extra: list[str] = []
    default = getattr(param, "default", None)
    if (
        default is not None
        and default is not False
        and not isinstance(default, (list, tuple))
        and getattr(param, "required", False) is False
        and not getattr(param, "is_flag", False)
    ):
        extra.append(f"[default: {default}]")
    text = help_text
    if extra:
        text = f"{text} {' '.join(extra)}".strip()
    return text or "(undocumented)"


def render_options_section(cmd: Command) -> str:
    """OPTIONS, built straight from *cmd*'s own Click params -- every
    option and argument the live CLI accepts, in declaration order."""
    ctx = click.Context(cmd)
    out: list[str] = []
    for param in cmd.params:
        if _is_argument(param):
            heading = _metavar(param, ctx) or (param.name or "").upper()
        else:
            heading = _flag_text(param)
        metavar = _metavar(param, ctx) if not _is_argument(param) else None
        body = _option_help(param)
        raw_prefix = f"\\fI{metavar}\\fR " if metavar else ""
        out.append(_tp(heading, body, raw_prefix=raw_prefix))
    return "".join(out)


def render_synopsis(command_path: str, cmd: Command) -> str:
    ctx = _ctx_for(cmd, command_path)
    pieces = cmd.collect_usage_pieces(ctx)
    return " ".join([command_path, *pieces])


def _th_line(title: str, version: str) -> str:
    return f'.TH {title} 1 "{MAN_DATE}" "cof {version}" "User Commands"\n'


def _version() -> str:
    from .version import resolve_version

    return resolve_version()


def render_page(name: str, *, version: str | None = None) -> str:
    """Builds *name*'s own full roff man page (a key of
    :data:`PAGE_EXTRAS`, or an alias in :data:`ALIASES`). Both `cof man`
    and `scripts/generate-man-pages.py` call this directly.

    *version* defaults to the installed package's own version -- correct
    for `cof man`, which only ever runs where that install is this build.
    The generator passes `pyproject.toml`'s version explicitly instead, so
    a stale editable install can't write a page with the wrong one.
    """
    if name in ALIASES:
        target = ALIASES[name]
        return f".so man1/cof-{target}.1\n"

    version = version or _version()
    root = _get_root_command()

    if name == "cof":
        title = "COF"
        name_line = "cof \\- run, check and manage Circuitry orchestrations"
        synopsis = render_synopsis("cof", root)
        options = render_options_section(root)
        commands_section = ".SH COMMANDS\n" + "".join(
            _tp(n, (root.commands[n].help or root.commands[n].get_short_help_str() or "").strip() or "(undocumented)")
            for n in sorted(root.commands)
        )
    elif name == "library":
        sub = root.commands["library"]
        title = "COF-LIBRARY"
        blurb = _format_text((sub.help or '').splitlines()[0].strip() or 'manage library sources')
        name_line = f"cof-library \\- {blurb}"
        refresh = sub.commands["refresh"]
        synopsis = render_synopsis("cof library", sub) + "\n.br\n" + render_synopsis("cof library refresh", refresh)
        options = render_options_section(refresh)
        commands_section = ""
    else:
        cmd = root.commands[name]
        title = f"COF-{name.upper()}"
        short = (cmd.help or cmd.get_short_help_str() or name).splitlines()[0].strip()
        blurb = _format_text(_name_blurb(short))
        name_line = f"cof-{name} \\- {blurb}"
        synopsis = render_synopsis(f"cof {name}", cmd)
        options = render_options_section(cmd)
        commands_section = ""

    extra = PAGE_EXTRAS.get(name) or PageExtra(description="(undocumented)")
    sections = [
        _th_line(title, version),
        ".SH NAME\n",
        _wrap(name_line),
        "\n.SH SYNOPSIS\n.nf\n",
        synopsis,
        "\n.fi\n",
        ".SH DESCRIPTION\n",
        _wrap_paragraphs(extra.description),
        "\n",
    ]
    if options:
        sections.append(".SH OPTIONS\n")
        sections.append(options)
    sections.append(commands_section)
    if extra.examples:
        sections.append(".SH EXAMPLES\n")
        sections.append(_tp_section(extra.examples))
    if extra.environment:
        sections.append(".SH ENVIRONMENT\n")
        sections.append(_tp_section(extra.environment))
    if extra.files:
        sections.append(".SH FILES\n")
        sections.append(_tp_section(extra.files, bold=False))
    sections.append(".SH EXIT STATUS\n")
    sections.append(_tp_section(extra.exit_status))
    if extra.see_also:
        sections.append(".SH SEE ALSO\n")
        sections.append(", ".join(f"\\fB{n}\\fR" for n in extra.see_also) + "\n")
    return "".join(sections)


def all_page_names() -> tuple[str, ...]:
    """Every page `cof man`/the generator know -- `cof` itself plus
    every :data:`COMMAND_ORDER` entry (aliases included)."""
    return ("cof", *COMMAND_ORDER)


def register_man(app: Any) -> None:
    @app.command(
        "man",
        help="Show a cof man page through man, or print its roff when piped or given --roff.",
    )
    def man_cmd(
        command: str | None = typer.Argument(
            None, help="Command to show the page for (default: cof itself)."
        ),
        roff: bool = typer.Option(False, "--roff", help="Print the page's roff instead of showing it through man."),
    ) -> None:
        name = command or "cof"
        if name not in all_page_names():
            console.print(f"[red]No such page:[/red] {name}")
            console.print("Pages: " + ", ".join(all_page_names()))
            raise typer.Exit(code=2)

        # An alias (`check`) shows its target's own content directly --
        # never the tiny `.so man1/cof-validate.1` stub, which only
        # resolves relative to an installed MANPATH tree, not a temp
        # file or a pipe. That stub form is for the committed file alone.
        resolved = ALIASES.get(name, name)
        text = render_page(resolved)
        if roff or not sys.stdout.isatty():
            sys.stdout.write(text)
            return

        fd, path_str = tempfile.mkstemp(suffix=".1", prefix=f"{resolved if resolved == 'cof' else f'cof-{resolved}'}.")
        path = Path(path_str)
        try:
            with open(fd, "w", encoding="utf-8") as fh:
                fh.write(text)
            man_bin = shutil.which("man")
            if man_bin is None:
                sys.stdout.write(text)
                return
            result = subprocess.run([man_bin, str(path)], check=False)
            if result.returncode != 0:
                raise typer.Exit(code=result.returncode)
        finally:
            path.unlink(missing_ok=True)
