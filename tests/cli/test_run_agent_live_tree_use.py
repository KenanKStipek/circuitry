"""``meta.live`` while an agent effect runs inside a ``flow: tree`` loop
pass or a ``use`` child (#636's acceptance list).

Measured against this branch: both already work without #634 (which is
about a running node's own visibility more generally, not about whether a
mid-run ``Store.set`` reaches ``--live-state`` -- ``Store.parallel_branches``
already republishes a branch's writes as they happen, the same path
``meta.waiting_for`` already used).
"""

from __future__ import annotations

import json
import threading
import time
from pathlib import Path
from typing import Any

import pytest
from agent_cli_test_support import REPLAY_BODY, read_fixture_stream, write_fake_cli

pytest.importorskip("typer")
from typer.testing import CliRunner

from circuitry.cli.app import app

runner = CliRunner()

TREE_DOCUMENT = """\
effects:
  - name: items
    type: tool
    provider: json
    params: {{mode: parse, input: "[1]"}}
  - name: tree
    type: loop
    flow: tree
    each: {{in: prime.items.value, as: x}}
    body:
      - name: agent_in_tree
        type: tool
        provider: agent
        timeout_ms: 30000
        params:
          prompt: "do the task"
          cwd: {cwd}
"""

USE_DOCUMENT = """\
effects:
  - name: child
    type: use
    inline: |
      effects:
        - name: agent_in_use
          type: tool
          provider: agent
          timeout_ms: 30000
          params:
            prompt: "do the task"
            cwd: {cwd}
"""


def _write(path: Path, content: str) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    return path


def _live_meta(live: Path, path: tuple[str, ...]) -> dict[str, Any] | None:
    if not live.exists():
        return None
    try:
        state = json.loads(live.read_text(encoding="utf-8"))
    except json.JSONDecodeError:
        return None
    node: Any = state
    for part in path:
        if not isinstance(node, dict):
            return None
        node = node.get(part)
    if not isinstance(node, dict):
        return None
    return node.get("meta", {}).get("live")


def _wait_for(predicate: Any, *, timeout: float = 10.0) -> Any:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.02)
    raise AssertionError("condition never became true within the timeout")


def _run_gated(tmp_path: Path, document: str, node_path: tuple[str, ...]) -> dict[str, Any]:
    work = tmp_path / "work"
    work.mkdir()
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    fake = write_fake_cli(bin_dir, "pi", body=REPLAY_BODY)
    stream = _write(tmp_path / "stream.jsonl", read_fixture_stream("pi-stream.jsonl"))
    orch = _write(tmp_path / "doc.yml", document.format(cwd=json.dumps(str(work))))
    config = _write(
        tmp_path / "config.json",
        json.dumps({"runtime": {"plugins": {"agent": {"pi": {"binary": str(fake)}}}}}),
    )
    live = tmp_path / "live.json"
    out = tmp_path / "out.json"

    def _run() -> None:
        runner.invoke(
            app,
            ["run", str(orch), "--config", str(config), "--out", str(out), "--live-state", str(live)],
            env={"FAKE_STREAM": str(stream)},
        )

    thread = threading.Thread(target=_run)
    thread.start()
    try:
        meta = _wait_for(
            lambda: (m := _live_meta(live, node_path)) is not None and m["tool_calls"] == 1 and m
        )
    finally:
        (work / "gate1").touch()
        (work / "gate2").touch()
        thread.join(timeout=20)
    return meta


def test_meta_live_reaches_live_state_inside_a_tree_loop_pass(tmp_path: Path) -> None:
    meta = _run_gated(tmp_path, TREE_DOCUMENT, ("prime", "tree", "iter_0", "agent_in_tree"))
    assert meta["engine"] == "pi"
    assert meta["current_tool"] is not None


def test_meta_live_reaches_live_state_inside_a_use_child(tmp_path: Path) -> None:
    meta = _run_gated(tmp_path, USE_DOCUMENT, ("prime", "child", "agent_in_use"))
    assert meta["engine"] == "pi"
    assert meta["current_tool"] is not None
