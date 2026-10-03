"""``_format_run_totals_line`` (#271): the one-line run summary printed
after ``Run succeeded`` in the normal (non-quiet, non-json) CLI display.
"""

from __future__ import annotations

from circuitry.cli.app import _format_run_totals_line


def test_formats_all_fields() -> None:
    line = _format_run_totals_line(
        {
            "wall_time_s": 125.4,
            "effects_run": 7,
            "tokens_sent": 120,
            "tokens_received": 340,
            "cost_usd": 0.0123,
        }
    )
    assert line == "2m05.4s  ·  7 effects  ·  ↑120 ↓340 tok  ·  $0.0123"


def test_omits_cost_when_unknown() -> None:
    line = _format_run_totals_line(
        {
            "wall_time_s": 1.2,
            "effects_run": 1,
            "tokens_sent": 0,
            "tokens_received": 0,
            "cost_usd": None,
        }
    )
    assert line == "1.2s  ·  1 effects  ·  ↑0 ↓0 tok"


def test_none_for_missing_or_malformed_totals() -> None:
    assert _format_run_totals_line(None) is None
    assert _format_run_totals_line({}) is None
    assert _format_run_totals_line("not a dict") is None
