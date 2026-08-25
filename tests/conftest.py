from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any

import pytest

ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "src"
TESTS = ROOT / "tests"
if str(SRC) not in sys.path:
    sys.path.insert(0, str(SRC))
if str(TESTS) not in sys.path:
    sys.path.insert(0, str(TESTS))

_CURRENT_TEST_ID = ""
_ORIG_STORE_ENSURE_DICT: Any = None
_ORIG_STORE_SET: Any = None


def pytest_addoption(parser: Any) -> None:
    parser.addoption(
        "--trace-state",
        action="store_true",
        default=False,
        help="Print Store state mutations during tests.",
    )


def pytest_runtest_setup(item: Any) -> None:
    global _CURRENT_TEST_ID
    _CURRENT_TEST_ID = item.nodeid


def pytest_configure(config: Any) -> None:
    config.addinivalue_line(
        "markers",
        "integration: marks tests that require local runtime dependencies (opt-in)",
    )
    config.addinivalue_line(
        "markers",
        "real_config_discovery: opt out of the autouse hermetic-config fixture "
        "(see _hermetic_global_config below) so a test can exercise genuine "
        "config discovery tiers, constructing its own layering explicitly",
    )

    if not config.getoption("--trace-state"):
        return

    from circuitry.core.store.store import Store

    global _ORIG_STORE_ENSURE_DICT
    global _ORIG_STORE_SET
    _ORIG_STORE_ENSURE_DICT = Store.ensure_dict
    _ORIG_STORE_SET = Store.set

    def traced_ensure_dict(self: Any, path: str) -> dict[str, Any]:
        result = _ORIG_STORE_ENSURE_DICT(self, path)
        _emit_state_trace("ensure_dict", path, self.state)
        return result

    def traced_set(self: Any, path: str, value: Any) -> None:
        _ORIG_STORE_SET(self, path, value)
        _emit_state_trace("set", path, self.state)

    Store.ensure_dict = traced_ensure_dict
    Store.set = traced_set


def pytest_unconfigure(config: Any) -> None:
    if not config.getoption("--trace-state"):
        return

    from circuitry.core.store.store import Store

    if _ORIG_STORE_ENSURE_DICT is not None:
        Store.ensure_dict = _ORIG_STORE_ENSURE_DICT
    if _ORIG_STORE_SET is not None:
        Store.set = _ORIG_STORE_SET


def _emit_state_trace(action: str, path: str, state: dict[str, Any]) -> None:
    payload = json.dumps(state, indent=2, sort_keys=True, default=str)
    print(
        f"[trace-state] test={_CURRENT_TEST_ID} action={action} path={path}\n{payload}",
        flush=True,
    )


@pytest.fixture(autouse=True)
def _hermetic_global_config(
    request: Any, tmp_path: Path, monkeypatch: Any
) -> None:
    """Isolate every test from the developer's real ``~/.config/circuitry/``.

    ``circuitry.cli.config`` binds ``GLOBAL_CONFIG_DIR``/``GLOBAL_CONFIG_PATH``
    at import time from ``Path.home()``, and several modules (``last_run``,
    ``app``, ``setup``) each bind their own copy of those paths on import too.
    Without this fixture, a real global ``config.json`` on the machine running
    the suite (e.g. a restrictive ``enabled_adapters`` allowlist) silently
    changes test behavior — tests were green in CI only because CI runners
    happen to have no global config. See issue #156.

    Tests that intentionally exercise config discovery tiers opt out with
    ``@pytest.mark.real_config_discovery`` and construct their own layering
    explicitly (patching ``GLOBAL_CONFIG_PATH`` to a controlled location, as
    ``tests/cli/test_config_resolution.py`` already does).
    """
    if request.node.get_closest_marker("real_config_discovery"):
        return

    from circuitry.cli import app as app_module
    from circuitry.cli import config as config_module
    from circuitry.cli import last_run as last_run_module
    from circuitry.cli import setup as setup_module

    fake_dir = tmp_path / "hermetic-global-config"
    fake_config_path = fake_dir / "config.json"
    fake_last_run_path = fake_dir / "last-run.json"

    monkeypatch.setattr(config_module, "GLOBAL_CONFIG_DIR", fake_dir)
    monkeypatch.setattr(config_module, "GLOBAL_CONFIG_PATH", fake_config_path)
    monkeypatch.setattr(last_run_module, "LAST_RUN_PATH", fake_last_run_path)
    monkeypatch.setattr(app_module, "GLOBAL_CONFIG_DIR", fake_dir)
    monkeypatch.setattr(app_module, "_LAST_RUN_PATH", fake_last_run_path)
    monkeypatch.setattr(setup_module, "GLOBAL_CONFIG_DIR", fake_dir)
    monkeypatch.setattr(setup_module, "GLOBAL_CONFIG_PATH", fake_config_path)
