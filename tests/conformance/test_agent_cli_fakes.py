"""Guards against a case accidentally starting a real `claude`/`pi` CLI
(issue #520's own hazard: "A real CLI on `PATH`"). A case that names
`claude_code`/`pi` -- through a document `adapter:`, a prompt's own
`provider:`/`provider_fallbacks`, or a config `default_adapter` -- must
ship its own fake under `fakes/<binary>`, unless its own configured
`binary` is a deliberately-missing `circuitry-test-missing-*` name: on a
machine with a logged-in Claude Code or pi, a case or test without either
would start the real CLI and spend a real request.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from circuitry.cli.allowlist import walk_orchestration_refs
from circuitry.cli.orchestration_loader import load_orchestration_file

from . import harness

#: The two adapter names this lane registers, mapped to their own
#: `adapters/factory.py` default `binary` -- `cfg.get("binary") or
#: default`, mirrored here only to resolve which fake file a case must
#: ship; the config parsing itself stays this test's own, deliberately
#: separate from `circuitry.adapters.factory`'s own (a guard must not
#: trust the very code path it exists to keep honest).
AGENT_CLI_DEFAULT_BINARY = {"claude_code": "claude", "pi": "pi"}

#: A binary name under this prefix can never exist (issue #520's own
#: settled question 15) -- a case that configures one is deliberately
#: testing the start-failure path, never a case that forgot a fake.
MISSING_BINARY_PREFIX = "circuitry-test-missing-"

CASE_DIRS = harness.list_case_dirs()


def _case_ids() -> list[str]:
    return [d.name for d in CASE_DIRS]


def _config_adapter_cfg(case_dir: Path, metadata: dict, adapter_name: str) -> dict:
    config_name = metadata.get("config")
    if not config_name:
        return {}
    config_path = case_dir / config_name
    if not config_path.is_file():
        return {}
    data = json.loads(config_path.read_text(encoding="utf-8"))
    cfg = ((data.get("runtime") or {}).get("adapters") or {}).get(adapter_name)
    return cfg if isinstance(cfg, dict) else {}


def _configured_binary(case_dir: Path, metadata: dict, adapter_name: str) -> str | None:
    """The `binary` this case's own config would actually run, or `None`
    when it isn't a plain, truthy string (a non-string config value is
    exercised only by Rust unit tests, never a conformance case per
    issue #520's acceptance criteria -- B5's own `binary: 5` rule has no
    real binary name to check a fake against here)."""
    cfg = _config_adapter_cfg(case_dir, metadata, adapter_name)
    binary = cfg.get("binary")
    if binary is None:
        return AGENT_CLI_DEFAULT_BINARY[adapter_name]
    if isinstance(binary, str) and binary:
        return binary
    if isinstance(binary, str):
        return AGENT_CLI_DEFAULT_BINARY[adapter_name]
    return None


def _config_default_adapter(case_dir: Path, metadata: dict) -> str | None:
    config_name = metadata.get("config")
    if not config_name:
        return None
    config_path = case_dir / config_name
    if not config_path.is_file():
        return None
    data = json.loads(config_path.read_text(encoding="utf-8"))
    default_adapter = data.get("default_adapter")
    return default_adapter if isinstance(default_adapter, str) else None


def _referenced_agent_cli_adapters(case_dir: Path, metadata: dict) -> set[str]:
    orch_path = case_dir / metadata["orchestration"]
    try:
        orch = load_orchestration_file(orch_path)
    except Exception:
        # A case whose own document is deliberately malformed (a
        # duplicate-key/parse-error case) never reaches far enough to
        # dispatch any adapter at all -- nothing to check here.
        return set()
    adapter_refs, _tool_refs = walk_orchestration_refs(orch)
    names = {name for name in adapter_refs if name in AGENT_CLI_DEFAULT_BINARY}
    default_adapter = _config_default_adapter(case_dir, metadata)
    if default_adapter in AGENT_CLI_DEFAULT_BINARY:
        names.add(default_adapter)
    return names


@pytest.mark.parametrize("case_dir", CASE_DIRS, ids=_case_ids())
def test_agent_cli_reference_has_a_fake_or_a_deliberately_missing_binary(
    case_dir: Path,
) -> None:
    metadata = harness.load_case(case_dir)
    for adapter_name in _referenced_agent_cli_adapters(case_dir, metadata):
        binary = _configured_binary(case_dir, metadata, adapter_name)
        if binary is None:
            continue
        if binary.startswith(MISSING_BINARY_PREFIX):
            continue
        fake_path = case_dir / "fakes" / binary
        assert fake_path.is_file(), (
            f"{case_dir.name}: references adapter '{adapter_name}' "
            f"(binary {binary!r}) without a fake at fakes/{binary}, and "
            f"{binary!r} doesn't start with {MISSING_BINARY_PREFIX!r} -- "
            "a real claude/pi CLI could run on a machine that has one"
        )
