#!/usr/bin/env python3
"""Records golden-test fixtures for oscilloscope-core (issue #424).

Runs ``cof`` over small synthetic orchestrations with the ``scripted``
adapter (no network, no credentials, electricity/docs/spec/scripted-replies.md),
polling ``--live-state`` the same way osp itself does, and writes the
recorded run (every snapshot with its elapsed time, stdout/stderr, and the
exit code) under ``oscilloscope/tests/fixtures/<case>/``.

This is a **recording** tool, not a generator in the `electricity/scripts/
generate_*.py --check` sense: a run's exact timing is never reproducible,
so the committed fixtures are *inputs* to the golden tests, not derived
outputs CI re-checks against this script. Re-run it by hand (`python
oscilloscope/scripts/record_fixtures.py`) only when a fixture needs to
change or a new case is added; review the diff like any other commit.

Every run gets its own temporary HOME, a hard per-case timeout, and the
four credential variables this repository's CLAUDE.md names removed. Each
engine process starts its own session (``start_new_session=True``), so a
signal this script sends during a case reaches only that process's own
group — never the recorder's own session.
"""

from __future__ import annotations

import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, field
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
FIXTURES_DIR = Path(__file__).resolve().parent.parent / "tests" / "fixtures"
POLL_INTERVAL_S = 0.05
HARD_TIMEOUT_S = 15.0


@dataclass
class SignalStep:
    after_s: float
    sig: signal.Signals


@dataclass
class Case:
    name: str
    doc: str
    replies: str | None = None
    signals: list[SignalStep] = field(default_factory=list)


CASES: list[Case] = [
    Case(
        name="simple_chain_ok",
        doc="""\
effects:
  - name: first
    type: tool
    provider: shell
    params:
      command: echo
      args: ["one"]
  - name: second
    type: tool
    provider: shell
    params:
      command: echo
      args: ["two"]
""",
    ),
    Case(
        name="on_error_continue",
        doc="""\
effects:
  - name: flaky
    type: tool
    provider: shell
    on_error: continue
    params:
      command: ls
      args: ["/nonexistent-osp-fixture-path"]
  - name: cleanup
    type: tool
    provider: shell
    params:
      command: echo
      args: ["cleaned"]
""",
    ),
    Case(
        name="sigint_once",
        doc="""\
effects:
  - name: slow
    type: tool
    provider: shell
    params:
      command: tail
      args: ["-f", "/dev/null"]
      allowed_commands: ["tail"]
""",
        signals=[SignalStep(after_s=0.5, sig=signal.SIGINT)],
    ),
    Case(
        name="sigint_twice",
        doc="""\
effects:
  - name: slow
    type: tool
    provider: shell
    params:
      command: tail
      args: ["-f", "/dev/null"]
      allowed_commands: ["tail"]
""",
        signals=[
            SignalStep(after_s=0.5, sig=signal.SIGINT),
            SignalStep(after_s=0.55, sig=signal.SIGINT),
        ],
    ),
    Case(
        name="sigterm",
        doc="""\
effects:
  - name: slow
    type: tool
    provider: shell
    params:
      command: tail
      args: ["-f", "/dev/null"]
      allowed_commands: ["tail"]
""",
        signals=[SignalStep(after_s=0.5, sig=signal.SIGTERM)],
    ),
    # A second SIGINT while `finally:`'s own cleanup (the sleep below) is
    # still running makes `cof` call `os._exit` at once
    # (`cli/interrupts.py`): no final `state.live.json` write and no
    # `--out` (DESIGN.md §1.1's "Second SIGINT during cleanup" row) —
    # the one case `sigint_once`/`sigint_twice` never exercise, since
    # neither has a `finally:` slow enough for a second signal to land
    # inside its cleanup window (F12).
    Case(
        name="aborted_second_sigint",
        doc="""\
effects:
  - name: slow
    type: tool
    provider: shell
    params:
      command: tail
      args: ["-f", "/dev/null"]
      allowed_commands: ["tail"]
finally:
  - name: cleanup
    type: tool
    provider: shell
    params:
      command: sleep
      args: ["5"]
      allowed_commands: ["sleep"]
""",
        signals=[
            SignalStep(after_s=1.0, sig=signal.SIGINT),
            SignalStep(after_s=1.1, sig=signal.SIGINT),
        ],
    ),
    Case(
        name="named_if_taken",
        doc="""\
effects:
  - name: gate
    type: if
    if:
      mode: cel
      expr: "true"
    then:
      - name: chosen
        type: tool
        provider: shell
        params:
          command: echo
          args: ["then-branch"]
    else:
      - name: chosen
        type: tool
        provider: shell
        params:
          command: echo
          args: ["else-branch"]
""",
    ),
    Case(
        name="on_error_skip",
        doc="""\
effects:
  - name: flaky
    type: tool
    provider: shell
    on_error: skip
    params:
      command: ls
      args: ["/nonexistent-osp-fixture-path"]
  - name: cleanup
    type: tool
    provider: shell
    params:
      command: echo
      args: ["cleaned"]
""",
    ),
    Case(
        name="each_chain_loop",
        doc="""\
interface:
  inputs:
    items: {type: array, default: ["a", "b", "c"]}
effects:
  - type: loop
    name: each_chain
    each:
      in: input.items
      as: item
    body:
      - name: nap
        type: tool
        provider: shell
        params:
          command: echo
          args: ["{{item}}"]
""",
    ),
    Case(
        name="each_tree_loop_concurrency",
        doc="""\
interface:
  inputs:
    items: {type: array, default: ["a", "b", "c", "d"]}
effects:
  - type: loop
    name: each_tree
    flow: tree
    max_concurrency: 2
    each:
      in: input.items
      as: item
    body:
      - name: nap
        type: tool
        provider: shell
        params:
          command: echo
          args: ["{{item}}"]
""",
    ),
    Case(
        name="prompt_chain",
        doc="""\
effects:
  - name: greet
    type: prompt
    template: "Say hello!"
  - name: thank
    type: prompt
    template: "Say thank you!"
""",
        replies="""\
prime.greet:
  - text: "hello there"
    tokens_sent: 5
    tokens_received: 3
prime.thank:
  - text: "you're welcome"
    tokens_sent: 4
    tokens_received: 3
""",
    ),
    Case(
        name="prompt_tree_loop_concurrency",
        doc="""\
interface:
  inputs:
    topics: {type: array, default: ["cats", "dogs", "birds"]}
effects:
  - type: loop
    name: drafts
    flow: tree
    max_concurrency: 2
    each:
      in: input.topics
      as: topic
    body:
      - name: draft
        type: prompt
        template: "Write one sentence about {{topic}}."
""",
        replies="""\
prime.drafts.iter_0.draft:
  - text: "cats are great"
    tokens_sent: 6
    tokens_received: 4
prime.drafts.iter_1.draft:
  - text: "dogs are great"
    tokens_sent: 6
    tokens_received: 4
prime.drafts.iter_2.draft:
  - text: "birds are great"
    tokens_sent: 6
    tokens_received: 4
""",
    ),
    Case(
        name="prompt_failing",
        doc="""\
effects:
  - name: ask
    type: prompt
    template: "Ask something risky."
""",
        replies="""\
prime.ask:
  - error:
      kind: server_error
      status: 500
      message: "scripted failure"
""",
    ),
]


def record_one(case: Case, cof_bin: str) -> None:
    print(f"recording {case.name} ...", file=sys.stderr)
    with tempfile.TemporaryDirectory(prefix="osp-record-") as scratch:
        scratch_path = Path(scratch)
        home = scratch_path / "home"
        home.mkdir()
        work = scratch_path / "work"
        work.mkdir()
        run_dir = scratch_path / "run"
        run_dir.mkdir()

        doc_path = work / "do.yml"
        doc_path.write_text(case.doc, encoding="utf-8")

        # `default_adapter`/`default_model` are Circuitry's own config
        # keys (`cli/config.py`) — a bare `"adapter": "scripted"` is an
        # unknown key the config loader silently ignores, so every run
        # fell back to the built-in `ollama` default instead of the
        # scripted adapter (K1). `runtime.adapters.scripted.replies_file`
        # is always set, even with no replies, so `resolve_effective_
        # settings` has a concrete adapter config to report regardless.
        config: dict = {
            "default_adapter": "scripted",
            "default_model": "scripted-model",
            "runtime": {"adapters": {"scripted": {}}},
        }
        if case.replies is not None:
            replies_path = work / "replies.yaml"
            replies_path.write_text(case.replies, encoding="utf-8")
            config["runtime"]["adapters"]["scripted"]["replies_file"] = "replies.yaml"
        config_path = work / "config.json"
        config_path.write_text(json.dumps(config), encoding="utf-8")

        live_state = run_dir / "state.live.json"
        out_state = run_dir / "state.json"
        events_path = run_dir / "events.jsonl"

        env = dict(os.environ)
        for key in ("OPENAI_API_KEY", "ANTHROPIC_API_KEY", "CYBERDINER_TOKEN", "CYBERDINER_EXPO_URL"):
            env.pop(key, None)
        env["HOME"] = str(home)

        cmd = [
            cof_bin,
            "run",
            str(doc_path),
            "--config",
            str(config_path),
            "--quiet",
            "--live-state",
            str(live_state),
            "--out",
            str(out_state),
            "--events",
            str(events_path),
        ]

        proc = subprocess.Popen(
            cmd,
            cwd=work,
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
        )

        start = time.monotonic()
        snapshots: list[dict] = []
        last_fingerprint: tuple[float, int] | None = None
        pending_signals = sorted(case.signals, key=lambda s: s.after_s)
        deadline = start + HARD_TIMEOUT_S

        while True:
            elapsed = time.monotonic() - start
            while pending_signals and elapsed >= pending_signals[0].after_s:
                step = pending_signals.pop(0)
                try:
                    os.killpg(proc.pid, step.sig)
                except (ProcessLookupError, PermissionError):
                    # The group already exited between this poll and the
                    # scheduled signal (a fast-dying process, or a second
                    # signal queued after a prior one already ended it) --
                    # not an error worth failing the recording over.
                    pass

            if live_state.exists():
                st = live_state.stat()
                fingerprint = (st.st_mtime, st.st_size)
                if fingerprint != last_fingerprint:
                    try:
                        data = json.loads(live_state.read_text(encoding="utf-8"))
                    except (OSError, json.JSONDecodeError):
                        data = None
                    if data is not None:
                        snapshots.append({"t": round(elapsed, 3), "state": data})
                        last_fingerprint = fingerprint

            if proc.poll() is not None:
                break
            if time.monotonic() > deadline:
                os.killpg(proc.pid, signal.SIGKILL)
                proc.wait(timeout=5)
                break
            time.sleep(POLL_INTERVAL_S)

        stdout, stderr = proc.communicate(timeout=5)
        exit_code = proc.returncode

        # One more poll in case the final write landed between the last
        # loop iteration and exit.
        if live_state.exists():
            st = live_state.stat()
            fingerprint = (st.st_mtime, st.st_size)
            if fingerprint != last_fingerprint:
                try:
                    data = json.loads(live_state.read_text(encoding="utf-8"))
                except (OSError, json.JSONDecodeError):
                    data = None
                if data is not None:
                    snapshots.append({"t": round(time.monotonic() - start, 3), "state": data})

        # K1: fail loudly (not just a silently-wrong fixture) if a run's
        # own effective settings didn't actually select the scripted
        # adapter — checked against the final `--out` write when there is
        # one, else the last live-state snapshot (an aborted case has
        # neither; the `runtime` key has never been observed there
        # either, so there is nothing to assert).
        final_state: dict | None = None
        if out_state.exists():
            try:
                final_state = json.loads(out_state.read_text(encoding="utf-8"))
            except (OSError, json.JSONDecodeError):
                final_state = None
        elif snapshots:
            final_state = snapshots[-1]["state"]
        if final_state is not None:
            observed_adapter = final_state.get("runtime", {}).get("effective_settings", {}).get("adapter")
            if observed_adapter is not None:
                assert observed_adapter == "scripted", (
                    f"{case.name}: runtime.effective_settings.adapter was "
                    f"{observed_adapter!r}, not 'scripted' — the scripted "
                    "adapter was never selected (K1)"
                )

        events_text = events_path.read_text(encoding="utf-8") if events_path.exists() else ""

        write_fixture(
            case, doc_path, config_path, work, scratch_path, home, snapshots, stdout, stderr, exit_code, events_text
        )


def scrub(text: str, *scrub_paths: Path) -> str:
    # Longest path first, and each path's **resolved** (symlink-free)
    # form scrubbed ahead of its as-given form: macOS's `tempfile`
    # returns a path under `/var/folders/...`, a symlink to the real
    # `/private/var/folders/...` — `cof` itself resolves paths before
    # writing them into state (`_orchestration_dir`, `orchestration_
    # path`), so the as-given replace alone left `/private<SCRUBBED>`
    # behind every time (K2). A shorter path replaced first could also
    # leave a dangling `<SCRUBBED>/...` inside a longer one still
    # waiting its turn, hence the length sort.
    resolved = [p.resolve() for p in scrub_paths]
    all_paths = sorted({*scrub_paths, *resolved}, key=lambda p: len(str(p)), reverse=True)
    for p in all_paths:
        text = text.replace(str(p), "<SCRUBBED>")
    # A safety net beyond the paths this script itself created: any
    # other absolute/temp path or the real username that leaked in some
    # other way (CLAUDE.md's own rule for committed fixtures).
    text = re.sub(r"/Users/[^/\s\"']+", "<SCRUBBED>", text)
    text = re.sub(r"/home/[^/\s\"']+", "<SCRUBBED>", text)
    text = re.sub(r"/private/var/folders/[^\s\"']+", "<SCRUBBED>", text)
    text = re.sub(r"/var/folders/[^\s\"']+", "<SCRUBBED>", text)
    text = re.sub(r"/private/[^\s\"']+", "<SCRUBBED>", text)
    return re.sub(r"/tmp/[^\s\"']+", "<SCRUBBED>", text)


def write_fixture(
    case: Case,
    doc_path: Path,
    config_path: Path,
    work: Path,
    scratch_path: Path,
    home: Path,
    snapshots: list[dict],
    stdout: bytes,
    stderr: bytes,
    exit_code: int,
    events_text: str = "",
) -> None:
    out_dir = FIXTURES_DIR / case.name
    if out_dir.exists():
        shutil.rmtree(out_dir)
    out_dir.mkdir(parents=True)

    shutil.copy(doc_path, out_dir / "do.yml")
    # Named `config.fixture.json`, not `config.json`: the repository's
    # own `.gitignore` blanket-ignores `config.json` everywhere, to
    # keep a real local config from ever being committed by accident.
    shutil.copy(config_path, out_dir / "config.fixture.json")
    if case.replies is not None:
        (out_dir / "replies.yaml").write_text(case.replies, encoding="utf-8")

    snapshots_text = "\n".join(json.dumps(s, sort_keys=True) for s in snapshots) + ("\n" if snapshots else "")
    snapshots_text = scrub(snapshots_text, work, scratch_path, home)
    (out_dir / "snapshots.jsonl").write_text(snapshots_text, encoding="utf-8")

    if events_text:
        (out_dir / "events.jsonl").write_text(scrub(events_text, work, scratch_path, home), encoding="utf-8")

    stdout_text = scrub(stdout.decode("utf-8", errors="replace"), work, scratch_path, home)
    stderr_text = scrub(stderr.decode("utf-8", errors="replace"), work, scratch_path, home)
    (out_dir / "stdout.txt").write_text(stdout_text, encoding="utf-8")
    (out_dir / "stderr.txt").write_text(stderr_text, encoding="utf-8")

    # Python's own `Popen.returncode` reports a signal-terminated
    # process as a negative number (`-signum`); osp, reading the real
    # OS wait status directly, applies the POSIX `128 + signum`
    # convention instead (`supervise::exit_code`) -- normalize here so
    # the fixture's own exit code is the one osp will actually report.
    normalized_exit_code = 128 + (-exit_code) if exit_code < 0 else exit_code
    (out_dir / "exit_code.txt").write_text(f"{normalized_exit_code}\n", encoding="utf-8")

    meta = {
        "signals": [[s.after_s, s.sig.name] for s in case.signals],
    }
    (out_dir / "meta.json").write_text(json.dumps(meta, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def check_no_leaked_paths() -> int:
    """A standing test, not just a recording-time scrub: fails if any
    committed fixture still contains an absolute/temp path."""
    # No trailing slash required on `/private`/`/var`: the bug this
    # guards against (K2) left exactly `/private<SCRUBBED>` behind, with
    # no second slash for a `/private/`-shaped needle to ever match.
    pattern = re.compile(r"/Users/|/home/[^\s\"']+|/var/folders|/private|(?<!<)/tmp/")
    failures = []
    for path in sorted(FIXTURES_DIR.rglob("*")):
        if not path.is_file():
            continue
        try:
            text = path.read_text(encoding="utf-8")
        except (UnicodeDecodeError, OSError):
            continue
        if pattern.search(text):
            failures.append(path)
    if failures:
        print("fixtures with a leaked path:", file=sys.stderr)
        for f in failures:
            print(f"  {f}", file=sys.stderr)
        return 1
    return 0


def main() -> int:
    if "--check-no-leaked-paths" in sys.argv:
        return check_no_leaked_paths()

    cof_bin = shutil.which("cof")
    if cof_bin is None:
        print("record_fixtures.py: 'cof' not found on PATH", file=sys.stderr)
        return 1

    for case in CASES:
        record_one(case, cof_bin)

    return check_no_leaked_paths()


if __name__ == "__main__":
    raise SystemExit(main())
