"""``runtime.state.parsed_stdout``: whether a parsed step keeps its raw stdout in state (#592).

With ``parse: json``, a subprocess-family step (``shell``, ``git``, ``gh`` and
the rest of that family) records the parsed ``value`` and the raw text on
``meta.stdout``, so every step that prints JSON is saved twice.

* ``full`` (the default): ``meta.stdout`` keeps the raw text.
* ``drop``: a step whose stdout parsed records ``meta.stdout: null``, plus
  ``meta.stdout_chars`` (its length in Unicode code points) and
  ``meta.stdout_sha256`` (lowercase hex SHA-256 of its UTF-8 bytes).

A failed parse already records no stdout, so ``drop`` leaves failures
unchanged, and a step without ``parse: json`` is never touched.
"""

from __future__ import annotations

import hashlib
from collections.abc import Mapping
from typing import Any

SETTING_PATH = "runtime.state.parsed_stdout"

_MODES = ("full", "drop")


def _parse(value: Any) -> tuple[bool, list[str]]:
    """``(drop, errors)`` for a ``runtime.state.parsed_stdout`` value; absent is ``full``."""
    if value is None or value == "full":
        return False, []
    if value == "drop":
        return True, []
    return False, [f"{SETTING_PATH} must be 'full' or 'drop', got {value!r}."]


def parsed_stdout_errors(runtime_config: Mapping[str, Any] | None) -> list[str]:
    """The configuration errors of ``runtime.state.parsed_stdout`` in *runtime_config*."""
    state_cfg = (runtime_config or {}).get("state")
    if not isinstance(state_cfg, Mapping):
        return []
    return _parse(state_cfg.get("parsed_stdout"))[1]


def drops_parsed_stdout(runtime_config: Mapping[str, Any] | None) -> bool:
    """True when the run says ``parsed_stdout: drop``; raises ``ValueError`` when invalid.

    Config resolution and ``cof check`` reject a bad value long before a run
    gets here, as they do for ``runtime.state.prompt_sent``.
    """
    state_cfg = (runtime_config or {}).get("state")
    if not isinstance(state_cfg, Mapping):
        return False
    drop, errors = _parse(state_cfg.get("parsed_stdout"))
    if errors:
        raise ValueError(errors[0])
    return drop


def dropped_stdout_fields(stdout: str) -> dict[str, Any]:
    """``meta.stdout`` (``None``), ``meta.stdout_chars`` and ``meta.stdout_sha256`` for *stdout*."""
    return {
        "stdout": None,
        "stdout_chars": len(stdout),
        "stdout_sha256": hashlib.sha256(stdout.encode("utf-8", "replace")).hexdigest(),
    }
