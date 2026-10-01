"""`RunRequest.validate_only` returns the same verdict `cof check` gives (#302).

It used to return `ok=True` before the structural check or compile ran at
all, so an API caller asking "is this valid?" was told yes for a document
`cof check` rejects. `run(validate_only=True)` now runs the same checks
`cof check` does by default — structural check, compile, preflight — and
only then stops short of building the adapter or dispatching an effect.
"""

from __future__ import annotations

from pathlib import Path

from circuitry.api import run_orchestration
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run


def _write(tmp_path: Path, name: str, content: str) -> Path:
    path = tmp_path / name
    path.write_text(content, encoding="utf-8")
    return path


def test_validate_only_rejects_a_near_miss_key(tmp_path: Path) -> None:
    path = _write(
        tmp_path,
        "invalid.yml",
        "effects:\n"
        "  - type: loop\n"
        "    whlie: {mode: cel, expr: 'true'}\n"
        "    body: [{type: tool, name: t, provider: json, params: {input: '1'}}]\n",
    )
    result = run(
        RunRequest(
            orchestration_path=path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=True,
            config=CircuitryConfig(),
        )
    )
    assert result.ok is False
    assert "did you mean 'while'?" in (result.error or "")


def test_validate_only_rejects_a_schema_violation(tmp_path: Path) -> None:
    path = _write(
        tmp_path,
        "invalid.yml",
        "effects:\n"
        "  - type: loop\n"
        "    name: fan\n"
        "    each: {in: input.xs}\n"
        "    max_concurrency: 0\n"
        "    body: [{type: tool, name: t, provider: json, params: {input: '1'}}]\n",
    )
    result = run(
        RunRequest(
            orchestration_path=path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=True,
            config=CircuitryConfig(),
        )
    )
    assert result.ok is False
    assert "max_concurrency: 0 is less than the minimum of 1" in (result.error or "")


def test_validate_only_passes_a_valid_document_without_executing(tmp_path: Path) -> None:
    path = _write(
        tmp_path,
        "valid.yml",
        "effects:\n"
        "  - {type: tool, name: a, provider: json, params: {mode: stringify, input: 'ran'}}\n",
    )
    result = run(
        RunRequest(
            orchestration_path=path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=True,
            config=CircuitryConfig(),
        )
    )
    assert result.ok is True, result.error
    # No effect actually dispatched.
    assert "a" not in result.state.get("prime", {})


def test_api_validate_only_matches_api_validate_orchestration(tmp_path: Path) -> None:
    path = _write(
        tmp_path,
        "invalid.yml",
        "effects:\n"
        "  - type: loop\n"
        "    whlie: {mode: cel, expr: 'true'}\n"
        "    body: [{type: tool, name: t, provider: json, params: {input: '1'}}]\n",
    )
    result = run_orchestration(
        orchestration_path=path, validate_only=True, raise_on_error=False
    )
    assert result.ok is False
    assert "did you mean 'while'?" in (result.error or "")
