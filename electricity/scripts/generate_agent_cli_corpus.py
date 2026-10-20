#!/usr/bin/env python3
"""Differential corpus for issue #520 (the agent-CLI runner): every text
and value `electricity-tools::agent_cli` (part A) and, later, a part B
adapter port must reproduce from `circuitry.agent_cli` and its adapter
helpers -- never a re-derivation of their rules, always the real
functions' own answer:

- `pi_command`/`claude_command` argv for every flag combination;
- `claude_json_schema`'s own wrapping, and its `json.dumps` bytes;
- `child_env` over a synthetic environment;
- `parse_claude_output`/`parse_pi_output` results or error texts for a
  battery of scripted `(returncode, stdout, stderr)` cases covering every
  branch named in issue #520's acceptance section;
- the timeout text `run_agent_cli` raises, for a handful of durations;
- `adapters._agent_cli.check_binary`'s own text (part B);
- `adapters.factory._string_list`'s own `ValueError` text (part B).

Must be run with Python 3.11 (the lane venv locally; `actions/setup-python`
3.11 in CI), with Circuitry installed (`pip install -e . -c
electricity/scripts/generator-constraints.txt`, as CI does).

Usage: python3 generate_agent_cli_corpus.py [--check]
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path
from typing import Any
from unittest import mock

from circuitry import agent_cli as agent_cli_module
from circuitry.adapters._agent_cli import check_binary
from circuitry.adapters.factory import _string_list
from circuitry.agent_cli import (
    AgentCliError,
    AgentCliResult,
    child_env,
    claude_command,
    claude_json_schema,
    parse_claude_output,
    parse_pi_output,
    pi_command,
    run_agent_cli,
)

CORPUS = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-tools"
    / "tests"
    / "golden"
    / "agent_cli_corpus.json"
)


def _result_to_dict(result: AgentCliResult) -> dict[str, Any]:
    return {
        "text": result.text,
        "session_id": result.session_id,
        "tokens_sent": result.tokens_sent,
        "tokens_received": result.tokens_received,
        "cost_usd": result.cost_usd,
        "finish_reason": result.finish_reason,
        "structured_output": result.structured_output,
    }


# --- pi_command / claude_command -----------------------------------------


def _pi_command_cases() -> list[dict[str, Any]]:
    scenarios: list[dict[str, Any]] = [
        {},
        {"model": "anthropic/claude-opus"},
        {"thinking": "high"},
        {"tools": True},
        {"session_id": "sess-1"},
        {"session_id": "sess-1", "persist_session": True},
        {"persist_session": True},
        {"extra_args": ["--foo", "bar"]},
        {"model": "m", "thinking": "low", "session_id": "s", "extra_args": ["--x"]},
        {"instruction": "custom instruction"},
    ]
    cases = []
    for kwargs in scenarios:
        argv = pi_command(binary="pi", prompt_file="/prompts/prompt.md", **kwargs)
        cases.append({"kwargs": kwargs, "expected": argv})
    return cases


def _claude_command_cases() -> list[dict[str, Any]]:
    object_schema = {"type": "object", "properties": {"a": {"type": "string"}}}
    array_schema = {"type": "array", "items": {"type": "string"}}
    scenarios: list[dict[str, Any]] = [
        {},
        {"model": "claude-x"},
        {"tools": True},
        {"session_id": "abc"},
        {"session_id": "abc", "persist_session": True},
        {"persist_session": True},
        {"extra_args": ["--extra"]},
        {"strict_mcp": False},
        {"json_schema": object_schema},
        {"json_schema": array_schema},
        {"model": "claude-x", "json_schema": object_schema},
    ]
    cases = []
    for kwargs in scenarios:
        argv = claude_command(binary="claude", **kwargs)
        cases.append({"kwargs": kwargs, "expected": argv})
    return cases


def _claude_json_schema_cases() -> list[dict[str, Any]]:
    schemas: list[dict[str, Any]] = [
        {"type": "object", "properties": {"a": {"type": "string"}}},
        {"type": "array", "items": {"type": "integer"}},
        {"description": "no type at all"},
        {"type": ["object", "null"]},
        {
            "type": "array",
            "items": {"$ref": "#/$defs/item"},
            "$defs": {"item": {"type": "string", "description": "caf\u00e9 \u00e9t\u00e9"}},
        },
        {
            "type": "object",
            "properties": {"x": {"type": "integer"}},
            "definitions": {"old": {"type": "string"}},
        },
    ]
    cases = []
    for schema in schemas:
        wrapped = claude_json_schema(schema)
        cases.append(
            {
                "schema": schema,
                "expected_schema": wrapped,
                "expected_dumps": json.dumps(wrapped),
            }
        )
    return cases


# --- child_env -------------------------------------------------------------


def _child_env_cases() -> list[dict[str, Any]]:
    base_envs: list[dict[str, str]] = [
        {
            "CLAUDECODE": "1",
            "CLAUDE_CODE_ENTRYPOINT": "cli",
            "PI_SESSION_ID": "s1",
            "ANTHROPIC_API_KEY": "secret",
            "ANTHROPIC_AUTH_TOKEN": "secret2",
            "PATH": "/bin",
            "NEUTRAL_VAR": "kept",
        },
        {"ANTHROPIC_API_KEY": "secret", "MY_VAR": "kept"},
        {},
    ]
    unset_envs: list[list[str]] = [
        ["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"],
        [],
        ["MY_VAR"],
    ]
    cases = []
    for base in base_envs:
        for unset in unset_envs:
            result = child_env(tuple(unset), base=base)
            cases.append({"base": base, "unset_env": unset, "expected": result})
    return cases


# --- parse_claude_output ---------------------------------------------------


def _claude_case(
    returncode: int,
    stdout: str,
    stderr: str = "",
    json_schema: dict[str, Any] | None = None,
) -> dict[str, Any]:
    proc = subprocess.CompletedProcess(args=["claude"], returncode=returncode, stdout=stdout, stderr=stderr)
    try:
        result = parse_claude_output(proc, json_schema=json_schema)
        expected: dict[str, Any] = {"ok": _result_to_dict(result)}
    except AgentCliError as exc:
        expected = {"error": str(exc)}
    except (ValueError, OverflowError) as exc:
        expected = {"numeric_error": {"py_class": type(exc).__name__, "message": str(exc)}}
    return {
        "returncode": returncode,
        "stdout": stdout,
        "stderr": stderr,
        "json_schema": json_schema,
        "expected": expected,
    }


def _parse_claude_output_cases() -> list[dict[str, Any]]:
    array_schema = {"type": "array", "items": {"type": "integer"}}
    long_non_ascii = "\u00e9rreur " * 400
    return [
        _claude_case(0, json.dumps({"result": "hello", "is_error": False})),
        _claude_case(
            0,
            json.dumps({"result": "boom", "is_error": True, "subtype": "error_max_turns"}),
        ),
        _claude_case(0, json.dumps({"result": "", "is_error": True, "subtype": ""})),
        # `is_error` must be exactly `true` (`is True` in Python): 1 and 1.0 are answers.
        _claude_case(0, json.dumps({"result": "one", "is_error": 1, "subtype": "error_max_turns"})),
        _claude_case(0, json.dumps({"result": "one point oh", "is_error": 1.0})),
        _claude_case(0, json.dumps({"result": "", "is_error": True, "subtype": "success"})),
        _claude_case(
            2,
            json.dumps({"result": "x", "is_error": True, "subtype": "error_during_execution"}),
            "stderr ignored",
        ),
        _claude_case(2, json.dumps({"result": "ok", "is_error": False}), "oops"),
        _claude_case(0, "not json at all"),
        _claude_case(1, "not json", long_non_ascii),
        _claude_case(
            0,
            "log line one\nlog line two\n" + json.dumps({"result": "final", "is_error": False}),
        ),
        _claude_case(0, "[1, 2, 3]\n" + json.dumps({"result": "after array", "is_error": False})),
        _claude_case(
            0,
            json.dumps({"result": "ignored", "is_error": False, "structured_output": None}),
        ),
        _claude_case(
            0,
            json.dumps({"result": "ignored", "is_error": False, "structured_output": False}),
        ),
        _claude_case(
            0,
            json.dumps({"result": "ignored", "is_error": False, "structured_output": 0}),
        ),
        _claude_case(
            0,
            json.dumps(
                {
                    "result": "```json\n[1,2]\n```",
                    "is_error": False,
                    "structured_output": {"value": [1, 2]},
                }
            ),
            json_schema=array_schema,
        ),
        _claude_case(
            0,
            json.dumps(
                {
                    "result": "ignored",
                    "is_error": False,
                    "structured_output": {"value": [1, 2]},
                }
            ),
        ),
        _claude_case(
            0,
            json.dumps(
                {
                    "result": "ignored",
                    "is_error": False,
                    "structured_output": {"other": 1},
                }
            ),
            json_schema=array_schema,
        ),
        _claude_case(
            0,
            json.dumps(
                {
                    "result": "ok",
                    "is_error": False,
                    "usage": {
                        "input_tokens": 10,
                        "cache_read_input_tokens": 5,
                        "cache_creation_input_tokens": 2,
                        "output_tokens": 7,
                    },
                    "total_cost_usd": 0.5,
                    "stop_reason": "end_turn",
                }
            ),
        ),
        _claude_case(
            0,
            json.dumps(
                {
                    "result": "ok",
                    "is_error": False,
                    "usage": {"input_tokens": True, "output_tokens": 3},
                }
            ),
        ),
        _claude_case(
            0,
            json.dumps(
                {
                    "result": "ok",
                    "is_error": False,
                    "usage": {"input_tokens": 1.9, "output_tokens": -2.9},
                }
            ),
        ),
        _claude_case(
            0,
            json.dumps({"result": "ok", "is_error": False, "usage": {"input_tokens": float("nan")}})
            .replace("NaN", "NaN"),
        ),
        _claude_case(
            0,
            '{"result": "ok", "is_error": false, "usage": {"input_tokens": Infinity}}',
        ),
        _claude_case(-9, ""),
        _claude_case(0, json.dumps({"result": "ok", "is_error": False, "session_id": 5})),
        _claude_case(
            0,
            json.dumps({"result": "ok", "is_error": False, "stop_reason": "max_tokens"}),
        ),
        _claude_case(
            0,
            json.dumps({"result": "ok", "is_error": False, "stop_reason": 5}),
        ),
        _claude_case(1, "", "é" * 2000),
        _claude_case(1, "", "é" * 2001),
        _claude_case(0, json.dumps({"result": "  spaced result  ", "is_error": False})),
    ]


# --- parse_pi_output --------------------------------------------------------


def _message_end(message: dict[str, Any]) -> str:
    return json.dumps({"type": "message_end", "message": message})


# `json.dumps` always escapes a control character (`\x00`-`\x1f`)
# regardless of `ensure_ascii`, and with `ensure_ascii=False` leaves every
# other character -- including the Unicode line separators -- raw. Either
# way the escaped text, if any, is exactly what dumping the one character
# alone produces, so replacing that substring reinjects the raw character
# for a case that needs `py_splitlines` to see it literally in the full
# stdout text (issue #520 settled question 8), never a re-derivation of
# which characters `json.dumps` escapes.
def _message_end_with_raw_char(content: str, char: str) -> str:
    dumped = json.dumps({"type": "message_end", "message": {"role": "assistant", "content": content}}, ensure_ascii=False)
    escaped = json.dumps(char)[1:-1]
    return dumped.replace(escaped, char)


def _pi_case(stdout: str, returncode: int = 0, stderr: str = "") -> dict[str, Any]:
    proc = subprocess.CompletedProcess(args=["pi"], returncode=returncode, stdout=stdout, stderr=stderr)
    try:
        result = parse_pi_output(proc)
        expected: dict[str, Any] = {"ok": _result_to_dict(result)}
    except AgentCliError as exc:
        expected = {"error": str(exc)}
    except (ValueError, OverflowError) as exc:
        expected = {"numeric_error": {"py_class": type(exc).__name__, "message": str(exc)}}
    return {"returncode": returncode, "stdout": stdout, "stderr": stderr, "expected": expected}


def _parse_pi_output_cases() -> list[dict[str, Any]]:
    assistant = lambda text, **extra: {"role": "assistant", "content": text, **extra}  # noqa: E731
    return [
        _pi_case(_message_end(assistant("hello"))),
        _pi_case(
            _message_end(
                {
                    "role": "assistant",
                    "content": [
                        {"type": "text", "text": " hello "},
                        {"type": "text", "text": "world "},
                        {"type": "other", "text": "ignored"},
                    ],
                }
            )
        ),
        _pi_case(
            "\n".join(
                [
                    _message_end(assistant("first")),
                    _message_end({"role": "user", "content": "ignored"}),
                    _message_end(assistant("second")),
                ]
            )
        ),
        _pi_case(
            _message_end(assistant("", stopReason="error", errorMessage="auth failed")),
            returncode=1,
        ),
        _pi_case(
            _message_end(assistant("", stopReason="error")),
            returncode=1,
        ),
        _pi_case(
            _message_end(assistant("partial text", stopReason="aborted")),
            returncode=1,
        ),
        _pi_case(
            _message_end(assistant("", stopReason="aborted")),
            returncode=1,
        ),
        _pi_case(""),
        _pi_case("", returncode=0, stderr="nothing happened"),
        _pi_case(_message_end(assistant("ok")), returncode=1, stderr="crashed after answering"),
        _pi_case(_message_end(assistant("truncated", stopReason="length"))),
        _pi_case("\n\n   \n" + _message_end(assistant("hi")) + "\nnot json\n[1, 2]\n"),
        _pi_case(
            json.dumps({"type": "session", "id": "pi-session-1"})
            + "\n"
            + _message_end(assistant("with session"))
        ),
        _pi_case(
            _message_end(
                {
                    "role": "assistant",
                    "content": "a",
                    "usage": {"input": 3, "cacheRead": 1, "cacheWrite": 1, "output": 2, "cost": {"total": 0.25}},
                }
            )
            + "\n"
            + _message_end(
                {
                    "role": "assistant",
                    "content": "b",
                    "usage": {"input": 4, "output": 1, "cost": 0.25},
                }
            )
        ),
        _pi_case(
            _message_end(
                {"role": "assistant", "content": "c", "usage": {"input": True, "output": 3}}
            )
        ),
        _pi_case(
            _message_end(assistant("fallback"))
            + "\n"
            + _message_end_with_raw_char("line1\u2028line2", "\u2028")
        ),
        _pi_case(
            _message_end(assistant("fallback"))
            + "\n"
            + _message_end_with_raw_char("line1\x1cline2", "\x1c")
        ),
        _pi_case(
            _message_end(assistant("fallback"))
            + "\n"
            + _message_end_with_raw_char("line1\x85line2", "\x85")
        ),
        _pi_case(
            _message_end(
                {
                    "role": "assistant",
                    "content": "x",
                    "usage": {"input": float("nan")},
                }
            )
        ),
        _pi_case(
            _message_end(assistant("\xa0\x1cstripped\x1f\xa0"))
        ),
    ]


# --- the timeout text run_agent_cli raises --------------------------------


def _timeout_text(binary: str, timeout_seconds: float) -> str:
    """`run_agent_cli`'s own timeout text, never a copied f-string: a
    `run_tracked` that always raises `subprocess.TimeoutExpired` makes
    `run_agent_cli` build the real sentence itself (PR #525's review
    -- the docstring above already promised this, but the
    `:g}s` text used to be re-derived here instead).
    """
    cmd = [binary]

    def fake_run_tracked(*_args: Any, **_kwargs: Any) -> Any:
        raise subprocess.TimeoutExpired(cmd=cmd, timeout=timeout_seconds)

    with mock.patch.object(agent_cli_module, "run_tracked", fake_run_tracked):
        try:
            run_agent_cli(cmd, env={}, timeout_seconds=timeout_seconds)
        except AgentCliError as exc:
            return str(exc)
    raise AssertionError("run_agent_cli must raise on a timeout")


def _timeout_text_cases() -> list[dict[str, Any]]:
    seconds = [
        1,
        120,
        999999,
        1000000,
        4294967295,
        # Ties at the 6th significant digit, where a mantissa computed by
        # dividing by `10 ** exponent` can round the wrong way or miss a
        # carry into the next exponent (PR #525's review).
        9999995,
        1000015,
        2000025,
        1500005,
        2500005,
        1234565,
    ]
    binaries = ["claude", "/usr/local/bin/pi"]
    return [
        {
            "binary": binary,
            "timeout_seconds": timeout_seconds,
            "expected": _timeout_text(binary, timeout_seconds),
        }
        for binary in binaries
        for timeout_seconds in seconds
    ]


# --- check_binary / _string_list (part B, reused from this corpus) -------


def _check_binary_cases() -> list[dict[str, Any]]:
    scenarios = [
        {"adapter_name": "claude_code", "binary": "circuitry-test-missing-claude"},
        {"adapter_name": "pi", "binary": "circuitry-test-missing-pi"},
    ]
    cases = []
    for scenario in scenarios:
        result = check_binary(scenario["adapter_name"], scenario["binary"])
        cases.append(
            {
                "adapter_name": scenario["adapter_name"],
                "binary": scenario["binary"],
                "expected_ok": result.ok,
                "expected_missing": result.missing,
                "expected_message": result.message,
            }
        )
    return cases


def _string_list_cases() -> list[dict[str, Any]]:
    configs: list[dict[str, Any]] = [
        {"extra_args": "not-a-list"},
        {"extra_args": 5},
        {"extra_args": [1, 2]},
        {"extra_args": [{"a": 1}]},
        {"extra_args": {"a": 1}},
        {"extra_args": None},
        {},
        {"extra_args": ["--a", "--b"]},
    ]
    cases = []
    for cfg in configs:
        try:
            result = list(_string_list(cfg, "extra_args", adapter_name="pi"))
            cases.append({"cfg": cfg, "expected_error": None, "expected_value": result})
        except ValueError as exc:
            cases.append({"cfg": cfg, "expected_error": str(exc), "expected_value": None})
    return cases


def build_corpus() -> dict[str, Any]:
    return {
        "pi_command_cases": _pi_command_cases(),
        "claude_command_cases": _claude_command_cases(),
        "claude_json_schema_cases": _claude_json_schema_cases(),
        "child_env_cases": _child_env_cases(),
        "parse_claude_output_cases": _parse_claude_output_cases(),
        "parse_pi_output_cases": _parse_pi_output_cases(),
        "timeout_text_cases": _timeout_text_cases(),
        "check_binary_cases": _check_binary_cases(),
        "string_list_cases": _string_list_cases(),
    }


def _write_or_check(output: Path, data: dict[str, Any]) -> bool:
    encoded = (json.dumps(data, indent=2, sort_keys=False, allow_nan=False) + "\n").encode("utf-8")
    output.parent.mkdir(parents=True, exist_ok=True)
    if "--check" in sys.argv[1:]:
        current = output.read_bytes() if output.exists() else b""
        if current != encoded:
            print(f"{output} is stale; run without --check to regenerate", file=sys.stderr)
            return False
        return True
    output.write_bytes(encoded)
    print(f"wrote {output}")
    return True


def main() -> int:
    ok = _write_or_check(CORPUS, build_corpus())
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
