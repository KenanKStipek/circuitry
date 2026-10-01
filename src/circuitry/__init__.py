from __future__ import annotations

import logging
from typing import TYPE_CHECKING, Any

logging.getLogger("circuitry").addHandler(logging.NullHandler())

__all__ = [
    "CircuitryConfig",
    "CircuitryExecutionError",
    "RunResult",
    "inspect_divergence_paths",
    "inspect_orchestration",
    "run_orchestration",
    "run_shared_orchestration",
    "validate_orchestration",
]

if TYPE_CHECKING:
    # Re-imported only for static types; `__getattr__` below resolves these
    # names lazily at runtime so `import circuitry` doesn't pull in `.api`'s
    # whole runtime stack.
    from .api import (
        CircuitryConfig,
        CircuitryExecutionError,
        RunResult,
        inspect_divergence_paths,
        inspect_orchestration,
        run_orchestration,
        run_shared_orchestration,
        validate_orchestration,
    )


def __getattr__(name: str) -> Any:
    # `.api` pulls in the whole runtime stack (core.compiler -> cel_eval's
    # CEL grammar parser, every adapter, jsonschema, ...) — ~250ms. A plain
    # `import circuitry` (e.g. any `circuitry.cli.*` submodule import, since
    # Python always initializes the parent package first) shouldn't pay for
    # that just to reach `cof --help`/`cof version`; resolve it lazily, on
    # first access to one of these public names instead.
    if name in __all__:
        from . import api

        value = getattr(api, name)
        globals()[name] = value
        return value
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")
