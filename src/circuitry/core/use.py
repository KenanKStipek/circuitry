"""Use effect — runs another orchestration as an isolated sub-step."""

from __future__ import annotations

import copy
import hashlib
import logging
import time
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Literal

from ..adapters import Adapter
from ..output import console as _console
from .document_check import structural_errors
from .interface_inputs import check_interface_inputs
from .outputs import normalize_outputs
from .store import Store
from .store.store import replace_node
from .templates import render_template
from .yaml_load import load_yaml

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
    return replace_node(root_state, node_path.split("."), {**node, **mirrored})


def _collect_child_errors(node: Any, prefix: str = "") -> list[dict[str, Any]]:
    """Gather ``{path, error}`` for every effect under *node* whose meta carries an error.

    A child effect's own ``on_error: skip``/``continue`` swallows its
    exception at that effect's level, leaving the ``use`` node's own
    ``meta.error`` (and thus the parent's view of the run) untouched — the
    composed failure is otherwise only visible by walking the child's full
    state tree. Recurses into every nested container (dynamic/conditional/
    loop bodies, nested ``use`` effects) so a swallowed error at any depth
    surfaces here.
    """
    errors: list[dict[str, Any]] = []
    if not isinstance(node, dict):
        return errors
    meta = node.get("meta")
    if isinstance(meta, dict) and meta.get("error"):
        errors.append({"path": prefix or _CHILD_ROOT, "error": meta["error"]})
    for key, value in node.items():
        if key in ("value", "meta") or not isinstance(value, dict):
            continue
        child_path = f"{prefix}.{key}" if prefix else key
        errors.extend(_collect_child_errors(value, child_path))
    return errors


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


#: The one key of a by-reference input: ``name: {from: <path>}``.
REFERENCE_KEY = "from"


def reference_path(value: Any) -> str | None:
    """The path of a by-reference input (``{from: <path>}``), or None for any other value.

    Only a mapping with exactly the one key ``from`` and a string value is a
    reference; every other mapping stays a literal, as before.
    """
    if isinstance(value, dict) and len(value) == 1 and REFERENCE_KEY in value:
        path = value[REFERENCE_KEY]
        if isinstance(path, str):
            return path.strip()
    return None


def _resolve_reference(ctx: Mapping[str, Any], path: str) -> Any:
    """Walk *path* through mappings (by key) and lists (by integer index).

    Returns None when any segment is missing, like an unset template path.
    """
    current: Any = ctx
    for part in path.split("."):
        if isinstance(current, Mapping):
            if part not in current:
                return None
            current = current[part]
        elif isinstance(current, (list, tuple)):
            try:
                current = current[int(part)]
            except (ValueError, IndexError):
                return None
        else:
            return None
    return current


def _render_inputs(inputs: dict[str, Any], ctx: dict[str, Any]) -> dict[str, Any]:
    """Render input values for a child run.

    A by-reference input (``{from: <path>}``) passes the resolved value itself,
    deep-copied so the child can never alias parent state. Strings are
    Mustache-rendered; anything else passes through unchanged.
    """
    rendered: dict[str, Any] = {}
    for key, value in inputs.items():
        path = reference_path(value)
        if path is not None:
            rendered[key] = copy.deepcopy(_resolve_reference(ctx, path))
        elif isinstance(value, str):
            rendered[key] = render_template(value, ctx, label=f"inputs.{key}")
        else:
            rendered[key] = value
    return rendered


def _unresolved_references(
    inputs: dict[str, Any], rendered: dict[str, Any]
) -> dict[str, str]:
    """``{input_name: path}`` for every by-reference input that resolved to nothing."""
    unresolved: dict[str, str] = {}
    for key, value in inputs.items():
        path = reference_path(value)
        if path is not None and rendered.get(key) is None:
            unresolved[key] = path
    return unresolved


def _record_children_enabled(runtime_config: Mapping[str, Any]) -> bool:
    """True when ``runtime.state.record_children`` asks for complete child records."""
    state_cfg = runtime_config.get("state")
    return isinstance(state_cfg, Mapping) and bool(state_cfg.get("record_children"))


def _graft_child_record(node: dict[str, Any], child_state: Mapping[str, Any]) -> None:
    """Keep the child's effects under the use node, in the shape live state shows them."""
    child_prime = child_state.get(_CHILD_ROOT)
    if not isinstance(child_prime, dict):
        return
    for key, val in child_prime.items():
        if key in ("value", "meta"):
            continue
        node[key] = val


def _validate_inline_yaml(yaml_text: str) -> tuple[bool, list[str]]:
    """Validate an inline YAML string the way ``cof check`` validates a file.

    A repeated key, a near-miss unknown key, or a schema violation fails it
    (see ``core.document_check``).

    Returns (ok, errors) where errors is a list of human-readable messages.
    """
    import yaml as _yaml  # type: ignore[import-untyped]

    try:
        parsed = load_yaml(yaml_text)
    except _yaml.YAMLError as e:
        return False, [f"YAML parse error: {e}"]

    if not isinstance(parsed, dict):
        return False, ["Inline orchestration must be a YAML mapping with an 'effects' key."]

    if "effects" not in parsed:
        return False, ["Inline orchestration is missing required 'effects' key."]

    errors = structural_errors(parsed)
    return (len(errors) == 0), errors


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
        model_locked: bool = False,
        runtime_config: dict[str, Any] | None = None,
        dry_run: bool = False,
        timeout_seconds: int = 120,
        verbose: bool = False,
        depth: int = 0,
        cb_start: Callable[[], None] | None = None,
        cb_done: Callable[[str], None] | None = None,
        cb_error: Callable[[str], None] | None = None,
        display_name: str | None = None,
        ancestors: list | None = None,
    ):
        self.defn = definition
        self.adapter = adapter
        self.model = model
        # See PromptRuntime: the run default was pinned by ``--model`` or a
        # profile's run-level ``model:``, so the complexity router defers to
        # it. Containers only carry the flag down to the prompts they run.
        self.model_locked = model_locked
        self.runtime_config = runtime_config or {}
        self.dry_run = dry_run
        self.timeout_seconds = timeout_seconds
        self.verbose = verbose
        self.depth = depth
        self.cb_start = cb_start
        self.cb_done = cb_done
        self.cb_error = cb_error
        self.display_name = display_name or definition.name
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
        self,
        orch: dict[str, Any],
        rendered_inputs: dict[str, Any],
        unresolved: dict[str, str] | None = None,
    ) -> dict[str, str] | None:
        """Validate inputs and auto-generate output mapping from interface declaration.

        Returns auto-generated outputs dict if interface has outputs and the use
        effect has no explicit outputs mapping, otherwise None.
        """
        interface = orch.get("interface")
        if not isinstance(interface, dict):
            return None

        # A by-reference input (`{from: path}`) that resolved to nothing is a
        # distinct failure from a plain missing input — name the path it came
        # from. Checked first: `rendered_inputs` always carries the key in
        # this case (as `None`), so the shared required-input check below
        # never sees it as missing.
        iface_inputs = interface.get("inputs")
        if isinstance(iface_inputs, dict) and unresolved:
            for key, spec in iface_inputs.items():
                if isinstance(spec, dict) and spec.get("required") and key in unresolved:
                    raise ValueError(
                        f"Use effect '{self.defn.name}': required input '{key}' "
                        f"resolved to nothing from '{unresolved[key]}'."
                    )

        check_interface_inputs(
            interface, rendered_inputs, label=f"Use effect '{self.defn.name}': "
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

    def _load_child_orch(
        self, ctx: dict[str, Any]
    ) -> tuple[dict[str, Any], str, str, str]:
        """Load the child orchestration dict, a display label, a cycle-identity
        string, and the SHA-256 of the YAML text it came from.

        Returns (orch_dict, label, identity, sha256).
        For file-based: identity is the absolute resolved path; the digest is of the file's bytes.
        For inline: identity is 'inline:<sha256-of-cleaned-yaml>'; the digest is of that YAML.
        """
        if self.defn.inline is not None:
            # Render Mustache template against parent context
            raw_yaml = render_template(self.defn.inline, ctx, label="inline")
            cleaned = _clean_yaml_fences(raw_yaml)

            # Validate against schema
            if self.defn.validate:
                ok, errors = _validate_inline_yaml(cleaned)
                if not ok:
                    raise ValueError(
                        "Inline orchestration validation failed:\n"
                        + "\n".join(f"  - {e}" for e in errors)
                    )

            parsed = load_yaml(cleaned)
            if not isinstance(parsed, dict):
                raise ValueError("Inline orchestration must be a YAML mapping with an 'effects' key.")
            digest = hashlib.sha256(cleaned.encode("utf-8")).hexdigest()
            return parsed, "inline", f"inline:{digest[:16]}", digest

        # File-based resolution
        from ..cli.orchestration_loader import load_orchestration_file

        resolved_path = self._resolve_orchestration()
        identity = str(resolved_path.resolve())
        digest = hashlib.sha256(resolved_path.read_bytes()).hexdigest()
        child_orch = load_orchestration_file(resolved_path)
        # A path/ref child gets the same structural check `cof check` gives a
        # file named on the command line — `cof check` on the parent never
        # loads it, and nothing else would before it runs.
        if self.defn.validate:
            errors = structural_errors(child_orch)
            if errors:
                raise ValueError(
                    f"Orchestration {resolved_path} validation failed:\n"
                    + "\n".join(f"  - {e}" for e in errors)
                )
        return child_orch, str(resolved_path), identity, digest

    def _check_allowlists(self, child_orch: dict[str, Any], label: str) -> None:
        """Refuse a child that references an adapter or tool the run disallows.

        Checked as the child loads, before any of it runs — inline and
        generated children only exist from here on, so no earlier check can
        see them. The factories still gate every build as a backstop.

        A child's own top-level ``adapter:`` is not judged: the child runs on
        the adapter object its parent already built (see ``execute`` below),
        never on a build from its own ``adapter:`` field, so that field is
        dead text. Its prompt ``provider:`` tokens are real references and are
        still checked.
        """
        from ..allowlist_gate import AllowlistError, allowed_adapters, allowed_tools
        from ..cli.allowlist import orchestration_denials

        denials = orchestration_denials(
            child_orch,
            enabled_adapters=allowed_adapters(self.runtime_config),
            enabled_tools=allowed_tools(self.runtime_config),
            skip_templated=True,
            include_document_adapter=False,
        )
        if denials:
            raise AllowlistError(
                f"use '{self.defn.name}': child {label} failed allowlist "
                "enforcement: " + "; ".join(denials)
            )

    def _check_capability_consent(
        self, child_orch: dict[str, Any], label: str, digest: str
    ) -> frozenset[str] | None:
        """Capability consent for this child (#275).

        A ``ref:`` child is a library entry, independently in scope
        regardless of whether this document itself is trusted (#284) or
        went through the whole-document gate at the top of the run: it must
        already be consented to by its own digest, never interactively here
        — this runs deep in execution, possibly off the main thread, and a
        ``ref:`` value only known once a Mustache tag renders is exactly the
        case :func:`circuitry.cli.document_consent.enforce_consent`'s static
        walk up front cannot see. A missing consent always refuses, the same
        non-interactive rule every surface besides an interactive
        ``cof run``/``cof run-library`` gets (#275 rule 5). On success,
        returns the capability ceiling this child's own subtree (further
        nested ``use`` children, a generated plan) must stay inside.

        A ``path:``/``inline:`` child is this document's own content, not
        independently gated — only checked against whatever ceiling this run
        is already under (``None`` return: inherit it unchanged), the same
        check a generated plan's tool refs get
        (:func:`circuitry.capability_gate.require_within_ceiling`).
        """
        from ..capability_gate import require_within_ceiling
        from ..cli.allowlist import walk_orchestration_refs
        from ..cli.document_consent import (
            DocumentConsentError,
            consented_capabilities,
            refusal_message,
        )
        from ..plugins.capabilities import capabilities_of

        _adapters, tools = walk_orchestration_refs(child_orch, include_document_adapter=False)
        required = frozenset(cap for tool in tools for cap in capabilities_of(tool))

        if self.defn.ref is None:
            require_within_ceiling(
                f"use '{self.defn.name}': child {label}", required, self.runtime_config
            )
            return None

        from ..cli.config import trust_store_path

        allow = self._capability_allow_override()
        consented = (
            consented_capabilities(digest, store_path=trust_store_path()) or frozenset()
        )
        missing = required - consented - allow
        if missing:
            raise DocumentConsentError(
                f"use '{self.defn.name}': child {refusal_message(label, missing)}"
            )
        # consented | allow, not just `required`: this child's own ceiling must
        # carry forward whatever broader capability set was already consented
        # for its digest (and any --allow-capabilities override), so a ref
        # child with few or no gated tools of its own doesn't wall off a
        # grandchild that needs more (#275).
        return consented | allow

    def _capability_allow_override(self) -> frozenset[str]:
        configured = self.runtime_config.get("_capability_allow")
        return frozenset(configured) if isinstance(configured, list) else frozenset()

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
        from ..capability_gate import install_capability_ceiling
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
        meta["child_errors"] = None
        #: What the child received, and a digest of the YAML it ran: together
        #: with the child's record they are what a later reader needs to check
        #: or reproduce this step.
        meta["inputs"] = None
        meta["orchestration_sha256"] = None
        record_children = _record_children_enabled(self.runtime_config)

        #: This effect's canonical dotted path — what child effects namespace
        #: under and where their live state is mirrored for observers.
        node_path = store.effect_path(self.defn.name)

        indent = "  " * self.depth
        t0 = time.monotonic()
        # Set once the child store exists, so the `except` branch below can
        # still recover any errors a tree-flow sibling recorded before the
        # effect that actually raised — see `_collect_child_errors`.
        child_store: Store | None = None

        # The use effect announces itself the way every other effect does, so
        # the child effects forwarded below have a node to hang under. The
        # matching complete fires from the `finally` — every exit path,
        # including a child that blew up, closes the pair.
        store.fire_effect_start(self.defn.name, node)

        if self.verbose:
            if self.cb_start is not None:
                self.cb_start()
            else:
                _console.print(
                    f"{indent}[info]→[/info] [green]⊕[/green] {self.display_name}"
                )

        try:
            if self.dry_run:
                node["value"] = None
                meta["completed_at"] = _now_iso()
                meta["dry_run"] = True
                if self.verbose:
                    elapsed = time.monotonic() - t0
                    line = (
                        f"{indent}[ok]✓[/ok] [green]⊕[/green] {self.display_name}"
                        f" [dim]{label} | {_elapsed_str(elapsed)} (dry)[/dim]"
                    )
                    if self.cb_done is not None:
                        self.cb_done(line)
                    else:
                        _console.print(line)
                return

            # Load (file or inline) and compile
            child_orch, resolved_label, identity, digest = self._load_child_orch(ctx)
            meta["orchestration_sha256"] = digest
            label = resolved_label
            if not meta["inline"]:
                meta["resolved_path"] = resolved_label
            if self._pin is not None:
                meta["library_ref"] = self._pin
            self._check_allowlists(child_orch, label)
            child_capability_ceiling = self._check_capability_consent(
                child_orch, label, digest
            )

            # Cycle detection — runtime call-stack tracking by resolved identity.
            # The stack is derived per call-path rather than mutated in place:
            # `runtime_config` is one dict shared by every runtime in the run,
            # and tree-flow iterations execute concurrently on a
            # ThreadPoolExecutor, so a shared, mutated list would let sibling
            # iterations see each other as ancestors (false-positive cycles).
            parent_stack: list[str] = list(
                self.runtime_config.get("_use_call_stack", [])
            )
            if identity in parent_stack:
                cycle_path = " → ".join([*parent_stack, identity])
                raise RecursionError(
                    f"use '{self.defn.name}': cycle detected — {cycle_path}"
                )
            child_runtime_config = dict(self.runtime_config)
            child_runtime_config["_use_call_stack"] = [*parent_stack, identity]
            if child_capability_ceiling is not None:
                install_capability_ceiling(child_runtime_config, child_capability_ceiling)
            # A nested `use: {path: ...}` inside this child resolves relative
            # to *this* child's own directory, not the root orchestration's —
            # composition chains through each file's own location. Inline
            # children have no file/directory of their own, so they inherit
            # whatever directory was already in effect.
            if not meta["inline"]:
                child_runtime_config["_orchestration_dir"] = str(Path(identity).parent)

            child_root = compile_orchestration(orch=child_orch, root_name="prime")

            # Build isolated child state: rendered inputs land in the
            # child's `input` namespace, same contract as a top-level run.
            child_inputs: dict[str, Any] = {}
            unresolved: dict[str, str] = {}
            if self.defn.inputs:
                child_inputs = _render_inputs(self.defn.inputs, ctx)
                unresolved = _unresolved_references(self.defn.inputs, child_inputs)
            # Check interface first — it fills in declared `default:`s and
            # coerces declared-typed values — so `meta["inputs"]` below
            # records what the child actually ran with, not the pre-check
            # rendering.
            auto_outputs = self._check_interface(child_orch, child_inputs, unresolved)
            meta["inputs"] = copy.deepcopy(child_inputs)
            child_state: dict[str, Any] = {"input": child_inputs}

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
                concurrent_dispatch=store.concurrent_dispatch,
                branch_settled=store.branch_settled,
                _lock=store._lock,
            )

            # Execute child orchestration. The child's own effects inherit
            # this invocation's display name as a label prefix — but only
            # when it actually differs from the bare effect name (i.e. an
            # enclosing loop or `use` gave it one); otherwise every
            # unqualified `use` would start tagging its child's lines,
            # changing output that today has nothing to disambiguate.
            child_label_prefix = (
                self.display_name if self.display_name != self.defn.name else None
            )
            DynamicRuntime(
                child_root,
                adapter=self.adapter,
                model=self.model,
                model_locked=self.model_locked,
                runtime_config=child_runtime_config,
                dry_run=self.dry_run,
                timeout_seconds=self.timeout_seconds,
                verbose=self.verbose,
                depth=self.depth + 1,
                ancestors=self._ancestors,
                label_prefix=child_label_prefix,
            ).execute(store=child_store)

            # Surface errors an on_error: skip/continue swallowed inside the
            # child — without this, a composed failure is indistinguishable
            # from a healthy result once only the mapped `value` is visible.
            child_errors = _collect_child_errors(child_store.state.get(_CHILD_ROOT))
            if child_errors:
                meta["child_errors"] = child_errors

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
                # Complete record (opt-in): the child's own effects stay under
                # this node in the final state, as --live-state showed them.
                if record_children:
                    _graft_child_record(node, child_store.state)
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
                line = (
                    f"{indent}[ok]✓[/ok] [green]⊕[/green] {self.display_name}"
                    f" [dim]{label} | {_elapsed_str(elapsed)}[/dim]"
                )
                if self.cb_done is not None:
                    self.cb_done(line)
                else:
                    _console.print(line)

        except Exception as e:
            error_msg = str(e)
            meta["error"] = error_msg
            meta["completed_at"] = _now_iso()

            # Capture validation errors separately for introspection
            if "validation failed" in error_msg.lower():
                meta["validation_errors"] = error_msg

            # A tree-flow sibling may have recorded its own swallowed error
            # before the effect that actually raised propagated — recover
            # whatever the child state tree holds, best-effort.
            if child_store is not None:
                child_errors = _collect_child_errors(child_store.state.get(_CHILD_ROOT))
                if child_errors:
                    meta["child_errors"] = child_errors
                # A failed child's record matters most: keep what it did get to.
                if record_children:
                    _graft_child_record(node, child_store.state)

            if self.verbose:
                elapsed = time.monotonic() - t0
                line = (
                    f"{indent}[err]✗[/err] [green]⊕[/green] {self.display_name}"
                    f" [dim]{label} | {_elapsed_str(elapsed)}[/dim]"
                )
                if self.cb_error is not None:
                    self.cb_error(line)
                else:
                    _console.print(line)

            if self.defn.on_error == "fail":
                raise RuntimeError(
                    f"use '{self.defn.name}' -> {label}: {e}"
                ) from e
            if self.defn.on_error in ("skip", "continue"):
                # A reused node (an unnamed loop's prior pass, a resume)
                # must not let that pass's value survive next to this
                # pass's error (#260).
                node["value"] = None
        finally:
            store.fire_effect_complete(self.defn.name, node)
