#!/usr/bin/env python3
"""Differential corpus for lane C of issue #459 (#448 lane C): the process
runtime plus `shell`/`awk`/`imagemagick`/`ffmpeg`.

Every case here asks Circuitry's own real code for the answer -- never a
re-derivation of its rules -- so the Rust port (`electricity-tools`) is
checked against the real functions, not against this script's own
understanding of them:

- `shlex.split`/`shlex.quote`, written to `electricity-value`'s own golden
  file (`crates/electricity-value/tests/golden/shlex_corpus.json`);
- `plugins.ffmpeg`'s own `_escape_drawtext_text`/`_build_drawtext_filter`/
  `_check_safe`/`_check_filter_safe`;
- `plugins.shell.ShellPlugin.execute`'s own pre-spawn validation (every
  message up to the `resolve_binary` call, which every case here is
  designed to never reach -- a bad `command`/`allowed_commands`/`args`
  shape always raises first);
- `plugins._subprocess.plugin_env_override`/`resolve_plugin_binary`/
  `check_binary`;
- Python's own `str()` of synthetic arg values (`_subprocess._validate_args`'s
  own conversion, and `ffmpeg.py`'s `str(flags)`/`str(map)`/`str(extra)`);
- `bytes.decode("utf-8")`'s own exact `UnicodeDecodeError` text, for a
  battery of synthetic invalid byte strings (text-mode decoding).

Must be run with Python 3.11 (the lane venv locally; `actions/setup-python`
3.11 in CI), with Circuitry installed (`pip install -e . -c
electricity/scripts/generator-constraints.txt`, as CI does).

Usage: python3 generate_process_tools_corpus.py [--check]
"""

from __future__ import annotations

import json
import shlex
import sys
from pathlib import Path
from typing import Any

from circuitry.plugins._subprocess import (
    check_binary,
    plugin_env_override,
    resolve_plugin_binary,
)
from circuitry.plugins.ffmpeg import (
    _build_drawtext_filter,
    _check_filter_safe,
    _check_safe,
    _escape_drawtext_text,
)
from circuitry.plugins.shell import ShellPlugin

VALUE_CORPUS = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-value"
    / "tests"
    / "golden"
    / "shlex_corpus.json"
)
TOOLS_CORPUS = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-tools"
    / "tests"
    / "golden"
    / "process_tools_corpus.json"
)


def _shlex_split_cases() -> list[dict[str, Any]]:
    inputs = [
        "",
        "   ",
        "ffmpeg -i in.mp4 out.mp4",
        "a 'b c' \"d e\"",
        "a '' b",
        "'abc'def",
        "'it'\"'\"'s'",
        "a\\ b",
        '"a\\$b"',
        '"a\\"b"',
        '"a\\\\b"',
        "café --flag 'naïve value'",
        "-vf scale=100:100",
        "'unterminated",
        "trailing\\",
        '"unterminated double',
        "a\\",
        "'\\n'",
        "--x=1 --y=2",
    ]
    cases = []
    for s in inputs:
        try:
            expected: dict[str, Any] = {"ok": shlex.split(s)}
        except ValueError as exc:
            expected = {"err": str(exc)}
        cases.append({"input": s, "expected": expected})
    return cases


def _shlex_quote_cases() -> list[dict[str, Any]]:
    inputs = [
        "",
        "plain-word_1.txt",
        "a b",
        "it's",
        "café",
        "/abs/path/to/binary",
        "key=value,more:stuff",
        "semi;colon",
        "a\nb",
        "100%",
    ]
    return [{"input": s, "expected": shlex.quote(s)} for s in inputs]


def _drawtext_escape_cases() -> list[dict[str, Any]]:
    texts = [
        "",
        "plain text",
        "say \"hi\" it's\nme",
        "line1\r\nline2",
        "100% done\\path",
        "unicode \u2028 line separator",
    ]
    return [{"text": t, "expected": _escape_drawtext_text(t)} for t in texts]


def _drawtext_filter_cases() -> list[dict[str, Any]]:
    configs: list[dict[str, Any]] = [
        {"text": "hello"},
        {"text": "\"hi.\"", "x": "10", "y": "20"},
        {"text": "'quoted'", "fontsize": 24, "fontcolor": "white"},
        {"text": "multi\nline\r\ntext", "box": "1", "boxcolor": "black@0.5"},
    ]
    return [{"cfg": cfg, "expected": _build_drawtext_filter(cfg)} for cfg in configs]


def _check_safe_cases() -> list[dict[str, Any]]:
    values = [
        ("input.mp4", "input", None),
        ("out;rm -rf /", "output", None),
        ("-vf scale=100", "flags", None),
        ("a && b", "flags", None),
        ("a | b", "flags", None),
        ("a > b", "flags", None),
        ("a < b", "flags", None),
        ("`cmd`", "flags", None),
        ("$(cmd)", "flags", None),
        ("a\nb", "flags", None),
    ]
    cases = []
    for value, field, _ in values:
        try:
            _check_safe(value, field)
            error = None
        except ValueError as exc:
            error = str(exc)
        cases.append({"value": value, "field": field, "expected_error": error})
    return cases


def _check_filter_safe_cases() -> list[dict[str, Any]]:
    values = [
        "scale=100:100",
        "a|b;c",
        "a && b",
        "a > b",
        "`cmd`",
        "$(cmd)",
        "a\nb",
    ]
    cases = []
    for value in values:
        try:
            _check_filter_safe(value, "filter_complex")
            error = None
        except ValueError as exc:
            error = str(exc)
        cases.append({"value": value, "expected_error": error})
    return cases


#: Every case here must fail *before* `ShellPlugin.execute` ever resolves
#: a binary -- a bad `command`/`allowed_commands`/`args` shape always
#: raises first, so no process ever runs while generating this corpus.
def _shell_validation_cases() -> list[dict[str, Any]]:
    scenarios: list[dict[str, Any]] = [
        {"params": {}, "pin": None},
        {"params": {"command": "   "}, "pin": None},
        {"params": {"command": 5}, "pin": None},
        {"params": {"command": "ls; rm -rf /"}, "pin": None},
        {"params": {"command": "ls/../etc"}, "pin": None},
        {"params": {"command": "rm"}, "pin": None},
        {"params": {"command": "rm", "allowed_commands": ["rm"]}, "pin": ["ls"]},
        {"params": {"command": "ls", "allowed_commands": "nope"}, "pin": None},
        {"params": {"command": "ls", "allowed_commands": [1, 2]}, "pin": None},
        {"params": {"command": "ls", "args": "nope"}, "pin": None},
        {"params": {"command": "ls", "args": ["a\x00b"]}, "pin": None},
        {"params": {"command": "ls", "args": ["a\nb"]}, "pin": None},
        {"params": {"command": "yt-dlp"}, "pin": None},
    ]
    cases = []
    for scenario in scenarios:
        pin = scenario["pin"]
        plugin = ShellPlugin(
            pinned_allowed_commands=tuple(pin) if pin is not None else None
        )
        error_class = None
        error_message = None
        try:
            plugin.execute(params=scenario["params"], timeout_seconds=1)
        except Exception as exc:  # recording the exact class/message
            error_class = type(exc).__name__
            error_message = str(exc)
        cases.append(
            {
                "params": scenario["params"],
                "pin": pin,
                "expected_error_class": error_class,
                "expected_error_message": error_message,
            }
        )
    return cases


def _str_conversion_cases() -> list[dict[str, Any]]:
    values: list[Any] = [
        True,
        False,
        1,
        -1,
        1.0,
        1.5,
        None,
        "already a string",
        [1, 2, "three"],
        {"a": 1},
    ]
    return [{"value": v, "expected": str(v)} for v in values]


def _plugin_env_override_cases() -> list[dict[str, Any]]:
    configs: list[dict[str, Any]] = [
        {},
        {"env": None},
        {"env": {}},
        {"env": "not-a-mapping"},
        {"env": [1, 2]},
        {"env": {"A": "1", "B": 2}},
        {"env": {"A": True}},
    ]
    cases = []
    for cfg in configs:
        try:
            result = plugin_env_override(cfg, plugin_name="awk")
            cases.append({"cfg": cfg, "expected_error": None, "expected_env": result})
        except ValueError as exc:
            cases.append(
                {"cfg": cfg, "expected_error": str(exc), "expected_env": None}
            )
    return cases


def _resolve_plugin_binary_cases() -> list[dict[str, Any]]:
    # Deliberately no "found via a real PATH search" case here: which
    # binary (if any) `awk`/`gawk`/`mawk` resolves to depends on the
    # machine that runs this generator, so a golden value baked from it
    # would not reproduce on CI's own image (the issue's own test plan:
    # "candidate fallback order is tested in Rust with a controlled
    # PATH, not in conformance") -- both cases below are fully
    # deterministic regardless of what's actually installed.
    scenarios = [
        {
            "plugin_name": "awk",
            "candidates": ["definitely-not-a-real-binary"],
            "configured": None,
        },
        {
            "plugin_name": "awk",
            "candidates": ["awk"],
            "configured": "relative/path",
        },
    ]
    cases = []
    for scenario in scenarios:
        try:
            resolve_plugin_binary(
                plugin_name=scenario["plugin_name"],
                candidates=scenario["candidates"],
                configured=scenario["configured"],
            )
            raise AssertionError("expected this scenario to fail deterministically")
        except RuntimeError as exc:
            cases.append({**scenario, "expected_error": str(exc)})
    return cases


def _check_binary_cases() -> list[dict[str, Any]]:
    # Same reproducibility note as `_resolve_plugin_binary_cases`: no
    # case here depends on what's actually installed on the machine
    # that runs this generator.
    scenarios = [
        {
            "candidates": ("definitely-not-a-real-binary",),
            "label": "definitely-not-a-real-binary",
            "configured": None,
            "plugin_name": "awk",
        },
        {
            "candidates": ("ffmpeg",),
            "label": "ffmpeg",
            "configured": "relative/path",
            "plugin_name": "ffmpeg",
        },
    ]
    cases = []
    for scenario in scenarios:
        result = check_binary(
            scenario["candidates"],
            label=scenario["label"],
            configured=scenario["configured"],
            plugin_name=scenario["plugin_name"],
        )
        cases.append(
            {
                "candidates": list(scenario["candidates"]),
                "label": scenario["label"],
                "configured": scenario["configured"],
                "plugin_name": scenario["plugin_name"],
                "expected_ok": result.ok,
                "expected_missing": result.missing,
                "expected_message": result.message,
            }
        )
    return cases


#: A battery of synthetic byte strings covering every branch of the
#: UTF-8 decoder's own error table (invalid start byte, invalid/missing
#: continuation byte, overlong/surrogate/out-of-range narrowing for
#: E0/ED/F0/F4) plus several valid strings as negative cases.
_UTF8_BYTE_CASES: list[bytes] = [
    b"hello world",
    "héllo wörld".encode(),
    "日本語".encode(),
    b"\xff",
    b"\xfe",
    b"abc\xffdef",
    b"\xc2",  # truncated 2-byte sequence (unexpected end of data)
    b"\xc2\x00",  # invalid continuation byte
    b"\xe0\x9f\x80",  # overlong 3-byte (E0's own narrowed range excludes this)
    b"\xed\xa0\x80",  # a UTF-16 surrogate half, invalid in UTF-8
    b"\xf0\x80\x80\x80",  # overlong 4-byte
    b"\xf4\x90\x80\x80",  # past U+10FFFF
    b"\x00",
    b"",
]


def _utf8_decode_cases() -> list[dict[str, Any]]:
    cases = []
    for data in _UTF8_BYTE_CASES:
        try:
            text = data.decode("utf-8")
            cases.append({"bytes_hex": data.hex(), "expected_error": None, "expected_text": text})
        except UnicodeDecodeError as exc:
            cases.append(
                {"bytes_hex": data.hex(), "expected_error": str(exc), "expected_text": None}
            )
    return cases


def build_tools_corpus() -> dict[str, Any]:
    return {
        "drawtext_escape_cases": _drawtext_escape_cases(),
        "drawtext_filter_cases": _drawtext_filter_cases(),
        "check_safe_cases": _check_safe_cases(),
        "check_filter_safe_cases": _check_filter_safe_cases(),
        "shell_validation_cases": _shell_validation_cases(),
        "str_conversion_cases": _str_conversion_cases(),
        "plugin_env_override_cases": _plugin_env_override_cases(),
        "resolve_plugin_binary_cases": _resolve_plugin_binary_cases(),
        "check_binary_cases": _check_binary_cases(),
        "utf8_decode_cases": _utf8_decode_cases(),
    }


def build_value_corpus() -> dict[str, Any]:
    return {
        "shlex_split_cases": _shlex_split_cases(),
        "shlex_quote_cases": _shlex_quote_cases(),
    }


def _write_or_check(output: Path, data: dict[str, Any]) -> bool:
    encoded = (json.dumps(data, indent=2, sort_keys=False) + "\n").encode("utf-8")
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
    ok = True
    ok &= _write_or_check(VALUE_CORPUS, build_value_corpus())
    ok &= _write_or_check(TOOLS_CORPUS, build_tools_corpus())
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
