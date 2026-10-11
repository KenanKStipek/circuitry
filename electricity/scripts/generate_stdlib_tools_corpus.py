#!/usr/bin/env python3
"""Generate the golden corpus for lane D1 of issue #448 (#460):
`electricity-tools`'s `fs`/`env_vars`/`hash`/`uuid`/`clock` tools, by
actually running Circuitry's own real plugins (`plugins/{fs,env_vars,
hash,uuid,clock}.py`). `clock` is recorded a different way (see its own
section below): `ClockPlugin.execute` always calls the live
`datetime.now()`, so its cases freeze `circuitry.plugins.clock`'s own
`datetime` name to a fixed instant for the length of each case, the
same injection point the Rust side's own `execute_at(params, now)`
gives a test.

`fs`'s own cases run against a real temp directory; every string leaf
in a case's recorded `ok`/`raw`/`err` has that directory's own absolute
path replaced with the stable placeholder `<root>` before it's written
to disk, so the committed corpus never carries a contributor's own
temp-directory path (the same `<root>` the test plan in issue #460
itself names). `stat` mode is deliberately not in this corpus: its own
`mtime` is real wall-clock data that can never match between generation
time and replay time -- issue #460's own test plan checks `stat` only
through a conformance case's `size`/`is_file` extract, not here.

`env_vars` runs under `mock.patch.dict(os.environ, clear=True)` against
a small, fixed synthetic environment, so `list`'s own `environ`-order
recording is deterministic across machines.

Must be run with Python 3.11 (the lane venv locally; `actions/setup-python`
3.11 in CI). Usage: python3 generate_stdlib_tools_corpus.py [--check]
"""

from __future__ import annotations

import contextlib
import json
import struct
import sys
from datetime import datetime as _real_datetime
from datetime import timezone as _real_timezone
from pathlib import Path
from tempfile import TemporaryDirectory
from typing import Any
from unittest import mock

import circuitry.plugins.clock as _clock_module
from circuitry.plugins.clock import ClockPlugin
from circuitry.plugins.env_vars import EnvVarsPlugin
from circuitry.plugins.fs import FsPlugin
from circuitry.plugins.hash import HashPlugin
from circuitry.plugins.uuid import UuidPlugin

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-tools"
    / "tests"
    / "golden"
    / "stdlib_tools_corpus.json"
)

ROOT_PLACEHOLDER = "<root>"


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


def _redact_root(value: Any, root: str) -> Any:
    """Replaces *root* with [`ROOT_PLACEHOLDER`] in every string leaf of
    an already-tagged value (recurses through `{"t": "str", "v": ...}`,
    `"list"`, `"dict"` nodes) or inside a bare error string."""
    if isinstance(value, str):
        return value.replace(root, ROOT_PLACEHOLDER)
    if isinstance(value, dict):
        if value.get("t") == "str":
            return {"t": "str", "v": value["v"].replace(root, ROOT_PLACEHOLDER)}
        if "v" in value:
            return {**value, "v": _redact_root(value["v"], root)}
        return value
    if isinstance(value, list):
        return [_redact_root(item, root) for item in value]
    return value


# ---------------------------------------------------------------------
# fs
# ---------------------------------------------------------------------


def fs_case(name: str, params: dict[str, Any], *, root: str) -> dict:
    plugin = FsPlugin()
    try:
        result = plugin.execute(params=params, timeout_seconds=30)
    except Exception as exc:  # recording whatever the plugin raises
        return {
            "case": name,
            "params": _redact_root(encode(params), root),
            "ok": None,
            "raw": None,
            "err": str(exc).replace(root, ROOT_PLACEHOLDER),
        }
    else:
        return {
            "case": name,
            "params": _redact_root(encode(params), root),
            "ok": _redact_root(encode(result.value), root),
            "raw": _redact_root(encode(result.raw), root),
            "err": None,
        }


def build_fs_cases(root: Path) -> list[dict]:
    cases: list[dict] = []
    r = str(root)

    greeting = root / "greeting.txt"
    cases.append(fs_case("write", {"mode": "write", "path": str(greeting), "content": "hello"}, root=r))
    cases.append(fs_case("read_after_write", {"mode": "read", "path": str(greeting)}, root=r))
    cases.append(
        fs_case(
            "append",
            {"mode": "append", "path": str(greeting), "content": " world"},
            root=r,
        )
    )
    cases.append(fs_case("read_after_append", {"mode": "read", "path": str(greeting)}, root=r))

    crlf = root / "crlf.txt"
    crlf.write_bytes(b"a\r\nb\rc\n")
    cases.append(fs_case("read_translates_crlf_and_cr", {"mode": "read", "path": str(crlf)}, root=r))

    sub = root / "sub"
    for name in ("b.txt", "a.txt", "c.txt"):
        (sub / name).parent.mkdir(parents=True, exist_ok=True)
        (sub / name).write_text("x")
    cases.append(fs_case("list_sorted_by_code_point", {"mode": "list", "path": str(sub)}, root=r))
    cases.append(fs_case("list_of_a_file_is_not_a_directory", {"mode": "list", "path": str(greeting)}, root=r))

    cases.append(fs_case("exists_true", {"mode": "exists", "path": str(greeting)}, root=r))
    cases.append(fs_case("exists_false", {"mode": "exists", "path": str(root / "missing")}, root=r))

    cases.append(fs_case("delete_missing_is_false", {"mode": "delete", "path": str(root / "missing2")}, root=r))
    cases.append(
        fs_case(
            "delete_directory_without_recursive_raises",
            {"mode": "delete", "path": str(sub)},
            root=r,
        )
    )
    cases.append(
        fs_case(
            "delete_directory_with_recursive_succeeds",
            {"mode": "delete", "path": str(sub), "recursive": True},
            root=r,
        )
    )

    cases.append(fs_case("read_of_missing_file", {"mode": "read", "path": str(root / "nope.txt")}, root=r))
    cases.append(fs_case("unknown_mode", {"mode": "bogus", "path": str(root)}, root=r))
    cases.append(fs_case("unknown_mode_explicit_none", {"mode": None, "path": str(root)}, root=r))
    cases.append(fs_case("path_must_be_non_empty_string", {"mode": "read", "path": ""}, root=r))
    cases.append(fs_case("path_rejects_null_byte", {"mode": "read", "path": "a\x00b"}, root=r))
    cases.append(fs_case("write_requires_str_content", {"mode": "write", "path": str(root / "x"), "content": 1}, root=r))

    # Encoding aliases and an unknown codec.
    utf8_path = root / "codec.txt"
    cases.append(
        fs_case(
            "write_with_utf8_alias",
            {"mode": "write", "path": str(utf8_path), "content": "café", "encoding": "UTF8"},
            root=r,
        )
    )
    cases.append(
        fs_case(
            "read_with_latin1_alias",
            {"mode": "read", "path": str(utf8_path), "encoding": "latin-1"},
            root=r,
        )
    )
    cases.append(
        fs_case(
            "unknown_codec",
            {"mode": "write", "path": str(root / "y"), "content": "x", "encoding": "bogus-codec"},
            root=r,
        )
    )
    cases.append(
        fs_case(
            "ascii_encode_error",
            {"mode": "write", "path": str(root / "z"), "content": "café", "encoding": "ascii"},
            root=r,
        )
    )

    return cases


def build_encode_run_hash_cases() -> list[dict]:
    """A *run* of consecutive unencodable characters gets one error for
    the whole run, not one per character (issue #460's own fix-pass
    finding 3 -- "日本".encode("ascii")'s own text)."""
    return [
        hash_case(
            "ascii_encode_error_run_of_two",
            {"algorithm": "sha256", "input": "日本", "encoding": "ascii"},
        ),
        hash_case(
            "latin1_encode_error_run_of_two",
            {"algorithm": "sha256", "input": "a日本b", "encoding": "latin-1"},
        ),
    ]


# ---------------------------------------------------------------------
# env_vars
# ---------------------------------------------------------------------


_FAKE_ENVIRON = {
    "PATH": "/usr/bin:/bin",
    "HOME": "/home/fake",
    "MY_API_KEY_SUFFIX": "visible-name-but-masked-value",
    "MY_APIKEYLESS_VAR": "also-masked",
    "PLAIN_VAR": "plain-value",
    "APP_PREFIXED_ONE": "one",
    "APP_PREFIXED_TWO": "two",
}


def env_vars_case(name: str, params: dict[str, Any]) -> dict:
    plugin = EnvVarsPlugin()
    with mock.patch.dict("os.environ", _FAKE_ENVIRON, clear=True):
        try:
            result = plugin.execute(params=params, timeout_seconds=30)
        except Exception as exc:
            return {"case": name, "params": encode(params), "ok": None, "raw": None, "err": str(exc)}
        else:
            return {
                "case": name,
                "params": encode(params),
                "ok": encode(result.value),
                "raw": encode(result.raw),
                "err": None,
            }


def build_env_vars_cases() -> list[dict]:
    cases: list[dict] = []
    cases.append(env_vars_case("get_present", {"mode": "get", "name": "PLAIN_VAR"}))
    cases.append(
        env_vars_case(
            "get_unset_with_default",
            {"mode": "get", "name": "DOES_NOT_EXIST", "default": "fallback"},
        )
    )
    cases.append(env_vars_case("get_requires_name", {"mode": "get"}))
    cases.append(env_vars_case("list_all_masks_secrets", {"mode": "list"}))
    cases.append(env_vars_case("list_includes_secrets", {"mode": "list", "include_secrets": True}))
    cases.append(env_vars_case("list_with_prefix", {"mode": "list", "prefix": "APP_PREFIXED_"}))
    cases.append(env_vars_case("list_prefix_matches_nothing", {"mode": "list", "prefix": "NO_SUCH_PREFIX_"}))
    cases.append(env_vars_case("unknown_mode", {"mode": "bogus"}))
    return cases


# ---------------------------------------------------------------------
# hash
# ---------------------------------------------------------------------


def hash_case(name: str, params: dict[str, Any], *, root: str | None = None) -> dict:
    plugin = HashPlugin()
    encoded_params = _redact_root(encode(params), root) if root else encode(params)
    try:
        result = plugin.execute(params=params, timeout_seconds=30)
    except Exception as exc:
        err = str(exc)
        if root:
            err = err.replace(root, ROOT_PLACEHOLDER)
        return {"case": name, "params": encoded_params, "ok": None, "raw": None, "err": err}
    else:
        return {
            "case": name,
            "params": encoded_params,
            "ok": encode(result.value),
            "raw": encode(result.raw),
            "err": None,
        }


def build_hash_cases(root: Path) -> list[dict]:
    cases: list[dict] = []
    for algorithm in (
        "md5", "sha1", "sha224", "sha256", "sha384", "sha512",
        "sha3_256", "sha3_512", "blake2b", "blake2s",
    ):
        cases.append(
            hash_case(f"hex_{algorithm}", {"algorithm": algorithm, "input": "hello"})
        )
        cases.append(
            hash_case(
                f"base64_{algorithm}",
                {"algorithm": algorithm, "input": "hello", "output_format": "base64"},
            )
        )
    cases.append(hash_case("default_algorithm_is_sha256", {"input": "hello"}))
    cases.append(hash_case("unsupported_algorithm", {"algorithm": "bogus"}))
    cases.append(hash_case("bad_output_format", {"algorithm": "sha256", "output_format": "bogus"}))
    cases.append(hash_case("in_memory_requires_str", {"algorithm": "sha256", "input": 1}))
    cases.append(hash_case("from_path_requires_path", {"from_path": True}))
    cases.append(hash_case("encoding_latin1", {"algorithm": "sha256", "input": "café", "encoding": "latin-1"}))

    src = root / "hash_input.bin"
    src.write_bytes(b"hello from a file")
    cases.append(
        hash_case(
            "from_path_hashes_file",
            {"algorithm": "sha256", "from_path": True, "input": str(src)},
            root=str(root),
        )
    )
    cases.append(
        hash_case(
            "from_path_missing_file",
            {"algorithm": "sha256", "from_path": True, "input": str(root / "nope.bin")},
            root=str(root),
        )
    )
    cases.extend(build_encode_run_hash_cases())
    return cases


# ---------------------------------------------------------------------
# uuid
# ---------------------------------------------------------------------


def uuid_case(name: str, params: dict[str, Any]) -> dict:
    plugin = UuidPlugin()
    try:
        result = plugin.execute(params=params, timeout_seconds=30)
    except Exception as exc:
        return {"case": name, "params": encode(params), "ok": None, "raw": None, "err": str(exc)}
    else:
        return {
            "case": name,
            "params": encode(params),
            "ok": encode(result.value),
            "raw": encode(result.raw),
            "err": None,
        }


def build_uuid_cases() -> list[dict]:
    cases: list[dict] = [
        uuid_case(f"v5_namespace_{ns}", {"version": 5, "namespace": ns, "name": "example.com"})
        for ns in ("dns", "url", "oid", "x500", "DNS")
    ]
    custom_ns = "6ba7b810-9dad-11d1-80b4-00c04fd430c8"
    for spelling_name, spelling in (
        ("plain", custom_ns),
        ("urn", f"urn:uuid:{custom_ns}"),
        ("braces", f"{{{custom_ns}}}"),
        ("no_dashes", custom_ns.replace("-", "")),
    ):
        cases.append(
            uuid_case(
                f"v5_custom_namespace_{spelling_name}",
                {"version": 5, "namespace": spelling, "name": "x"},
            )
        )
    cases.append(
        uuid_case(
            "v5_count_3_hex_true",
            {"version": 5, "namespace": "dns", "name": "example.com", "count": 3, "hex": True},
        )
    )
    cases.append(uuid_case("v5_requires_name", {"version": 5}))
    cases.append(uuid_case("v5_requires_namespace", {"version": 5, "name": "x"}))
    cases.append(
        uuid_case("v5_bad_namespace", {"version": 5, "name": "x", "namespace": "not-a-uuid"})
    )
    cases.append(uuid_case("unsupported_version", {"version": 2}))
    cases.append(uuid_case("count_must_be_at_least_1", {"count": -1}))
    # `version`/`count` coercion for a value that still produces a v4/v1
    # (random) uuid is deliberately not a case here -- there is no
    # single real output to pin in a byte-for-byte corpus for those;
    # `electricity_value::pycompat::py_int`'s own corpus already covers
    # the shared coercion function these two params go through, and
    # `uuid.rs`'s own unit tests check the *coerced* value lands in
    # `raw.version`/`raw.count` without needing the random `value` to
    # match anything.
    return cases


# ---------------------------------------------------------------------
# clock
# ---------------------------------------------------------------------
#
# `ClockPlugin.execute` always calls the live `datetime.now(tz)`, so
# unlike the other four plugins this can't just call the real thing
# once and pin the result -- the same real call, run again at replay
# time, would give a different answer. Instead, `circuitry.plugins.
# clock`'s own module-level `datetime` name (imported with `from
# datetime import datetime`) is swapped for a subclass whose `now(tz)`
# always returns the *same* fixed instant, converted into whatever `tz`
# the real code asks for -- exactly what the Rust side's own
# `execute_at(params, now)` injection point does (#460's own lane D1
# ruling: "a module-level function..., never a global mutable"), so
# both sides inject the identical instant rather than one trying to
# replay the other's wall-clock reading.

_FIXED_INSTANT = _real_datetime(2024, 7, 4, 12, 30, 45, 0, tzinfo=_real_timezone.utc)
_FIXED_INSTANT_WITH_MICROS = _real_datetime(2024, 7, 4, 12, 30, 45, 654321, tzinfo=_real_timezone.utc)
_WINTER_INSTANT = _real_datetime(2024, 1, 15, 8, 0, 0, 0, tzinfo=_real_timezone.utc)


@contextlib.contextmanager
def _frozen_clock(instant: Any):
    class _FrozenDatetime(_real_datetime):
        @classmethod
        def now(cls, tz: Any = None) -> Any:
            return instant.astimezone(tz) if tz is not None else instant

    original = _clock_module.datetime
    _clock_module.datetime = _FrozenDatetime
    try:
        yield
    finally:
        _clock_module.datetime = original


def clock_case(name: str, params: dict[str, Any], *, instant: Any = _FIXED_INSTANT) -> dict:
    plugin = ClockPlugin()
    # The instant itself travels with the case (as whole seconds since
    # the epoch plus a microseconds remainder) so the Rust replay can
    # reconstruct the exact same `DateTime<Utc>` to pass to its own
    # `execute_at` -- there's no live clock on either side for this
    # plugin to inject through a shared recording the way the other
    # four tools' corpus entries do.
    epoch_seconds = int(instant.timestamp())
    epoch_micros = instant.microsecond
    with _frozen_clock(instant):
        try:
            result = plugin.execute(params=params, timeout_seconds=30)
        except Exception as exc:
            return {
                "case": name,
                "params": encode(params),
                "epoch_seconds": epoch_seconds,
                "epoch_micros": epoch_micros,
                "ok": None,
                "raw": None,
                "err": str(exc),
            }
        else:
            return {
                "case": name,
                "params": encode(params),
                "epoch_seconds": epoch_seconds,
                "epoch_micros": epoch_micros,
                "ok": encode(result.value),
                "raw": encode(result.raw),
                "err": None,
            }


def build_clock_cases() -> list[dict]:
    return [
        clock_case("default_format_utc", {}),
        clock_case("epoch_mode", {"epoch": True}),
        clock_case("percent_percent", {"format": "%%"}),
        clock_case("percent_percent_then_z_literal", {"format": "%%z"}),
        clock_case(
            "microseconds_in_f_and_iso",
            {"format": "%f"},
            instant=_FIXED_INSTANT_WITH_MICROS,
        ),
        clock_case(
            "named_timezone_summer_dst",
            {"timezone": "America/New_York", "format": "%Y-%m-%d %H:%M:%S %Z %z"},
        ),
        clock_case(
            "named_timezone_winter_no_dst",
            {"timezone": "America/New_York", "format": "%Y-%m-%d %H:%M:%S %Z %z"},
            instant=_WINTER_INSTANT,
        ),
        clock_case("unknown_timezone", {"timezone": "Not/AZone"}),
        clock_case("unknown_timezone_non_string", {"timezone": 5}),
        clock_case("explicit_null_format", {"format": None}),
    ]


# ---------------------------------------------------------------------
# write / check
# ---------------------------------------------------------------------


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
    with TemporaryDirectory(prefix="electricity-stdlib-tools-corpus-") as tmp:
        root = Path(tmp).resolve()
        corpus = {
            "fs_cases": build_fs_cases(root),
            "env_vars_cases": build_env_vars_cases(),
            "hash_cases": build_hash_cases(root),
            "uuid_cases": build_uuid_cases(),
            "clock_cases": build_clock_cases(),
        }
    text = render(corpus)
    return 0 if write_or_check(OUTPUT, text, check=check) else 1


if __name__ == "__main__":
    raise SystemExit(main())
