"""Tests for subprocess-wrapping tool plugins.

The pass-through plugins (git, ripgrep, pytest, awk, sed, pandoc,
mediainfo, imagemagick, exiftool, yt_dlp, 7z, ping, traceroute, docker,
kubectl, gh, linter, ocr) all delegate to ``_subprocess.GenericSubprocessTool``,
so they share most semantics. We exercise the helper directly (with
mocked subprocess.run) and verify each plugin's factory wiring +
binary-resolution metadata, instead of duplicating the same mocked
test 18 times.

The dedicated plugins (shell, gpg, diff_patch, pdf_render, web_search,
weather) get focused per-plugin tests covering their unique semantics.
"""

from __future__ import annotations

import json as _json
import shutil
import subprocess
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import pytest

from circuitry.plugins import build_plugin
from circuitry.plugins._subprocess import (
    GenericSubprocessTool,
    check_binary,
    resolve_binary,
    run_binary,
)
from circuitry.plugins.base import validate_tool_result
from circuitry.plugins.diff_patch import DiffPatchPlugin
from circuitry.plugins.gpg import GpgPlugin
from circuitry.plugins.shell import ShellPlugin
from circuitry.plugins.weather import WeatherPlugin
from circuitry.plugins.web_search import WebSearchPlugin


@dataclass(frozen=True)
class FakeProc:
    returncode: int
    stdout: str = ""
    stderr: str = ""


# ---------------------------------------------------------------------------
# _subprocess helper
# ---------------------------------------------------------------------------


def test_resolve_binary_picks_first_match(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(shutil, "which", lambda name: "/bin/" + name if name == "second" else None)
    assert resolve_binary(("first", "second", "third")) == "/bin/second"


def test_resolve_binary_returns_none_when_all_missing(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(shutil, "which", lambda name: None)
    assert resolve_binary(("nope", "noway")) is None


def test_check_binary_reports_missing(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(shutil, "which", lambda name: None)
    r = check_binary(("foo",))
    assert r.ok is False
    assert "binary:foo" in r.missing


def test_run_binary_happy_path(monkeypatch: pytest.MonkeyPatch) -> None:
    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured["cmd"] = cmd
        captured["kwargs"] = kwargs
        return FakeProc(returncode=0, stdout="out", stderr="warn")

    monkeypatch.setattr(subprocess, "run", fake_run)
    r = run_binary(binary="/usr/bin/echo", args=["hi"], timeout_seconds=5)
    assert r.value == "out"
    assert r.exit_code == 0
    assert validate_tool_result(r, plugin_name="generic") == []
    assert captured["cmd"] == ["/usr/bin/echo", "hi"]
    assert captured["kwargs"]["timeout"] == 5
    assert captured["kwargs"]["text"] is True


def test_run_binary_raises_on_nonzero_by_default(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    def fake_run(*a: Any, **k: Any) -> FakeProc:
        return FakeProc(returncode=2, stderr="boom")

    monkeypatch.setattr(subprocess, "run", fake_run)
    with pytest.raises(RuntimeError, match="exit 2"):
        run_binary(binary="/x", args=[])


def test_run_binary_allow_nonzero_returns_result(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    def fake_run(*a: Any, **k: Any) -> FakeProc:
        return FakeProc(returncode=1, stderr="oops")

    monkeypatch.setattr(subprocess, "run", fake_run)
    r = run_binary(binary="/x", args=[], allow_nonzero=True)
    assert r.exit_code == 1
    assert r.stderr == "oops"


def test_run_binary_translates_filenotfound_to_runtimeerror(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    def fake_run(*a: Any, **k: Any) -> Any:
        raise FileNotFoundError("no such")

    monkeypatch.setattr(subprocess, "run", fake_run)
    with pytest.raises(RuntimeError, match="binary not found"):
        run_binary(binary="/missing", args=[])


def test_run_binary_translates_timeout_to_runtimeerror(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    def fake_run(*a: Any, **k: Any) -> Any:
        raise subprocess.TimeoutExpired(cmd=["x"], timeout=1)

    monkeypatch.setattr(subprocess, "run", fake_run)
    with pytest.raises(RuntimeError, match="exceeded timeout"):
        run_binary(binary="/x", args=[], timeout_seconds=1)


def test_run_binary_rejects_null_byte_in_args() -> None:
    with pytest.raises(ValueError, match="null byte"):
        run_binary(binary="/x", args=["good", "bad\x00"])


# ---------------------------------------------------------------------------
# GenericSubprocessTool
# ---------------------------------------------------------------------------


def test_generic_tool_requires_args_list() -> None:
    plugin = GenericSubprocessTool(name="x", binary_candidates=("x",))
    with pytest.raises(ValueError, match="args"):
        plugin.execute(params={})


def test_generic_tool_runs_resolved_binary(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(shutil, "which", lambda n: f"/usr/bin/{n}" if n == "rg" else None)

    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured["cmd"] = cmd
        return FakeProc(returncode=0, stdout="match\n")

    monkeypatch.setattr(subprocess, "run", fake_run)

    plugin = GenericSubprocessTool(name="ripgrep", binary_candidates=("rg",))
    r = plugin.execute(params={"args": ["TODO", "src/"]})
    assert captured["cmd"] == ["/usr/bin/rg", "TODO", "src/"]
    assert r.value == "match\n"


def test_generic_tool_check_reports_missing(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(shutil, "which", lambda n: None)
    plugin = GenericSubprocessTool(name="rg", binary_candidates=("rg",))
    assert plugin.check().ok is False


# ---------------------------------------------------------------------------
# Per-plugin factory + binary-candidate sanity check
# ---------------------------------------------------------------------------


SUBPROCESS_CATALOG: list[tuple[str, tuple[str, ...]]] = [
    ("git", ("git",)),
    ("ripgrep", ("rg",)),
    ("pytest", ("pytest",)),
    ("awk", ("awk", "gawk", "mawk")),
    ("sed", ("sed", "gsed")),
    ("pandoc", ("pandoc",)),
    ("mediainfo", ("mediainfo",)),
    ("imagemagick", ("magick", "convert")),
    ("exiftool", ("exiftool",)),
    ("yt_dlp", ("yt-dlp",)),
    ("7z", ("7z", "7za", "7zz")),
    ("ping", ("ping",)),
    ("traceroute", ("traceroute", "tracert")),
    ("docker", ("docker",)),
    ("kubectl", ("kubectl",)),
    ("gh", ("gh",)),
    ("linter", ("ruff", "eslint")),
    ("ocr", ("tesseract",)),
]


@pytest.mark.parametrize("name,candidates", SUBPROCESS_CATALOG)
def test_each_subprocess_plugin_factory_wires_correct_binary(
    name: str, candidates: tuple[str, ...]
) -> None:
    plugin = build_plugin(plugin_name=name, runtime={})
    assert plugin.name == name
    # GenericSubprocessTool exposes binary_candidates.
    assert tuple(plugin.binary_candidates) == candidates


# ---------------------------------------------------------------------------
# shell — security-sensitive
# ---------------------------------------------------------------------------


def test_shell_default_allowlist_runs_ls(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(shutil, "which", lambda n: f"/bin/{n}" if n == "ls" else None)

    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured["cmd"] = cmd
        return FakeProc(returncode=0, stdout="a\nb\n")

    monkeypatch.setattr(subprocess, "run", fake_run)

    r = ShellPlugin().execute(params={"command": "ls", "args": ["-la"]})
    assert r.value == "a\nb\n"
    assert captured["cmd"] == ["/bin/ls", "-la"]


def test_shell_rejects_command_outside_allowlist() -> None:
    """AC C.5: non-allowlisted command rejected before any side effect."""
    with pytest.raises(PermissionError, match="not in allowlist"):
        ShellPlugin().execute(params={"command": "rm", "args": ["-rf", "/"]})


def test_shell_per_effect_allowlist_override() -> None:
    """User can broaden allowlist per-effect; reject still fires for
    commands outside the override."""
    with pytest.raises(PermissionError):
        ShellPlugin().execute(
            params={
                "command": "curl",
                "allowed_commands": ["ls"],  # explicit narrow
                "args": ["http://x"],
            }
        )


def test_shell_rejects_path_in_command_name() -> None:
    """``./malicious`` and slashed paths rejected before allowlist check."""
    with pytest.raises(ValueError, match="alphanumeric"):
        ShellPlugin().execute(params={"command": "./malicious"})


def test_shell_rejects_command_with_spaces() -> None:
    """Splitting via shell is not supported — reject space-bearing commands."""
    with pytest.raises(ValueError, match="alphanumeric"):
        ShellPlugin().execute(params={"command": "ls -la"})


def test_shell_rejects_newline_in_args() -> None:
    with pytest.raises(ValueError, match="forbidden char"):
        ShellPlugin().execute(
            params={"command": "echo", "args": ["line1\nline2"]}
        )


def test_shell_check_always_ok() -> None:
    assert ShellPlugin().check().ok is True


# ---------------------------------------------------------------------------
# gpg — multi-mode
# ---------------------------------------------------------------------------


def test_gpg_check_reports_missing(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(shutil, "which", lambda n: None)
    r = GpgPlugin().check()
    assert r.ok is False
    assert "binary:gpg" in r.missing


def test_gpg_encrypt_calls_binary_with_recipient(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(shutil, "which", lambda n: f"/bin/{n}" if n == "gpg" else None)

    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured["cmd"] = cmd
        captured["input"] = kwargs.get("input")
        return FakeProc(returncode=0, stdout="-----BEGIN PGP MESSAGE-----\n...")

    monkeypatch.setattr(subprocess, "run", fake_run)

    r = GpgPlugin().execute(
        params={"mode": "encrypt", "recipient": "alice@x", "input": "secret"}
    )
    assert "BEGIN PGP" in r.value
    assert "--recipient" in captured["cmd"]
    assert "alice@x" in captured["cmd"]
    assert captured["input"] == "secret"


def test_gpg_verify_returns_bool_via_exit_code(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(shutil, "which", lambda n: f"/bin/{n}" if n == "gpg" else None)

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        return FakeProc(returncode=1, stderr="BAD signature")

    monkeypatch.setattr(subprocess, "run", fake_run)

    r = GpgPlugin().execute(
        params={
            "mode": "verify",
            "input": "data",
            "signature": "-----BEGIN PGP SIGNATURE-----...",
        }
    )
    assert r.value is False
    assert r.exit_code == 1


def test_gpg_passphrase_masked_in_error(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(shutil, "which", lambda n: f"/bin/{n}" if n == "gpg" else None)
    secret = "hunter2-super-secret"

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        return FakeProc(returncode=2, stderr=f"bad pass {secret}")

    monkeypatch.setattr(subprocess, "run", fake_run)

    with pytest.raises(RuntimeError) as exc:
        GpgPlugin().execute(
            params={
                "mode": "decrypt",
                "input": "x",
                "passphrase": secret,
            }
        )
    assert secret not in str(exc.value)


def test_gpg_unknown_mode_raises() -> None:
    with pytest.raises(ValueError, match="mode must be one of"):
        GpgPlugin().execute(params={"mode": "explode", "input": "x"})


# ---------------------------------------------------------------------------
# diff_patch
# ---------------------------------------------------------------------------


def test_diff_patch_diff_strings() -> None:
    r = DiffPatchPlugin().execute(
        params={
            "mode": "diff",
            "from": "line1\nline2\n",
            "to": "line1\nline2_changed\n",
            "from_label": "before",
            "to_label": "after",
        }
    )
    assert "before" in r.value
    assert "after" in r.value
    assert "-line2" in r.value
    assert "+line2_changed" in r.value


def test_diff_patch_diff_files(tmp_path: Path) -> None:
    a = tmp_path / "a.txt"
    a.write_text("alpha\n")
    b = tmp_path / "b.txt"
    b.write_text("alpha\nbeta\n")
    r = DiffPatchPlugin().execute(
        params={
            "mode": "diff",
            "from": str(a), "from_path": True,
            "to": str(b), "to_path": True,
        }
    )
    assert "+beta" in r.value


def test_diff_patch_unknown_mode() -> None:
    with pytest.raises(ValueError, match="unknown mode"):
        DiffPatchPlugin().execute(
            params={"mode": "merge", "from": "a", "to": "b"}
        )


def test_diff_patch_check_returns_ok() -> None:
    """Diff mode is stdlib; check() doesn't fail just because patch
    binary may be missing."""
    assert DiffPatchPlugin().check().ok is True


# ---------------------------------------------------------------------------
# web_search — DuckDuckGo IA via curl
# ---------------------------------------------------------------------------


def test_web_search_calls_duckduckgo_with_format_json(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(shutil, "which", lambda n: "/usr/bin/curl" if n == "curl" else None)

    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured["cmd"] = cmd
        return FakeProc(
            returncode=0,
            stdout=_json.dumps({"AbstractText": "Yaml is a data language"}),
        )

    monkeypatch.setattr(subprocess, "run", fake_run)

    r = WebSearchPlugin().execute(params={"query": "yaml"})
    assert r.value["AbstractText"] == "Yaml is a data language"
    url = captured["cmd"][-1]
    assert "duckduckgo.com" in url
    assert "format=json" in url
    assert "q=yaml" in url


def test_web_search_requires_query() -> None:
    with pytest.raises(ValueError, match="query"):
        WebSearchPlugin().execute(params={})


def test_web_search_curl_failure_raises(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(shutil, "which", lambda n: "/usr/bin/curl" if n == "curl" else None)

    def fake_run(*a: Any, **k: Any) -> FakeProc:
        return FakeProc(returncode=22, stderr="HTTP 503")

    monkeypatch.setattr(subprocess, "run", fake_run)
    with pytest.raises(RuntimeError, match="web_search request failed"):
        WebSearchPlugin().execute(params={"query": "x"})


def test_web_search_check_reports_curl_missing(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(shutil, "which", lambda n: None)
    r = WebSearchPlugin().check()
    assert r.ok is False
    assert "binary:curl" in r.missing


# ---------------------------------------------------------------------------
# weather — wttr.in via curl
# ---------------------------------------------------------------------------


def test_weather_default_returns_text(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(shutil, "which", lambda n: "/usr/bin/curl" if n == "curl" else None)

    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured["cmd"] = cmd
        return FakeProc(returncode=0, stdout="Boston: ☀ +60°F\n")

    monkeypatch.setattr(subprocess, "run", fake_run)
    r = WeatherPlugin().execute(params={"location": "Boston"})
    assert "Boston" in r.value
    url = captured["cmd"][-1]
    assert "wttr.in/Boston" in url


def test_weather_json_mode_returns_parsed(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(shutil, "which", lambda n: "/usr/bin/curl" if n == "curl" else None)

    payload = {"current_condition": [{"temp_F": "60"}]}

    def fake_run(*a: Any, **k: Any) -> FakeProc:
        return FakeProc(returncode=0, stdout=_json.dumps(payload))

    monkeypatch.setattr(subprocess, "run", fake_run)
    r = WeatherPlugin().execute(params={"location": "Boston", "json": True})
    assert r.value == payload


def test_weather_format_and_json_mutually_exclusive() -> None:
    with pytest.raises(ValueError, match="not both"):
        WeatherPlugin().execute(
            params={"location": "x", "format": "%C", "json": True}
        )


def test_weather_format_string_appended_to_url(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(shutil, "which", lambda n: "/usr/bin/curl" if n == "curl" else None)

    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured["cmd"] = cmd
        return FakeProc(returncode=0, stdout="Cloudy")

    monkeypatch.setattr(subprocess, "run", fake_run)
    WeatherPlugin().execute(params={"location": "Boston", "format": "%C"})
    assert "format=%25C" in captured["cmd"][-1] or "format=%C" in captured["cmd"][-1]
