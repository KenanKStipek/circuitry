"""electricity's own `.env` startup load (issue #468), end to end
against `cof run`'s `load_user_env` -- the same temp `HOME`, the same
synthetic `.env`, on both engines: the same visible effect (a
`CIRCUITRY_ENVIRONMENT` only the `.env` file defines reaching config
resolution) and the same `WARNING:` line for an unsafe-mode file.

Not a `tests/conformance/cases/` case (`harness`'s own per-case
fixtures hold a document/config pair, not a `HOME` layout) -- this is
the lane's own dual-engine parity check for a startup concern neither
engine's document ever sees.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

ELECTRICITY_DIR = Path(__file__).resolve().parents[2] / "electricity"

#: A minimal document both engines already run identically (M0's own
#: supported subset) -- this test cares only about the process
#: environment `cof run`/`electricity` see before they ever open it.
ORCHESTRATION = (
    "effects:\n"
    "  - name: ok\n"
    "    type: tool\n"
    "    provider: json\n"
    "    params: {mode: parse, input: '{}'}\n"
)

#: An `env_vars get` of a name only the `.env` file ever defines --
#: the visible, running-document effect issue #471 named
#: (`env_vars` is implemented on both engines at HEAD, not the stub the
#: PR body once described), proving the variable reaches a tool call,
#: not just config resolution.
_ENV_VARS_ORCHESTRATION = (
    "effects:\n"
    "  - name: env_value\n"
    "    type: tool\n"
    "    provider: env_vars\n"
    "    params: {mode: get, name: USER_ENV_468_PARITY_VALUE}\n"
)

#: Mirrors `electricity/scripts/generate_cli_warning_lines.py::_CREDENTIAL_ENV_VARS`.
_CREDENTIAL_ENV_VARS = (
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "CYBERDINER_TOKEN",
    "CYBERDINER_EXPO_URL",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "NPM_TOKEN",
)


def _sandboxed_env(home_dir: Path) -> dict[str, str]:
    env = {
        k: v
        for k, v in os.environ.items()
        if k not in _CREDENTIAL_ENV_VARS and not k.startswith("CIRCUITRY_")
    }
    env["HOME"] = str(home_dir)
    return env


def _write_user_env(home_dir: Path, text: str, *, mode: int) -> Path:
    config_dir = home_dir / ".config" / "circuitry"
    config_dir.mkdir(parents=True)
    path = config_dir / ".env"
    path.write_text(text, encoding="utf-8")
    path.chmod(mode)
    return path


def _prepare_case_dir(case_dir: Path, orchestration: str = ORCHESTRATION) -> None:
    case_dir.mkdir()
    (case_dir / "orchestration.yml").write_text(orchestration, encoding="utf-8")
    (case_dir / "config.json").write_text("{}", encoding="utf-8")


def _run_cof(
    case_dir: Path, home_dir: Path, *, orchestration: str = ORCHESTRATION
) -> subprocess.CompletedProcess[str]:
    _prepare_case_dir(case_dir, orchestration)
    cmd = [
        sys.executable,
        "-m",
        "circuitry.cli.app",
        "run",
        "orchestration.yml",
        "--quiet",
        "--config",
        "config.json",
    ]
    return subprocess.run(
        cmd, cwd=case_dir, env=_sandboxed_env(home_dir), capture_output=True, text=True,
        timeout=30, check=False,
    )


def _run_electricity(
    binary: Path, case_dir: Path, home_dir: Path, *, orchestration: str = ORCHESTRATION
) -> subprocess.CompletedProcess[str]:
    _prepare_case_dir(case_dir, orchestration)
    cmd = [str(binary), "config.json", "orchestration.yml"]
    return subprocess.run(
        cmd, cwd=case_dir, env=_sandboxed_env(home_dir), capture_output=True, text=True,
        timeout=30, check=False,
    )


@pytest.fixture(scope="module")
def electricity_binary() -> Path:
    """Builds the `electricity` binary once for this module -- same
    pattern as `test_electricity_runner.py`'s own session fixture
    (incremental, so a prior build in the same tree is cache-hit fast)."""
    if shutil.which("cargo") is None:
        pytest.skip("cargo not available; electricity binary cannot be built")
    proc = subprocess.run(
        ["cargo", "build", "-p", "electricity-cli", "--bin", "electricity"],
        cwd=ELECTRICITY_DIR, capture_output=True, text=True, timeout=600, check=False,
    )
    if proc.returncode != 0:
        pytest.fail(f"cargo build -p electricity-cli failed:\n{proc.stdout}\n{proc.stderr}")
    binary = ELECTRICITY_DIR / "target" / "debug" / "electricity"
    assert binary.exists(), f"cargo build succeeded but {binary} is missing"
    return binary


def test_a_dotenv_defined_variable_reaches_config_resolution_the_same_way_on_both_engines(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    _write_user_env(home_dir, "CIRCUITRY_ENVIRONMENT=userenv468staging\n", mode=0o600)

    cof_result = _run_cof(tmp_path / "cof_case", home_dir)
    electricity_result = _run_electricity(electricity_binary, tmp_path / "electricity_case", home_dir)

    expected_line = (
        "WARNING: Unknown environment 'userenv468staging'; falling back to 'dev'. "
        "Valid: ('dev', 'prod', 'test')"
    )
    assert expected_line in cof_result.stderr, cof_result.stderr
    assert expected_line in electricity_result.stderr, electricity_result.stderr


def test_an_unsafe_mode_dotenv_file_is_refused_with_the_same_warning_on_both_engines(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    env_path = _write_user_env(
        home_dir, "CIRCUITRY_ENVIRONMENT=should_never_apply\n", mode=0o622
    )

    cof_result = _run_cof(tmp_path / "cof_case", home_dir)
    electricity_result = _run_electricity(electricity_binary, tmp_path / "electricity_case", home_dir)

    def refusal_line(stderr: str) -> str:
        for line in stderr.splitlines():
            if "is not safe to load" in line:
                return line
        raise AssertionError(f"no refusal warning in stderr: {stderr!r}")

    cof_warning = refusal_line(cof_result.stderr)
    electricity_warning = refusal_line(electricity_result.stderr)
    assert str(env_path) in cof_warning
    assert cof_warning == electricity_warning, (cof_warning, electricity_warning)

    # The file was refused on both engines, so the `CIRCUITRY_ENVIRONMENT`
    # it defines never applied either -- neither run should also report
    # falling back from an unknown environment it never actually saw.
    assert "Unknown environment" not in cof_result.stderr
    assert "Unknown environment" not in electricity_result.stderr


def test_a_dotenv_defined_variable_reaches_a_running_tool_call_the_same_way_on_both_engines(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    _write_user_env(home_dir, "USER_ENV_468_PARITY_VALUE=from_the_dotenv_file\n", mode=0o600)

    cof_result = _run_cof(
        tmp_path / "cof_case", home_dir, orchestration=_ENV_VARS_ORCHESTRATION
    )
    electricity_result = _run_electricity(
        electricity_binary, tmp_path / "electricity_case", home_dir,
        orchestration=_ENV_VARS_ORCHESTRATION,
    )

    assert cof_result.returncode == 0, cof_result.stderr
    assert electricity_result.returncode == 0, electricity_result.stderr
    cof_state = json.loads(cof_result.stdout)
    electricity_state = json.loads(electricity_result.stdout)
    assert cof_state["prime"]["env_value"]["value"] == "from_the_dotenv_file"
    assert electricity_state["prime"]["env_value"]["value"] == "from_the_dotenv_file"


def test_a_group_readable_dotenv_prints_the_exact_same_warning_text_on_both_engines(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    """#471 review finding 8: the readable-by-others warning, compared
    byte for byte rather than with `contains`."""
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    env_path = _write_user_env(home_dir, "CIRCUITRY_ENVIRONMENT=dev\n", mode=0o644)

    cof_result = _run_cof(tmp_path / "cof_case", home_dir)
    electricity_result = _run_electricity(electricity_binary, tmp_path / "electricity_case", home_dir)

    def warning_line(stderr: str) -> str:
        for line in stderr.splitlines():
            if "group/world-readable" in line:
                return line
        raise AssertionError(f"no readable-by-others warning in stderr: {stderr!r}")

    cof_warning = warning_line(cof_result.stderr)
    electricity_warning = warning_line(electricity_result.stderr)
    expected = f"WARNING: {env_path} is mode 0o644 (group/world-readable); run 'chmod 600 {env_path}'."
    assert cof_warning == expected, cof_warning
    assert cof_warning == electricity_warning, (cof_warning, electricity_warning)


def test_an_unreadable_dotenv_prints_the_exact_same_warning_text_on_both_engines(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    """#471 review finding 8: the unreadable-file warning, compared byte
    for byte -- mode 0 rather than `contains`."""
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    env_path = _write_user_env(home_dir, "CIRCUITRY_ENVIRONMENT=dev\n", mode=0o000)
    if os.access(env_path, os.R_OK):
        pytest.skip("running as a user that can read a mode-0 file (e.g. root)")

    try:
        cof_result = _run_cof(tmp_path / "cof_case", home_dir)
        electricity_result = _run_electricity(
            electricity_binary, tmp_path / "electricity_case", home_dir
        )
    finally:
        env_path.chmod(0o600)

    def warning_line(stderr: str) -> str:
        for line in stderr.splitlines():
            if "could not be read" in line:
                return line
        raise AssertionError(f"no unreadable-file warning in stderr: {stderr!r}")

    cof_warning = warning_line(cof_result.stderr)
    electricity_warning = warning_line(electricity_result.stderr)
    expected = f"WARNING: {env_path} could not be read (PermissionError); skipping it."
    assert cof_warning == expected, cof_warning
    assert cof_warning == electricity_warning, (cof_warning, electricity_warning)


def test_a_directory_at_the_dotenv_path_parses_as_empty_on_both_engines(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    """#471 review finding 6: `DotEnv._get_stream`'s own
    `_is_file_or_fifo` check means a directory there is never opened at
    all -- it parses as an empty file (loaded, nothing supplied), not
    an OSError."""
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    config_dir = home_dir / ".config" / "circuitry"
    config_dir.mkdir(parents=True)
    env_dir = config_dir / ".env"
    env_dir.mkdir()
    env_dir.chmod(0o755)

    cof_result = _run_cof(tmp_path / "cof_case", home_dir)
    electricity_result = _run_electricity(electricity_binary, tmp_path / "electricity_case", home_dir)

    assert cof_result.returncode == 0, cof_result.stderr
    assert electricity_result.returncode == 0, electricity_result.stderr
    assert "could not be read" not in cof_result.stderr
    assert "could not be read" not in electricity_result.stderr
    assert "group/world-readable" in cof_result.stderr
    assert "group/world-readable" in electricity_result.stderr


def test_a_key_containing_equals_prints_the_exact_same_warning_and_never_the_value(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    """#473: a single-quoted key can contain `=` (`'A=B'=...`); both
    engines must skip the entry (never crash/panic) with one identical
    `WARNING:` line naming only the key, and keep loading the rest of
    the file."""
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    env_path = _write_user_env(
        home_dir,
        "'A=B'=s3cr3t-conformance-probe\nCIRCUITRY_ENVIRONMENT=dev\n",
        mode=0o600,
    )

    cof_result = _run_cof(tmp_path / "cof_case", home_dir)
    electricity_result = _run_electricity(electricity_binary, tmp_path / "electricity_case", home_dir)

    assert cof_result.returncode == 0, cof_result.stderr
    assert electricity_result.returncode == 0, electricity_result.stderr
    assert "s3cr3t-conformance-probe" not in cof_result.stdout
    assert "s3cr3t-conformance-probe" not in cof_result.stderr
    assert "s3cr3t-conformance-probe" not in electricity_result.stdout
    assert "s3cr3t-conformance-probe" not in electricity_result.stderr

    def invalid_entry_line(stderr: str) -> str:
        for line in stderr.splitlines():
            if "not a valid environment variable name or value" in line:
                return line
        raise AssertionError(f"no invalid-entry warning in stderr: {stderr!r}")

    cof_warning = invalid_entry_line(cof_result.stderr)
    electricity_warning = invalid_entry_line(electricity_result.stderr)
    expected = f"WARNING: {env_path}: skipping 'A=B': not a valid environment variable name or value."
    assert cof_warning == expected, cof_warning
    assert cof_warning == electricity_warning, (cof_warning, electricity_warning)
    assert cof_result.stderr.count("not a valid environment variable name or value") == 1
    assert electricity_result.stderr.count("not a valid environment variable name or value") == 1


def test_a_nul_byte_in_a_value_prints_the_exact_same_warning_and_never_the_value(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    """#473: a raw NUL byte decodes as valid UTF-8 inside a value; both
    engines must skip the entry with one identical `WARNING:` line
    naming only the key, and keep loading the rest of the file."""
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    env_text = "NUL_KEY_473=before_s3cr3t_nul\x00after_nul\nCIRCUITRY_ENVIRONMENT=dev\n"
    env_path = _write_user_env(home_dir, env_text, mode=0o600)

    cof_result = _run_cof(tmp_path / "cof_case", home_dir)
    electricity_result = _run_electricity(electricity_binary, tmp_path / "electricity_case", home_dir)

    assert cof_result.returncode == 0, cof_result.stderr
    assert electricity_result.returncode == 0, electricity_result.stderr
    assert "s3cr3t_nul" not in cof_result.stdout
    assert "s3cr3t_nul" not in cof_result.stderr
    assert "s3cr3t_nul" not in electricity_result.stdout
    assert "s3cr3t_nul" not in electricity_result.stderr

    def invalid_entry_line(stderr: str) -> str:
        for line in stderr.splitlines():
            if "not a valid environment variable name or value" in line:
                return line
        raise AssertionError(f"no invalid-entry warning in stderr: {stderr!r}")

    cof_warning = invalid_entry_line(cof_result.stderr)
    electricity_warning = invalid_entry_line(electricity_result.stderr)
    expected = (
        f"WARNING: {env_path}: skipping 'NUL_KEY_473': "
        "not a valid environment variable name or value."
    )
    assert cof_warning == expected, cof_warning
    assert cof_warning == electricity_warning, (cof_warning, electricity_warning)
    assert cof_result.stderr.count("not a valid environment variable name or value") == 1
    assert electricity_result.stderr.count("not a valid environment variable name or value") == 1
