"""``type: report`` — compile-time checks and runtime behaviour (#605)."""

from __future__ import annotations

import logging
from pathlib import Path
from typing import Any

import pytest

from circuitry.cli.runtime_shim import validate
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store


def _validate(tmp_path: Path, text: str) -> dict:
    path = tmp_path / "doc.yml"
    path.write_text(text, encoding="utf-8")
    return validate(path)


def _run(
    orch: dict[str, Any],
    *,
    initial_state: dict[str, Any] | None = None,
    effect_observers: list[tuple[str, dict[str, Any]]] | None = None,
    report_calls: list[tuple[str, str, str, Any]] | None = None,
) -> dict[str, Any]:
    root = compile_orchestration(orch=orch, root_name="prime")
    store = Store(
        dict(initial_state or {}),
        effect_complete=(
            (lambda path, node: effect_observers.append((path, node)))
            if effect_observers is not None
            else None
        ),
        report=(
            (lambda path, level, message, data: report_calls.append((path, level, message, data)))
            if report_calls is not None
            else None
        ),
    )
    DynamicRuntime(root, adapter=None, model="run-default").execute(store=store)
    return store.state


# ── compile-time ─────────────────────────────────────────────────────────────


def test_compile_report_requires_message() -> None:
    orch = {"effects": [{"type": "report"}]}
    with pytest.raises(ValueError, match="message"):
        compile_orchestration(orch=orch, root_name="prime")


def test_compile_report_rejects_invalid_level() -> None:
    orch = {"effects": [{"type": "report", "message": "hi", "level": "debug"}]}
    with pytest.raises(ValueError, match="level"):
        compile_orchestration(orch=orch, root_name="prime")


def test_compile_report_name_is_optional() -> None:
    orch = {"effects": [{"type": "report", "message": "hi"}]}
    root = compile_orchestration(orch=orch, root_name="prime")
    report = root.effects[0]
    assert report.name is None
    assert report.message == "hi"
    assert report.level == "info"


def test_compile_report_rejects_malformed_message_template() -> None:
    orch = {"effects": [{"type": "report", "message": "{{unclosed"}]}
    with pytest.raises(ValueError, match="malformed Mustache"):
        compile_orchestration(orch=orch, root_name="prime")


def test_compile_report_data_from_rejects_unrooted_path() -> None:
    orch = {
        "effects": [
            {
                "type": "report",
                "message": "hi",
                "data": {"x": {"from": "bogus"}},
            }
        ]
    }
    with pytest.raises(ValueError, match="not rooted"):
        compile_orchestration(orch=orch, root_name="prime")


@pytest.mark.parametrize("key, value", [
    ("on_error", "skip"),
    ("retries", {"max_attempts": 3}),
    ("timeout_ms", 1000),
    ("expect", "true"),
])
def test_check_rejects_model_or_tool_only_keys(tmp_path: Path, key: str, value: object) -> None:
    """None of these are report keys, or near misses of one -- the schema
    (additionalProperties: false) rejects the document, and the unknown-key
    scan separately names each as ignored (#605)."""
    import yaml

    doc = {"effects": [{"type": "report", "message": "hi", key: value}]}
    result = _validate(tmp_path, yaml.dump(doc))
    assert result["ok"] is False
    assert any(key in e for e in result["errors"])
    assert any(key in w and "is ignored" in w for w in result["warnings"])


def test_check_rejects_any_other_unknown_key_too(tmp_path: Path) -> None:
    """A report's 'additionalProperties: false' (like yield's) rejects even
    a key related to no other effect type at all -- not just the model/tool
    keys above."""
    result = _validate(
        tmp_path,
        "effects:\n"
        "  - type: report\n"
        "    message: hi\n"
        "    owner: ops\n",
    )
    assert result["ok"] is False
    assert any("owner" in w and "is ignored" in w for w in result["warnings"])


def test_check_accepts_a_minimal_named_report(tmp_path: Path) -> None:
    result = _validate(
        tmp_path,
        "effects:\n"
        "  - type: report\n"
        "    name: status\n"
        "    level: warning\n"
        "    message: 'Found {{prime.x.value}} issues'\n"
        "    data:\n"
        "      found: {from: prime.x.value}\n",
    )
    assert result["ok"] is True, result.get("errors")


# ── runtime ──────────────────────────────────────────────────────────────────


def test_named_report_writes_state_node() -> None:
    orch = {
        "effects": [
            {
                "type": "report",
                "name": "status",
                "level": "warning",
                "message": "found {{input.n}} issues",
                "data": {"n": {"from": "input.n"}, "literal": 3, "text": "hi {{input.n}}"},
            }
        ]
    }
    state = _run(orch, initial_state={"input": {"n": 2}})
    node = state["prime"]["status"]
    assert node["value"] == {
        "message": "found 2 issues",
        "level": "warning",
        "data": {"n": 2, "literal": 3, "text": "hi 2"},
    }
    assert node["meta"]["created_at"]
    assert node["meta"]["completed_at"]


def test_unnamed_report_writes_no_node() -> None:
    orch = {"effects": [{"type": "report", "message": "hi"}]}
    state = _run(orch)
    assert set(state["prime"]) == {"value", "meta"}


def test_report_fires_report_hook_not_effect_hooks() -> None:
    orch = {"effects": [{"type": "report", "name": "status", "message": "hi"}]}
    effect_calls: list[tuple[str, dict[str, Any]]] = []
    report_calls: list[tuple[str, str, str, Any]] = []
    _run(orch, effect_observers=effect_calls, report_calls=report_calls)
    assert report_calls == [("prime.status", "info", "hi", None)]
    # The enclosing (unnamed) root dynamic still fires its own
    # effect_complete for itself -- only the report leaf must not.
    assert all(path != "prime.status" for path, _ in effect_calls)


def test_unnamed_report_uses_container_path() -> None:
    orch = {
        "effects": [
            {
                "type": "dynamic",
                "name": "phase",
                "effects": [{"type": "report", "message": "working"}],
            }
        ]
    }
    report_calls: list[tuple[str, str, str, Any]] = []
    _run(orch, report_calls=report_calls)
    assert report_calls == [("prime.phase", "info", "working", None)]


def test_report_missing_from_path_resolves_to_null_with_warning(
    caplog: pytest.LogCaptureFixture,
) -> None:
    orch = {
        "effects": [
            {
                "type": "report",
                "name": "status",
                "message": "hi",
                "data": {"missing": {"from": "input.nope"}},
            }
        ]
    }
    with caplog.at_level(logging.WARNING):
        state = _run(orch)
    assert state["prime"]["status"]["value"]["data"] == {"missing": None}
    assert any("did not resolve" in r.message for r in caplog.records)


def test_report_rendering_failure_never_fails_the_run(monkeypatch: pytest.MonkeyPatch) -> None:
    import circuitry.core.report as report_mod

    def _boom(*args: Any, **kwargs: Any) -> str:
        raise RuntimeError("boom")

    monkeypatch.setattr(report_mod, "render_with_composition", _boom)
    orch = {"effects": [{"type": "report", "name": "status", "message": "hi"}]}
    report_calls: list[tuple[str, str, str, Any]] = []
    state = _run(orch, report_calls=report_calls)  # must not raise
    assert state["prime"]["status"]["value"] == {"message": "", "level": "info", "data": None}
    assert report_calls == [("prime.status", "info", "", None)]


def test_report_not_counted_toward_effects_run(tmp_path: Path) -> None:
    from circuitry.cli.runtime_shim import RunRequest, run

    orch_path = tmp_path / "doc.yml"
    orch_path.write_text(
        "effects:\n"
        "  - type: report\n"
        "    message: hi\n"
        "  - type: yield\n"
        "    name: noted\n"
        "    template: text\n",
        encoding="utf-8",
    )
    req = RunRequest(
        orchestration_path=orch_path,
        state_path=None,
        out_path=None,
        dry_run=False,
        validate_only=False,
    )
    result = run(req)
    assert result.ok, result.warnings
    totals = result.state["runtime"]["last_run"]["totals"]
    # prime (dynamic) + yield only — the report does not count.
    assert totals["effects_run"] == 2


def test_resume_skips_a_completed_named_report() -> None:
    orch = {"effects": [{"type": "report", "name": "status", "message": "second {{input.n}}"}]}
    root = compile_orchestration(orch=orch, root_name="prime")
    prior_state = {
        "prime": {
            "value": True,
            "meta": {"created_at": "x", "completed_at": "x", "error": None},
            "status": {
                "value": {"message": "first", "level": "info", "data": None},
                "meta": {"created_at": "x", "completed_at": "x"},
            },
        }
    }
    store = Store(prior_state)
    DynamicRuntime(root, adapter=None, model="m", resume=True).execute(store=store)
    assert store.state["prime"]["status"]["value"]["message"] == "first"
