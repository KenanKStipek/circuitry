"""Runs the `electricity` binary (built from `electricity/` with cargo when
available) on each conformance case applicable to it and compares the same
way the Python runner does (electricity/DESIGN.md §12). A case only ever
skips through the preview refusal when it declares `case.json`'s own
`electricity_preview_ok: true` -- content the preview genuinely doesn't
support yet, on either a success or a failure case. Any other preview
refusal fails the test loudly, never a silent skip: a case electricity
wrongly refuses this way is a real regression.
"""

from __future__ import annotations

import json
import shutil
import subprocess
import sys
from pathlib import Path
from typing import NoReturn

import pytest

from . import harness
from .normalize import (
    apply_known_divergence,
    assert_errors_equal,
    assert_events_equal,
    assert_out_serialization,
    assert_states_equal,
    prepare_for_comparison,
    redact_events_text,
    redact_runtime_strings,
    redact_third_party_text_lines,
)

CASE_DIRS = harness.list_case_dirs()
ELECTRICITY_DIR = Path(__file__).resolve().parents[2] / "electricity"
#: The stable substring every electricity preview refusal carries
#: (issue #476), matched in lockstep by `electricity_bytecode::
#: PREVIEW_MARKER` and osi's own e2e test -- only a case that opts in
#: with `case.json`'s own `electricity_preview_ok: true` may legitimately
#: get refused this way, success or failure case alike (issue #511's own
#: review: letting every failure case through unconditionally would mask
#: a regression that brings a refusal back for content this build is
#: actually supposed to run and fail on its own terms). Any other case
#: that hits this marker fails loudly, never skips (PR #441: a
#: case electricity wrongly refuses this way is a real
#: regression).
PREVIEW_MESSAGE_MARKER = "is a preview and can't run this orchestration"

#: electricity's CLI has no notion yet of "no config file" the way `cof
#: run` does (an explicit --config, CIRCUITRY_CONFIG, a discovered
#: project/global file, or none of those — built-in defaults): its
#: positional config argument is required. A case that doesn't set
#: `case.json`'s `"config"` gets this fixture (`{}`) instead — the same
#: "nothing set" starting point `cof run` reaches when sandboxed HOME finds
#: no project or global config — as a documented stand-in until electricity
#: has its own config-discovery tiers to mirror. Many cases now set
#: `"config"` explicitly for both engines (model/adapter config, allowed
#: commands, …); this fixture only backs the cases that don't.
DEFAULT_CONFIG_PATH = Path(__file__).resolve().parent / "default_config.json"


def _case_ids() -> list[str]:
    return [d.name for d in CASE_DIRS]


def _assert_warnings_match(case_dir: Path, metadata: dict, stderr: str) -> None:
    """`tests/conformance/test_python_runner.py`'s own counterpart --
    electricity's `WARNING:`/`ERROR:` stderr lines against the same
    committed `expected.warnings.txt` (issue #442's "Warning lines on
    stderr" item)."""
    actual = harness.warning_lines(stderr, sort=metadata.get("sort_warnings", False))
    actual = redact_third_party_text_lines(actual, metadata.get("third_party_text", []))
    expected_text = (case_dir / "expected.warnings.txt").read_text(encoding="utf-8")
    expected = [line for line in expected_text.splitlines() if line]
    assert actual == expected, (
        f"{case_dir.name}: stderr warning lines mismatch\n  got: {actual}\n  want: {expected}"
    )


def _assert_run_warnings_match(case_dir: Path, metadata: dict, stderr: str) -> None:
    """`test_python_runner.py`'s own counterpart -- opt-in via `case.json`'s
    `compare_run_warnings` key (issue #495)."""
    if not metadata.get("compare_run_warnings"):
        return
    actual = harness.run_warning_lines(stderr)
    expected_text = (case_dir / "expected.run_warnings.txt").read_text(encoding="utf-8")
    expected = [line for line in expected_text.splitlines() if line]
    assert actual == expected, (
        f"{case_dir.name}: stderr run-warning lines mismatch\n  got: {actual}\n  want: {expected}"
    )


#: Set when `cargo build` failed: the first test that needs the binary
#: reports it, and every later one skips (#667), so a missing toolchain
#: shows up as one failure instead of one error per case.
_BUILD_FAILURE: str | None = None
_BUILD_FAILURE_REPORTED = False


def _no_binary() -> NoReturn:
    global _BUILD_FAILURE_REPORTED
    if _BUILD_FAILURE is None:
        pytest.skip("cargo not available; electricity binary cannot be built")
    if not _BUILD_FAILURE_REPORTED:
        _BUILD_FAILURE_REPORTED = True
        pytest.fail(_BUILD_FAILURE)
    pytest.skip("electricity binary unavailable: see the build failure reported earlier")


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
        env=harness.cargo_env(),
        capture_output=True,
        text=True,
        timeout=600,
        check=False,
    )
    if proc.returncode != 0:
        global _BUILD_FAILURE
        _BUILD_FAILURE = (
            "cargo build -p electricity-cli failed (if rustup reports no toolchain, set "
            f"RUSTUP_HOME/CARGO_HOME):\n{proc.stdout}\n{proc.stderr}"
        )
        return None
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
    profile_rel = metadata.get("profile")
    if profile_rel:
        # electricity takes an explicit path, never a discovered name
        # (DESIGN.md §6.11) -- the absolute path lines up both engines'
        # own `{path}` in a schema-failure message under the
        # `<CASE_DIR>/...` redaction (issue #495's own harness addition).
        cmd += ["--profile", str(case_dir / profile_rel)]
    cmd += harness.materialize_cli_args(
        metadata.get("cli_args", []), mock_http_port=mock_http_port, case_dir=case_dir
    )
    proc = subprocess.run(
        cmd,
        cwd=case_dir,
        env=harness._sandboxed_env(
            case_dir,
            home_dir,
            leftover_replies_path=leftover_replies_path,
            extra_env=metadata.get("env"),
            fake_credentials=metadata.get("fake_credentials"),
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
        _no_binary()

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
        recorded_requests = list(server.requests) if server is not None else None

    combined_output = result.stdout + result.stderr
    if (
        result.returncode == 1
        and PREVIEW_MESSAGE_MARKER in combined_output
        and metadata["electricity_preview_ok"]
    ):
        pytest.skip(
            "this case's real document is content this preview doesn't "
            "support yet, declared by case.json's own electricity_preview_ok"
        )
    # Skipping through the preview marker is opt-in, on every case
    # regardless of `expect` (issue #511's own review: letting any preview
    # text through unconditionally would silently mask a regression that
    # brings the refusal back for content this build is actually supposed
    # to run and fail on its own terms). The one way a case opts in is
    # `case.json`'s own `electricity_preview_ok: true` (issue #450): a
    # sample case whose document uses content the preview genuinely
    # doesn't support yet (`prompt`/`shell`/`http`/a document-level
    # `adapter:`/...), kept passing (or, for a failure case, kept failing
    # the *right* way) on the Python engine and deliberately still refused
    # by electricity until the lane that implements that capability flips
    # it (never `known_divergence`, which means *both* engines succeeded
    # with one documented difference -- not applicable when one engine
    # never ran the document at all). Without the flag, any other preview
    # refusal fails the test loudly, success or failure case alike.

    harness.assert_no_leftover_replies(
        leftover_replies_path, case_dir=case_dir, metadata=metadata, case_name=case_dir.name
    )

    harness.assert_no_leaked_credentials(
        harness.run_result_texts(result, events_path=events_path, live_state_path=live_state_path),
        case_name=case_dir.name,
        fake_credentials=metadata.get("fake_credentials"),
    )

    _assert_warnings_match(case_dir, metadata, result.stderr)
    _assert_run_warnings_match(case_dir, metadata, result.stderr)

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
        third_party_text = metadata.get("third_party_text", [])
        assert_states_equal(
            prepare_for_comparison(actual_state, replacements, third_party_text),
            prepare_for_comparison(expected_state, replacements, third_party_text),
        )

        harness.assert_recorded_http_requests(
            case_dir, metadata, recorded_requests, replacements=replacements
        )

        expected_events_text = (case_dir / "expected.events.jsonl").read_text(encoding="utf-8")
        assert_events_equal(
            redact_events_text(events_path.read_text(encoding="utf-8"), replacements, third_party_text),
            redact_events_text(expected_events_text, replacements, third_party_text),
        )

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
            harness.assert_no_leaked_credentials(
                harness.run_result_texts(pretty_result),
                case_name=case_dir.name,
                fake_credentials=metadata.get("fake_credentials"),
            )
            actual_pretty_text = pretty_out_path.read_text(encoding="utf-8")
            assert_out_serialization(actual_pretty_text, pretty=True)
            actual_pretty_state = json.loads(actual_pretty_text)
            expected_pretty_state = json.loads(
                (case_dir / "expected.pretty.json").read_text(encoding="utf-8")
            )
            assert_states_equal(
                prepare_for_comparison(actual_pretty_state, replacements, third_party_text),
                prepare_for_comparison(expected_pretty_state, replacements, third_party_text),
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
        third_party_text = metadata.get("third_party_text", [])
        assert_states_equal(
            prepare_for_comparison(actual_out_state, replacements, third_party_text),
            prepare_for_comparison(expected_out_state, replacements, third_party_text),
        )
        return

    raise AssertionError(f"{case_dir.name}: unknown case.json 'expect': {metadata['expect']!r}")


def test_a_date_as_a_dict_key_exits_nonzero_on_both_engines_with_no_out(
    tmp_path: Path, electricity_binary: Path | None
) -> None:
    """Issue #512's review: `default=`/`dumps_default_str` is never
    consulted for a dict *key* (`json.dumps` doesn't either), so an
    unquoted YAML 1.1 date used as one still fails state serialization --
    with no `--out` to redirect to, `cof run --json` crashes uncaught
    (`dumps_saved_state` raising inside `console.print_json(...)`, exit 1,
    no stdout), and electricity's own non-TTY stdout summary must exit 1
    with a stderr message too, never the old `{"error": ...}` placeholder
    on stdout with exit 0."""
    if electricity_binary is None:
        _no_binary()

    doc = tmp_path / "orch.yml"
    doc.write_text(
        "effects:\n"
        "  - type: tool\n"
        "    name: a\n"
        "    provider: uuid\n"
        "    params:\n"
        "      tag: {2024-01-02: x}\n",
        encoding="utf-8",
    )

    cof_home = tmp_path / "cof-home"
    cof_home.mkdir()
    cof_proc = subprocess.run(
        [sys.executable, "-m", "circuitry.cli.app", "run", str(doc), "--json", "--quiet"],
        cwd=tmp_path,
        env=harness._sandboxed_env(tmp_path, cof_home),
        capture_output=True,
        text=True,
        timeout=harness.DEFAULT_TIMEOUT_SECONDS,
        check=False,
    )
    assert cof_proc.returncode == 1, (cof_proc.stdout, cof_proc.stderr)
    assert cof_proc.stdout == ""

    electricity_home = tmp_path / "electricity-home"
    electricity_home.mkdir()
    config_path = tmp_path / "config.json"
    config_path.write_text("{}", encoding="utf-8")
    electricity_proc = subprocess.run(
        [str(electricity_binary), str(config_path), str(doc)],
        cwd=tmp_path,
        env=harness._sandboxed_env(tmp_path, electricity_home),
        capture_output=True,
        text=True,
        timeout=harness.DEFAULT_TIMEOUT_SECONDS,
        check=False,
    )
    assert electricity_proc.returncode == 1, (electricity_proc.stdout, electricity_proc.stderr)
    assert electricity_proc.stdout == ""
    assert "could not serialize run state" in electricity_proc.stderr
    assert '{"error"' not in electricity_proc.stdout
