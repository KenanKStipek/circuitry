from __future__ import annotations

import json
from pathlib import Path

from circuitry import run_shared_orchestration
from circuitry.cli.config import CircuitryConfig


def _write(path: Path, content: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")


def test_run_shared_orchestration_embedded_api_supports_service_profile(
    tmp_path: Path,
) -> None:
    lib_root = tmp_path / "library"
    _write(
        lib_root / "welcome" / "1.0.0.yml",
        (
            "effects:\n"
            "  - type: prompt\n"
            "    name: greet\n"
            '    template: "hello {{input.name}}"\n'
        ),
    )

    cfg = CircuitryConfig(
        runtime={
            "library": {
                "backend": "filesystem",
                "local_root": str(lib_root),
                "service_profiles": {
                    "svc-a": {
                        "default_adapter": "openai",
                        "default_model": "gpt-4o-mini",
                    }
                },
            }
        }
    )

    result = run_shared_orchestration(
        asset_id="welcome",
        version="1.0.0",
        config=cfg,
        service_profile="svc-a",
        state={"input": {"name": "Elena"}},
        dry_run=True,
    )

    assert result.ok is True
    assert result.state["runtime"]["shared_library"]["asset_id"] == "welcome"
    assert result.state["runtime"]["shared_library"]["service_profile"] == "svc-a"
    assert result.state["runtime"]["effective_settings"]["adapter"] == "openai"
    assert result.state["prime"]["greet"]["meta"]["prompt_sent"] == "hello Elena"


def test_run_shared_orchestration_out_path_writes_the_state_file(tmp_path: Path) -> None:
    """Same root cause as `run_orchestration(out_path=...)` (#265): `run()`
    never writes `out_path` itself."""
    lib_root = tmp_path / "library"
    _write(
        lib_root / "welcome" / "1.0.0.yml",
        (
            "effects:\n"
            "  - type: prompt\n"
            "    name: greet\n"
            '    template: "hello {{input.name}}"\n'
        ),
    )
    cfg = CircuitryConfig(
        default_adapter="ollama",
        default_model="phi3:mini",
        runtime={"library": {"backend": "filesystem", "local_root": str(lib_root)}},
    )
    out_path = tmp_path / "state.json"

    result = run_shared_orchestration(
        asset_id="welcome",
        version="1.0.0",
        config=cfg,
        out_path=out_path,
        state={"input": {"name": "Elena"}},
        dry_run=True,
    )

    assert result.ok is True
    assert out_path.exists()
    written = json.loads(out_path.read_text(encoding="utf-8"))
    assert "prime" in written
