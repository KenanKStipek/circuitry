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
import os
import shutil
import subprocess
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import pytest
from curl_test_support import (
    assert_not_in_argv,
    assert_q_first,
    read_config_headers,
    read_config_url,
)

from circuitry.core import cancellation
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

    # run_tracked (#356) uses `subprocess.Popen` + `communicate`, not
    # `subprocess.run` — this fake stands in for the former now.
    def communicate(self, input: Any = None, timeout: Any = None) -> tuple[str, str]:
        return (self.stdout, self.stderr)

    def __enter__(self) -> FakeProc:
        return self

    def __exit__(self, *exc: Any) -> bool:
        return False


class FakePopen:
    """Stand-in for ``subprocess.Popen`` in tests exercising ``run_binary``'s
    communicate()-based path (#356) without spawning a real process.

    ``captured``, when given, records this call's ``cmd``/``kwargs`` the
    same way the old ``fake_run(cmd, **kwargs)`` monkeypatches did.
    """

    def __init__(
        self,
        cmd: list[str],
        *,
        fake_returncode: int = 0,
        fake_stdout: str = "",
        fake_stderr: str = "",
        captured: dict[str, Any] | None = None,
        **kwargs: Any,
    ) -> None:
        self.args = cmd
        self.pid = 999999
        self.returncode: int | None = None
        self._returncode = fake_returncode
        self._stdout = fake_stdout
        self._stderr = fake_stderr
        self._captured = captured
        if captured is not None:
            captured["cmd"] = cmd
            captured["kwargs"] = kwargs

    def communicate(
        self, input: str | None = None, timeout: float | None = None
    ) -> tuple[str, str]:
        if self._captured is not None:
            self._captured["communicate_kwargs"] = {"input": input, "timeout": timeout}
        self.returncode = self._returncode
        return self._stdout, self._stderr

    def poll(self) -> int | None:
        return self.returncode

    def wait(self, timeout: float | None = None) -> int | None:
        return self.returncode

    def __enter__(self) -> FakePopen:
        return self

    def __exit__(self, *exc_info: Any) -> None:
        return None


def _patch_popen(
    monkeypatch: pytest.MonkeyPatch,
    *,
    returncode: int = 0,
    stdout: str = "",
    stderr: str = "",
    captured: dict[str, Any] | None = None,
    raises: BaseException | None = None,
) -> None:
    """Replace ``subprocess.Popen`` with one that returns/raises as given,
    recording the call on *captured* like the old ``fake_run`` did."""

    def fake_popen(cmd: list[str], **kwargs: Any) -> FakePopen:
        if raises is not None:
            raise raises
        return FakePopen(
            cmd, fake_returncode=returncode, fake_stdout=stdout, fake_stderr=stderr,
            captured=captured, **kwargs,
        )

    monkeypatch.setattr(subprocess, "Popen", fake_popen)


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
    _patch_popen(monkeypatch, returncode=0, stdout="out", stderr="warn", captured=captured)

    r = run_binary(binary="/usr/bin/echo", args=["hi"], timeout_seconds=5)
    assert r.value == "out"
    assert r.exit_code == 0
    assert validate_tool_result(r, plugin_name="generic") == []
    assert captured["cmd"] == ["/usr/bin/echo", "hi"]
    # run_tracked (#385 follow-up) slices a long `timeout` into short polls
    # rather than handing the whole thing to one `communicate()` call, so
    # the signal-handling main thread returns to bytecode that often
    # regardless of which thread a signal reaches — each individual call
    # gets the poll slice, not the step's own full timeout.
    assert captured["communicate_kwargs"]["timeout"] == cancellation._SIGNAL_POLL_SECONDS
    assert captured["kwargs"]["text"] is True


def test_run_binary_raises_on_nonzero_by_default(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _patch_popen(monkeypatch, returncode=2, stderr="boom")
    with pytest.raises(RuntimeError, match="exit 2"):
        run_binary(binary="/x", args=[])


def test_run_binary_allow_nonzero_returns_result(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _patch_popen(monkeypatch, returncode=1, stderr="oops")
    r = run_binary(binary="/x", args=[], allow_nonzero=True)
    assert r.exit_code == 1
    assert r.stderr == "oops"


def test_run_binary_translates_filenotfound_to_runtimeerror(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _patch_popen(monkeypatch, raises=FileNotFoundError("no such"))
    with pytest.raises(RuntimeError, match="binary not found"):
        run_binary(binary="/missing", args=[])


def test_run_binary_translates_timeout_to_runtimeerror(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    class _TimingOutPopen(FakePopen):
        def communicate(
            self, input: str | None = None, timeout: float | None = None
        ) -> tuple[str, str]:
            raise subprocess.TimeoutExpired(cmd=["x"], timeout=1)

        def wait(self, timeout: float | None = None) -> int | None:
            self.returncode = -9
            return self.returncode

    monkeypatch.setattr(subprocess, "Popen", _TimingOutPopen)
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
    _patch_popen(monkeypatch, returncode=0, stdout="match\n", captured=captured)

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
    # Unset settings change nothing (issue #222 acceptance criterion).
    assert plugin.binary is None
    assert plugin.env is None


@pytest.mark.parametrize("name,candidates", SUBPROCESS_CATALOG)
def test_each_subprocess_plugin_factory_wires_binary_and_env_from_config(
    name: str, candidates: tuple[str, ...]
) -> None:
    """runtime.plugins.<name>.binary / .env reach every GenericSubprocessTool
    plugin's factory wiring, not just imagemagick (issue #222)."""
    del candidates
    runtime = {
        "plugins": {
            name: {
                "binary": "/opt/custom/bin/tool",
                "env": {"SOME_VAR": "1"},
            }
        }
    }
    plugin = build_plugin(plugin_name=name, runtime=runtime)
    assert plugin.binary == "/opt/custom/bin/tool"
    assert plugin.env == {"SOME_VAR": "1"}


# ---------------------------------------------------------------------------
# GenericSubprocessTool — configured binary / env (issue #222)
# ---------------------------------------------------------------------------


def test_generic_tool_configured_binary_bypasses_path_search(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    # No candidate is resolvable on PATH — configured binary must still win.
    monkeypatch.setattr(shutil, "which", lambda n: None)

    captured: dict[str, Any] = {}
    _patch_popen(monkeypatch, returncode=0, stdout="ok", captured=captured)
    monkeypatch.setattr(Path, "is_file", lambda self: True)
    monkeypatch.setattr(os, "access", lambda path, mode: True)

    plugin = GenericSubprocessTool(
        name="imagemagick",
        binary_candidates=("magick", "convert"),
        binary="~/opt/imagemagick-omp/bin/magick",
    )
    r = plugin.execute(params={"args": ["in.png", "out.png"]})
    assert captured["cmd"][0] == str(Path("~/opt/imagemagick-omp/bin/magick").expanduser())
    assert r.raw["binary"] == captured["cmd"][0]


def test_generic_tool_configured_binary_missing_fails_naming_setting_and_path() -> None:
    plugin = GenericSubprocessTool(
        name="imagemagick",
        binary_candidates=("magick", "convert"),
        binary="/definitely/not/a/real/path/magick",
    )
    with pytest.raises(
        RuntimeError,
        match=r"runtime\.plugins\.imagemagick\.binary=.*not/a/real/path/magick.*not exist",
    ):
        plugin.execute(params={"args": []})


def test_generic_tool_check_reports_configured_binary_missing() -> None:
    plugin = GenericSubprocessTool(
        name="imagemagick",
        binary_candidates=("magick", "convert"),
        binary="/definitely/not/a/real/path/magick",
    )
    r = plugin.check()
    assert r.ok is False
    assert "binary:imagemagick" in r.missing
    assert "runtime.plugins.imagemagick.binary" in (r.message or "")


def test_generic_tool_configured_binary_relative_path_rejected() -> None:
    plugin = GenericSubprocessTool(
        name="imagemagick",
        binary_candidates=("magick", "convert"),
        binary="relative/magick",
    )
    with pytest.raises(
        RuntimeError,
        match=r"runtime\.plugins\.imagemagick\.binary=.*must be an absolute path",
    ):
        plugin.execute(params={"args": []})


def test_generic_tool_check_reports_relative_configured_binary() -> None:
    plugin = GenericSubprocessTool(
        name="imagemagick",
        binary_candidates=("magick", "convert"),
        binary="relative/magick",
    )
    r = plugin.check()
    assert r.ok is False
    assert "must be an absolute path" in (r.message or "")


def test_generic_tool_check_no_path_match_names_the_setting(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(shutil, "which", lambda n: None)
    plugin = GenericSubprocessTool(name="imagemagick", binary_candidates=("magick", "convert"))
    r = plugin.check()
    assert r.ok is False
    assert "runtime.plugins.imagemagick.binary" in (r.message or "")


def test_plugin_env_override_rejects_non_mapping() -> None:
    from circuitry.plugins._subprocess import plugin_env_override

    with pytest.raises(ValueError, match=r"runtime\.plugins\.imagemagick\.env"):
        plugin_env_override({"env": "A=1"}, plugin_name="imagemagick")


def test_plugin_env_override_rejects_non_dict_non_string() -> None:
    from circuitry.plugins._subprocess import plugin_env_override

    with pytest.raises(ValueError, match=r"runtime\.plugins\.imagemagick\.env"):
        plugin_env_override({"env": 5}, plugin_name="imagemagick")


def test_generic_tool_not_found_message_names_the_setting(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """run_binary's FileNotFoundError message used to say 'override the
    plugin's binary path' — no such override existed. It must now name the
    real setting."""

    monkeypatch.setattr(shutil, "which", lambda n: "/usr/bin/magick" if n == "magick" else None)
    _patch_popen(monkeypatch, raises=FileNotFoundError("no such file"))

    plugin = GenericSubprocessTool(name="imagemagick", binary_candidates=("magick",))
    with pytest.raises(RuntimeError, match=r"runtime\.plugins\.imagemagick\.binary"):
        plugin.execute(params={"args": ["in.png"]})


def test_generic_tool_real_script_proves_binary_and_env_reach_the_process(
    tmp_path: Path,
) -> None:
    """No mocked subprocess.run here — a real tiny executable proves both
    the configured binary and env actually reach the child process,
    without depending on any real CLI tool being installed."""
    script = tmp_path / "fake-magick"
    script.write_text(
        "#!/bin/sh\n"
        'echo "me: $0"\n'
        'echo "threads: $MAGICK_THREAD_LIMIT"\n'
        'echo "args: $@"\n'
    )
    script.chmod(0o755)

    plugin = GenericSubprocessTool(
        name="imagemagick",
        binary_candidates=("magick", "convert"),
        binary=str(script),
        env={"MAGICK_THREAD_LIMIT": "4"},
    )
    r = plugin.execute(params={"args": ["in.png", "-resize", "50%", "out.png"]})

    assert r.exit_code == 0
    assert str(script) in r.stdout
    assert "threads: 4" in r.stdout
    assert "in.png -resize 50% out.png" in r.stdout
    assert r.raw["binary"] == str(script)


def test_generic_tool_env_merges_over_inherited_environment(tmp_path: Path) -> None:
    """env entries are merged over the inherited environment, not a
    replacement for it — an unrelated inherited var must still be visible."""
    script = tmp_path / "fake-tool"
    script.write_text(
        "#!/bin/sh\n"
        'echo "OVERRIDE=$OVERRIDE_VAR"\n'
        'echo "INHERITED=$CIRCUITRY_TEST_INHERITED"\n'
    )
    script.chmod(0o755)

    os.environ["CIRCUITRY_TEST_INHERITED"] = "still-here"
    try:
        plugin = GenericSubprocessTool(
            name="imagemagick",
            binary_candidates=("magick",),
            binary=str(script),
            env={"OVERRIDE_VAR": "set-by-config"},
        )
        r = plugin.execute(params={"args": []})
    finally:
        del os.environ["CIRCUITRY_TEST_INHERITED"]

    assert "OVERRIDE=set-by-config" in r.stdout
    assert "INHERITED=still-here" in r.stdout


# ---------------------------------------------------------------------------
# shell — security-sensitive
# ---------------------------------------------------------------------------


def test_shell_default_allowlist_runs_ls(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(shutil, "which", lambda n: f"/bin/{n}" if n == "ls" else None)

    captured: dict[str, Any] = {}
    _patch_popen(monkeypatch, returncode=0, stdout="a\nb\n", captured=captured)

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


def test_shell_host_pin_narrows_default_allowlist(monkeypatch: pytest.MonkeyPatch) -> None:
    """A host pin intersects with the default allowlist, not replaces it."""
    monkeypatch.setattr(shutil, "which", lambda n: f"/bin/{n}" if n == "ls" else None)
    captured: dict[str, Any] = {}
    _patch_popen(monkeypatch, returncode=0, stdout="a\n", captured=captured)

    plugin = ShellPlugin(pinned_allowed_commands=("ls",))
    r = plugin.execute(params={"command": "ls"})
    assert r.value == "a\n"

    with pytest.raises(PermissionError, match="blocked by the host's"):
        plugin.execute(params={"command": "cat"})


def test_shell_host_pin_narrows_effect_override() -> None:
    """A document's per-effect allowlist can only narrow a host pin, never widen it."""
    plugin = ShellPlugin(pinned_allowed_commands=("ls",))
    with pytest.raises(PermissionError, match="blocked by the host's"):
        plugin.execute(
            params={"command": "curl", "allowed_commands": ["curl"], "args": ["http://x"]}
        )


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

    def fake_run(cmd: list[str], **kwargs: Any) -> Any:
        captured["cmd"] = cmd
        proc = FakeProc(returncode=0, stdout="-----BEGIN PGP MESSAGE-----\n...")

        class _Proc:
            returncode = proc.returncode

            def communicate(self, input: Any = None, timeout: Any = None) -> tuple[str, str]:
                captured["input"] = input
                return proc.communicate(input=input, timeout=timeout)

            def __enter__(self) -> _Proc:
                return self

            def __exit__(self, *exc: Any) -> bool:
                return False

        return _Proc()

    monkeypatch.setattr(subprocess, "Popen", fake_run)

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

    monkeypatch.setattr(subprocess, "Popen", fake_run)

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

    monkeypatch.setattr(subprocess, "Popen", fake_run)

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
        captured["url"] = read_config_url(cmd)
        return FakeProc(
            returncode=0,
            stdout=_json.dumps({"AbstractText": "Yaml is a data language"}),
        )

    monkeypatch.setattr(subprocess, "Popen", fake_run)

    r = WebSearchPlugin().execute(params={"query": "yaml"})
    assert r.value["AbstractText"] == "Yaml is a data language"
    url = captured["url"]
    assert url is not None
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

    monkeypatch.setattr(subprocess, "Popen", fake_run)
    with pytest.raises(RuntimeError, match="web_search request failed"):
        WebSearchPlugin().execute(params={"query": "x"})


def test_web_search_check_reports_curl_missing(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(shutil, "which", lambda n: None)
    r = WebSearchPlugin().check()
    assert r.ok is False
    assert "binary:curl" in r.missing


def test_web_search_uses_q_first_and_no_headers_on_argv(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(shutil, "which", lambda n: "/usr/bin/curl" if n == "curl" else None)

    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured["cmd"] = cmd
        return FakeProc(returncode=0, stdout=_json.dumps({"AbstractText": "x"}))

    monkeypatch.setattr(subprocess, "Popen", fake_run)
    WebSearchPlugin().execute(params={"query": "yaml"})
    assert_q_first(captured["cmd"])


def test_web_search_canary_key_in_extra_params_never_leaks(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Regression for #280: a search API's key commonly travels in
    ``extra_params`` as a query parameter, so a failed request's error
    message must mask it rather than storing it verbatim in the run
    record."""
    monkeypatch.setattr(shutil, "which", lambda n: "/usr/bin/curl" if n == "curl" else None)
    secret = "canary-search-api-key-999"

    def fake_run(*a: Any, **k: Any) -> FakeProc:
        return FakeProc(returncode=22, stderr="HTTP 401")

    monkeypatch.setattr(subprocess, "Popen", fake_run)
    with pytest.raises(RuntimeError) as exc:
        WebSearchPlugin().execute(
            params={"query": "yaml", "extra_params": {"key": secret}}
        )
    assert secret not in str(exc.value)
    assert "cmd=" not in str(exc.value)


def test_web_search_user_pass_base_url_masked_in_error(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Regression for #280: a base_url with embedded userinfo must not be
    echoed verbatim in the error message either."""
    monkeypatch.setattr(shutil, "which", lambda n: "/usr/bin/curl" if n == "curl" else None)

    def fake_run(*a: Any, **k: Any) -> FakeProc:
        return FakeProc(returncode=22, stderr="HTTP 401")

    monkeypatch.setattr(subprocess, "Popen", fake_run)
    with pytest.raises(RuntimeError) as exc:
        WebSearchPlugin().execute(
            params={
                "query": "yaml",
                "base_url": "https://user:canarypw@example.test/search",
            }
        )
    assert "canarypw" not in str(exc.value)


# ---------------------------------------------------------------------------
# weather — wttr.in via curl
# ---------------------------------------------------------------------------


def test_weather_default_returns_text(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(shutil, "which", lambda n: "/usr/bin/curl" if n == "curl" else None)

    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured["cmd"] = cmd
        captured["url"] = read_config_url(cmd)
        return FakeProc(returncode=0, stdout="Boston: ☀ +60°F\n")

    monkeypatch.setattr(subprocess, "Popen", fake_run)
    r = WeatherPlugin().execute(params={"location": "Boston"})
    assert "Boston" in r.value
    url = captured["url"]
    assert url is not None
    assert "wttr.in/Boston" in url


def test_weather_json_mode_returns_parsed(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(shutil, "which", lambda n: "/usr/bin/curl" if n == "curl" else None)

    payload = {"current_condition": [{"temp_F": "60"}]}

    def fake_run(*a: Any, **k: Any) -> FakeProc:
        return FakeProc(returncode=0, stdout=_json.dumps(payload))

    monkeypatch.setattr(subprocess, "Popen", fake_run)
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
        captured["url"] = read_config_url(cmd)
        return FakeProc(returncode=0, stdout="Cloudy")

    monkeypatch.setattr(subprocess, "Popen", fake_run)
    WeatherPlugin().execute(params={"location": "Boston", "format": "%C"})
    url = captured["url"] or ""
    assert "format=%25C" in url or "format=%C" in url


def test_weather_uses_q_first_and_header_off_argv(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(shutil, "which", lambda n: "/usr/bin/curl" if n == "curl" else None)

    captured: dict[str, Any] = {}

    def fake_run(cmd: list[str], **kwargs: Any) -> FakeProc:
        captured["cmd"] = cmd
        captured["headers"] = read_config_headers(cmd)
        return FakeProc(returncode=0, stdout="Cloudy")

    monkeypatch.setattr(subprocess, "Popen", fake_run)
    WeatherPlugin().execute(params={"location": "Boston"})
    assert_q_first(captured["cmd"])
    assert captured["headers"]["Accept-Language"] == "en"
    assert_not_in_argv(captured["cmd"], "Accept-Language")


def test_weather_curl_failure_does_not_echo_cmd(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(shutil, "which", lambda n: "/usr/bin/curl" if n == "curl" else None)

    def fake_run(*a: Any, **k: Any) -> FakeProc:
        return FakeProc(returncode=22, stderr="HTTP 503")

    monkeypatch.setattr(subprocess, "Popen", fake_run)
    with pytest.raises(RuntimeError) as exc:
        WeatherPlugin().execute(params={"location": "Boston"})
    assert "cmd=" not in str(exc.value)
