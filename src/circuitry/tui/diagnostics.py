"""Environment and file diagnostics behind the Doctor, Settings and Validate views.

Pure data — nothing here imports Textual — so the logic is testable without a
terminal and the screens stay presentation-only. Every function drives the same
code path the matching `cof` subcommand uses, so the TUI can never disagree with
the CLI about whether a machine is healthy or a file is valid.

Three groups live here:

``check_targets`` / ``run_check``
    The preflight walk, split into "what is going to be checked" (instant, so a
    view can paint one row per extension immediately) and "check this one
    thing" (slow — some ``check()`` implementations probe the network — so the
    view runs it in a worker and fills the row in when it lands).
``settings_rows``
    :func:`~circuitry.cli.effective_settings.resolve_effective_settings`
    flattened to one row per value, each carrying the layer it came from and
    passed through :func:`~circuitry.cli.redaction.redact` first.
``validate_report``
    Every gate ``cof check`` runs, evaluated *independently*. The CLI returns at
    the first gate that trips; a person staring at a broken file wants the whole
    list, so schema, compile, cycle and preflight problems are all collected.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Literal, Protocol

from ..adapters import build_adapter
from ..adapters.factory import ADAPTER_REGISTRY
from ..cli import runtime_shim
from ..cli.allowlist import check_allowlist
from ..cli.config import CircuitryConfig, find_config_path, load_config, resolve_config
from ..cli.effective_settings import EffectiveSettings, resolve_effective_settings
from ..cli.orchestration_loader import load_orchestration_file
from ..cli.redaction import redact
from ..core.compiler import compile_orchestration
from ..core.cycle_check import detect_cycles
from ..core.lint import lint_orchestration
from ..core.runtime_plugins import load_plugins
from ..plugins.factory import PLUGIN_REGISTRY, build_plugin
from ..preflight import CheckResult, call_check

__all__ = [
    "CATEGORIES",
    "ISSUE_KINDS",
    "CheckTarget",
    "Diagnostics",
    "DiagnosticsSource",
    "ExtensionCheck",
    "SettingRow",
    "ValidationIssue",
    "ValidationReport",
    "check_targets",
    "load_diagnostics",
    "next_step",
    "run_check",
    "run_checks",
    "settings_rows",
    "validate_report",
]

#: Extension categories, in the order the Doctor view lists them.
CATEGORIES: tuple[str, ...] = ("adapter", "tool", "runtime_plugin")

CATEGORY_LABELS: dict[str, str] = {
    "adapter": "Adapters",
    "tool": "Tool plugins",
    "runtime_plugin": "Runtime plugins",
}

CATEGORY_NOUNS: dict[str, tuple[str, str]] = {
    "adapter": ("adapter", "adapters"),
    "tool": ("tool plugin", "tool plugins"),
    "runtime_plugin": ("runtime plugin", "runtime plugins"),
}


def counted(count: int, category: str) -> str:
    """``1 adapter`` / ``3 adapters`` — counts read badly when pluralised wrong."""
    singular, plural = CATEGORY_NOUNS[category]
    return f"{count} {singular if count == 1 else plural}"

#: Outcome of one extension check. ``checking`` is the pre-result placeholder
#: the view paints while the worker is still running.
CheckState = Literal["checking", "ok", "deferred", "missing", "error"]

STATE_LABELS: dict[str, str] = {
    "checking": "checking…",
    "ok": "ok",
    "deferred": "deferred",
    "missing": "missing deps",
    "error": "error",
}


def next_step(item: str) -> str:
    """Translate one ``missing`` item into a sentence telling you what to do.

    The grammar is the one :class:`~circuitry.preflight.CheckResult` documents
    (``env:``/``binary:``/``library:``/``host:``). Anything that does not match
    is echoed back rather than dropped — an unknown prefix is still information.
    """
    kind, separator, rest = item.partition(":")
    rest = rest.strip()
    if not separator or not rest:
        return f"Resolve: {item}"
    if kind == "env":
        return f"Set the {rest} environment variable, then re-run the check."
    if kind == "binary":
        return f"Install {rest} and make sure it is on your PATH."
    if kind == "library":
        return f"Install the Python package: pip install {rest.split('.')[0]}"
    if kind == "host":
        return f"Start the service at {rest}, or point the config at a host that is up."
    return f"Resolve: {item}"


@dataclass(frozen=True)
class CheckTarget:
    """One extension that is going to be checked."""

    category: str
    name: str

    @property
    def label(self) -> str:
        """``adapter:ollama`` — the same label preflight uses in its errors."""
        return f"{self.category}:{self.name}"

    @property
    def slug(self) -> str:
        """DOM-id-safe form of :attr:`label`."""
        return "".join(c if c.isalnum() else "-" for c in self.label)


@dataclass(frozen=True)
class ExtensionCheck:
    """Result of checking one extension, plus what to do about it."""

    target: CheckTarget
    state: CheckState = "checking"
    missing: tuple[str, ...] = ()
    message: str | None = None

    @property
    def ok(self) -> bool:
        return self.state in ("ok", "deferred")

    @property
    def pending(self) -> bool:
        return self.state == "checking"

    @property
    def next_steps(self) -> tuple[str, ...]:
        """One actionable sentence per missing dependency."""
        return tuple(next_step(item) for item in self.missing)

    @property
    def detail(self) -> str:
        """One line of detail: the next steps if any, else the message."""
        steps = self.next_steps
        if steps:
            return " ".join(steps)
        return self.message or "—"

    def line(self) -> str:
        """Full row text: ``name  status — detail``."""
        return f"{self.target.name}  {STATE_LABELS[self.state]} — {self.detail}"


def _allowed_or_compiled(allowed: list[str] | None, compiled: Any) -> list[str]:
    """Allowlisted names when one is configured, else everything compiled in."""
    if allowed is None:
        return sorted(compiled)
    return sorted(allowed)


def check_targets(config: CircuitryConfig) -> tuple[CheckTarget, ...]:
    """Every extension ``cof doctor`` would check, in display order.

    Cheap by design: enumerating names touches no network and builds nothing,
    so a view can render the full row list before a single ``check()`` runs.
    """
    targets: list[CheckTarget] = [
        CheckTarget("adapter", name)
        for name in _allowed_or_compiled(config.enabled_adapters, ADAPTER_REGISTRY.keys())
    ]
    targets += [
        CheckTarget("tool", name)
        for name in _allowed_or_compiled(config.enabled_tools, PLUGIN_REGISTRY.keys())
    ]
    targets += [CheckTarget("runtime_plugin", name) for name in config.plugins]
    return tuple(targets)


def _from_result(target: CheckTarget, result: CheckResult) -> ExtensionCheck:
    """Map a :class:`~circuitry.preflight.CheckResult` onto an ExtensionCheck."""
    if result.ok:
        return ExtensionCheck(target, "ok", (), result.message)
    return ExtensionCheck(
        target, "missing", tuple(result.missing or ()), result.message
    )


def run_check(target: CheckTarget, config: CircuitryConfig) -> ExtensionCheck:
    """Build ``target`` and call its ``check()``. Slow: may hit the network.

    Mirrors ``cof doctor``'s classification exactly: an adapter that can only be
    built with a runtime-injected handler is *deferred* rather than broken, an
    unknown or unloadable extension is an *error*, and everything else is
    whatever ``check()`` said.
    """
    runtime = config.runtime or {}
    if target.category == "adapter":
        try:
            adapter = build_adapter(adapter_name=target.name, runtime=runtime)
        except RuntimeError as exc:
            return ExtensionCheck(target, "deferred", (), f"runtime-injected: {exc}")
        except ValueError as exc:
            return ExtensionCheck(target, "error", (), str(exc))
        return _from_result(target, call_check(adapter))

    if target.category == "tool":
        try:
            tool = build_plugin(plugin_name=target.name, runtime=runtime)
        except (RuntimeError, ValueError) as exc:
            return ExtensionCheck(target, "error", (), str(exc))
        return _from_result(target, call_check(tool))

    for load_result in load_plugins([target.name], allowed=config.enabled_plugins):
        if load_result.plugin is None:
            return ExtensionCheck(target, "error", (), f"load failed: {load_result.error}")
        return _from_result(target, call_check(load_result.plugin))
    return ExtensionCheck(target, "error", (), "plugin was not loaded (not allowlisted?)")


def run_checks(config: CircuitryConfig) -> tuple[ExtensionCheck, ...]:
    """Check every target serially. Convenience for non-interactive callers."""
    return tuple(run_check(target, config) for target in check_targets(config))


@dataclass(frozen=True)
class SettingRow:
    """One effective setting: its value and the layer it came from."""

    key: str
    value: str
    source: str

    def line(self) -> str:
        return f"{self.key}  {self.value}  (from {self.source})"


def _format_value(value: Any) -> str:
    if value is None:
        return "—"
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, str):
        return value or "—"
    if isinstance(value, (list, tuple)):
        return ", ".join(_format_value(item) for item in value) if value else "—"
    return json.dumps(value, sort_keys=True, default=str)


def _flatten(prefix: str, value: Any) -> list[tuple[str, Any]]:
    """Flatten nested dicts to dotted keys; anything else is a leaf."""
    if not isinstance(value, dict) or not value:
        return [(prefix, value)]
    rows: list[tuple[str, Any]] = []
    for key in sorted(value, key=str):
        rows.extend(_flatten(f"{prefix}.{key}", value[key]))
    return rows


def settings_rows(effective: EffectiveSettings) -> tuple[SettingRow, ...]:
    """One row per effective value, redacted, tagged with its source layer.

    ``runtime`` is flattened to dotted keys so a nested credential shows up as
    its own row — and, having gone through :func:`redact`, shows up as the
    redaction sentinel rather than the secret.
    """
    sources = effective.sources
    rows = [
        SettingRow("model", _format_value(redact(effective.model)), sources.get("model", "default")),
        SettingRow(
            "adapter",
            _format_value(redact(effective.adapter)),
            sources.get("adapter", "default"),
        ),
        SettingRow(
            "plugins",
            _format_value(redact(list(effective.plugins))),
            sources.get("plugins", "default"),
        ),
    ]
    runtime_source = sources.get("runtime", "default")
    safe_runtime = redact(effective.runtime or {})
    if not safe_runtime:
        rows.append(SettingRow("runtime", "—", runtime_source))
    else:
        rows += [
            SettingRow(key, _format_value(value), runtime_source)
            for key, value in _flatten("runtime", safe_runtime)
        ]
    return tuple(rows)


#: Validation gates, in the order they run. ``load`` is a hard stop: nothing
#: else can be evaluated when the file will not parse.
ISSUE_KINDS: tuple[str, ...] = ("load", "schema", "allowlist", "compile", "cycle", "preflight")

KIND_LABELS: dict[str, str] = {
    "load": "Load",
    "schema": "Schema",
    "allowlist": "Allowlist",
    "compile": "Compile",
    "cycle": "Cycle",
    "preflight": "Preflight",
}


@dataclass(frozen=True)
class ValidationIssue:
    """One problem found in an orchestration file."""

    kind: str
    message: str
    location: str | None = None
    hints: tuple[str, ...] = ()

    def line(self) -> str:
        where = f"{self.location}: " if self.location else ""
        return f"{where}{self.message}"


@dataclass(frozen=True)
class ValidationReport:
    """Result of validating one file: every gate's findings, not just the first."""

    path: Path
    issues: tuple[ValidationIssue, ...] = ()
    #: Gates that could not run (no config supplied, jsonschema absent, ...).
    skipped: tuple[str, ...] = ()
    #: Advisory lint (deprecated aliases, type-keyword names). Deliberately
    #: outside :attr:`issues` — a warned-about file is still a valid file.
    warnings: tuple[str, ...] = ()

    @property
    def ok(self) -> bool:
        return not self.issues

    def of_kind(self, kind: str) -> tuple[ValidationIssue, ...]:
        return tuple(issue for issue in self.issues if issue.kind == kind)

    def kinds(self) -> tuple[str, ...]:
        """Kinds that produced at least one issue, in pipeline order."""
        return tuple(kind for kind in ISSUE_KINDS if self.of_kind(kind))


def _preflight_message(result: CheckResult) -> str:
    """Human phrasing of a failed preflight check (the raw list is for hints)."""
    parts = []
    if result.missing:
        parts.append("missing " + ", ".join(result.missing))
    if result.message:
        parts.append(result.message)
    return " — ".join(parts) or "not ready"


def _schema_location(error: Any) -> str | None:
    path = getattr(error, "absolute_path", None)
    if not path:
        return None
    return "/" + "/".join(str(part) for part in path)


def validate_report(
    path: Path,
    *,
    config: CircuitryConfig | None = None,
    skip_preflight: bool = False,
) -> ValidationReport:
    """Run every validation gate against ``path`` and collect all findings.

    ``cof check`` short-circuits at the first failing gate because it only needs
    an exit code. The view needs the whole picture, so each gate runs in its own
    ``try`` and a gate blowing up is reported as that gate's failure instead of
    aborting the rest.
    """
    issues: list[ValidationIssue] = []
    skipped: list[str] = []

    try:
        text = path.read_text(encoding="utf-8")
    except OSError as exc:
        return ValidationReport(path, (ValidationIssue("load", str(exc)),))
    if not text.strip():
        return ValidationReport(path, (ValidationIssue("load", "Orchestration file is empty."),))

    try:
        orch = load_orchestration_file(path)
    except Exception as exc:
        return ValidationReport(path, (ValidationIssue("load", str(exc)),))
    if not isinstance(orch, dict):
        return ValidationReport(
            path,
            (ValidationIssue("load", f"Expected a mapping at the top level, got {type(orch).__name__}."),),
        )

    warnings = tuple(lint_orchestration(orch))

    schema = runtime_shim.load_schema()
    if schema is None:
        skipped.append("schema")
    else:
        # Only reachable when the schema loaded, which means jsonschema imported.
        import jsonschema

        validator = jsonschema.Draft7Validator(schema)
        issues += [
            ValidationIssue("schema", error.message, _schema_location(error))
            for error in sorted(validator.iter_errors(orch), key=str)
        ]

    if config is None:
        skipped.append("allowlist")
    else:
        issues += [
            ValidationIssue("allowlist", message)
            for message in check_allowlist(orch=orch, config=config)
        ]

    try:
        compile_orchestration(orch=orch, root_name="prime")
    except Exception as exc:
        issues.append(ValidationIssue("compile", str(exc)))

    try:
        cycle = detect_cycles(
            orch,
            root_path=path,
            runtime=(config.runtime if config is not None else None),
        )
    except Exception as exc:  # noqa: BLE001 - unreadable sub-orchestration, etc.
        issues.append(ValidationIssue("cycle", str(exc)))
    else:
        if cycle is not None:
            issues.append(ValidationIssue("cycle", " → ".join(cycle)))

    if config is None or skip_preflight:
        skipped.append("preflight")
    else:
        try:
            results = runtime_shim.preflight(path, config)
        except Exception as exc:
            issues.append(ValidationIssue("preflight", str(exc)))
        else:
            issues += [
                ValidationIssue(
                    "preflight",
                    _preflight_message(result),
                    label,
                    tuple(next_step(item) for item in result.missing or ()),
                )
                for label, result in results
                if not result.ok
            ]

    return ValidationReport(path, tuple(issues), tuple(skipped), warnings)


class DiagnosticsSource(Protocol):
    """What the Doctor and Settings views need from an environment.

    A Protocol rather than a concrete class so a test can hand a screen a
    fixture machine — a handful of canned results — instead of whatever the
    machine running the suite happens to have installed.
    """

    def targets(self) -> tuple[CheckTarget, ...]:
        """Everything that will be checked, known without checking anything."""
        ...

    def check(self, target: CheckTarget) -> ExtensionCheck:
        """Check one target. May be slow; callers run it off the UI thread."""
        ...

    def rows(self) -> tuple[SettingRow, ...]:
        """The effective settings, redacted, with source attribution."""
        ...


@dataclass(frozen=True)
class Diagnostics:
    """The live environment: the real config, the real ``check()`` calls."""

    config: CircuitryConfig
    settings: EffectiveSettings

    def targets(self) -> tuple[CheckTarget, ...]:
        return check_targets(self.config)

    def check(self, target: CheckTarget) -> ExtensionCheck:
        return run_check(target, self.config)

    def rows(self) -> tuple[SettingRow, ...]:
        return settings_rows(self.settings)


def load_diagnostics(
    *,
    config_path: Path | None = None,
    orchestration_path: Path | None = None,
) -> Diagnostics:
    """Resolve the machine's config the way ``cof doctor`` resolves it."""
    raw_config = load_config(find_config_path(explicit_path=config_path))
    orch: dict[str, Any] = {}
    if orchestration_path is not None:
        try:
            orch = load_orchestration_file(orchestration_path)
        except Exception:
            orch = {}
    return Diagnostics(
        config=resolve_config(explicit_path=config_path),
        settings=resolve_effective_settings(cfg=raw_config, orch=orch),
    )
