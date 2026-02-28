from __future__ import annotations

import json
import os
from contextlib import nullcontext
from pathlib import Path
from typing import Optional

import typer
from rich.console import Console
from rich.panel import Panel
from rich.table import Table

from .config import find_config_path, load_config
from .doctor import register_doctor
from .runtime_shim import RunRequest, inspect_orchestration, run, validate
from .shared_library import (
    apply_service_profile,
    fetch_shared_orchestration,
    resolve_service_profile,
)

app = typer.Typer(add_completion=False)
console = Console()

register_doctor(app)


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


@app.command("run")
def run_cmd(
    orchestration: Path = typer.Argument(
        ..., exists=True, dir_okay=False, readable=True
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
    verbose: bool = typer.Option(False, "--verbose", "-v", help="More logs."),
):
    cfg_path = find_config_path(explicit_path=config)
    cfg = load_config(cfg_path)

    if not (quiet or json_out):
        _print_header("Circuitry · Run")
        console.print(
            f"[bold]Config:[/bold] {cfg_path if cfg_path else '— (defaults)'}"
        )
        console.print(f"[bold]Orchestration:[/bold] {orchestration}")
        console.print(f"[bold]State (in):[/bold] {state if state else '—'}")
        console.print(f"[bold]State (out):[/bold] {out if out else '—'}")
        console.print(f"[bold]Dry run:[/bold] {dry_run}")

    req = RunRequest(
        orchestration_path=orchestration,
        state_path=state,
        initial_state=None,
        out_path=out,
        dry_run=dry_run,
        validate_only=False,
        verbose=verbose,
        config=cfg,
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

    if not (quiet or json_out):
        console.print("[green]Run succeeded[/green]")
        if out:
            console.print(f"[bold]State written:[/bold] {out}")

    # Print --print (or default print for --json with no --out)
    if print_state or (not out and json_out):
        if pretty:
            console.print_json(json.dumps(result.state, indent=2, sort_keys=True))
        else:
            console.print_json(json.dumps(result.state))

    # warnings
    if result.warnings and not (quiet or json_out):
        for w in result.warnings:
            console.print(f"[yellow]Warning:[/yellow] {w}")


@app.command("fetch")
def fetch_cmd(
    asset_id: str = typer.Argument(..., help="Shared library asset identifier."),
    version: Optional[str] = typer.Option(
        None, "--version", "-V", help="Specific asset version. Defaults to latest."
    ),
    out: Path = typer.Option(
        ...,
        "--out",
        "-o",
        help="Output path for fetched orchestration YAML.",
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
    cfg_path = find_config_path(explicit_path=config)
    cfg = load_config(cfg_path)
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


@app.command("run-library")
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
    verbose: bool = typer.Option(False, "--verbose", "-v", help="More logs."),
):
    cfg_path = find_config_path(explicit_path=config)
    base_cfg = load_config(cfg_path)
    token = auth_token or os.getenv("CIRCUITRY_LIBRARY_TOKEN")

    try:
        profile = resolve_service_profile(cfg=base_cfg, profile_name=service_profile)
        cfg = apply_service_profile(cfg=base_cfg, profile=profile)
        asset = fetch_shared_orchestration(
            cfg=cfg,
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
        console.print(f"[bold]Dry run:[/bold] {dry_run}")

    req = RunRequest(
        orchestration_path=asset.file_path,
        state_path=state,
        initial_state=None,
        out_path=out,
        dry_run=dry_run,
        validate_only=False,
        shared_library_metadata=asset.metadata,
        verbose=verbose,
        config=cfg,
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

    if not (quiet or json_out):
        console.print("[green]Run succeeded[/green]")
        if out:
            console.print(f"[bold]State written:[/bold] {out}")

    if print_state or (not out and json_out):
        if pretty:
            console.print_json(json.dumps(result.state, indent=2, sort_keys=True))
        else:
            console.print_json(json.dumps(result.state))

    if result.warnings and not (quiet or json_out):
        for w in result.warnings:
            console.print(f"[yellow]Warning:[/yellow] {w}")


@app.command("validate")
def validate_cmd(
    orchestration: Path = typer.Argument(
        ..., exists=True, dir_okay=False, readable=True
    ),
    json_out: bool = typer.Option(
        False, "--json", help="Output machine-readable JSON only."
    ),
):
    _print_header("Circuitry · Validate")
    with console.status("[cyan]Validating…[/cyan]"):
        result = validate(orchestration)

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


@app.command("inspect")
def inspect_cmd(
    orchestration: Path = typer.Argument(
        ..., exists=True, dir_okay=False, readable=True
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


@app.command("version")
def version_cmd():
    console.print("circuitry 0.1.0")


def main() -> None:
    app()


if __name__ == "__main__":
    main()
