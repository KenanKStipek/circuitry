from __future__ import annotations

import json
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

app = typer.Typer(add_completion=False)
console = Console()

register_doctor(app)


def _print_header(title: str) -> None:
    console.print(Panel.fit(title, border_style="cyan"))


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
        out_path=out,
        dry_run=dry_run,
        validate_only=False,
        verbose=verbose,
        config=cfg,
    )

    with (
        nullcontext()
        if (quiet or json_out)
        else console.status("[cyan]Running…[/cyan]")
    ):
        result = run(req)

    if not result.ok:
        if json_out:
            payload = {"ok": False, "error": result.error, "warnings": result.warnings}
            console.print_json(json.dumps(payload))
        else:
            console.print(f"[red]Error:[/red] {result.error}")
        raise typer.Exit(code=1)

    # Write --out
    if out:
        out.parent.mkdir(parents=True, exist_ok=True)
        if pretty:
            out.write_text(
                json.dumps(result.state, indent=2, sort_keys=True) + "\n",
                encoding="utf-8",
            )
        else:
            out.write_text(json.dumps(result.state) + "\n", encoding="utf-8")

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
