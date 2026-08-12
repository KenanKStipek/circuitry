"""Sandboxed Python evaluation tool plugin via RestrictedPython.

Optional dep: ``RestrictedPython``. Install with
``pip install circuitry-cof[python_eval]``.

Runs a small Python expression or statement block under
RestrictedPython's compile_restricted, with a curated builtins map and
no access to file/network/process APIs.

Params:
  - ``code`` (required): Python source to execute.
  - ``inputs`` (optional, dict): variables made available to the code.
    Names must be valid identifiers and not start with ``_``.
  - ``mode`` (optional, default ``"eval"``):
    * ``"eval"`` — evaluates a single expression; ``value`` = its result.
    * ``"exec"`` — executes a statement block; ``value`` = the
      ``result`` variable from the local namespace, or None if absent.

AC C.5: payloads outside the sandbox (``import os``, ``__import__``,
attribute access starting with ``_``) are rejected at compile time
before any side effect.
"""

from __future__ import annotations

import importlib.util
import keyword
from dataclasses import dataclass
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult

# Tiny safe-builtins set — math + string handling only.
_SAFE_BUILTINS = {
    "abs": abs, "all": all, "any": any, "bool": bool, "dict": dict,
    "divmod": divmod, "enumerate": enumerate, "filter": filter,
    "float": float, "frozenset": frozenset, "hash": hash, "hex": hex,
    "int": int, "isinstance": isinstance, "issubclass": issubclass,
    "iter": iter, "len": len, "list": list, "map": map, "max": max,
    "min": min, "next": next, "oct": oct, "ord": ord, "pow": pow,
    "range": range, "repr": repr, "reversed": reversed, "round": round,
    "set": set, "slice": slice, "sorted": sorted, "str": str, "sum": sum,
    "tuple": tuple, "zip": zip,
    "True": True, "False": False, "None": None,
}


def _validate_input_names(inputs: dict[str, Any]) -> None:
    for k in inputs:
        if not isinstance(k, str):
            raise ValueError(
                f"python_eval: input keys must be strings, got {type(k).__name__}"
            )
        if not k.isidentifier() or keyword.iskeyword(k):
            raise ValueError(
                f"python_eval: input name {k!r} is not a valid identifier."
            )
        if k.startswith("_"):
            raise ValueError(
                f"python_eval: input name {k!r} cannot start with underscore."
            )


@dataclass(frozen=True)
class PythonEvalPlugin:
    name: str = "python_eval"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        del timeout_seconds
        # Validate params first so callers get a clean ValueError /
        # PermissionError even when RestrictedPython isn't installed.
        code = params.get("code")
        if not isinstance(code, str) or not code.strip():
            raise ValueError("python_eval requires params['code'].")
        mode = str(params.get("mode") or "eval").lower()
        if mode not in ("eval", "exec"):
            raise ValueError(f"python_eval: mode must be eval|exec, got {mode!r}")
        inputs = params.get("inputs") or {}
        if not isinstance(inputs, dict):
            raise ValueError("python_eval: params['inputs'] must be a dict.")
        _validate_input_names(inputs)

        try:
            from RestrictedPython import (  # type: ignore[import-not-found]
                compile_restricted,
                safe_globals,
            )
            from RestrictedPython.Eval import (
                default_guarded_getitem,  # type: ignore[import-not-found]
            )
            from RestrictedPython.Guards import (  # type: ignore[import-not-found]
                guarded_iter_unpack_sequence,
                guarded_unpack_sequence,
                safer_getattr,
            )
        except ImportError as exc:
            raise RuntimeError(
                "python_eval: RestrictedPython not installed. "
                "Install with: pip install RestrictedPython"
            ) from exc

        # Build the evaluation environment. ``safe_globals`` contains
        # RestrictedPython's runtime helpers; we extend it with our
        # curated builtins.
        env_globals = dict(safe_globals)
        env_globals["__builtins__"] = dict(_SAFE_BUILTINS)
        env_globals["_getitem_"] = default_guarded_getitem
        env_globals["_getattr_"] = safer_getattr
        env_globals["_getiter_"] = iter
        env_globals["_iter_unpack_sequence_"] = guarded_iter_unpack_sequence
        env_globals["_unpack_sequence_"] = guarded_unpack_sequence
        env_locals = dict(inputs)

        try:
            compiled = compile_restricted(
                code, filename="<python_eval>", mode=mode
            )
        except SyntaxError as exc:
            # RestrictedPython raises SyntaxError for sandbox violations
            # (forbidden imports, dunder access, etc).
            raise PermissionError(
                f"python_eval: rejected by sandbox: {exc}"
            ) from exc

        if mode == "eval":
            result = eval(compiled, env_globals, env_locals)
            value: Any = result
        else:
            exec(compiled, env_globals, env_locals)
            value = env_locals.get("result")

        return ToolResult(
            value=value,
            raw={"mode": mode},
            stdout=None, stderr=None, exit_code=None,
        )

    def check(self) -> CheckResult:
        if importlib.util.find_spec("RestrictedPython") is None:
            return CheckResult(
                ok=False,
                missing=["library:RestrictedPython"],
                message="pip install RestrictedPython",
            )
        return CheckResult(ok=True, missing=[])
