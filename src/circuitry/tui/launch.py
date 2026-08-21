"""Everything the Run view needs that is not a widget.

Orchestration discovery, the typed input form derived from an
orchestration's ``interface.inputs``, adapter/model option lists, and the
worker-thread run session with its cooperative cancel. No Textual imports
live here, so the rules that decide *what a run is* are testable — and
reusable — without booting an app.

The run session's ``state_observer`` is deliberately pure: it deep-copies
the snapshot it hands to the UI and never writes back, so a run driven
from the TUI ends in exactly the state a plain ``cof run`` would produce.
"""

from __future__ import annotations

import json
import threading
from collections.abc import Callable, Iterable
from copy import deepcopy
from dataclasses import dataclass, field, replace
from pathlib import Path
from typing import Any

from ..adapters.factory import ADAPTER_REGISTRY
from ..adapters.models import list_adapter_models
from ..cli.config import CircuitryConfig
from ..cli.orchestration_loader import ORCHESTRATION_SUFFIXES, load_orchestration_file
from ..cli.registry import load_index, resolve_bundled
from ..cli.runtime_shim import RunRequest, RunResult
from ..cli.runtime_shim import run as shim_run

__all__ = [
    "CANCEL_MESSAGE",
    "CUSTOM_MODEL",
    "INPUT_TYPES",
    "NO_OVERRIDE",
    "InputError",
    "InputField",
    "OrchestrationChoice",
    "OrchestrationForm",
    "RunCancelled",
    "RunSession",
    "adapter_models",
    "adapter_options",
    "build_initial_state",
    "coerce_input",
    "default_text",
    "discover_orchestrations",
    "input_fields",
    "load_form",
    "model_options",
    "placeholder_for",
]

#: Sentinel option for the adapter/model dropdowns: leave resolution alone.
NO_OVERRIDE = "—"

#: Sentinel option on the model dropdown that swaps it for a free-text
#: box. A curated list can always be wrong about the one model you want;
#: this keeps the TUI at parity with ``cof run --model <anything>``.
CUSTOM_MODEL = "custom…"

#: Error text a cancelled run surfaces through ``RunResult.error``.
CANCEL_MESSAGE = "Run cancelled by request."

#: Input types the schema declares; anything else is treated as a string.
INPUT_TYPES: tuple[str, ...] = ("string", "number", "boolean", "array", "object")

#: Adapters that cannot be built from config alone, so they are never
#: offered as an override (host_claude needs an injected request handler).
_UNSELECTABLE_ADAPTERS = frozenset({"host_claude"})

#: Guard rails for the local-file scan: skip anything implausible rather
#: than parsing a directory full of unrelated JSON.
_MAX_LOCAL_FILES = 200
_MAX_LOCAL_BYTES = 1_000_000

_TRUE_WORDS = frozenset({"true", "t", "yes", "y", "on", "1"})
_FALSE_WORDS = frozenset({"false", "f", "no", "n", "off", "0"})


class InputError(ValueError):
    """A form value that does not satisfy its declared type."""


class RunCancelled(RuntimeError):
    """Raised inside the observer to unwind a run the user cancelled."""


# -- orchestration discovery -------------------------------------------------


@dataclass(frozen=True)
class OrchestrationChoice:
    """One row in the picker."""

    key: str
    label: str
    path: Path
    source: str  # "bundled" | "local"
    description: str = ""

    @property
    def option(self) -> str:
        """Text shown in the picker."""
        suffix = f" — {self.description}" if self.description else ""
        return f"[{self.source}] {self.label}{suffix}"


def bundled_choices() -> list[OrchestrationChoice]:
    """Curated orchestrations from the bundled manifest, in manifest order."""
    choices: list[OrchestrationChoice] = []
    for entry in load_index():
        name = str(entry.get("name") or "").strip()
        if not name:
            continue
        path = resolve_bundled(name)
        if path is None:
            # Manifest entry without a file on disk — nothing to launch.
            continue
        choices.append(
            OrchestrationChoice(
                key=name,
                label=name,
                path=path,
                source="bundled",
                description=str(entry.get("description") or "").strip(),
            )
        )
    return choices


def local_choices(root: Path) -> list[OrchestrationChoice]:
    """Orchestration files sitting next to the user, newest naming first.

    Scans ``root`` and ``root/orchestrations`` one level deep. A file only
    counts when it parses as a mapping with ``effects`` (or legacy
    ``steps``), which keeps unrelated JSON out of the picker.
    """
    choices: list[OrchestrationChoice] = []
    seen: set[Path] = set()
    scanned = 0
    for directory in (root, root / "orchestrations"):
        if not directory.is_dir():
            continue
        for path in sorted(directory.iterdir()):
            if scanned >= _MAX_LOCAL_FILES:
                return choices
            if not path.is_file() or path.suffix.lower() not in ORCHESTRATION_SUFFIXES:
                continue
            scanned += 1
            resolved = path.resolve()
            if resolved in seen:
                continue
            orch = _try_load(path)
            if orch is None:
                continue
            seen.add(resolved)
            rel = path.relative_to(root) if path.is_relative_to(root) else path
            choices.append(
                OrchestrationChoice(
                    key=str(path),
                    label=str(rel),
                    path=path,
                    source="local",
                    description=_summary(orch),
                )
            )
    return choices


def discover_orchestrations(root: Path | None = None) -> list[OrchestrationChoice]:
    """Local files first (they are what the user is working on), then bundled."""
    base = Path.cwd() if root is None else root
    return [*local_choices(base), *bundled_choices()]


def _try_load(path: Path) -> dict[str, Any] | None:
    """Load ``path`` if it is plausibly an orchestration, else ``None``."""
    try:
        if path.stat().st_size > _MAX_LOCAL_BYTES:
            return None
        orch = load_orchestration_file(path)
    except Exception:
        # Unparseable, unreadable, or an optional-format dependency is
        # missing. Either way it is not a launchable orchestration.
        return None
    if not isinstance(orch.get("effects") or orch.get("steps"), list):
        return None
    return orch


def _summary(orch: dict[str, Any]) -> str:
    for key in ("description", "intent"):
        value = orch.get(key)
        if isinstance(value, str) and value.strip():
            return value.strip()
    return ""


# -- the typed input form ----------------------------------------------------


@dataclass(frozen=True)
class InputField:
    """One declared input from ``interface.inputs``."""

    name: str
    type: str = "string"
    required: bool = False
    description: str = ""
    default: Any = None

    @property
    def has_default(self) -> bool:
        return self.default is not None

    @property
    def label(self) -> str:
        marker = " *" if self.required else ""
        return f"{self.name}{marker} ({self.type})"


@dataclass(frozen=True)
class OrchestrationForm:
    """What the Run view needs to render one orchestration."""

    choice: OrchestrationChoice
    fields: tuple[InputField, ...] = ()
    adapter: str | None = None
    model: str | None = None
    orchestration: dict[str, Any] = field(default_factory=dict)


def input_fields(orch: dict[str, Any]) -> list[InputField]:
    """Read ``interface.inputs`` into form fields, in declaration order."""
    interface = orch.get("interface")
    if not isinstance(interface, dict):
        return []
    declared = interface.get("inputs")
    if not isinstance(declared, dict):
        return []

    fields: list[InputField] = []
    for name, spec in declared.items():
        if not isinstance(name, str) or not name:
            continue
        if not isinstance(spec, dict):
            fields.append(InputField(name=name))
            continue
        raw_type = str(spec.get("type") or "string").strip().lower()
        fields.append(
            InputField(
                name=name,
                type=raw_type if raw_type in INPUT_TYPES else "string",
                required=bool(spec.get("required")),
                description=str(spec.get("description") or "").strip(),
                default=spec.get("default"),
            )
        )
    return fields


def load_form(choice: OrchestrationChoice) -> OrchestrationForm:
    """Load ``choice`` and derive its form. Raises if the file is unusable."""
    orch = load_orchestration_file(choice.path)
    adapter = orch.get("adapter")
    model = orch.get("model")
    return OrchestrationForm(
        choice=choice,
        fields=tuple(input_fields(orch)),
        adapter=str(adapter) if adapter is not None else None,
        model=str(model) if model is not None else None,
        orchestration=orch,
    )


def default_text(field: InputField) -> str:
    """The declared default rendered for a text box (empty when unset)."""
    return "" if field.default is None else _render(field.default)


def placeholder_for(field: InputField) -> str:
    """Hint text: the description, else the syntax the type accepts."""
    if field.description:
        return field.description
    return {
        "number": "a number",
        "boolean": "true or false",
        "array": "JSON array, e.g. [1, 2]",
        "object": 'JSON object, e.g. {"k": "v"}',
    }.get(field.type, "text")


def _render(value: Any) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float, str)):
        return str(value)
    return json.dumps(value, ensure_ascii=False)


def coerce_input(field: InputField, raw: str) -> Any:
    """Parse ``raw`` into ``field``'s declared type, or raise `InputError`."""
    text = raw.strip()
    if field.type == "string":
        # Strings keep their interior whitespace; only the edges are noise.
        return text
    if field.type == "number":
        try:
            return int(text)
        except ValueError:
            pass
        try:
            return float(text)
        except ValueError:
            raise InputError(f"{field.name}: expected a number, got {raw!r}") from None
    if field.type == "boolean":
        lowered = text.lower()
        if lowered in _TRUE_WORDS:
            return True
        if lowered in _FALSE_WORDS:
            return False
        raise InputError(f"{field.name}: expected true or false, got {raw!r}")
    if field.type in ("array", "object"):
        try:
            value = json.loads(text)
        except json.JSONDecodeError as exc:
            raise InputError(f"{field.name}: expected JSON {field.type} ({exc.msg})") from None
        expected = list if field.type == "array" else dict
        if not isinstance(value, expected):
            raise InputError(f"{field.name}: expected a JSON {field.type}")
        return value
    return text


def build_initial_state(
    fields: Iterable[InputField], raw: dict[str, str]
) -> tuple[dict[str, Any], dict[str, str]]:
    """Turn raw form text into ``initial_state``.

    Returns ``(state, errors)`` keyed by field name. A blank optional field
    falls back to its declared default and is otherwise left out entirely,
    so the orchestration's own handling of a missing key still applies.
    """
    state: dict[str, Any] = {}
    errors: dict[str, str] = {}
    for spec in fields:
        text = raw.get(spec.name, "")
        if not text.strip():
            if spec.required:
                errors[spec.name] = f"{spec.name} is required"
            elif spec.has_default:
                state[spec.name] = deepcopy(spec.default)
            continue
        try:
            state[spec.name] = coerce_input(spec, text)
        except InputError as exc:
            errors[spec.name] = str(exc)
    return state, errors


# -- adapter / model options -------------------------------------------------


def adapter_options(cfg: CircuitryConfig) -> list[str]:
    """Adapters the user has configured, filtered by the allowlist.

    Falls back to every buildable adapter when nothing is configured, so a
    fresh install still has something to pick.
    """
    configured = {
        str(name).strip().lower()
        for name in (cfg.runtime.get("adapters") or {})
        if str(name).strip()
    }
    if cfg.default_adapter:
        configured.add(cfg.default_adapter.strip().lower())
    if not configured:
        configured = set(ADAPTER_REGISTRY)
    allowed = cfg.enabled_adapters
    if allowed is not None:
        configured &= {name.strip().lower() for name in allowed}
    return sorted(configured - _UNSELECTABLE_ADAPTERS)


def model_options(
    cfg: CircuitryConfig,
    orch: dict[str, Any] | None = None,
    extra: Iterable[str] = (),
) -> list[str]:
    """Models named anywhere in config or in the selected orchestration.

    ``extra`` folds in whatever an adapter reported from
    :func:`~circuitry.adapters.models.call_list_models` — the installed
    Ollama tags, a tier list — so the dropdown offers what the machine
    actually has, not only what the config happens to mention.
    """
    models: set[str] = set()
    if cfg.default_model:
        models.add(cfg.default_model.strip())
    adapters_cfg = cfg.runtime.get("adapters") or {}
    if isinstance(adapters_cfg, dict):
        for adapter_cfg in adapters_cfg.values():
            if isinstance(adapter_cfg, dict):
                model = adapter_cfg.get("default_model")
                if isinstance(model, str) and model.strip():
                    models.add(model.strip())
    if orch:
        model = orch.get("model")
        if isinstance(model, str) and model.strip():
            models.add(model.strip())
    models.update(name.strip() for name in extra if name and name.strip())
    return sorted(models)


def adapter_models(cfg: CircuitryConfig, adapter_name: str | None) -> list[str]:
    """Models the named adapter offers, or ``[]`` — never raises.

    Called from a worker thread: ``ollama`` reaches out to its local
    daemon, and even a two-second connect is two seconds the UI thread
    must not spend.
    """
    name = (adapter_name or "").strip().lower()
    if not name or name in _UNSELECTABLE_ADAPTERS:
        return []
    return list_adapter_models(adapter_name=name, runtime=cfg.runtime or {})


# -- the run session ---------------------------------------------------------

Runner = Callable[[RunRequest], RunResult]
StateCallback = Callable[[dict[str, Any]], None]
EffectCallback = Callable[[str, dict[str, Any]], None]
FinishCallback = Callable[[RunResult], None]


class RunSession:
    """One run, executed on a worker thread with a pure state observer.

    ``on_state`` receives a deep copy of every snapshot the runtime writes;
    ``on_finish`` receives the :class:`RunResult`. Both are invoked from the
    worker thread — Textual's ``post_message`` is thread-safe, which is what
    the Run view uses.

    Cancellation is cooperative: :meth:`cancel` sets a flag the observer
    checks on the next state write, which unwinds the runtime through its
    normal error path. The result is an ordinary failed
    :class:`RunResult`, so nothing is left half-torn-down.
    """

    def __init__(
        self,
        request: RunRequest,
        *,
        on_state: StateCallback | None = None,
        on_effect: EffectCallback | None = None,
        on_effect_start: EffectCallback | None = None,
        on_finish: FinishCallback | None = None,
        runner: Runner | None = None,
    ) -> None:
        self._on_state = on_state
        self._on_effect = on_effect
        self._on_effect_start = on_effect_start
        self._on_finish = on_finish
        self._runner: Runner = runner if runner is not None else shim_run
        self._cancelled = threading.Event()
        self._request = replace(
            request,
            state_observer=self._observe,
            effect_observer=self._observe_effect,
            effect_start_observer=self._observe_effect_start,
        )
        self._thread = threading.Thread(
            target=self._execute, name="circuitry-tui-run", daemon=True
        )
        self._result: RunResult | None = None

    @property
    def request(self) -> RunRequest:
        """The request as it will be executed (observer attached)."""
        return self._request

    @property
    def result(self) -> RunResult | None:
        """The finished result, or ``None`` while the run is in flight."""
        return self._result

    @property
    def cancelled(self) -> bool:
        return self._cancelled.is_set()

    @property
    def running(self) -> bool:
        return self._thread.is_alive()

    def start(self) -> None:
        self._thread.start()

    def cancel(self) -> None:
        """Ask the run to stop at the next state write."""
        self._cancelled.set()

    def join(self, timeout: float | None = None) -> None:
        self._thread.join(timeout)

    # -- worker side ---------------------------------------------------------

    def _observe(self, state: dict[str, Any]) -> None:
        if self._cancelled.is_set():
            raise RunCancelled(CANCEL_MESSAGE)
        if self._on_state is not None:
            # Deep copy: the UI must never share (or write back into) the
            # live runtime state, or an observed run would drift from an
            # unobserved one.
            self._on_state(deepcopy(state))

    def _observe_effect_start(self, path: str, node: dict[str, Any]) -> None:
        """One effect is about to dispatch. Deep-copied like everything else.

        This is the only moment the UI can learn something about an effect
        *before* it runs — the node already carries the resolved adapter and
        model, the rendered prompt, and the complexity score when scoring is
        on. Not a cancellation point: the run stops at the next state write,
        which keeps the unwind out of the lifecycle hooks.
        """
        if self._on_effect_start is not None:
            self._on_effect_start(path, deepcopy(node))

    def _observe_effect(self, path: str, node: dict[str, Any]) -> None:
        """One effect landed. Deep-copied for the same reason as state.

        Not a cancellation point: the effect has already happened, and
        raising here would unwind the run from inside a lifecycle hook
        rather than at the next state write.
        """
        if self._on_effect is not None:
            self._on_effect(path, deepcopy(node))

    def _execute(self) -> None:
        try:
            result = self._runner(self._request)
        except BaseException as exc:  # noqa: BLE001 - a dead worker must still report
            result = RunResult(ok=False, state={}, warnings=[], error=str(exc))
        self._result = result
        if self._on_finish is not None:
            self._on_finish(result)
