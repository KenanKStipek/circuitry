"""Proves the autouse hermetic-config fixture in ``tests/conftest.py``.

See issue #156: without isolating the global config tier, a real
``~/.config/circuitry/config.json`` on the machine running the suite (e.g.
one with a restrictive ``enabled_adapters`` allowlist) silently changed test
behavior. These tests confirm the fixture keeps discovery pointed at an
empty, per-test temp location regardless of what a real machine's home
directory holds, and that the opt-out marker works for tests that need to
exercise genuine discovery.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest

from circuitry.cli.config import resolve_config


def test_hostile_home_does_not_leak_into_resolved_config(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    """Simulate a machine whose real home dir carries a restrictive
    ``enabled_adapters`` allowlist (the exact shape reported in #156) and
    confirm ``resolve_config()`` — what ``tests/mcp/test_runs.py`` and
    ``tests/mcp/test_server.py`` rely on via ``mcp/runs.py`` — stays
    default-open, because discovery no longer derives from ``Path.home()``
    at call time once the autouse fixture has patched it.
    """
    hostile_home = tmp_path / "hostile-home"
    hostile_config_dir = hostile_home / ".config" / "circuitry"
    hostile_config_dir.mkdir(parents=True)
    (hostile_config_dir / "config.json").write_text(
        json.dumps({"enabled_adapters": ["cyberdiner"]}), encoding="utf-8"
    )
    monkeypatch.setattr(Path, "home", lambda: hostile_home)

    cfg = resolve_config()

    assert cfg.enabled_adapters is None


def test_global_config_path_is_redirected_to_a_temp_location() -> None:
    """The fixture's own postcondition: discovery never points at the real
    ``~/.config/circuitry/config.json`` while a test is running."""
    from circuitry.cli import config as config_module

    real_global_config_path = Path.home() / ".config" / "circuitry" / "config.json"
    assert real_global_config_path != config_module.GLOBAL_CONFIG_PATH


@pytest.mark.real_config_discovery
def test_real_config_discovery_marker_skips_the_autouse_patch() -> None:
    """The opt-out marker leaves ``GLOBAL_CONFIG_PATH`` at its real,
    ``Path.home()``-derived value so a test can construct its own discovery
    layering explicitly (as ``tests/cli/test_config_resolution.py`` does by
    patching it locally per-test)."""
    from circuitry.cli import config as config_module

    real_global_config_path = Path.home() / ".config" / "circuitry" / "config.json"
    assert real_global_config_path == config_module.GLOBAL_CONFIG_PATH
