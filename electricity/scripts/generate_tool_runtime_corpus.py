#!/usr/bin/env python3
"""Generate the golden corpora for lane C of issue #431 (electricity-tools'
`json` tool and electricity-redaction's `redact`/`cap_raw`) from Circuitry's
own real functions (`plugins/json.py::JsonPlugin`, `cli/redaction.py::redact`).

Every expected value here comes from actually running the Python code, not
from a hand-typed literal -- this is the corpus issue #431's review asked
for: it would have caught, among others, the `{from:}`-resolves-to-null
regression and the `json` extract-path `i64`-overflow panic this PR's other
commits fix, had it existed beforehand.

`cap_raw`'s own corpus is the one exception: electricity-redaction's
`cap_raw(bytes) -> Option<RawCapMarker>` takes already-redacted, already-
JSON-encoded bytes (the caller's job, still electricity-vm's stub
`exec::tool::execute_tool`, not this crate's), so there's no single real
Python function with that exact signature to call. Its cases instead run
the literal expression `core/tool.py::_capped_raw` uses for the truncated
preview -- `encoded[:_RAW_META_MAX_BYTES].decode("utf-8", errors="ignore")`
-- against hand-built byte strings that straddle the cut with a multi-byte
codepoint, which is the real behaviour under test (finding 5).

Must be run with Python 3.11 (the lane venv locally; `actions/setup-python`
3.11 in CI). Usage: python3 generate_tool_runtime_corpus.py [--check]
"""

from __future__ import annotations

import json
import struct
import sys
from pathlib import Path

from circuitry.cli.redaction import redact
from circuitry.plugins.json import JsonPlugin

TOOLS_OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-tools"
    / "tests"
    / "golden"
    / "json_tool_corpus.json"
)
REDACTION_OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-redaction"
    / "tests"
    / "golden"
    / "corpus.json"
)

_RAW_META_MAX_BYTES = 64 * 1024


# ---------------------------------------------------------------------
# Tagged value encoding (matches generate_template_corpus.py's/
# generate_value_corpus.py's, so a Rust decoder can be copied verbatim)
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


# ---------------------------------------------------------------------
# electricity-tools: the `json` tool (`JsonPlugin.execute`)
# ---------------------------------------------------------------------


def json_tool_case(params: dict, *, timeout_seconds: int = 30) -> dict:
    plugin = JsonPlugin()
    try:
        result = plugin.execute(params=params, timeout_seconds=timeout_seconds)
    except ValueError as exc:
        # A `json.JSONDecodeError` cause means the tail of `str(exc)` is
        # third-party text (DESIGN.md §1/§12): electricity only has to
        # fail at the same character offset, never match it byte for
        # byte. The corpus records that offset separately from the
        # (still-exact) Circuitry-authored prefix in front of it, rather
        # than the third-party message text itself.
        cause = exc.__cause__
        if isinstance(cause, json.JSONDecodeError):
            message = str(exc)
            third_party = str(cause)
            assert message.endswith(third_party), (message, third_party)
            prefix = message[: -len(third_party)]
            return {
                "params": encode(params),
                "ok": None,
                "raw": None,
                "err": None,
                "err_prefix": prefix,
                "err_char": cause.pos,
            }
        return {
            "params": encode(params),
            "ok": None,
            "raw": None,
            "err": str(exc),
            "err_prefix": None,
            "err_char": None,
        }
    else:
        return {
            "params": encode(params),
            "ok": encode(result.value),
            "raw": encode(result.raw),
            "err": None,
            "err_prefix": None,
            "err_char": None,
        }


def build_json_tool_cases() -> list[dict]:
    cases: list[dict] = []

    # --- parse ---
    cases.append(json_tool_case({"mode": "parse", "input": '{"a": 1}'}))
    cases.append(json_tool_case({"mode": "parse", "input": 1}))  # not a string
    cases.append(json_tool_case({"mode": "parse", "input": "not json"}))
    cases.append(json_tool_case({"input": "null"}))  # mode absent -> defaults to parse
    cases.append(json_tool_case({"mode": None, "input": "null"}))  # explicit null != absent
    cases.append(json_tool_case({"mode": "PARSE", "input": "1"}))  # case-folded mode

    # --- stringify ---
    cases.append(json_tool_case({"mode": "stringify", "input": {"a": 1}}))
    cases.append(json_tool_case({"mode": "stringify", "input": {"a": 1}, "indent": 2}))
    cases.append(json_tool_case({"mode": "stringify", "input": None}))
    cases.append(json_tool_case({"mode": "stringify", "input": [1, "a", None, True]}))
    # Non-string keys collapse under Python's own dict/JSON-key rules
    # (finding 6's "non-string keys" probe): `True` and `1` collide as
    # dict keys (`True == 1`), so only the last-written entry survives.
    cases.append(
        json_tool_case({"mode": "stringify", "input": {True: "yes-value", 1: "one-value"}})
    )
    cases.append(json_tool_case({"mode": "stringify", "input": {None: "none-value"}}))
    cases.append(json_tool_case({"mode": "stringify", "input": {1.5: "float-key"}}))

    # --- extract ---
    cases.append(
        json_tool_case(
            {
                "mode": "extract",
                "input": {"foo": {"bar": [1, 2]}},
                "path": "foo.bar[1]",
            }
        )
    )
    cases.append(
        json_tool_case(
            {"mode": "extract", "input": {}, "path": "missing", "default": "fallback"}
        )
    )
    cases.append(json_tool_case({"mode": "extract", "input": {}}))  # no path
    cases.append(
        json_tool_case({"mode": "extract", "input": '{"a": 1}', "path": "a"})
    )
    cases.append(
        json_tool_case({"mode": "extract", "input": "not json", "path": "a"})
    )
    # Finding 2's own probe: an extract index too large to fit an i64
    # never raises in Python -- `int()` parses it fine, then indexing a
    # real list with it raises `IndexError`, which `_walk_path` turns
    # into an ordinary miss.
    cases.append(
        json_tool_case(
            {
                "mode": "extract",
                "input": {"a": [1]},
                "path": "a[99999999999999999999]",
                "default": "d",
            }
        )
    )
    cases.append(
        json_tool_case(
            {
                "mode": "extract",
                "input": {"a": [1]},
                "path": "a[-99999999999999999999]",
                "default": "d",
            }
        )
    )
    cases.append(
        json_tool_case({"mode": "extract", "input": [1, 2, 3], "path": "[-1]"})
    )

    # --- unknown mode ---
    cases.append(json_tool_case({"mode": "bogus"}))

    return cases


# ---------------------------------------------------------------------
# electricity-redaction: `redact`
# ---------------------------------------------------------------------


def redact_case(value: object) -> dict:
    return {"input": encode(value), "output": encode(redact(value))}


def build_redaction_cases() -> list[dict]:
    cases: list[dict] = []

    cases.append(redact_case({"api_key": "anything-at-all"}))
    cases.append(redact_case({"MY-API-KEY": "x"}))
    cases.append(redact_case({"name": "not a secret"}))
    cases.append(
        redact_case(
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0."
            "dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U"
        )
    )
    cases.append(redact_case("sk-" + "a" * 20))
    cases.append(redact_case("Bearer " + "a" * 16))
    cases.append(redact_case("https://user:pass@example.com:8080/path?q=1#frag"))
    cases.append(redact_case("https://example.com/path"))
    cases.append(redact_case("www.example.com"))
    cases.append(
        redact_case({"outer": [{"password": "x"}]}),
    )
    cases.append(redact_case({True: "sk-" + "a" * 20}))  # non-str key never sensitive
    cases.append(redact_case(1))
    cases.append(redact_case(True))
    cases.append(redact_case(None))

    # --- finding 3: Python's `$` (no re.MULTILINE) also matches just
    # before a single trailing newline ---
    cases.append(redact_case("sk-" + "a" * 20 + "\n"))
    cases.append(redact_case("sk-" + "a" * 20 + "\n\n"))  # two newlines: no longer matches
    cases.append(redact_case("Bearer " + "a" * 16 + "\n"))
    cases.append(
        redact_case(
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0."
            "dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U\n"
        )
    )
    cases.append(redact_case({"api_key\n": "x"}))

    # --- finding 4: redact_url vs. urlsplit/urlunsplit ---
    cases.append(redact_case(" https://u:p@h"))  # leading space, lstripped
    cases.append(redact_case("ht\ttps://u:p@h"))  # embedded tab, dropped
    cases.append(redact_case("HTTPS://u:p@h"))  # scheme lowercased
    cases.append(redact_case("https://u:p@h/p?"))  # empty query dropped
    cases.append(redact_case("https://u:p@h/p?#f"))  # empty query, present fragment
    cases.append(redact_case("https://u:p@h[invalid"))  # unbalanced bracket: unchanged

    return cases


def build_cap_raw_probe(
    name: str, encoded: bytes
) -> dict:
    # The exact expression `core/tool.py::_capped_raw` uses for its own
    # `_preview` field.
    preview = encoded[:_RAW_META_MAX_BYTES].decode("utf-8", errors="ignore")
    return {
        "name": name,
        "encoded_hex": encoded.hex(),
        "original_bytes": len(encoded),
        "capped": len(encoded) > _RAW_META_MAX_BYTES,
        "preview": preview,
    }


def build_cap_raw_cases() -> list[dict]:
    cases: list[dict] = []
    cases.append(build_cap_raw_probe("at_the_limit", b"a" * _RAW_META_MAX_BYTES))
    cases.append(build_cap_raw_probe("one_over_the_limit", b"a" * (_RAW_META_MAX_BYTES + 1)))

    # A 2/3/4-byte UTF-8 codepoint straddling the cut at every possible
    # split point -- finding 5's own probe (`"\u00e9" * 40000`, a 2-byte
    # codepoint) generalized to 3- and 4-byte codepoints too.
    for codepoint, label in ((b"\xc3\xa9", "2byte"), (b"\xe4\xb8\xad", "3byte"), (b"\xf0\x9f\x8e\x89", "4byte")):
        for split in range(1, len(codepoint)):
            prefix_len = _RAW_META_MAX_BYTES - split
            encoded = b"a" * prefix_len + codepoint + b"a" * 8
            cases.append(build_cap_raw_probe(f"{label}_split_at_{split}", encoded))

    return cases


def render(corpus: dict) -> str:
    return json.dumps(corpus, indent=2, ensure_ascii=False, sort_keys=True) + "\n"


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

    tools_text = render({"json_tool_cases": build_json_tool_cases()})
    redaction_text = render(
        {
            "redact_cases": build_redaction_cases(),
            "cap_raw_cases": build_cap_raw_cases(),
        }
    )

    ok = write_or_check(TOOLS_OUTPUT, tools_text, check=check)
    ok = write_or_check(REDACTION_OUTPUT, redaction_text, check=check) and ok
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
