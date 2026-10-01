"""`RunRequest.validate_only` returns the same verdict `cof check` gives (#302).

It used to return `ok=True` before the structural check or compile ran at
all, so an API caller asking "is this valid?" was told yes for a document
`cof check` rejects. `run(validate_only=True)` now runs the same checks
`cof check` does by default — structural check, compile, preflight — and
only then stops short of building the adapter or dispatching an effect.
"""

from __future__ import annotations

from pathlib import Path

from circuitry.api import run_orchestration, validate_orchestration
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run, validate


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
    run_result = run_orchestration(
        orchestration_path=path, validate_only=True, raise_on_error=False
    )
    check_result = validate_orchestration(orchestration_path=path)
    assert run_result.ok is False
    assert check_result["ok"] is False
    assert "did you mean 'while'?" in (run_result.error or "")
    assert "did you mean 'while'?" in check_result["errors"][0]


def test_api_validate_orchestration_accepts_config_for_allowlist_preflight(
    tmp_path: Path,
) -> None:
    """The public SDK's `validate_orchestration` had no `config` parameter at
    all, so a caller could never get the allowlist/preflight checks
    `run_orchestration(config=...)` runs for the equivalent real run —
    'valid' and 'runnable' could silently disagree (#265 part 4)."""
    path = _write(
        tmp_path,
        "locked.yml",
        "effects:\n"
        "  - {type: tool, name: t, provider: ffmpeg, params: {}}\n",
    )
    cfg = CircuitryConfig(enabled_tools=[])
    result = validate_orchestration(orchestration_path=path, config=cfg)
    assert result["ok"] is False
    assert any("ffmpeg" in e for e in result["errors"])


def test_validate_only_does_not_require_a_resolved_adapter_cof_check_allows(
    tmp_path: Path,
) -> None:
    """Neither `cof check` nor `validate_only` requires a buildable adapter
    to say ok — that's a build-time concern, checked again right before the
    adapter is actually built for a real run."""
    path = _write(
        tmp_path,
        "valid.yml",
        "effects:\n  - {type: prompt, name: greet, template: 'hi'}\n",
    )
    check_result = validate(path, config=CircuitryConfig())
    assert check_result["ok"] is True, check_result["errors"]

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


def test_validate_only_rejects_a_use_cycle(tmp_path: Path) -> None:
    a_path = _write(
        tmp_path,
        "a.yml",
        "effects:\n"
        "  - {type: use, name: call_b, path: " + repr(str(tmp_path / "b.yml")) + "}\n",
    )
    _write(
        tmp_path,
        "b.yml",
        "effects:\n"
        "  - {type: use, name: call_a, path: " + repr(str(a_path)) + "}\n",
    )
    result = run(
        RunRequest(
            orchestration_path=a_path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=True,
            config=CircuitryConfig(),
        )
    )
    assert result.ok is False
    assert "cycle" in (result.error or "").lower()
    assert "a.yml" in (result.error or "") and "b.yml" in (result.error or "")
