from __future__ import annotations

from pathlib import Path

import circuitry
import circuitry.adapters as adapters

README_PATH = Path("README.md")
API_REFERENCE_PATH = Path("docs/api-reference.md")


def _read(path: Path) -> str:
    return path.read_text(encoding="utf-8")


def test_readme_quick_start_includes_dry_run_and_verification() -> None:
    readme = _read(README_PATH)
    assert "circuitry run orchestrations/hello.yml --dry-run" in readme
    assert "Quick verification checklist:" in readme
    assert "result.ok == True" in readme


def test_api_reference_symbols_match_public_exports() -> None:
    api_doc = _read(API_REFERENCE_PATH)

    top_level_symbols = [
        "CircuitryExecutionError",
        "inspect_divergence_paths",
        "inspect_orchestration",
        "run_orchestration",
        "run_shared_orchestration",
        "validate_orchestration",
    ]
    for symbol in top_level_symbols:
        assert hasattr(circuitry, symbol), f"Missing top-level symbol: {symbol}"
        assert f"`{symbol}`" in api_doc, f"API doc missing symbol: {symbol}"

    assert hasattr(adapters, "build_adapter")
    assert "`build_adapter`" in api_doc
