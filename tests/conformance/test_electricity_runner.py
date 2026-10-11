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
    apply_known_divergence,
    assert_errors_equal,
    assert_events_equal,
    assert_out_serialization,
    assert_states_equal,
    normalize_for_comparison,
    redact_runtime_strings,
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
    leftover_replies_path: Path | None = None,
    mock_http_port: int | None = None,
) -> harness.CaseResult:
    """Invoke `electricity` the same way `harness.run_case` invokes `cof
    run`, so the two engines are given the same inputs: cwd set to the case
    directory, the orchestration path passed exactly as `case.json` names
    it (relative, not pre-resolved to absolute — otherwise a Circuitry-own
    byte-for-byte message like `"orchestration.yml: duplicate key ..."`
    could never match), and `harness._sandboxed_env` for the environment
    (the case's own `fakes/` first on `PATH`, no credential or
    `CIRCUITRY_*` variables). `mock_http_port`, when given, is materialized
    into the config/cli_args the same way `harness.run_case` does, via the
    same `harness.materialize_config`/`materialize_cli_args` helpers, so
    generation and verification never diverge on how the port reaches
    either engine."""
    if metadata.get("config") and mock_http_port is not None:
        config_path = harness.materialize_config(
            case_dir, metadata["config"], mock_http_port=mock_http_port, tmp_dir=out_path.parent
        )
    elif metadata.get("config"):
        config_path = case_dir / metadata["config"]
    else:
        config_path = DEFAULT_CONFIG_PATH
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
    cmd += harness.materialize_cli_args(metadata.get("cli_args", []), mock_http_port=mock_http_port)
    proc = subprocess.run(
        cmd,
        cwd=case_dir,
        env=harness._sandboxed_env(
            case_dir, home_dir, leftover_replies_path=leftover_replies_path
        ),
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
    events_path = tmp_path / "events.jsonl"
    live_state_path = tmp_path / "live.json"
    leftover_replies_path = tmp_path / "leftover-replies.json"

    with harness.mock_http_server(case_dir, metadata) as server:
        replacements = harness.case_redaction_replacements(
            case_dir, mock_http_port=server.port if server else None
        )
        result = _run_electricity(
            electricity_binary,
            case_dir,
            metadata,
            out_path=out_path,
            home_dir=home_dir,
            events_path=events_path,
            live_state_path=live_state_path,
            leftover_replies_path=leftover_replies_path,
            mock_http_port=server.port if server else None,
        )

    combined_output = result.stdout + result.stderr
    if (
        result.returncode == 1
        and PREVIEW_MESSAGE_MARKER in combined_output
        and (metadata["expect"] != "success" or metadata["electricity_preview_ok"])
    ):
        pytest.skip(
            "this case's real document is content M0-H's own preview "
            "doesn't support yet — electricity/crates/electricity-cli"
        )
    # A *success* case never skips through the preview marker on its own
    # (PR #441 review finding 6) -- a success case electricity wrongly
    # refuses is a real regression, and must fail loudly. The one declared
    # exception is `case.json`'s own `electricity_preview_ok: true` (issue
    # #450): a sample case whose document uses content M0-H's preview
    # genuinely doesn't support yet (`prompt`/`shell`/`http`/...), kept
    # passing on the Python engine and deliberately still refused by
    # electricity until the lane that implements that capability flips it
    # (never `known_divergence`, which means *both* engines succeeded with
    # one documented difference -- not applicable when one engine never ran
    # the document at all).

    harness.assert_no_leftover_replies(
        leftover_replies_path, case_dir=case_dir, metadata=metadata, case_name=case_dir.name
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
        known_divergence = metadata.get("known_divergence")
        if known_divergence is not None:
            expected_state = apply_known_divergence(
                expected_state,
                location=known_divergence["location"],
                value=known_divergence["electricity_value"],
            )
        assert_states_equal(
            normalize_for_comparison(actual_state, replacements),
            normalize_for_comparison(expected_state, replacements),
        )

        expected_events_text = (case_dir / "expected.events.jsonl").read_text(encoding="utf-8")
        assert_events_equal(events_path.read_text(encoding="utf-8"), expected_events_text)

        # `--live-state`'s final write always ends equal to `--out`
        # (electricity/DESIGN.md §10.5, issue #431's acceptance criterion).
        assert live_state_path.read_bytes() == out_path.read_bytes(), (
            "--live-state's final write is not byte-identical to --out"
        )

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
                normalize_for_comparison(actual_pretty_state, replacements),
                normalize_for_comparison(expected_pretty_state, replacements),
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
            redact_runtime_strings(actual_error, replacements),
            redact_runtime_strings(expected_error, replacements),
            byte_for_byte=error_compare == "exact",
            location_pattern=metadata.get("location_pattern"),
        )

        actual_out_text = out_path.read_text(encoding="utf-8")
        assert_out_serialization(actual_out_text, pretty=False)
        actual_out_state = json.loads(actual_out_text)
        expected_out_state = json.loads(
            (case_dir / "expected.out.json").read_text(encoding="utf-8")
        )
        assert_states_equal(
            normalize_for_comparison(actual_out_state, replacements),
            normalize_for_comparison(expected_out_state, replacements),
        )
        return

    raise AssertionError(f"{case_dir.name}: unknown case.json 'expect': {metadata['expect']!r}")
