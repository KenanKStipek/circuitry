"""Live-network integration tests for the ``cyberdiner`` adapter.

These are the only tests in the suite that make a real outbound request to
a live, metered CyberDiner network — a running expo (the job broker) plus
at least one cook serving the requested tier, all production infrastructure
someone pays for. They are marked both ``integration`` and ``live_network``.
CI runs ``pytest -m 'not integration'``, so the offline guarantee is
unchanged, and the documented local command
(``CIRCUITRY_RUN_INTEGRATION=1 pytest -m integration tests/integration/``)
is *not* enough to run these on its own: having ``CYBERDINER_EXPO_URL``/
``CYBERDINER_TOKEN`` set in the environment — e.g. the developer's own
shell, for unrelated reasons — must never be enough either. A third, clearly
deliberate opt-in, ``CIRCUITRY_LIVE_TESTS=1``, is required before the
``live`` fixture does anything but skip (#266 — one such accidental request
reached production during a QA pass that only set the credential env vars).

Run them by hand:

    export CIRCUITRY_LIVE_TESTS=1
    export CYBERDINER_EXPO_URL=https://expo.example.com
    export CYBERDINER_TOKEN=ck_...
    pytest tests/integration/test_cyberdiner_live.py -q

Optional knobs: ``CYBERDINER_TIER`` (default ``cheap``) and
``CYBERDINER_TIMEOUT_SECONDS`` (default 180) — a cold cook fleet can take
a while to pick up the first job. See ``docs/cyberdiner-demo-runbook.md``
for the full end-to-end demo.
"""

from __future__ import annotations

import os
from pathlib import Path
from typing import Any

import pytest

from circuitry.adapters.conformance import validate_generate_result
from circuitry.adapters.cyberdiner import CyberdinerAdapter
from circuitry.api import run_orchestration
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.redaction import REDACTED
from circuitry.cli.registry import resolve_bundled

LIVE_OPT_IN_ENV = "CIRCUITRY_LIVE_TESTS"
EXPO_URL_ENV = "CYBERDINER_EXPO_URL"
TOKEN_ENV = "CYBERDINER_TOKEN"
TIER_ENV = "CYBERDINER_TIER"
TIMEOUT_ENV = "CYBERDINER_TIMEOUT_SECONDS"

DEFAULT_TIER = "cheap"
DEFAULT_TIMEOUT_SECONDS = 180

# Candidate names for the bundled cyberdiner example (issue #6). When none
# resolve — e.g. this branch predates that example — the tests fall back to
# the inline orchestration below, so they never depend on it landing first.
BUNDLED_EXAMPLE_NAMES = ("learn/cyberdiner_hello", "learn/cyberdiner")

INLINE_ORCHESTRATION = """\
adapter: cyberdiner
model: {tier}
effects:
  - type: prompt
    name: greet
    template: "Reply with a short friendly greeting. Two words is plenty."
"""


class LiveSettings:
    """Env-sourced connection settings for a live CyberDiner network."""

    def __init__(self, *, expo_url: str, token: str, tier: str, timeout_seconds: int):
        self.expo_url = expo_url
        self.token = token
        self.tier = tier
        self.timeout_seconds = timeout_seconds

    def config(self) -> CircuitryConfig:
        return CircuitryConfig(
            default_adapter="cyberdiner",
            default_model=self.tier,
            runtime={
                "adapters": {
                    "cyberdiner": {
                        "expo_url": self.expo_url,
                        "token": self.token,
                        "default_tier": self.tier,
                        "timeout_seconds": self.timeout_seconds,
                    }
                }
            },
        )


@pytest.fixture
def live() -> LiveSettings:
    if (os.getenv(LIVE_OPT_IN_ENV) or "").strip() != "1":
        pytest.skip(
            f"Live CyberDiner tests spend a real request against production "
            f"and need an explicit opt-in: set {LIVE_OPT_IN_ENV}=1 (credentials "
            f"alone are not enough — see the module docstring)."
        )

    expo_url = (os.getenv(EXPO_URL_ENV) or "").strip()
    token = (os.getenv(TOKEN_ENV) or "").strip()
    if not expo_url or not token:
        pytest.skip(
            f"Live CyberDiner tests also need {EXPO_URL_ENV} (expo root URL) and "
            f"{TOKEN_ENV} (ck_... API key) in the environment, plus a cook "
            f"serving the requested tier. See docs/cyberdiner-demo-runbook.md."
        )

    raw_timeout = (os.getenv(TIMEOUT_ENV) or "").strip()
    try:
        timeout_seconds = int(raw_timeout) if raw_timeout else DEFAULT_TIMEOUT_SECONDS
    except ValueError:
        pytest.skip(f"{TIMEOUT_ENV}={raw_timeout!r} is not an integer.")

    return LiveSettings(
        expo_url=expo_url,
        token=token,
        tier=(os.getenv(TIER_ENV) or "").strip() or DEFAULT_TIER,
        timeout_seconds=timeout_seconds,
    )


def _orchestration_path(live: LiveSettings, tmp_path: Path) -> Path:
    """Prefer the bundled cyberdiner example; fall back to an inline one."""
    for name in BUNDLED_EXAMPLE_NAMES:
        bundled = resolve_bundled(name)
        if bundled is not None and bundled.exists():
            return bundled

    path = tmp_path / "cyberdiner_hello.yml"
    path.write_text(INLINE_ORCHESTRATION.format(tier=live.tier), encoding="utf-8")
    return path


def _completed_effect_values(state: dict[str, Any]) -> list[str]:
    """Non-empty prompt outputs under ``prime`` — the completions we asked for."""
    prime = state.get("prime")
    if not isinstance(prime, dict):
        return []
    return [
        node["value"]
        for node in prime.values()
        if isinstance(node, dict)
        and isinstance(node.get("value"), str)
        and node["value"].strip()
    ]


@pytest.mark.integration
@pytest.mark.live_network
def test_generate_returns_text_from_live_network(live: LiveSettings) -> None:
    """submit → cook serves → completion, straight through the adapter."""
    adapter = CyberdinerAdapter(
        expo_url=live.expo_url,
        token=live.token,
        default_tier=live.tier,
    )

    result = adapter.generate(
        model=live.tier,
        prompt="Reply with a short friendly greeting. Two words is plenty.",
        timeout_seconds=live.timeout_seconds,
    )

    assert validate_generate_result(result, adapter_name="cyberdiner") == []
    assert result.text.strip(), "Live cyberdiner job returned an empty completion."
    assert result.raw is not None
    # expo writes `completed`; cookd's client polls for `complete` — both real.
    assert result.raw.get("status") in ("complete", "completed")
    assert result.raw.get("tierName") == live.tier


@pytest.mark.integration
@pytest.mark.live_network
def test_run_orchestration_completes_against_live_network(
    live: LiveSettings, tmp_path: Path
) -> None:
    """The same path the demo runbook drives: a full orchestration run."""
    orchestration_path = _orchestration_path(live, tmp_path)

    result = run_orchestration(
        orchestration_path=orchestration_path,
        config=live.config(),
        raise_on_error=False,
    )

    assert result.ok is True, f"Live run failed: {result.error}"

    completions = _completed_effect_values(result.state)
    assert completions, (
        "Live run produced no non-empty prompt output under 'prime': "
        f"{result.state.get('prime')!r}"
    )

    embedded = result.state["runtime"]["effective_settings"]["runtime"]
    cyberdiner_cfg = embedded["adapters"]["cyberdiner"]
    assert cyberdiner_cfg["token"] == REDACTED
    assert cyberdiner_cfg["expo_url"] == live.expo_url


# ---------------------------------------------------------------------------
# #266 — credentials alone must never enable the tests above. Deliberately
# *not* marked integration/live_network: the whole point is to prove the
# ``live`` fixture skips before making any request, so this has to actually
# run in the default ``-m 'not integration'`` CI lane. ``live.__wrapped__``
# reaches the fixture's plain function body, bypassing pytest's fixture-
# injection machinery so it can be called directly like any other function.
# ---------------------------------------------------------------------------


def test_live_fixture_skips_on_credentials_alone(monkeypatch: pytest.MonkeyPatch) -> None:
    """Both credential env vars set, no CIRCUITRY_LIVE_TESTS: must skip."""
    monkeypatch.setenv(EXPO_URL_ENV, "https://expo.example.com")
    monkeypatch.setenv(TOKEN_ENV, "ck_test")
    monkeypatch.delenv(LIVE_OPT_IN_ENV, raising=False)

    with pytest.raises(pytest.skip.Exception) as exc_info:
        live.__wrapped__()

    assert LIVE_OPT_IN_ENV in str(exc_info.value)


def test_live_fixture_skips_when_opt_in_is_not_exactly_one(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv(EXPO_URL_ENV, "https://expo.example.com")
    monkeypatch.setenv(TOKEN_ENV, "ck_test")
    monkeypatch.setenv(LIVE_OPT_IN_ENV, "true")  # not "1"

    with pytest.raises(pytest.skip.Exception):
        live.__wrapped__()


def test_live_fixture_still_requires_credentials_after_opt_in(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv(LIVE_OPT_IN_ENV, "1")
    monkeypatch.delenv(EXPO_URL_ENV, raising=False)
    monkeypatch.delenv(TOKEN_ENV, raising=False)

    with pytest.raises(pytest.skip.Exception) as exc_info:
        live.__wrapped__()

    assert EXPO_URL_ENV in str(exc_info.value)


def test_live_fixture_proceeds_with_opt_in_and_credentials(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv(LIVE_OPT_IN_ENV, "1")
    monkeypatch.setenv(EXPO_URL_ENV, "https://expo.example.com")
    monkeypatch.setenv(TOKEN_ENV, "ck_test")
    monkeypatch.delenv(TIER_ENV, raising=False)
    monkeypatch.delenv(TIMEOUT_ENV, raising=False)

    settings = live.__wrapped__()

    assert settings.expo_url == "https://expo.example.com"
    assert settings.token == "ck_test"
