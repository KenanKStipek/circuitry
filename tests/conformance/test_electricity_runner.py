"""Runs the `electricity` binary (built from `electricity/` with cargo when
available) on each conformance case applicable to it and compares the same
way the Python runner does (electricity/DESIGN.md §12). A case that
expects *failure* still skips when electricity refuses it with the
preview marker for content M0-H's own VM genuinely doesn't support yet
(`prompt`/`loop`/`use`/... -- there is nothing to usefully compare for
those); a case that expects *success* never does -- a success case
electricity wrongly refuses is a real regression, and must fail the
test, not silently skip it.
"""

from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path

import pytest

from . import harness
from .normalize import (
    assert_errors_equal,
    assert_out_serialization,
    assert_states_equal,
    normalize,
)

CASE_DIRS = harness.list_case_dirs()
ELECTRICITY_DIR = Path(__file__).resolve().parents[2] / "electricity"
#: Every M0-H lane (issue #408/#431) is on main now, so nothing under
#: `electricity/` emits this text any more -- a case that expects
#: *failure* and lists `electricity` in its own engines may still
#: legitimately get refused this way (content M0-H's own preview truly
#: doesn't support yet, e.g. `prompt`/`loop`/`use`); a case that expects
#: *success* must not (PR #441 review finding 6: a success case wrongly
#: refused is a real regression, and must fail loudly, not skip).
PREVIEW_MESSAGE_MARKER = "is a preview and cannot run orchestrations yet"

#: electricity's CLI has no notion yet of "no config file" the way `cof
#: run` does (an explicit --config, CIRCUITRY_CONFIG, a discovered
#: project/global file, or none of those — built-in defaults): its
#: positional config argument is required. A case that doesn't set
#: `case.json`'s `"config"` gets this fixture (`{}`) instead — the same
#: "nothing set" starting point `cof run` reaches when sandboxed HOME finds
#: no project or global config — as a documented stand-in until electricity
#: has its own config-discovery tiers to mirror. No case needs anything
#: more today (none is model- or adapter-config-dependent yet); a case that
#: does should set `"config"` explicitly for both engines instead of
#: relying on this default.
DEFAULT_CONFIG_PATH = Path(__file__).resolve().parent / "default_config.json"


def _case_ids() -> list[str]:
    return [d.name for d in CASE_DIRS]


@pytest.fixture(scope="session")
def electricity_binary() -> Path | None:
    """Build the `electricity` binary once per test session. Returns
    `None` (every case skips) when `cargo` isn't on `PATH` — the ordinary
    pytest gate (`quality.yml`) also builds this crate (GitHub's
    `ubuntu-latest` ships `cargo`); this only degrades, rather than
    failing, on a contributor machine that genuinely has none."""
    if shutil.which("cargo") is None:
        return None
    proc = subprocess.run(
        ["cargo", "build", "-p", "electricity-cli", "--bin", "electricity"],
        cwd=ELECTRICITY_DIR,
        capture_output=True,
        text=True,
        timeout=600,
        check=False,
    )
    if proc.returncode != 0:
        pytest.fail(f"cargo build -p electricity-cli failed:\n{proc.stdout}\n{proc.stderr}")
    binary = ELECTRICITY_DIR / "target" / "debug" / "electricity"
    assert binary.exists(), f"cargo build succeeded but {binary} is missing"
    return binary


def _run_electricity(
    binary: Path,
    case_dir: Path,
    metadata: dict,
    *,
    out_path: Path,
    home_dir: Path,
    pretty: bool = False,
    events_path: Path | None = None,
    live_state_path: Path | None = None,
) -> harness.CaseResult:
    """Invoke `electricity` the same way `harness.run_case` invokes `cof
    run`, so the two engines are given the same inputs: cwd set to the case
    directory, the orchestration path passed exactly as `case.json` names
    it (relative, not pre-resolved to absolute — otherwise a Circuitry-own
    byte-for-byte message like `"orchestration.yml: duplicate key ..."`
    could never match), and `harness._sandboxed_env` for the environment
    (the case's own `fakes/` first on `PATH`, no credential or
    `CIRCUITRY_*` variables)."""
    config_path = (
        case_dir / metadata["config"] if metadata.get("config") else DEFAULT_CONFIG_PATH
    )
    cmd = [
        str(binary),
        str(config_path),
        metadata["orchestration"],
        "--out",
        str(out_path),
    ]
    if pretty:
        cmd.append("--pretty")
    if events_path is not None:
        cmd += ["--events", str(events_path)]
    if live_state_path is not None:
        cmd += ["--live-state", str(live_state_path)]
    cmd += [str(arg) for arg in metadata.get("cli_args", [])]
    proc = subprocess.run(
        cmd,
        cwd=case_dir,
        env=harness._sandboxed_env(case_dir, home_dir),
        capture_output=True,
        text=True,
        timeout=metadata.get("timeout_seconds", harness.DEFAULT_TIMEOUT_SECONDS),
        check=False,
    )
    return harness.CaseResult(proc.returncode, proc.stdout, proc.stderr, out_path)


@pytest.mark.parametrize("case_dir", CASE_DIRS, ids=_case_ids())
def test_case(case_dir: Path, tmp_path: Path, electricity_binary: Path | None) -> None:
    metadata = harness.load_case(case_dir)
    if "electricity" not in metadata["engines"]:
        pytest.skip(f"{case_dir.name}: does not apply to the electricity engine")
    if electricity_binary is None:
        pytest.skip("cargo not available; electricity binary cannot be built")

    home_dir = tmp_path / "home"
    home_dir.mkdir()
    out_path = tmp_path / "out.json"
    result = _run_electricity(
        electricity_binary, case_dir, metadata, out_path=out_path, home_dir=home_dir
    )

    combined_output = result.stdout + result.stderr
    if (
        metadata["expect"] != "success"
        and result.returncode == 1
        and PREVIEW_MESSAGE_MARKER in combined_output
    ):
        pytest.skip(
            "this case's real document is content M0-H's own preview "
            "doesn't support yet — electricity/crates/electricity-cli"
        )

    if metadata["expect"] == "success":
        assert result.returncode == 0, (
            f"expected success, got exit {result.returncode}\n"
            f"stdout: {result.stdout}\nstderr: {result.stderr}"
        )
        actual_text = out_path.read_text(encoding="utf-8")
        assert_out_serialization(actual_text, pretty=False)
        actual_state = json.loads(actual_text)
        expected_state = json.loads((case_dir / "expected.json").read_text(encoding="utf-8"))
        assert_states_equal(normalize(actual_state), normalize(expected_state))

        if metadata.get("also_pretty"):
            pretty_out_path = tmp_path / "out.pretty.json"
            pretty_home_dir = tmp_path / "home-pretty"
            pretty_home_dir.mkdir()
            pretty_result = _run_electricity(
                electricity_binary,
                case_dir,
                metadata,
                out_path=pretty_out_path,
                home_dir=pretty_home_dir,
                pretty=True,
            )
            assert pretty_result.returncode == 0, (
                f"expected success (--pretty), got exit {pretty_result.returncode}\n"
                f"stdout: {pretty_result.stdout}\nstderr: {pretty_result.stderr}"
            )
            actual_pretty_text = pretty_out_path.read_text(encoding="utf-8")
            assert_out_serialization(actual_pretty_text, pretty=True)
            actual_pretty_state = json.loads(actual_pretty_text)
            expected_pretty_state = json.loads(
                (case_dir / "expected.pretty.json").read_text(encoding="utf-8")
            )
            assert_states_equal(
                normalize(actual_pretty_state), normalize(expected_pretty_state)
            )
        return

    if metadata["expect"] == "failure":
        assert result.returncode != 0, (
            f"expected a load/check failure, but exited 0\nstdout: {result.stdout}"
        )
        # electricity's non-TTY stdout contract matches `cof run`'s own:
        # `{"ok": false, "error": ...}` JSON on stdout regardless of
        # `--out` (issue #431's "CLI output" decision) — so the same
        # `harness.parse_cli_error` the Python runner uses applies here
        # too.
        actual_error = harness.parse_cli_error(result.stdout)
        expected_error = json.loads((case_dir / "expected.json").read_text(encoding="utf-8"))[
            "error"
        ]
        error_compare = metadata.get("error_compare", "exact")
        assert_errors_equal(
            actual_error,
            expected_error,
            byte_for_byte=error_compare == "exact",
            location_pattern=metadata.get("location_pattern"),
        )
        return

    raise AssertionError(f"{case_dir.name}: unknown case.json 'expect': {metadata['expect']!r}")
