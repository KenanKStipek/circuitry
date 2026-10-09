#!/usr/bin/env python3
"""Generate the golden corpus for lane C of issue #431's own
`electricity-vm::exec::tool::execute_tool` -- a close port of
`core/tool.py::ToolRuntime.execute` -- by actually running that real
Python method against a real `circuitry.core.store.Store`, for every
branch the function can take: success, a tool error (`on_error: fail`
propagating it), `on_error: skip`/`continue`, retries exhausted against
an always-failing CEL `expect:`, an always-passing `expect:`, an
allowlist refusal, a `params_json` merge, a non-string param key end to
end, redaction surfacing in `meta.params_rendered`, and the
`allowed_commands` dispatch-time literal check (a templated/non-string
list item -- a top-level or list-item `{from: ...}` value is *not*
covered here: `electricity-compiler::compile::params::check_no_reference`
already rejects that at compile time, before a `ParamNode` tree like
this corpus's own cases ever reaches `execute_tool`).

Every `retries:` case uses `backoff_ms: 0` so the real wait (and this
corpus's own expected values) are deterministic regardless of
`random.uniform`'s own jitter -- a zero ceiling always waits zero,
on both sides of the port.

Must be run with Python 3.11 (the lane venv locally; `actions/setup-python`
3.11 in CI). Usage: python3 generate_tool_execute_corpus.py [--check]
"""

from __future__ import annotations

import json
import struct
import sys
from pathlib import Path
from typing import Any

from circuitry.core.expect import ExpectDef
from circuitry.core.prompt import RetryPolicyDef
from circuitry.core.store import Store
from circuitry.core.tool import ToolDefinition, ToolRuntime

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-vm"
    / "tests"
    / "golden"
    / "tool_execute_corpus.json"
)


# ---------------------------------------------------------------------
# Tagged value encoding (matches generate_tool_runtime_corpus.py's)
# ---------------------------------------------------------------------


def encode(value: object) -> dict:
    if value is None:
        return {"t": "none"}
    if isinstance(value, bool):
        return {"t": "bool", "v": value}
    if isinstance(value, int):
        return {"t": "int", "v": str(value)}
    if isinstance(value, float):
        bits = struct.unpack("<Q", struct.pack("<d", value))[0]
        return {"t": "float", "bits": format(bits, "016x")}
    if isinstance(value, str):
        return {"t": "str", "v": value}
    if isinstance(value, list):
        return {"t": "list", "v": [encode(item) for item in value]}
    if isinstance(value, dict):
        return {"t": "dict", "v": [[encode(k), encode(v)] for k, v in value.items()]}
    raise TypeError(f"no tagged encoding for {type(value)!r}")


def _normalize_timestamps(value: object) -> object:
    """Replaces every `created_at`/`completed_at` leaf under *value* with a
    fixed placeholder -- this generator's own real wall-clock timestamps
    would otherwise make the committed corpus change on every run,
    failing `--check` even when nothing behavioural changed. Mirrors
    `electricity/scripts/_run_corpus.py`'s own normalizer, which the Rust
    side of this corpus's own comparison also applies (so either a
    pre-normalized or a real timestamp on that side compares equal).
    """
    if isinstance(value, dict):
        return {
            k: (
                "<TIMESTAMP>"
                if k in ("created_at", "completed_at") and isinstance(v, str)
                else _normalize_timestamps(v)
            )
            for k, v in value.items()
        }
    if isinstance(value, list):
        return [_normalize_timestamps(v) for v in value]
    return value


# ---------------------------------------------------------------------
# One case: build a ToolDefinition, run it against a real Store, record
# whether it raised and the resulting node.
# ---------------------------------------------------------------------


def run_case(
    *,
    name: str,
    on_error: str = "fail",
    params: dict[str, Any] | None = None,
    params_json: str | None = None,
    retries: RetryPolicyDef | None = None,
    expect: ExpectDef | None = None,
    runtime_config: dict[str, Any] | None = None,
    ctx: dict[str, Any] | None = None,
) -> dict:
    defn = ToolDefinition(
        name=name,
        provider="json",
        on_error=on_error,  # type: ignore[arg-type]
        params=params or {},
        params_json=params_json,
        retries=retries,
        expect=expect,
    )
    store = Store(state={})
    rt = ToolRuntime(defn, runtime_config=runtime_config or {})
    error: str | None = None
    try:
        rt.execute(store=store, ctx=ctx or {})
    except Exception as exc:  # recording whatever ToolRuntime raises
        error = str(exc)
    return {
        "case": name,
        "on_error": on_error,
        "params": encode(params or {}),
        "params_json": params_json,
        "retries": (
            {"max_attempts": retries.max_attempts, "backoff_ms": retries.backoff_ms}
            if retries is not None
            else None
        ),
        "expect": (
            {"mode": expect.mode, "expr": expect.expr} if expect is not None else None
        ),
        "runtime_config": encode(runtime_config or {}),
        "ctx": encode(ctx or {}),
        "raised": error is not None,
        "error": error,
        "node": encode(_normalize_timestamps(store.state.get(name))),
    }


def build_cases() -> list[dict]:
    cases: list[dict] = []

    cases.append(
        run_case(
            name="success",
            params={"mode": "stringify", "input": {"a": 1}},
        )
    )
    cases.append(
        run_case(
            name="tool_error_fail",
            params={"mode": "bogus"},
        )
    )
    cases.append(
        run_case(
            name="tool_error_skip",
            on_error="skip",
            params={"mode": "bogus"},
        )
    )
    cases.append(
        run_case(
            name="tool_error_continue",
            on_error="continue",
            params={"mode": "bogus"},
        )
    )
    cases.append(
        run_case(
            name="expect_always_fails_exhausts_retries",
            on_error="skip",
            params={"mode": "parse", "input": "1"},
            retries=RetryPolicyDef(max_attempts=3, backoff_ms=0),
            expect=ExpectDef(mode="cel", expr="value == 2"),
        )
    )
    cases.append(
        run_case(
            name="expect_always_passes_first_attempt",
            params={"mode": "parse", "input": "2"},
            expect=ExpectDef(mode="cel", expr="value == 2"),
        )
    )
    cases.append(
        run_case(
            name="allowlist_denial",
            params={"mode": "parse", "input": "{}"},
            runtime_config={"_allowlists": {"tools": ["other"]}},
        )
    )
    cases.append(
        run_case(
            name="params_json_merge",
            params={"mode": "stringify", "input": {"a": 1}},
            params_json='{"input": {"b": 2}}',
        )
    )
    cases.append(
        run_case(
            name="non_string_param_keys",
            params={"mode": "stringify", "input": {True: "yes-value", 1: "one-value"}},  # noqa: F601
        )
    )
    cases.append(
        run_case(
            name="redaction_in_params_rendered",
            params={"mode": "stringify", "input": {"api_key": "super-secret", "name": "ok"}},
        )
    )
    cases.append(
        run_case(
            name="allowed_commands_templated_item_refused",
            params={
                "mode": "parse",
                "input": "{}",
                "allowed_commands": ["rm {{input.target}}"],
            },
            ctx={"input": {"target": "x"}},
        )
    )
    cases.append(
        run_case(
            name="allowed_commands_non_string_item_refused",
            params={"mode": "parse", "input": "{}", "allowed_commands": [1, 2]},
        )
    )
    cases.append(
        run_case(
            name="params_json_allowed_commands_override_refused",
            params={"mode": "parse", "input": "{}"},
            params_json='{"allowed_commands": ["ls"]}',
        )
    )

    return cases


def render(cases: list[dict]) -> str:
    return json.dumps({"tool_execute_cases": cases}, indent=2, ensure_ascii=False, sort_keys=True) + "\n"


def write_or_check(path: Path, text: str, *, check: bool) -> bool:
    path.parent.mkdir(parents=True, exist_ok=True)
    if check:
        current = path.read_text() if path.exists() else ""
        if current != text:
            print(f"{path} is stale; run without --check to regenerate", file=sys.stderr)
            return False
        return True
    path.write_text(text)
    print(f"wrote {path}")
    return True


def main() -> int:
    check = "--check" in sys.argv[1:]
    text = render(build_cases())
    return 0 if write_or_check(OUTPUT, text, check=check) else 1


if __name__ == "__main__":
    raise SystemExit(main())
