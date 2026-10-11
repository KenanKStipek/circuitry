"""Shared subprocess harness for the conformance suite: loads a case's
`case.json` and runs `cof run` (as `python -m circuitry.cli.app run`, the
same invocation `tests/cli/test_run_cancel_parallel.py` uses for a hermetic
CLI subprocess) with a temporary `HOME`, no credential env vars, no
`CIRCUITRY_*` env vars (CLAUDE.md's hermeticity rule — `CIRCUITRY_CONFIG`,
`CIRCUITRY_MODEL`, `CIRCUITRY_ADAPTER` and friends all change
`effective_settings` if the caller's shell happens to export one), the
case's own `fakes/` directory first on `PATH` if present, and cwd set to the
case directory. Used by both `scripts/generate-conformance-cases.py` and
the Python/electricity pytest runners, so generation and verification can
never drift apart on *how* a case is run — only on what's done with the
result.
"""

from __future__ import annotations

import contextlib
import json
import os
import subprocess
import sys
from collections.abc import Iterator
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from circuitry.adapters.scripted import (
    LEFTOVER_REPLIES_EXPORT_ENV_VAR,
    total_replies_by_path,
)

from .mock_http import MockHttpServer, RecordedRequest
from .normalize import (
    assert_states_equal,
    canonicalize_http_requests,
    normalize_for_comparison,
)

CASES_DIR = Path(__file__).resolve().parent / "cases"

#: Substituted, verbatim, for the mock HTTP server's real ephemeral port
#: wherever a case's own `config` file content or `cli_args` need to name
#: it -- an adapter's `base_url` (config) or a `-e` input an `http` tool
#: URL template reads (cli_args). The harness materializes both with the
#: real port right before invoking either engine; see `run_case` and
#: `materialize_cli_args` below.
MOCK_HTTP_PORT_PLACEHOLDER = "__MOCK_HTTP_PORT__"

#: Mirrors `tests/cli/test_run_cancel_parallel.py`'s `_CREDENTIAL_ENV_VARS` —
#: a case subprocess must never see a live credential, real or scripted.
#: `_sandboxed_env` additionally drops every `CIRCUITRY_*` variable
#: (`CIRCUITRY_CONFIG`, `CIRCUITRY_MODEL`, ... — see `config.py`'s
#: `CONFIG_ENV_VARS`/`_apply_env_vars`), since those aren't credentials but
#: still change `effective_settings` if set in the caller's shell.
CREDENTIAL_ENV_VARS = (
    "OPENAI_API_KEY",
    "ANTHROPIC_API_KEY",
    "CYBERDINER_TOKEN",
    "CYBERDINER_EXPO_URL",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "NPM_TOKEN",
)

DEFAULT_TIMEOUT_SECONDS = 30


@dataclass(frozen=True)
class CaseResult:
    returncode: int
    stdout: str
    stderr: str
    out_path: Path


def list_case_dirs() -> list[Path]:
    return sorted(p.parent for p in CASES_DIR.glob("*/case.json"))


#: `case.json`'s allowed top-level keys (`tests/conformance/README.md`'s table).
_CASE_JSON_KEYS = frozenset(
    {
        "spec_case",
        "description",
        "engines",
        "expect",
        "cli_args",
        "config",
        "also_pretty",
        "error_compare",
        "location_pattern",
        "mock_http_fixture",
        "sort_http_requests",
        "known_divergence",
        "electricity_preview_ok",
        "orchestration",
        "timeout_seconds",
    }
)
_VALID_EXPECT = frozenset({"success", "failure"})
_VALID_ERROR_COMPARE = frozenset({"exact", "location"})
_VALID_ENGINES = frozenset({"python", "electricity"})
#: `known_divergence`'s only two keys (`tests/conformance/README.md`'s table):
#: the dotted-path location in the captured state, and the value electricity
#: is documented to produce there instead.
_KNOWN_DIVERGENCE_KEYS = frozenset({"location", "electricity_value"})


def load_case(case_dir: Path) -> dict[str, Any]:
    metadata: dict[str, Any] = json.loads((case_dir / "case.json").read_text(encoding="utf-8"))

    unknown_keys = set(metadata) - _CASE_JSON_KEYS
    if unknown_keys:
        raise ValueError(
            f"{case_dir.name}: case.json has unknown key(s): {sorted(unknown_keys)}"
        )

    metadata.setdefault("orchestration", "orchestration.yml")
    metadata.setdefault("cli_args", [])
    metadata.setdefault("engines", ["python"])
    metadata.setdefault("config", None)
    metadata.setdefault("mock_http_fixture", None)
    metadata.setdefault("sort_http_requests", False)
    metadata.setdefault("known_divergence", None)
    metadata.setdefault("electricity_preview_ok", False)
    metadata.setdefault("timeout_seconds", DEFAULT_TIMEOUT_SECONDS)

    if not isinstance(metadata["electricity_preview_ok"], bool):
        raise ValueError(f"{case_dir.name}: case.json 'electricity_preview_ok' must be a bool")
    if not isinstance(metadata["sort_http_requests"], bool):
        raise ValueError(f"{case_dir.name}: case.json 'sort_http_requests' must be a bool")

    if "expect" not in metadata:
        raise ValueError(f"{case_dir.name}: case.json is missing the required 'expect' key")
    if metadata["expect"] not in _VALID_EXPECT:
        raise ValueError(
            f"{case_dir.name}: case.json 'expect' must be one of {sorted(_VALID_EXPECT)}, "
            f"got {metadata['expect']!r}"
        )
    engines = metadata["engines"]
    if not isinstance(engines, list) or not engines or set(engines) - _VALID_ENGINES:
        raise ValueError(
            f"{case_dir.name}: case.json 'engines' must be a non-empty list drawn from "
            f"{sorted(_VALID_ENGINES)}, got {engines!r}"
        )
    if "error_compare" in metadata and metadata["error_compare"] not in _VALID_ERROR_COMPARE:
        raise ValueError(
            f"{case_dir.name}: case.json 'error_compare' must be one of "
            f"{sorted(_VALID_ERROR_COMPARE)}, got {metadata['error_compare']!r}"
        )
    if metadata.get("also_pretty") and metadata["expect"] != "success":
        raise ValueError(
            f"{case_dir.name}: case.json 'also_pretty' only applies to a 'success' case"
        )
    if metadata.get("also_pretty") and metadata.get("mock_http_fixture"):
        raise ValueError(
            f"{case_dir.name}: case.json 'also_pretty' cannot combine with "
            "'mock_http_fixture' -- the --pretty re-run gets neither a fresh mock server "
            "nor a fresh leftover-replies path, and runs after the server has already "
            "stopped"
        )
    if metadata.get("electricity_preview_ok") and (
        metadata["expect"] != "success" or "electricity" not in metadata["engines"]
    ):
        raise ValueError(
            f"{case_dir.name}: case.json 'electricity_preview_ok' only applies to a "
            "'success' case that lists 'electricity' in 'engines' -- it names a case "
            "electricity is expected to refuse, so there is nothing for it to mean "
            "otherwise"
        )
    known_divergence = metadata["known_divergence"]
    if known_divergence is not None:
        if (
            not isinstance(known_divergence, dict)
            or set(known_divergence) != _KNOWN_DIVERGENCE_KEYS
        ):
            raise ValueError(
                f"{case_dir.name}: case.json 'known_divergence' must be a mapping with "
                f"exactly {sorted(_KNOWN_DIVERGENCE_KEYS)}"
            )
        if not isinstance(known_divergence["location"], str) or not known_divergence["location"]:
            raise ValueError(
                f"{case_dir.name}: case.json 'known_divergence.location' must be a "
                "non-empty string"
            )
        if metadata.get("also_pretty"):
            raise ValueError(
                f"{case_dir.name}: case.json 'known_divergence' cannot combine with "
                "'also_pretty' -- the --pretty comparison never applies the override, so "
                "a documented divergence there would just fail the --pretty assertion"
            )
        if metadata["expect"] != "success":
            raise ValueError(
                f"{case_dir.name}: case.json 'known_divergence' only applies to a "
                "'success' case -- a documented divergence is by definition a case "
                "both engines succeed on"
            )
        if set(engines) != _VALID_ENGINES:
            raise ValueError(
                f"{case_dir.name}: case.json 'known_divergence' requires 'engines' to "
                f"list both {sorted(_VALID_ENGINES)} -- there is nothing to diverge "
                "from with only one engine running"
            )
    return metadata


def _sandboxed_env(
    case_dir: Path, home_dir: Path, *, leftover_replies_path: Path | None = None
) -> dict[str, str]:
    env = {
        k: v
        for k, v in os.environ.items()
        if k not in CREDENTIAL_ENV_VARS and not k.startswith("CIRCUITRY_")
    }
    env["HOME"] = str(home_dir)
    fakes_dir = case_dir / "fakes"
    if fakes_dir.is_dir():
        env["PATH"] = f"{fakes_dir}{os.pathsep}{env.get('PATH', '')}"
    # Deliberately set *after* the CIRCUITRY_* filter above: a test-only
    # variable the harness itself injects for this one subprocess, not
    # inherited from the caller's shell (electricity/docs/spec/
    # scripted-replies.md §7) -- never a real config knob, so it is never
    # one of `config.py`'s CONFIG_ENV_VARS and can't change
    # `effective_settings`.
    if leftover_replies_path is not None:
        env[LEFTOVER_REPLIES_EXPORT_ENV_VAR] = str(leftover_replies_path)
    return env


def resolved_scripted_replies_path(case_dir: Path, metadata: dict[str, Any]) -> Path | None:
    """The case's own `runtime.adapters.scripted.replies_file`, resolved
    against the case directory, or `None` when the case's `config` doesn't
    set one (`config` is always JSON -- `cli.config.load_config`'s own
    format -- so this reads it the same way, never the materialized-port
    copy, since a replies-file name never carries the mock-port
    placeholder)."""
    config_name = metadata.get("config")
    if not config_name:
        return None
    config_data = json.loads((case_dir / config_name).read_text(encoding="utf-8"))
    replies_file = (
        ((config_data.get("runtime") or {}).get("adapters") or {}).get("scripted") or {}
    ).get("replies_file")
    if not replies_file:
        return None
    return case_dir / replies_file


def read_leftover_replies(path: Path, *, replies_file: Path | None = None) -> dict[str, int]:
    """The merged leftover-reply counts a run exported (scripted-replies.md
    §7), when the file is present. An **absent** file means no instance
    ever registered for it -- either nothing in the run ever loaded
    *replies_file*, or the engine running it doesn't implement the export at
    all -- so nothing was *consumed*, and every reply *replies_file* itself
    configures is left over; this is computed by parsing that file with the
    same loader the adapter uses (`total_replies_by_path`), never assumed to
    be `{}`. When *replies_file* is also `None` (the case's own config
    doesn't name a scripted replies file at all), there is nothing to check
    and an absent export file means exactly that: `{}`."""
    if path.exists():
        return dict(json.loads(path.read_text(encoding="utf-8")))
    if replies_file is None:
        return {}
    return total_replies_by_path(replies_file)


def assert_no_leftover_replies(
    leftover_replies_path: Path, *, case_dir: Path, metadata: dict[str, Any], case_name: str
) -> None:
    """A case that leaves a scripted reply unused fails here, on either
    engine (issue #450's acceptance criterion) -- over-provisioning a script
    is not itself a run failure (scripted-replies.md §6), but it is always a
    conformance-case bug: a fixture author who left dead replies behind, or
    a document that stopped calling a path the fixture still answers for.
    The one place both pytest runners and the generator make this check, so
    it can never drift into three slightly different copies of the same
    assertion."""
    replies_file = resolved_scripted_replies_path(case_dir, metadata)
    leftover = read_leftover_replies(leftover_replies_path, replies_file=replies_file)
    if leftover:
        raise AssertionError(
            f"{case_name}: scripted replies left over after the run: {leftover} "
            "-- remove them from the replies file or make the document consume them"
        )


def materialize_cli_args(cli_args: list[Any], *, mock_http_port: int | None) -> list[str]:
    """`cli_args` as given, or with `MOCK_HTTP_PORT_PLACEHOLDER` substituted
    for the mock server's real port in every element -- the only way a
    `-e key=value` input can name the port an `http` tool URL needs, since
    the port is only known once the harness has actually started the
    server for this run."""
    args = [str(a) for a in cli_args]
    if mock_http_port is None:
        return args
    return [a.replace(MOCK_HTTP_PORT_PLACEHOLDER, str(mock_http_port)) for a in args]


def materialize_config(
    case_dir: Path, config_name: str, *, mock_http_port: int, tmp_dir: Path
) -> Path:
    """Write *config_name*'s own content, with `MOCK_HTTP_PORT_PLACEHOLDER`
    substituted for the mock server's real port, to *tmp_dir*, and return
    the new absolute path -- an adapter's `base_url` can only name the
    port this way, materialized fresh for each run since the port is
    different every time (electricity/DESIGN.md §12). Both runners and the
    generator call this (and `materialize_cli_args`) so generation and
    verification can never diverge on how the port reaches either engine.
    """
    text = (case_dir / config_name).read_text(encoding="utf-8")
    materialized = text.replace(MOCK_HTTP_PORT_PLACEHOLDER, str(mock_http_port))
    target = tmp_dir / f"materialized-{config_name}"
    target.write_text(materialized, encoding="utf-8")
    return target


def case_redaction_replacements(
    case_dir: Path, *, mock_http_port: int | None = None
) -> list[tuple[str, str]]:
    """`(literal, placeholder)` pairs for `normalize.redact_runtime_strings`,
    in the order they must be applied: the case's own `fakes/`
    subdirectory (a longer path than, and a prefix of, the case directory
    itself) before the case directory, then the mock server's own
    ephemeral port, if one is running for this case -- only in the
    `host:port` forms it can actually appear in (`127.0.0.1:<port>`, the
    loopback address `MockHttpServer` binds, and `localhost:<port>`, since
    a case's own `base_url`/URL input is free to spell the same loopback
    address either way), never as the bare port number on its own: a bare
    digit string would also match anywhere those same digits happen to
    occur inside a run id/UUID, a compact timestamp, or any other field
    with no relation to the mock server at all, corrupting it instead of a
    genuine port reference."""
    replacements: list[tuple[str, str]] = []
    fakes_dir = case_dir / "fakes"
    if fakes_dir.is_dir():
        replacements.append((str(fakes_dir), "<FAKES_DIR>"))
    replacements.append((str(case_dir), "<CASE_DIR>"))
    if mock_http_port is not None:
        port = str(mock_http_port)
        replacements.append((f"127.0.0.1:{port}", "127.0.0.1:<MOCK_PORT>"))
        replacements.append((f"localhost:{port}", "localhost:<MOCK_PORT>"))
    return replacements


def run_case(
    case_dir: Path,
    metadata: dict[str, Any],
    *,
    out_path: Path,
    home_dir: Path,
    pretty: bool = False,
    events_path: Path | None = None,
    live_state_path: Path | None = None,
    leftover_replies_path: Path | None = None,
    mock_http_port: int | None = None,
) -> CaseResult:
    """Run one case's document through `cof run`, writing state to
    `out_path`. `cwd` is the case directory, so `orchestration.yml` and any
    relative `config`/`fakes/` resolve the same way for every case.
    `mock_http_port`, when given, is substituted for
    `MOCK_HTTP_PORT_PLACEHOLDER` in both `cli_args` and (materialized to a
    fresh file under `out_path`'s own directory, never the committed
    `config.json` itself) the case's own `config`."""
    cmd = [
        sys.executable,
        "-m",
        "circuitry.cli.app",
        "run",
        metadata["orchestration"],
        "--out",
        str(out_path),
        "--quiet",
    ]
    if pretty:
        cmd.append("--pretty")
    if events_path is not None:
        cmd += ["--events", str(events_path)]
    if live_state_path is not None:
        cmd += ["--live-state", str(live_state_path)]
    config_name = metadata.get("config")
    if config_name:
        if mock_http_port is not None:
            config_path = materialize_config(
                case_dir, config_name, mock_http_port=mock_http_port, tmp_dir=out_path.parent
            )
            cmd += ["--config", str(config_path)]
        else:
            cmd += ["--config", config_name]
    cmd += materialize_cli_args(metadata.get("cli_args", []), mock_http_port=mock_http_port)

    proc = subprocess.run(
        cmd,
        cwd=case_dir,
        env=_sandboxed_env(case_dir, home_dir, leftover_replies_path=leftover_replies_path),
        capture_output=True,
        text=True,
        timeout=metadata.get("timeout_seconds", DEFAULT_TIMEOUT_SECONDS),
        check=False,
    )
    return CaseResult(proc.returncode, proc.stdout, proc.stderr, out_path)


@contextlib.contextmanager
def mock_http_server(case_dir: Path, metadata: dict[str, Any]) -> Iterator[MockHttpServer | None]:
    """Start the case's own mock HTTP server (`case.json`'s
    `mock_http_fixture`) for the duration of the `with` block, or yield
    `None` when the case doesn't set one. Always stops the server, even
    on failure -- the one place that owns its lifecycle, used by both
    pytest runners and the generator so a case is scripted identically
    everywhere it runs."""
    fixture_name = metadata.get("mock_http_fixture")
    if fixture_name is None:
        yield None
        return
    server = MockHttpServer(case_dir / fixture_name)
    server.start()
    try:
        yield server
    finally:
        server.stop()


def assert_recorded_http_requests(
    case_dir: Path,
    metadata: dict[str, Any],
    recorded_requests: list[RecordedRequest] | None,
    *,
    replacements: list[tuple[str, str]],
) -> None:
    """Compare *recorded_requests* (captured from the case's own mock HTTP
    server, `None` when the case sets no `mock_http_fixture`) against the
    case's own committed `expected.http_requests.json` -- the one place
    both the Python and electricity runners make this comparison, so a gap
    in one (issue #450 item 3: "both engines' records must match") can
    never go unnoticed the way an inlined copy in only one runner did.
    Headers are compared case-insensitively on name and order-insensitively
    as a whole, values exactly (`normalize.canonicalize_http_requests`);
    requests themselves are compared in arrival order unless `case.json`
    sets `sort_http_requests: true` (a tree-flow case, whose concurrent
    branches may dispatch in a different wall-clock order on either
    engine)."""
    if recorded_requests is None:
        return
    expected_requests = json.loads(
        (case_dir / "expected.http_requests.json").read_text(encoding="utf-8")
    )
    actual_requests = [r.to_json() for r in recorded_requests]
    sort_requests = bool(metadata.get("sort_http_requests"))
    assert_states_equal(
        canonicalize_http_requests(
            normalize_for_comparison(actual_requests, replacements), sort_requests=sort_requests
        ),
        canonicalize_http_requests(
            normalize_for_comparison(expected_requests, replacements), sort_requests=sort_requests
        ),
    )


def parse_cli_error(stdout: str) -> str:
    """Pull the `error` field out of `cof run`'s failure-path JSON (printed
    to stdout on any non-zero exit, with or without `--json`)."""
    payload = json.loads(stdout)
    error = payload.get("error")
    if not isinstance(error, str):
        raise AssertionError(f"expected a string 'error' field in CLI output, got: {payload!r}")
    return error
