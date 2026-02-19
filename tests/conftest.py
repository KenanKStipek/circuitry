from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
SRC = ROOT / "src"
if str(SRC) not in sys.path:
    sys.path.insert(0, str(SRC))

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
