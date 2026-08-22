"""Tests for the stdlib-only tool plugin batch.

Coverage per plugin: factory build, ToolResult contract conformance,
happy paths, edge cases, error paths. No external services or binaries
are touched.
"""

from __future__ import annotations

import gzip as _gzip
import io
import json as _json
import socket
import tarfile
import zipfile
from pathlib import Path
from typing import Any

import pytest

from circuitry.plugins import build_plugin
from circuitry.plugins.base import validate_tool_result
from circuitry.plugins.base64 import Base64Plugin
from circuitry.plugins.clock import ClockPlugin
from circuitry.plugins.csv import CsvPlugin
from circuitry.plugins.email_smtp import EmailSmtpPlugin
from circuitry.plugins.env_vars import EnvVarsPlugin
from circuitry.plugins.fs import FsPlugin
from circuitry.plugins.gzip import GzipPlugin
from circuitry.plugins.hash import HashPlugin
from circuitry.plugins.hex import HexPlugin
from circuitry.plugins.json import JsonPlugin
from circuitry.plugins.math import MathPlugin
from circuitry.plugins.port_check import PortCheckPlugin
from circuitry.plugins.regex import RegexPlugin
from circuitry.plugins.tar import TarPlugin
from circuitry.plugins.uuid import UuidPlugin
from circuitry.plugins.zip import ZipPlugin

STDLIB_PLUGIN_NAMES = [
    "clock", "math", "regex", "json", "fs", "csv", "email_smtp",
    "tar", "zip", "gzip", "port_check", "env_vars", "hash",
    "base64", "hex", "uuid",
]


@pytest.mark.parametrize("name", STDLIB_PLUGIN_NAMES)
def test_factory_builds_each_plugin(name: str) -> None:
    p = build_plugin(plugin_name=name, runtime={})
    assert p.name == name
    assert p.check().ok is True


# ---------- clock ----------


def test_clock_default_returns_iso_string() -> None:
    r = ClockPlugin().execute(params={})
    assert isinstance(r.value, str)
    assert "T" in r.value
    assert validate_tool_result(r, plugin_name="clock") == []


def test_clock_epoch_returns_int() -> None:
    r = ClockPlugin().execute(params={"epoch": True})
    assert isinstance(r.value, int)
    assert r.value > 1_700_000_000  # past Nov 2023


def test_clock_unknown_timezone_raises() -> None:
    with pytest.raises(ValueError, match="unknown timezone"):
        ClockPlugin().execute(params={"timezone": "Mars/Olympus"})


def test_clock_custom_format() -> None:
    r = ClockPlugin().execute(params={"format": "%Y", "timezone": "UTC"})
    assert isinstance(r.value, str) and len(r.value) == 4 and r.value.isdigit()


# ---------- math ----------


@pytest.mark.parametrize(
    "expr,expected",
    [
        ("1 + 2", 3),
        ("2 ** 10", 1024),
        ("10 // 3", 3),
        ("10 % 3", 1),
        ("(1 + 2) * (4 - 1)", 9),
        ("-5 + 3", -2),
        ("0.5 * 4", 2),
    ],
)
def test_math_evaluates_arithmetic(expr: str, expected: float) -> None:
    r = MathPlugin().execute(params={"expression": expr})
    assert r.value == expected


def test_math_rejects_function_calls() -> None:
    with pytest.raises(ValueError, match="disallowed"):
        MathPlugin().execute(params={"expression": "__import__('os')"})


def test_math_rejects_names() -> None:
    with pytest.raises(ValueError, match="disallowed"):
        MathPlugin().execute(params={"expression": "x + 1"})


def test_math_rejects_invalid_syntax() -> None:
    with pytest.raises(ValueError, match="invalid expression"):
        MathPlugin().execute(params={"expression": "1 +"})


# ---------- regex ----------


def test_regex_findall() -> None:
    r = RegexPlugin().execute(
        params={"pattern": r"\d+", "input": "a1 b22 c333", "mode": "findall"}
    )
    assert r.value == ["1", "22", "333"]


def test_regex_search_with_groups_returns_tuple() -> None:
    r = RegexPlugin().execute(
        params={
            "pattern": r"(\w+)=(\d+)",
            "input": "x=42",
            "mode": "search",
        }
    )
    assert r.value == ["x", "42"]


def test_regex_sub_with_count() -> None:
    r = RegexPlugin().execute(
        params={
            "pattern": r"a",
            "input": "aaaa",
            "mode": "sub",
            "replacement": "Z",
            "count": 2,
        }
    )
    assert r.value == "ZZaa"


def test_regex_unknown_flag_raises() -> None:
    with pytest.raises(ValueError, match="unknown flag"):
        RegexPlugin().execute(
            params={"pattern": "a", "input": "a", "flags": ["NOPE"]}
        )


def test_regex_invalid_pattern_raises() -> None:
    with pytest.raises(ValueError, match="invalid pattern"):
        RegexPlugin().execute(params={"pattern": "(abc", "input": "x"})


# ---------- json ----------


def test_json_parse_and_stringify_roundtrip() -> None:
    plugin = JsonPlugin()
    parsed = plugin.execute(
        params={"mode": "parse", "input": '{"a": 1, "b": [2, 3]}'}
    )
    assert parsed.value == {"a": 1, "b": [2, 3]}
    s = plugin.execute(params={"mode": "stringify", "input": parsed.value})
    assert _json.loads(s.value) == parsed.value


def test_json_extract_dotted_path() -> None:
    r = JsonPlugin().execute(
        params={
            "mode": "extract",
            "input": {"a": {"b": [{"c": 42}]}},
            "path": "a.b[0].c",
        }
    )
    assert r.value == 42


def test_json_extract_miss_returns_default() -> None:
    r = JsonPlugin().execute(
        params={
            "mode": "extract",
            "input": {"a": 1},
            "path": "a.b.c",
            "default": "NA",
        }
    )
    assert r.value == "NA"


def test_json_parse_invalid_raises() -> None:
    with pytest.raises(ValueError, match="parse failed"):
        JsonPlugin().execute(params={"mode": "parse", "input": "{not json}"})


# ---------- fs ----------


def test_fs_write_then_read_roundtrip(tmp_path: Path) -> None:
    plugin = FsPlugin()
    target = tmp_path / "sub" / "f.txt"
    w = plugin.execute(
        params={"mode": "write", "path": str(target), "content": "hello"}
    )
    assert w.value == str(target)
    r = plugin.execute(params={"mode": "read", "path": str(target)})
    assert r.value == "hello"


def test_fs_append_extends_existing(tmp_path: Path) -> None:
    p = FsPlugin()
    target = tmp_path / "a.txt"
    p.execute(params={"mode": "write", "path": str(target), "content": "ab"})
    p.execute(params={"mode": "append", "path": str(target), "content": "cd"})
    r = p.execute(params={"mode": "read", "path": str(target)})
    assert r.value == "abcd"


def test_fs_list_sorts_entries(tmp_path: Path) -> None:
    (tmp_path / "b.txt").write_text("")
    (tmp_path / "a.txt").write_text("")
    r = FsPlugin().execute(params={"mode": "list", "path": str(tmp_path)})
    assert r.value == ["a.txt", "b.txt"]


def test_fs_exists_and_stat(tmp_path: Path) -> None:
    p = FsPlugin()
    target = tmp_path / "x.txt"
    target.write_text("x")
    assert p.execute(params={"mode": "exists", "path": str(target)}).value is True
    stat = p.execute(params={"mode": "stat", "path": str(target)}).value
    assert stat["is_file"] is True and stat["size"] == 1


def test_fs_delete_directory_requires_recursive(tmp_path: Path) -> None:
    sub = tmp_path / "sub"
    sub.mkdir()
    with pytest.raises(IsADirectoryError):
        FsPlugin().execute(params={"mode": "delete", "path": str(sub)})


def test_fs_delete_recursive(tmp_path: Path) -> None:
    sub = tmp_path / "sub"
    sub.mkdir()
    (sub / "x.txt").write_text("hi")
    r = FsPlugin().execute(
        params={"mode": "delete", "path": str(sub), "recursive": True}
    )
    assert r.value is True
    assert not sub.exists()


def test_fs_rejects_null_byte() -> None:
    with pytest.raises(ValueError, match="null byte"):
        FsPlugin().execute(params={"mode": "read", "path": "a\x00b"})


def test_fs_delete_missing_is_idempotent(tmp_path: Path) -> None:
    r = FsPlugin().execute(
        params={"mode": "delete", "path": str(tmp_path / "ghost")}
    )
    assert r.value is False


# ---------- csv ----------


def test_csv_parse_with_header_returns_dicts() -> None:
    text = "name,age\nA,30\nB,40\n"
    r = CsvPlugin().execute(params={"mode": "parse", "input": text})
    assert r.value == [{"name": "A", "age": "30"}, {"name": "B", "age": "40"}]


def test_csv_parse_without_header_returns_lists() -> None:
    text = "x,y\n1,2\n"
    r = CsvPlugin().execute(
        params={"mode": "parse", "input": text, "has_header": False}
    )
    assert r.value == [["x", "y"], ["1", "2"]]


def test_csv_write_dicts_uses_keys_as_header() -> None:
    rows = [{"a": 1, "b": 2}, {"a": 3, "b": 4}]
    r = CsvPlugin().execute(params={"mode": "write", "input": rows})
    assert "a,b\r\n1,2\r\n3,4\r\n" in r.value or "a,b\n1,2\n3,4\n" in r.value


def test_csv_parse_from_path(tmp_path: Path) -> None:
    f = tmp_path / "a.csv"
    f.write_text("k,v\n1,2\n")
    r = CsvPlugin().execute(
        params={"mode": "parse", "input": str(f), "from_path": True}
    )
    assert r.value == [{"k": "1", "v": "2"}]


# ---------- email_smtp ----------


def test_email_smtp_sends_via_mocked_smtp(monkeypatch: pytest.MonkeyPatch) -> None:
    """Mock smtplib.SMTP so the test never opens a socket."""
    sent: dict[str, Any] = {}

    class FakeSMTP:
        def __init__(self, host: str, port: int, timeout: int) -> None:
            sent["host"] = host
            sent["port"] = port

        def __enter__(self) -> FakeSMTP:
            return self

        def __exit__(self, *args: Any) -> None:
            return None

        def starttls(self, **kwargs: Any) -> None:
            sent["tls"] = True

        def login(self, u: str, p: str) -> None:
            sent["login"] = (u, p)

        def sendmail(self, frm: str, rcpts: list[str], body: str) -> dict:
            sent["from"] = frm
            sent["rcpts"] = list(rcpts)
            sent["body"] = body
            return {}  # zero refused

    monkeypatch.setattr("circuitry.plugins.email_smtp.smtplib.SMTP", FakeSMTP)

    r = EmailSmtpPlugin().execute(
        params={
            "host": "smtp.example.test",
            "port": 587,
            "from_addr": "sender@x.test",
            "to": ["a@x.test", "b@x.test"],
            "cc": "c@x.test",
            "subject": "hi",
            "body": "<p>x</p>",
            "content_type": "text/html",
            "username": "u",
            "password": "p",
        }
    )
    assert r.value == 3
    assert sent["host"] == "smtp.example.test"
    assert sent["rcpts"] == ["a@x.test", "b@x.test", "c@x.test"]
    assert sent.get("tls") is True
    assert sent.get("login") == ("u", "p")


def test_email_smtp_requires_to() -> None:
    with pytest.raises(ValueError, match="params\\['to'\\]"):
        EmailSmtpPlugin().execute(
            params={
                "host": "h",
                "from_addr": "f@x",
                "subject": "s",
                "body": "b",
            }
        )


# ---------- tar ----------


def test_tar_create_and_extract_roundtrip(tmp_path: Path) -> None:
    src = tmp_path / "src.txt"
    src.write_text("hello")
    archive = tmp_path / "out.tar.gz"
    plugin = TarPlugin()

    plugin.execute(
        params={
            "mode": "create",
            "archive": str(archive),
            "sources": [str(src)],
            "compression": "gz",
        }
    )
    listing = plugin.execute(
        params={"mode": "list", "archive": str(archive)}
    )
    assert "src.txt" in listing.value

    dest = tmp_path / "out"
    plugin.execute(
        params={
            "mode": "extract",
            "archive": str(archive),
            "destination": str(dest),
        }
    )
    assert (dest / "src.txt").read_text() == "hello"


def test_tar_extract_refuses_path_traversal(tmp_path: Path) -> None:
    archive = tmp_path / "evil.tar"
    with tarfile.open(archive, "w") as tf:
        info = tarfile.TarInfo(name="../escape.txt")
        info.size = 0
        tf.addfile(info, io.BytesIO(b""))
    dest = tmp_path / "extract"
    with pytest.raises(ValueError, match="unsafe member path"):
        TarPlugin().execute(
            params={
                "mode": "extract",
                "archive": str(archive),
                "destination": str(dest),
            }
        )


# ---------- zip ----------


def test_zip_create_and_extract_roundtrip(tmp_path: Path) -> None:
    src = tmp_path / "src.txt"
    src.write_text("zip-me")
    archive = tmp_path / "out.zip"
    plugin = ZipPlugin()

    plugin.execute(
        params={"mode": "create", "archive": str(archive), "sources": [str(src)]}
    )
    listing = plugin.execute(
        params={"mode": "list", "archive": str(archive)}
    )
    assert "src.txt" in listing.value

    dest = tmp_path / "out"
    plugin.execute(
        params={
            "mode": "extract",
            "archive": str(archive),
            "destination": str(dest),
        }
    )
    assert (dest / "src.txt").read_text() == "zip-me"


def test_zip_extract_refuses_traversal(tmp_path: Path) -> None:
    archive = tmp_path / "evil.zip"
    with zipfile.ZipFile(archive, "w") as zf:
        zf.writestr("../escape.txt", "x")
    with pytest.raises(ValueError, match="unsafe member path"):
        ZipPlugin().execute(
            params={
                "mode": "extract",
                "archive": str(archive),
                "destination": str(tmp_path / "x"),
            }
        )


# ---------- gzip ----------


def test_gzip_in_memory_roundtrip() -> None:
    plugin = GzipPlugin()
    c = plugin.execute(params={"mode": "compress", "input": "hello world"})
    d = plugin.execute(params={"mode": "decompress", "input": c.value})
    assert d.value == "hello world"


def test_gzip_path_roundtrip(tmp_path: Path) -> None:
    src = tmp_path / "s.txt"
    src.write_text("payload")
    gz = tmp_path / "s.txt.gz"
    out = tmp_path / "out.txt"

    plugin = GzipPlugin()
    plugin.execute(
        params={
            "mode": "compress",
            "input": str(src),
            "output": str(gz),
            "from_path": True,
        }
    )
    # Verify it's actual gzip.
    with _gzip.open(gz, "rb") as fh:
        assert fh.read() == b"payload"
    plugin.execute(
        params={
            "mode": "decompress",
            "input": str(gz),
            "output": str(out),
            "from_path": True,
        }
    )
    assert out.read_text() == "payload"


# ---------- port_check ----------


def test_port_check_returns_true_for_open_port() -> None:
    """Bind a temporary listener and confirm port_check sees it."""
    listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    listener.bind(("127.0.0.1", 0))
    listener.listen(1)
    port = listener.getsockname()[1]
    try:
        r = PortCheckPlugin().execute(
            params={"host": "127.0.0.1", "port": port, "timeout_ms": 1000}
        )
        assert r.value is True
        assert r.exit_code == 0
    finally:
        listener.close()


def test_port_check_returns_false_for_closed_port() -> None:
    # Port 9 is the discard service; bind ephemerally then close to get a
    # known-closed port number, more reliable than picking a static one.
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.bind(("127.0.0.1", 0))
    closed_port = s.getsockname()[1]
    s.close()

    r = PortCheckPlugin().execute(
        params={"host": "127.0.0.1", "port": closed_port, "timeout_ms": 500}
    )
    assert r.value is False
    assert r.exit_code == 1


def test_port_check_invalid_port_raises() -> None:
    with pytest.raises(ValueError, match="port out of range"):
        PortCheckPlugin().execute(params={"host": "x", "port": 999_999})


# ---------- env_vars ----------


def test_env_vars_get_returns_value(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("CIRCUITRY_TEST_VAR", "yes")
    r = EnvVarsPlugin().execute(
        params={"mode": "get", "name": "CIRCUITRY_TEST_VAR"}
    )
    assert r.value == "yes"


def test_env_vars_get_missing_returns_default(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("CIRCUITRY_TEST_MISSING", raising=False)
    r = EnvVarsPlugin().execute(
        params={"mode": "get", "name": "CIRCUITRY_TEST_MISSING", "default": "x"}
    )
    assert r.value == "x"


def test_env_vars_list_redacts_secrets_by_default(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("CIRCUITRY_TEST_API_KEY", "sk-secret")
    monkeypatch.setenv("CIRCUITRY_TEST_HARMLESS", "ok")
    r = EnvVarsPlugin().execute(
        params={"mode": "list", "prefix": "CIRCUITRY_TEST_"}
    )
    assert r.value["CIRCUITRY_TEST_API_KEY"] == "***"
    assert r.value["CIRCUITRY_TEST_HARMLESS"] == "ok"


def test_env_vars_list_can_include_secrets(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("CIRCUITRY_TEST_TOKEN", "real-token")
    r = EnvVarsPlugin().execute(
        params={
            "mode": "list",
            "prefix": "CIRCUITRY_TEST_",
            "include_secrets": True,
        }
    )
    assert r.value["CIRCUITRY_TEST_TOKEN"] == "real-token"


# ---------- hash ----------


def test_hash_sha256_default() -> None:
    r = HashPlugin().execute(params={"input": "abc"})
    assert r.value == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"


def test_hash_md5_short() -> None:
    r = HashPlugin().execute(params={"input": "abc", "algorithm": "md5"})
    assert r.value == "900150983cd24fb0d6963f7d28e17f72"


def test_hash_unsupported_algorithm() -> None:
    with pytest.raises(ValueError, match="unsupported algorithm"):
        HashPlugin().execute(params={"input": "x", "algorithm": "rot13"})


def test_hash_from_path(tmp_path: Path) -> None:
    f = tmp_path / "x.txt"
    f.write_bytes(b"abc")
    r = HashPlugin().execute(params={"input": str(f), "from_path": True})
    assert r.value == "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"


def test_hash_base64_output() -> None:
    r = HashPlugin().execute(
        params={"input": "abc", "output_format": "base64"}
    )
    assert r.value == "ungWv48Bz+pBQUDeXa4iI7ADYaOWF3qctBD/YfIAFa0="


# ---------- base64 ----------


def test_base64_encode_decode_roundtrip() -> None:
    plugin = Base64Plugin()
    enc = plugin.execute(params={"mode": "encode", "input": "hello"})
    assert enc.value == "aGVsbG8="
    dec = plugin.execute(params={"mode": "decode", "input": enc.value})
    assert dec.value == "hello"


def test_base64_urlsafe_no_padding() -> None:
    enc = Base64Plugin().execute(
        params={"mode": "encode", "input": "??", "urlsafe": True}
    )
    assert "=" not in enc.value


def test_base64_urlsafe_decode_handles_missing_padding() -> None:
    plugin = Base64Plugin()
    enc = plugin.execute(
        params={"mode": "encode", "input": "ab", "urlsafe": True}
    )
    dec = plugin.execute(
        params={"mode": "decode", "input": enc.value, "urlsafe": True}
    )
    assert dec.value == "ab"


# ---------- hex ----------


def test_hex_encode_decode_roundtrip() -> None:
    plugin = HexPlugin()
    enc = plugin.execute(params={"mode": "encode", "input": "hi"})
    assert enc.value == "6869"
    dec = plugin.execute(params={"mode": "decode", "input": enc.value})
    assert dec.value == "hi"


def test_hex_encode_with_separator() -> None:
    r = HexPlugin().execute(
        params={"mode": "encode", "input": "hi", "separator": ":"}
    )
    assert r.value == "68:69"


def test_hex_decode_strips_non_hex_chars() -> None:
    """Decode tolerates separators / whitespace."""
    r = HexPlugin().execute(params={"mode": "decode", "input": "68 69"})
    assert r.value == "hi"


# ---------- uuid ----------


def test_uuid_v4_unique() -> None:
    a = UuidPlugin().execute(params={}).value
    b = UuidPlugin().execute(params={}).value
    assert a != b
    assert len(a) == 36 and a.count("-") == 4


def test_uuid_v5_deterministic() -> None:
    plugin = UuidPlugin()
    a = plugin.execute(
        params={"version": 5, "namespace": "dns", "name": "example.com"}
    ).value
    b = plugin.execute(
        params={"version": 5, "namespace": "dns", "name": "example.com"}
    ).value
    assert a == b


def test_uuid_count_returns_list() -> None:
    r = UuidPlugin().execute(params={"count": 3})
    assert isinstance(r.value, list) and len(r.value) == 3
    assert len(set(r.value)) == 3  # all unique


def test_uuid_hex_strips_dashes() -> None:
    r = UuidPlugin().execute(params={"hex": True})
    assert "-" not in r.value


def test_uuid_v5_requires_namespace_and_name() -> None:
    with pytest.raises(ValueError, match="v5 requires"):
        UuidPlugin().execute(params={"version": 5, "name": "x"})


# ---------- cross-cutting contract conformance ----------


def test_each_plugin_returns_conforming_tool_result() -> None:
    """Every stdlib plugin's happy-path result satisfies the ToolResult
    contract checker."""
    cases: list[tuple[str, dict[str, Any]]] = [
        ("clock", {}),
        ("math", {"expression": "1+1"}),
        ("regex", {"pattern": r"\d", "input": "a1b2"}),
        ("json", {"mode": "parse", "input": '{"a":1}'}),
        ("uuid", {}),
        ("hash", {"input": "x"}),
        ("base64", {"mode": "encode", "input": "x"}),
        ("hex", {"mode": "encode", "input": "x"}),
    ]
    for name, params in cases:
        plugin = build_plugin(plugin_name=name, runtime={})
        result = plugin.execute(params=params)
        diagnostics = validate_tool_result(result, plugin_name=name)
        assert diagnostics == [], f"{name}: {diagnostics}"
