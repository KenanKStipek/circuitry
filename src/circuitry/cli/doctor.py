from __future__ import annotations

from pathlib import Path
from typing import Optional

import typer
from rich.console import Console
from rich.table import Table

from .config import find_config_path, load_config, resolve_config
from .detect import detect_all
from .effective_settings import resolve_effective_settings
from .orchestration_loader import load_orchestration_file
from ..adapters import build_adapter

console = Console()


def register_doctor(app: typer.Typer) -> None:
    @app.command("doctor", help="System diagnostics — check backends, config, and connectivity.")
    def doctor_cmd(
        config: Optional[Path] = typer.Option(
            None,
            "--config",
            "-c",
            help="Path to config JSON (or use CIRCUITRY_CONFIG).",
        ),
        orchestration: Optional[Path] = typer.Option(
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
