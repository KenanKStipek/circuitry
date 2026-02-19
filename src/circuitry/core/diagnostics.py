from __future__ import annotations

from typing import Any, Mapping


def find_divergence_paths(
    state: Mapping[str, Any], *, root_path: str | None = "prime"
) -> list[dict[str, Any]]:
    """Return deterministic failure records from state metadata.

    A divergence record exists when a node has `meta.error` set to a non-empty string.
    """
    records: list[dict[str, Any]] = []

    if root_path is None:
        for key, value in state.items():
            if isinstance(value, Mapping):
                _walk_records(value, path=str(key), records=records)
    else:
        node = _resolve_path(state=state, path=root_path)
        if isinstance(node, Mapping):
            _walk_records(node, path=root_path, records=records)

    records.sort(key=lambda item: str(item["path"]))
    return records


def _walk_records(
    node: Mapping[str, Any], *, path: str, records: list[dict[str, Any]]
) -> None:
    meta = node.get("meta")
    if isinstance(meta, Mapping):
        error = meta.get("error")
        if isinstance(error, str) and error.strip():
            records.append(
                {
                    "path": path,
                    "error": error,
                    "created_at": meta.get("created_at"),
                    "completed_at": meta.get("completed_at"),
                }
            )

    for key, value in node.items():
        if key in ("meta", "value"):
            continue
        if isinstance(value, Mapping):
            _walk_records(value, path=f"{path}.{key}", records=records)


def _resolve_path(state: Mapping[str, Any], path: str) -> Any:
    current: Any = state
    for segment in path.split("."):
        if not segment:
            continue
        if isinstance(current, Mapping):
            current = current.get(segment)
        else:
            return None
    return current
