from __future__ import annotations

import importlib.resources
import json
import os
import sys
import tempfile
from contextlib import nullcontext
from pathlib import Path
from typing import Any

import typer
from rich.console import Console
from rich.markup import escape
from rich.panel import Panel
from rich.table import Table
from typer.core import TyperGroup

from ..core.resume import document_sha256
from ..core.saved_state import dumps_saved_state
from ..core.step_cache import StepCache
from ..core.store import build_persistence_backend

# The wizard host (chat's transcript, verdict, and save logic) — `cof wizard`
# drives the exact same functions `circuitry.tui.chat.ChatScreen` does, so the
# two hosts can never produce different artifacts from the same input.
from ..tui.wizard_host import CATEGORIES as WIZARD_CATEGORIES
from ..tui.wizard_host import DEFAULT_CATEGORY as WIZARD_DEFAULT_CATEGORY
from ..tui.wizard_host import (
    Conversation,
    InvalidDraft,
    Seed,
    Turn,
    default_library_dir,
    drive_conversation,
    run_turn,
    save_to_file,
    save_to_library,
)
from .complexity_config import ComplexitySettings
from .config import (
    GLOBAL_CONFIG_DIR,
    CircuitryConfig,
    ConfigError,
    describe_config_sources,
    resolve_config,
    trust_store_path,
)
from .config_trust import TrustStoreError, record_trust
from .doctor import register_doctor
from .effective_settings import resolve_effective_settings
from .explain_routing import make_explain_routing_observer
from .interrupts import sigterm_as_interrupt
from .last_run import LAST_RUN_PATH, read_last_run
from .library_sources import (
    Entry,
    LibraryFetchError,
    LibraryRegistry,
    LibrarySourceError,
    build_registry,
)
from .logging_setup import configure_cli_logging
from .orchestration_loader import load_orchestration_file, serialize_orchestration
from .profiles import ProfileError, ProfileSettings, load_profile
from .redaction import REDACTED, redact_env_pairs
from .registry import eject_destination, load_index, resolve_bundled, write_ejected
from .score import register_score
from .setup import register_setup
from .shared_library import (
    apply_service_profile,
    fetch_shared_orchestration,
    resolve_service_profile,
)
from .state_merge import apply_inline_overrides
from .trust import register_trust

console = Console()
err_console = Console(stderr=True)


# `.runtime_shim` pulls in `core.compiler` -> `core.cel_eval` (a full CEL
# grammar parser via celpy/lark) and every adapter — real cost for commands
# that actually run/validate an orchestration, but wasted on `--help`/
# `version`/every other command that doesn't. These four names keep the same
# module-level, patchable surface (`patch("circuitry.cli.app.run", ...)` in
# tests) while deferring the import to first call.
def RunRequest(**kwargs: Any) -> Any:
    from .runtime_shim import RunRequest as _RunRequest

    return _RunRequest(**kwargs)


def run(req: Any) -> Any:
    from .runtime_shim import run as _run

    return _run(req)


def validate(*args: Any, **kwargs: Any) -> Any:
    from .runtime_shim import validate as _validate

    return _validate(*args, **kwargs)


def inspect_orchestration(*args: Any, **kwargs: Any) -> Any:
    from .runtime_shim import inspect_orchestration as _inspect_orchestration

    return _inspect_orchestration(*args, **kwargs)


class CircuitryGroup(TyperGroup):
    """Root command group that renders config problems as one actionable line.

    Every subcommand taking ``--config/-c`` reaches the same loader, so the
    catch lives here once instead of in per-command ``try``/``except`` blocks —
    commands registered from other modules (``doctor``, ``setup``) and any
    future ones are covered automatically.
    """

    # ``ctx`` is typed Any because Typer vendors its own click.Context under a
    # private module path; naming either concrete class trips mypy's override check.
    def invoke(self, ctx: Any) -> Any:
        try:
            return super().invoke(ctx)
        except ConfigError as exc:
            # soft_wrap keeps the message on a single line regardless of
            # terminal width, so it stays greppable when piped.
            err_console.print(
                f"[red]Error:[/red] {escape(str(exc))}", highlight=False, soft_wrap=True
            )
            raise typer.Exit(code=1) from exc


app = typer.Typer(
    cls=CircuitryGroup,
    add_completion=False,
    help="Circuitry — Cybernetic orchestration framework. (cof)",
    rich_markup_mode="rich",
)


def _resolve_version() -> str:
    from importlib.metadata import PackageNotFoundError
    from importlib.metadata import version as pkg_version

    # Distribution name is `circuitry-cof` on PyPI; the legacy `circuitry`
    # lookup is kept as a fallback for editable installs that pre-date the
    # rename.
    for dist in ("circuitry-cof", "circuitry"):
        try:
            return pkg_version(dist)
        except PackageNotFoundError:
            continue
    return "0.1.0+unknown"


def _version_callback(value: bool) -> None:
    # ``is_eager=True`` on the option means this runs before Typer resolves
    # a subcommand, same as ``--help`` — ``cof --version`` doesn't need (or
    # want) a subcommand at all, matching the existing `version` subcommand.
    if value:
        console.print(f"Circuitry {_resolve_version()}")
        raise typer.Exit()


@app.callback(invoke_without_command=True)
def _root(
    ctx: typer.Context,
    version: bool = typer.Option(
        False, "--version",
        callback=_version_callback, is_eager=True,
        help="Print version and exit.",
    ),
) -> None:
    # No docstring/help here on purpose: the group's help text comes from
    # ``Typer(help=...)`` above and must stay byte-identical.
    del version  # handled by the eager callback above
    # Baseline WARNING on every invocation; a command with its own
    # --verbose/-v bumps this to INFO once its own options are parsed.
    configure_cli_logging()
    if ctx.invoked_subcommand is not None:
        return
    from ..tui import run_tui, should_launch_tui

    if should_launch_tui():
        run_tui()
        raise typer.Exit()
    # Not an interactive terminal (or no `tui` extra): reproduce exactly what
    # Click does for a group invoked without a subcommand.
    ctx.fail("Missing command.")


register_doctor(app)
register_score(app)
register_setup(app)
register_trust(app)

#: Aliased from :mod:`circuitry.cli.last_run`, which the TUI's replay reads
#: too — one location, so the two can never disagree about where the stash is.
_LAST_RUN_PATH = LAST_RUN_PATH


def _print_header(title: str) -> None:
    console.print(Panel.fit(title, border_style="cyan"))


def _write_state_json(*, out: Path, state: dict, pretty: bool) -> None:
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(dumps_saved_state(state, pretty=pretty) + "\n", encoding="utf-8")


def _print_run_warnings(warnings: list[str]) -> None:
    """Print a run's warnings on stderr, for a failed run as for a good one.

    Stderr leaves `--json` / `--tail` / piped stdout machine-readable, so
    `--quiet` (which a pipe implies) does not silence them: a warning can say
    that part of the orchestration was ignored.
    """
    for w in warnings:
        err_console.print(
            f"[yellow]Warning:[/yellow] {escape(w)}", highlight=False, soft_wrap=True
        )


def _parse_allow_capabilities(value: str | None) -> frozenset[str] | None:
    """``--allow-capabilities shell,network`` -> ``{"shell", "network"}``."""
    if not value:
        return None
    names = frozenset(part.strip() for part in value.split(",") if part.strip())
    return names or None


def _confirm_capabilities(label: str, capabilities: frozenset[str]) -> bool:
    """The first-run capability consent prompt (#275): what *label* needs,
    then y/N. Only ever reached when stdin/stdout are both a TTY and the
    caller isn't `--quiet`/`--json` — see `_capability_prompt` below.
    """
    names = ", ".join(sorted(capabilities))
    console.print(f"[bold]{escape(label)}[/bold] needs: [yellow]{escape(names)}[/yellow]")
    return typer.confirm("Allow it? (recorded for this document until it changes)", default=False)


def _capability_prompt(*, quiet: bool, json_out: bool) -> Any:
    """The interactive capability-consent callback for this invocation, or
    `None` (refuse rather than prompt) for anything that isn't a real
    terminal on both ends — `--quiet`/`--json`, a pipe, CI (#275 rule 5).
    """
    if quiet or json_out or not (sys.stdin.isatty() and sys.stdout.isatty()):
        return None
    return _confirm_capabilities


def _status_pausing_prompt(
    base_prompt: Any, status_holder: dict[str, Any]
) -> Any:
    """Wrap *base_prompt* so it stops the run's "Running…" status spinner
    before asking (and resumes it after) — the two otherwise fight over the
    terminal. *status_holder* is filled with the live ``Status`` only once
    the caller actually enters it, so most runs (no prompt ever fires) pay
    nothing beyond this wrapper. ``None`` in, ``None`` out.
    """
    if base_prompt is None:
        return None

    def _prompt(label: str, capabilities: frozenset[str]) -> bool:
        status = status_holder.get("status")
        if status is not None:
            status.stop()
        try:
            return bool(base_prompt(label, capabilities))
        finally:
            if status is not None:
                status.start()

    return _prompt


def _read_state_file(path: Path) -> dict[str, Any]:
    """Read a --state JSON file, failing loudly when it doesn't exist."""
    if not path.exists():
        raise FileNotFoundError(f"state file not found: {path}")
    return json.loads(path.read_text(encoding="utf-8"))


def _show_loop_progress(*, verbose: bool, quiet: bool, json_out: bool) -> bool:
    """Whether a loop's interactive progress line may print (#271): verbose,
    a real TTY, and neither ``--quiet`` nor ``--json``. The TTY check reads
    ``sys.stdout`` at call time (not a parameter) so it always reflects
    whatever the process's stdout actually is at the moment a run starts."""
    return bool(verbose and not quiet and not json_out and sys.stdout.isatty())


def _format_seconds(value: float) -> str:
    if value >= 60:
        minutes, rest = divmod(value, 60)
        return f"{int(minutes)}m{rest:04.1f}s"
    return f"{value:.1f}s"


def _format_run_totals_line(totals: Any) -> str | None:
    """The one-line run summary (#271): wall time, effects, tokens, cost.

    ``None`` when ``totals`` isn't the dict ``run()`` writes — a caller on
    an old state shape, or a run that failed before totals were computed.
    """
    if not isinstance(totals, dict):
        return None
    bits: list[str] = []
    wall_time_s = totals.get("wall_time_s")
    if isinstance(wall_time_s, (int, float)):
        bits.append(_format_seconds(wall_time_s))
    effects_run = totals.get("effects_run")
    if isinstance(effects_run, int):
        bits.append(f"{effects_run} effects")
    tokens_sent = totals.get("tokens_sent")
    tokens_received = totals.get("tokens_received")
    if isinstance(tokens_sent, int) or isinstance(tokens_received, int):
        bits.append(f"↑{tokens_sent or 0} ↓{tokens_received or 0} tok")
    cost_usd = totals.get("cost_usd")
    if isinstance(cost_usd, (int, float)):
        bits.append(f"${cost_usd:.4f}")
    if not bits:
        return None
    return "  ·  ".join(bits)


def _print_missing_state_file_error(exc: FileNotFoundError, *, json_out: bool) -> None:
    """Report a missing `--state` file the same way a failed run does, so
    `--json` output stays valid JSON on this (`--state ... -e ...`) early-exit
    path too, not just the one `run()` itself reaches."""
    if json_out:
        console.print_json(
            json.dumps(
                {"ok": False, "error": str(exc), "warnings": [], "state_out": None}
            )
        )
    else:
        console.print(f"[red]Error:[/red] {exc}")


def _parse_env_vars(env_vars: list[str] | None) -> dict[str, Any]:
    """Parse -e KEY=VALUE entries into a state dict."""
    if not env_vars:
        return {}
    result: dict[str, Any] = {}
    for entry in env_vars:
        if "=" not in entry:
            raise typer.BadParameter(f"Invalid -e format: {entry!r} (expected KEY=VALUE)")
        key, value = entry.split("=", 1)
        # Try parsing as JSON for structured values
        try:
            parsed = json.loads(value)
            result[key] = parsed
        except (json.JSONDecodeError, ValueError):
            result[key] = value
    return result


def _raw_env_var_text(env_vars: list[str] | None) -> dict[str, str]:
    """Each `-e KEY=VALUE`'s exact text, keyed by KEY — what `_parse_env_vars`
    sees before JSON-sniffing it into a number/boolean/structured value.
    """
    if not env_vars:
        return {}
    return dict(entry.split("=", 1) for entry in env_vars if "=" in entry)


def _restore_raw_text_for_string_inputs(
    inline: dict[str, Any], raw: dict[str, str], orch_path: Path
) -> None:
    """Give back the exact `-e` text for any key `interface.inputs` declares
    `type: string`, undoing `_parse_env_vars`'s JSON-sniffing for it — so
    `-e start=06` keeps "06", `-e x=1.50` keeps its trailing zero, and
    `-e drawn=true` keeps its exact case, instead of round-tripping through
    a parsed int/float/bool first.

    Best-effort: an orchestration that fails to load here still runs (and
    fails, with its own error) through the normal path below — this peek
    only restores fidelity, it never blocks the run.
    """
    try:
        doc = load_orchestration_file(orch_path)
    except Exception:
        return
    if not isinstance(doc, dict):
        return
    iface = doc.get("interface")
    if not isinstance(iface, dict):
        return
    iface_inputs = iface.get("inputs")
    if not isinstance(iface_inputs, dict):
        return
    for key, spec in iface_inputs.items():
        if isinstance(spec, dict) and spec.get("type") == "string" and key in raw:
            inline[key] = raw[key]


def _find_last_effect_value(state: dict[str, Any]) -> Any:
    """Walk prime to find the last completed effect's value, recursing into dynamics."""
    prime = state.get("prime")
    if not isinstance(prime, dict):
        return None
    last_val = None
    for key, val in prime.items():
        if key in ("value", "meta"):
            continue
        if isinstance(val, dict):
            # If this child is a dynamic/scope container, recurse into it
            # to find the deepest leaf value.
            inner = _find_deepest_value(val)
            if inner is not None:
                last_val = inner
    return last_val


def _find_deepest_value(node: dict[str, Any]) -> Any:
    """Recursively find the last 'value' in a nested effect tree."""
    last_val = None
    if "value" in node:
        # Check if this is a leaf (value is not just a container marker like True)
        candidate = node["value"]
        if not isinstance(candidate, bool):
            last_val = candidate
    for key, val in node.items():
        if key in ("value", "meta"):
            continue
        if isinstance(val, dict):
            inner = _find_deepest_value(val)
            if inner is not None:
                last_val = inner
    return last_val


def _extract_generated_orchestration(yaml_text: str) -> dict[str, Any] | None:
    """Pull the orchestration mapping out of `cof gen`'s raw model output.

    Tries the whole (fence/separator-stripped) text first, which handles a
    bare document — but only if every top-level key is one the schema
    recognizes. A model reply with a prose preamble line (e.g. "Here's the
    YAML:") parses as a *valid* mapping once fences are stripped, with the
    preamble as a bogus key, so that key is the only signal left that this
    wasn't a bare document. Models imitating the bundled examples' own house
    style often open with a `#`-commented header (e.g. `# effects:\\n  - type:
    ...`) before the real content; either case falls through to: drop
    comment-only lines and look for the first effects:/adapter:/interface:
    line to find the real document's start.
    """
    import yaml as _yaml  # type: ignore[import-untyped]

    from ..core.document_check import _known_top_level_keys

    try:
        parsed = _yaml.safe_load(yaml_text)
    except _yaml.YAMLError:
        parsed = None
    if isinstance(parsed, dict) and set(parsed).issubset(_known_top_level_keys()):
        return parsed

    lines = [line for line in yaml_text.splitlines() if not line.strip().startswith("#")]
    for i, line in enumerate(lines):
        if line.startswith(("effects:", "adapter:", "interface:")):
            candidate = "\n".join(lines[i:]).strip()
            try:
                parsed = _yaml.safe_load(candidate)
            except _yaml.YAMLError:
                return None
            return parsed if isinstance(parsed, dict) else None
    return None


def _save_last_run(args: dict[str, Any]) -> None:
    """Stash the current run args for --last replay."""
    try:
        GLOBAL_CONFIG_DIR.mkdir(parents=True, exist_ok=True)
        _LAST_RUN_PATH.write_text(json.dumps(args) + "\n", encoding="utf-8")
    except Exception:
        pass  # Best-effort; don't fail the run


def _load_last_run() -> dict[str, Any]:
    """Load the stashed last-run args."""
    if not _LAST_RUN_PATH.exists():
        raise typer.BadParameter("No previous run found. Run an orchestration first.")
    return json.loads(_LAST_RUN_PATH.read_text(encoding="utf-8"))


def _resolve_resume_state(
    *,
    resume: str,
    force: bool,
    state_arg: Path | None,
    orch_path: Path,
    env_vars: list[str] | None,
    cfg: CircuitryConfig,
    trust_document: bool,
) -> tuple[dict[str, Any], list[str], Path | None]:
    """The saved state ``--resume`` continues from, the args to replay
    alongside it, and the file it came from (``None`` for a run-id, which
    has no single file of its own) — with both safety checks (#270)
    already applied. The caller uses the third value as the default
    ``--out`` when it didn't name one itself (#270 F10).

    Three sources, picked by what the caller gave:

    * ``--state <file>`` (also given): that file, named explicitly — the
      most direct way to point at "whatever the run left behind".
    * ``--resume last``: the most recent run's own ``--out``, found via the
      ``--last`` stash (#320) — that file already carries its own resolved
      ``input.*``, so (unlike plain ``--last``) nothing from the stash's
      ``-e`` args is replayed on top of it.
    * ``--resume <run-id>``: looked up in the orchestration's configured
      ``runtime.persistence`` backend.

    Raises ``typer.BadParameter`` naming exactly what's missing or
    mismatched; the caller turns that into the usual CLI error.
    """
    replay_env_vars: list[str] = []
    resume_source_path: Path | None = None
    if state_arg is not None:
        try:
            loaded = _read_state_file(state_arg)
        except FileNotFoundError as exc:
            raise typer.BadParameter(str(exc)) from exc
        resume_source_path = state_arg
    elif resume == "last":
        stash = read_last_run()
        if stash is None:
            raise typer.BadParameter(
                "No previous run found — nothing to resume from."
            )
        if not stash.ok:
            raise typer.BadParameter(f"{stash.error} Nothing to resume from.")
        if Path(stash.orchestration) != orch_path and stash.orchestration != str(orch_path):
            raise typer.BadParameter(
                f"The last run was {stash.orchestration!r}, not {orch_path} — "
                "run that orchestration again, or pass --state <file> to "
                "resume a specific saved state instead of 'last'."
            )
        if stash.out_path is None:
            raise typer.BadParameter(
                "The last run did not use --out, so it saved no state to resume from."
            )
        try:
            loaded = _read_state_file(stash.out_path)
        except FileNotFoundError as exc:
            raise typer.BadParameter(
                f"The last run's --out file is gone: {exc}"
            ) from exc
        resume_source_path = stash.out_path
        # Not replayed from the stash: the saved state (just loaded) already
        # carries the resolved `input.*` from that run, so replaying the
        # stashed `-e` pairs on top would be redundant at best — and at
        # worst (a redacted secret) would feed the literal redaction marker
        # in as a real value, which plain `--last` already refuses outright
        # (#270 F9). A caller who wants different inputs this time passes
        # its own `-e`, checked against `loaded["input"]` below.
    else:
        try:
            orch = load_orchestration_file(orch_path)
            effective = resolve_effective_settings(
                cfg=cfg,
                orch=orch,
                trust_document=trust_document,
                document_name=orch_path.name,
            )
            persistence = build_persistence_backend(effective.runtime or {})
        except typer.BadParameter:
            raise
        except Exception as exc:
            raise typer.BadParameter(
                f"Could not resolve {orch_path}'s runtime.persistence backend "
                f"to look up --resume {resume!r}: {exc}"
            ) from exc
        if persistence is None:
            raise typer.BadParameter(
                "--resume <run-id> needs this orchestration's "
                "runtime.persistence backend enabled — that's how a run-id "
                "is looked up. Use --resume last or --state <file> instead, "
                "or configure persistence."
            )
        try:
            found = persistence.load_run(orchestration_path=str(orch_path), run_id=resume)
        except Exception as exc:
            raise typer.BadParameter(
                f"Could not look up --resume {resume!r}: {exc}"
            ) from exc
        if found is None:
            raise typer.BadParameter(
                f"No persisted state found for run-id {resume!r} and "
                f"orchestration {orch_path} — check the run-id, and that it "
                "was run against this same orchestration."
            )
        loaded = found

    if not isinstance(loaded, dict):
        raise typer.BadParameter("The saved state to resume from is not a JSON object.")

    # Safety 1: refuse a document that changed since the run being resumed —
    # a stale node's recorded `params_rendered`/output could no longer match
    # what this document would now produce for it.
    last_run_record = loaded.get("runtime", {}).get("last_run") if isinstance(
        loaded.get("runtime"), dict
    ) else None
    recorded_hash = (
        last_run_record.get("document_hash")
        if isinstance(last_run_record, dict)
        else None
    )
    if not force:
        if not recorded_hash:
            raise typer.BadParameter(
                "The saved state to resume from has no "
                "runtime.last_run.document_hash — it wasn't written by `cof "
                "run` (or predates this field), so there is no way to tell "
                "whether this document changed since it ran. Pass --force "
                "to resume anyway."
            )
        try:
            current_hash = document_sha256(orch_path)
        except OSError:
            current_hash = None
        if current_hash is not None and current_hash != recorded_hash:
            raise typer.BadParameter(
                f"{orch_path} changed since the run being resumed (content "
                "hash differs) — rerun from scratch, or pass --force to "
                "resume anyway."
            )

    # Safety 2: refuse silently-inherited inputs when there's no args stash
    # to fall back on. --state names its own file (as explicit as it gets)
    # and --resume last loads the prior run's own --out, which already
    # carries its resolved input.* — only a bare run-id has neither, so
    # that's the one path that must see every one of the saved run's input
    # keys named again via -e, not merely *some* -e, before reusing
    # anything (#270 F7: a single unrelated -e used to be enough to pass).
    recorded_input_ns = loaded.get("input")
    if (
        state_arg is None
        and resume != "last"
        and isinstance(recorded_input_ns, dict)
        and recorded_input_ns
    ):
        passed_keys = {
            pair.split("=", 1)[0]
            for pair in (env_vars or [])
            if isinstance(pair, str) and "=" in pair
        }
        missing = sorted(set(recorded_input_ns) - passed_keys)
        if missing:
            raise typer.BadParameter(
                f"The saved run had explicit inputs ({', '.join(missing)}) "
                "that this invocation didn't pass again — pass them with "
                "-e to resume, or rerun from scratch."
            )

    return loaded, replay_env_vars, resume_source_path


def _do_validate(
    orchestration: Path,
    json_out: bool,
    *,
    config: Path | None = None,
    skip_preflight: bool = False,
) -> None:
    """Shared validation logic for validate and check commands."""
    # Resolve config (incl. allowlist + env-var overlays) so allowlist
    # enforcement runs alongside schema checks. ``skip_preflight``
    # disables the dependency-readiness checks in offline CI / smoke
    # contexts where binaries / hosts are not available. Resolved before
    # the header so a bad --config prints the error alone.
    cfg = resolve_config(explicit_path=config)
    if not json_out:
        _print_header("Circuitry · Validate")
        console.print(f"[bold]Config:[/bold] {describe_config_sources(cfg.sources)}")
    with console.status("[cyan]Validating…[/cyan]") if not json_out else nullcontext():
        # The argument is always a path the user named: trusted like `cof run
        # ./file.yml`, so the report matches what that run would apply.
        result = validate(
            orchestration,
            config=cfg,
            skip_preflight=skip_preflight,
            trust_document=True,
        )

    if json_out:
        console.print_json(json.dumps(result, ensure_ascii=False))
        raise typer.Exit(code=0 if result["ok"] else 1)

    if result["ok"]:
        console.print("[green]Valid[/green]")
    else:
        console.print("[red]Invalid[/red]")
        for e in result.get("errors", []):
            console.print(f" - {escape(str(e))}")

    # Advisory only — deprecated aliases and type-keyword names still run.
    # Printed for both outcomes; never affects the exit code. Warnings can
    # quote document text (e.g. an ignored runtime key), so escape markup.
    for w in result.get("warnings", []):
        console.print(f"[yellow]Warning:[/yellow] {escape(w)}")

    if not result["ok"]:
        raise typer.Exit(code=1)


def _library_registry(
    config_path: Path | None = None, *, cfg: CircuitryConfig | None = None
) -> LibraryRegistry:
    """Build the configured library registry, reporting config errors as CLI errors."""
    try:
        return build_registry(config_path=config_path, cfg=cfg)
    except LibrarySourceError as exc:
        console.print(f"[red]Error:[/red] {exc}")
        raise typer.Exit(code=1) from exc


def _warn(message: str) -> None:
    console.print(f"[yellow]Warning:[/yellow] {message}")


def _print_source_notices(registry: LibraryRegistry) -> None:
    """Surface "not fetched yet" hints so a miss points at the right fix."""
    for message in registry.notices():
        _warn(message)


def _lookup_entry(registry: LibraryRegistry, name: str) -> Entry | None:
    """Find an entry by bare or source-qualified name, warning on ambiguity."""
    resolution = registry.resolve(name)
    if resolution is None:
        return None
    if resolution.is_ambiguous:
        _warn(resolution.ambiguity_warning(name))
    return resolution.entry


def _names_a_file(name_or_path: str) -> bool:
    """Whether *name_or_path* is a file on disk rather than a library name.

    The first step of `_resolve_orchestration`, and what makes a `cof run`
    argument a document the user named by path — a trusted one (see
    RunRequest.trust_document).
    """
    return Path(name_or_path).is_file()


def _resolve_orchestration(
    name_or_path: str,
    *,
    registry: LibraryRegistry | None = None,
    on_warning: Any = None,
) -> Path | None:
    """Resolve an orchestration argument to a file path.

    Resolution order:
      1. Literal file path (exists on disk)
      2. Library source lookup by name, in `runtime.library.sources` order.
         Source-qualified names (`"<source>:<name>"`) skip precedence.

    Ambiguity (a bare name matching more than one source) is reported through
    *on_warning* rather than printed, so non-CLI callers — the MCP server talks
    JSON-RPC over stdout — stay silent by default.
    """
    # 1. Try as a file path
    if _names_a_file(name_or_path):
        return Path(name_or_path)

    # 2. Try library source resolution
    try:
        reg = registry if registry is not None else build_registry()
    except LibrarySourceError:
        # A malformed sources config must not make bundled names unreachable;
        # commands surface the error separately via _library_registry().
        return resolve_bundled(name_or_path)

    resolution = reg.resolve(name_or_path)
    if resolution is None:
        return None
    if resolution.is_ambiguous and callable(on_warning):
        on_warning(resolution.ambiguity_warning(name_or_path))
    return resolution.path


def _is_remote_library_source(name_or_path: str, registry: LibraryRegistry) -> bool:
    """Whether *name_or_path* resolves to an entry from a refreshable
    (``REFRESHABLE``) library source — today, a ``github`` one.

    Capability consent (#275) scopes its whole-document gate to documents
    that did not come from the user's own disk: a ``cof fetch``/
    ``cof run-library`` asset, and — this — a remote library source run by
    bare name (``cof run hub/entry``), not just by the dedicated command. A
    ``folder``/``curation`` source stays ungated: both are already on the
    user's own disk or bundled with Circuitry itself, same as a path run.
    """
    if _names_a_file(name_or_path):
        return False
    resolution = registry.resolve(name_or_path)
    if resolution is None:
        return False
    return registry.is_refreshable(resolution.entry.source)


RUN_EPILOG = """
[bold]Examples:[/bold]
  cof run hello -e name=World
  cof run article-summarizer -e article_text='...'
  cof run ./my-orch.yml -e topic=cats --tail
  cof run ./my-orch.yml --live-state ./state.json
  cof run learn/hello -e name=World --model gpt-oss:20b
  cof run learn/hello -e name=World --adapter ollama --model llama3.1:8b
  cof run ./my-orch.yml --explain-routing
  cof run ./my-orch.yml --decompose-out ./plans
  cof run ./my-orch.yml --scoring --routing --decompose
  cof run ./my-orch.yml --no-routing
  cof run --last
  cof run ./my-orch.yml --resume last
  cof run ./my-orch.yml --state ./run.json --resume x

[bold]Resolution order:[/bold] local file path > bundled orchestration name.

[bold]--resume[/bold] continues an interrupted or failed run of the SAME
orchestration from its saved state: an effect that already finished without
error is skipped and reused; the first unfinished or failed one, and
everything after it, reruns (a named loop resumes at its first unfinished
pass). 'last' finds the saved state through the --last stash; a run-id is
looked up in the orchestration's runtime.persistence backend; --state
<file> --resume names a saved state file directly. Refuses a document that
changed since that run unless --force, and refuses silently-inherited
inputs on the run-id path unless -e passes them again.

[bold]Trust:[/bold] a file you name by path applies its whole runtime: block
and plugins: list, with one notice naming any host settings among them; a
library name applies only runtime.complexity and runtime.state.

[bold]Settings precedence:[/bold] CLI flags (--adapter/--model) > --profile >
orchestration > environment (CIRCUITRY_ADAPTER/CIRCUITRY_MODEL) > config file
> defaults. Env vars overlay the config layer, so a flag or profile beats them.

[bold]Complexity switches:[/bold] --scoring/--no-scoring, --routing/--no-routing,
and --decompose/--no-decompose each override one runtime.complexity.<switch>.enabled
for this run only, ranking above the orchestration and config the same way
--adapter/--model do. --routing/--decompose still need scoring on (from
--scoring or config) — turning either on without it is the same prerequisite
error the config path raises. --no-routing also overrides any --profile
per-effect routing pin, since routing is off for the whole run either way.
Run [bold]cof list[/bold] to see available bundled orchestrations.

[yellow]Note:[/yellow] do not pass secrets via -e KEY=VALUE; use environment
variables or a config file instead. Values for keys matching common secret
patterns (api_key, token, password, secret, etc.) are redacted before being
written to ~/.config/circuitry/last-run.json, and `--last` will refuse to
replay redacted runs.
"""


@app.command(
    "run",
    help="Execute an orchestration.",
    epilog=RUN_EPILOG,
)
def run_cmd(
    orchestration: str | None = typer.Argument(
        None,
        help="Path to orchestration file, or name of a bundled orchestration.",
    ),
    config: Path | None = typer.Option(
        None, "--config", "-c", help="Path to config JSON (or use CIRCUITRY_CONFIG)."
    ),
    state: Path | None = typer.Option(
        None, "--state", "-s", help="Optional input state JSON."
    ),
    out: Path | None = typer.Option(
        None, "--out", "-o", help="Write resulting state JSON to this path."
    ),
    pretty: bool = typer.Option(
        False, "--pretty", help="Pretty-print JSON when writing/printing."
    ),
    print_state: bool = typer.Option(
        False, "--print", help="Print resulting state JSON to stdout."
    ),
    dry_run: bool = typer.Option(
        False, "--dry-run", help="Skip adapter calls; validate/plan only."
    ),
    json_out: bool = typer.Option(
        False, "--json", help="Machine-readable output only (minimal logs)."
    ),
    quiet: bool = typer.Option(False, "--quiet", help="Suppress non-essential output."),
    verbose: bool = typer.Option(False, "--verbose", "-v", help="Show detailed progress."),
    live_state: Path | None = typer.Option(
        None, "--live-state",
        help="Mirror state atomically to this file while the run goes (at most every "
        "0.5 s, and once more when it ends). For live monitoring.",
    ),
    env_vars: list[str] | None = typer.Option(
        None, "-e",
        help="Inline state variable (KEY=VALUE). Repeatable.",
    ),
    tail: bool = typer.Option(
        False, "--tail",
        help="Print only the final effect's value as plain text. Ideal for piping.",
    ),
    last: bool = typer.Option(
        False, "--last",
        help="Re-run the most recent orchestration with the same arguments.",
    ),
    resume: str | None = typer.Option(
        None, "--resume",
        help=(
            "Resume an interrupted or failed run of the SAME orchestration "
            "from its saved state: 'last' (the most recent run's --out) or "
            "a run-id (looked up in the orchestration's configured "
            "runtime.persistence backend). Effects whose node already "
            "finished without error are skipped and reused; the first "
            "unfinished or failed one, and everything after it, reruns. A "
            "named loop resumes at its first unfinished pass. Combine with "
            "--state <file> to resume from a specific saved state file "
            "directly instead of 'last'/a run-id."
        ),
    ),
    force: bool = typer.Option(
        False, "--force",
        help=(
            "With --resume, continue even though the orchestration's "
            "content changed since the saved run."
        ),
    ),
    skip_preflight: bool = typer.Option(
        False, "--skip-preflight",
        help="Bypass dependency preflight; run even if check()s reported missing deps.",
    ),
    allow_capabilities: str | None = typer.Option(
        None, "--allow-capabilities",
        help=(
            "Comma-separated capabilities (shell,python_eval,fs-write,network) to "
            "approve for this run without prompting — a scripted/CI escape hatch for "
            "a use: ref: child that needs fresh consent. Not persisted; see `cof trust` "
            "to approve a document permanently."
        ),
    ),
    profile: str | None = typer.Option(
        None, "--profile",
        help=(
            "Named profile to apply (profiles/<name>.yml, orchestration-scoped "
            "wins over project-level). Precedence: CLI > profile > orchestration > config."
        ),
    ),
    profile_from_state: Path | None = typer.Option(
        None, "--profile-from-state",
        help=(
            "Reconstruct and apply the profile recorded at "
            "runtime.effective_settings.profile in this state JSON (e.g. a "
            "prior --out), instead of discovering profiles/<name>.yml by "
            "name. Fails if that record was redacted. Mutually exclusive "
            "with --profile."
        ),
    ),
    adapter: str | None = typer.Option(
        None, "--adapter",
        help="Adapter to use for this run. Beats CIRCUITRY_ADAPTER, --profile, and the orchestration.",
    ),
    model: str | None = typer.Option(
        None, "--model",
        help="Model to use for this run. Beats CIRCUITRY_MODEL, --profile, and the orchestration.",
    ),
    explain_routing: bool = typer.Option(
        False, "--explain-routing",
        help=(
            "Print each prompt effect's complexity score, band, and model "
            "choice as it dispatches. Needs runtime.complexity.scoring.enabled; "
            "prints nothing if scoring is off. Suppressed by --quiet/--json."
        ),
    ),
    decompose_out: Path | None = typer.Option(
        None, "--decompose-out",
        help=(
            "Write every triggered decomposition's generated orchestration to "
            "this directory, one file per effect (failed plans included, "
            "marked as such). Needs runtime.complexity.decomposition.enabled; "
            "a run that never decomposes writes nothing."
        ),
    ),
    scoring: bool | None = typer.Option(
        None, "--scoring/--no-scoring",
        help=(
            "Force runtime.complexity.scoring on/off for this run. Beats "
            "--profile and the orchestration/config; omit to leave it resolved "
            "as configured."
        ),
    ),
    routing: bool | None = typer.Option(
        None, "--routing/--no-routing",
        help=(
            "Force runtime.complexity.routing on/off for this run. Beats "
            "--profile and the orchestration/config; --no-routing also drops "
            "any profile per-effect routing pin, since routing is off for the "
            "whole run either way. Requires scoring (from this flag or "
            "config) when turned on."
        ),
    ),
    decompose: bool | None = typer.Option(
        None, "--decompose/--no-decompose",
        help=(
            "Force runtime.complexity.decomposition on/off for this run. "
            "Beats --profile and the orchestration/config. Requires scoring "
            "(from this flag or config) when turned on."
        ),
    ),
    no_cache: bool = typer.Option(
        False, "--no-cache",
        help=(
            "Neither read nor write the per-step cache for `cache:` effects "
            "this run — every such effect dispatches as if it had none."
        ),
    ),
):
    if resume and last:
        console.print("[red]Error:[/red] --resume and --last are mutually exclusive.")
        raise typer.Exit(code=1)

    # --last: replay stashed args
    stashed_trust: bool | None = None
    stashed_remote_library_source: bool | None = None
    stashed_service_profile: str | None = None
    if last:
        stashed = _load_last_run()
        orchestration = stashed["orchestration"]
        # The stash holds the resolved file even for a library name, so
        # whether the original run named a file (trust_document) or came
        # from a remote library source (remote_library_source) comes from
        # the stash too — re-resolving the stashed, already-local path would
        # always say neither.
        stashed_trust = stashed.get("trust_document") is True
        stashed_remote_library_source = stashed.get("remote_library_source") is True
        # A run-library run stashed with `--service-profile` applied its
        # adapter/model/runtime/plugin overrides on top of `cfg` before
        # fetching and running — replaying via plain `cof run --last`
        # otherwise rebuilds `cfg` from `config` alone and silently drops
        # every one of them (#265 part 2 follow-up).
        stashed_service_profile = stashed.get("service_profile")
        config = Path(stashed["config"]) if stashed.get("config") else None
        state = Path(stashed["state"]) if stashed.get("state") else None
        out = Path(stashed["out"]) if stashed.get("out") else None
        pretty = stashed.get("pretty", False)
        print_state = stashed.get("print_state", False)
        dry_run = stashed.get("dry_run", False)
        json_out = stashed.get("json_out", False)
        quiet = stashed.get("quiet", False)
        verbose = stashed.get("verbose", False)
        live_state = Path(stashed["live_state"]) if stashed.get("live_state") else None
        env_vars = stashed.get("env_vars")
        tail = stashed.get("tail", False)
        skip_preflight = stashed.get("skip_preflight", False)
        allow_capabilities = stashed.get("allow_capabilities")
        profile = stashed.get("profile")
        profile_from_state = (
            Path(stashed["profile_from_state"]) if stashed.get("profile_from_state") else None
        )
        adapter = stashed.get("adapter")
        model = stashed.get("model")
        explain_routing = stashed.get("explain_routing", False)
        decompose_out = Path(stashed["decompose_out"]) if stashed.get("decompose_out") else None
        scoring = stashed.get("scoring")
        routing = stashed.get("routing")
        decompose = stashed.get("decompose")
        # Unlike the other replayed flags above, an explicit --no-cache on
        # this invocation is never overridden by the stash: the cost of
        # honoring it is at most a cache miss, but silently dropping it
        # would turn "bypass the cache" into a cache hit the user
        # explicitly asked not to get (#270 review).
        no_cache = no_cache or stashed.get("no_cache", False)

        # Refuse to replay if the previous run stashed redacted secrets — the
        # sentinel string would silently flow into the new run as a literal.
        if env_vars and any(
            isinstance(pair, str) and pair.endswith(f"={REDACTED}") for pair in env_vars
        ):
            console.print(
                "[red]Error:[/red] previous run included redacted secrets in -e values."
            )
            console.print(
                "[dim]Re-run explicitly with the secret supplied via env var or"
                " config file (recommended), or via -e for this invocation.[/dim]"
            )
            raise typer.Exit(code=1)

    configure_cli_logging(verbose=verbose)

    if orchestration is None:
        console.print("[red]Error:[/red] Missing orchestration. Use --last or provide a path/name.")
        console.print("[dim]Tip: run [bold]cof list[/bold] to see available orchestrations.[/dim]")
        raise typer.Exit(code=1)

    # Resolved once, up front, so a project config the run never reaches —
    # for example one skipped file that defined runtime.library sources —
    # can still explain an "Orchestration not found".
    cfg = resolve_config(explicit_path=config)

    if stashed_service_profile:
        try:
            svc_profile = resolve_service_profile(
                cfg=cfg, profile_name=stashed_service_profile
            )
            cfg = apply_service_profile(cfg=cfg, profile=svc_profile)
        except Exception as exc:
            console.print(
                f"[red]Error:[/red] Could not reapply service profile "
                f"{stashed_service_profile!r}: {exc}"
            )
            raise typer.Exit(code=1) from exc

    # A file named by path is trusted with its whole runtime:/plugins:; a
    # library name resolves to someone else's document and stays limited.
    trust_document = (
        stashed_trust if stashed_trust is not None else _names_a_file(orchestration)
    )

    # Resolve orchestration: local file path > library source name
    run_registry = _library_registry(config, cfg=cfg)
    orch_path = _resolve_orchestration(
        orchestration,
        registry=run_registry,
        on_warning=_warn,
    )
    if orch_path is None:
        console.print(f"[red]Error:[/red] Orchestration not found: {orchestration}")
        _print_source_notices(run_registry)
        _print_run_warnings(cfg.resolution_warnings())
        console.print("[dim]Tip: run [bold]cof list[/bold] to see available orchestrations.[/dim]")
        raise typer.Exit(code=1)
    remote_library_source = (
        stashed_remote_library_source
        if stashed_remote_library_source is not None
        else _is_remote_library_source(orchestration, run_registry)
    )

    # Auto-pipe detection (before mutual exclusivity check so --tail wins in pipes)
    if not sys.stdout.isatty() and not tail:
        json_out = True
        quiet = True

    # Mutual exclusivity: --tail vs --print/--json
    if tail and (print_state or json_out):
        console.print("[red]Error:[/red] --tail is mutually exclusive with --print and --json.")
        raise typer.Exit(code=1)

    resumed_state: dict[str, Any] | None = None
    resume_replay_env_vars: list[str] = []
    resume_default_out: Path | None = None
    if resume:
        try:
            resumed_state, resume_replay_env_vars, resume_default_out = _resolve_resume_state(
                resume=resume,
                force=force,
                state_arg=state,
                orch_path=orch_path,
                env_vars=env_vars,
                cfg=cfg,
                trust_document=trust_document,
            )
        except typer.BadParameter as exc:
            console.print(f"[red]Error:[/red] {exc}")
            raise typer.Exit(code=1) from exc

    if profile and profile_from_state:
        console.print("[red]Error:[/red] --profile and --profile-from-state are mutually exclusive.")
        raise typer.Exit(code=1)

    profile_record: dict[str, Any] | None = None
    if profile_from_state:
        try:
            recorded_state = json.loads(profile_from_state.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as exc:
            console.print(f"[red]Error:[/red] Could not read {profile_from_state}: {exc}")
            raise typer.Exit(code=1) from exc
        profile_record = (
            recorded_state.get("runtime", {}).get("effective_settings", {}).get("profile")
            if isinstance(recorded_state, dict)
            else None
        )
        if not isinstance(profile_record, dict):
            console.print(
                f"[red]Error:[/red] {profile_from_state} carries no "
                "runtime.effective_settings.profile record — that run did "
                "not use --profile, so there is nothing to reconstruct."
            )
            raise typer.Exit(code=1)

    if not (quiet or json_out):
        _print_header("Circuitry · Run")
        console.print(f"[bold]Config:[/bold] {describe_config_sources(cfg.sources)}")
        orch_label = orchestration if str(orch_path) == orchestration else f"{orchestration} ({orch_path})"
        console.print(f"[bold]Orchestration:[/bold] {orch_label}")
        console.print(f"[bold]State (in):[/bold] {state or '—'}")
        console.print(f"[bold]State (out):[/bold] {out or '—'}")
        if live_state:
            console.print(f"[bold]Live state:[/bold] {live_state}")
        if decompose_out:
            console.print(f"[bold]Decompose out:[/bold] {decompose_out}")
        if adapter:
            console.print(f"[bold]Adapter (override):[/bold] {adapter}")
        if model:
            console.print(f"[bold]Model (override):[/bold] {model}")
        for label, value in (("Scoring", scoring), ("Routing", routing), ("Decomposition", decompose)):
            if value is not None:
                console.print(f"[bold]{label} (override):[/bold] {'on' if value else 'off'}")
        console.print(f"[bold]Dry run:[/bold] {dry_run}")
        if resume:
            console.print(f"[bold]Resume:[/bold] {resume}{' (--force)' if force else ''}")

    # Build initial state from --state file + -e overrides, or (--resume)
    # from the resolved resume source + -e overrides (the stashed env_vars,
    # for --resume last, when this invocation didn't pass its own).
    initial_state: dict[str, Any] | None = None
    if resume:
        assert resumed_state is not None
        effective_env_vars = env_vars or resume_replay_env_vars or None
        inline = _parse_env_vars(effective_env_vars)
        if inline:
            _restore_raw_text_for_string_inputs(
                inline, _raw_env_var_text(effective_env_vars), orch_path
            )
            initial_state = apply_inline_overrides(resumed_state, inline)
        else:
            initial_state = resumed_state
    else:
        inline = _parse_env_vars(env_vars)
        if inline:
            _restore_raw_text_for_string_inputs(inline, _raw_env_var_text(env_vars), orch_path)
            if state:
                try:
                    initial_state = _read_state_file(state)
                except FileNotFoundError as exc:
                    _print_missing_state_file_error(exc, json_out=json_out)
                    raise typer.Exit(code=1) from exc
                initial_state = apply_inline_overrides(initial_state, inline)
            else:
                initial_state = inline

    # --explain-routing prints its own line per prompt effect as it dispatches
    # (see cli.explain_routing); --quiet and --json both mean "no prose on
    # stdout", so either suppresses it same as the header/status text above.
    effect_start_observer = (
        make_explain_routing_observer(console.print)
        if explain_routing and not (quiet or json_out)
        else None
    )

    show_status = not (quiet or json_out or verbose)
    status_holder: dict[str, Any] = {}

    req = RunRequest(
        orchestration_path=orch_path,
        state_path=state if initial_state is None else None,
        initial_state=initial_state,
        out_path=out,
        dry_run=dry_run,
        validate_only=False,
        verbose=verbose,
        show_loop_progress=_show_loop_progress(verbose=verbose, quiet=quiet, json_out=json_out),
        config=cfg,
        live_state_path=live_state,
        skip_preflight=skip_preflight,
        profile_name=profile,
        profile_record=profile_record,
        adapter_override=adapter,
        model_override=model,
        scoring_override=scoring,
        routing_override=routing,
        decompose_override=decompose,
        effect_start_observer=effect_start_observer,
        decompose_out=decompose_out,
        trust_document=trust_document,
        remote_library_source=remote_library_source,
        allow_capabilities=_parse_allow_capabilities(allow_capabilities),
        capability_prompt=(
            _status_pausing_prompt(
                _capability_prompt(quiet=quiet, json_out=json_out), status_holder
            )
            if show_status
            else _capability_prompt(quiet=quiet, json_out=json_out)
        ),
        resume=bool(resume),
        resume_default_out=resume_default_out,
        no_cache=no_cache,
    )

    # A capability-consent prompt (#275), when one actually fires, pauses this
    # status spinner for the duration of the y/N ask (_status_pausing_prompt)
    # rather than this run losing the spinner outright on the mere chance one
    # might — most interactive runs never need to ask at all.
    status_cm = console.status("[cyan]Running…[/cyan]") if show_status else nullcontext()
    # SIGTERM for the duration of the run only (#338) — same resumable
    # cleanup path Ctrl-C/SIGINT already take, see `cli.interrupts`.
    with status_cm, sigterm_as_interrupt():
        if show_status:
            status_holder["status"] = status_cm
        result = run(req)
    _print_run_warnings(result.warnings)

    # Resolved --out path: the CLI flag if given, else the profile's `out:`
    # (precedence cli > profile > default — see cli.effective_settings).
    resolved_out = result.out_path

    # Write --out for both success and failure (failure state still contains runtime metadata).
    if resolved_out:
        _write_state_json(out=resolved_out, state=result.state, pretty=pretty)

    # Stash for --last/--resume last (success or failure, skip if replaying
    # via --last itself) — a failed run must leave a stash too, or
    # `--resume last` would have no record to find its --out through (#270).
    # Env-var values for credential-shaped keys are redacted before disk write
    # — see RUN_EPILOG and circuitry.cli.redaction.
    if not last:
        _save_last_run({
            "orchestration": str(orch_path),
            "config": str(config) if config else None,
            "state": str(state) if state else None,
            # The raw --out flag, not the resolved path — a profile's own
            # `out:` is deliberately NOT frozen here, so a later edit to the
            # profile is still honoured on replay (see
            # test_run_out_profile_precedence.py). `resume_default_out` is
            # the one exception: the file this *resumed* run saved its own
            # progress back to by default (no --out, no profile `out:`) has
            # no profile to re-resolve on replay, and must round-trip
            # through a later `--resume last` the same way an explicit
            # --out would (#270 F10) — otherwise that resume sees "did not
            # use --out" for a run that plainly did write one.
            "out": str(out) if out else (str(resume_default_out) if resume_default_out else None),
            "pretty": pretty,
            "print_state": print_state,
            "dry_run": dry_run,
            "json_out": json_out,
            "quiet": quiet,
            "verbose": verbose,
            "live_state": str(live_state) if live_state else None,
            "env_vars": redact_env_pairs(env_vars),
            "tail": tail,
            "skip_preflight": skip_preflight,
            "allow_capabilities": allow_capabilities,
            "profile": profile,
            "profile_from_state": str(profile_from_state) if profile_from_state else None,
            "adapter": adapter,
            "model": model,
            "explain_routing": explain_routing,
            "decompose_out": str(decompose_out) if decompose_out else None,
            "scoring": scoring,
            "routing": routing,
            "decompose": decompose,
            "no_cache": no_cache,
            "trust_document": trust_document,
            "remote_library_source": remote_library_source,
        })

    if not result.ok:
        if json_out:
            payload = {
                "ok": False,
                "error": result.error,
                "warnings": result.warnings,
                "state_out": str(resolved_out) if resolved_out else None,
            }
            console.print_json(json.dumps(payload))
        else:
            console.print(
                "[red]Run interrupted[/red]" if result.interrupted else "[red]Run failed[/red]"
            )
            console.print(f"[red]Error:[/red] {result.error}")
            if resolved_out:
                console.print(f"[bold]State written:[/bold] {resolved_out}")
            totals_line = _format_run_totals_line(
                result.state.get("runtime", {}).get("last_run", {}).get("totals")
            )
            if totals_line:
                console.print(f"[bold]Totals:[/bold] {totals_line}")
        # 130 (128 + SIGINT) is the conventional exit code for Ctrl-C —
        # distinct from an ordinary failure's 1, even though both wrote the
        # same --out/--last record above and are equally resumable (#270 F6).
        # SIGTERM gets its own, 143 (128 + SIGTERM), for the same reason
        # (#338).
        raise typer.Exit(code=143 if result.sigterm else 130 if result.interrupted else 1)

    if tail:
        val = _find_last_effect_value(result.state)
        if val is not None:
            print(val if isinstance(val, str) else json.dumps(val))
    elif not (quiet or json_out):
        console.print("[green]Run succeeded[/green]")
        if resolved_out:
            console.print(f"[bold]State written:[/bold] {resolved_out}")
        totals_line = _format_run_totals_line(
            result.state.get("runtime", {}).get("last_run", {}).get("totals")
        )
        if totals_line:
            console.print(f"[bold]Totals:[/bold] {totals_line}")

    # Print --print (or default print for --json with no --out)
    if not tail and (print_state or (not resolved_out and json_out)):
        console.print_json(dumps_saved_state(result.state, pretty=pretty))


@app.command("fetch", help="Fetch a shared library orchestration.")
def fetch_cmd(
    asset_id: str = typer.Argument(..., help="Shared library asset identifier."),
    version: str | None = typer.Option(
        None, "--version", "-V", help="Specific asset version. Defaults to latest."
    ),
    out: Path = typer.Option(
        ...,
        "--out",
        "-o",
        help="Output path for fetched orchestration.",
    ),
    config: Path | None = typer.Option(
        None, "--config", "-c", help="Path to config JSON (or use CIRCUITRY_CONFIG)."
    ),
    auth_token: str | None = typer.Option(
        None,
        "--auth-token",
        help="Shared library auth token (or set CIRCUITRY_LIBRARY_TOKEN).",
    ),
    json_out: bool = typer.Option(
        False, "--json", help="Machine-readable output only (minimal logs)."
    ),
):
    cfg = resolve_config(explicit_path=config)
    token = auth_token or os.getenv("CIRCUITRY_LIBRARY_TOKEN")

    try:
        asset = fetch_shared_orchestration(
            cfg=cfg,
            asset_id=asset_id,
            version=version,
            auth_token=token,
        )
        out.parent.mkdir(parents=True, exist_ok=True)
        out.write_text(asset.file_path.read_text(encoding="utf-8"), encoding="utf-8")
    except Exception as e:
        if json_out:
            console.print_json(json.dumps({"ok": False, "error": str(e)}))
        else:
            console.print(f"[red]Fetch failed:[/red] {e}")
        raise typer.Exit(code=1) from e

    payload = {"ok": True, "asset": asset.metadata, "out_path": str(out)}
    if json_out:
        console.print_json(json.dumps(payload))
        return
    console.print("[green]Fetch succeeded[/green]")
    console.print(f"[bold]Asset:[/bold] {asset.asset_id}@{asset.version}")
    console.print(f"[bold]Written:[/bold] {out}")


@app.command("run-library", help="Fetch and run a shared library orchestration.")
def run_library_cmd(
    asset_id: str = typer.Argument(..., help="Shared library asset identifier."),
    version: str | None = typer.Option(
        None, "--version", "-V", help="Specific asset version. Defaults to latest."
    ),
    config: Path | None = typer.Option(
        None, "--config", "-c", help="Path to config JSON (or use CIRCUITRY_CONFIG)."
    ),
    auth_token: str | None = typer.Option(
        None,
        "--auth-token",
        help="Shared library auth token (or set CIRCUITRY_LIBRARY_TOKEN).",
    ),
    service_profile: str | None = typer.Option(
        None,
        "--service-profile",
        help="Apply runtime overrides from runtime.library.service_profiles.<name>.",
    ),
    state: Path | None = typer.Option(
        None, "--state", "-s", help="Optional input state JSON."
    ),
    out: Path | None = typer.Option(
        None, "--out", "-o", help="Write resulting state JSON to this path."
    ),
    pretty: bool = typer.Option(
        False, "--pretty", help="Pretty-print JSON when writing/printing."
    ),
    print_state: bool = typer.Option(
        False, "--print", help="Print resulting state JSON to stdout."
    ),
    dry_run: bool = typer.Option(
        False, "--dry-run", help="Skip adapter calls; validate/plan only."
    ),
    json_out: bool = typer.Option(
        False, "--json", help="Machine-readable output only (minimal logs)."
    ),
    quiet: bool = typer.Option(False, "--quiet", help="Suppress non-essential output."),
    verbose: bool = typer.Option(False, "--verbose", "-v", help="Show detailed progress."),
    live_state: Path | None = typer.Option(
        None, "--live-state",
        help="Mirror state atomically to this file while the run goes (at most every "
        "0.5 s, and once more when it ends). For live monitoring.",
    ),
    env_vars: list[str] | None = typer.Option(
        None, "-e",
        help="Inline state variable (KEY=VALUE). Repeatable.",
    ),
    tail: bool = typer.Option(
        False, "--tail",
        help="Print only the final effect's value as plain text. Ideal for piping.",
    ),
    skip_preflight: bool = typer.Option(
        False, "--skip-preflight",
        help="Bypass dependency preflight; run even if check()s reported missing deps.",
    ),
    allow_capabilities: str | None = typer.Option(
        None, "--allow-capabilities",
        help=(
            "Comma-separated capabilities (shell,python_eval,fs-write,network) to "
            "approve for this run without prompting — a scripted/CI escape hatch. "
            "Not persisted; see `cof trust` to approve a document permanently."
        ),
    ),
    profile: str | None = typer.Option(
        None, "--profile",
        help=(
            "Named profile to apply (profiles/<name>.yml, orchestration-scoped "
            "wins over project-level). Precedence: CLI > profile > orchestration > config."
        ),
    ),
    adapter: str | None = typer.Option(
        None, "--adapter",
        help="Adapter to use for this run. Beats CIRCUITRY_ADAPTER and the orchestration.",
    ),
    model: str | None = typer.Option(
        None, "--model",
        help="Model to use for this run. Beats CIRCUITRY_MODEL and the orchestration.",
    ),
    explain_routing: bool = typer.Option(
        False, "--explain-routing",
        help=(
            "Print each prompt effect's complexity score, band, and model "
            "choice as it dispatches. Needs runtime.complexity.scoring.enabled; "
            "prints nothing if scoring is off. Suppressed by --quiet/--json."
        ),
    ),
    scoring: bool | None = typer.Option(
        None, "--scoring/--no-scoring",
        help=(
            "Force runtime.complexity.scoring on/off for this run. Beats "
            "--profile and the orchestration/config; omit to leave it resolved "
            "as configured."
        ),
    ),
    routing: bool | None = typer.Option(
        None, "--routing/--no-routing",
        help=(
            "Force runtime.complexity.routing on/off for this run. Beats "
            "--profile and the orchestration/config; --no-routing also drops "
            "any profile per-effect routing pin, since routing is off for the "
            "whole run either way. Requires scoring (from this flag or "
            "config) when turned on."
        ),
    ),
    decompose: bool | None = typer.Option(
        None, "--decompose/--no-decompose",
        help=(
            "Force runtime.complexity.decomposition on/off for this run. "
            "Beats --profile and the orchestration/config. Requires scoring "
            "(from this flag or config) when turned on."
        ),
    ),
):
    configure_cli_logging(verbose=verbose)

    # Auto-pipe detection (before mutual exclusivity check so --tail wins in
    # pipes) — exactly as `cof run` does, so `run-library ... --tail | cat`
    # works the same way `run ... --tail | cat` does (#265 part 2).
    if not sys.stdout.isatty() and not tail:
        json_out = True
        quiet = True

    if tail and (print_state or json_out):
        console.print("[red]Error:[/red] --tail is mutually exclusive with --print and --json.")
        raise typer.Exit(code=1)

    cfg = resolve_config(explicit_path=config)
    token = auth_token or os.getenv("CIRCUITRY_LIBRARY_TOKEN")

    try:
        svc_profile = resolve_service_profile(cfg=cfg, profile_name=service_profile)
        effective_cfg = apply_service_profile(cfg=cfg, profile=svc_profile)
        asset = fetch_shared_orchestration(
            cfg=effective_cfg,
            asset_id=asset_id,
            version=version,
            auth_token=token,
        )
        if svc_profile is not None:
            asset.metadata["service_profile"] = svc_profile.name
    except Exception as e:
        _print_run_warnings(cfg.resolution_warnings())
        if json_out:
            console.print_json(json.dumps({"ok": False, "error": str(e)}))
        else:
            console.print(f"[red]Shared library retrieval failed:[/red] {e}")
        raise typer.Exit(code=1) from e

    if not (quiet or json_out):
        _print_header("Circuitry · Run Library")
        console.print(f"[bold]Asset:[/bold] {asset.asset_id}@{asset.version}")
        console.print(f"[bold]Source:[/bold] {asset.source}")
        console.print(f"[bold]Resolved path:[/bold] {asset.file_path}")
        console.print(f"[bold]Config:[/bold] {describe_config_sources(cfg.sources)}")
        console.print(
            f"[bold]Service profile:[/bold] {service_profile or '—'}"
        )
        console.print(f"[bold]State (in):[/bold] {state or '—'}")
        console.print(f"[bold]State (out):[/bold] {out or '—'}")
        if live_state:
            console.print(f"[bold]Live state:[/bold] {live_state}")
        if adapter:
            console.print(f"[bold]Adapter (override):[/bold] {adapter}")
        if model:
            console.print(f"[bold]Model (override):[/bold] {model}")
        for label, value in (
            ("Scoring", scoring), ("Routing", routing), ("Decomposition", decompose),
        ):
            if value is not None:
                console.print(f"[bold]{label} (override):[/bold] {'on' if value else 'off'}")
        console.print(f"[bold]Dry run:[/bold] {dry_run}")

    # Build initial state from --state file + -e overrides
    initial_state: dict[str, Any] | None = None
    inline = _parse_env_vars(env_vars)
    if inline:
        _restore_raw_text_for_string_inputs(inline, _raw_env_var_text(env_vars), asset.file_path)
        if state:
            try:
                initial_state = _read_state_file(state)
            except FileNotFoundError as exc:
                _print_missing_state_file_error(exc, json_out=json_out)
                raise typer.Exit(code=1) from exc
            initial_state = apply_inline_overrides(initial_state, inline)
        else:
            initial_state = inline

    effect_start_observer = (
        make_explain_routing_observer(console.print)
        if explain_routing and not (quiet or json_out)
        else None
    )

    show_status = not (quiet or json_out or verbose)
    status_holder: dict[str, Any] = {}

    req = RunRequest(
        orchestration_path=asset.file_path,
        state_path=state if initial_state is None else None,
        initial_state=initial_state,
        out_path=out,
        dry_run=dry_run,
        validate_only=False,
        shared_library_metadata=asset.metadata,
        verbose=verbose,
        show_loop_progress=_show_loop_progress(verbose=verbose, quiet=quiet, json_out=json_out),
        config=effective_cfg,
        live_state_path=live_state,
        skip_preflight=skip_preflight,
        profile_name=profile,
        adapter_override=adapter,
        model_override=model,
        scoring_override=scoring,
        routing_override=routing,
        decompose_override=decompose,
        effect_start_observer=effect_start_observer,
        allow_capabilities=_parse_allow_capabilities(allow_capabilities),
        capability_prompt=(
            _status_pausing_prompt(
                _capability_prompt(quiet=quiet, json_out=json_out), status_holder
            )
            if show_status
            else _capability_prompt(quiet=quiet, json_out=json_out)
        ),
    )

    status_cm = console.status("[cyan]Running…[/cyan]") if show_status else nullcontext()
    # SIGTERM for the duration of the run only (#338) — same resumable
    # cleanup path Ctrl-C/SIGINT already take, see `cli.interrupts`.
    with status_cm, sigterm_as_interrupt():
        if show_status:
            status_holder["status"] = status_cm
        result = run(req)
    _print_run_warnings(result.warnings)

    # Resolved --out path: the CLI flag if given, else the profile's `out:`
    # (precedence cli > profile > default), the same as `cof run` — a
    # `--profile` here used to silently skip writing the state file when it
    # set `out:` and `--out` wasn't also given (#265 part 2).
    resolved_out = result.out_path

    if resolved_out:
        _write_state_json(out=resolved_out, state=result.state, pretty=pretty)

    if not result.ok:
        if json_out:
            payload = {
                "ok": False,
                "error": result.error,
                "warnings": result.warnings,
                "state_out": str(resolved_out) if resolved_out else None,
            }
            console.print_json(json.dumps(payload))
        else:
            console.print(
                "[red]Run interrupted[/red]" if result.interrupted else "[red]Run failed[/red]"
            )
            console.print(f"[red]Error:[/red] {result.error}")
            if resolved_out:
                console.print(f"[bold]State written:[/bold] {resolved_out}")
            totals_line = _format_run_totals_line(
                result.state.get("runtime", {}).get("last_run", {}).get("totals")
            )
            if totals_line:
                console.print(f"[bold]Totals:[/bold] {totals_line}")
        raise typer.Exit(code=143 if result.sigterm else 130 if result.interrupted else 1)

    # Stash for --last, the same shape `cof run` writes — so `cof run --last`
    # can replay a run-library run too. The resolved asset file (not the
    # asset id) is what the stash reruns; a library asset is never a trusted
    # document (#265 part 2).
    _save_last_run({
        "orchestration": str(asset.file_path),
        "config": str(config) if config else None,
        "state": str(state) if state else None,
        "out": str(out) if out else None,
        "pretty": pretty,
        "print_state": print_state,
        "dry_run": dry_run,
        "json_out": json_out,
        "quiet": quiet,
        "verbose": verbose,
        "live_state": str(live_state) if live_state else None,
        "env_vars": redact_env_pairs(env_vars),
        "tail": tail,
        "skip_preflight": skip_preflight,
        "profile": profile,
        "profile_from_state": None,
        "adapter": adapter,
        "model": model,
        "explain_routing": explain_routing,
        "decompose_out": None,
        "scoring": scoring,
        "routing": routing,
        "decompose": decompose,
        "trust_document": False,
        # So `cof run --last` can reapply the same adapter/model/runtime/
        # plugin overrides this run used — otherwise a replayed run-library
        # run silently loses them (#265 part 2 follow-up).
        "service_profile": svc_profile.name if svc_profile is not None else None,
    })

    if tail:
        val = _find_last_effect_value(result.state)
        if val is not None:
            print(val if isinstance(val, str) else json.dumps(val))
    elif not (quiet or json_out):
        console.print("[green]Run succeeded[/green]")
        if out:
            console.print(f"[bold]State written:[/bold] {out}")
        totals_line = _format_run_totals_line(
            result.state.get("runtime", {}).get("last_run", {}).get("totals")
        )
        if totals_line:
            console.print(f"[bold]Totals:[/bold] {totals_line}")

    if not tail and (print_state or (not out and json_out)):
        console.print_json(dumps_saved_state(result.state, pretty=pretty))


def _curation_category_names() -> str:
    """Real `category:` values in the bundled curation index, so the
    `--category` help text can't drift from them again (#265 part 5) — it
    used to name five categories that matched none of the real ones."""
    names = sorted({e["category"] for e in load_index() if e.get("category")})
    return ", ".join(names)


@app.command(
    "list",
    help=(
        "List available bundled orchestrations (compiled-in extensions with "
        "--extensions, an adapter's models with --models <adapter>)."
    ),
)
def list_cmd(
    category: str | None = typer.Option(
        None, "--category", "-C",
        help=f"Filter by category ({_curation_category_names()}).",
    ),
    json_out: bool = typer.Option(
        False, "--json", help="Output machine-readable JSON only."
    ),
    extensions: bool = typer.Option(
        False,
        "--extensions",
        "-x",
        help="List compiled-in adapters / tool plugins / runtime plugins with allowlist status.",
    ),
    models: str | None = typer.Option(
        None,
        "--models",
        "-M",
        help="List the models an adapter offers (e.g. --models ollama). Same data the TUI's model picker uses.",
    ),
    source: str | None = typer.Option(
        None, "--source", "-S", help="Only list entries from this library source."
    ),
    config: Path | None = typer.Option(
        None, "--config", "-c", help="Path to config JSON (or use CIRCUITRY_CONFIG)."
    ),
):
    if models is not None:
        _list_models(adapter_name=models, json_out=json_out, config_path=config)
        return

    if extensions:
        _list_extensions(json_out=json_out, config_path=config)
        return

    registry = _library_registry(config)
    if source is not None and registry.get_source(source) is None:
        known = ", ".join(registry.source_names)
        console.print(f"[red]Error:[/red] Unknown library source: {source}")
        console.print(f"[dim]Configured sources: {known}[/dim]")
        raise typer.Exit(code=1)

    show_source = registry.is_multi_source
    library_entries = registry.list_entries(source=source)
    if not json_out:
        # A remote source with an empty cache is *not* an error — it just has
        # nothing to show until `cof library refresh` runs.
        for message in registry.notices(source=source):
            _warn(message)
    if not library_entries:
        console.print("[yellow]No bundled orchestrations found.[/yellow]")
        raise typer.Exit(code=1)

    if category:
        library_entries = [e for e in library_entries if e.category == category]
        if not library_entries:
            console.print(f"[yellow]No orchestrations in category: {category}[/yellow]")
            raise typer.Exit(code=1)

    entries = [e.as_dict(include_source=show_source) for e in library_entries]

    if json_out:
        console.print_json(json.dumps(entries, ensure_ascii=False))
        return

    # Check backend availability from current config
    cfg = resolve_config()
    available_backends = _detect_backends(cfg)

    _print_header("Circuitry · Orchestrations")

    table = Table(show_header=True, header_style="bold cyan")
    table.add_column("Name", style="bold")
    table.add_column("Description")
    table.add_column("Category", style="dim")
    if show_source:
        table.add_column("Source", style="dim")
    table.add_column("Backends", justify="center")

    for entry in library_entries:
        backends = entry.metadata.get("backends", [])
        backend_parts = []
        for b in backends:
            if b in available_backends:
                backend_parts.append(f"[green]{b}[/green]")
            else:
                backend_parts.append(f"[red]{b}[/red]")
        backends_str = " ".join(backend_parts) if backend_parts else "—"

        row = [
            entry.metadata.get("name", "?"),
            entry.metadata.get("description", ""),
            entry.category,
        ]
        if show_source:
            row.append(entry.source)
        row.append(backends_str)
        table.add_row(*row)

    console.print(table)
    console.print()
    console.print("[dim]Run with:[/dim] cof run <name> [dim](e.g.[/dim] cof run hello -e name=World[dim])[/dim]")
    console.print("[dim]Backends: [green]available[/green] [red]not detected[/red][/dim]")


def _list_models(
    *, adapter_name: str, json_out: bool, config_path: Path | None
) -> None:
    """Render the models one adapter offers — CLI parity with the TUI picker.

    Adapters answer optionally (see ``circuitry.adapters.models``): an
    adapter that cannot enumerate, or a backend that is not running,
    reports nothing rather than failing. Only an unknown adapter name is
    an error — that one is a typo, not a missing daemon.
    """
    from ..adapters.factory import ADAPTER_REGISTRY
    from ..adapters.models import list_adapter_models

    name = (adapter_name or "").strip().lower()
    if name not in ADAPTER_REGISTRY:
        known = ", ".join(sorted(ADAPTER_REGISTRY))
        if json_out:
            console.print_json(
                json.dumps({"adapter": name, "error": "unknown adapter", "models": []})
            )
        else:
            console.print(f"[red]Error:[/red] Unknown adapter: {adapter_name}")
            console.print(f"[dim]Known adapters: {known}[/dim]")
        raise typer.Exit(code=1)

    cfg = resolve_config(explicit_path=config_path)
    names = list_adapter_models(adapter_name=name, runtime=cfg.runtime or {})

    if json_out:
        console.print_json(
            json.dumps({"adapter": name, "models": names}, ensure_ascii=False)
        )
        return

    if not names:
        console.print(f"[yellow]No models reported by {name}.[/yellow]")
        console.print(
            "[dim]The adapter may not enumerate models, or its backend may not "
            "be reachable — run `cof doctor`. Any model string still works with "
            "`cof run --model <name>`.[/dim]"
        )
        return

    _print_header(f"Circuitry · Models · {name}")
    table = Table(show_header=True, header_style="bold cyan")
    table.add_column("Model", style="bold")
    for model_name in names:
        table.add_row(model_name)
    console.print(table)
    console.print()
    console.print(f"[dim]Run with:[/dim] cof run <name> --adapter {name} --model <model>")


def _list_extensions(*, json_out: bool, config_path: Path | None) -> None:
    """Render compiled-in adapters / tool plugins / runtime plugins with
    allowlist status."""
    from ..adapters.factory import ADAPTER_REGISTRY
    from ..plugins.factory import PLUGIN_REGISTRY

    cfg = resolve_config(explicit_path=config_path)

    adapters_compiled = sorted(ADAPTER_REGISTRY.keys())
    tools_compiled = sorted(PLUGIN_REGISTRY.keys())
    # Runtime plugins are loaded by dotted path; cfg.plugins lists what
    # this project asks for. Phase 6 will introduce a compiled-in catalog;
    # for now there's no in-tree set, so derive from cfg.plugins.
    runtime_compiled = sorted(set(cfg.plugins or []))

    def status(name: str, allowed: list[str] | None) -> str:
        if allowed is None:
            return "compiled-in (default-open)"
        return "enabled" if name in allowed else "disabled (not in allowlist)"

    if json_out:
        payload = {
            "adapters": [
                {"name": n, "status": status(n, cfg.enabled_adapters)}
                for n in adapters_compiled
            ],
            "tool_plugins": [
                {"name": n, "status": status(n, cfg.enabled_tools)}
                for n in tools_compiled
            ],
            "runtime_plugins": [
                {"name": n, "status": status(n, cfg.enabled_plugins)}
                for n in runtime_compiled
            ],
            "environment": cfg.environment,
        }
        console.print_json(json.dumps(payload, ensure_ascii=False))
        return

    _print_header("Circuitry · Extensions")

    def render_section(title: str, names: list[str], allowed: list[str] | None) -> None:
        table = Table(title=title, show_header=True, header_style="bold cyan")
        table.add_column("Name", style="bold")
        table.add_column("Status")
        if not names:
            table.add_row("[dim](none registered)[/dim]", "[dim]—[/dim]")
        else:
            for n in names:
                s = status(n, allowed)
                style = (
                    "green" if s == "enabled" or s == "compiled-in (default-open)"
                    else "red"
                )
                table.add_row(n, f"[{style}]{s}[/{style}]")
        console.print(table)
        console.print()

    render_section("Adapters", adapters_compiled, cfg.enabled_adapters)
    render_section("Tool plugins", tools_compiled, cfg.enabled_tools)
    render_section("Runtime plugins", runtime_compiled, cfg.enabled_plugins)
    console.print(f"[dim]Environment: {cfg.environment}[/dim]")


def _detect_backends(cfg: CircuitryConfig) -> set[str]:
    """Best-effort detection of which backends are actually reachable."""
    from .detect import detect_all

    ollama_url = cfg.runtime.get("adapters", {}).get("ollama", {}).get("base_url", "http://localhost:11434")
    comfyui_url = cfg.runtime.get("plugins", {}).get("comfyui", {}).get("base_url", "http://localhost:8188")

    result = detect_all(ollama_url=ollama_url, comfyui_url=comfyui_url)
    available = result.available_names

    # Map specific LLM backends to the generic 'llm' tag used in index.yml
    if available & {"ollama", "openai", "anthropic"}:
        available.add("llm")

    return available


def _complexity_field(value: Any, source: str) -> dict[str, Any]:
    return {"value": value, "source": source}


def _complexity_info_dict(
    complexity: ComplexitySettings, sources: dict[str, str]
) -> dict[str, Any]:
    """Machine-readable resolved complexity block, each value annotated with
    the layer (config/orchestration/profile/cli/default) that supplied it —
    see `cli.effective_settings.resolve_effective_settings`."""
    bands = [
        {**band.as_dict(), "catch_all": band.is_catch_all}
        for band in complexity.routing.bands
    ]
    return {
        "scoring": {
            "enabled": _complexity_field(
                complexity.scoring.enabled,
                sources.get("complexity.scoring", "default"),
            ),
        },
        "routing": {
            "enabled": _complexity_field(
                complexity.routing.enabled,
                sources.get("complexity.routing", "default"),
            ),
            "bands": _complexity_field(
                bands, sources.get("complexity.routing.bands", "default")
            ),
        },
        "decomposition": {
            "enabled": _complexity_field(
                complexity.decomposition.enabled,
                sources.get("complexity.decomposition", "default"),
            ),
            "threshold": _complexity_field(
                complexity.decomposition.threshold,
                sources.get("complexity.decomposition.threshold", "default"),
            ),
            "max_depth": _complexity_field(
                complexity.decomposition.max_depth,
                sources.get("complexity.decomposition.max_depth", "default"),
            ),
            "on_failure": _complexity_field(
                complexity.decomposition.on_failure,
                sources.get("complexity.decomposition.on_failure", "default"),
            ),
        },
    }


def _render_complexity_info(
    complexity: ComplexitySettings, sources: dict[str, str]
) -> None:
    """Human-readable counterpart to `_complexity_info_dict` — the three
    switches, the routing band table (ordered, catch-all marked), and the
    decomposition scalars, each with its winning source."""
    console.print()
    switch_table = Table(title="Complexity", show_header=True, header_style="bold cyan")
    switch_table.add_column("Switch", style="bold")
    switch_table.add_column("State", justify="center")
    switch_table.add_column("Source")
    for label, key, enabled in (
        ("Scoring", "complexity.scoring", complexity.scoring.enabled),
        ("Routing", "complexity.routing", complexity.routing.enabled),
        ("Decomposition", "complexity.decomposition", complexity.decomposition.enabled),
    ):
        state = "[green]on[/green]" if enabled else "[dim]off[/dim]"
        switch_table.add_row(label, state, sources.get(key, "default"))
    console.print(switch_table)

    if complexity.routing.bands:
        console.print()
        bands_source = sources.get("complexity.routing.bands", "default")
        band_table = Table(
            title=f"Routing bands (source: {bands_source})",
            show_header=True,
            header_style="bold cyan",
        )
        band_table.add_column("#", justify="right")
        band_table.add_column("Name")
        band_table.add_column("Max", justify="right")
        band_table.add_column("Model")
        for index, band in enumerate(complexity.routing.bands, start=1):
            upper = "[bold]— (catch-all)[/bold]" if band.is_catch_all else f"{band.max:g}"
            band_table.add_row(str(index), band.name or "—", upper, band.model)
        console.print(band_table)

    console.print()
    decomp_table = Table(title="Decomposition", show_header=True, header_style="bold cyan")
    decomp_table.add_column("Setting", style="bold")
    decomp_table.add_column("Value")
    decomp_table.add_column("Source")
    for label, key, value in (
        (
            "Threshold",
            "complexity.decomposition.threshold",
            f"{complexity.decomposition.threshold:g}",
        ),
        (
            "Max depth",
            "complexity.decomposition.max_depth",
            str(complexity.decomposition.max_depth),
        ),
        (
            "On failure",
            "complexity.decomposition.on_failure",
            complexity.decomposition.on_failure,
        ),
    ):
        decomp_table.add_row(label, value, sources.get(key, "default"))
    console.print(decomp_table)


@app.command("info", help="Show details for a bundled orchestration.")
def info_cmd(
    name: str = typer.Argument(..., help="Name of the orchestration."),
    json_out: bool = typer.Option(False, "--json", help="Output machine-readable JSON only."),
    config: Path | None = typer.Option(
        None, "--config", "-c", help="Path to config JSON (or use CIRCUITRY_CONFIG)."
    ),
    profile: str | None = typer.Option(
        None, "--profile",
        help=(
            "Named profile to layer in when resolving the complexity block, "
            "as `cof run --profile` would (see `cof run --help`)."
        ),
    ),
    scoring: bool | None = typer.Option(
        None, "--scoring/--no-scoring",
        help="Preview runtime.complexity.scoring forced on/off, as `cof run --scoring` would resolve it.",
    ),
    routing: bool | None = typer.Option(
        None, "--routing/--no-routing",
        help="Preview runtime.complexity.routing forced on/off, as `cof run --routing` would resolve it.",
    ),
    decompose: bool | None = typer.Option(
        None, "--decompose/--no-decompose",
        help="Preview runtime.complexity.decomposition forced on/off, as `cof run --decompose` would resolve it.",
    ),
):
    registry = _library_registry(config)
    found = _lookup_entry(registry, name)
    if found is None:
        console.print(f"[red]Error:[/red] Orchestration not found: {name}")
        _print_source_notices(registry)
        console.print("[dim]Run [bold]cof list[/bold] to see available orchestrations.[/dim]")
        raise typer.Exit(code=1)

    show_source = registry.is_multi_source
    entry = found.as_dict(include_source=show_source)

    bundled_path = found.path
    orch: dict[str, Any] = {}
    if bundled_path and bundled_path.exists():
        orch = load_orchestration_file(bundled_path)

    cfg = resolve_config(explicit_path=config)
    try:
        profile_settings: ProfileSettings | None = None
        if profile:
            profile_settings = load_profile(
                name=profile,
                orchestration_path=bundled_path or Path.cwd(),
                orch=orch,
            )
        effective = resolve_effective_settings(
            cfg=cfg,
            orch=orch,
            cli_scoring=scoring,
            cli_routing=routing,
            cli_decompose=decompose,
            profile=profile_settings,
        )
    except (ProfileError, ValueError) as exc:
        console.print(f"[red]Error:[/red] {exc}")
        raise typer.Exit(code=1) from exc

    if json_out:
        entry["complexity"] = _complexity_info_dict(effective.complexity, effective.sources)
        console.print_json(json.dumps(entry, ensure_ascii=False))
        return

    _print_header(f"Circuitry · {entry['name']}")

    console.print(f"[bold]Description:[/bold] {entry.get('description', '—')}")
    console.print(f"[bold]Category:[/bold] {entry.get('category', '—')}")
    console.print(f"[bold]File:[/bold] {entry.get('file', '—')}")
    if show_source:
        console.print(f"[bold]Source:[/bold] {found.source}")
    console.print(f"[bold]Backends:[/bold] {', '.join(entry.get('backends', []))}")

    inputs = entry.get("inputs", [])
    if inputs:
        console.print()
        input_table = Table(title="Inputs", show_header=True, header_style="bold cyan")
        input_table.add_column("Name", style="bold")
        input_table.add_column("Required", justify="center")
        input_table.add_column("Description")
        for inp in inputs:
            req = "[green]yes[/green]" if inp.get("required") else "[dim]no[/dim]"
            input_table.add_row(inp.get("name", "?"), req, inp.get("description", ""))
        console.print(input_table)

    example = entry.get("example")
    if example:
        console.print()
        console.print("[bold]Example:[/bold]")
        console.print(f"  [cyan]{example}[/cyan]")

    _render_complexity_info(effective.complexity, effective.sources)

    # Show the actual orchestration YAML source
    if bundled_path and bundled_path.exists():
        console.print()
        source = bundled_path.read_text(encoding="utf-8").strip()
        # Truncate long sources
        lines = source.splitlines()
        if len(lines) > 30:
            preview = "\n".join(lines[:30]) + f"\n# ... ({len(lines) - 30} more lines)"
        else:
            preview = source
        from rich.syntax import Syntax
        console.print(Syntax(preview, "yaml", theme="monokai", line_numbers=False))


@app.command("eject", help="Copy a bundled orchestration to the current directory for editing.")
def eject_cmd(
    name: str = typer.Argument(..., help="Name of the orchestration to eject."),
    out: Path | None = typer.Option(
        None, "--out", "-o", help="Output path. Defaults to ./<filename>."
    ),
    config: Path | None = typer.Option(
        None, "--config", "-c", help="Path to config JSON (or use CIRCUITRY_CONFIG)."
    ),
):
    registry = _library_registry(config)
    found = _lookup_entry(registry, name)
    if found is None:
        console.print(f"[red]Error:[/red] Orchestration not found: {name}")
        console.print("[dim]Run [bold]cof list[/bold] to see available orchestrations.[/dim]")
        raise typer.Exit(code=1)

    entry = found.metadata
    bundled_path = found.path
    if bundled_path is None or not bundled_path.exists():
        console.print(f"[red]Error:[/red] Bundled file not found for: {name}")
        raise typer.Exit(code=1)

    dest = out or eject_destination(entry)
    if dest.exists() and not typer.confirm(
        f"{dest} already exists. Overwrite?", default=False
    ):
        raise typer.Exit(code=0)

    write_ejected(bundled_path.read_text(encoding="utf-8"), dest)
    console.print(f"[green]Ejected:[/green] {dest}")
    console.print(f"[dim]Edit freely — this is your local copy. Run with: cof run {dest}[/dim]")


library_app = typer.Typer(
    add_completion=False,
    no_args_is_help=True,
    help="Manage library sources (`runtime.library.sources`).",
)
app.add_typer(library_app, name="library")


cache_app = typer.Typer(
    add_completion=False,
    no_args_is_help=True,
    help="Manage the per-step result cache (`cache:` on prompt/tool effects, #270).",
)
app.add_typer(cache_app, name="cache")


@cache_app.command("clear", help="Delete every cached step result.")
def cache_clear_cmd(
    json_out: bool = typer.Option(False, "--json", help="Output machine-readable JSON only."),
) -> None:
    removed = StepCache().clear()
    if json_out:
        console.print_json(json.dumps({"removed": removed}))
    else:
        console.print(f"[green]Cleared {removed} cached result(s).[/green]")


@cache_app.command("stats", help="Show cached-entry count and size.")
def cache_stats_cmd(
    json_out: bool = typer.Option(False, "--json", help="Output machine-readable JSON only."),
) -> None:
    stats = StepCache().stats()
    if json_out:
        console.print_json(json.dumps(stats))
    else:
        console.print(f"[bold]Path:[/bold] {stats['path']}")
        console.print(f"[bold]Entries:[/bold] {stats['entries']}")
        console.print(f"[bold]Size:[/bold] {stats['bytes']} bytes")


@library_app.command(
    "refresh",
    help="Fetch remote library sources into the local cache. This is the only "
    "command that touches the network for a library source.",
)
def library_refresh_cmd(
    source: str | None = typer.Argument(
        None, help="Source to refresh. Omit to refresh every configured source."
    ),
    json_out: bool = typer.Option(False, "--json", help="Output machine-readable JSON only."),
    config: Path | None = typer.Option(
        None, "--config", "-c", help="Path to config JSON (or use CIRCUITRY_CONFIG)."
    ),
):
    registry = _library_registry(config)
    if source is not None and registry.get_source(source) is None:
        known = ", ".join(registry.source_names)
        console.print(f"[red]Error:[/red] Unknown library source: {source}")
        console.print(f"[dim]Configured sources: {known}[/dim]")
        raise typer.Exit(code=1)

    if not json_out:
        _print_header("Circuitry · Library refresh")

    results: list[dict[str, Any]] = []
    failed = False
    for candidate in registry.sources:
        if source is not None and candidate.name != source:
            continue
        try:
            outcome = registry.refresh(source=candidate.name)[0]
        except LibraryFetchError as exc:
            failed = True
            results.append({"source": candidate.name, "status": "error", "error": str(exc)})
            if not json_out:
                console.print(f"[red]Error:[/red] {exc}")
            continue
        results.append(
            {
                "source": outcome.source,
                "status": outcome.status,
                "sha": outcome.sha,
                "detail": outcome.detail,
            }
        )
        if not json_out:
            colour = {"updated": "green", "unchanged": "cyan"}.get(outcome.status, "dim")
            console.print(f"[{colour}]{outcome.summary()}[/{colour}]")

    if json_out:
        console.print_json(json.dumps(results, ensure_ascii=False))
    raise typer.Exit(code=1 if failed else 0)


@app.command(
    "validate",
    help="Validate an orchestration file against the schema. (alias: check)",
)
def validate_cmd(
    orchestration: Path = typer.Argument(
        ..., exists=True, dir_okay=False, readable=True,
        help="Path to orchestration file.",
    ),
    json_out: bool = typer.Option(
        False, "--json", help="Output machine-readable JSON only."
    ),
    config: Path | None = typer.Option(
        None, "--config", "-c", help="Path to config JSON (or use CIRCUITRY_CONFIG)."
    ),
    skip_preflight: bool = typer.Option(
        False, "--skip-preflight",
        help="Skip dependency-readiness checks; only verify structure / schema.",
    ),
):
    _do_validate(orchestration, json_out, config=config, skip_preflight=skip_preflight)


@app.command("check", help="Validate an orchestration file against the schema. (alias of `validate`)")
def check_cmd(
    orchestration: Path = typer.Argument(
        ..., exists=True, dir_okay=False, readable=True,
        help="Path to orchestration file.",
    ),
    json_out: bool = typer.Option(
        False, "--json", help="Output machine-readable JSON only."
    ),
    config: Path | None = typer.Option(
        None, "--config", "-c", help="Path to config JSON (or use CIRCUITRY_CONFIG)."
    ),
    skip_preflight: bool = typer.Option(
        False, "--skip-preflight",
        help="Skip dependency-readiness checks; only verify structure / schema.",
    ),
):
    _do_validate(orchestration, json_out, config=config, skip_preflight=skip_preflight)


@app.command("inspect", help="Show orchestration metadata.")
def inspect_cmd(
    orchestration: Path = typer.Argument(
        ..., exists=True, dir_okay=False, readable=True,
        help="Path to orchestration file.",
    ),
):
    _print_header("Circuitry · Inspect")
    with console.status("[cyan]Inspecting…[/cyan]"):
        summary = inspect_orchestration(orchestration)

    table = Table(title="Orchestration Summary")
    table.add_column("Field")
    table.add_column("Value")
    for k, v in summary.items():
        table.add_row(
            str(k),
            json.dumps(v, ensure_ascii=False)
            if isinstance(v, (dict, list))
            else str(v),
        )
    console.print(table)


@app.command(
    "gen",
    help=(
        "Generate an orchestration from a natural language prompt, single-shot. "
        "Drives `agents/meta_orchestrator.yml`. For a multi-turn, clarifying-"
        "questions build instead, see `cof wizard`."
    ),
)
def gen_cmd(
    name: str = typer.Argument(
        ..., help="Name for the generated orchestration (used as filename)."
    ),
    prompt: str = typer.Argument(
        ..., help="Natural language description of the orchestration to generate."
    ),
    out: Path | None = typer.Option(
        None, "--out", "-o",
        help="Write the generated orchestration here (default: ./<name>.<ext>).",
    ),
    live_state: Path | None = typer.Option(
        None, "--live-state",
        help="Mirror the generation run's state JSON to this file while it goes. "
        "For live monitoring — not the generated orchestration.",
    ),
    config: Path | None = typer.Option(
        None, "--config", "-c", help="Path to config JSON."
    ),
    verbose: bool = typer.Option(False, "--verbose", "-v", help="Show detailed progress."),
    output_format: str = typer.Option(
        "yaml", "--format", "-f", help="Output format: yaml, json, or toon.",
    ),
    retries: int = typer.Option(
        3, "--retries", "-r", help="Max retry attempts per prompt on failure.",
    ),
):
    configure_cli_logging(verbose=verbose)

    _VALID_FORMATS = {"yaml", "json", "toon"}
    if output_format not in _VALID_FORMATS:
        console.print(
            f"[red]Error:[/red] Unsupported format: {output_format!r}. "
            f"Supported: {', '.join(sorted(_VALID_FORMATS))}"
        )
        raise typer.Exit(code=1)

    cfg = resolve_config(explicit_path=config)

    # Inject retry config into runtime so PromptRuntime picks it up
    if retries > 1:
        cfg = CircuitryConfig(
            default_model=cfg.default_model,
            default_adapter=cfg.default_adapter,
            plugins=cfg.plugins,
            runtime={**cfg.runtime, "default_prompt_retries": retries},
        )

    # Locate curation meta_orchestrator
    try:
        pkg = importlib.resources.files("circuitry") / "curation" / "agents" / "meta_orchestrator.yml"
        meta_orch_path = Path(str(pkg))
    except Exception as e:
        console.print("[red]Error:[/red] Could not locate curation/agents/meta_orchestrator.yml")
        raise typer.Exit(code=1) from e

    if not meta_orch_path.exists():
        console.print("[red]Error:[/red] curation/agents/meta_orchestrator.yml not found.")
        raise typer.Exit(code=1)

    # Load structured rules from bundled rules/ directory
    initial_state: dict[str, Any] = {"user_request": prompt}
    try:
        from circuitry.rules import EFFECT_TYPES, load_all_rules, load_rules_for

        rules_pkg = importlib.resources.files("circuitry") / "bundled" / "rules"
        rules_dir = Path(str(rules_pkg))
        if rules_dir.is_dir():
            initial_state["rules"] = load_all_rules(rules_dir)
            for etype in EFFECT_TYPES:
                initial_state[f"rules_{etype}"] = load_rules_for(etype, rules_dir=rules_dir)
    except Exception:
        pass  # Best-effort; gen still works without rules

    # Load plugin descriptions from bundled docs/plugins/
    try:
        plugins_pkg = importlib.resources.files("circuitry") / "bundled" / "docs" / "plugins"
        plugins_dir = Path(str(plugins_pkg))
        if plugins_dir.is_dir():
            parts = [
                md_file.read_text(encoding="utf-8").strip()
                for md_file in sorted(plugins_dir.glob("*.md"))
            ]
            if parts:
                initial_state["plugins"] = "\n\n---\n\n".join(parts)
    except Exception:
        pass  # Best-effort

    # Determine orchestration output path from --out, or name + format
    _ext = {"yaml": ".yml", "json": ".json", "toon": ".toon"}
    orch_out = out if out is not None else Path(f"{name}{_ext.get(output_format, '.yml')}")

    req = RunRequest(
        orchestration_path=meta_orch_path,
        state_path=None,
        initial_state=initial_state,
        out_path=None,
        dry_run=False,
        validate_only=False,
        verbose=verbose,
        config=cfg,
        live_state_path=live_state,
    )

    if live_state:
        console.print(f"[bold]Live state:[/bold] {live_state}")

    if not verbose:
        with console.status("[cyan]Generating orchestration…[/cyan]"):
            result = run(req)
    else:
        result = run(req)

    _print_run_warnings(result.warnings)

    if not result.ok:
        console.print(f"[red]Generation failed:[/red] {result.error}")
        raise typer.Exit(code=1)

    # Extract generated YAML from the final effect
    generated = _find_last_effect_value(result.state)
    if generated is None:
        console.print("[red]Error:[/red] No output generated.")
        raise typer.Exit(code=1)

    yaml_text = str(generated)

    # Clean up LLM output: strip fences and document separators
    _clean = []
    for _line in yaml_text.splitlines():
        if _line.strip().startswith("```"):
            continue
        if _line.strip() == "---":
            continue
        _clean.append(_line)
    yaml_text = "\n".join(_clean).strip()

    parsed = _extract_generated_orchestration(yaml_text)
    if parsed is None:
        console.print(
            "[red]Error:[/red] The model's response did not contain a "
            "parseable orchestration document. Nothing was written."
        )
        raise typer.Exit(code=1)

    output_text = serialize_orchestration(parsed, output_format).rstrip("\n") + "\n"

    # Check the generated document exactly as `cof check` would, before
    # writing it anywhere the user's own path might already exist. mkstemp
    # (not a predictable "<stem>.tmp<suffix>" name) avoids a symlink planted
    # at that path, and the suffix matches --format rather than --out, so the
    # temp file's own extension is always one `serialize_orchestration` wrote.
    orch_out.parent.mkdir(parents=True, exist_ok=True)
    tmp_fd, tmp_name = tempfile.mkstemp(
        dir=orch_out.parent, suffix=_ext.get(output_format, ".yml")
    )
    tmp_out = Path(tmp_name)
    try:
        with os.fdopen(tmp_fd, "w", encoding="utf-8") as fh:
            fh.write(output_text)
        report = validate(tmp_out, config=cfg, skip_preflight=False, trust_document=True)
        if not report["ok"]:
            console.print(
                "[red]Error:[/red] The generated orchestration failed `cof check`; nothing was written:"
            )
            for err in report["errors"]:
                console.print(f"  - {escape(str(err))}")
            for warn in report.get("warnings", []):
                console.print(f"[yellow]Warning:[/yellow] {escape(str(warn))}")
            raise typer.Exit(code=1)

        for warn in report.get("warnings", []):
            console.print(f"[yellow]Warning:[/yellow] {escape(str(warn))}")
        tmp_out.replace(orch_out)
    finally:
        tmp_out.unlink(missing_ok=True)
    console.print(f"[green]Generated:[/green] {orch_out} (checked: Valid)")


WIZARD_EPILOG = """
[bold]Examples:[/bold]
  cof wizard --goal "Summarize an article, then translate it"
  cof wizard --goal "..." --out my_orch.yml
  cof wizard --goal "..." --reply answers.txt      # scripted, no TTY needed
  cof wizard --goal "..." --name my_pipeline --library

[bold]Not `cof gen`:[/bold] `gen` drives `agents/meta_orchestrator.yml`, a
single-shot generator — one prompt in, one document out. `wizard` drives
`agents/wizard.yml`, a multi-turn conversation that asks clarifying questions
before drafting — the same orchestration, and the same host code
(`circuitry.tui.wizard_host`), that the TUI's Chat view (`cof tui`, then `8`)
drives. The two commands produce different artifacts from different
orchestrations on purpose; reach for `wizard` when you want the conversation.
"""


@app.command(
    "wizard",
    help="Build an orchestration by talking to the wizard, headlessly. (Not `cof gen` — see below.)",
    epilog=WIZARD_EPILOG,
)
def wizard_cmd(
    goal: str = typer.Option(
        ..., "--goal", "-g", help="One-line description of what the orchestration should do."
    ),
    name: str | None = typer.Option(
        None,
        "--name",
        "-n",
        help="Name for the result (used for the filename / library slug). "
        "Derived from --goal if omitted.",
    ),
    category: str = typer.Option(
        WIZARD_DEFAULT_CATEGORY,
        "--category",
        help=f"One of: {', '.join(WIZARD_CATEGORIES)}.",
    ),
    reply: Path | None = typer.Option(
        None,
        "--reply",
        "-r",
        exists=True,
        dir_okay=False,
        readable=True,
        help="File of scripted replies, one per line, used instead of stdin.",
    ),
    out: Path | None = typer.Option(
        None, "--out", "-o", help="Write the final YAML here (default: stdout)."
    ),
    library: bool = typer.Option(
        False, "--library", help="Also save into the local library, indexed like a bundled entry."
    ),
    max_turns: int = typer.Option(10, "--max-turns", help="Stop after this many turns."),
    config: Path | None = typer.Option(None, "--config", "-c", help="Path to config JSON."),
    verbose: bool = typer.Option(False, "--verbose", "-v", help="Show orchestration execution logs."),
) -> None:
    configure_cli_logging(verbose=verbose)

    seed = Seed(
        name=name or " ".join(goal.split()[:6]),
        category=category.strip().lower() or WIZARD_DEFAULT_CATEGORY,
        goal=goal,
    )
    problems = seed.problems()
    if problems:
        for problem in problems:
            console.print(f"[red]Error:[/red] {problem}")
        raise typer.Exit(code=1)

    cfg = resolve_config(explicit_path=config)

    def _runner(state: dict[str, Any]) -> Turn:
        return run_turn(state, config=cfg, verbose=verbose)

    scripted = iter(reply.read_text(encoding="utf-8").splitlines()) if reply is not None else None

    def _next_reply() -> str | None:
        if scripted is not None:
            return next(scripted, None)
        line = sys.stdin.readline()
        return line.rstrip("\n") if line else None

    def _respond(turn: Turn, convo: Conversation) -> str | None:
        console.print(f"[bold]wizard[/bold]  {escape(turn.say)}", soft_wrap=True)
        if turn.yaml is not None:
            lines = len(convo.draft.splitlines())
            console.print(f"[dim]        (draft: {lines} lines)[/dim]")
        if convo.status is not None and not convo.status.ok and convo.status.errors:
            console.print(f"[yellow]        unresolved: {escape(convo.status.errors[0])}[/yellow]")
        if convo.done:
            console.print("[green]wizard is done.[/green]")
            return None
        next_reply = _next_reply()
        if next_reply is None:
            return None
        console.print(f"[bold]you[/bold]     {escape(next_reply)}")
        return next_reply

    from ..api import CircuitryExecutionError

    try:
        conversation = drive_conversation(seed, runner=_runner, respond=_respond, max_turns=max_turns)
    except CircuitryExecutionError as exc:
        console.print(f"[red]Error:[/red] {exc}")
        raise typer.Exit(code=1) from exc

    if not conversation.can_save:
        if not conversation.draft:
            console.print("[red]Error:[/red] No orchestration was produced.")
        else:
            errors = "; ".join(conversation.status.errors) if conversation.status else "not valid"
            console.print(f"[red]Error:[/red] The final draft did not pass validation — {errors}")
        raise typer.Exit(code=1)

    wrote = False
    if out is not None:
        try:
            path = save_to_file(conversation.draft, out)
        except InvalidDraft as exc:
            console.print(f"[red]Error:[/red] {exc}")
            raise typer.Exit(code=1) from exc
        console.print(f"[green]Wrote:[/green] {path} — check it with: cof check {path}")
        wrote = True

    if library:
        try:
            saved = save_to_library(conversation.draft, seed, library_dir=default_library_dir(cfg))
        except InvalidDraft as exc:
            console.print(f"[red]Error:[/red] {exc}")
            raise typer.Exit(code=1) from exc
        console.print(f"[green]Saved:[/green] {saved.name} -> {saved.path} — run it with: cof run {saved.name}")
        wrote = True

    if not wrote:
        print(conversation.draft)


@app.command("init", help="Initialize a new circuitry project in the current directory.")
def init_cmd(
    yes: bool = typer.Option(
        False, "--yes", "-y",
        help="Non-interactive: accept defaults (or --adapter/--adapter-url/--model) "
        "without prompting.",
    ),
    adapter: str | None = typer.Option(
        None, "--adapter", help="Adapter to configure (default: ollama).",
    ),
    adapter_url: str | None = typer.Option(
        None, "--adapter-url", help="Adapter base URL (default: http://localhost:11434).",
    ),
    model: str | None = typer.Option(
        None, "--model", help="Default model (default: llama3.1:8b).",
    ),
):
    config_path = Path.cwd() / "circuitry.config.json"
    hello_path = Path.cwd() / "hello.yml"

    if config_path.exists():
        console.print(f"[yellow]Warning:[/yellow] {config_path} already exists. Aborting.")
        raise typer.Exit(code=1)

    if yes:
        adapter = adapter or "ollama"
        adapter_url = adapter_url or "http://localhost:11434"
        model = model or "llama3.1:8b"
    else:
        adapter = adapter or typer.prompt("Adapter", default="ollama")
        adapter_url = adapter_url or typer.prompt("Adapter URL", default="http://localhost:11434")
        model = model or typer.prompt("Model", default="llama3.1:8b")

    config_data = {
        "default_model": model,
        "default_adapter": adapter,
        "runtime": {
            "adapters": {
                adapter: {
                    "base_url": adapter_url,
                },
            },
        },
    }
    config_bytes = (json.dumps(config_data, indent=2) + "\n").encode("utf-8")
    config_path.write_bytes(config_bytes)
    # The user just chose every value in it, so it is theirs: trust it, or
    # the first run here would skip it (see cli.config_trust). A broken
    # trust store shouldn't abort init — hello.yml still gets written and
    # the user can trust the file by hand.
    trusted = True
    try:
        record_trust(config_path, config_bytes, store_path=trust_store_path())
    except (TrustStoreError, OSError) as exc:
        trusted = False
        console.print(
            f"[yellow]Warning:[/yellow] Could not record trust for {config_path.name}: {exc} "
            f"Run `cof trust {config_path.name}` once this is fixed."
        )

    # No model, no network, no API key: `regex` is a bundled zero-dependency
    # plugin, so this runs immediately regardless of what adapter/model was
    # just configured above. See docs/guidebook/04-configuration.md.
    hello_yaml = """interface:
  inputs:
    name:
      type: string
      required: true
      description: Who to greet.

effects:
  - type: tool
    name: greet
    provider: regex
    params:
      pattern: "^(.*)$"
      input: "{{input.name}}"
      mode: sub
      replacement: "Hello, \\\\1! Welcome to circuitry."
"""
    hello_path.write_text(hello_yaml, encoding="utf-8")

    suffix = " (trusted)" if trusted else " (not trusted)"
    console.print(f"[green]Created:[/green] {config_path.name}{suffix}")
    console.print(f"[green]Created:[/green] {hello_path.name}")
    console.print()
    console.print("Try: [bold]cof run hello.yml -e name=World[/bold]")


@app.command("mcp", help="Run the circuitry MCP server (stdio transport).")
def mcp_cmd():
    """
    Launch the MCP server. Equivalent to running `circuitry-mcp` directly —
    both entrypoints invoke the same `circuitry.mcp.server.main`.

    Pair with a Claude Code .mcp.json entry to drive orchestrations from a
    chat session. See `.claude/commands/cof.md` for the full tool-loop docs.
    """
    from ..mcp.server import main as _mcp_main
    from .logging_setup import reset_cli_logging

    # The MCP server configures its own root-logger handler (stdout is MCP
    # framing) — undo this command's own `circuitry` logger handler/level
    # first, or a warning prints twice and INFO never reaches it (see
    # `reset_cli_logging`).
    reset_cli_logging()
    _mcp_main()


@app.command("tui", help="Launch the terminal UI (requires the 'tui' extra).")
def tui_cmd():
    """
    Open the Textual UI unconditionally, even when stdout is not a terminal.

    A bare `cof` opens the same UI automatically when stdin and stdout are
    both terminals and the extra is installed; this command forces it.
    """
    from ..tui import MISSING_EXTRA_MESSAGE, run_tui, textual_available

    if not textual_available():
        console.print(MISSING_EXTRA_MESSAGE, markup=False, highlight=False)
        raise typer.Exit(1)
    run_tui()


@app.command("version", help="Print version. (also: `cof --version`)")
def version_cmd():
    console.print(f"Circuitry {_resolve_version()}")


def main() -> None:
    app()


if __name__ == "__main__":
    main()
