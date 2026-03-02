from __future__ import annotations

from typing import Any

from .base import GenerateResult, ImageResult


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


def validate_image_result(result: ImageResult, *, adapter_name: str) -> list[str]:
    """
    Validate adapter generate_image() output against normalized contract.

    Returns a list of diagnostics. Empty list means the contract is satisfied.
    """
    diagnostics: list[str] = []

    if not isinstance(result.raw, dict):
        diagnostics.append(
            f"{adapter_name}: 'raw' must be dict, got {type(result.raw).__name__}"
        )

    if result.image_bytes is not None and not isinstance(result.image_bytes, bytes):
        diagnostics.append(
            f"{adapter_name}: 'image_bytes' must be bytes|None, "
            f"got {type(result.image_bytes).__name__}"
        )

    if result.image_url is not None and not isinstance(result.image_url, str):
        diagnostics.append(
            f"{adapter_name}: 'image_url' must be str|None, "
            f"got {type(result.image_url).__name__}"
        )

    if result.image_bytes is None and result.image_url is None:
        diagnostics.append(
            f"{adapter_name}: at least one of 'image_bytes' or 'image_url' must be non-None"
        )

    return diagnostics
