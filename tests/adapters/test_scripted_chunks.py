"""Tests for the ``scripted`` adapter's ``chunks`` key (#674): a reply may
deliver its text as an ordered list of deltas, each one reported through
``GenerateOptions.on_progress`` before the reply completes, with the final
state unchanged from the same reply without ``chunks``.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest
import yaml

from circuitry.adapters._retry import AdapterCallError
from circuitry.adapters.base import GenerateOptions, GenerateProgress
from circuitry.adapters.scripted import (
    ScriptedAdapter,
    ScriptedRepliesError,
    total_replies_by_path,
)
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, RunResult, run
from circuitry.core.effect_identity import model_call
from circuitry.core.store import Store


def _write_replies(tmp_path: Path, replies: dict[str, Any], name: str = "replies.yaml") -> Path:
    path = tmp_path / name
    path.write_text(yaml.dump(replies, sort_keys=False), encoding="utf-8")
    return path


def test_chunks_delivered_as_deltas_before_the_reply_completes(tmp_path: Path) -> None:
    replies_path = _write_replies(
        tmp_path,
        {"greeting": [{"chunks": ["hello ", "from ", "scripted"]}]},
    )
    adapter = ScriptedAdapter(replies_file=str(replies_path))
    deltas: list[GenerateProgress] = []
    store = Store({})

    with model_call(store, "greeting"):
        result = adapter.generate(
            model="test",
            prompt="say hello",
            options=GenerateOptions(on_progress=deltas.append),
        )

    assert [d.delta_chars for d in deltas] == [6, 5, 8]
    assert all(d.delta_count == 1 for d in deltas)
    assert result.text == "hello from scripted"


def test_text_defaults_to_the_concatenation_of_chunks(tmp_path: Path) -> None:
    replies_path = _write_replies(
        tmp_path,
        {"greeting": [{"chunks": ["a", "b"], "tokens_sent": 1, "tokens_received": 1}]},
    )
    adapter = ScriptedAdapter(replies_file=str(replies_path))
    store = Store({})

    with model_call(store, "greeting"):
        result = adapter.generate(model="test", prompt="x")

    assert result.text == "ab"


def test_text_mismatched_with_chunks_is_a_load_error(tmp_path: Path) -> None:
    replies_path = _write_replies(
        tmp_path,
        {"greeting": [{"text": "nope", "chunks": ["a", "b"]}]},
    )
    with pytest.raises(ScriptedRepliesError, match="does not equal"):
        total_replies_by_path(replies_path)


def test_chunks_must_be_a_non_empty_list_of_strings(tmp_path: Path) -> None:
    replies_path = _write_replies(tmp_path, {"greeting": [{"chunks": []}]})
    with pytest.raises(ScriptedRepliesError, match="non-empty list"):
        total_replies_by_path(replies_path)

    replies_path2 = _write_replies(
        tmp_path, {"greeting": [{"chunks": [1, 2]}]}, name="replies2.yaml"
    )
    with pytest.raises(ScriptedRepliesError, match="non-empty list"):
        total_replies_by_path(replies_path2)


def test_chunks_error_reply_never_calls_on_progress(tmp_path: Path) -> None:
    replies_path = _write_replies(
        tmp_path, {"greeting": [{"error": {"kind": "server_error"}}]}
    )
    adapter = ScriptedAdapter(replies_file=str(replies_path))
    deltas: list[GenerateProgress] = []
    store = Store({})

    with model_call(store, "greeting"), pytest.raises(AdapterCallError):
        adapter.generate(
            model="test", prompt="x", options=GenerateOptions(on_progress=deltas.append)
        )

    assert deltas == []


def _run_greeting(tmp_path: Path, name: str, reply: dict[str, Any]) -> RunResult:
    orch_path = tmp_path / f"{name}.yml"
    orch_path.write_text(
        yaml.dump(
            {
                "adapter": "scripted",
                "model": "test",
                "effects": [{"type": "prompt", "name": "greeting", "template": "say hello"}],
            },
            sort_keys=False,
        ),
        encoding="utf-8",
    )
    replies_path = _write_replies(tmp_path, {"prime.greeting": [reply]}, name=f"{name}-replies.yaml")
    adapter = ScriptedAdapter(replies_file=str(replies_path))
    return run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state=None,
            adapter=adapter,
            config=CircuitryConfig(default_adapter="scripted", default_model="test"),
            skip_preflight=True,
        )
    )


def _strip_timestamps(node: Any) -> Any:
    if isinstance(node, dict):
        return {
            k: _strip_timestamps(v)
            for k, v in node.items()
            if k not in ("created_at", "completed_at", "started_at", "run_id", "_run_id", "_timestamp")
        }
    if isinstance(node, list):
        return [_strip_timestamps(v) for v in node]
    return node


def test_chunks_reply_final_state_matches_the_same_reply_without_chunks(tmp_path: Path) -> None:
    """The conformance case `stream-scripted-chunks` pins one streamed
    run's own output; this asserts directly that delivering the identical
    text through `chunks` instead of `text` changes nothing about the
    final node -- no `meta.live` left behind, same `meta` otherwise."""
    chunked = _run_greeting(
        tmp_path,
        "chunked",
        {"chunks": ["hello ", "from ", "scripted"], "tokens_sent": 4, "tokens_received": 6},
    )
    plain = _run_greeting(
        tmp_path,
        "plain",
        {"text": "hello from scripted", "tokens_sent": 4, "tokens_received": 6},
    )

    assert chunked.ok and plain.ok
    assert "live" not in chunked.state["prime"]["greeting"].get("meta", {})
    chunked_meta = _strip_timestamps(chunked.state["prime"])
    plain_meta = _strip_timestamps(plain.state["prime"])
    assert chunked_meta == plain_meta
    # Dict equality alone doesn't check key order; meta's own key order
    # is part of the contract this must not disturb.
    assert list(chunked_meta["greeting"]["meta"].keys()) == list(
        plain_meta["greeting"]["meta"].keys()
    )


def test_chunks_inside_a_use_child_fires_progress_namespaced_under_the_parent(
    tmp_path: Path,
) -> None:
    """#674 review finding 5: a streamed prompt running inside a `use`
    child's own `progress` line must carry the parent's own path, the
    same way its `start`/`end` events already do."""
    child_orch_path = tmp_path / "child.yml"
    child_orch_path.write_text(
        yaml.dump(
            {"effects": [{"type": "prompt", "name": "greeting", "template": "say hello"}]},
            sort_keys=False,
        ),
        encoding="utf-8",
    )
    orch_path = tmp_path / "parent.yml"
    orch_path.write_text(
        yaml.dump(
            {
                "adapter": "scripted",
                "model": "test",
                "effects": [
                    {"type": "use", "name": "sub", "path": str(child_orch_path)}
                ],
            },
            sort_keys=False,
        ),
        encoding="utf-8",
    )
    replies_path = _write_replies(
        tmp_path,
        {"prime.sub.greeting": [{"chunks": ["hello ", "from ", "scripted"]}]},
    )
    adapter = ScriptedAdapter(replies_file=str(replies_path))
    events_path = tmp_path / "events.jsonl"

    result = run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state=None,
            adapter=adapter,
            config=CircuitryConfig(default_adapter="scripted", default_model="test"),
            skip_preflight=True,
            events_path=events_path,
        )
    )

    assert result.ok
    lines = [
        json.loads(line)
        for line in events_path.read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]
    progress_lines = [line for line in lines if line["ev"] == "progress"]
    assert progress_lines, "expected at least one progress line"
    assert all(line["path"] == "prime.sub.greeting" for line in progress_lines)
