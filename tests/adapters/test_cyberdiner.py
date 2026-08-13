"""Tests for the CyberDiner adapter (submit/poll job broker).

Fakes ``urllib.request.urlopen`` per the idiom in
``tests/plugins/test_http.py``, and fakes ``time.monotonic``/``time.sleep``
so poll-cadence and timeout behavior is deterministic without real delays.
"""

from __future__ import annotations

import io
import json as _json
from pathlib import Path
from typing import Any
from urllib.error import HTTPError, URLError

import pytest

from circuitry.adapters.conformance import validate_generate_result
from circuitry.adapters.cyberdiner import CyberdinerAdapter, _resolve_tier
from circuitry.adapters.factory import build_adapter
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.redaction import REDACTED
from circuitry.cli.runtime_shim import RunRequest, run


# ---------------------------------------------------------------------------
# urlopen / clock fakes
# ---------------------------------------------------------------------------


class _FakeResponse:
    def __init__(self, *, status: int, body: bytes) -> None:
        self.status = status
        self._body = body
        self.headers: dict[str, str] = {}

    def read(self) -> bytes:
        return self._body

    def __enter__(self) -> "_FakeResponse":
        return self

    def __exit__(self, *args: Any) -> None:
        return None


class _ScriptedUrlopen:
    """Returns one canned response per call, in order; records each request."""

    def __init__(self, responses: list[dict[str, Any]]) -> None:
        self._responses = responses
        self.calls: list[dict[str, Any]] = []

    def __call__(self, req: Any, timeout: float = 0) -> _FakeResponse:
        self.calls.append(
            {
                "url": req.full_url,
                "method": req.get_method(),
                "headers": dict(req.header_items()),
                "body": req.data.decode("utf-8") if req.data else None,
                "timeout": timeout,
            }
        )
        idx = len(self.calls) - 1
        payload = self._responses[min(idx, len(self._responses) - 1)]
        return _FakeResponse(status=200, body=_json.dumps(payload).encode("utf-8"))


class _FakeClock:
    def __init__(self, start: float = 0.0) -> None:
        self.now = start

    def monotonic(self) -> float:
        return self.now

    def sleep(self, seconds: float) -> None:
        self.now += seconds


def _install_clock(monkeypatch: pytest.MonkeyPatch) -> _FakeClock:
    clock = _FakeClock()
    monkeypatch.setattr("circuitry.adapters.cyberdiner.time.monotonic", clock.monotonic)
    monkeypatch.setattr("circuitry.adapters.cyberdiner.time.sleep", clock.sleep)
    return clock


def _adapter(**overrides: Any) -> CyberdinerAdapter:
    fields: dict[str, Any] = {
        "expo_url": "https://expo.example.test",
        "token": "ck_test_token_123",
        "default_tier": "cheap",
        "poll_interval_ms": 500,
        "timeout_seconds": 30,
    }
    fields.update(overrides)
    return CyberdinerAdapter(**fields)


# ---------------------------------------------------------------------------
# Tier mapping
# ---------------------------------------------------------------------------


# CyberDiner's seeded tier names (expo `scripts/seed-tiers.sh`).
_REAL_TIERS = [
    "cheap",
    "fast-cheap",
    "fast",
    "good-cheap",
    "good",
    "good-fast",
    "alpha",
]


@pytest.mark.parametrize("tier", _REAL_TIERS)
def test_tier_mapping_real_network_tier_passes_through(tier: str) -> None:
    assert _resolve_tier(tier, "cheap") == tier


def test_tier_mapping_empty_model_uses_default_tier() -> None:
    assert _resolve_tier("", "good") == "good"
    assert _resolve_tier("   ", "good") == "good"


def test_tier_mapping_trims_surrounding_whitespace() -> None:
    assert _resolve_tier("  good-fast \n", "cheap") == "good-fast"


def test_tier_mapping_unknown_tier_passes_through_by_default() -> None:
    """The network owns the tier list — the client must not second-guess it."""
    assert _resolve_tier("tier-that-shipped-yesterday", "cheap") == (
        "tier-that-shipped-yesterday"
    )


def test_tier_mapping_no_tier_at_all_is_actionable() -> None:
    with pytest.raises(ValueError, match="default_tier") as exc:
        _resolve_tier("", "")
    assert "model:" in str(exc.value)


def test_tier_mapping_valid_tiers_opt_in_rejects_unknown_tier() -> None:
    with pytest.raises(ValueError, match="valid_tiers: cheap, good-fast") as exc:
        _resolve_tier("gpt-4o-mini", "cheap", ("cheap", "good-fast"))
    assert "gpt-4o-mini" in str(exc.value)


def test_tier_mapping_valid_tiers_opt_in_allows_listed_tier() -> None:
    assert _resolve_tier("good-fast", "cheap", ("cheap", "good-fast")) == "good-fast"


# ---------------------------------------------------------------------------
# Happy path
# ---------------------------------------------------------------------------


def test_generate_happy_path_submit_poll_complete(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _install_clock(monkeypatch)
    scripted = _ScriptedUrlopen(
        [
            {"job_id": "job-1", "status": "pending"},
            {"job_id": "job-1", "status": "running"},
            {"job_id": "job-1", "status": "complete", "result": "hello world"},
        ]
    )
    monkeypatch.setattr("urllib.request.urlopen", scripted)

    adapter = _adapter()
    result = adapter.generate(model="good-fast", prompt="hi", timeout_seconds=10)

    assert result.text == "hello world"
    assert result.raw == {"job_id": "job-1", "status": "complete", "tier": "good-fast"}
    assert result.tokens_sent is None
    assert result.tokens_received is None
    assert validate_generate_result(result, adapter_name="cyberdiner") == []

    # Submit call.
    submit_call = scripted.calls[0]
    assert submit_call["method"] == "POST"
    assert submit_call["url"] == "https://expo.example.test/beta/jobs"
    assert _json.loads(submit_call["body"]) == {"prompt": "hi", "tier": "good-fast"}

    # Poll calls hit the job_id endpoint.
    for call in scripted.calls[1:]:
        assert call["method"] == "GET"
        assert call["url"] == "https://expo.example.test/beta/jobs/job-1"


def test_generate_uses_default_tier_when_model_empty(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _install_clock(monkeypatch)
    scripted = _ScriptedUrlopen(
        [{"job_id": "job-2", "status": "complete", "result": "ok"}]
    )
    monkeypatch.setattr("urllib.request.urlopen", scripted)

    adapter = _adapter(default_tier="good")
    result = adapter.generate(model="", prompt="hi", timeout_seconds=10)

    assert result.raw["tier"] == "good"
    assert _json.loads(scripted.calls[0]["body"])["tier"] == "good"


def test_generate_uses_default_tier_when_model_is_whitespace(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _install_clock(monkeypatch)
    scripted = _ScriptedUrlopen(
        [{"job_id": "job-2b", "status": "complete", "result": "ok"}]
    )
    monkeypatch.setattr("urllib.request.urlopen", scripted)

    adapter = _adapter(default_tier="good-cheap")
    result = adapter.generate(model="   ", prompt="hi", timeout_seconds=10)

    assert result.raw["tier"] == "good-cheap"
    assert _json.loads(scripted.calls[0]["body"])["tier"] == "good-cheap"


def test_generate_unknown_tier_is_submitted_and_expo_400_surfaces_actionably(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Tier validation belongs to expo; its 400 is the actionable error."""
    _install_clock(monkeypatch)
    seen: list[str] = []

    def fake_urlopen(req: Any, timeout: float = 0) -> Any:
        seen.append(_json.loads(req.data.decode("utf-8"))["tier"])
        raise HTTPError(
            url=req.full_url,
            code=400,
            msg="Bad Request",
            hdrs=None,  # type: ignore[arg-type]
            fp=io.BytesIO(b'{"error":"unknown tier \'gpt-4o-mini\'"}'),
        )

    monkeypatch.setattr("urllib.request.urlopen", fake_urlopen)

    adapter = _adapter()
    with pytest.raises(RuntimeError, match="HTTP 400") as exc:
        adapter.generate(model="gpt-4o-mini", prompt="hi", timeout_seconds=10)

    assert seen == ["gpt-4o-mini"]
    assert "unknown tier" in str(exc.value)


def test_generate_valid_tiers_opt_in_raises_before_any_request(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    scripted = _ScriptedUrlopen([{"job_id": "job-x", "status": "complete"}])
    monkeypatch.setattr("urllib.request.urlopen", scripted)

    adapter = _adapter(valid_tiers=("cheap", "good-fast"))
    with pytest.raises(ValueError, match="valid_tiers: cheap, good-fast"):
        adapter.generate(model="not-a-tier", prompt="hi", timeout_seconds=10)
    assert scripted.calls == []


# ---------------------------------------------------------------------------
# Terminal-status errors
# ---------------------------------------------------------------------------


def test_generate_failed_status_raises_with_error_message(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _install_clock(monkeypatch)
    scripted = _ScriptedUrlopen(
        [
            {"job_id": "job-3", "status": "pending"},
            {"job_id": "job-3", "status": "failed", "error_message": "model exploded"},
        ]
    )
    monkeypatch.setattr("urllib.request.urlopen", scripted)

    adapter = _adapter()
    with pytest.raises(RuntimeError, match="model exploded"):
        adapter.generate(model="cheap", prompt="hi", timeout_seconds=10)


def test_generate_cancelled_status_raises(monkeypatch: pytest.MonkeyPatch) -> None:
    _install_clock(monkeypatch)
    scripted = _ScriptedUrlopen(
        [
            {"job_id": "job-4", "status": "pending"},
            {"job_id": "job-4", "status": "cancelled"},
        ]
    )
    monkeypatch.setattr("urllib.request.urlopen", scripted)

    adapter = _adapter()
    with pytest.raises(RuntimeError, match="cancelled"):
        adapter.generate(model="cheap", prompt="hi", timeout_seconds=10)


# ---------------------------------------------------------------------------
# Timeout / poll cadence
# ---------------------------------------------------------------------------


def test_poll_timeout_raises_naming_timeout_and_job_id(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    clock = _install_clock(monkeypatch)

    def always_running(req: Any, timeout: float = 0) -> _FakeResponse:
        if req.get_method() == "POST":
            body = {"job_id": "job-5", "status": "pending"}
        else:
            body = {"job_id": "job-5", "status": "running"}
        return _FakeResponse(status=200, body=_json.dumps(body).encode("utf-8"))

    monkeypatch.setattr("urllib.request.urlopen", always_running)

    adapter = _adapter(poll_interval_ms=500)
    with pytest.raises(RuntimeError, match=r"timed out after 1s.*job-5") as exc:
        adapter.generate(model="cheap", prompt="hi", timeout_seconds=1)
    assert clock.now >= 1.0
    assert "job-5" in str(exc.value)


def test_poll_cadence_honors_poll_interval(monkeypatch: pytest.MonkeyPatch) -> None:
    """Each poll must be spaced by poll_interval_ms, not busy-looped."""
    _install_clock(monkeypatch)
    calls: list[float] = []

    def always_running(req: Any, timeout: float = 0) -> _FakeResponse:
        import circuitry.adapters.cyberdiner as mod

        calls.append(mod.time.monotonic())
        if req.get_method() == "POST":
            body = {"job_id": "job-6", "status": "pending"}
        else:
            body = {"job_id": "job-6", "status": "running"}
        return _FakeResponse(status=200, body=_json.dumps(body).encode("utf-8"))

    monkeypatch.setattr("urllib.request.urlopen", always_running)

    adapter = _adapter(poll_interval_ms=500)
    with pytest.raises(RuntimeError, match="timed out"):
        adapter.generate(model="cheap", prompt="hi", timeout_seconds=2)

    # Poll calls (excluding the initial submit) should be ~0.5s apart.
    poll_times = calls[1:]
    for prev, nxt in zip(poll_times, poll_times[1:]):
        assert nxt - prev == pytest.approx(0.5)


# ---------------------------------------------------------------------------
# Auth header / missing config
# ---------------------------------------------------------------------------


def test_bearer_header_sent_with_token(monkeypatch: pytest.MonkeyPatch) -> None:
    _install_clock(monkeypatch)
    scripted = _ScriptedUrlopen(
        [{"job_id": "job-7", "status": "complete", "result": "ok"}]
    )
    monkeypatch.setattr("urllib.request.urlopen", scripted)

    adapter = _adapter(token="ck_super_secret")
    adapter.generate(model="cheap", prompt="hi", timeout_seconds=10)

    headers = {k.lower(): v for k, v in scripted.calls[0]["headers"].items()}
    assert headers.get("authorization") == "Bearer ck_super_secret"


def test_missing_token_raises_at_generate(monkeypatch: pytest.MonkeyPatch) -> None:
    adapter = _adapter(token="")
    with pytest.raises(RuntimeError, match="token not configured"):
        adapter.generate(model="cheap", prompt="hi", timeout_seconds=10)


def test_missing_expo_url_raises_at_generate(monkeypatch: pytest.MonkeyPatch) -> None:
    adapter = _adapter(expo_url="")
    with pytest.raises(RuntimeError, match="expo_url not configured"):
        adapter.generate(model="cheap", prompt="hi", timeout_seconds=10)


def test_http_error_wrapped_in_runtime_error(monkeypatch: pytest.MonkeyPatch) -> None:
    def fake_urlopen(req: Any, timeout: float = 0) -> Any:
        raise HTTPError(
            url=req.full_url,
            code=500,
            msg="Internal Server Error",
            hdrs=None,  # type: ignore[arg-type]
            fp=io.BytesIO(b"boom"),
        )

    monkeypatch.setattr("urllib.request.urlopen", fake_urlopen)

    adapter = _adapter()
    with pytest.raises(RuntimeError, match="HTTP 500") as exc:
        adapter.generate(model="cheap", prompt="hi", timeout_seconds=10)
    assert exc.value.__cause__ is not None


def test_connection_error_wrapped_in_runtime_error(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    def fake_urlopen(req: Any, timeout: float = 0) -> Any:
        raise URLError("Name or service not known")

    monkeypatch.setattr("urllib.request.urlopen", fake_urlopen)

    adapter = _adapter()
    with pytest.raises(RuntimeError, match="Name or service not known") as exc:
        adapter.generate(model="cheap", prompt="hi", timeout_seconds=10)
    assert exc.value.__cause__ is not None


# ---------------------------------------------------------------------------
# check()
# ---------------------------------------------------------------------------


def test_check_missing_token_is_actionable(monkeypatch: pytest.MonkeyPatch) -> None:
    scripted = _ScriptedUrlopen([{"jobs": []}])
    monkeypatch.setattr("urllib.request.urlopen", scripted)

    r = _adapter(token="").check()
    assert r.ok is False
    assert "env:CYBERDINER token (runtime.adapters.cyberdiner.token)" in r.missing


def test_check_missing_expo_url_is_actionable() -> None:
    r = _adapter(expo_url="", token="").check()
    assert r.ok is False
    assert "env:CYBERDINER expo_url (runtime.adapters.cyberdiner.expo_url)" in r.missing


def test_check_unreachable_expo_reports_host(monkeypatch: pytest.MonkeyPatch) -> None:
    def fake_urlopen(req: Any, timeout: float = 0) -> Any:
        raise URLError("Connection refused")

    monkeypatch.setattr("urllib.request.urlopen", fake_urlopen)

    r = _adapter().check()
    assert r.ok is False
    assert "host:https://expo.example.test" in r.missing


def test_check_ok_when_reachable(monkeypatch: pytest.MonkeyPatch) -> None:
    scripted = _ScriptedUrlopen([{"jobs": []}])
    monkeypatch.setattr("urllib.request.urlopen", scripted)

    r = _adapter().check()
    assert r.ok is True
    assert r.missing == []


def test_check_ok_when_server_answers_with_http_error(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A 401/404 still proves the host is reachable — only connection failures count."""

    def fake_urlopen(req: Any, timeout: float = 0) -> Any:
        raise HTTPError(
            url=req.full_url,
            code=401,
            msg="Unauthorized",
            hdrs=None,  # type: ignore[arg-type]
            fp=io.BytesIO(b""),
        )

    monkeypatch.setattr("urllib.request.urlopen", fake_urlopen)

    r = _adapter().check()
    assert r.ok is True


# ---------------------------------------------------------------------------
# Factory build + config passthrough
# ---------------------------------------------------------------------------


def test_factory_builds_cyberdiner_adapter() -> None:
    adapter = build_adapter(adapter_name="cyberdiner", runtime={})
    assert isinstance(adapter, CyberdinerAdapter)
    assert adapter.name == "cyberdiner"


def test_factory_cyberdiner_config_passthrough() -> None:
    runtime = {
        "adapters": {
            "cyberdiner": {
                "expo_url": "https://expo.internal",
                "token": "ck_abc123",
                "default_tier": "fast",
                "valid_tiers": ["cheap", " good-fast ", ""],
                "poll_interval_ms": 250,
                "timeout_seconds": 45,
            }
        }
    }
    adapter = build_adapter(adapter_name="cyberdiner", runtime=runtime)
    assert isinstance(adapter, CyberdinerAdapter)
    assert adapter.expo_url == "https://expo.internal"
    assert adapter.token == "ck_abc123"
    assert adapter.default_tier == "fast"
    assert adapter.valid_tiers == ("cheap", "good-fast")
    assert adapter.poll_interval_ms == 250
    assert adapter.timeout_seconds == 45


def test_factory_cyberdiner_defaults_when_config_absent() -> None:
    adapter = build_adapter(adapter_name="cyberdiner", runtime={})
    assert isinstance(adapter, CyberdinerAdapter)
    assert adapter.expo_url == ""
    assert adapter.token == ""
    assert adapter.default_tier == "cheap"
    assert adapter.valid_tiers == ()
    assert adapter.poll_interval_ms == 500
    assert adapter.timeout_seconds == 30


# ---------------------------------------------------------------------------
# Redaction
# ---------------------------------------------------------------------------

_NOOP_ORCH = (
    """
effects:
  - type: dynamic
    name: noop
    flow: chain
    effects: []
""".strip()
    + "\n"
)


def test_token_is_redacted_in_effective_settings(tmp_path: Path) -> None:
    cfg = CircuitryConfig(
        default_adapter="cyberdiner",
        runtime={
            "adapters": {
                "cyberdiner": {
                    "expo_url": "https://expo.example.test",
                    "token": "ck_super_secret_do_not_leak",
                }
            }
        },
    )
    orch_path = tmp_path / "noop.yml"
    orch_path.write_text(_NOOP_ORCH, encoding="utf-8")

    result = run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=None,
            dry_run=True,
            validate_only=True,
            config=cfg,
        )
    )
    assert result.ok is True

    embedded = result.state["runtime"]["effective_settings"]["runtime"]
    cyberdiner_cfg = embedded["adapters"]["cyberdiner"]
    assert cyberdiner_cfg["token"] == REDACTED
    assert cyberdiner_cfg["expo_url"] == "https://expo.example.test"
