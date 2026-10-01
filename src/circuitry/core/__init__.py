from typing import Any

__all__ = [
    "DynamicDefinition",
    "DynamicRuntime",
    "PromptDefinition",
    "PromptRuntime",
    "ReflectorDefinition",
    "ReflectorRuntime",
    "Store",
    "TreeExecutionError",
    "UseDefinition",
    "UseRuntime",
    "find_divergence_paths",
]

#: Which submodule each re-exported name actually lives in.
_SOURCE_MODULES: dict[str, str] = {
    "find_divergence_paths": "diagnostics",
    "DynamicDefinition": "dynamic",
    "DynamicRuntime": "dynamic",
    "TreeExecutionError": "dynamic",
    "PromptDefinition": "prompt",
    "PromptRuntime": "prompt",
    "ReflectorDefinition": "reflector",
    "ReflectorRuntime": "reflector",
    "Store": "store",
    "UseDefinition": "use",
    "UseRuntime": "use",
}


def __getattr__(name: str) -> Any:
    # These re-exports pull in `core.dynamic` (CEL eval -> celpy/lark, a full
    # grammar parser) at `circuitry.core` package-init time — paid by every
    # submodule import (`core.saved_state`, `core.json_load`, ...) since
    # Python always initializes the parent package first. Nothing in this
    # codebase uses the package-level re-export (only `from circuitry.core
    # import <submodule>`), so resolve these lazily instead.
    module_name = _SOURCE_MODULES.get(name)
    if module_name is not None:
        import importlib

        module = importlib.import_module(f".{module_name}", __name__)
        value = getattr(module, name)
        globals()[name] = value
        return value
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")
