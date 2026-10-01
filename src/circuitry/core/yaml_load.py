"""YAML loading for orchestration documents: ``safe_load`` minus silent duplicates.

``yaml.safe_load`` keeps the last of two identical keys in one mapping and
says nothing, so a duplicated ``template:`` or ``effects:`` quietly drops the
first. Every orchestration the runtime loads — a file, a library ref, a
``use: inline`` child — goes through :func:`load_yaml` instead, which rejects
the mapping and names both lines.

A ``<<:`` merge key is not a duplicate: keys it brings in are overridden by
the mapping's own keys, which is what YAML merge means.
"""

from __future__ import annotations

from typing import Any

import yaml  # type: ignore[import-untyped]

__all__ = ["DuplicateKeyError", "load_yaml"]

_MERGE_TAG = "tag:yaml.org,2002:merge"


class DuplicateKeyError(yaml.YAMLError):
    """A mapping in the document defines the same key twice."""


class _UniqueKeyLoader(yaml.SafeLoader):
    def construct_mapping(self, node: Any, deep: bool = False) -> Any:
        if isinstance(node, yaml.MappingNode):
            first_seen: dict[Any, str] = {}
            for key_node, _ in node.value:
                if key_node.tag == _MERGE_TAG:
                    continue
                key = self.construct_object(key_node, deep=deep)
                try:
                    hash(key)
                except TypeError:
                    continue  # SafeLoader reports unhashable keys itself
                mark = key_node.start_mark
                where = f"line {mark.line + 1}, column {mark.column + 1}"
                if key in first_seen:
                    raise DuplicateKeyError(
                        f"duplicate key {key!r} at {where} "
                        f"(first defined at {first_seen[key]}); "
                        "YAML would silently keep only the last one"
                    )
                first_seen[key] = where
        return super().construct_mapping(node, deep=deep)


def load_yaml(text: str) -> Any:
    """``yaml.safe_load`` that raises :class:`DuplicateKeyError` on a repeated key."""
    return yaml.load(text, Loader=_UniqueKeyLoader)
