from __future__ import annotations

import importlib.resources
import json
import os
import sys
from contextlib import nullcontext
from pathlib import Path
from typing import Any, Optional

import click
import typer
from rich.console import Console
from rich.markup import escape
from rich.panel import Panel
from rich.table import Table
from typer.core import TyperGroup

from .config import GLOBAL_CONFIG_DIR, CircuitryConfig, ConfigError, resolve_config
from .doctor import register_doctor
from .library_sources import (
    Entry,
    LibraryFetchError,
    LibraryRegistry,
    LibrarySourceError,
    build_registry,
)
from .orchestration_loader import serialize_orchestration
from .redaction import REDACTED, redact_env_pairs
from .registry import eject_destination, resolve_bundled, write_ejected
from .runtime_shim import RunRequest, inspect_orchestration, run, validate
from .setup import register_setup
from .shared_library import (
    apply_service_profile,
    fetch_shared_orchestration,
    resolve_service_profile,
)

console = Console()
err_console = Console(stderr=True)


class CircuitryGroup(TyperGroup):
    """Root command group that renders config problems as one actionable line.

    Every subcommand taking ``--config/-c`` reaches the same loader, so the
    catch lives here once instead of in per-command ``try``/``except`` blocks —
    commands registered from other modules (``doctor``, ``setup``) and any
    future ones are covered automatically.
    """

    def invoke(self, ctx: click.Context) -> Any:
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


@app.callback(invoke_without_command=True)
def _root(ctx: typer.Context) -> None:
    # No docstring/help here on purpose: the group's help text comes from
    # ``Typer(help=...)`` above and must stay byte-identical.
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
register_setup(app)

_LAST_RUN_PATH = GLOBAL_CONFIG_DIR / "last-run.json"


def _print_header(title: str) -> None:
    console.print(Panel.fit(title, border_style="cyan"))


def _write_state_json(*, out: Path, state: dict, pretty: bool) -> None:
    out.parent.mkdir(parents=True, exist_ok=True)
    if pretty:
        out.write_text(
            json.dumps(state, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
    else:
        out.write_text(json.dumps(state) + "\n", encoding="utf-8")


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


def _do_validate(
    orchestration: Path,
    json_out: bool,
    *,
    config: Optional[Path] = None,
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
    with console.status("[cyan]Validating…[/cyan]") if not json_out else nullcontext():
        result = validate(orchestration, config=cfg, skip_preflight=skip_preflight)

    if json_out:
        console.print_json(json.dumps(result, ensure_ascii=False))
        raise typer.Exit(code=0 if result["ok"] else 1)

    if result["ok"]:
        console.print("[green]Valid[/green]")
    else:
        console.print("[red]Invalid[/red]")
        for e in result.get("errors", []):
            console.print(f" - {e}")
        raise typer.Exit(code=1)


def _library_registry(config_path: Optional[Path] = None) -> LibraryRegistry:
    """Build the configured library registry, reporting config errors as CLI errors."""
    try:
        return build_registry(config_path=config_path)
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
    candidate = Path(name_or_path)
    if candidate.exists() and candidate.is_file():
        return candidate

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


RUN_EPILOG = """
[bold]Examples:[/bold]
  cof run hello -e name=World
  cof run article-summarizer -e article_text='...'
  cof run ./my-orch.yml -e topic=cats --tail
  cof run ./my-orch.yml --live-state ./state.json
  cof run learn/hello -e name=World --model gpt-oss:20b
  cof run learn/hello -e name=World --adapter ollama --model llama3.1:8b
  cof run --last

[bold]Resolution order:[/bold] local file path > bundled orchestration name.

[bold]Settings precedence:[/bold] CLI flags (--adapter/--model) > --profile >
orchestration > environment (CIRCUITRY_ADAPTER/CIRCUITRY_MODEL) > config file
> defaults. Env vars overlay the config layer, so a flag or profile beats them.
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
    orchestration: Optional[str] = typer.Argument(
        None,
        help="Path to orchestration file, or name of a bundled orchestration.",
    ),
    config: Optional[Path] = typer.Option(
        None, "--config", "-c", help="Path to config JSON (or use CIRCUITRY_CONFIG)."
    ),
    state: Optional[Path] = typer.Option(
        None, "--state", "-s", help="Optional input state JSON."
    ),
    out: Optional[Path] = typer.Option(
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
    live_state: Optional[Path] = typer.Option(
        None, "--live-state",
        help="Write state atomically to this file after each effect. For live monitoring.",
    ),
    env_vars: Optional[list[str]] = typer.Option(
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
    skip_preflight: bool = typer.Option(
        False, "--skip-preflight",
        help="Bypass dependency preflight; run even if check()s reported missing deps.",
    ),
    profile: Optional[str] = typer.Option(
        None, "--profile",
        help=(
            "Named profile to apply (profiles/<name>.yml, orchestration-scoped "
            "wins over project-level). Precedence: CLI > profile > orchestration > config."
        ),
    ),
    adapter: Optional[str] = typer.Option(
        None, "--adapter",
        help="Adapter to use for this run. Beats CIRCUITRY_ADAPTER, --profile, and the orchestration.",
    ),
    model: Optional[str] = typer.Option(
        None, "--model",
        help="Model to use for this run. Beats CIRCUITRY_MODEL, --profile, and the orchestration.",
    ),
):
    # --last: replay stashed args
    if last:
        stashed = _load_last_run()
        orchestration = stashed["orchestration"]
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
        profile = stashed.get("profile")
        adapter = stashed.get("adapter")
        model = stashed.get("model")

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

    if orchestration is None:
        console.print("[red]Error:[/red] Missing orchestration. Use --last or provide a path/name.")
        console.print("[dim]Tip: run [bold]cof list[/bold] to see available orchestrations.[/dim]")
        raise typer.Exit(code=1)

    # Resolve orchestration: local file path > library source name
    run_registry = _library_registry(config)
    orch_path = _resolve_orchestration(
        orchestration,
        registry=run_registry,
        on_warning=_warn,
    )
    if orch_path is None:
        console.print(f"[red]Error:[/red] Orchestration not found: {orchestration}")
        _print_source_notices(run_registry)
        console.print("[dim]Tip: run [bold]cof list[/bold] to see available orchestrations.[/dim]")
        raise typer.Exit(code=1)

    # Auto-pipe detection (before mutual exclusivity check so --tail wins in pipes)
    if not sys.stdout.isatty() and not tail:
        json_out = True
        quiet = True

    # Mutual exclusivity: --tail vs --print/--json
    if tail and (print_state or json_out):
        console.print("[red]Error:[/red] --tail is mutually exclusive with --print and --json.")
        raise typer.Exit(code=1)

    cfg = resolve_config(explicit_path=config)

    if not (quiet or json_out):
        _print_header("Circuitry · Run")
        console.print(
            f"[bold]Config:[/bold] {config if config else '— (resolved)'}"
        )
        orch_label = orchestration if str(orch_path) == orchestration else f"{orchestration} ({orch_path})"
        console.print(f"[bold]Orchestration:[/bold] {orch_label}")
        console.print(f"[bold]State (in):[/bold] {state if state else '—'}")
        console.print(f"[bold]State (out):[/bold] {out if out else '—'}")
        if live_state:
            console.print(f"[bold]Live state:[/bold] {live_state}")
        if adapter:
            console.print(f"[bold]Adapter (override):[/bold] {adapter}")
        if model:
            console.print(f"[bold]Model (override):[/bold] {model}")
        console.print(f"[bold]Dry run:[/bold] {dry_run}")

    # Build initial state from --state file + -e overrides
    initial_state: dict[str, Any] | None = None
    inline = _parse_env_vars(env_vars)
    if inline:
        if state:
            initial_state = json.loads(state.read_text(encoding="utf-8"))
            initial_state.update(inline)
        else:
            initial_state = inline

    req = RunRequest(
        orchestration_path=orch_path,
        state_path=state if initial_state is None else None,
        initial_state=initial_state,
        out_path=out,
        dry_run=dry_run,
        validate_only=False,
        verbose=verbose,
        config=cfg,
        live_state_path=live_state,
        skip_preflight=skip_preflight,
        profile_name=profile,
        adapter_override=adapter,
        model_override=model,
    )

    with (
        nullcontext()
        if (quiet or json_out or verbose)
        else console.status("[cyan]Running…[/cyan]")
    ):
        result = run(req)

    # Write --out for both success and failure (failure state still contains runtime metadata).
    if out:
        _write_state_json(out=out, state=result.state, pretty=pretty)

    if not result.ok:
        if json_out:
            payload = {
                "ok": False,
                "error": result.error,
                "warnings": result.warnings,
                "state_out": str(out) if out else None,
            }
            console.print_json(json.dumps(payload))
        else:
            console.print("[red]Run failed[/red]")
            console.print(f"[red]Error:[/red] {result.error}")
            if out:
                console.print(f"[bold]State written:[/bold] {out}")
        raise typer.Exit(code=1)

    # Stash for --last (only on success, skip if replaying via --last).
    # Env-var values for credential-shaped keys are redacted before disk write —
    # see RUN_EPILOG and circuitry.cli.redaction.
    if not last:
        _save_last_run({
            "orchestration": str(orch_path),
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
            "adapter": adapter,
            "model": model,
        })

    if tail:
        val = _find_last_effect_value(result.state)
        if val is not None:
            print(val if isinstance(val, str) else json.dumps(val))
    elif not (quiet or json_out):
        console.print("[green]Run succeeded[/green]")
        if out:
            console.print(f"[bold]State written:[/bold] {out}")

    # Print --print (or default print for --json with no --out)
    if not tail and (print_state or (not out and json_out)):
        if pretty:
            console.print_json(json.dumps(result.state, indent=2, sort_keys=True))
        else:
            console.print_json(json.dumps(result.state))

    # warnings
    if result.warnings and not (quiet or json_out):
        for w in result.warnings:
            console.print(f"[yellow]Warning:[/yellow] {w}")


@app.command("fetch", help="Fetch a shared library orchestration.")
def fetch_cmd(
    asset_id: str = typer.Argument(..., help="Shared library asset identifier."),
    version: Optional[str] = typer.Option(
        None, "--version", "-V", help="Specific asset version. Defaults to latest."
    ),
    out: Path = typer.Option(
        ...,
        "--out",
        "-o",
        help="Output path for fetched orchestration.",
    ),
    config: Optional[Path] = typer.Option(
        None, "--config", "-c", help="Path to config JSON (or use CIRCUITRY_CONFIG)."
    ),
    auth_token: Optional[str] = typer.Option(
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
        raise typer.Exit(code=1)

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
    version: Optional[str] = typer.Option(
        None, "--version", "-V", help="Specific asset version. Defaults to latest."
    ),
    config: Optional[Path] = typer.Option(
        None, "--config", "-c", help="Path to config JSON (or use CIRCUITRY_CONFIG)."
    ),
    auth_token: Optional[str] = typer.Option(
        None,
        "--auth-token",
        help="Shared library auth token (or set CIRCUITRY_LIBRARY_TOKEN).",
    ),
    service_profile: Optional[str] = typer.Option(
        None,
        "--service-profile",
        help="Apply runtime overrides from runtime.library.service_profiles.<name>.",
    ),
    state: Optional[Path] = typer.Option(
        None, "--state", "-s", help="Optional input state JSON."
    ),
    out: Optional[Path] = typer.Option(
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
    live_state: Optional[Path] = typer.Option(
        None, "--live-state",
        help="Write state atomically to this file after each effect. For live monitoring.",
    ),
    env_vars: Optional[list[str]] = typer.Option(
        None, "-e",
        help="Inline state variable (KEY=VALUE). Repeatable.",
    ),
    tail: bool = typer.Option(
        False, "--tail",
        help="Print only the final effect's value as plain text. Ideal for piping.",
    ),
    adapter: Optional[str] = typer.Option(
        None, "--adapter",
        help="Adapter to use for this run. Beats CIRCUITRY_ADAPTER and the orchestration.",
    ),
    model: Optional[str] = typer.Option(
        None, "--model",
        help="Model to use for this run. Beats CIRCUITRY_MODEL and the orchestration.",
    ),
):
    # Auto-pipe detection
    if not sys.stdout.isatty():
        json_out = True
        quiet = True

    if tail and (print_state or json_out):
        console.print("[red]Error:[/red] --tail is mutually exclusive with --print and --json.")
        raise typer.Exit(code=1)

    cfg = resolve_config(explicit_path=config)
    token = auth_token or os.getenv("CIRCUITRY_LIBRARY_TOKEN")

    try:
        profile = resolve_service_profile(cfg=cfg, profile_name=service_profile)
        effective_cfg = apply_service_profile(cfg=cfg, profile=profile)
        asset = fetch_shared_orchestration(
            cfg=effective_cfg,
            asset_id=asset_id,
            version=version,
            auth_token=token,
        )
        if profile is not None:
            asset.metadata["service_profile"] = profile.name
    except Exception as e:
        if json_out:
            console.print_json(json.dumps({"ok": False, "error": str(e)}))
        else:
            console.print(f"[red]Shared library retrieval failed:[/red] {e}")
        raise typer.Exit(code=1)

    if not (quiet or json_out):
        _print_header("Circuitry · Run Library")
        console.print(f"[bold]Asset:[/bold] {asset.asset_id}@{asset.version}")
        console.print(f"[bold]Source:[/bold] {asset.source}")
        console.print(f"[bold]Resolved path:[/bold] {asset.file_path}")
        console.print(
            f"[bold]Service profile:[/bold] {service_profile if service_profile else '—'}"
        )
        console.print(f"[bold]State (in):[/bold] {state if state else '—'}")
        console.print(f"[bold]State (out):[/bold] {out if out else '—'}")
        if live_state:
            console.print(f"[bold]Live state:[/bold] {live_state}")
        if adapter:
            console.print(f"[bold]Adapter (override):[/bold] {adapter}")
        if model:
            console.print(f"[bold]Model (override):[/bold] {model}")
        console.print(f"[bold]Dry run:[/bold] {dry_run}")

    # Build initial state from --state file + -e overrides
    initial_state: dict[str, Any] | None = None
    inline = _parse_env_vars(env_vars)
    if inline:
        if state:
            initial_state = json.loads(state.read_text(encoding="utf-8"))
            initial_state.update(inline)
        else:
            initial_state = inline

    req = RunRequest(
        orchestration_path=asset.file_path,
        state_path=state if initial_state is None else None,
        initial_state=initial_state,
        out_path=out,
        dry_run=dry_run,
        validate_only=False,
        shared_library_metadata=asset.metadata,
        verbose=verbose,
        config=effective_cfg,
        live_state_path=live_state,
        adapter_override=adapter,
        model_override=model,
    )

    with (
        nullcontext()
        if (quiet or json_out or verbose)
        else console.status("[cyan]Running…[/cyan]")
    ):
        result = run(req)

    if out:
        _write_state_json(out=out, state=result.state, pretty=pretty)

    if not result.ok:
        if json_out:
            payload = {
                "ok": False,
                "error": result.error,
                "warnings": result.warnings,
                "state_out": str(out) if out else None,
            }
            console.print_json(json.dumps(payload))
        else:
            console.print("[red]Run failed[/red]")
            console.print(f"[red]Error:[/red] {result.error}")
            if out:
                console.print(f"[bold]State written:[/bold] {out}")
        raise typer.Exit(code=1)

    if tail:
        val = _find_last_effect_value(result.state)
        if val is not None:
            print(val if isinstance(val, str) else json.dumps(val))
    elif not (quiet or json_out):
        console.print("[green]Run succeeded[/green]")
        if out:
            console.print(f"[bold]State written:[/bold] {out}")

    if not tail and (print_state or (not out and json_out)):
        if pretty:
            console.print_json(json.dumps(result.state, indent=2, sort_keys=True))
        else:
            console.print_json(json.dumps(result.state))

    if result.warnings and not (quiet or json_out):
        for w in result.warnings:
            console.print(f"[yellow]Warning:[/yellow] {w}")


@app.command(
    "list",
    help="List available bundled orchestrations (or compiled-in extensions with --extensions).",
)
def list_cmd(
    category: Optional[str] = typer.Option(
        None, "--category", "-C", help="Filter by category (example, utility, creative, tooling, template)."
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
    source: Optional[str] = typer.Option(
        None, "--source", "-S", help="Only list entries from this library source."
    ),
    config: Optional[Path] = typer.Option(
        None, "--config", "-c", help="Path to config JSON (or use CIRCUITRY_CONFIG)."
    ),
):
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


def _list_extensions(*, json_out: bool, config_path: Optional[Path]) -> None:
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

    def status(name: str, allowed: Optional[list[str]]) -> str:
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

    def render_section(title: str, names: list[str], allowed: Optional[list[str]]) -> None:
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


@app.command("info", help="Show details for a bundled orchestration.")
def info_cmd(
    name: str = typer.Argument(..., help="Name of the orchestration."),
    json_out: bool = typer.Option(False, "--json", help="Output machine-readable JSON only."),
    config: Optional[Path] = typer.Option(
        None, "--config", "-c", help="Path to config JSON (or use CIRCUITRY_CONFIG)."
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

    if json_out:
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

    # Show the actual orchestration YAML source
    bundled_path = found.path
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
    out: Optional[Path] = typer.Option(
        None, "--out", "-o", help="Output path. Defaults to ./<filename>."
    ),
    config: Optional[Path] = typer.Option(
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
    if dest.exists():
        if not typer.confirm(f"{dest} already exists. Overwrite?", default=False):
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


@library_app.command(
    "refresh",
    help="Fetch remote library sources into the local cache. This is the only "
    "command that touches the network for a library source.",
)
def library_refresh_cmd(
    source: Optional[str] = typer.Argument(
        None, help="Source to refresh. Omit to refresh every configured source."
    ),
    json_out: bool = typer.Option(False, "--json", help="Output machine-readable JSON only."),
    config: Optional[Path] = typer.Option(
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
    config: Optional[Path] = typer.Option(
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
    config: Optional[Path] = typer.Option(
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


@app.command("gen", help="Generate an orchestration from a natural language prompt.")
def gen_cmd(
    name: str = typer.Argument(
        ..., help="Name for the generated orchestration (used as filename)."
    ),
    prompt: str = typer.Argument(
        ..., help="Natural language description of the orchestration to generate."
    ),
    out: Optional[Path] = typer.Option(
        None, "--out", "-o", help="Write resulting state JSON to this file (live-updated during run)."
    ),
    config: Optional[Path] = typer.Option(
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
    except Exception:
        console.print("[red]Error:[/red] Could not locate curation/agents/meta_orchestrator.yml")
        raise typer.Exit(code=1)

    if not meta_orch_path.exists():
        console.print("[red]Error:[/red] curation/agents/meta_orchestrator.yml not found.")
        raise typer.Exit(code=1)

    # Load structured rules from bundled rules/ directory
    initial_state: dict[str, Any] = {"user_request": prompt}
    try:
        from circuitry.rules import load_all_rules, load_rules_for

        rules_pkg = importlib.resources.files("circuitry") / "bundled" / "rules"
        rules_dir = Path(str(rules_pkg))
        if rules_dir.is_dir():
            initial_state["rules"] = load_all_rules(rules_dir)
            for etype in ("prompt", "dynamic", "loop", "conditional", "tool", "reflector"):
                initial_state[f"rules_{etype}"] = load_rules_for(etype, rules_dir=rules_dir)
    except Exception:
        pass  # Best-effort; gen still works without rules

    # Load plugin descriptions from bundled docs/plugins/
    try:
        plugins_pkg = importlib.resources.files("circuitry") / "bundled" / "docs" / "plugins"
        plugins_dir = Path(str(plugins_pkg))
        if plugins_dir.is_dir():
            parts = []
            for md_file in sorted(plugins_dir.glob("*.md")):
                parts.append(md_file.read_text(encoding="utf-8").strip())
            if parts:
                initial_state["plugins"] = "\n\n---\n\n".join(parts)
    except Exception:
        pass  # Best-effort

    # Determine orchestration output path from name + format
    _ext = {"yaml": ".yml", "json": ".json", "toon": ".toon"}
    orch_out = Path(f"{name}{_ext.get(output_format, '.yml')}")

    # --out is for live state / resulting state JSON
    live_state_path = None
    if out:
        out.parent.mkdir(parents=True, exist_ok=True)
        live_state_path = out

    req = RunRequest(
        orchestration_path=meta_orch_path,
        state_path=None,
        initial_state=initial_state,
        out_path=None,
        dry_run=False,
        validate_only=False,
        verbose=verbose,
        config=cfg,
        live_state_path=live_state_path,
    )

    if live_state_path:
        console.print(f"[bold]Live state:[/bold] {live_state_path}")

    if not verbose:
        with console.status("[cyan]Generating orchestration…[/cyan]"):
            result = run(req)
    else:
        result = run(req)

    # Write resulting state to --out
    if out:
        _write_state_json(out=out, state=result.state, pretty=False)

    if not result.ok:
        console.print(f"[red]Generation failed:[/red] {result.error}")
        raise typer.Exit(code=1)

    # Extract generated YAML from the final effect
    generated = _find_last_effect_value(result.state)
    if generated is None:
        console.print("[red]Error:[/red] No output generated.")
        raise typer.Exit(code=1)

    yaml_text = str(generated)

    # Clean up LLM output: strip fences, preamble, and document separators
    _clean = []
    for _line in yaml_text.splitlines():
        if _line.strip().startswith("```"):
            continue
        if _line.strip() == "---":
            continue
        _clean.append(_line)
    yaml_text = "\n".join(_clean).strip()

    # Strip preamble text before the first effects: or adapter: line
    _clean_lines = yaml_text.splitlines()
    for _i, _line in enumerate(_clean_lines):
        if _line.startswith("effects:") or _line.startswith("adapter:"):
            yaml_text = "\n".join(_clean_lines[_i:]).strip()
            break

    import yaml as _yaml  # type: ignore[import-untyped]
    parsed = _yaml.safe_load(yaml_text)
    if not isinstance(parsed, dict):
        parsed = {"raw": yaml_text}
    output_text = serialize_orchestration(parsed, output_format).rstrip("\n")

    orch_out.parent.mkdir(parents=True, exist_ok=True)
    orch_out.write_text(output_text + "\n", encoding="utf-8")
    console.print(f"[green]Generated:[/green] {orch_out}")


@app.command("init", help="Initialize a new circuitry project in the current directory.")
def init_cmd():
    config_path = Path.cwd() / "circuitry.config.json"
    hello_path = Path.cwd() / "hello.yml"

    if config_path.exists():
        console.print(f"[yellow]Warning:[/yellow] {config_path} already exists. Aborting.")
        raise typer.Exit(code=1)

    adapter = typer.prompt("Adapter", default="ollama")
    adapter_url = typer.prompt("Adapter URL", default="http://localhost:11434")
    model = typer.prompt("Model", default="llama3.1:8b")

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
    config_path.write_text(
        json.dumps(config_data, indent=2) + "\n", encoding="utf-8"
    )

    hello_yaml = """effects:
  - type: prompt
    name: greet
    template: "Say hello to {{name}} in a creative way."
    format: text
"""
    hello_path.write_text(hello_yaml, encoding="utf-8")

    console.print(f"[green]Created:[/green] {config_path.name}")
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


@app.command("version", help="Print version.")
def version_cmd():
    from importlib.metadata import PackageNotFoundError, version as pkg_version

    # Distribution name is `circuitry-cof` on PyPI; the legacy `circuitry`
    # lookup is kept as a fallback for editable installs that pre-date the
    # rename.
    for dist in ("circuitry-cof", "circuitry"):
        try:
            ver = pkg_version(dist)
            break
        except PackageNotFoundError:
            continue
    else:
        ver = "0.1.0+unknown"
    console.print(f"Circuitry {ver}")


def main() -> None:
    app()


if __name__ == "__main__":
    main()
