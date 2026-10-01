"""`cof run` applies `cof check`'s structural gate before anything runs (#261).

The schema and the near-miss key check used to run only in `validate()`, so a
document `cof check` rejects could still start, run its first effects, and
fail partway. `run()` and a `use` effect loading a `path:`/`ref:` child now
apply the same check up front.
"""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path

from circuitry.adapters.base import GenerateResult
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, RunResult, run

FIRST_EFFECT = """\
  - type: tool
    name: first
    provider: json
    params: {mode: stringify, input: "ran"}
"""


def _write(path: Path, content: str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    return path


@dataclass(frozen=True)
class StubAdapter:
    """A `use` makes the run build an adapter; nothing here calls it."""

    name: str = "stub"

    def generate(self, *, model: str, prompt: str, timeout_seconds: int = 120) -> GenerateResult:
        raise AssertionError("no prompt effect should run")


def _run(path: Path, config: CircuitryConfig | None = None) -> RunResult:
    return run(
        RunRequest(
            orchestration_path=path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=config or CircuitryConfig(),
            skip_preflight=True,
            adapter=StubAdapter(),
        )
    )


def test_schema_invalid_document_fails_before_its_first_effect(tmp_path: Path) -> None:
    # The issue's case: max_concurrency: 0 passed nothing at run time until the
    # loop itself crashed — after earlier effects had already run.
    path = _write(
        tmp_path / "doc.yml",
        "effects:\n"
        + FIRST_EFFECT
        + "  - type: loop\n"
        "    name: fan\n"
        "    each: {in: input.xs}\n"
        "    flow: tree\n"
        "    max_concurrency: 0\n"
        "    body: [{type: tool, name: t, provider: json, params: {input: '1'}}]\n",
    )
    result = _run(path)
    assert result.ok is False
    assert result.error is not None
    assert result.error.startswith("Orchestration validation failed:")
    assert "effects[1].max_concurrency: 0 is less than the minimum of 1" in result.error
    assert "prime" not in result.state


def test_near_miss_key_fails_the_run(tmp_path: Path) -> None:
    # The issue's case: `whlie:` ran green with zero passes.
    path = _write(
        tmp_path / "doc.yml",
        "effects:\n"
        + FIRST_EFFECT
        + "  - type: loop\n"
        "    name: poll\n"
        "    whlie: {mode: cel, expr: 'true'}\n"
        "    body: [{type: tool, name: t, provider: json, params: {input: '1'}}]\n",
    )
    result = _run(path)
    assert result.ok is False
    assert "unknown key 'whlie' on a 'loop' effect — did you mean 'while'?" in (
        result.error or ""
    )
    assert "prime" not in result.state


def test_valid_document_still_runs(tmp_path: Path) -> None:
    path = _write(tmp_path / "doc.yml", "effects:\n" + FIRST_EFFECT)
    result = _run(path)
    assert result.ok is True, result.error
    assert result.state["prime"]["first"]["value"] == '"ran"'


def _parent_using(tmp_path: Path, *, validate: bool = True) -> Path:
    flag = "" if validate else "    validate: false\n"
    return _write(
        tmp_path / "parent.yml",
        "effects:\n"
        "  - type: use\n"
        "    name: child\n"
        "    path: child.yml\n"
        "    on_error: continue\n" + flag,
    )


def test_path_child_is_schema_validated_when_loaded(tmp_path: Path) -> None:
    _write(
        tmp_path / "child.yml",
        "effects:\n" + FIRST_EFFECT + "    timeout_ms: -1\n",
    )
    result = _run(_parent_using(tmp_path))
    assert result.ok is True, result.error
    meta = result.state["prime"]["child"]["meta"]
    assert "child.yml validation failed" in meta["error"]
    assert "effects[0].timeout_ms: -1 is less than the minimum of 0" in meta["error"]
    assert meta["validation_errors"] == meta["error"]
    # Nothing in the child ran.
    assert "first" not in result.state["prime"]["child"]


def test_path_child_validation_respects_validate_false(tmp_path: Path) -> None:
    _write(
        tmp_path / "child.yml",
        "effects:\n" + FIRST_EFFECT + "    timeout_ms: -1\n",
    )
    result = _run(_parent_using(tmp_path, validate=False))
    assert result.ok is True, result.error
    assert result.state["prime"]["child"]["meta"]["error"] is None


def test_path_child_with_duplicate_key_fails_to_load(tmp_path: Path) -> None:
    _write(
        tmp_path / "child.yml",
        "effects:\n" + FIRST_EFFECT + "    name: second\n",
    )
    result = _run(_parent_using(tmp_path, validate=False))
    assert result.ok is True, result.error
    error = result.state["prime"]["child"]["meta"]["error"]
    assert "child.yml: duplicate key 'name' at line 6" in error


def test_ref_child_is_schema_validated_when_loaded(tmp_path: Path) -> None:
    _write(
        tmp_path / "local" / "helpers" / "child.yml",
        "effects:\n" + FIRST_EFFECT + "    timeout_ms: -1\n",
    )
    parent = _write(
        tmp_path / "parent.yml",
        "effects:\n"
        "  - type: use\n"
        "    name: child\n"
        "    ref: local:helpers/child\n"
        "    on_error: continue\n",
    )
    config = CircuitryConfig(
        runtime={
            "library": {
                "sources": [{"type": "folder", "name": "local", "path": str(tmp_path / "local")}]
            }
        }
    )
    result = _run(parent, config)
    assert result.ok is True, result.error
    meta = result.state["prime"]["child"]["meta"]
    assert "child.yml validation failed" in meta["error"]
    assert "effects[0].timeout_ms: -1 is less than the minimum of 0" in meta["error"]
    assert "first" not in result.state["prime"]["child"]
