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

#: Key name -> placeholder, replaced unconditionally (never gated on
#: [`_looks_environment_dependent`], unlike [`_NORMALIZED_KEYS`]) --
#: `run_start`'s own `engine` field (`cli/events.py::_engine_label`) is
#: always the installed `cof <version>` text, which doesn't match any of
#: [`_looks_environment_dependent`]'s own shapes but would otherwise go
#: stale on *any* `pyproject.toml` version bump, failing `--check` for a
#: change this corpus has nothing to do with.
_UNCONDITIONALLY_NORMALIZED_KEYS: dict[str, str] = {
    "engine": "<ENGINE>",
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
    if key is not None and key in _UNCONDITIONALLY_NORMALIZED_KEYS:
        return _UNCONDITIONALLY_NORMALIZED_KEYS[key]
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


def _branch_sort_key(
    segment: str, declared: list[str], seen_order: list[str]
) -> tuple[int, int]:
    """Where *segment* (a tree branch's own immediate child-path segment,
    right after its parent dispatch's own path) belongs in canonical
    order: by *declared* position when the caller knows the document's
    own declared branch order for this dispatch (every named-child tree
    case -- `prime.fan_out.left`/`prime.fan_out.right`, no index of any
    kind in the path at all, so nothing *in* the event stream can ever
    recover this order generically); otherwise by `EffectPath`'s own
    `iter_N` placeholder, resolved, which numbers an *iterated* branch's
    own path directly (`prime.loop.0`, `prime.loop.1`, out of scope for
    M0-H's own tree `dynamic` but handled here so a later lane's own
    generator, iterating `each`-style branches, doesn't have to touch
    this helper at all); last, a segment neither names -- not a gap this
    generator's own corpus exercises today -- falls back to first-seen
    order (a no-op, i.e. exactly [`_reorder_branches`]'s pre-ordering
    behaviour for it, not a new source of nondeterminism beyond what
    already existed before this function ran).
    """
    if segment in declared:
        return (0, declared.index(segment))
    try:
        return (1, int(segment))
    except ValueError:
        return (2, seen_order.index(segment))


def _reorder_branches(
    events: list[dict[str, Any]],
    branch_order: dict[str, list[str]] | None = None,
) -> list[dict[str, Any]]:
    """Moves each `dispatch` event's own branch region into canonical
    branch order, recursively (a branch's own events can themselves
    contain a nested dispatch) -- every event outside a dispatch's own
    region (there is none at all in a document with no concurrent
    `dynamic`) keeps its real relative position, so this is a no-op for
    every sequential case ([`canonical_event_order`]'s own doc comment
    has the full rationale for why only this, not a global sort, is
    safe). *branch_order* maps a dispatch's own `path` to its own
    branches' declared order (immediate child-path segments, e.g.
    `{"prime.fan_out": ["left", "right"]}`) -- the generator's own case
    passes this for a named-child tree, since nothing in the events
    themselves names that order ([`_branch_sort_key`]'s own doc comment).
    """
    branch_order = branch_order or {}
    result: list[dict[str, Any]] = []
    index = 0
    while index < len(events):
        event = events[index]
        if event.get("ev") != "dispatch":
            result.append(event)
            index += 1
            continue
        parent_path = event.get("path", "")
        result.append(event)
        index += 1
        region: list[dict[str, Any]] = []
        while index < len(events) and not (
            events[index].get("path") == parent_path
            and events[index].get("ev") == "end"
        ):
            region.append(events[index])
            index += 1
        declared = branch_order.get(parent_path, [])
        prefix = f"{parent_path}."
        branches: dict[str, list[dict[str, Any]]] = {}
        seen_order: list[str] = []
        for branch_event in region:
            path = branch_event.get("path", "")
            if not path.startswith(prefix):
                result.append(branch_event)
                continue
            segment = path[len(prefix) :].split(".", 1)[0]
            if segment not in branches:
                branches[segment] = []
                seen_order.append(segment)
            branches[segment].append(branch_event)
        for segment in sorted(
            seen_order, key=lambda s: _branch_sort_key(s, declared, seen_order)
        ):
            result.extend(_reorder_branches(branches[segment], branch_order))
        if index < len(events):
            result.append(events[index])  # the dispatching container's own `end`
            index += 1
    return result


def canonical_event_order(
    events: list[dict[str, Any]],
    branch_order: dict[str, list[str]] | None = None,
) -> list[dict[str, Any]]:
    """Reorders *events* so a tree `dynamic`'s own branches always come
    out in canonical order rather than real finish order
    ([`_reorder_branches`]), then renumbers `seq` and each `start`/`end`
    pair's own `id` to match -- two tree branches running on separate
    threads can have their `start`/`end` writes (and the numeric `id`
    `cli/events.py`'s own `_next_instance_id` hands each one, under one
    process-wide lock, in whichever thread gets there first) land in
    either relative order between two runs of the very same document
    (confirmed directly: the same document's own generated golden was
    observed to differ run to run before this normalization existed).
    *branch_order* is [`_reorder_branches`]'s own, unchanged.

    This reordering is deliberately narrow -- see [`_reorder_branches`]'s
    own doc comment -- so a sequential case's events (and a tree case's
    own events *outside* a branch region) keep their real emission
    order exactly as `cof run --events` wrote it; renumbering `id`/`seq`
    afterwards is then a no-op for every one of those, since canonical
    order already equals real order there. This is *not* the comparison
    rule a later lane's own conformance test should use for two
    *different* engines' `--events` output -- issue #431's own
    acceptance criteria already specifies that one: "compare the
    start-before-child / child-end-before-container partial order and
    the per-path multiset, not the interleaving". This is only what
    keeps *this* generator's own output stable from one invocation to
    the next so it can be committed and checked with `--check` at all.
    """
    ordered = _reorder_branches(events, branch_order)
    id_map: dict[int, int] = {}
    next_id = 0
    renumbered = []
    for position, original in enumerate(ordered):
        copy = dict(original)
        copy["seq"] = position
        old_id = copy.get("id")
        if old_id is not None:
            if copy.get("ev") == "start":
                id_map[old_id] = next_id
                next_id += 1
            copy["id"] = id_map.get(old_id, old_id)
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
