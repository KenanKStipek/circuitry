"""cof setup — interactive onboarding wizard."""

from __future__ import annotations

import json
from pathlib import Path

import typer
from rich.console import Console
from rich.panel import Panel
from rich.table import Table

from .config import GLOBAL_CONFIG_DIR, GLOBAL_CONFIG_PATH
from .detect import DetectionResult, detect_all
from .registry import load_index

console = Console()


def _detect_urls_from_existing_config() -> tuple[str, str]:
    """Read existing config (if any) to get non-default URLs for probing."""
    ollama_url = "http://localhost:11434"
    comfyui_url = "http://localhost:8188"
    if GLOBAL_CONFIG_PATH.exists():
        try:
            raw = json.loads(GLOBAL_CONFIG_PATH.read_text(encoding="utf-8"))
            rt = raw.get("runtime", {})
            ollama_url = rt.get("adapters", {}).get("ollama", {}).get("base_url", ollama_url)
            comfyui_url = rt.get("plugins", {}).get("comfyui", {}).get("base_url", comfyui_url)
        except Exception:
            pass
    return ollama_url, comfyui_url


def _print_detection(result: DetectionResult) -> None:
    """Print backend detection results as a Rich table."""
    table = Table(title="Backend Detection", show_header=True, header_style="bold cyan")
    table.add_column("Backend")
    table.add_column("Status", justify="center")
    table.add_column("Details")

    for b in result.backends:
        status = "[green]available[/green]" if b.available else "[red]not found[/red]"
        table.add_row(b.name, status, b.detail)

    console.print(table)


def _print_models(result: DetectionResult) -> None:
    """Print discovered models grouped by backend."""
    has_models = any(b.models for b in result.backends if b.available)
    if not has_models:
        return

    console.print()
    console.print("[bold]Available models:[/bold]")
    for b in result.backends:
        if b.available and b.models:
            models_str = ", ".join(b.models[:10])
            if len(b.models) > 10:
                models_str += f" (+{len(b.models) - 10} more)"
            console.print(f"  [cyan]{b.name}:[/cyan] {models_str}")


def _print_capability_match(result: DetectionResult) -> None:
    """Cross-reference detected backends against bundled orchestration requirements."""
    entries = load_index()
    if not entries:
        return

    available = result.available_names
    # Map 'llm' to any LLM backend being available
    if available & {"ollama", "openai", "anthropic"}:
        available.add("llm")

    console.print()
    table = Table(title="Orchestration Availability", show_header=True, header_style="bold cyan")
    table.add_column("Orchestration", style="bold")
    table.add_column("Runnable", justify="center")
    table.add_column("Missing")

    # Only show non-template orchestrations
    runnable_entries = [e for e in entries if e.get("category") != "template"]

    for entry in runnable_entries:
        required = set(entry.get("backends", []))
        missing = required - available
        if not missing:
            runnable = "[green]yes[/green]"
            missing_str = ""
        else:
            runnable = "[red]no[/red]"
            missing_str = ", ".join(sorted(missing))
        table.add_row(entry["name"], runnable, missing_str)

    console.print(table)


def _pick_adapter_and_model(result: DetectionResult) -> tuple[str, str]:
    """Interactive selection of adapter and model based on what was detected."""
    llm_backends = [b for b in result.backends if b.name in ("ollama", "openai", "anthropic") and b.available]

    if not llm_backends:
        console.print("[yellow]No LLM backend detected.[/yellow]")
        console.print("Install Ollama (https://ollama.com) or set OPENAI_API_KEY / ANTHROPIC_API_KEY.")
        adapter = typer.prompt("Adapter (ollama/openai/anthropic)", default="ollama")
        model = typer.prompt("Model", default="llama3:latest")
        return adapter, model

    if len(llm_backends) == 1:
        backend = llm_backends[0]
        console.print(f"\n[bold]Detected LLM backend:[/bold] {backend.name}")
        default_model = backend.models[0] if backend.models else "llama3:latest"
    else:
        console.print(f"\n[bold]Detected LLM backends:[/bold] {', '.join(b.name for b in llm_backends)}")
        choices = [b.name for b in llm_backends]
        backend_name = typer.prompt(
            f"Which adapter? ({'/'.join(choices)})",
            default=choices[0],
        )
        backend = next((b for b in llm_backends if b.name == backend_name), llm_backends[0])
        default_model = backend.models[0] if backend.models else "llama3:latest"

    if backend.models:
        console.print(f"[dim]Models: {', '.join(backend.models[:8])}[/dim]")

    model = typer.prompt("Model", default=default_model)
    return backend.name, model


def _build_config(
    adapter: str,
    model: str,
    result: DetectionResult,
) -> dict:
    """Build a config dict from wizard selections and detection results."""
    config: dict = {
        "default_model": model,
        "default_adapter": adapter,
        "runtime": {
            "adapters": {},
        },
    }

    # Adapter URL
    ollama = result.get("ollama")
    if adapter == "ollama" and ollama and ollama.available:
        url = ollama.detail.split(" ")[0] if ollama.detail else "http://localhost:11434"
        config["runtime"]["adapters"]["ollama"] = {
            "base_url": url,
            "timeout_seconds": 6000,
        }
    elif adapter == "ollama":
        url = typer.prompt("Ollama URL", default="http://localhost:11434")
        config["runtime"]["adapters"]["ollama"] = {
            "base_url": url,
            "timeout_seconds": 6000,
        }

    # ComfyUI
    comfyui = result.get("comfyui")
    if comfyui and comfyui.available:
        config["runtime"]["plugins"] = {
            "comfyui": {
                "base_url": comfyui.detail,
                "default_image_output": "path",
                "image_dir": "./output/images",
            }
        }

    return config


def _write_config(config: dict) -> Path:
    """Write config to the global config directory."""
    GLOBAL_CONFIG_DIR.mkdir(parents=True, exist_ok=True)
    GLOBAL_CONFIG_PATH.write_text(
        json.dumps(config, indent=2) + "\n", encoding="utf-8"
    )
    return GLOBAL_CONFIG_PATH


def _write_env_file(result: DetectionResult) -> Path | None:
    """Interactively create a .env file for API keys if needed."""
    env_path = GLOBAL_CONFIG_DIR / ".env"
    lines: list[str] = []

    openai = result.get("openai")
    anthropic = result.get("anthropic")

    # Only prompt for keys that aren't already set
    if not (openai and openai.available) and typer.confirm(
        "Set up OpenAI API key?", default=False
    ):
        key = typer.prompt("OPENAI_API_KEY")
        lines.append(f"OPENAI_API_KEY={key}")

    if not (anthropic and anthropic.available) and typer.confirm(
        "Set up Anthropic API key?", default=False
    ):
        key = typer.prompt("ANTHROPIC_API_KEY")
        lines.append(f"ANTHROPIC_API_KEY={key}")

    if not lines:
        return None

    GLOBAL_CONFIG_DIR.mkdir(parents=True, exist_ok=True)
    # Append to existing .env if present
    if env_path.exists():
        existing = env_path.read_text(encoding="utf-8")
        lines = [existing.rstrip(), *lines]

    env_path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return env_path


def register_setup(app: typer.Typer) -> None:
    @app.command("setup", help="Interactive setup wizard — detect backends, configure model, and create config.")
    def setup_cmd(
        json_out: bool = typer.Option(
            False, "--json", help="Output detection results as JSON (non-interactive)."
        ),
    ) -> None:
        console.print(Panel.fit("Circuitry · Setup", border_style="cyan"))
        console.print()

        # Detect
        ollama_url, comfyui_url = _detect_urls_from_existing_config()
        with console.status("[cyan]Detecting backends…[/cyan]"):
            result = detect_all(ollama_url=ollama_url, comfyui_url=comfyui_url)

        if json_out:
            data = [
                {"name": b.name, "available": b.available, "detail": b.detail, "models": b.models}
                for b in result.backends
            ]
            import json as _json
            console.print_json(_json.dumps(data))
            return

        _print_detection(result)
        _print_models(result)
        _print_capability_match(result)

        # Config
        console.print()
        existing = GLOBAL_CONFIG_PATH.exists()
        if existing:
            console.print(f"[dim]Existing config: {GLOBAL_CONFIG_PATH}[/dim]")
            if not typer.confirm("Overwrite config?", default=True):
                console.print("[dim]Keeping existing config.[/dim]")
                return

        adapter, model = _pick_adapter_and_model(result)
        config = _build_config(adapter, model, result)
        config_path = _write_config(config)
        console.print(f"[green]Config written:[/green] {config_path}")

        # .env
        env_path = _write_env_file(result)
        if env_path:
            console.print(f"[green]Env file written:[/green] {env_path}")

        # Next steps
        console.print()
        console.print(Panel.fit(
            "[bold]You're all set![/bold]\n\n"
            "  cof list                         Browse orchestrations\n"
            "  cof run hello -e name=World      Try it!\n"
            "  cof doctor                       Verify connectivity\n",
            border_style="green",
        ))
