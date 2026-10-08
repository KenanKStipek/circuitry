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

import json
import os
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Any

CASES_DIR = Path(__file__).resolve().parent / "cases"

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
        "replies_file",
        "known_divergence",
        "orchestration",
        "timeout_seconds",
    }
)
_VALID_EXPECT = frozenset({"success", "failure"})
_VALID_ERROR_COMPARE = frozenset({"exact", "location"})
_VALID_ENGINES = frozenset({"python", "electricity"})


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
    metadata.setdefault("replies_file", None)
    metadata.setdefault("known_divergence", None)
    metadata.setdefault("timeout_seconds", DEFAULT_TIMEOUT_SECONDS)

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
    return metadata


def _sandboxed_env(case_dir: Path, home_dir: Path) -> dict[str, str]:
    env = {
        k: v
        for k, v in os.environ.items()
        if k not in CREDENTIAL_ENV_VARS and not k.startswith("CIRCUITRY_")
    }
    env["HOME"] = str(home_dir)
    fakes_dir = case_dir / "fakes"
    if fakes_dir.is_dir():
        env["PATH"] = f"{fakes_dir}{os.pathsep}{env.get('PATH', '')}"
    return env


def run_case(
    case_dir: Path,
    metadata: dict[str, Any],
    *,
    out_path: Path,
    home_dir: Path,
    pretty: bool = False,
) -> CaseResult:
    """Run one case's document through `cof run`, writing state to
    `out_path`. `cwd` is the case directory, so `orchestration.yml` and any
    relative `config`/`fakes/` resolve the same way for every case."""
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
    if metadata.get("config"):
        cmd += ["--config", metadata["config"]]
    cmd += [str(arg) for arg in metadata.get("cli_args", [])]

    proc = subprocess.run(
        cmd,
        cwd=case_dir,
        env=_sandboxed_env(case_dir, home_dir),
        capture_output=True,
        text=True,
        timeout=metadata.get("timeout_seconds", DEFAULT_TIMEOUT_SECONDS),
        check=False,
    )
    return CaseResult(proc.returncode, proc.stdout, proc.stderr, out_path)


def parse_cli_error(stdout: str) -> str:
    """Pull the `error` field out of `cof run`'s failure-path JSON (printed
    to stdout on any non-zero exit, with or without `--json`)."""
    payload = json.loads(stdout)
    error = payload.get("error")
    if not isinstance(error, str):
        raise AssertionError(f"expected a string 'error' field in CLI output, got: {payload!r}")
    return error
