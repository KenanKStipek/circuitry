"""JSON loading for orchestration documents: ``json.loads`` minus silent duplicates.

``json.loads`` keeps the last of two identical keys in one object and says
nothing, so a duplicated ``"effects"`` or a duplicated key nested anywhere in
the document quietly drops the first. Every ``.json`` orchestration the
runtime loads — a file, a library ref, a ``use`` child loaded by ``path:`` —
goes through :func:`load_json` instead (see ``cli/orchestration_loader.py``),
which rejects the object and names the key and its path, mirroring
``core/yaml_load.py`` for ``.yml``/``.yaml`` documents.
"""

from __future__ import annotations

import json
from typing import Any

__all__ = ["DuplicateKeyError", "load_json"]


class DuplicateKeyError(ValueError):
    """A JSON object in the document defines the same key twice."""


class _RawObject:
    """One JSON object's keys/values, not yet checked for duplicates.

    ``object_pairs_hook`` sees one object at a time, innermost first, with
    no path context of its own — materializing bottom-up into this marker
    (rather than a plain ``dict``/``list``, either of which a real JSON
    array could also produce) lets :func:`_materialize` walk the whole tree
    top-down afterward and build each key's path as it descends.
    """

    __slots__ = ("pairs",)

    def __init__(self, pairs: list[tuple[str, Any]]) -> None:
        self.pairs = pairs


def _materialize(node: Any, path: str) -> Any:
    if isinstance(node, _RawObject):
        location = path or "top level"
        result: dict[str, Any] = {}
        for key, value in node.pairs:
            if key in result:
                raise DuplicateKeyError(
                    f"duplicate key {key!r} in {location}; "
                    "JSON would silently keep only the last one"
                )
            result[key] = _materialize(value, f"{path}.{key}" if path else key)
        return result
    if isinstance(node, list):
        return [_materialize(item, f"{path}[{i}]") for i, item in enumerate(node)]
    return node


def load_json(text: str) -> Any:
    """``json.loads`` that raises :class:`DuplicateKeyError` on a repeated key."""
    data = json.loads(text, object_pairs_hook=_RawObject)
    return _materialize(data, "")
