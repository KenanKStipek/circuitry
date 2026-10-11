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


def _prepare_case_dir(case_dir: Path) -> None:
    case_dir.mkdir()
    (case_dir / "orchestration.yml").write_text(ORCHESTRATION, encoding="utf-8")
    (case_dir / "config.json").write_text("{}", encoding="utf-8")


def _run_cof(case_dir: Path, home_dir: Path) -> subprocess.CompletedProcess[str]:
    _prepare_case_dir(case_dir)
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


def _run_electricity(binary: Path, case_dir: Path, home_dir: Path) -> subprocess.CompletedProcess[str]:
    _prepare_case_dir(case_dir)
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
