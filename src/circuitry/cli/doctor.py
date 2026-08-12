from __future__ import annotations

from pathlib import Path

import typer
from rich.console import Console
from rich.table import Table

from ..adapters import build_adapter
from ..adapters.factory import ADAPTER_REGISTRY
from ..core.runtime_plugins import load_plugins
from ..plugins.factory import PLUGIN_REGISTRY, build_plugin
from ..preflight import call_check
from .config import find_config_path, load_config, resolve_config
from .detect import detect_all
from .effective_settings import resolve_effective_settings
from .orchestration_loader import load_orchestration_file

console = Console()


def register_doctor(app: typer.Typer) -> None:
    @app.command("doctor", help="System diagnostics — check backends, config, and connectivity.")
    def doctor_cmd(
        config: Path | None = typer.Option(
            None,
            "--config",
            "-c",
            help="Path to config JSON (or use CIRCUITRY_CONFIG).",
        ),
        orchestration: Path | None = typer.Option(
            None,
            "--orch",
            help="Optional orchestration file to include in effective settings.",
        ),
        generate: bool = typer.Option(
            False, "--generate", help="Also run a tiny generate call to test the adapter."
        ),
    ) -> None:
        cfg_path = find_config_path(explicit_path=config)
        cfg = load_config(cfg_path)
        resolved_cfg = resolve_config(explicit_path=config)

        orch_obj = {}
        if orchestration:
            orch_obj = load_orchestration_file(orchestration)

        effective = resolve_effective_settings(cfg=cfg, orch=orch_obj)

        table = Table(title="Circuitry · Doctor", show_lines=True)
        table.add_column("Check")
        table.add_column("Result")

        # Config
        table.add_row("Config path", str(cfg_path) if cfg_path else "— (defaults)")
        table.add_row(
            "Effective adapter",
            f"{effective.adapter} (source: {effective.sources.get('adapter')})",
        )
        table.add_row(
            "Effective model",
            f"{effective.model} (source: {effective.sources.get('model')})",
        )

        # Backend detection
        ollama_url = resolved_cfg.runtime.get("adapters", {}).get("ollama", {}).get("base_url", "http://localhost:11434")
        comfyui_url = resolved_cfg.runtime.get("plugins", {}).get("comfyui", {}).get("base_url", "http://localhost:8188")

        result = detect_all(ollama_url=ollama_url, comfyui_url=comfyui_url)

        for backend in result.backends:
            status = f"OK ({backend.detail})" if backend.available else f"NOT FOUND ({backend.detail})"
            table.add_row(backend.name, status)

        # Model check (if Ollama available, check if effective model is present)
        ollama = result.get("ollama")
        if ollama and ollama.available and effective.model:
            if effective.model in ollama.models:
                table.add_row("Model present", "YES")
            else:
                table.add_row("Model present", f"NO (missing: {effective.model})")

        # Generate test
        if generate:
            adapter = build_adapter(
                adapter_name=effective.adapter or "", runtime=effective.runtime or {}
            )
            try:
                if not effective.model:
                    raise RuntimeError(
                        "No model resolved (set default_model or model in orchestration)."
                    )
                res = adapter.generate(
                    model=effective.model,
                    prompt="Say hello in 5 words.",
                    timeout_seconds=15,
                )
                table.add_row("Generate test", f"OK ({res.text})")
            except Exception as e:
                table.add_row("Generate test", f"FAIL ({e})")
                console.print(table)
                raise typer.Exit(code=1)

        console.print(table)

        # Allowlist + preflight: walk every enabled (or compiled-in, when
        # no allowlist is set) extension, call check(), report per-item.
        any_failed = _check_extensions(resolved_cfg)
        if any_failed:
            raise typer.Exit(code=1)


def _check_extensions(cfg) -> bool:  # type: ignore[no-untyped-def]
    """Render adapter / tool / runtime-plugin check() results.

    Returns True if any check failed. Used to set the doctor exit code.
    """
    runtime_cfg = cfg.runtime or {}
    any_failed = False

    def select(allowed, compiled):  # type: ignore[no-untyped-def]
        if allowed is None:
            return list(compiled), "compiled-in (default-open)"
        return list(allowed), "allowlisted"

    # Adapters
    adapter_names, adapter_mode = select(cfg.enabled_adapters, ADAPTER_REGISTRY.keys())
    if adapter_names:
        table = Table(
            title=f"Adapters ({adapter_mode})", show_header=True, header_style="bold cyan"
        )
        table.add_column("Name")
        table.add_column("Status")
        table.add_column("Missing / message")
        for name in sorted(adapter_names):
            try:
                adapter = build_adapter(adapter_name=name, runtime=runtime_cfg)
            except RuntimeError as exc:
                table.add_row(name, "[yellow]deferred[/yellow]", str(exc))
                continue
            except ValueError as exc:
                table.add_row(name, "[red]unknown[/red]", str(exc))
                any_failed = True
                continue
            r = call_check(adapter)
            if r.ok:
                table.add_row(name, "[green]ok[/green]", r.message or "—")
            else:
                table.add_row(
                    name,
                    "[red]missing deps[/red]",
                    f"{r.missing}" + (f" — {r.message}" if r.message else ""),
                )
                any_failed = True
        console.print(table)

    # Tool plugins
    tool_names, tool_mode = select(cfg.enabled_tools, PLUGIN_REGISTRY.keys())
    if tool_names:
        table = Table(
            title=f"Tool plugins ({tool_mode})", show_header=True, header_style="bold cyan"
        )
        table.add_column("Name")
        table.add_column("Status")
        table.add_column("Missing / message")
        for name in sorted(tool_names):
            try:
                tool = build_plugin(plugin_name=name, runtime=runtime_cfg)
            except (RuntimeError, ValueError) as exc:
                table.add_row(name, "[red]unknown[/red]", str(exc))
                any_failed = True
                continue
            r = call_check(tool)
            if r.ok:
                table.add_row(name, "[green]ok[/green]", r.message or "—")
            else:
                table.add_row(
                    name,
                    "[red]missing deps[/red]",
                    f"{r.missing}" + (f" — {r.message}" if r.message else ""),
                )
                any_failed = True
        console.print(table)

    # Runtime plugins (only those configured for this project)
    if cfg.plugins:
        table = Table(
            title="Runtime plugins (configured)",
            show_header=True, header_style="bold cyan",
        )
        table.add_column("Name")
        table.add_column("Status")
        table.add_column("Missing / message")
        for load_result in load_plugins(
            list(cfg.plugins), allowed=cfg.enabled_plugins
        ):
            display = load_result.plugin_id
            if load_result.plugin is None:
                table.add_row(display, "[red]load failed[/red]", str(load_result.error))
                any_failed = True
                continue
            r = call_check(load_result.plugin)
            display = getattr(load_result.plugin, "name", display)
            if r.ok:
                table.add_row(display, "[green]ok[/green]", r.message or "—")
            else:
                table.add_row(
                    display,
                    "[red]missing deps[/red]",
                    f"{r.missing}" + (f" — {r.message}" if r.message else ""),
                )
                any_failed = True
        console.print(table)

    return any_failed
