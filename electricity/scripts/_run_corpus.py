"""Shared helper for the M0-H VM golden run corpus (issue #431's Test
strategy section, "Golden run corpus"). Not itself a `generate_*.py`
script (the generated-files workflow's glob skips a leading underscore),
but imported by `generate_vm_run_corpus.py` (lane A's own smoke corpus)
and every later lane's run-corpus generator.

Ground truth is a real `cof run --out --events --live-state`
(`python -m circuitry.cli.app run`, the same invocation
`tests/conformance/harness.py` uses for a hermetic CLI subprocess), with a
temporary `HOME`, the case directory as `cwd`, and every `CIRCUITRY_*`/
credential environment variable stripped -- this repository's hermeticity
rule (`LANE-CONTRACT.md`), the same one `electricity-compiler`'s own
`scripts/_compiler_corpus.py` follows.

Records the run's final state (`--out`), its full `--events` sequence, and
stdout/stderr/exit code -- each normalized (a run id/UUID, a timestamp, a
process id, a `--events` `ms`/`ts` field, and the case's own temporary
root all become a stable placeholder) so a generated case can be committed
and still compare byte-for-byte from a different machine or a later run,
matching `tests/conformance/normalize.py`'s own rationale for the
Python/electricity conformance suite -- this module is a separate,
self-contained implementation of that same idea rather than a shared
import, since every other `electricity/scripts/` generator already
follows that same "self-contained helper, not a cross-directory import"
convention (`_compiler_corpus.py`).

Must be run with Python 3.11 (the lane venv locally; `actions/setup-python`
3.11 in CI, same as every other generator in this directory).
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any

#: Mirrors `tests/conformance/harness.py`'s own `CREDENTIAL_ENV_VARS`.
_CREDENTIAL_ENV_VARS = (
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "CYBERDINER_TOKEN",
    "CYBERDINER_EXPO_URL",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "NPM_TOKEN",
)

DEFAULT_TIMEOUT_SECONDS = 30

#: A committed golden file must carry no local path (`LANE-CONTRACT.md`).
_LEAKED_PATH_PATTERNS = [
    re.compile(re.escape(str(Path.home()))),
    re.compile(re.escape(tempfile.gettempdir())),
    re.compile(r"/private/"),
    re.compile(r"(?<![A-Za-z0-9_])[A-Za-z]:(?:\\\\|/(?!/))"),
]


def _sandboxed_env(home_dir: Path) -> dict[str, str]:
    env = {
        k: v
        for k, v in os.environ.items()
        if k not in _CREDENTIAL_ENV_VARS and not k.startswith("CIRCUITRY_")
    }
    env["HOME"] = str(home_dir)
    return env


def run_cof(
    case_dir: Path,
    *,
    orchestration: str = "orchestration.yml",
    config: dict[str, Any] | None = None,
    inputs: dict[str, str] | None = None,
    home_dir: Path,
    timeout_seconds: int = DEFAULT_TIMEOUT_SECONDS,
) -> dict[str, Any]:
    """Runs *orchestration* (inside *case_dir*) the way `cof run --out
    --events --live-state` does, returning the **raw**, not yet
    normalized result: `returncode`, `stdout`, `stderr`, the `--out`
    state (`None` if nothing was written), and the parsed `--events`
    sequence (`[]` if nothing was written). *config*, if given, is
    written to `config.json` inside *case_dir* first.
    """
    out_path = case_dir / "expected.out.json"
    events_path = case_dir / "expected.events.jsonl"
    live_state_path = case_dir / "expected.live_state.json"

    cmd = [
        sys.executable,
        "-m",
        "circuitry.cli.app",
        "run",
        orchestration,
        "--out",
        str(out_path),
        "--events",
        str(events_path),
        "--live-state",
        str(live_state_path),
        "--quiet",
    ]
    if config is not None:
        config_path = case_dir / "config.json"
        config_path.write_text(json.dumps(config), encoding="utf-8")
        cmd += ["--config", str(config_path)]
    for key, value in (inputs or {}).items():
        cmd += ["-e", f"{key}={value}"]

    proc = subprocess.run(
        cmd,
        cwd=case_dir,
        env=_sandboxed_env(home_dir),
        capture_output=True,
        text=True,
        timeout=timeout_seconds,
        check=False,
    )

    state = None
    if out_path.exists():
        state = json.loads(out_path.read_text(encoding="utf-8"))

    events: list[dict[str, Any]] = []
    if events_path.exists():
        events.extend(
            json.loads(line)
            for line in events_path.read_text(encoding="utf-8").splitlines()
            if line.strip()
        )

    live_state = None
    if live_state_path.exists():
        live_state = json.loads(live_state_path.read_text(encoding="utf-8"))

    return {
        "returncode": proc.returncode,
        "stdout": proc.stdout,
        "stderr": proc.stderr,
        "state": state,
        "events": events,
        "live_state": live_state,
    }


_UUID_RE = re.compile(
    r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$", re.IGNORECASE
)
_COMPACT_TS_RE = re.compile(r"^\d{8}_\d{6}$")
_ISO_TS_RE = re.compile(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}")

#: Key name -> placeholder, for a field whose *value* is test-environment
#: dependent (a run id, a timestamp, a process id, an events-stream
#: duration) rather than document-content dependent -- matched by key
#: name alone, regardless of nesting depth, the same approach
#: `tests/conformance/normalize.py` takes for the Python/electricity
#: conformance suite.
_NORMALIZED_KEYS: dict[str, str] = {
    "run_id": "<RUN_ID>",
    "_run_id": "<RUN_ID>",
    "pid": "<PID>",
    "started_at": "<TIMESTAMP>",
    "completed_at": "<TIMESTAMP>",
    "created_at": "<TIMESTAMP>",
    "_timestamp": "<TIMESTAMP>",
    "ts": "<TS>",
    "ms": "<MS>",
    "wall_time_s": "<DURATION>",
}


def _looks_environment_dependent(value: Any) -> bool:
    if isinstance(value, bool):
        return False
    if isinstance(value, (int, float)):
        return True
    if isinstance(value, str):
        return bool(
            _UUID_RE.match(value)
            or _COMPACT_TS_RE.match(value)
            or _ISO_TS_RE.match(value)
        )
    return False


def _normalize(key: str | None, value: Any, root: str) -> Any:
    if isinstance(value, str):
        value = value.replace(root, "<root>")
        placeholder = _NORMALIZED_KEYS.get(key) if key is not None else None
        if placeholder is not None and _looks_environment_dependent(value):
            return placeholder
        return value
    if isinstance(value, dict):
        return {k: _normalize(k, v, root) for k, v in value.items()}
    if isinstance(value, list):
        return [_normalize(key, v, root) for v in value]
    placeholder = _NORMALIZED_KEYS.get(key) if key is not None else None
    if placeholder is not None and _looks_environment_dependent(value):
        return placeholder
    return value


#: `ev` -> a rank used only to make a tree case's own golden JSON
#: reproducible: two tree branches that finish in a different order
#: between two runs of the very same document emit their `start`/`end`
#: pair in a different relative position in the raw `--events` stream
#: (real `ThreadPoolExecutor` scheduling, confirmed directly: the same
#: document's own generated golden was observed to differ run to run
#: before this normalization existed). This is *not* the comparison rule
#: a later lane's own conformance test should use for two *different*
#: engines' `--events` output -- issue #431's own acceptance criteria
#: already specifies that one: "compare the start-before-child /
#: child-end-before-container partial order and the per-path multiset,
#: not the interleaving". This is only what keeps *this* generator's own
#: output stable from one invocation to the next so it can be committed
#: and checked with `--check` at all.
_EVENT_KIND_RANK = {"start": 0, "dispatch": 1, "end": 2}


def canonical_event_order(events: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Reorders *events* into a stable, path-then-kind order (`run_start`
    first, `run_end` last, everything else sorted by its own `path` then
    [`_EVENT_KIND_RANK`]), and renumbers `seq` to match that order --
    see [`_EVENT_KIND_RANK`]'s own doc comment for why a tree case needs
    this at all.
    """
    run_start = [e for e in events if e.get("ev") == "run_start"]
    run_end = [e for e in events if e.get("ev") == "run_end"]
    middle = [e for e in events if e.get("ev") not in ("run_start", "run_end")]
    middle.sort(
        key=lambda e: (e.get("path", ""), _EVENT_KIND_RANK.get(e.get("ev"), 99))
    )
    ordered = run_start + middle + run_end
    renumbered = []
    for index, original in enumerate(ordered):
        copy = dict(original)
        copy["seq"] = index
        renumbered.append(copy)
    return renumbered


def normalize_result(result: dict[str, Any], root: str) -> dict[str, Any]:
    """*result* ([`run_cof`]'s own output), with every environment-
    dependent field replaced by a stable placeholder and *root* (the
    case's own temporary directory) replaced by the literal `<root>`
    everywhere it appears."""
    return {
        "returncode": result["returncode"],
        "stdout": result["stdout"].replace(root, "<root>"),
        "stderr": result["stderr"].replace(root, "<root>"),
        "state": _normalize(None, result["state"], root),
        "events": _normalize(None, result["events"], root),
        "live_state": _normalize(None, result["live_state"], root),
    }


def render_corpus(cases: list[dict[str, Any]]) -> str:
    """JSON-renders *cases* (each already normalized), refusing to
    produce output that still leaks a real local path."""
    text = json.dumps(cases, indent=2, ensure_ascii=False) + "\n"
    for pattern in _LEAKED_PATH_PATTERNS:
        if pattern.search(text):
            raise AssertionError(
                f"generated run-corpus output leaks a local path (matched {pattern.pattern!r})"
            )
    return text


def write_or_check(output_path: Path, text: str, *, check: bool) -> bool:
    """Writes *text* to *output_path*, or (`check=True`) reports whether
    it is already exactly *text* -- `generate_vm_run_corpus.py --check`'s
    own contract, the same one every other `generate_*.py` in this
    directory follows."""
    if check:
        if not output_path.exists():
            print(f"{output_path}: missing (run without --check to generate it)")
            return False
        if output_path.read_text(encoding="utf-8") != text:
            print(f"{output_path}: stale (run without --check to regenerate it)")
            return False
        return True
    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(text, encoding="utf-8")
    print(f"wrote {output_path}")
    return True
