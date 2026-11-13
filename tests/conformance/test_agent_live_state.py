"""Both engines publish the same `meta.live` keys while an `agent` effect's
session is still running (issue #720, #636) -- not a `cases/` conformance
case, since those only ever compare a run's *final* state (`README.md`'s
own "Events and live state" section): this samples `--live-state` while a
bounded fake `pi` is deliberately held open by a semaphore file, for each
engine in turn.
"""

from __future__ import annotations

import json
import shutil
import subprocess
import sys
import time
from pathlib import Path

import pytest

from . import harness

ELECTRICITY_DIR = Path(__file__).resolve().parents[2] / "electricity"

#: How long this test waits for `meta.live.current_tool` to appear before
#: giving up -- generous for a loaded machine, far short of the fake's own
#: #661 bound (120 s).
_POLL_DEADLINE_SECONDS = 20.0
_POLL_INTERVAL_SECONDS = 0.1

_FAKE_PI = '''#!/usr/bin/env python3
"""A fake `pi` that holds its first tool call open until a semaphore file
appears, so a caller sampling `--live-state` is guaranteed a window with
`meta.live.current_tool` set -- bounded (#661): gives up and exits once
orphaned or after 120 s, whichever comes first, never waiting forever."""

from __future__ import annotations

import json
import os
import sys
import time

attached = [arg[1:] for arg in sys.argv[1:] if arg.startswith("@")]
with open(attached[0], encoding="utf-8") as handle:
    handle.read()

semaphore = sys.argv[sys.argv.index("--go-file") + 1]

sys.stdout.write(json.dumps({"type": "session", "id": "sess-live-state"}) + "\\n")
sys.stdout.flush()
sys.stdout.write(
    json.dumps(
        {
            "type": "tool_execution_start",
            "toolName": "write",
            "toolCallId": "t1",
            "args": {"path": "result.json", "content": "x\\n"},
        }
    )
    + "\\n"
)
sys.stdout.flush()

parent = os.getppid()
deadline = time.monotonic() + 120
while not os.path.exists(semaphore):
    if os.getppid() != parent or time.monotonic() > deadline:
        raise SystemExit(1)
    time.sleep(0.05)

usage = {"input": 3, "output": 3, "cacheRead": 0, "cacheWrite": 0, "cost": {"total": 0.001}}
sys.stdout.write(json.dumps({"type": "tool_execution_end", "toolCallId": "t1", "isError": False, "durationMs": 1}) + "\\n")
sys.stdout.write(
    json.dumps(
        {
            "type": "message_end",
            "message": {
                "role": "assistant",
                "usage": usage,
                "content": [{"type": "text", "text": "done"}],
                "stopReason": "stop",
            },
        }
    )
    + "\\n"
)
'''

_ORCHESTRATION = """name: agent-live-state-sample
effects:
  - type: tool
    name: implement
    provider: agent
    timeout_ms: 30000
    params:
      engine: pi
      prompt: "Write the result file."
      cwd: {from: input.work_dir}
      extra_args: ["--go-file", "{{input.go_file}}"]
"""


def _write_fixture(tmp_path: Path) -> tuple[Path, Path, Path]:
    """The shared orchestration/config/fake, written once per engine run
    (each gets its own `tmp_path`, so neither reuses the other's
    semaphore/output files) -- returns `(case_dir, go_file, work_dir)`."""
    case_dir = tmp_path / "doc"
    work_dir = case_dir / "work"
    fakes_dir = case_dir / "fakes"
    work_dir.mkdir(parents=True)
    fakes_dir.mkdir(parents=True)
    (case_dir / "orchestration.yml").write_text(_ORCHESTRATION, encoding="utf-8")
    (case_dir / "config.json").write_text("{}", encoding="utf-8")
    fake_path = fakes_dir / "pi"
    fake_path.write_text(_FAKE_PI, encoding="utf-8")
    fake_path.chmod(0o700)
    go_file = tmp_path / "go"
    return case_dir, go_file, work_dir


def _poll_live(live_state_path: Path) -> dict | None:
    """`prime.implement.meta.live`, read repeatedly until its own
    `current_tool` is set or the deadline passes -- `None` on timeout."""
    deadline = time.monotonic() + _POLL_DEADLINE_SECONDS
    while time.monotonic() < deadline:
        if live_state_path.exists():
            try:
                data = json.loads(live_state_path.read_text(encoding="utf-8"))
            except (json.JSONDecodeError, OSError):
                data = None
            if data is not None:
                live = data.get("prime", {}).get("implement", {}).get("meta", {}).get("live")
                if isinstance(live, dict) and live.get("current_tool") is not None:
                    return live
        time.sleep(_POLL_INTERVAL_SECONDS)
    return None


def _sample_python(tmp_path: Path) -> dict:
    case_dir, go_file, work_dir = _write_fixture(tmp_path)
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    live_state_path = tmp_path / "live-state.json"
    out_path = tmp_path / "out.json"
    cmd = [
        sys.executable,
        "-m",
        "circuitry.cli.app",
        "run",
        "orchestration.yml",
        "--out",
        str(out_path),
        "--quiet",
        "--config",
        "config.json",
        "--live-state",
        str(live_state_path),
        "-e",
        f"work_dir={work_dir}",
        "-e",
        f"go_file={go_file}",
    ]
    proc = subprocess.Popen(
        cmd,
        cwd=case_dir,
        env=harness._sandboxed_env(case_dir, home_dir),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        live = _poll_live(live_state_path)
        go_file.write_text("go", encoding="utf-8")
        stdout, stderr = proc.communicate(timeout=harness.DEFAULT_TIMEOUT_SECONDS)
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait(timeout=5)
    assert live is not None, "cof run never published meta.live.current_tool in time"
    assert proc.returncode == 0, f"cof run failed: {stdout}\n{stderr}"
    return live


def _sample_electricity(tmp_path: Path, binary: Path) -> dict:
    case_dir, go_file, work_dir = _write_fixture(tmp_path)
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    live_state_path = tmp_path / "live-state.json"
    out_path = tmp_path / "out.json"
    cmd = [
        str(binary),
        str(case_dir / "config.json"),
        "orchestration.yml",
        "--out",
        str(out_path),
        "--live-state",
        str(live_state_path),
        "-e",
        f"work_dir={work_dir}",
        "-e",
        f"go_file={go_file}",
    ]
    proc = subprocess.Popen(
        cmd,
        cwd=case_dir,
        env=harness._sandboxed_env(case_dir, home_dir),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        live = _poll_live(live_state_path)
        go_file.write_text("go", encoding="utf-8")
        stdout, stderr = proc.communicate(timeout=harness.DEFAULT_TIMEOUT_SECONDS)
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait(timeout=5)
    assert live is not None, "electricity run never published meta.live.current_tool in time"
    assert proc.returncode == 0, f"electricity run failed: {stdout}\n{stderr}"
    return live


@pytest.fixture(scope="module")
def electricity_binary() -> Path | None:
    if shutil.which("cargo") is None:
        return None
    proc = subprocess.run(
        ["cargo", "build", "-p", "electricity-cli", "--bin", "electricity"],
        cwd=ELECTRICITY_DIR,
        env=harness.cargo_env(),
        capture_output=True,
        text=True,
        timeout=600,
        check=False,
    )
    if proc.returncode != 0:
        pytest.fail(f"cargo build -p electricity-cli failed:\n{proc.stdout}\n{proc.stderr}")
    binary = ELECTRICITY_DIR / "target" / "debug" / "electricity"
    assert binary.exists(), f"cargo build succeeded but {binary} is missing"
    return binary


def test_both_engines_publish_the_same_meta_live_keys_while_running(
    tmp_path: Path, electricity_binary: Path | None
) -> None:
    if electricity_binary is None:
        pytest.skip("cargo not available; electricity binary cannot be built")
    python_live = _sample_python(tmp_path / "python")
    electricity_live = _sample_electricity(tmp_path / "electricity", electricity_binary)

    assert set(python_live.keys()) == set(electricity_live.keys()), (
        f"meta.live key sets differ:\n  cof: {sorted(python_live.keys())}\n"
        f"  electricity: {sorted(electricity_live.keys())}"
    )
    # Figures a mid-run sample can assert without racing either engine's
    # own publish cadence (#636/#741's own "never byte-for-byte reproducible
    # between the two" timing rule, DESIGN.md §13): the numbers/names this
    # test's own fake has already made true by the time `current_tool` is
    # set, not anything that depends on exactly when either engine's
    # publish thread/tick last ran.
    for live in (python_live, electricity_live):
        assert live["v"] == 1
        assert live["engine"] == "pi"
        assert live["session_id"] == "sess-live-state"
        assert live["phase"] == "turn"
        assert live["attempt"] == 1
        assert live["current_tool"]["name"] == "write"
        assert live["tool_calls"] == 1
        assert live["tools"]["write"]["calls"] == 1
