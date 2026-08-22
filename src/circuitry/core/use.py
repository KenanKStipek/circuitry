"""Use effect — runs another orchestration as an isolated sub-step."""

from __future__ import annotations

import hashlib
import logging
import time
from collections.abc import Callable
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Literal

from ..adapters import Adapter
from ..output import console as _console
from .outputs import normalize_outputs
from .store import Store

logger = logging.getLogger(__name__)

#: The key every compiled orchestration — parent or child — is rooted under.
_CHILD_ROOT = "prime"

#: ``Store.effect_start`` / ``Store.effect_complete``.
_EffectCallback = Callable[[str, dict[str, Any]], None]


def _relative_child_path(child_path: str) -> str | None:
    """The child effect path with the child's own root key stripped.

    ``None`` means the path *is* the child's root container, which the parent
    already represents as the ``use`` effect's own node — forwarding it would
    duplicate that node under itself.
    """
    if child_path == _CHILD_ROOT:
        return None
    prefix = f"{_CHILD_ROOT}."
    if child_path.startswith(prefix):
        return child_path[len(prefix) :]
    return child_path


def _namespaced_effect_cb(
    callback: _EffectCallback | None, node_path: str
) -> _EffectCallback | None:
    """Wrap a lifecycle callback so child effect paths nest under *node_path*.

    The child runs in its own store rooted at ``prime``; left alone its
    effects would announce themselves at top-level paths that collide with
    the parent's own. Rewriting ``prime.step`` → ``<use path>.step`` gives
    observers one coherent tree, and composes to arbitrary depth because a
    nested ``use`` rewrites against an already-rewritten parent path.
    """
    if callback is None:
        return None

    def _forward(child_path: str, payload: dict[str, Any]) -> None:
        relative = _relative_child_path(child_path)
        if relative is None:
            return
        callback(f"{node_path}.{relative}", payload)

    return _forward


def _replace_node(
    state: dict[str, Any], parts: list[str], replacement: dict[str, Any]
) -> dict[str, Any]:
    """A shallow copy of *state* with the node at *parts* swapped out.

    Only the dicts along the path are copied; everything else stays a live
    reference, which is all a snapshot consumer (JSON dump, deepcopy) needs.
    """
    head, rest = parts[0], parts[1:]
    if not rest:
        return {**state, head: replacement}
    inner = state.get(head)
    if not isinstance(inner, dict):
        return state
    return {**state, head: _replace_node(inner, rest, replacement)}


def _grafted_snapshot(
    root_state: dict[str, Any],
    node_path: str,
    node: dict[str, Any],
    child_snapshot: dict[str, Any],
) -> dict[str, Any]:
    """Parent state with the child's in-flight effects mirrored under the node.

    Observation only: the returned dict is a copy, so the child's state stays
    isolated from the parent's namespace and the output mapping still decides
    what actually lands in ``node``.
    """
    child_effects = child_snapshot.get(_CHILD_ROOT)
    if not isinstance(child_effects, dict):
        return root_state
    mirrored = {
        key: value
        for key, value in child_effects.items()
        if key not in ("value", "meta")
    }
    if not mirrored:
        return root_state
    return _replace_node(root_state, node_path.split("."), {**node, **mirrored})


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
    ref: str | None = None
    path: str | None = None
    orchestration: str | None = None
    inline: str | None = None
    inputs: dict[str, Any] | None = None
    #: name -> state path. Values may be the canonical ``{path: ...}`` object
    #: or the bare-path shorthand; both normalize through ``core.outputs``.
    outputs: dict[str, Any] | None = None
    validate: bool = True
    on_error: Literal["fail", "skip", "continue"] = "fail"
    description: str | None = None

    # False = skip execution and write a disabled node (see core.disabled).
    enabled: bool = True


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
        # Pin recorded when `ref:` resolved through a library source, mirrored
        # onto the effect's meta for per-effect introspection.
        self._pin: dict[str, Any] | None = None

    def _resolve_orchestration(self) -> Path:
        """Resolve orchestration reference to a file path.

        Field-driven resolution:
          - ref: library lookup across every configured source — bare names by
            source precedence, `source:name` exactly. Remote resolutions record
            their commit pin so the run is reproducible from cache.
          - path: filesystem path (absolute, cwd-relative, or parent-orchestration-relative)
          - orchestration (deprecated): try filesystem first, then the library — same
            chain as the legacy field semantics.
        """
        if self.defn.ref:
            resolved = self._resolve_library_ref(self.defn.ref)
            if resolved is not None:
                return resolved
            raise ValueError(
                f"Use effect '{self.defn.name}': ref '{self.defn.ref}' did not resolve "
                f"in any configured library source ({self._source_names()}). "
                "Run `cof list` to see available entries."
            )

        if self.defn.path:
            candidate = Path(self.defn.path)
            if candidate.exists() and candidate.is_file():
                return candidate

            parent_dir = self.runtime_config.get("_orchestration_dir")
            if parent_dir:
                relative = Path(parent_dir) / self.defn.path
                if relative.exists() and relative.is_file():
                    return relative

            raise ValueError(
                f"Use effect '{self.defn.name}': path '{self.defn.path}' not found "
                "(checked absolute, cwd-relative, and parent-orchestration-relative)."
            )

        # Deprecated `orchestration` field: legacy fallback chain.
        legacy = self.defn.orchestration
        if legacy:
            candidate = Path(legacy)
            if candidate.exists() and candidate.is_file():
                return candidate

            parent_dir = self.runtime_config.get("_orchestration_dir")
            if parent_dir:
                relative = Path(parent_dir) / legacy
                if relative.exists() and relative.is_file():
                    return relative

            bundled = self._resolve_library_ref(legacy)
            if bundled is not None:
                return bundled

            raise ValueError(
                f"Use effect '{self.defn.name}': orchestration '{legacy}' not found. "
                "Provide a valid file path or a bundled orchestration name "
                "(run `cof list` to see available names)."
            )

        raise ValueError(
            f"Use effect '{self.defn.name}': no reference field set "
            "(expected one of ref/path/orchestration)."
        )

    def _resolve_library_ref(self, ref: str) -> Path | None:
        """Resolve through the library registry, recording the pin on success.

        A never-fetched source raises with the `cof library refresh` command
        needed — validate/preflight normally catches that first, so reaching it
        here means the cache went away between check and run.
        """
        from .library_ref import LibraryRefError, record_pin, resolve_ref

        try:
            resolved = resolve_ref(ref, runtime=self.runtime_config)
        except LibraryRefError as exc:
            raise ValueError(f"Use effect '{self.defn.name}': {exc}") from exc

        if resolved is None:
            return None

        record_pin(self.runtime_config, resolved)
        self._pin = resolved.as_pin()
        if resolved.ambiguous_sources:
            logger.warning(
                "use '%s': ref %r matched multiple sources; using %s:%s (also in: %s)",
                self.defn.name,
                ref,
                resolved.source,
                ref,
                ", ".join(resolved.ambiguous_sources),
            )
        return resolved.path

    def _source_names(self) -> str:
        from .library_ref import build_registry

        return ", ".join(build_registry(self.runtime_config).source_names) or "none"

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

        # Same canonical shape as `use.outputs`: objects with a `path`, with
        # the bare-string shorthand accepted too (see core.outputs).
        iface_outputs = normalize_outputs(
            interface.get("outputs"),
            context=f"Use effect '{self.defn.name}': child interface.outputs",
        )
        return iface_outputs or None

    def _load_child_orch(self, ctx: dict[str, Any]) -> tuple[dict[str, Any], str, str]:
        """Load the child orchestration dict, a display label, and a cycle-identity string.

        Returns (orch_dict, label, identity).
        For file-based: identity is the absolute resolved path.
        For inline: identity is 'inline:<sha256-of-cleaned-yaml>'.
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
                        "Inline orchestration validation failed:\n"
                        + "\n".join(f"  - {e}" for e in errors)
                    )

            parsed = _yaml.safe_load(cleaned)
            if not isinstance(parsed, dict):
                raise ValueError("Inline orchestration must be a YAML mapping with an 'effects' key.")
            content_hash = hashlib.sha256(cleaned.encode("utf-8")).hexdigest()[:16]
            return parsed, "inline", f"inline:{content_hash}"

        # File-based resolution
        from ..cli.orchestration_loader import load_orchestration_file

        resolved_path = self._resolve_orchestration()
        identity = str(resolved_path.resolve())
        return load_orchestration_file(resolved_path), str(resolved_path), identity

    def _child_on_write(
        self, store: Store, node: dict[str, Any], node_path: str
    ) -> Callable[[dict[str, Any]], None] | None:
        """The child's ``on_write``: republish the parent run, child included.

        The child writes into its own state, which no parent observer can
        see — so every child write republishes the *parent's* whole-run
        snapshot with the child's effects mirrored under this use effect's
        node. The mirror is built per call and thrown away; the parent state
        dict itself is never touched, which is what keeps child state
        isolated while making it observable.

        The child's snapshot arrives as the callback's argument rather than
        being read back off the child store, so a nested ``use`` — whose own
        wrapper has already mirrored *its* child in — composes to any depth.
        """
        parent_on_write = store.on_write
        if parent_on_write is None:
            return None

        root_state = store.root_state

        def _publish(child_snapshot: dict[str, Any]) -> None:
            parent_on_write(
                _grafted_snapshot(root_state, node_path, node, child_snapshot)
            )

        return _publish

    def execute(self, *, store: Store, ctx: dict[str, Any]) -> None:
        from .compiler import compile_orchestration
        from .dynamic import DynamicRuntime

        node = store.ensure_dict(self.defn.name)
        node.setdefault("value", None)
        meta = node.get("meta")
        if not isinstance(meta, dict):
            meta = {}
            node["meta"] = meta

        legacy_ref = self.defn.ref or self.defn.path or self.defn.orchestration
        label = legacy_ref or "inline"
        meta["created_at"] = _now_iso()
        meta["completed_at"] = None
        meta["orchestration"] = legacy_ref
        meta["inline"] = self.defn.inline is not None
        meta["resolved_path"] = None
        meta["validation_errors"] = None
        meta["error"] = None

        #: This effect's canonical dotted path — what child effects namespace
        #: under and where their live state is mirrored for observers.
        node_path = store.effect_path(self.defn.name)

        indent = "  " * self.depth
        t0 = time.monotonic()
        cycle_pushed = False
        call_stack: list[str] = self.runtime_config.setdefault("_use_call_stack", [])

        # The use effect announces itself the way every other effect does, so
        # the child effects forwarded below have a node to hang under. The
        # matching complete fires from the `finally` — every exit path,
        # including a child that blew up, closes the pair.
        store.fire_effect_start(self.defn.name, node)

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
            child_orch, resolved_label, identity = self._load_child_orch(ctx)
            label = resolved_label
            if not meta["inline"]:
                meta["resolved_path"] = resolved_label
            if self._pin is not None:
                meta["library_ref"] = self._pin

            # Cycle detection — runtime call-stack tracking by resolved identity.
            if identity in call_stack:
                cycle_path = " → ".join([*call_stack, identity])
                raise RecursionError(
                    f"use '{self.defn.name}': cycle detected — {cycle_path}"
                )
            call_stack.append(identity)
            cycle_pushed = True

            child_root = compile_orchestration(orch=child_orch, root_name="prime")

            # Build isolated child state from inputs
            child_state: dict[str, Any] = {}
            if self.defn.inputs:
                child_state = _render_inputs(self.defn.inputs, ctx)

            # Check interface: validate required inputs, auto-generate output mapping
            auto_outputs = self._check_interface(child_orch, child_state)

            # Isolated state, shared observation: the child keeps its own
            # state dict (and its explicit inputs/outputs mapping) but
            # inherits the parent's callbacks, its lock — so a snapshot is
            # never composed mid-write — and a path prefix that nests its
            # effects under this one.
            child_store = Store(
                state=child_state,
                on_write=self._child_on_write(store, node, node_path),
                effect_complete=_namespaced_effect_cb(
                    store.effect_complete, node_path
                ),
                effect_start=_namespaced_effect_cb(store.effect_start, node_path),
                _lock=store._lock,
            )

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

            # Extract outputs (explicit > auto-generated from interface > full child state).
            # The compiler already normalized `outputs`, but a UseDefinition can
            # also be built directly (embedded API, tests) — normalize again so
            # both spellings work on every path in.
            explicit_outputs = normalize_outputs(
                self.defn.outputs, context=f"Use effect '{self.defn.name}'"
            )
            effective_outputs = explicit_outputs or auto_outputs
            if effective_outputs:
                result: dict[str, Any] = {}
                for output_key, child_path in effective_outputs.items():
                    result[output_key] = _resolve_dot_path(child_store.state, child_path)
                node["value"] = result
            else:
                # Full-namespace mode: expose child's prime subtree at
                # prime.<use_name>.<child_effect>.value (matches dynamic namespacing).
                child_prime = child_store.state.get("prime") or {}
                if isinstance(child_prime, dict):
                    for key, val in child_prime.items():
                        if key in ("value", "meta"):
                            continue
                        node[key] = val

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
            if self.defn.on_error == "skip":
                node["value"] = None
            # continue: keep going with None value
        finally:
            if cycle_pushed and call_stack:
                # Pop only if we pushed and the top still matches.
                try:
                    call_stack.pop()
                except IndexError:
                    pass
            store.fire_effect_complete(self.defn.name, node)
