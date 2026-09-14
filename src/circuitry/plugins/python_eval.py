"""Sandboxed Python evaluation tool plugin via RestrictedPython.

Optional dep: ``RestrictedPython``. Install with
``pip install circuitry-cof[python_eval]``.

Runs a small Python expression or statement block under
RestrictedPython's compile_restricted, with a curated builtins map and
no access to file/network/process APIs.

Params:
  - ``code`` (required): Python source to execute.
  - ``inputs`` (optional, dict): variables made available to the code.
    Names must be valid identifiers and not start with ``_``. Mirrored
    into both globals and locals, so they're visible inside comprehension
    bodies too (see note below).
  - ``mode`` (optional, default ``"eval"``):
    * ``"eval"`` — evaluates a single expression; ``value`` = its result.
    * ``"exec"`` — executes a statement block; ``value`` = the
      ``result`` variable from the local namespace, or None if absent.

Subscript assignment (``x[k] = v``), attribute assignment (``x.attr = v``)
and augmented assignment of names (``n += 1``) are supported: RestrictedPython
rewrites these to calls against ``_write_``/``_inplacevar_`` guards, which
this plugin wires up. ``_write_`` (``full_write_guard``) permits item/attr
assignment on plain ``list``/``dict`` and rejects writes to other object
types; augmented assignment of subscripts/attributes (``x[k] += v``) is
rejected at compile time by RestrictedPython itself, not just Names.

Note: ``eval``/``exec`` with separate globals/locals dicts make comprehension
bodies a nested scope that only sees globals, not the enclosing locals — a
plain CPython quirk, not specific to RestrictedPython. Without mirroring
``inputs`` into globals, ``[x for x in range(int(w))]`` would raise
``NameError: name 'w' is not defined`` even though ``w`` is a top-level
input.

AC C.5: payloads outside the sandbox (``import os``, ``__import__``,
attribute access starting with ``_``) are rejected at compile time
before any side effect.
"""

from __future__ import annotations

import importlib.util
import keyword
import operator
from dataclasses import dataclass
from typing import Any, Literal

from ..preflight import CheckResult
from .base import ToolResult

# RestrictedPython rewrites `x += y` (etc.) to `x = _inplacevar_('+=', x, y)`
# for Name targets — augmented assignment of attributes/subscripts is
# rejected at compile time instead, so this only needs to cover Name
# rebinding. Keys match the operator strings RestrictedPython's transformer
# emits (`IOPERATOR_TO_STR`).
_INPLACE_OPS: dict[str, Any] = {
    "+=": operator.iadd,
    "-=": operator.isub,
    "*=": operator.imul,
    "/=": operator.itruediv,
    "//=": operator.ifloordiv,
    "%=": operator.imod,
    "**=": operator.ipow,
    "<<=": operator.ilshift,
    ">>=": operator.irshift,
    "|=": operator.ior,
    "^=": operator.ixor,
    "&=": operator.iand,
    "@=": operator.imatmul,
}


def _inplacevar(op: str, x: Any, y: Any) -> Any:
    try:
        func = _INPLACE_OPS[op]
    except KeyError:
        raise TypeError(f"python_eval: unsupported augmented assignment {op!r}") from None
    return func(x, y)


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
        mode_raw = str(params.get("mode") or "eval").lower()
        mode: Literal["eval", "exec"]
        if mode_raw == "eval":
            mode = "eval"
        elif mode_raw == "exec":
            mode = "exec"
        else:
            raise ValueError(f"python_eval: mode must be eval|exec, got {mode_raw!r}")
        inputs = params.get("inputs") or {}
        if not isinstance(inputs, dict):
            raise ValueError("python_eval: params['inputs'] must be a dict.")
        _validate_input_names(inputs)

        try:
            from RestrictedPython import (  # type: ignore[import-not-found]
                compile_restricted,
                safe_globals,
            )
            from RestrictedPython.Eval import (  # type: ignore[import-not-found]
                default_guarded_getitem,
            )
            from RestrictedPython.Guards import (  # type: ignore[import-not-found]
                full_write_guard,
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
        env_globals: dict[str, Any] = dict(safe_globals)
        env_globals["__builtins__"] = dict(_SAFE_BUILTINS)
        env_globals["_getitem_"] = default_guarded_getitem
        env_globals["_getattr_"] = safer_getattr
        env_globals["_getiter_"] = iter
        env_globals["_iter_unpack_sequence_"] = guarded_iter_unpack_sequence
        env_globals["_unpack_sequence_"] = guarded_unpack_sequence
        env_globals["_write_"] = full_write_guard
        env_globals["_inplacevar_"] = _inplacevar
        # `eval`/`exec` with separate globals/locals make comprehension
        # bodies a nested scope that only sees globals — mirror inputs into
        # both so names from `inputs` resolve the same way whether they're
        # used at the top level or inside a comprehension.
        env_globals.update(inputs)
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
            result = eval(compiled, env_globals, env_locals)  # noqa: S307
            value: Any = result
        else:
            exec(compiled, env_globals, env_locals)  # noqa: S102
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
