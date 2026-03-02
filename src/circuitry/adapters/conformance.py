from __future__ import annotations

from typing import Any

from .base import GenerateResult


def validate_generate_result(result: GenerateResult, *, adapter_name: str) -> list[str]:
    """
    Validate adapter generate() output against normalized contract.

    Returns a list of diagnostics. Empty list means the contract is satisfied.
    """
    diagnostics: list[str] = []

    if not isinstance(result.text, str):
        diagnostics.append(
            f"{adapter_name}: 'text' must be str, got {type(result.text).__name__}"
        )

    if not isinstance(result.raw, dict):
        diagnostics.append(
            f"{adapter_name}: 'raw' must be dict, got {type(result.raw).__name__}"
        )

    for field_name in ("tokens_sent", "tokens_received"):
        value: Any = getattr(result, field_name)
        if value is None:
            continue
        if not isinstance(value, int):
            diagnostics.append(
                f"{adapter_name}: '{field_name}' must be int|None, "
                f"got {type(value).__name__}"
            )
            continue
        if value < 0:
            diagnostics.append(
                f"{adapter_name}: '{field_name}' must be >= 0, got {value}"
            )

    return diagnostics
