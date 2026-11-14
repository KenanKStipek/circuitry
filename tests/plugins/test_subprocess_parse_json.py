"""`parse: json` on `shell` and the `GenericSubprocessTool` family (#565)."""

from __future__ import annotations

import json
import os
import stat
import subprocess
import sys
import textwrap
from pathlib import Path

import pytest

from circuitry.core.store import Store
from circuitry.core.tool import ToolDefinition, ToolRuntime
from circuitry.plugins._subprocess import GenericSubprocessTool
from circuitry.plugins.shell import ShellPlugin

_CREDENTIAL_ENV_VARS = ("OPENAI_API_KEY", "CYBERDINER_TOKEN", "CYBERDINER_EXPO_URL")


def _assert_no_window(text: str, secret: str, window: int = 6) -> None:
    """No substring of *secret* at least *window* characters long survives
    in *text*."""
    for start in range(max(len(secret) - window + 1, 0)):
        fragment = secret[start : start + window]
        assert fragment not in text, f"{fragment!r} (secret[{start}:{start + window}]) found in text"


def _script(tmp_path: Path, stdout: str, *, exit_code: int = 0) -> str:
    """An executable that prints *stdout* verbatim and exits *exit_code*."""
    path = tmp_path / "emit"
    path.write_text(
        textwrap.dedent(
            f"""\
            #!{sys.executable}
            import sys
            sys.stdout.write({stdout!r})
            sys.exit({exit_code})
            """
        ),
        encoding="utf-8",
    )
    path.chmod(path.stat().st_mode | stat.S_IXUSR)
    return str(path)


def _generic(binary: str) -> GenericSubprocessTool:
    return GenericSubprocessTool(name="emit", binary_candidates=("emit",), binary=binary)


@pytest.mark.parametrize(
    ("stdout", "expected"),
    [
        ('{"ok": true, "n": 2}\n', {"ok": True, "n": 2}),
        ("[1, 2, 3]", [1, 2, 3]),
        ("42\n", 42),
        ('"text"', "text"),
        ("null", None),
    ],
)
def test_generic_tool_parse_json_value_is_the_parsed_result(
    tmp_path: Path, stdout: str, expected: object
) -> None:
    result = _generic(_script(tmp_path, stdout)).execute(
        params={"args": [], "parse": "json"}
    )
    assert result.value == expected
    assert result.stdout == stdout


def test_without_parse_value_stays_the_text(tmp_path: Path) -> None:
    result = _generic(_script(tmp_path, '{"ok": true}')).execute(params={"args": []})
    assert result.value == '{"ok": true}'


def test_parse_null_is_the_same_as_omitted(tmp_path: Path) -> None:
    result = _generic(_script(tmp_path, '{"ok": true}')).execute(
        params={"args": [], "parse": None}
    )
    assert result.value == '{"ok": true}'


def test_generic_tool_invalid_json_fails_showing_the_start_of_stdout(
    tmp_path: Path,
) -> None:
    tool = _generic(_script(tmp_path, "warning: not json at all\n"))
    with pytest.raises(ValueError) as excinfo:
        tool.execute(params={"args": [], "parse": "json"})
    message = str(excinfo.value)
    assert "stdout is not valid JSON" in message
    assert "warning: not json at all" in message
    assert "Expecting value" in message


def test_invalid_json_excerpt_is_bounded(tmp_path: Path) -> None:
    tool = _generic(_script(tmp_path, "x" * 5000))
    with pytest.raises(ValueError) as excinfo:
        tool.execute(params={"args": [], "parse": "json"})
    assert "x" * 200 in str(excinfo.value)
    assert "x" * 201 not in str(excinfo.value)
    assert len(str(excinfo.value)) < 600


@pytest.mark.parametrize("stdout", ["", "  \n\t\n"])
def test_empty_stdout_is_an_error(tmp_path: Path, stdout: str) -> None:
    tool = _generic(_script(tmp_path, stdout))
    with pytest.raises(ValueError, match="stdout is empty"):
        tool.execute(params={"args": [], "parse": "json"})


def test_scrub_secrets_runs_on_the_whole_stdout_before_the_excerpt_is_cut(
    tmp_path: Path,
) -> None:
    """#600 item 1: a secret straddling the 200-character cut leaves no
    fragment once ``scrub_secrets`` is given -- scrubbing the excerpt
    alone, after the cut, could never find the whole value to replace."""
    secret = "sk-straddle-600"
    stdout = "x" * 190 + secret + "more"
    tool = _generic(_script(tmp_path, stdout))
    with pytest.raises(ValueError) as excinfo:
        tool.execute(
            params={"args": [], "parse": "json"},
            scrub_secrets=lambda text: text.replace(secret, "[secret:x]"),
        )
    message = str(excinfo.value)
    assert secret[:10] not in message
    assert "[secret:x]" in message


def test_without_scrub_secrets_the_excerpt_is_unaffected(tmp_path: Path) -> None:
    tool = _generic(_script(tmp_path, "not json"))
    with pytest.raises(ValueError, match="not json"):
        tool.execute(params={"args": [], "parse": "json"})


def test_unknown_parse_mode_is_rejected_before_the_process_runs(
    tmp_path: Path,
) -> None:
    marker = tmp_path / "ran"
    binary = tmp_path / "touch-marker"
    binary.write_text(f"#!/bin/sh\ntouch {marker}\n", encoding="utf-8")
    binary.chmod(binary.stat().st_mode | stat.S_IXUSR)
    with pytest.raises(ValueError, match=r"params\['parse'\] must be 'json'"):
        _generic(str(binary)).execute(params={"args": [], "parse": "yaml"})
    assert not marker.exists()


def test_nonzero_exit_without_allow_nonzero_still_fails_on_the_exit(
    tmp_path: Path,
) -> None:
    tool = _generic(_script(tmp_path, '{"ok": false}', exit_code=3))
    with pytest.raises(RuntimeError, match="exit 3"):
        tool.execute(params={"args": [], "parse": "json"})


def test_allow_nonzero_parses_stdout_and_keeps_the_exit_code(tmp_path: Path) -> None:
    tool = _generic(_script(tmp_path, '{"ok": false}', exit_code=3))
    result = tool.execute(
        params={"args": [], "parse": "json", "allow_nonzero": True}
    )
    assert result.value == {"ok": False}
    assert result.exit_code == 3
    assert result.stdout == '{"ok": false}'


def test_allow_nonzero_with_unparseable_stdout_names_the_exit_code(
    tmp_path: Path,
) -> None:
    tool = _generic(_script(tmp_path, "boom", exit_code=3))
    with pytest.raises(ValueError, match=r"not valid JSON \(exit 3\)"):
        tool.execute(params={"args": [], "parse": "json", "allow_nonzero": True})


def test_shell_parse_json(tmp_path: Path) -> None:
    result = ShellPlugin().execute(
        params={"command": "echo", "args": ['{"ok": true}'], "parse": "json"}
    )
    assert result.value == {"ok": True}
    assert result.stdout == '{"ok": true}\n'


def test_shell_invalid_json_fails(tmp_path: Path) -> None:
    with pytest.raises(ValueError, match="not valid JSON"):
        ShellPlugin().execute(
            params={"command": "echo", "args": ["plain text"], "parse": "json"}
        )


def test_shell_empty_stdout_fails(tmp_path: Path) -> None:
    with pytest.raises(ValueError, match="stdout is empty"):
        ShellPlugin().execute(params={"command": "echo", "args": [""], "parse": "json"})


def test_shell_unknown_parse_mode_is_rejected() -> None:
    with pytest.raises(ValueError, match=r"shell: params\['parse'\]"):
        ShellPlugin().execute(
            params={"command": "echo", "args": ["{}"], "parse": "toml"}
        )


def test_shell_allowlist_is_checked_before_parse_mode() -> None:
    with pytest.raises(PermissionError):
        ShellPlugin().execute(
            params={"command": "rm", "args": ["x"], "parse": "toml"}
        )


def test_shell_scrub_secrets_runs_on_the_whole_stdout_before_the_excerpt_is_cut() -> None:
    """#600 item 1, for `shell` too."""
    secret = "sk-straddle-600-shell"
    stdout = "x" * 190 + secret
    with pytest.raises(ValueError) as excinfo:
        ShellPlugin().execute(
            params={"command": "echo", "args": ["-n", stdout], "parse": "json"},
            scrub_secrets=lambda text: text.replace(secret, "[secret:x]"),
        )
    message = str(excinfo.value)
    assert secret[:10] not in message
    assert "[secret:x]" in message


def test_tool_effect_value_is_parsed_and_raw_stdout_is_on_meta() -> None:
    defn = ToolDefinition(
        name="report",
        provider="shell",
        params={"command": "echo", "args": ['{"ok": true, "n": [1, 2]}'], "parse": "json"},
    )
    store = Store({})
    ToolRuntime(defn).execute(store=store, ctx={})

    node = store.state["report"]
    assert node["value"] == {"ok": True, "n": [1, 2]}
    assert node["meta"]["stdout"] == '{"ok": true, "n": [1, 2]}\n'
    assert node["meta"]["exit_code"] == 0
    assert node["meta"]["error"] is None


def test_tool_effect_invalid_json_fails_the_effect() -> None:
    defn = ToolDefinition(
        name="report",
        provider="shell",
        params={"command": "echo", "args": ["plain text"], "parse": "json"},
        on_error="continue",
    )
    store = Store({})
    ToolRuntime(defn).execute(store=store, ctx={})

    node = store.state["report"]
    assert node["value"] is None
    assert "not valid JSON" in node["meta"]["error"]
    assert "plain text" in node["meta"]["error"]


def test_e2e_cof_run_subprocess_secret_straddling_parse_json_cut_never_leaks(
    tmp_path: Path,
) -> None:
    """#600 item 1, end to end: a real `cof run` subprocess, a fake secret
    resolved from a temp HOME's own environment (never the real Keychain),
    and every artifact `cof run` can produce -- --out, --events,
    --live-state, stdout and stderr -- checked for the secret and every
    six-character window of it."""
    secret = "tok3n-e2e-straddle-600-xyz"
    fake_awk = tmp_path / "fake-awk"
    fake_awk.write_text(
        f"""#!{sys.executable}
import os
print("x" * 190 + os.environ["FAKE_SECRET_600"] + "tail", end="")
""",
        encoding="utf-8",
    )
    fake_awk.chmod(fake_awk.stat().st_mode | stat.S_IXUSR)

    doc = tmp_path / "doc.yml"
    doc.write_text(
        """
effects:
  - type: tool
    name: run_it
    provider: awk
    params: {args: ["{print}"], parse: json}
""",
        encoding="utf-8",
    )
    config = tmp_path / "config.json"
    config.write_text(
        json.dumps(
            {
                "runtime": {
                    "secrets": {"api": {"env": "FAKE_SECRET_600"}},
                    "plugins": {
                        "awk": {"binary": str(fake_awk), "env": {"TOK": {"secret": "api"}}}
                    },
                }
            }
        ),
        encoding="utf-8",
    )
    home = tmp_path / "home"
    home.mkdir()
    out = tmp_path / "out.json"
    events = tmp_path / "events.jsonl"
    live = tmp_path / "live.json"

    env = {key: value for key, value in os.environ.items() if key not in _CREDENTIAL_ENV_VARS}
    env.update(HOME=str(home), FAKE_SECRET_600=secret, COLUMNS="200")
    proc = subprocess.run(
        [
            sys.executable, "-m", "circuitry.cli.app", "run", str(doc),
            "--config", str(config),
            "--out", str(out),
            "--events", str(events),
            "--live-state", str(live),
            "--skip-preflight",
        ],
        capture_output=True,
        text=True,
        env=env,
        cwd=tmp_path,
        timeout=120,
        check=False,
    )
    assert proc.returncode != 0, proc.stdout + proc.stderr

    artifacts = [proc.stdout, proc.stderr]
    artifacts.extend(path.read_text() for path in (out, events, live) if path.exists())
    # The placeholder itself can be cut short by the 200-character excerpt
    # limit the same way the secret it replaces was -- what matters is
    # that no part of the raw secret survives, not that the whole
    # placeholder does.
    assert any("[secret:ap" in text for text in artifacts)
    for text in artifacts:
        assert secret not in text
        _assert_no_window(text, secret)
