"""Tests for curation/agents/agent_loop.yml — the bundled tool-using agent loop.

The document is run the way a host runs it — one whole run through the runtime
shim — with a scripted adapter standing in for the model, so no test ever
reaches a real one. Each scripted reply is one ``decide`` decision; the adapter
records every prompt it was sent, which is how a test sees the transcript the
model was shown on each pass.

``search`` runs a fake ``rg`` (a Python script configured as
``runtime.plugins.ripgrep.binary``) that logs its argv and cwd and greps like
the real one, so the dispatch tests don't depend on ripgrep being installed;
one extra test runs the real binary when it is. ``git`` runs the real binary
against a throwaway repository.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import pytest
import yaml

from circuitry.adapters.base import GenerateResult
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.registry import find_entry, resolve_bundled
from circuitry.cli.runtime_shim import RunRequest, RunResult, run
from circuitry.core.store import Store

AGENT_LOOP_PATH = Path("src/circuitry/curation/agents/agent_loop.yml")

# A line of the decide prompt, so a test can tell decide prompts apart from
# anything else the run might send.
DECIDE_MARKER = "You are a careful agent working on a task."

FAKE_RG = """\
import json, os, re, sys
from pathlib import Path

args = sys.argv[1:]
with open(os.environ["FAKE_RG_LOG"], "a", encoding="utf-8") as log:
    log.write(json.dumps({"argv": args, "cwd": os.getcwd()}) + "\\n")
pattern = next(a.split("=", 1)[1] for a in args if a.startswith("--regexp="))
root = Path(args[args.index("--") + 1])
files = [root] if root.is_file() else sorted(p for p in root.rglob("*") if p.is_file())
found = False
for path in files:
    for number, line in enumerate(path.read_text(encoding="utf-8").splitlines(), 1):
        if re.search(pattern, line):
            print(f"{path}:{number}:{line}")
            found = True
sys.exit(0 if found else 1)
"""


@dataclass
class ScriptedModel:
    """Pops one scripted reply per call and records every prompt it was sent."""

    name: str = "scripted"
    replies: list[str] = field(default_factory=list)
    prompts: list[str] = field(default_factory=list)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.prompts.append(prompt)
        if not self.replies:
            raise AssertionError("the agent asked the model more often than scripted")
        return GenerateResult(text=self.replies.pop(0), raw={"model": model})

    @property
    def decide_prompts(self) -> list[str]:
        return [p for p in self.prompts if DECIDE_MARKER in p]


def _step(tool: str, args: dict[str, Any] | None = None) -> str:
    return json.dumps(
        {"thought": f"call {tool}", "tool": tool, "args": args or {}, "done": False, "answer": ""}
    )


def _finish(answer: str) -> str:
    return json.dumps(
        {"thought": "I know enough.", "tool": "", "args": {}, "done": True, "answer": answer}
    )


@pytest.fixture
def workdir(tmp_path: Path) -> Path:
    root = tmp_path / "project"
    (root / "src").mkdir(parents=True)
    (root / "src" / "durations.py").write_text(
        "def parse_duration(text):\n    raise ValueError('invalid duration')\n",
        encoding="utf-8",
    )
    (root / "README.md").write_text("A duration parser.\n", encoding="utf-8")
    return root


@pytest.fixture
def fake_rg(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    script = tmp_path / "bin" / "rg"
    script.parent.mkdir()
    script.write_text(f"#!{sys.executable}\n{FAKE_RG}", encoding="utf-8")
    script.chmod(0o755)
    log = tmp_path / "rg.log"
    monkeypatch.setenv("FAKE_RG_LOG", str(log))
    return script


def _rg_calls(tmp_path: Path) -> list[dict[str, Any]]:
    log = tmp_path / "rg.log"
    if not log.exists():
        return []
    return [json.loads(line) for line in log.read_text(encoding="utf-8").splitlines()]


def _run(
    model: ScriptedModel,
    *,
    workdir: Path,
    transcript: Path | None,
    max_steps: int = 8,
    rg_binary: Path | None = None,
) -> RunResult:
    inputs: dict[str, Any] = {
        "task": "Why does parse_duration fail?",
        "workdir": str(workdir),
        "max_steps": max_steps,
    }
    if transcript is not None:
        inputs["transcript"] = str(transcript)
    runtime: dict[str, Any] = {}
    if rg_binary is not None:
        runtime["plugins"] = {"ripgrep": {"binary": str(rg_binary)}}
    return run(
        RunRequest(
            orchestration_path=AGENT_LOOP_PATH,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state=inputs,
            adapter=model,
            config=CircuitryConfig(
                default_adapter="scripted", default_model="test", runtime=runtime
            ),
            skip_preflight=True,
        )
    )


def _output(result: RunResult, name: str) -> Any:
    """Read one declared interface output off the final state."""
    interface = yaml.safe_load(AGENT_LOOP_PATH.read_text(encoding="utf-8"))["interface"]
    return Store(result.state).get(interface["outputs"][name]["path"])


def _files(root: Path) -> list[str]:
    return sorted(str(p.relative_to(root)) for p in root.rglob("*") if ".git" not in p.parts)


def test_bundled_library_lists_and_resolves_agent_loop() -> None:
    entry = find_entry("agents/agent_loop")
    assert entry is not None
    assert entry["category"] == "agents"
    assert resolve_bundled("agents/agent_loop") == AGENT_LOOP_PATH.resolve()


def test_agent_loop_names_no_adapter_or_model() -> None:
    orch = yaml.safe_load(AGENT_LOOP_PATH.read_text(encoding="utf-8"))
    assert "adapter" not in orch
    assert "model" not in orch


def test_search_then_read_then_answer(
    tmp_path: Path, workdir: Path, fake_rg: Path
) -> None:
    transcript = tmp_path / "transcript.md"
    model = ScriptedModel(
        replies=[
            _step("search", {"pattern": "def parse_duration", "path": "src"}),
            _step("read_file", {"path": "src/durations.py"}),
            _finish("parse_duration always raises ValueError('invalid duration')."),
        ]
    )

    result = _run(model, workdir=workdir, transcript=transcript, rg_binary=fake_rg)

    assert result.ok, result.error
    assert _output(result, "answer") == (
        "parse_duration always raises ValueError('invalid duration')."
    )
    assert _output(result, "transcript") == str(transcript)

    # Dispatch: search ran ripgrep inside workdir with the pattern as one
    # --regexp= argument and the path after --.
    (call,) = _rg_calls(tmp_path)
    assert Path(call["cwd"]).resolve() == workdir.resolve()
    assert call["argv"][0] == "--no-config"
    assert "--regexp=def parse_duration" in call["argv"]
    assert call["argv"][-2:] == ["--", "src"]

    # Transcript growth: each pass's prompt holds every earlier observation.
    first, second, third = model.decide_prompts
    assert "## Step 0" not in first
    assert "src/durations.py:1:def parse_duration(text):" in second
    assert "raise ValueError('invalid duration')" not in second
    assert "src/durations.py:1:def parse_duration(text):" in third
    assert "    raise ValueError('invalid duration')" in third

    text = transcript.read_text(encoding="utf-8")
    assert "Task: Why does parse_duration fail?" in text
    assert [line for line in text.splitlines() if line.startswith("## Step")] == [
        "## Step 0",
        "## Step 1",
        "## Step 2",
    ]
    assert "answer: parse_duration always raises" in text
    assert result.state["prime"]["agent"]["value"]["iterations"] == 3


def test_stops_at_max_steps_without_an_answer(tmp_path: Path, workdir: Path) -> None:
    transcript = tmp_path / "transcript.md"
    model = ScriptedModel(replies=[_step("list_dir", {"path": "."}) for _ in range(5)])

    result = _run(model, workdir=workdir, transcript=transcript, max_steps=2)

    assert result.ok, result.error
    assert len(model.decide_prompts) == 2
    assert result.state["prime"]["agent"]["value"]["iterations"] == 2
    assert _output(result, "answer").startswith("No answer: max_steps ran out")
    text = transcript.read_text(encoding="utf-8")
    assert "['README.md', 'src']" in text
    assert "## Step 2" not in text


def test_missing_workdir_fails_before_any_model_call(tmp_path: Path) -> None:
    transcript = tmp_path / "transcript.md"
    model = ScriptedModel(replies=[_step("list_dir")])

    result = _run(model, workdir=tmp_path / "no-such-dir", transcript=transcript)

    assert not result.ok
    assert model.prompts == []
    assert not transcript.exists()


def test_unknown_tool_is_reported_back_to_the_model(tmp_path: Path, workdir: Path) -> None:
    transcript = tmp_path / "transcript.md"
    before = _files(workdir)
    model = ScriptedModel(
        replies=[
            _step("write_file", {"path": "src/durations.py"}),
            _finish("Could not write; nothing changed."),
        ]
    )

    result = _run(model, workdir=workdir, transcript=transcript)

    assert result.ok, result.error
    assert "tool: write_file" in model.decide_prompts[1]
    assert "Unknown tool. Use one of: list_dir, read_file, search" in model.decide_prompts[1]
    assert _output(result, "answer") == "Could not write; nothing changed."
    assert _files(workdir) == before


@pytest.mark.parametrize(
    ("tool", "args"),
    [
        ("read_file", {"path": "../secret.txt"}),
        ("read_file", {"path": "src/../../secret.txt"}),
        ("read_file", {"path": "ABSOLUTE"}),
        ("list_dir", {"path": ".."}),
        ("search", {"pattern": "TOP SECRET", "path": "../"}),
        ("search", {"pattern": "", "path": "src"}),
    ],
)
def test_arguments_that_leave_workdir_are_refused(
    tmp_path: Path, workdir: Path, fake_rg: Path, tool: str, args: dict[str, str]
) -> None:
    secret = tmp_path / "secret.txt"
    secret.write_text("TOP SECRET\n", encoding="utf-8")
    if args.get("path") == "ABSOLUTE":
        args = {"path": str(secret)}
    transcript = tmp_path / "transcript.md"
    model = ScriptedModel(replies=[_step(tool, args), _finish("refused")])

    result = _run(model, workdir=workdir, transcript=transcript, rg_binary=fake_rg)

    assert result.ok, result.error
    assert "Refused: path must be relative to the working directory" in model.decide_prompts[1]
    assert _rg_calls(tmp_path) == []
    assert "TOP SECRET\n" not in transcript.read_text(encoding="utf-8")


def _git(repo: Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=repo, check=True, capture_output=True, text=True
    ).stdout


@pytest.fixture
def repo(workdir: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    # Neither the machine's global nor system git config reaches the repo
    # or the agent's own git calls.
    monkeypatch.setenv("GIT_CONFIG_GLOBAL", os.devnull)
    monkeypatch.setenv("GIT_CONFIG_NOSYSTEM", "1")
    _git(workdir, "init", "-q")
    _git(workdir, "add", ".")
    _git(
        workdir, "-c", "user.name=Test", "-c", "user.email=test@example.com",
        "commit", "-q", "-m", "Add the duration parser",
    )
    (workdir / "README.md").write_text("A duration parser, now documented.\n", encoding="utf-8")
    return workdir


def test_git_tools_read_the_repository(tmp_path: Path, repo: Path) -> None:
    transcript = tmp_path / "transcript.md"
    model = ScriptedModel(
        replies=[
            _step("git_status"),
            _step("git_log"),
            _step("git_diff"),
            _step("git_show", {"rev": "HEAD"}),
            _finish("done"),
        ]
    )

    result = _run(model, workdir=repo, transcript=transcript)

    assert result.ok, result.error
    status, log, diff, show = model.decide_prompts[1:]
    assert " M README.md" in status
    assert "Add the duration parser" in log
    assert "+A duration parser, now documented." in diff
    assert "+def parse_duration(text):" in show


def test_git_rev_cannot_become_an_option(tmp_path: Path, repo: Path) -> None:
    written = tmp_path / "written-by-git.txt"
    transcript = tmp_path / "transcript.md"
    model = ScriptedModel(
        replies=[
            _step("git_show", {"rev": f"--output={written}"}),
            _step("git_diff", {"rev": f"--output={written}"}),
            _step("git_log", {"rev": f"--output={written}"}),
            _finish("done"),
        ]
    )

    result = _run(model, workdir=repo, transcript=transcript)

    assert result.ok, result.error
    assert not written.exists()
    assert transcript.read_text(encoding="utf-8").count("error: ") == 3


def test_default_transcript_is_in_tmpdir_not_workdir(
    tmp_path: Path, workdir: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    tmpdir = tmp_path / "tmp"
    monkeypatch.setenv("TMPDIR", str(tmpdir))
    before = _files(workdir)
    model = ScriptedModel(replies=[_step("list_dir"), _finish("listed")])

    result = _run(model, workdir=workdir, transcript=None)

    assert result.ok, result.error
    transcript = Path(_output(result, "transcript"))
    # Directly in TMPDIR under an unguessable name: no shared subdirectory
    # that another user could create, or point elsewhere, first.
    assert transcript.parent == tmpdir
    assert re.fullmatch(r"circuitry-agent-loop-[0-9a-f]{32}\.md", transcript.name)
    assert "## Step 1" in transcript.read_text(encoding="utf-8")
    assert _files(workdir) == before


@pytest.mark.skipif(shutil.which("rg") is None, reason="ripgrep (rg) is not installed")
def test_search_with_real_ripgrep_ignores_ripgrep_config(
    tmp_path: Path, workdir: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # A ripgreprc could add any flag, --pre (a command per file) included;
    # --no-config means this one's --invert-match never applies.
    config = tmp_path / "ripgreprc"
    config.write_text("--invert-match\n", encoding="utf-8")
    monkeypatch.setenv("RIPGREP_CONFIG_PATH", str(config))
    transcript = tmp_path / "transcript.md"
    model = ScriptedModel(
        replies=[_step("search", {"pattern": "invalid duration"}), _finish("found")]
    )

    result = _run(model, workdir=workdir, transcript=transcript)

    assert result.ok, result.error
    observation = model.decide_prompts[1]
    assert "src/durations.py:2:    raise ValueError('invalid duration')" in observation
    assert "def parse_duration(text):" not in observation.split("## Step 0", 1)[1]
