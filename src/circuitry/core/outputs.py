"""One canonical shape for output declarations, in both places they appear.

``use.outputs`` and ``interface.outputs`` say the same thing — "expose this
child state path under this name" — and used to say it in two different
syntaxes. The canonical spelling is now the object form in both::

    outputs:
      summary: {path: prime.summarize.value, type: string}

The bare-string shorthand is sugar for the same thing and stays accepted
everywhere, so neither spelling is an error::

    outputs:
      summary: prime.summarize.value

Normalizing here means the runtime only ever handles ``name -> path``.
``type`` and ``description`` are declaration metadata for humans and for
``use`` wiring; they carry no runtime behaviour.
"""

from __future__ import annotations

from typing import Any, Mapping

__all__ = ["normalize_outputs"]


def normalize_outputs(raw: Any, *, context: str) -> dict[str, str]:
    """Reduce an outputs mapping to ``name -> dot-path``.

    Accepts either canonical form (``{path: ..., type?: ..., description?: ...}``)
    or the bare-string shorthand, per key — a document may mix them.

    *context* prefixes error messages so the author knows which block is at
    fault (e.g. ``"Use effect 'critique'"`` or ``"interface.outputs"``).
    Returns an empty dict when *raw* is empty or absent.
    """
    if raw is None:
        return {}
    if not isinstance(raw, Mapping):
        raise ValueError(
            f"{context}: outputs must be a mapping of name -> "
            "{path: <state path>}, not " + type(raw).__name__ + "."
        )

    normalized: dict[str, str] = {}
    for key, spec in raw.items():
        normalized[str(key)] = _normalize_one(spec, context=context, key=str(key))
    return normalized


def _normalize_one(spec: Any, *, context: str, key: str) -> str:
    if isinstance(spec, str):
        path = spec.strip()
        if not path:
            raise ValueError(
                f"{context}: output '{key}' is an empty string. "
                f"Give it a state path — {key}: {{path: prime.<effect>.value}}."
            )
        return path

    if isinstance(spec, Mapping):
        declared = spec.get("path")
        if not isinstance(declared, str) or not declared.strip():
            raise ValueError(
                f"{context}: output '{key}' has no 'path'. "
                f"Canonical form is {key}: {{path: prime.<effect>.value}} "
                f"(the bare string {key}: prime.<effect>.value is also accepted)."
            )
        return declared.strip()

    raise ValueError(
        f"{context}: output '{key}' must be an object with a 'path' key "
        f"(canonical) or a bare state-path string (shorthand); "
        f"got {type(spec).__name__}."
    )
