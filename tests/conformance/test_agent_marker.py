"""No prompt, reply or transcript text reaches state, events or live state
(issue #720's own acceptance criterion 3) -- not a `cases/` conformance
case, since those only ever compare a run's *final* state (`README.md`'s
own "Events and live state" section) and never sample `--live-state`
mid-run: this plants one distinctive marker in the prompt file, the fake's
own reply text and a tool call's own argument, then greps every artifact
of a run on both engines (`--out`, `--events`, every `--live-state`
sample taken while it runs, and the run's own stderr warnings) for it.
The marker must appear in the transcript -- proof this test's own fake
was actually exercised -- and nowhere else.
"""

from __future__ import annotations

import json
import shutil
import subprocess
import sys
import time
import uuid
from pathlib import Path

import pytest

from . import harness

ELECTRICITY_DIR = Path(__file__).resolve().parents[2] / "electricity"

#: How long this test waits for `meta.live.current_tool` to appear before
#: releasing the fake's own semaphore -- generous for a loaded machine,
#: far short of the fake's own #661 bound (120 s).
_POLL_DEADLINE_SECONDS = 20.0
_POLL_INTERVAL_SECONDS = 0.1

_FAKE_PI = '''#!/usr/bin/env python3
"""A fake `pi` that reads a marker line out of its own attached prompt
file, echoes it in a tool call's own `path` argument and in its final
reply text, then writes a marker-free result -- bounded (#661): gives up
and exits once orphaned or after 120 s, never waiting forever for the
semaphore file this test releases once it has sampled `--live-state`."""

from __future__ import annotations

import json
import os
import sys
import time

attached = [arg[1:] for arg in sys.argv[1:] if arg.startswith("@")]
with open(attached[0], encoding="utf-8") as handle:
    prompt_text = handle.read()

marker = next(
    line[len("MARKER:") :] for line in prompt_text.splitlines() if line.startswith("MARKER:")
)
semaphore = sys.argv[sys.argv.index("--go-file") + 1]

sys.stdout.write(json.dumps({"type": "session", "id": "sess-marker"}) + "\\n")
sys.stdout.write(
    json.dumps(
        {
            "type": "tool_execution_start",
            "toolName": "write",
            "toolCallId": "t1",
            "args": {"path": marker, "content": "irrelevant"},
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

scratch_dir = os.getcwd()
with open(os.path.join(scratch_dir, "result.json"), "w", encoding="utf-8") as handle:
    json.dump({"summary": "done"}, handle)

usage = {"input": 3, "output": 3, "cacheRead": 0, "cacheWrite": 0, "cost": {"total": 0.001}}
sys.stdout.write(
    json.dumps({"type": "tool_execution_end", "toolCallId": "t1", "isError": False, "durationMs": 1})
    + "\\n"
)
sys.stdout.write(
    json.dumps(
        {
            "type": "message_end",
            "message": {
                "role": "assistant",
                "usage": usage,
                "content": [{"type": "text", "text": f"I used marker {marker} while working."}],
                "stopReason": "stop",
            },
        }
    )
    + "\\n"
)
'''

_ORCHESTRATION = """name: agent-marker-sample
effects:
  - type: tool
    name: implement
    provider: agent
    timeout_ms: 30000
    params:
      engine: pi
      prompt:
        file: {from: input.prompt_file}
        within: {from: input.work_dir}
      cwd: {from: input.work_dir}
      extra_args: ["--go-file", "{{input.go_file}}"]
      result_file: result.json
      result_schema:
        type: object
        properties: {summary: {type: string}}
        required: [summary]
      transcript: {from: input.transcript_path}
"""


def _write_fixture(tmp_path: Path, marker: str) -> tuple[Path, Path, Path, Path]:
    """Returns `(case_dir, go_file, work_dir, transcript_path)`. The
    marker goes into a prompt *file* named by `input.prompt_file`
    (issue #591's own form, `{file: {from: ...}, within: ...}`) --
    never a literal YAML `params.prompt` string, which `meta.
    params_rendered` would echo back into state like any other tool's
    own literal param text, and never a CLI `-e` input variable either,
    which state's own `input` block always echoes back too. Only the
    file form's own `{file: <resolved path>, within: <resolved within>}`
    -- a path, never the file's content -- is what state is allowed to
    carry for it."""
    case_dir = tmp_path / "doc"
    work_dir = case_dir / "work"
    fakes_dir = case_dir / "fakes"
    prompts_dir = work_dir / "prompts"
    prompts_dir.mkdir(parents=True)
    fakes_dir.mkdir(parents=True)
    (prompts_dir / "marker.md").write_text(
        f"Write the result file.\nMARKER:{marker}\n", encoding="utf-8"
    )
    (case_dir / "orchestration.yml").write_text(_ORCHESTRATION, encoding="utf-8")
    (case_dir / "config.json").write_text("{}", encoding="utf-8")
    fake_path = fakes_dir / "pi"
    fake_path.write_text(_FAKE_PI, encoding="utf-8")
    fake_path.chmod(0o700)
    go_file = tmp_path / "go"
    transcript_path = tmp_path / "transcript.log"
    return case_dir, go_file, work_dir, transcript_path


def _poll_live_samples(live_state_path: Path, samples: list[str]) -> bool:
    """Appends every distinct raw `--live-state` file read to *samples*,
    returning `True` once `meta.live.current_tool` has appeared at least
    once (so the caller knows it is safe to release the fake's semaphore)."""
    deadline = time.monotonic() + _POLL_DEADLINE_SECONDS
    seen_tool = False
    while time.monotonic() < deadline:
        if live_state_path.exists():
            try:
                text = live_state_path.read_text(encoding="utf-8")
            except OSError:
                text = None
            if text is not None:
                if not samples or samples[-1] != text:
                    samples.append(text)
                try:
                    data = json.loads(text)
                except json.JSONDecodeError:
                    data = None
                if data is not None:
                    live = data.get("prime", {}).get("implement", {}).get("meta", {}).get("live")
                    if isinstance(live, dict) and live.get("current_tool") is not None:
                        seen_tool = True
        if seen_tool:
            return True
        time.sleep(_POLL_INTERVAL_SECONDS)
    return seen_tool


def _run_one(
    cmd: list[str],
    case_dir: Path,
    env: dict[str, str],
    live_state_path: Path,
    go_file: Path,
) -> tuple[int, str, str, list[str]]:
    samples: list[str] = []
    proc = subprocess.Popen(
        cmd, cwd=case_dir, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True
    )
    try:
        seen = _poll_live_samples(live_state_path, samples)
        go_file.write_text("go", encoding="utf-8")
        stdout, stderr = proc.communicate(timeout=harness.DEFAULT_TIMEOUT_SECONDS)
    finally:
        if proc.poll() is None:
            proc.kill()
            proc.wait(timeout=5)
    assert seen, "the agent effect never published meta.live.current_tool in time"
    return proc.returncode, stdout, stderr, samples


def _assert_marker_only_in_transcript(
    *,
    marker: str,
    out_path: Path,
    events_path: Path,
    transcript_path: Path,
    live_samples: list[str],
    stderr: str,
) -> None:
    assert marker in transcript_path.read_text(encoding="utf-8"), (
        "the marker never reached the transcript -- this test's own fake path was never exercised"
    )
    out_text = out_path.read_text(encoding="utf-8") if out_path.exists() else ""
    assert marker not in out_text, "the marker leaked into --out"
    events_text = events_path.read_text(encoding="utf-8") if events_path.exists() else ""
    assert marker not in events_text, "the marker leaked into --events"
    for sample in live_samples:
        assert marker not in sample, "the marker leaked into a --live-state sample"
    assert marker not in stderr, "the marker leaked into the run's own warnings/stderr"


def _sample_python(tmp_path: Path, marker: str) -> None:
    case_dir, go_file, work_dir, transcript_path = _write_fixture(tmp_path, marker)
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    live_state_path = tmp_path / "live-state.json"
    out_path = tmp_path / "out.json"
    events_path = tmp_path / "events.jsonl"
    cmd = [
        sys.executable, "-m", "circuitry.cli.app", "run", "orchestration.yml",
        "--out", str(out_path), "--quiet", "--config", "config.json",
        "--live-state", str(live_state_path), "--events", str(events_path),
        "-e", f"work_dir={work_dir}", "-e", f"go_file={go_file}",
        "-e", f"transcript_path={transcript_path}", "-e", "prompt_file=prompts/marker.md",
    ]
    returncode, stdout, stderr, live_samples = _run_one(
        cmd, case_dir, harness._sandboxed_env(case_dir, home_dir), live_state_path, go_file
    )
    assert returncode == 0, f"cof run failed: {stdout}\n{stderr}"
    _assert_marker_only_in_transcript(
        marker=marker,
        out_path=out_path,
        events_path=events_path,
        transcript_path=transcript_path,
        live_samples=live_samples,
        stderr=stderr,
    )


def _sample_electricity(tmp_path: Path, marker: str, binary: Path) -> None:
    case_dir, go_file, work_dir, transcript_path = _write_fixture(tmp_path, marker)
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    live_state_path = tmp_path / "live-state.json"
    out_path = tmp_path / "out.json"
    events_path = tmp_path / "events.jsonl"
    cmd = [
        str(binary), str(case_dir / "config.json"), "orchestration.yml",
        "--out", str(out_path), "--live-state", str(live_state_path),
        "--events", str(events_path),
        "-e", f"work_dir={work_dir}", "-e", f"go_file={go_file}",
        "-e", f"transcript_path={transcript_path}", "-e", "prompt_file=prompts/marker.md",
    ]
    returncode, stdout, stderr, live_samples = _run_one(
        cmd, case_dir, harness._sandboxed_env(case_dir, home_dir), live_state_path, go_file
    )
    assert returncode == 0, f"electricity run failed: {stdout}\n{stderr}"
    _assert_marker_only_in_transcript(
        marker=marker,
        out_path=out_path,
        events_path=events_path,
        transcript_path=transcript_path,
        live_samples=live_samples,
        stderr=stderr,
    )


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


def test_both_engines_keep_the_marker_out_of_every_artifact_but_the_transcript(
    tmp_path: Path, electricity_binary: Path | None
) -> None:
    if electricity_binary is None:
        pytest.skip("cargo not available; electricity binary cannot be built")
    marker = f"CIRCUITRY-MARKER-{uuid.uuid4().hex}"
    _sample_python(tmp_path / "python", marker)
    _sample_electricity(tmp_path / "electricity", marker, electricity_binary)
