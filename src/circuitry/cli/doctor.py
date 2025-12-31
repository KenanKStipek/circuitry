from __future__ import annotations

from pathlib import Path
from typing import Optional

import typer
from rich.console import Console
from rich.table import Table

from .config import find_config_path, load_config
from .orchestration_loader import load_orchestration_file
from .effective_settings import resolve_effective_settings
from ..adapters import build_adapter

console = Console()


def register_doctor(app: typer.Typer) -> None:
    @app.command("doctor")
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
            help="Optional orchestration YAML to include in effective settings.",
        ),
        generate: bool = typer.Option(
            False, "--generate", help="Also run a tiny generate call."
        ),
    ) -> None:
        cfg_path = find_config_path(explicit_path=config)
        cfg = load_config(cfg_path)

        orch_obj = {}
        if orchestration:
            orch_obj = load_orchestration_file(orchestration)

        effective = resolve_effective_settings(cfg=cfg, orch=orch_obj)

        table = Table(title="Circuitry · Doctor", show_lines=True)
        table.add_column("Check")
        table.add_column("Result")

        table.add_row("Config path", str(cfg_path) if cfg_path else "— (defaults)")
        table.add_row(
            "Effective adapter",
            f"{effective.adapter} (source: {effective.sources.get('adapter')})",
        )
        table.add_row(
            "Effective model",
            f"{effective.model} (source: {effective.sources.get('model')})",
        )

        adapter = build_adapter(
            adapter_name=effective.adapter or "", runtime=effective.runtime or {}
        )

        # tags / list models
        try:
            tags = adapter.list_models(timeout_seconds=5)  # type: ignore[attr-defined]
            names = [
                m.get("name") for m in (tags.get("models") or []) if isinstance(m, dict)
            ]
            table.add_row("Ollama /api/tags", f"OK ({len(names)} models)")
            if effective.model and effective.model not in names:
                table.add_row("Model present", f"NO (missing: {effective.model})")
            else:
                table.add_row("Model present", "YES")
        except Exception as e:
            table.add_row("Ollama /api/tags", f"FAIL ({e})")
            console.print(table)
            raise typer.Exit(code=1)

        if generate:
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
                table.add_row("Ollama /api/generate", f"OK ({res.text})")
            except Exception as e:
                table.add_row("Ollama /api/generate", f"FAIL ({e})")
                console.print(table)
                raise typer.Exit(code=1)

        console.print(table)
