"""Guards against a case accidentally starting a real `claude`/`pi` CLI
(issue #520's own hazard: "A real CLI on `PATH`"). A case that names
`claude_code`/`pi` -- through a document `adapter:`, a prompt's own
`provider:`/`provider_fallbacks`, a `use` child (statically reachable,
``path:``/``ref:``/``inline:``), a config `default_adapter`, a `cli_args`
`--adapter` override, or a profile's per-effect `provider` override --
must ship its own fake under `fakes/<binary>`, unless its own configured
`binary` is a deliberately-missing `circuitry-test-missing-*` name: on a
machine with a logged-in Claude Code or pi, a case or test without either
would start the real CLI and spend a real request.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from circuitry.cli.allowlist import _provider_token_to_adapter, walk_orchestration_refs
from circuitry.cli.orchestration_loader import load_orchestration_file
from circuitry.core.cycle_check import iter_use_children

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
    when it's a *truthy* non-string (a non-string config value is
    exercised only by Rust unit tests, never a conformance case per
    issue #520's acceptance criteria -- B5's own `binary: 5` rule raises
    `TypeError` before anything is ever looked up on `PATH`, so there is
    no real binary name to check a fake against here).

    Any *falsy* value -- `None`, `""`, `False`, `0`, an empty list --
    is Python's own `cfg.get("binary") or default`: it substitutes the
    default name exactly as a missing key would, which WOULD start a
    real CLI without a fake. Treating a falsy non-string as "nothing to
    check" (the bug this guard exists to fix) would silently allow that.
    """
    cfg = _config_adapter_cfg(case_dir, metadata, adapter_name)
    binary = cfg.get("binary")
    if isinstance(binary, str) and binary:
        return binary
    if binary:
        return None
    return AGENT_CLI_DEFAULT_BINARY[adapter_name]


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


def _cli_args_adapter_override(metadata: dict) -> str | None:
    """`cof run ... --adapter <name>`'s own flag (`cli/app.py`'s own
    `--adapter` option) in a case's own `cli_args` -- a bare adapter
    name, never an `adapter:model` token, so no `_provider_token_to_
    adapter` parsing is needed."""
    cli_args = metadata.get("cli_args") or []
    for index, arg in enumerate(cli_args):
        if arg == "--adapter" and index + 1 < len(cli_args):
            value = cli_args[index + 1]
            return value if isinstance(value, str) else None
    return None


def _profile_adapter_overrides(case_dir: Path, metadata: dict) -> set[str]:
    """Adapter names named by a profile's own per-effect `provider`
    override (`cli/allowlist.py::profile_provider_denials`'s own
    `effects[path]["provider"]`) -- a profile swaps an effect's
    adapter after the document itself was already walked."""
    profile_rel = metadata.get("profile")
    if not profile_rel:
        return set()
    profile_path = case_dir / profile_rel
    if not profile_path.is_file():
        return set()
    data = load_orchestration_file(profile_path)
    effects = data.get("effects")
    if not isinstance(effects, dict):
        return set()
    names: set[str] = set()
    for override in effects.values():
        if not isinstance(override, dict):
            continue
        token = override.get("provider")
        name = _provider_token_to_adapter(token) if isinstance(token, str) else None
        if name in AGENT_CLI_DEFAULT_BINARY:
            names.add(name)
    return names


def _use_child_adapter_refs(case_dir: Path, orch_path: Path, orch: dict) -> set[str]:
    """Adapter names referenced by any `use` child statically reachable
    from *orch* (`path:`/`ref:`/`inline:`, `core.cycle_check.iter_use_
    children`'s own walk) -- a child's own top-level `adapter:` is dead
    text at run time (`walk_orchestration_refs`'s own
    `include_document_adapter=False` for exactly this reason), but its
    prompt `provider:`/`provider_fallbacks` tokens are real."""
    names: set[str] = set()
    for _label, child in iter_use_children(orch, root_path=orch_path, runtime=None):
        child_adapters, _child_tools = walk_orchestration_refs(
            child, include_document_adapter=False
        )
        names |= {name for name in child_adapters if name in AGENT_CLI_DEFAULT_BINARY}
    return names


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
    names |= _use_child_adapter_refs(case_dir, orch_path, orch)
    default_adapter = _config_default_adapter(case_dir, metadata)
    if default_adapter in AGENT_CLI_DEFAULT_BINARY:
        names.add(default_adapter)
    cli_adapter = _cli_args_adapter_override(metadata)
    if cli_adapter in AGENT_CLI_DEFAULT_BINARY:
        names.add(cli_adapter)
    names |= _profile_adapter_overrides(case_dir, metadata)
    return names


def _resolve_configured_binary(case_dir: Path, binary: str) -> Path:
    """Where *binary* would actually run from, the same way a real
    `Popen`/`which` would resolve it: absolute as given, otherwise
    relative to the case directory (`cof run`'s own cwd for a case,
    per `tests/conformance/README.md`'s "How a case runs")."""
    candidate = Path(binary)
    return candidate if candidate.is_absolute() else case_dir / candidate


@pytest.mark.parametrize("case_dir", CASE_DIRS, ids=_case_ids())
def test_agent_cli_reference_has_a_fake_or_a_deliberately_missing_binary(
    case_dir: Path,
) -> None:
    metadata = harness.load_case(case_dir)
    for adapter_name in _referenced_agent_cli_adapters(case_dir, metadata):
        binary = _configured_binary(case_dir, metadata, adapter_name)
        if binary is None:
            continue
        if Path(binary).name.startswith(MISSING_BINARY_PREFIX):
            continue
        if "/" in binary:
            fakes_dir = (case_dir / "fakes").resolve()
            resolved = _resolve_configured_binary(case_dir, binary).resolve()
            assert resolved.parent == fakes_dir, (
                f"{case_dir.name}: references adapter '{adapter_name}' with a "
                f"path-separated binary {binary!r} outside its own fakes/ "
                f"directory and not under {MISSING_BINARY_PREFIX!r} -- a real "
                "claude/pi CLI at that path could run on a machine that has one"
            )
            fake_path = resolved
        else:
            fake_path = case_dir / "fakes" / binary
        assert fake_path.is_file(), (
            f"{case_dir.name}: references adapter '{adapter_name}' "
            f"(binary {binary!r}) without a fake at fakes/{binary}, and "
            f"{binary!r} doesn't start with {MISSING_BINARY_PREFIX!r} -- "
            "a real claude/pi CLI could run on a machine that has one"
        )
