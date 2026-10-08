"""Tests for the ``scripted`` adapter (#362): deterministic replies, no network.

Every run goes through the real ``runtime_shim.run``, the same path a live
run takes, with a :class:`ScriptedAdapter` injected via ``RunRequest.adapter``
— the same seam ``tests/orchestrations/test_decompose_agent.py`` uses.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest
import yaml

from circuitry.adapters.factory import build_adapter
from circuitry.adapters.scripted import ScriptedAdapter
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, RunResult, run


@pytest.fixture(autouse=True)
def _no_credentials(monkeypatch: pytest.MonkeyPatch) -> None:
    for var in ("OPENAI_API_KEY", "ANTHROPIC_API_KEY", "CYBERDINER_TOKEN", "CYBERDINER_EXPO_URL"):
        monkeypatch.delenv(var, raising=False)


def _write_orch(tmp_path: Path, orch: dict[str, Any]) -> Path:
    path = tmp_path / "demo.yml"
    path.write_text(yaml.dump(orch, sort_keys=False), encoding="utf-8")
    return path


def _write_replies(tmp_path: Path, replies: dict[str, Any], name: str = "replies.yaml") -> Path:
    path = tmp_path / name
    path.write_text(yaml.dump(replies, sort_keys=False), encoding="utf-8")
    return path


def _run(orch_path: Path, adapter: ScriptedAdapter, *, initial_state: dict[str, Any] | None = None) -> RunResult:
    return run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state=initial_state,
            adapter=adapter,
            config=CircuitryConfig(default_adapter="scripted", default_model="test"),
            skip_preflight=True,
        )
    )


def _prompt(name: str, **extra: Any) -> dict[str, Any]:
    return {"type": "prompt", "name": name, "template": name, **extra}


def test_tree_dynamic_matches_by_path_not_call_order(tmp_path: Path) -> None:
    orch = {
        "adapter": "scripted",
        "model": "test",
        "flow": "tree",
        "effects": [_prompt("left"), _prompt("right")],
    }
    orch_path = _write_orch(tmp_path, orch)
    replies_path = _write_replies(
        tmp_path,
        {
            "prime.left": [{"text": "left-reply", "tokens_sent": 3, "tokens_received": 1}],
            "prime.right": [{"text": "right-reply", "tokens_sent": 5, "tokens_received": 2}],
        },
    )
    adapter = ScriptedAdapter(replies_file=str(replies_path))

    result = _run(orch_path, adapter)

    assert result.ok, result.error
    assert result.state["prime"]["left"]["value"] == "left-reply"
    assert result.state["prime"]["right"]["value"] == "right-reply"
    totals = result.state["runtime"]["last_run"]["totals"]
    assert totals["tokens_sent"] == 8
    assert totals["tokens_received"] == 3
    assert adapter.leftover_replies() == {}


def test_retries_consume_replies_in_order(tmp_path: Path) -> None:
    orch = {
        "adapter": "scripted",
        "model": "test",
        "effects": [
            _prompt("flaky", retries={"max_attempts": 2, "backoff_ms": 1}),
        ],
    }
    orch_path = _write_orch(tmp_path, orch)
    replies_path = _write_replies(
        tmp_path,
        {
            "prime.flaky": [
                {"error": {"kind": "server_error"}},
                {"text": "recovered", "tokens_sent": 4, "tokens_received": 6},
            ],
        },
    )
    adapter = ScriptedAdapter(replies_file=str(replies_path))

    result = _run(orch_path, adapter)

    assert result.ok, result.error
    node = result.state["prime"]["flaky"]
    assert node["value"] == "recovered"
    assert node["meta"]["retries_used"] == 1
    assert node["meta"]["tokens_sent_total"] == 4
    assert node["meta"]["tokens_received_total"] == 6
    totals = result.state["runtime"]["last_run"]["totals"]
    assert totals["tokens_sent"] == 4
    assert totals["tokens_received"] == 6
    assert adapter.leftover_replies() == {}


def test_not_retryable_error_fails_after_one_attempt(tmp_path: Path) -> None:
    orch = {
        "adapter": "scripted",
        "model": "test",
        "effects": [
            _prompt(
                "flaky",
                retries={"max_attempts": 3, "backoff_ms": 1},
                on_error="continue",
            ),
        ],
    }
    orch_path = _write_orch(tmp_path, orch)
    replies_path = _write_replies(
        tmp_path,
        {
            "prime.flaky": [
                {"error": {"kind": "invalid_request"}},
                {"text": "should never be reached"},
            ],
        },
    )
    adapter = ScriptedAdapter(replies_file=str(replies_path))

    result = _run(orch_path, adapter)

    assert result.ok, result.error
    node = result.state["prime"]["flaky"]
    assert node["value"] is None
    assert "retries_used" not in node["meta"]
    assert adapter.leftover_replies() == {"prime.flaky": 1}


def test_expect_model_mode_reask_consumes_in_order(tmp_path: Path) -> None:
    orch = {
        "adapter": "scripted",
        "model": "test",
        "effects": [
            {
                "type": "tool",
                "name": "calc",
                "provider": "math",
                "params": {"expression": "1 + 1"},
                "retries": {"max_attempts": 2, "backoff_ms": 1},
                "expect": {"mode": "model", "template": "Is {{value}} correct?"},
            },
        ],
    }
    orch_path = _write_orch(tmp_path, orch)
    replies_path = _write_replies(
        tmp_path,
        {
            "prime.calc": [
                {"text": "no"},
                {"text": "yes"},
            ],
        },
    )
    adapter = ScriptedAdapter(replies_file=str(replies_path))

    result = _run(orch_path, adapter)

    assert result.ok, result.error
    node = result.state["prime"]["calc"]
    assert node["value"] == 2
    assert node["meta"]["retries_used"] == 1
    assert node["meta"]["expect"]["result"] is True
    assert adapter.leftover_replies() == {}


def test_if_model_mode_condition_wired(tmp_path: Path) -> None:
    """An `if` effect's own `mode: model` decision dispatches through the
    runtime's model-call identity, keyed at the conditional's own path
    (#370 review F1 — previously ``conditional.py`` called the adapter
    directly, outside any ``model_call``, so ``scripted`` failed naming no
    path at all).
    """
    orch = {
        "adapter": "scripted",
        "model": "test",
        "effects": [
            {
                "type": "if",
                "name": "gate",
                "if": {"mode": "model", "template": "Is 2 > 1?"},
                "then": [_prompt("yes_branch")],
                "else": [_prompt("no_branch")],
            }
        ],
    }
    orch_path = _write_orch(tmp_path, orch)
    replies_path = _write_replies(
        tmp_path,
        {
            "prime.gate": [{"text": "yes"}],
            "prime.gate.yes_branch": [{"text": "yes-run"}],
        },
    )
    adapter = ScriptedAdapter(replies_file=str(replies_path))

    result = _run(orch_path, adapter)

    assert result.ok, result.error
    node = result.state["prime"]["gate"]
    assert node["meta"]["branch"] == "then"
    assert node["meta"]["answer"] == "yes"
    assert node["yes_branch"]["value"] == "yes-run"
    assert adapter.leftover_replies() == {}


def test_while_model_mode_condition_wired(tmp_path: Path) -> None:
    """A loop's own `while: {mode: model}` re-check dispatches through the
    runtime's model-call identity the same way (#370 review F1); each check
    re-asks at the loop's own path, consumed in order like a retry.
    """
    orch = {
        "adapter": "scripted",
        "model": "test",
        "effects": [
            {
                "type": "loop",
                "name": "lp",
                "while": {"mode": "model", "template": "Continue?"},
                "body": [_prompt("step")],
            }
        ],
    }
    orch_path = _write_orch(tmp_path, orch)
    replies_path = _write_replies(
        tmp_path,
        {
            "prime.lp": [{"text": "yes"}, {"text": "no"}],
            "prime.lp.iter_0.step": [{"text": "hi"}],
        },
    )
    adapter = ScriptedAdapter(replies_file=str(replies_path))

    result = _run(orch_path, adapter)

    assert result.ok, result.error
    node = result.state["prime"]["lp"]
    assert node["meta"]["answer"] == "no"
    assert node["iter_0"]["step"]["value"] == "hi"
    assert adapter.leftover_replies() == {}


def test_unmatched_call_raises_naming_path(tmp_path: Path) -> None:
    orch = {
        "adapter": "scripted",
        "model": "test",
        "effects": [_prompt("unscripted")],
    }
    orch_path = _write_orch(tmp_path, orch)
    replies_path = _write_replies(tmp_path, {"prime.other": [{"text": "x"}]})
    adapter = ScriptedAdapter(replies_file=str(replies_path))

    result = _run(orch_path, adapter)

    assert not result.ok
    assert "prime.unscripted" in (result.error or "")


def test_leftover_replies_exposed_without_changing_run_state(tmp_path: Path) -> None:
    orch = {
        "adapter": "scripted",
        "model": "test",
        "effects": [_prompt("only")],
    }
    orch_path = _write_orch(tmp_path, orch)
    replies_path = _write_replies(
        tmp_path,
        {
            "prime.only": [{"text": "used"}, {"text": "never consumed"}],
        },
    )
    adapter = ScriptedAdapter(replies_file=str(replies_path))

    result = _run(orch_path, adapter)

    assert result.ok, result.error
    assert result.state["prime"]["only"]["value"] == "used"
    assert adapter.leftover_replies() == {"prime.only": 1}


def test_named_tree_loop_path_includes_iter_index(tmp_path: Path) -> None:
    orch = {
        "adapter": "scripted",
        "model": "test",
        "effects": [
            {
                "type": "loop",
                "name": "shots",
                "flow": "tree",
                "each": {"in": "input.items", "as": "item"},
                "body": [_prompt("describe", template="{{item}}")],
            }
        ],
    }
    orch_path = _write_orch(tmp_path, orch)
    replies_path = _write_replies(
        tmp_path,
        {
            "prime.shots.iter_0.describe": [{"text": "shot-0"}],
            "prime.shots.iter_1.describe": [{"text": "shot-1"}],
            "prime.shots.iter_2.describe": [{"text": "shot-2"}],
        },
    )
    adapter = ScriptedAdapter(replies_file=str(replies_path))

    result = _run(orch_path, adapter, initial_state={"input": {"items": ["a", "b", "c"]}})

    assert result.ok, result.error
    shots = result.state["prime"]["shots"]
    assert shots["iter_0"]["describe"]["value"] == "shot-0"
    assert shots["iter_1"]["describe"]["value"] == "shot-1"
    assert shots["iter_2"]["describe"]["value"] == "shot-2"
    assert adapter.leftover_replies() == {}


def test_unnamed_tree_loop_path_has_no_none_segment(tmp_path: Path) -> None:
    """An unnamed `flow: tree` loop's body is keyed at the enclosing
    container's own path, not ``<prefix>.None.<body>`` (#370 review F2 —
    previously ``nested_container(store, self.defn.name)`` formatted the
    loop's ``None`` name straight into the path). A single-item collection
    keeps this deterministic: every pass of an *unnamed* tree loop shares
    one path by design, so a multi-item version would not reliably pick a
    particular reply.
    """
    orch = {
        "adapter": "scripted",
        "model": "test",
        "effects": [
            {
                "type": "loop",
                "flow": "tree",
                "each": {"in": "input.items", "as": "item"},
                "body": [_prompt("describe", template="{{item}}")],
            }
        ],
    }
    orch_path = _write_orch(tmp_path, orch)
    replies_path = _write_replies(tmp_path, {"prime.describe": [{"text": "shot"}]})
    adapter = ScriptedAdapter(replies_file=str(replies_path))

    result = _run(orch_path, adapter, initial_state={"input": {"items": ["x"]}})

    assert result.ok, result.error
    assert result.state["prime"]["describe"]["value"] == "shot"
    assert adapter.leftover_replies() == {}


def test_use_children_namespace_identical_prompt_names(tmp_path: Path) -> None:
    child_yaml = yaml.dump({"effects": [_prompt("answer")]}, sort_keys=False)
    orch = {
        "adapter": "scripted",
        "model": "test",
        "flow": "tree",
        "effects": [
            {"type": "use", "name": "first", "inline": child_yaml},
            {"type": "use", "name": "second", "inline": child_yaml},
        ],
    }
    orch_path = _write_orch(tmp_path, orch)
    replies_path = _write_replies(
        tmp_path,
        {
            "prime.first.answer": [{"text": "from-first"}],
            "prime.second.answer": [{"text": "from-second"}],
        },
    )
    adapter = ScriptedAdapter(replies_file=str(replies_path))

    result = _run(orch_path, adapter)

    assert result.ok, result.error
    assert result.state["prime"]["first"]["answer"]["value"] == "from-first"
    assert result.state["prime"]["second"]["answer"]["value"] == "from-second"
    assert adapter.leftover_replies() == {}


def test_check_validates_replies_file(tmp_path: Path) -> None:
    good_path = _write_replies(tmp_path, {"prime.x": [{"text": "ok"}]})
    adapter = ScriptedAdapter(replies_file=str(good_path))
    result = adapter.check()
    assert result.ok

    bad_path = tmp_path / "bad.yaml"
    bad_path.write_text("prime.x:\n  - not-a-mapping\n", encoding="utf-8")
    bad_adapter = ScriptedAdapter(replies_file=str(bad_path))
    bad_result = bad_adapter.check()
    assert not bad_result.ok
    assert "prime.x" in (bad_result.message or "")


def test_unknown_error_kind_rejected_at_load(tmp_path: Path) -> None:
    path = _write_replies(tmp_path, {"prime.x": [{"error": {"kind": "nonsense"}}]})
    adapter = ScriptedAdapter(replies_file=str(path))
    result = adapter.check()
    assert not result.ok
    assert "nonsense" in (result.message or "")


@pytest.mark.parametrize(
    "entry",
    [
        {"text": "ok", "tokens_sent": -1},
        {"text": "ok", "tokens_received": True},
        {"text": "ok", "token_sent": 3},
        {"text": "ok", "error": {"kind": "timeout"}},
        {"error": {"kind": "rate_limited", "status": True}},
        {"error": {"kind": "rate_limited", "status": None}},
        {"error": {"kind": "timeout", "unexpected": 1}},
    ],
    ids=[
        "negative-tokens_sent",
        "bool-tokens_received",
        "unknown-key-typo",
        "both-text-and-error",
        "bool-status",
        "null-status-on-a-status-bearing-kind",
        "unknown-error-key",
    ],
)
def test_check_rejects_malformed_reply_entries(tmp_path: Path, entry: dict[str, Any]) -> None:
    """#370 review F5: a fixture ``check()`` accepts must not then crash
    with an unnamed ``AssertionError`` during the run — booleans, negative
    token counts, unknown keys, a ``text``+``error`` entry and an explicit
    ``status: null`` on a kind that needs one are all rejected at load time.
    """
    path = _write_replies(tmp_path, {"prime.x": [entry]})
    adapter = ScriptedAdapter(replies_file=str(path))
    result = adapter.check()
    assert not result.ok


def test_config_resolves_relative_replies_file_against_cwd(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _write_replies(tmp_path, {"prime.x": [{"text": "ok"}]}, name="relative-replies.yaml")
    monkeypatch.chdir(tmp_path)

    adapter = build_adapter(
        adapter_name="scripted",
        runtime={"adapters": {"scripted": {"replies_file": "relative-replies.yaml"}}},
    )

    assert isinstance(adapter, ScriptedAdapter)
    check_result = adapter.check()
    assert check_result.ok
