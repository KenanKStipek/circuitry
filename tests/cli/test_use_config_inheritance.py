"""Repro for issue #199: does a `use:` child see the parent's resolved config?

wheeld injects a runtime-only MCP server into the *user-level*
``~/.config/circuitry/config.json`` (never the project file, which must stay
credential-free). The hypothesis was that a composed child re-resolves its
own config from disk/cwd rather than reusing the parent run's already-merged
``CircuitryConfig``, so the user-level server would be invisible and every
composed MCP call would fail with "Unknown MCP server" — silently swallowed
by the child effect's own ``on_error: continue`` and indistinguishable from
a legitimate "no".

No network involved: the injected server points at an address nothing
listens on (``http://127.0.0.1:1``), which lets a real connection attempt
(any exception *other than* "Unknown MCP server: ...") prove the server was
found and resolution proceeded past config lookup.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from circuitry.cli import config as config_module
from circuitry.cli.runtime_shim import RunRequest, run


def _write(path: Path, content: str) -> Path:
    path.write_text(content, encoding="utf-8")
    return path


@pytest.fixture
def _repro_project(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """Project config with an empty MCP server map; global config injects one.

    Mirrors wheeld's split: project-level ``circuitry.config.json`` must not
    carry credentials, so the real server only exists in the user-level file.
    """
    global_dir = tmp_path / "home" / ".config" / "circuitry"
    global_dir.mkdir(parents=True)
    global_config_path = global_dir / "config.json"
    _write(
        global_config_path,
        json.dumps(
            {
                "runtime": {
                    "plugins": {
                        "mcp": {
                            "servers": {
                                "fake": {"url": "http://127.0.0.1:1/mcp"},
                            }
                        }
                    }
                }
            }
        ),
    )
    monkeypatch.setattr(config_module, "GLOBAL_CONFIG_PATH", global_config_path)
    monkeypatch.setattr(config_module, "GLOBAL_CONFIG_DIR", global_dir)

    project_dir = tmp_path / "project"
    project_dir.mkdir()
    _write(
        project_dir / "circuitry.config.json",
        json.dumps(
            {
                "enabled_tools": ["mcp"],
                "runtime": {"plugins": {"mcp": {"servers": {}}}},
            }
        ),
    )
    monkeypatch.chdir(project_dir)

    _write(
        project_dir / "child.yml",
        """
effects:
  - type: tool
    name: fetch
    provider: mcp
    on_error: continue
    params:
      server: fake
      operation: list_tools
""".strip()
        + "\n",
    )
    _write(
        project_dir / "parent.yml",
        """
effects:
  - type: use
    name: guard
    path: child.yml
    on_error: continue
""".strip()
        + "\n",
    )
    return project_dir


def _run(orchestration_path: Path) -> dict:
    result = run(
        RunRequest(
            orchestration_path=orchestration_path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=config_module.resolve_config(),
            skip_preflight=True,
        )
    )
    assert result.ok, result.error
    return result.state


def test_standalone_child_sees_user_level_mcp_server(_repro_project: Path) -> None:
    """Baseline from the issue: `cof run child.yml` resolves the server."""
    state = _run(_repro_project / "child.yml")
    error = state["prime"]["fetch"]["meta"]["error"]
    assert error is not None, "expected a connection failure to the unreachable fake server"
    assert "Unknown MCP server" not in error


def test_composed_child_sees_user_level_mcp_server(_repro_project: Path) -> None:
    """The bug: `use:`-composed child must see the same resolved config."""
    state = _run(_repro_project / "parent.yml")
    error = state["prime"]["guard"]["fetch"]["meta"]["error"]
    assert error is not None, "expected a connection failure to the unreachable fake server"
    assert "Unknown MCP server" not in error
