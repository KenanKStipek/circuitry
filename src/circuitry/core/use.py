"""Use effect — runs another orchestration as an isolated sub-step."""

from __future__ import annotations

import logging
import time
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable, Literal

from ..adapters import Adapter
from ..output import console as _console
from .store import Store

logger = logging.getLogger(__name__)


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


def _elapsed_str(seconds: float) -> str:
    if seconds >= 1:
        return f"{seconds:.2f}s"
    return f"{seconds * 1000:.0f}ms"


def _resolve_dot_path(state: dict[str, Any], dot_path: str) -> Any:
    """Walk a dot-delimited path into a nested dict, returning the value or None."""
    parts = dot_path.split(".")
    current: Any = state
    for part in parts:
        if not isinstance(current, dict):
            return None
        current = current.get(part)
    return current


def _render_inputs(inputs: dict[str, Any], ctx: dict[str, Any]) -> dict[str, Any]:
    """Render input values: Mustache-render strings, pass others through."""
    try:
        import chevron  # type: ignore
    except ImportError:
        return dict(inputs)

    rendered: dict[str, Any] = {}
    for key, value in inputs.items():
        if isinstance(value, str):
            rendered[key] = chevron.render(value, ctx)
        else:
            rendered[key] = value
    return rendered


def _validate_inline_yaml(yaml_text: str) -> tuple[bool, list[str]]:
    """Validate an inline YAML string against the orchestration schema.

    Returns (ok, errors) where errors is a list of human-readable messages.
    """
    import importlib.resources
    import json

    import jsonschema  # type: ignore[import-untyped]
    import yaml as _yaml  # type: ignore[import-untyped]

    try:
        parsed = _yaml.safe_load(yaml_text)
    except _yaml.YAMLError as e:
        return False, [f"YAML parse error: {e}"]

    if not isinstance(parsed, dict):
        return False, ["Inline orchestration must be a YAML mapping with an 'effects' key."]

    if "effects" not in parsed:
        return False, ["Inline orchestration is missing required 'effects' key."]

    try:
        schema_path = importlib.resources.files("circuitry") / "schema" / "orchestration.schema.json"
        schema = json.loads(Path(str(schema_path)).read_text(encoding="utf-8"))
        validator = jsonschema.Draft7Validator(schema)
        errors = [e.message for e in validator.iter_errors(parsed)]
        return (len(errors) == 0), errors
    except Exception as e:
        # If schema loading fails, skip validation (best-effort)
        logger.warning("Schema validation skipped: %s", e)
        return True, []


def _clean_yaml_fences(text: str) -> str:
    """Strip markdown code fences and YAML document separators from LLM output."""
    lines = []
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("```"):
            continue
        if stripped == "---":
            continue
        lines.append(line)
    return "\n".join(lines).strip()


@dataclass(frozen=True)
class UseDefinition:
    name: str
    orchestration: str | None = None
    inline: str | None = None
    inputs: dict[str, Any] | None = None
    outputs: dict[str, str] | None = None
    validate: bool = True
    on_error: Literal["fail", "skip", "continue"] = "fail"
    description: str | None = None


class UseRuntime:
    """
    Executes a UseDefinition: resolves an orchestration reference, runs it
    in an isolated state, and maps outputs back to the parent store.
    """

    def __init__(
        self,
        definition: UseDefinition,
        *,
        adapter: Adapter,
        model: str,
        runtime_config: dict[str, Any] | None = None,
        dry_run: bool = False,
        timeout_seconds: int = 120,
        verbose: bool = False,
        depth: int = 0,
        ancestors: list | None = None,
    ):
        self.defn = definition
        self.adapter = adapter
        self.model = model
        self.runtime_config = runtime_config or {}
        self.dry_run = dry_run
        self.timeout_seconds = timeout_seconds
        self.verbose = verbose
        self.depth = depth
        self._ancestors = ancestors or []

    def _resolve_orchestration(self) -> Path:
        """Resolve orchestration reference to a file path.

        Resolution order:
          1. Literal file path (absolute or relative to cwd)
          2. Relative to parent orchestration's directory (if available)
          3. Bundled name via registry
        """
        ref = self.defn.orchestration

        # 1. Direct file path
        candidate = Path(ref)
        if candidate.exists() and candidate.is_file():
            return candidate

        # 2. Relative to parent orchestration directory
        parent_dir = self.runtime_config.get("_orchestration_dir")
        if parent_dir:
            relative = Path(parent_dir) / ref
            if relative.exists() and relative.is_file():
                return relative

        # 3. Bundled name
        from ..cli.registry import resolve_bundled

        bundled = resolve_bundled(ref)
        if bundled is not None:
            return bundled

        raise ValueError(
            f"Use effect '{self.defn.name}': orchestration '{ref}' not found. "
            "Provide a valid file path or a bundled orchestration name "
            "(run `cof list` to see available names)."
        )

    def _check_interface(
        self, orch: dict[str, Any], rendered_inputs: dict[str, Any]
    ) -> dict[str, str] | None:
        """Validate inputs and auto-generate output mapping from interface declaration.

        Returns auto-generated outputs dict if interface has outputs and the use
        effect has no explicit outputs mapping, otherwise None.
        """
        interface = orch.get("interface")
        if not isinstance(interface, dict):
            return None

        # Validate required inputs
        iface_inputs = interface.get("inputs")
        if isinstance(iface_inputs, dict):
            for key, spec in iface_inputs.items():
                if not isinstance(spec, dict):
                    continue
                if spec.get("required") and key not in rendered_inputs:
                    raise ValueError(
                        f"Use effect '{self.defn.name}': missing required input '{key}' "
                        f"declared in orchestration interface."
                    )

        # Auto-generate output mapping if not explicitly provided
        if self.defn.outputs is not None:
            return None  # explicit mapping takes precedence

        iface_outputs = interface.get("outputs")
        if isinstance(iface_outputs, dict) and iface_outputs:
            auto_outputs: dict[str, str] = {}
            for key, spec in iface_outputs.items():
                if isinstance(spec, dict) and "path" in spec:
                    auto_outputs[key] = spec["path"]
            return auto_outputs if auto_outputs else None

        return None

    def _load_child_orch(self, ctx: dict[str, Any]) -> tuple[dict[str, Any], str]:
        """Load the child orchestration dict and a label for display.

        Returns (orch_dict, label).
        For file-based: loads from resolved path.
        For inline: renders Mustache template, cleans fences, parses YAML, validates.
        """
        import yaml as _yaml  # type: ignore[import-untyped]

        if self.defn.inline is not None:
            import chevron  # type: ignore

            # Render Mustache template against parent context
            raw_yaml = chevron.render(self.defn.inline, ctx)
            cleaned = _clean_yaml_fences(raw_yaml)

            # Validate against schema
            if self.defn.validate:
                ok, errors = _validate_inline_yaml(cleaned)
                if not ok:
                    raise ValueError(
                        f"Inline orchestration validation failed:\n"
                        + "\n".join(f"  - {e}" for e in errors)
                    )

            parsed = _yaml.safe_load(cleaned)
            if not isinstance(parsed, dict):
                raise ValueError("Inline orchestration must be a YAML mapping with an 'effects' key.")
            return parsed, "inline"

        # File-based resolution
        from ..cli.orchestration_loader import load_orchestration_file

        resolved_path = self._resolve_orchestration()
        return load_orchestration_file(resolved_path), str(resolved_path)

    def execute(self, *, store: Store, ctx: dict[str, Any]) -> None:
        from .compiler import compile_orchestration
        from .dynamic import DynamicRuntime

        node = store.ensure_dict(self.defn.name)
        node.setdefault("value", None)
        meta = node.get("meta")
        if not isinstance(meta, dict):
            meta = {}
            node["meta"] = meta

        label = self.defn.orchestration or "inline"
        meta["created_at"] = _now_iso()
        meta["completed_at"] = None
        meta["orchestration"] = self.defn.orchestration
        meta["inline"] = self.defn.inline is not None
        meta["resolved_path"] = None
        meta["validation_errors"] = None
        meta["error"] = None

        indent = "  " * self.depth
        t0 = time.monotonic()

        try:
            if self.dry_run:
                node["value"] = None
                meta["completed_at"] = _now_iso()
                meta["dry_run"] = True
                if self.verbose:
                    elapsed = time.monotonic() - t0
                    _console.print(
                        f"{indent}[ok]✓[/ok] [green]⊕[/green] {self.defn.name}"
                        f" [dim]{label} | {_elapsed_str(elapsed)} (dry)[/dim]"
                    )
                return

            # Load (file or inline) and compile
            child_orch, resolved_label = self._load_child_orch(ctx)
            label = resolved_label
            if not meta["inline"]:
                meta["resolved_path"] = resolved_label

            child_root = compile_orchestration(orch=child_orch, root_name="prime")

            # Build isolated child state from inputs
            child_state: dict[str, Any] = {}
            if self.defn.inputs:
                child_state = _render_inputs(self.defn.inputs, ctx)

            # Check interface: validate required inputs, auto-generate output mapping
            auto_outputs = self._check_interface(child_orch, child_state)

            child_store = Store(state=child_state)

            # Execute child orchestration
            DynamicRuntime(
                child_root,
                adapter=self.adapter,
                model=self.model,
                runtime_config=self.runtime_config,
                dry_run=self.dry_run,
                timeout_seconds=self.timeout_seconds,
                verbose=self.verbose,
                depth=self.depth + 1,
                ancestors=self._ancestors,
            ).execute(store=child_store)

            # Extract outputs (explicit > auto-generated from interface > default True)
            effective_outputs = self.defn.outputs or auto_outputs
            if effective_outputs:
                result: dict[str, Any] = {}
                for output_key, child_path in effective_outputs.items():
                    result[output_key] = _resolve_dot_path(child_store.state, child_path)
                node["value"] = result
            else:
                node["value"] = True

            meta["completed_at"] = _now_iso()

            if self.verbose:
                elapsed = time.monotonic() - t0
                _console.print(
                    f"{indent}[ok]✓[/ok] [green]⊕[/green] {self.defn.name}"
                    f" [dim]{label} | {_elapsed_str(elapsed)}[/dim]"
                )

        except Exception as e:
            error_msg = str(e)
            meta["error"] = error_msg
            meta["completed_at"] = _now_iso()

            # Capture validation errors separately for introspection
            if "validation failed" in error_msg.lower():
                meta["validation_errors"] = error_msg

            if self.verbose:
                elapsed = time.monotonic() - t0
                _console.print(
                    f"{indent}[err]✗[/err] [green]⊕[/green] {self.defn.name}"
                    f" [dim]{label} | {_elapsed_str(elapsed)}[/dim]"
                )

            if self.defn.on_error == "fail":
                raise RuntimeError(
                    f"use '{self.defn.name}' -> {label}: {e}"
                ) from e
            elif self.defn.on_error == "skip":
                node["value"] = None
            # continue: keep going with None value
