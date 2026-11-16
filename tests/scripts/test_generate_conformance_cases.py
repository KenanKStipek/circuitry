"""Tests for ``scripts/generate-conformance-cases.py``'s own T3 checks

(issue #778 step 6: the generator refuses a conformance case that pins
third-party text exactly)."""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path
from types import ModuleType

import pytest

REPO_ROOT = Path(__file__).resolve().parents[2]
SCRIPT_PATH = REPO_ROOT / "scripts" / "generate-conformance-cases.py"


def _load(path: Path, name: str) -> ModuleType:
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


gen = _load(SCRIPT_PATH, "generate_conformance_cases")


class _FakeCaseDir:
    name = "fake-case"


def test_error_not_pinning_third_party_text_accepts_a_recognised_shape() -> None:
    error = "CEL evaluation failed for 'state.x > 0': overflow"
    with pytest.raises(ValueError, match="matches tests/conformance/normalize"):
        gen._check_error_not_pinning_third_party_text(_FakeCaseDir(), {}, error)


def test_error_not_pinning_third_party_text_accepts_circuitry_owned_text() -> None:
    error = "CEL evaluation failed for 'value.missing_key == 1': no such key: 'missing_key'"
    gen._check_error_not_pinning_third_party_text(_FakeCaseDir(), {}, error)


def test_error_not_pinning_third_party_text_skipped_when_location_mode() -> None:
    error = "CEL evaluation failed for 'state.x > 0': overflow"
    gen._check_error_not_pinning_third_party_text(
        _FakeCaseDir(), {"error_compare": "location"}, error
    )


def test_third_party_text_not_redundant_accepts_a_wildcard_pattern() -> None:
    # The wildcard covers the catalogue's *entire* masked span (everything
    # after "curl: (28) ") -- genuinely not pinned, so this stays accepted.
    state = {"meta": {"error": "curl: (28) Operation timed out after 1012 milliseconds"}}
    specs = [{"location": "meta.error", "pattern": r"curl: \(28\) .*"}]
    gen._check_third_party_text_not_redundant(_FakeCaseDir(), specs, state)


def test_third_party_text_not_redundant_rejects_a_fully_literal_pattern() -> None:
    value = "curl: (28) Operation timed out after 1012 milliseconds"
    state = {"meta": {"error": value}}
    specs = [{"location": "meta.error", "pattern": __import__("re").escape(value)}]
    with pytest.raises(ValueError, match="pins text"):
        gen._check_third_party_text_not_redundant(_FakeCaseDir(), specs, state)


def test_third_party_text_not_redundant_rejects_a_pattern_that_wildcards_elsewhere_but_pins_the_masked_span() -> None:
    # The gap the old `pattern == re.escape(value)` check missed: wildcards
    # on either side of the masked span, but the span's own masked text is
    # still spelled out literally in the middle.
    value = "curl: (28) Operation timed out after 1012 milliseconds with 0 bytes received"
    state = {"meta": {"error": value}}
    specs = [
        {
            "location": "meta.error",
            "pattern": r"curl: \(28\) Operation timed out.*? with \d+ bytes received",
        }
    ]
    with pytest.raises(ValueError, match="pins text"):
        gen._check_third_party_text_not_redundant(_FakeCaseDir(), specs, state)


def test_third_party_text_not_redundant_skips_a_location_the_catalogue_does_not_recognise() -> None:
    state = {"meta": {"error": "effects[0].name: 'value' is reserved per schema XYZ"}}
    specs = [{"location": "meta.error", "pattern": "effects.*"}]
    gen._check_third_party_text_not_redundant(_FakeCaseDir(), specs, state)
