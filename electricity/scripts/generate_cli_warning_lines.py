#!/usr/bin/env python3
"""`cof run`'s own stderr `WARNING:`/`Warning:` lines, for every site
reachable from M0-H's supported subset (issue #442's "Warning lines on
stderr" item of #442): the `on_error: skip`/`continue` and `finally:`
degradation warnings `core/dynamic.py` logs, `core/conditional.py`'s own
`on_error: continue` warning, the CEL "unresolved path" warning
`core/cel_eval.py` logs for a non-strict `if`, `cli/config.py`'s own
"Unknown environment" warning, `core/tool.py::ToolRuntime.
_resolve_timeout_seconds`'s own "Invalid runtime.tools.timeout_seconds"
warning for a `runtime.tools.timeout_seconds` value `int()` rejects,
and the mid-run `--live-state`/`--events` write-failure warnings
`cli/live_state.py`/`cli/events.py` log.

Every case uses a *relative* `--live-state`/`--events` path and a fixed
*cwd*, so `cof run`'s own warning text (which embeds that path verbatim)
never depends on this machine's temporary-directory name -- no
normalization step is needed, unlike `_run_corpus.py`'s own run-corpus
generators, and the committed output carries no local path by
construction. A self-contained `cof run` subprocess per case (this
script's own `_run_cof_stderr`), not `_run_corpus.py`'s shared helper:
that helper always passes its own `--out`/`--events`/`--live-state`
paths, where several cases here need a *specific* (and, for the
events-write-failure case, pre-blocked) one of their own.

No `--live-state`-write-failure case: unlike `--events` (whose own
`open` failure just disables further writes and lets the run continue,
`cli/events.py::EventLog.__init__`'s own `except OSError`),
`--live-state`'s *first* write is synchronous and unconditionally fatal
in both engines (issue #431's run-wiring step 16) -- reproducing it
would need a document that can fail a *later* write after a successful
first one, which M0-H's own supported effects (`json`-tool/`dynamic`/
`if`/`finally` only, no filesystem-mutating tool) cannot construct.
electricity's own code for that later-write case (`live_state.rs::
flush_if_due`/`close`) still logs the same warning `cli/live_state.py`'s
own `_write` does, by inspection; it is just not exercised by an
automated case here. The *first*-write-fails scenario is a separate,
pre-existing gap this script does not cover: `cof run`'s own top-level
`error` for it is the raw `OSError` text (`_replace_file`'s own
unwrapped call, `cli/live_state.py::LiveStateMirror.__call__`'s "first"
branch), where electricity's `fail!` wraps it in its own "Could not
write --live-state ..." prefix -- and cof's own `finally:` still
retries the write once more through `close()`, logging a second
mid-run `WARNING:` plus the usual end-of-run `Warning:` fold, where
electricity's `fail!` returns immediately with neither. Neither engine
changed Python behaviour; this is tracked as a follow-up, not fixed
here.

Must be run with Python 3.11 (the lane venv locally; `actions/setup-python`
3.11 in CI, same as every other generator in this directory). Usage:
    python3 generate_cli_warning_lines.py [--check]
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-cli"
    / "tests"
    / "golden"
    / "warning_lines.json"
)

#: Mirrors `_run_corpus.py`'s own list.
_CREDENTIAL_ENV_VARS = (
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "CYBERDINER_TOKEN",
    "CYBERDINER_EXPO_URL",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "NPM_TOKEN",
)

JSON_PARSE_FAILURE = """\
mode: parse
input: "not json"
"""


def _tool(name: str, *, mode: str = "parse", value: str = "not json", indent: int = 6) -> str:
    pad = " " * indent
    return (
        f"{pad}- name: {name}\n"
        f"{pad}  type: tool\n"
        f"{pad}  provider: json\n"
        f"{pad}  params: {{mode: {mode}, input: {value!r}}}\n"
    )


DYNAMIC_ON_ERROR_SKIP = (
    "effects:\n"
    "  - name: guarded\n"
    "    type: dynamic\n"
    "    on_error: skip\n"
    "    effects:\n" + _tool("bad")
)

DYNAMIC_ON_ERROR_CONTINUE = (
    "effects:\n"
    "  - name: guarded\n"
    "    type: dynamic\n"
    "    on_error: continue\n"
    "    effects:\n" + _tool("bad")
)

DYNAMIC_FINALLY_FAILED_DEGRADED = (
    "effects:\n"
    "  - name: cleanup_case\n"
    "    type: dynamic\n"
    "    on_error: continue\n"
    "    effects:\n"
    + _tool("body", value="{}")
    + "    finally:\n"
    + _tool("cleanup")
)

DYNAMIC_CLEANUP_ALSO_FAILED = (
    "effects:\n"
    "  - name: cleanup_case\n"
    "    type: dynamic\n"
    "    on_error: continue\n"
    "    effects:\n"
    + _tool("body")
    + "    finally:\n"
    + _tool("cleanup", value="also not json")
)

CONDITIONAL_ON_ERROR_CONTINUE = (
    "effects:\n"
    "  - name: gate\n"
    "    type: if\n"
    "    on_error: continue\n"
    "    if:\n"
    "      mode: cel\n"
    '      expr: "state.prime.missing.value == 1"\n'
    "      strict: true\n"
    "    then:\n"
    + _tool("yes_branch", value="{}")
    + "    else:\n"
    + _tool("no_branch", value="{}")
)

CEL_UNRESOLVED_PATH = (
    "effects:\n"
    "  - name: gate\n"
    "    type: if\n"
    "    if:\n"
    "      mode: cel\n"
    '      expr: "state.prime.missing.value == 1"\n'
    "    then:\n"
    + _tool("yes_branch", value="{}")
    + "    else:\n"
    + _tool("no_branch", value="{}")
)

PLAIN_SUCCESS = "effects:\n" + _tool("ok", value="{}", indent=2)

INVALID_TOOL_TIMEOUT_SECONDS = "effects:\n" + _tool("ok", value="{}", indent=2)


def _sandboxed_env(home_dir: Path) -> dict[str, str]:
    env = {
        k: v
        for k, v in os.environ.items()
        if k not in _CREDENTIAL_ENV_VARS and not k.startswith("CIRCUITRY_")
    }
    env["HOME"] = str(home_dir)
    return env


def _run_cof_stderr(
    case_dir: Path,
    *,
    orchestration: str,
    config: dict[str, Any] | None,
    extra_args: list[str],
    touch_blocker: str | None,
) -> str:
    """Runs `cof run orchestration.yml --quiet [extra_args]` inside
    *case_dir* (its own `cwd`, with its own throwaway `home/`), returning
    stderr alone, decoded as text. *config*, if given, is written to
    `config.json` first; *touch_blocker*, if given, is a relative path
    (inside *case_dir*) pre-created as a plain file, so a later
    `mkdir(parents=True)` over it (`--live-state`/`--events`'s own
    directory-creation step) fails with `[Errno 17] File exists`.
    """
    home_dir = case_dir / "home"
    home_dir.mkdir(parents=True)
    (case_dir / "orchestration.yml").write_text(orchestration, encoding="utf-8")
    cmd = [sys.executable, "-m", "circuitry.cli.app", "run", "orchestration.yml", "--quiet"]
    if config is not None:
        (case_dir / "config.json").write_text(json.dumps(config), encoding="utf-8")
        cmd += ["--config", "config.json"]
    if touch_blocker is not None:
        blocker_path = case_dir / touch_blocker
        blocker_path.parent.mkdir(parents=True, exist_ok=True)
        blocker_path.write_text("", encoding="utf-8")
    cmd += extra_args
    proc = subprocess.run(
        cmd,
        cwd=case_dir,
        env=_sandboxed_env(home_dir),
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    return proc.stderr


def build_case(
    *,
    name: str,
    orchestration: str,
    home_root: Path,
    config: dict[str, Any] | None = None,
    extra_args: list[str] | None = None,
    touch_blocker: str | None = None,
    os_error_prefix: str | None = None,
) -> dict[str, Any]:
    """*os_error_prefix*, when given, is the literal text immediately
    before an OS error's own message in one `expected_stderr` line (the
    `events_write_failure` case's own "...disabling further writes: "):
    the OS-specific text after it is never byte-for-byte identical
    between Python's `OSError.__str__` (`[Errno 17] File exists:
    '...'`) and Rust's `io::Error::Display` (`File exists (os error
    17)`), the same long-standing divergence `electricity-compiler`'s
    own prompt-file read errors document -- the consuming Rust test
    compares only up to and including this prefix on the line it
    appears on, verbatim on every other line.
    """
    case_dir = home_root / name
    case_dir.mkdir(parents=True)
    stderr = _run_cof_stderr(
        case_dir,
        orchestration=orchestration,
        config=config,
        extra_args=extra_args or [],
        touch_blocker=touch_blocker,
    )
    return {
        "name": name,
        "orchestration": orchestration,
        "config": config,
        "extra_args": extra_args or [],
        "touch_blocker": touch_blocker,
        "expected_stderr": stderr,
        "os_error_prefix": os_error_prefix,
    }


def render(cases: list[dict[str, Any]]) -> str:
    return json.dumps(cases, indent=2, ensure_ascii=False) + "\n"


def write_or_check(output_path: Path, text: str, *, check: bool) -> bool:
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


def main() -> int:
    check = "--check" in sys.argv[1:]

    with tempfile.TemporaryDirectory(prefix="electricity-cli-warning-lines-") as tmp:
        root = Path(tmp).resolve()
        cases = [
            build_case(
                name="dynamic_on_error_skip",
                orchestration=DYNAMIC_ON_ERROR_SKIP,
                home_root=root,
            ),
            build_case(
                name="dynamic_on_error_continue",
                orchestration=DYNAMIC_ON_ERROR_CONTINUE,
                home_root=root,
            ),
            build_case(
                name="dynamic_finally_failed_degraded",
                orchestration=DYNAMIC_FINALLY_FAILED_DEGRADED,
                home_root=root,
            ),
            build_case(
                name="dynamic_cleanup_also_failed",
                orchestration=DYNAMIC_CLEANUP_ALSO_FAILED,
                home_root=root,
            ),
            build_case(
                name="conditional_on_error_continue",
                orchestration=CONDITIONAL_ON_ERROR_CONTINUE,
                home_root=root,
            ),
            build_case(
                name="cel_unresolved_path",
                orchestration=CEL_UNRESOLVED_PATH,
                home_root=root,
            ),
            build_case(
                name="config_unknown_environment",
                orchestration=PLAIN_SUCCESS,
                home_root=root,
                config={"environment": "staging"},
            ),
            build_case(
                name="invalid_tool_timeout_seconds",
                orchestration=INVALID_TOOL_TIMEOUT_SECONDS,
                home_root=root,
                config={"runtime": {"tools": {"timeout_seconds": "abc"}}},
            ),
            # No `live_state_write_failure` case: `--live-state`'s own
            # *first* write is synchronous and unconditionally fatal
            # (issue #431's run-wiring step 16, both engines), a
            # separate, pre-existing gap from this one -- see this
            # script's own module doc comment.
            build_case(
                name="events_write_failure",
                orchestration=PLAIN_SUCCESS,
                home_root=root,
                extra_args=["--events", "blocker/sub/events.jsonl"],
                touch_blocker="blocker/sub",
                os_error_prefix="--events open failed, disabling further writes: ",
            ),
            build_case(
                name="plain_success_has_no_warnings",
                orchestration=PLAIN_SUCCESS,
                home_root=root,
            ),
        ]

    text = render(cases)
    ok = write_or_check(OUTPUT, text, check=check)
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
