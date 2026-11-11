"""`cof run` and `electricity run` on the same command line (issue #728):
the same options (`-e`, `--config`, `--live-state`, `--out`, `--events`,
`--allow-capabilities`, `--adapter`, `--quiet`), the same config
discovery when `--config` is absent, and the same exit code/message for
a missing input, an unknown option, and a missing document -- plus the
legacy `electricity <config.json> <doc>` form staying a working alias.

Not a `tests/conformance/cases/` case (`harness`'s own per-case fixtures
hold a document/config pair and compare final state; several of these
invocations never reach a document load at all) -- same rationale as
`test_user_env_parity.py`, whose `electricity_binary` fixture and
sandboxed-environment helpers this module reuses the pattern of.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest

from . import harness

ELECTRICITY_DIR = Path(__file__).resolve().parents[2] / "electricity"

#: A trivial, adapter-free document both engines already run identically.
ORCHESTRATION = (
    "effects:\n"
    "  - name: ok\n"
    "    type: tool\n"
    "    provider: json\n"
    "    params: {mode: parse, input: '{}'}\n"
)

#: Needs the `shell` capability (`plugins/capabilities.py::PLUGIN_CAPABILITIES`)
#: -- used to show neither engine gates a document run by path on
#: capability consent at all, with or without `--allow-capabilities`
#: (`cli/document_consent.py::enforce_consent`'s own `gate_whole_document`
#: is only ever true for a fetched/remote-library document, never a
#: plain path run; electricity runs documents by path only).
SHELL_ORCHESTRATION = (
    "effects:\n"
    "  - name: ok\n"
    "    type: tool\n"
    "    provider: shell\n"
    "    params: {command: 'true', allowed_commands: ['true']}\n"
)

_CREDENTIAL_ENV_VARS = (
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "CYBERDINER_TOKEN",
    "CYBERDINER_EXPO_URL",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "NPM_TOKEN",
)


def _sandboxed_env(home_dir: Path, extra: dict[str, str] | None = None) -> dict[str, str]:
    env = {
        k: v
        for k, v in os.environ.items()
        if k not in _CREDENTIAL_ENV_VARS and not k.startswith("CIRCUITRY_")
    }
    env["HOME"] = str(home_dir)
    env.update(extra or {})
    return env


def _write_case(case_dir: Path, orchestration: str = ORCHESTRATION) -> None:
    case_dir.mkdir()
    (case_dir / "orchestration.yml").write_text(orchestration, encoding="utf-8")
    (case_dir / "config.json").write_text("{}", encoding="utf-8")


def _run_cof(
    case_dir: Path, home_dir: Path, *, args: list[str], env: dict[str, str] | None = None,
    orchestration: str = ORCHESTRATION,
) -> subprocess.CompletedProcess[str]:
    _write_case(case_dir, orchestration)
    cmd = [sys.executable, "-m", "circuitry.cli.app", "run", *args]
    return subprocess.run(
        cmd, cwd=case_dir, env=_sandboxed_env(home_dir, env), capture_output=True, text=True,
        timeout=30, check=False,
    )


def _run_electricity(
    binary: Path, case_dir: Path, home_dir: Path, *, args: list[str],
    env: dict[str, str] | None = None, orchestration: str = ORCHESTRATION,
) -> subprocess.CompletedProcess[str]:
    _write_case(case_dir, orchestration)
    cmd = [str(binary), "run", *args]
    return subprocess.run(
        cmd, cwd=case_dir, env=_sandboxed_env(home_dir, env), capture_output=True, text=True,
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
        cwd=ELECTRICITY_DIR, env=harness.cargo_env(), capture_output=True, text=True,
        timeout=600, check=False,
    )
    if proc.returncode != 0:
        pytest.fail(f"cargo build -p electricity-cli failed:\n{proc.stdout}\n{proc.stderr}")
    binary = ELECTRICITY_DIR / "target" / "debug" / "electricity"
    assert binary.exists(), f"cargo build succeeded but {binary} is missing"
    return binary


def test_a_basic_run_succeeds_with_the_same_exit_code_and_state_on_both_engines(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    args = ["orchestration.yml", "--config", "config.json"]

    cof_result = _run_cof(tmp_path / "cof_case", home_dir, args=args)
    electricity_result = _run_electricity(
        electricity_binary, tmp_path / "electricity_case", home_dir, args=args
    )

    assert cof_result.returncode == 0, cof_result.stderr
    assert electricity_result.returncode == 0, electricity_result.stderr
    assert json.loads(cof_result.stdout)["prime"]["ok"]["value"] == {}
    assert json.loads(electricity_result.stdout)["prime"]["ok"]["value"] == {}


def test_allow_capabilities_adapter_and_quiet_are_accepted_on_both_engines(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    """None of the three options this issue adds change a plain path
    run's outcome on either engine (`SHELL_ORCHESTRATION`'s own
    docstring) -- this only proves they parse and don't themselves
    break a run, not that they have some other effect."""
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    args = [
        "orchestration.yml", "--config", "config.json",
        "--allow-capabilities", "shell,network",
        "--adapter", "ollama",
        "--quiet",
    ]

    cof_result = _run_cof(tmp_path / "cof_case", home_dir, args=args)
    electricity_result = _run_electricity(
        electricity_binary, tmp_path / "electricity_case", home_dir, args=args
    )

    assert cof_result.returncode == 0, cof_result.stderr
    assert electricity_result.returncode == 0, electricity_result.stderr


def test_a_document_needing_a_capability_runs_without_allow_capabilities_on_either_engine(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    """A document run by path is never gated on capability consent at
    all (`cli/document_consent.py::enforce_consent`'s own
    `gate_whole_document` is false for it): `--allow-capabilities`
    being absent never refuses `SHELL_ORCHESTRATION` on `cof run`, so
    the same is true, by construction, of `electricity run`."""
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    args = ["orchestration.yml", "--config", "config.json"]

    cof_result = _run_cof(
        tmp_path / "cof_case", home_dir, args=args, orchestration=SHELL_ORCHESTRATION
    )
    electricity_result = _run_electricity(
        electricity_binary, tmp_path / "electricity_case", home_dir, args=args,
        orchestration=SHELL_ORCHESTRATION,
    )

    assert cof_result.returncode == 0, cof_result.stderr
    assert electricity_result.returncode == 0, electricity_result.stderr


def test_missing_orchestration_fails_the_same_way_on_both_engines(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    args = ["--config", "config.json"]

    cof_result = _run_cof(tmp_path / "cof_case", home_dir, args=args)
    electricity_result = _run_electricity(
        electricity_binary, tmp_path / "electricity_case", home_dir, args=args
    )

    expected = "Error: Missing orchestration. Use --last or provide a path/name."
    assert cof_result.returncode == 1, cof_result.stderr
    assert electricity_result.returncode == 1, electricity_result.stderr
    assert expected in cof_result.stdout
    assert expected in electricity_result.stdout


def test_orchestration_not_found_fails_with_the_same_message_on_both_engines(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    args = ["does-not-exist.yml", "--config", "config.json"]

    cof_result = _run_cof(tmp_path / "cof_case", home_dir, args=args)
    electricity_result = _run_electricity(
        electricity_binary, tmp_path / "electricity_case", home_dir, args=args
    )

    expected = "Error: Orchestration not found: does-not-exist.yml"
    assert cof_result.returncode == 1, cof_result.stderr
    assert electricity_result.returncode == 1, electricity_result.stderr
    assert expected in cof_result.stdout
    assert expected in electricity_result.stdout


def test_an_unknown_option_fails_with_exit_code_2_on_both_engines(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    """The message text itself is allowed to differ (clap's own usage
    error can't match argparse's word for word, electricity's own
    `main.rs` doc comment) -- only the exit code is compared."""
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    args = ["orchestration.yml", "--config", "config.json", "--bogus"]

    cof_result = _run_cof(tmp_path / "cof_case", home_dir, args=args)
    electricity_result = _run_electricity(
        electricity_binary, tmp_path / "electricity_case", home_dir, args=args
    )

    assert cof_result.returncode == 2, cof_result.stderr
    assert electricity_result.returncode == 2, electricity_result.stderr


def test_config_discovery_finds_a_trusted_project_config_with_no_config_flag(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    home_dir = tmp_path / "home"
    home_dir.mkdir()
    env = {"CIRCUITRY_TRUST_PROJECT_CONFIG": "1"}

    def prepare(case_dir: Path) -> None:
        case_dir.mkdir()
        (case_dir / "orchestration.yml").write_text(ORCHESTRATION, encoding="utf-8")
        (case_dir / "circuitry.config.json").write_text(
            '{"default_model": "project-model"}', encoding="utf-8"
        )

    cof_case = tmp_path / "cof_case"
    electricity_case = tmp_path / "electricity_case"
    prepare(cof_case)
    prepare(electricity_case)

    cof_result = subprocess.run(
        [sys.executable, "-m", "circuitry.cli.app", "run", "orchestration.yml"],
        cwd=cof_case, env=_sandboxed_env(home_dir, env), capture_output=True, text=True,
        timeout=30, check=False,
    )
    electricity_result = subprocess.run(
        [str(electricity_binary), "run", "orchestration.yml"],
        cwd=electricity_case, env=_sandboxed_env(home_dir, env), capture_output=True, text=True,
        timeout=30, check=False,
    )

    assert cof_result.returncode == 0, cof_result.stderr
    assert electricity_result.returncode == 0, electricity_result.stderr


def test_config_discovery_skips_an_untrusted_project_config_with_the_same_warning(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    home_dir = tmp_path / "home"
    home_dir.mkdir()

    def prepare(case_dir: Path) -> None:
        case_dir.mkdir()
        (case_dir / "orchestration.yml").write_text(ORCHESTRATION, encoding="utf-8")
        (case_dir / "circuitry.config.json").write_text(
            '{"default_model": "project-model"}', encoding="utf-8"
        )

    cof_case = tmp_path / "cof_case"
    electricity_case = tmp_path / "electricity_case"
    prepare(cof_case)
    prepare(electricity_case)

    cof_result = subprocess.run(
        [sys.executable, "-m", "circuitry.cli.app", "run", "orchestration.yml"],
        cwd=cof_case, env=_sandboxed_env(home_dir), capture_output=True, text=True,
        timeout=30, check=False,
    )
    electricity_result = subprocess.run(
        [str(electricity_binary), "run", "orchestration.yml"],
        cwd=electricity_case, env=_sandboxed_env(home_dir), capture_output=True, text=True,
        timeout=30, check=False,
    )

    assert cof_result.returncode == 0, cof_result.stderr
    assert electricity_result.returncode == 0, electricity_result.stderr

    def skip_line(stderr: str) -> str:
        for line in stderr.splitlines():
            if "Skipped project config" in line:
                return line
        raise AssertionError(f"no skip warning in stderr: {stderr!r}")

    cof_warning = skip_line(cof_result.stderr)
    electricity_warning = skip_line(electricity_result.stderr)
    assert "not trusted" in cof_warning
    assert cof_warning.replace(str(cof_case), "") == electricity_warning.replace(
        str(electricity_case), ""
    )


def test_circuitry_config_env_var_is_honored_with_no_config_flag(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    home_dir = tmp_path / "home"
    home_dir.mkdir()

    cof_case = tmp_path / "cof_case"
    electricity_case = tmp_path / "electricity_case"
    _write_case(cof_case)
    _write_case(electricity_case)
    cof_env_config = cof_case / "env-config.json"
    cof_env_config.write_text("{}", encoding="utf-8")
    electricity_env_config = electricity_case / "env-config.json"
    electricity_env_config.write_text("{}", encoding="utf-8")

    cof_result = subprocess.run(
        [sys.executable, "-m", "circuitry.cli.app", "run", "orchestration.yml"],
        cwd=cof_case,
        env=_sandboxed_env(home_dir, {"CIRCUITRY_CONFIG": str(cof_env_config)}),
        capture_output=True, text=True, timeout=30, check=False,
    )
    electricity_result = subprocess.run(
        [str(electricity_binary), "run", "orchestration.yml"],
        cwd=electricity_case,
        env=_sandboxed_env(home_dir, {"CIRCUITRY_CONFIG": str(electricity_env_config)}),
        capture_output=True, text=True, timeout=30, check=False,
    )

    assert cof_result.returncode == 0, cof_result.stderr
    assert electricity_result.returncode == 0, electricity_result.stderr


def test_the_legacy_positional_form_is_still_a_working_alias_for_electricity(
    tmp_path: Path, electricity_binary: Path,
) -> None:
    home_dir = tmp_path / "home"
    home_dir.mkdir()

    cof_result = _run_cof(
        tmp_path / "cof_case", home_dir, args=["orchestration.yml", "--config", "config.json"]
    )
    case_dir = tmp_path / "electricity_case"
    _write_case(case_dir)
    electricity_result = subprocess.run(
        [str(electricity_binary), "config.json", "orchestration.yml"],
        cwd=case_dir, env=_sandboxed_env(home_dir), capture_output=True, text=True,
        timeout=30, check=False,
    )

    assert cof_result.returncode == 0, cof_result.stderr
    assert electricity_result.returncode == 0, electricity_result.stderr
    assert (
        json.loads(cof_result.stdout)["prime"]["ok"]["value"]
        == json.loads(electricity_result.stdout)["prime"]["ok"]["value"]
    )
