"""The shared `interface.inputs` contract: defaults, `required`/`type` enforcement.

Applied everywhere a document's own inputs are checked against its declared
`interface.inputs` — `use:` children (`core/use.py`) and every top-level run
entry (`cli/runtime_shim.py`, reached by `cof run`, the SDK, REST, MCP, and
the scheduler) — so a document behaves the same way wherever it runs.
"""

from __future__ import annotations

import json
from copy import deepcopy
from typing import Any

_TYPE_NAMES = ("string", "number", "boolean", "array", "object")


def _matches_type(value: Any, declared_type: str) -> bool:
    if declared_type == "string":
        return isinstance(value, str)
    if declared_type == "number":
        return isinstance(value, (int, float)) and not isinstance(value, bool)
    if declared_type == "boolean":
        return isinstance(value, bool)
    if declared_type == "array":
        return isinstance(value, list)
    if declared_type == "object":
        return isinstance(value, dict)
    return True  # an unrecognized declared type: nothing to enforce


def _coerce(raw: str, declared_type: str) -> Any:
    """Convert a CLI `-e` / Mustache-rendered string to its declared type.

    Only reached for a value that is already a `str` but whose declared type
    says otherwise — `use:` child inputs cross as rendered text even when the
    source was numeric/boolean/structured, and so does every CLI `-e`
    KEY=VALUE pair. Raises `ValueError` (`json.JSONDecodeError` is one) on a
    value that cannot be read as that type.
    """
    if declared_type == "number":
        try:
            return int(raw)
        except ValueError:
            return float(raw)
    if declared_type == "boolean":
        lowered = raw.strip().lower()
        if lowered in ("true", "1"):
            return True
        if lowered in ("false", "0"):
            return False
        raise ValueError(f"{raw!r} is not a boolean")
    if declared_type in ("array", "object"):
        return json.loads(raw)
    return raw


def check_interface_inputs(
    interface: dict[str, Any] | None,
    inputs: dict[str, Any],
    *,
    label: str,
) -> dict[str, Any]:
    """Apply declared `default:`s, then enforce `required`/`type`.

    Mutates and returns `inputs`: a key missing from `inputs` gets its
    `default:` filled in; a present string value whose declared `type` is
    not `string` is coerced (CLI `-e` values and `use:` inputs both cross as
    text). Keys `interface.inputs` doesn't mention are left alone —
    undeclared extra inputs stay allowed. Raises `ValueError`, prefixed with
    `label`, on a missing required input or a value that doesn't match (and
    can't be coerced to) its declared type.
    """
    if not isinstance(interface, dict):
        return inputs
    iface_inputs = interface.get("inputs")
    if not isinstance(iface_inputs, dict):
        return inputs

    for key, spec in iface_inputs.items():
        if not isinstance(spec, dict):
            continue
        if key not in inputs:
            if "default" in spec:
                inputs[key] = deepcopy(spec["default"])
            elif spec.get("required"):
                raise ValueError(
                    f"{label}missing required input '{key}' declared in "
                    "orchestration interface."
                )
            continue

        declared_type = spec.get("type")
        if not isinstance(declared_type, str) or declared_type not in _TYPE_NAMES:
            continue
        value = inputs[key]
        if _matches_type(value, declared_type):
            continue
        if isinstance(value, str):
            try:
                coerced = _coerce(value, declared_type)
            except (ValueError, json.JSONDecodeError) as exc:
                raise ValueError(
                    f"{label}input '{key}' declared type '{declared_type}' "
                    f"but {value!r} could not be converted: {exc}"
                ) from exc
            if not _matches_type(coerced, declared_type):
                raise ValueError(
                    f"{label}input '{key}' declared type '{declared_type}' "
                    f"but got {type(coerced).__name__}."
                )
            inputs[key] = coerced
            continue
        raise ValueError(
            f"{label}input '{key}' declared type '{declared_type}' but got "
            f"{type(value).__name__}."
        )
    return inputs
