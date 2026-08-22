"""Validate Circuitry orchestration YAML — schema + compiler, no LLM.

This is the deterministic half of any orchestration that *writes*
orchestrations (see ``curation/agents/wizard.yml``): a model drafts YAML,
this plugin says whether the draft is real, and the errors it returns are
specific enough to feed straight back into a revision prompt.

Params:
  - ``yaml`` (required, str): the orchestration document to validate. An
    empty string is reported as invalid (``ok: false``) rather than raising —
    "the model produced no YAML" is a revisable outcome, not a tool failure.
  - ``strip_fences`` (optional, bool, default ``True``): strip markdown code
    fences and stray ``---`` separators before parsing. LLM-authored YAML
    arrives fenced often enough that the default is lenient; set ``false``
    to hold the model to a strictly fence-free contract.
  - ``compile`` (optional, bool, default ``True``): run the compiler after the
    JSON Schema pass to surface semantic errors the schema cannot express
    (duplicate sibling names, reserved ``iter_N`` names, unknown effect types).
  - ``max_errors`` (optional, int, default ``20``): cap on reported errors.
    Schema violations cascade through the ``EffectDef`` if/then chain, and an
    uncapped list can swamp the revision prompt it is meant to inform.

``ToolResult.value``::

    {"ok": bool, "errors": [str], "yaml": str}

``yaml`` echoes the cleaned document that was actually validated, so a
revision step can feed the exact text back to the model alongside the errors
without having to re-derive it.
"""

from __future__ import annotations

import importlib.resources
import json
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from ..preflight import CheckResult
from .base import ToolResult

_DEFAULT_MAX_ERRORS = 20


def _as_bool(value: Any, *, default: bool) -> bool:
    if value is None:
        return default
    if isinstance(value, bool):
        return value
    if isinstance(value, str):
        return value.strip().lower() in ("1", "true", "yes", "on")
    return bool(value)


def _load_schema() -> dict[str, Any]:
    ref = (
        importlib.resources.files("circuitry")
        / "schema"
        / "orchestration.schema.json"
    )
    return json.loads(Path(str(ref)).read_text(encoding="utf-8"))


def _describe(err: Any) -> str:
    """Render one schema error as ``<json path>: <message>``.

    ``EffectDef`` discriminates on ``type`` through nested if/then and
    ``PromptEffect`` requires ``template`` or ``messages`` via ``oneOf``, so the
    top-level message is often the useless "is not valid under any of the given
    schemas". The deepest sub-error carries the actionable detail — append it.
    """
    import jsonschema  # type: ignore[import-untyped]

    message = err.message
    if err.context:
        sub = jsonschema.exceptions.best_match(err.context)
        if sub is not None and sub.message != err.message:
            message = f"{message} ({sub.message})"
    return f"{err.json_path}: {message}"


def _validate_document(document: str, *, run_compile: bool) -> list[str]:
    """Return human-readable errors for *document*. Empty list means valid."""
    import jsonschema  # type: ignore[import-untyped]
    import yaml as _yaml

    if not document.strip():
        return ["Empty document — expected a Circuitry orchestration YAML."]

    try:
        parsed = _yaml.safe_load(document)
    except _yaml.YAMLError as exc:
        return [f"YAML parse error: {exc}"]

    if not isinstance(parsed, dict):
        return [
            (
                "Orchestration must be a YAML mapping with an 'effects' key, "
                f"got {type(parsed).__name__}."
            )
        ]
    if "effects" not in parsed:
        return ["Orchestration is missing the required top-level 'effects' key."]

    validator = jsonschema.Draft7Validator(_load_schema())
    errors = [
        _describe(err) for err in sorted(validator.iter_errors(parsed), key=str)
    ]
    if errors:
        return errors

    if run_compile:
        # The compiler enforces what the schema cannot: duplicate sibling
        # names, reserved iter_N names, per-type required-field combinations.
        from ..core.compiler import compile_orchestration

        try:
            compile_orchestration(orch=parsed, root_name="prime")
        except Exception as exc:
            return [f"Compile error: {exc}"]

    return []


@dataclass(frozen=True)
class ValidateYamlPlugin:
    name: str = "validate_yaml"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        del timeout_seconds
        document = params.get("yaml")
        if document is None:
            document = ""
        if not isinstance(document, str):
            raise ValueError(
                "ValidateYamlPlugin requires params['yaml'] as a string "
                f"(got {type(document).__name__})."
            )

        if _as_bool(params.get("strip_fences"), default=True):
            from ..core.use import _clean_yaml_fences

            document = _clean_yaml_fences(document)

        max_errors = params.get("max_errors")
        try:
            limit = int(max_errors) if max_errors is not None else _DEFAULT_MAX_ERRORS
        except (TypeError, ValueError) as exc:
            raise ValueError(
                f"ValidateYamlPlugin params['max_errors'] must be an integer: {exc}"
            ) from exc
        if limit < 1:
            raise ValueError("ValidateYamlPlugin params['max_errors'] must be >= 1.")

        errors = _validate_document(
            document,
            run_compile=_as_bool(params.get("compile"), default=True),
        )
        if len(errors) > limit:
            hidden = len(errors) - limit
            errors = [*errors[:limit], f"... and {hidden} more error(s)."]

        value: dict[str, Any] = {
            "ok": not errors,
            "errors": errors,
            "yaml": document,
        }
        return ToolResult(
            value=value,
            raw=dict(value),
            stdout=None,
            stderr="\n".join(errors) or None,
            exit_code=0 if not errors else 1,
        )

    def check(self) -> CheckResult:
        # pyyaml and jsonschema are core dependencies — nothing to probe.
        return CheckResult(ok=True, missing=[])


def make_plugin() -> ValidateYamlPlugin:
    return ValidateYamlPlugin()
