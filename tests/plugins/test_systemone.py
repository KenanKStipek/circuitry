"""Tests for the systemone tool plugin (#580).

Every request goes to a fake server on 127.0.0.1 that replays scripted
responses; nothing reaches the network.
"""

from __future__ import annotations

import hashlib
import json
import socket
import threading
import urllib.request
from collections.abc import Iterator
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any
from unittest.mock import MagicMock

import pytest
import yaml

from circuitry.adapters._retry import classify_exception
from circuitry.cli.config import CircuitryConfig, resolve_config
from circuitry.cli.effective_settings import resolve_effective_settings
from circuitry.cli.runtime_shim import RunRequest, run
from circuitry.core.compiler import compile_orchestration
from circuitry.core.prompt import RetryPolicyDef
from circuitry.core.store import Store
from circuitry.core.tool import ToolDefinition, ToolRuntime
from circuitry.plugins import build_plugin
from circuitry.plugins import systemone as systemone_module
from circuitry.plugins.base import validate_tool_result
from circuitry.plugins.systemone import SystemOnePlugin, is_loopback_host

KEY = "sys1-test-key-0123456789abcdef"
STATE_MARKER = "UNIQUE-STATE-TEXT-7f3a"

QUESTIONS: dict[str, Any] = {
    "spam": {"type": "noul", "instructions": "Is it spam?"},
    "tone": {
        "type": "choice",
        "instructions": "Which tone?",
        "criteria": {"formal": "Formal", "casual": None},
    },
    "quality": {
        "type": "score",
        "instructions": "Rate it.",
        "criteria": ["bad", "ok", "good"],
    },
}

ANSWERS: dict[str, Any] = {
    "spam": {"type": "noul", "noul": 0.1},
    "tone": {
        "type": "choice",
        "choice": "formal",
        "probabilities": {"formal": 0.7, "casual": 0.3},
        "confidence": 0.8,
    },
    "quality": {
        "type": "score",
        "score": 1.5,
        "legend": {"0": "bad", "1": "ok", "2": "good"},
        "probabilities": {"0": 0.1, "1": 0.5, "2": 0.4},
        "confidence": 0.7,
    },
}

USAGE = {"input_tokens": 120, "output_tokens": 15}


# ---------------------------------------------------------------------------
# Fake server
# ---------------------------------------------------------------------------


@dataclass
class _Reply:
    status: int = 200
    body: bytes = b""
    headers: dict[str, str] = field(default_factory=dict)


@dataclass
class _Seen:
    method: str
    path: str
    headers: dict[str, str]
    body: bytes

    def json(self) -> Any:
        return json.loads(self.body)


class FakeServer:
    """A 127.0.0.1 server that answers each request with the next scripted reply."""

    def __init__(self) -> None:
        self.requests: list[_Seen] = []
        self._replies: list[_Reply] = []
        self._lock = threading.Lock()
        outer = self

        class Handler(BaseHTTPRequestHandler):
            def _handle(self) -> None:
                length = int(self.headers.get("Content-Length") or 0)
                body = self.rfile.read(length) if length else b""
                with outer._lock:
                    outer.requests.append(
                        _Seen(
                            method=self.command,
                            path=self.path,
                            headers={k.lower(): v for k, v in self.headers.items()},
                            body=body,
                        )
                    )
                    reply = outer._replies.pop(0) if outer._replies else _Reply(500, b"unscripted")
                self.send_response(reply.status)
                for name, value in reply.headers.items():
                    self.send_header(name, value)
                self.send_header("Content-Length", str(len(reply.body)))
                self.end_headers()
                self.wfile.write(reply.body)

            do_POST = _handle
            do_GET = _handle

            def log_message(self, format: str, *args: Any) -> None:
                return None

        self._httpd = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self._thread = threading.Thread(target=self._httpd.serve_forever, daemon=True)

    @property
    def url(self) -> str:
        return f"http://127.0.0.1:{self._httpd.server_address[1]}"

    def start(self) -> None:
        self._thread.start()

    def stop(self) -> None:
        self._httpd.shutdown()
        self._httpd.server_close()
        self._thread.join(timeout=5)

    def reply(
        self,
        status: int = 200,
        body: Any = None,
        headers: dict[str, str] | None = None,
    ) -> None:
        if isinstance(body, bytes):
            payload = body
        elif isinstance(body, str):
            payload = body.encode("utf-8")
        else:
            payload = json.dumps(body if body is not None else {}).encode("utf-8")
        with self._lock:
            self._replies.append(_Reply(status, payload, headers or {}))

    def reply_answers(self, answers: dict[str, Any] | None = None) -> None:
        self.reply(
            200,
            {"model": "typesafe-1", "answers": answers if answers is not None else ANSWERS, "usage": USAGE},
        )


@pytest.fixture
def server() -> Iterator[FakeServer]:
    fake = FakeServer()
    fake.start()
    try:
        yield fake
    finally:
        fake.stop()


@pytest.fixture(autouse=True)
def _clean_env(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("SYSTEMONE_TEST_KEY", raising=False)
    monkeypatch.delenv("CIRCUITRY_SYSTEMONE_URL", raising=False)


def _params(**overrides: Any) -> dict[str, Any]:
    params: dict[str, Any] = {
        "model": "typesafe-1",
        "state": {"text": "hello"},
        "questions": QUESTIONS,
    }
    params.update(overrides)
    return params


def _plugin(server: FakeServer, **kwargs: Any) -> SystemOnePlugin:
    return SystemOnePlugin(base_url=server.url, **kwargs)


def _closed_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def _tool_runtime(
    server_url: str, *, params: dict[str, Any] | None = None, retries: int = 1, **plugin_cfg: Any
) -> tuple[ToolRuntime, Store]:
    defn = ToolDefinition(
        name="decide",
        provider="systemone",
        params=params if params is not None else _params(),
        retries=RetryPolicyDef(max_attempts=retries, backoff_ms=1),
    )
    runtime = ToolRuntime(
        defn,
        runtime_config={"plugins": {"systemone": {"base_url": server_url, **plugin_cfg}}},
    )
    return runtime, Store({})


# ---------------------------------------------------------------------------
# Round trips
# ---------------------------------------------------------------------------


def test_round_trip_noul_choice_and_score(server: FakeServer) -> None:
    server.reply_answers()
    result = _plugin(server).execute(params=_params(), timeout_seconds=10)

    assert result.ok is True
    assert result.value == ANSWERS
    assert validate_tool_result(result, plugin_name="systemone") == []

    (seen,) = server.requests
    assert (seen.method, seen.path) == ("POST", "/v1/systemone")
    assert seen.headers["content-type"] == "application/json"
    assert "authorization" not in seen.headers
    assert not seen.body.startswith(b"\xef\xbb\xbf")
    assert seen.json() == {
        "model": "typesafe-1",
        "state": {"text": "hello"},
        "questions": QUESTIONS,
    }

    state_bytes = json.dumps({"text": "hello"}, separators=(",", ":")).encode()
    assert result.raw == {
        "model": "typesafe-1",
        "usage": USAGE,
        "status": 200,
        "state_sha256": hashlib.sha256(state_bytes).hexdigest(),
        "state_bytes": len(state_bytes),
        "question_ids": ["spam", "tone", "quality"],
    }
    assert (result.tokens_sent, result.tokens_received) == (120, 15)


def test_each_question_type_round_trips_alone(server: FakeServer) -> None:
    for qid in QUESTIONS:
        server.reply_answers({qid: ANSWERS[qid]})
        result = _plugin(server).execute(
            params=_params(questions={qid: QUESTIONS[qid]}), timeout_seconds=10
        )
        assert result.ok is True, (qid, result.stderr)
        assert result.value == {qid: ANSWERS[qid]}


def test_noul_criteria_and_a_string_state_are_sent_as_written(server: FakeServer) -> None:
    questions = {
        "q": {
            "type": "noul",
            "instructions": {"rubric": ["a", "b"]},
            "criteria": {"true": "yes", "false": ["no"]},
        }
    }
    server.reply_answers({"q": {"type": "noul", "noul": 1}})
    result = _plugin(server).execute(
        params=_params(state="just text", questions=questions), timeout_seconds=10
    )
    assert result.ok is True
    assert server.requests[0].json()["questions"] == questions
    assert server.requests[0].json()["state"] == "just text"


def test_tool_effect_records_meta_status_tokens_and_raw(server: FakeServer) -> None:
    server.reply_answers()
    runtime, store = _tool_runtime(server.url)
    runtime.execute(store=store, ctx={})

    node = store.state["decide"]
    assert node["value"] == ANSWERS
    meta = node["meta"]
    assert meta["error"] is None
    assert meta["status_code"] == 200
    assert (meta["tokens_sent"], meta["tokens_received"]) == (120, 15)
    assert meta["raw"]["question_ids"] == ["spam", "tone", "quality"]
    assert "hello" not in json.dumps(meta["raw"])


def test_factory_builds_the_plugin_from_runtime_config() -> None:
    plugin = build_plugin(
        plugin_name="systemone",
        runtime={
            "plugins": {
                "systemone": {
                    "base_url": "http://localhost:9",
                    "api_key_env": "SYSTEMONE_TEST_KEY",
                    "allow_remote": "false",
                }
            }
        },
    )
    assert isinstance(plugin, SystemOnePlugin)
    assert plugin.base_url == "http://localhost:9"
    assert plugin.api_key_env == "SYSTEMONE_TEST_KEY"
    assert plugin.allow_remote is False


# ---------------------------------------------------------------------------
# Answer validation
# ---------------------------------------------------------------------------


def _with(qid: str, **changes: Any) -> dict[str, Any]:
    answers = json.loads(json.dumps(ANSWERS))
    answers[qid].update(changes)
    return answers


def _without(qid: str) -> dict[str, Any]:
    answers = json.loads(json.dumps(ANSWERS))
    del answers[qid]
    return answers


_NAN = float("nan")

INVALID_ANSWERS = [
    pytest.param(_without("tone"), "tone", "no answer", id="missing-answer"),
    pytest.param(
        {**ANSWERS, "spam": {"type": "choice", "choice": "x", "probabilities": {"x": 1}}},
        "spam",
        "differs from the question's 'noul'",
        id="type-mismatch",
    ),
    pytest.param(_with("tone", choice="formel"), "tone", "not one of the question's options", id="undefined-option"),
    pytest.param(
        _with("tone", probabilities={"formal": _NAN, "casual": 1.0}), "tone", "not a number in [0, 1]", id="nan-probability"
    ),
    pytest.param(
        _with("tone", probabilities={"formal": float("inf"), "casual": 0.0}),
        "tone",
        "not a number in [0, 1]",
        id="infinite-probability",
    ),
    pytest.param(
        _with("tone", probabilities={"formal": 1.5, "casual": -0.5}), "tone", "not a number in [0, 1]", id="probability-out-of-range"
    ),
    pytest.param(
        _with("tone", probabilities={"formal": 0.5, "casual": 0.3}), "tone", "sum to 0.8", id="probabilities-sum-short"
    ),
    pytest.param(
        _with("tone", probabilities={"formal": 0.7, "casual": 0.3, "stern": 0.0}),
        "tone",
        "'stern'",
        id="probability-for-undefined-option",
    ),
    pytest.param(_with("quality", probabilities={"0": 0.5, "1": 0.2, "2": 0.1}), "quality", "sum to 0.8", id="score-probabilities-sum"),
    pytest.param(_with("spam", noul=1.2), "spam", "not a number in [0, 1]", id="noul-too-high"),
    pytest.param(_with("spam", noul=-0.1), "spam", "not a number in [0, 1]", id="noul-negative"),
    pytest.param(_with("spam", noul="0.5"), "spam", "not a number in [0, 1]", id="noul-string"),
    pytest.param(_with("quality", score=2.5), "quality", "not a number in [0, 2]", id="score-above-rubric"),
    pytest.param(_with("quality", score=-1), "quality", "not a number in [0, 2]", id="score-below-rubric"),
    pytest.param(_with("spam", confidence=1.5), "spam", "confidence", id="confidence-out-of-range"),
    pytest.param({**ANSWERS, "extra": {"type": "noul", "noul": 0.5}}, "extra", "no such question was asked", id="unasked-answer"),
]


@pytest.mark.parametrize(("answers", "qid", "reason"), INVALID_ANSWERS)
def test_a_mismatched_answer_fails_the_effect_naming_the_question(
    server: FakeServer, answers: dict[str, Any], qid: str, reason: str
) -> None:
    server.reply_answers(answers)
    result = _plugin(server).execute(params=_params(), timeout_seconds=10)

    assert result.ok is False
    assert result.value is None
    assert f"'{qid}'" in (result.stderr or "")
    assert reason in (result.stderr or "")
    assert result.raw["error_kind"] == "invalid_answer"
    assert result.raw["status"] == 200


def test_a_mismatched_answer_fails_without_a_retry(server: FakeServer) -> None:
    server.reply_answers(_without("tone"))
    server.reply_answers()
    runtime, store = _tool_runtime(server.url, retries=3)
    with pytest.raises(RuntimeError, match="'tone'"):
        runtime.execute(store=store, ctx={})
    assert len(server.requests) == 1
    assert store.state["decide"]["meta"]["raw"]["error_kind"] == "invalid_answer"


def test_a_confidence_is_optional_and_probabilities_may_sum_within_tolerance(
    server: FakeServer,
) -> None:
    answers = json.loads(json.dumps(ANSWERS))
    del answers["tone"]["confidence"]
    answers["tone"]["probabilities"] = {"formal": 0.70, "casual": 0.295}
    server.reply_answers(answers)
    assert _plugin(server).execute(params=_params(), timeout_seconds=10).ok is True


@pytest.mark.parametrize(
    "body",
    [b"not json", b"[]", b'{"model": "m"}'],
    ids=["not-json", "not-an-object", "no-answers"],
)
def test_a_malformed_response_fails_as_invalid_response(server: FakeServer, body: bytes) -> None:
    server.reply(200, body)
    result = _plugin(server).execute(params=_params(), timeout_seconds=10)
    assert result.ok is False
    assert result.raw["error_kind"] == "invalid_response"


# ---------------------------------------------------------------------------
# HTTP errors, retries, redirects, connection failures
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("status", "body", "kind"),
    [
        (400, "bad request: max_tokens_exceeded", "context_exceeded"),
        (400, "bad request: something else", "invalid"),
        (401, '{"error": "unauthorized"}', "unauthorized"),
        (413, "too big", "too_large"),
        (415, "wrong type", "unsupported_media_type"),
        (422, '{"detail": "invalid questions"}', "invalid"),
        (429, "slow down", "rate_limited"),
        (529, "overloaded", "overloaded"),
        (503, "down", "server_error"),
    ],
)
def test_http_errors_fail_with_an_error_kind(
    server: FakeServer, status: int, body: str, kind: str
) -> None:
    server.reply(status, body)
    result = _plugin(server).execute(params=_params(), timeout_seconds=10)

    assert result.ok is False
    assert result.value is None
    assert result.raw["status"] == status
    assert result.raw["error_kind"] == kind
    assert f"HTTP {status}" in (result.stderr or "")


def test_422_fails_without_a_retry(server: FakeServer) -> None:
    server.reply(422, '{"detail": "invalid questions"}')
    server.reply_answers()
    runtime, store = _tool_runtime(server.url, retries=3)

    with pytest.raises(RuntimeError, match="invalid questions"):
        runtime.execute(store=store, ctx={})

    assert len(server.requests) == 1
    meta = store.state["decide"]["meta"]
    assert meta["status_code"] == 422
    assert meta["raw"]["error_kind"] == "invalid"


def test_429_then_200_is_retried(server: FakeServer) -> None:
    server.reply(429, "slow down", headers={"Retry-After": "0"})
    server.reply_answers()
    runtime, store = _tool_runtime(server.url, retries=3)

    runtime.execute(store=store, ctx={})

    assert len(server.requests) == 2
    node = store.state["decide"]
    assert node["value"] == ANSWERS
    assert node["meta"]["retries_used"] == 1
    assert node["meta"]["error"] is None


@pytest.mark.parametrize("status", [408, 503, 529])
def test_other_retryable_statuses_are_retried(server: FakeServer, status: int) -> None:
    server.reply(status, "later")
    server.reply_answers()
    runtime, store = _tool_runtime(server.url, retries=2)
    runtime.execute(store=store, ctx={})
    assert len(server.requests) == 2


def test_retry_after_is_kept_for_the_retry_loop(server: FakeServer) -> None:
    server.reply(429, "slow down", headers={"Retry-After": "7"})
    result = _plugin(server).execute(params=_params(), timeout_seconds=10)
    assert result.raw["headers"] == {"retry-after": "7"}


@pytest.mark.parametrize("status", [301, 302, 307, 308])
def test_a_redirect_fails_the_effect_and_is_never_followed(
    server: FakeServer, monkeypatch: pytest.MonkeyPatch, status: int
) -> None:
    target = FakeServer()
    target.start()
    try:
        monkeypatch.setenv("SYSTEMONE_TEST_KEY", KEY)
        server.reply(status, "", headers={"Location": target.url + "/stolen"})
        target.reply_answers()
        result = _plugin(server, api_key_env="SYSTEMONE_TEST_KEY").execute(
            params=_params(), timeout_seconds=10
        )

        assert result.ok is False
        assert f"HTTP {status}" in (result.stderr or "")
        assert result.raw["status"] == status
        assert result.raw["error_kind"] == "redirect"
        assert target.requests == []
        assert len(server.requests) == 1
        assert KEY not in json.dumps(result.raw) + (result.stderr or "")
    finally:
        target.stop()


def test_a_redirect_is_not_retried(server: FakeServer) -> None:
    server.reply(302, "", headers={"Location": server.url + "/elsewhere"})
    server.reply_answers()
    runtime, store = _tool_runtime(server.url, retries=3)
    with pytest.raises(RuntimeError, match="HTTP 302"):
        runtime.execute(store=store, ctx={})
    assert len(server.requests) == 1


def test_a_refused_connection_names_the_url_and_is_retryable() -> None:
    url = f"http://127.0.0.1:{_closed_port()}"
    with pytest.raises(RuntimeError) as excinfo:
        SystemOnePlugin(base_url=url).execute(params=_params(), timeout_seconds=5)

    assert str(excinfo.value) == f"systemone server not reachable at {url}"
    assert classify_exception(excinfo.value).retryable is True


def test_a_timeout_names_the_url_and_the_seconds_and_is_retryable() -> None:
    with socket.socket() as silent:
        silent.bind(("127.0.0.1", 0))
        silent.listen(1)
        url = f"http://127.0.0.1:{silent.getsockname()[1]}"
        with pytest.raises(RuntimeError) as excinfo:
            SystemOnePlugin(base_url=url).execute(params=_params(), timeout_seconds=1)

    assert str(excinfo.value) == f"systemone request to {url} timed out after 1s"
    assert classify_exception(excinfo.value).retryable is True


def test_a_refused_connection_is_retried_by_the_tool_effect() -> None:
    url = f"http://127.0.0.1:{_closed_port()}"
    runtime, store = _tool_runtime(url, retries=2)
    with pytest.raises(RuntimeError, match="not reachable"):
        runtime.execute(store=store, ctx={})
    assert store.state["decide"]["meta"]["retries_used"] == 1


def test_a_trailing_slash_on_base_url_is_ignored(server: FakeServer) -> None:
    server.reply_answers()
    SystemOnePlugin(base_url=server.url + "/").execute(params=_params(), timeout_seconds=10)
    assert server.requests[0].path == "/v1/systemone"


# ---------------------------------------------------------------------------
# Request limits
# ---------------------------------------------------------------------------


def test_a_body_over_the_limit_is_refused_before_sending(
    server: FakeServer, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(systemone_module, "MAX_BODY_BYTES", 2000)
    result = _plugin(server).execute(
        params=_params(state="x" * 5000), timeout_seconds=10
    )

    assert result.ok is False
    assert result.raw["error_kind"] == "too_large"
    assert "status" not in result.raw
    assert server.requests == []
    assert "x" * 100 not in json.dumps(result.raw) + (result.stderr or "")


def test_too_large_is_not_retried(server: FakeServer, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(systemone_module, "MAX_BODY_BYTES", 2000)
    runtime, store = _tool_runtime(server.url, params=_params(state="x" * 5000), retries=3)
    with pytest.raises(RuntimeError, match="over the"):
        runtime.execute(store=store, ctx={})
    assert store.state["decide"]["meta"].get("retries_used") is None


def test_state_nested_past_the_limit_is_refused(server: FakeServer) -> None:
    state: Any = "leaf"
    for _ in range(70):
        state = [state]
    with pytest.raises(ValueError, match="64 levels"):
        _plugin(server).execute(params=_params(state=state), timeout_seconds=10)
    assert server.requests == []


def test_state_nested_exactly_to_the_limit_is_sent(server: FakeServer) -> None:
    state: Any = "leaf"
    for _ in range(63):  # the body's own object is level 1
        state = [state]
    server.reply_answers()
    assert _plugin(server).execute(params=_params(state=state), timeout_seconds=10).ok is True
    with pytest.raises(ValueError, match="64 levels"):
        _plugin(server).execute(params=_params(state=[state]), timeout_seconds=10)


@pytest.mark.parametrize("bad", [float("nan"), float("inf"), {"a": [float("-inf")]}])
def test_non_finite_numbers_in_the_state_are_refused(server: FakeServer, bad: Any) -> None:
    with pytest.raises(ValueError, match="cannot be sent as JSON"):
        _plugin(server).execute(params=_params(state=bad), timeout_seconds=10)
    assert server.requests == []


def test_non_ascii_state_is_sent_as_utf8_without_a_bom(server: FakeServer) -> None:
    server.reply_answers()
    _plugin(server).execute(params=_params(state={"t": "héllo ✓"}), timeout_seconds=10)
    body = server.requests[0].body
    assert "héllo ✓".encode() in body
    assert not body.startswith(b"\xef\xbb\xbf")


# ---------------------------------------------------------------------------
# state / state_file
# ---------------------------------------------------------------------------


def _file_params(path: Path, within: Path | str, **overrides: Any) -> dict[str, Any]:
    params = _params(state_file=str(path), within=str(within))
    del params["state"]
    params.update(overrides)
    return params


def test_state_file_json_is_parsed_and_hashed(server: FakeServer, tmp_path: Path) -> None:
    doc = tmp_path / "doc.json"
    doc.write_text(json.dumps({"body": STATE_MARKER, "n": [1, 2]}), encoding="utf-8")
    server.reply_answers()

    result = _plugin(server).execute(params=_file_params(doc, tmp_path), timeout_seconds=10)

    assert result.ok is True
    assert server.requests[0].json()["state"] == {"body": STATE_MARKER, "n": [1, 2]}
    sent = json.dumps({"body": STATE_MARKER, "n": [1, 2]}, separators=(",", ":")).encode()
    assert result.raw["state_sha256"] == hashlib.sha256(sent).hexdigest()
    assert result.raw["state_bytes"] == len(sent)
    assert STATE_MARKER not in json.dumps(result.raw)


def test_state_file_other_than_json_is_sent_as_a_string(server: FakeServer, tmp_path: Path) -> None:
    doc = tmp_path / "doc.md"
    doc.write_text(f"# Title\n\n{STATE_MARKER} \"quoted\"\n", encoding="utf-8")
    server.reply_answers()

    result = _plugin(server).execute(params=_file_params(doc, tmp_path), timeout_seconds=10)

    assert result.ok is True
    assert server.requests[0].json()["state"] == f"# Title\n\n{STATE_MARKER} \"quoted\"\n"
    assert result.raw["state_sha256"] == hashlib.sha256(
        json.dumps(f"# Title\n\n{STATE_MARKER} \"quoted\"\n", ensure_ascii=False, separators=(",", ":")).encode()
    ).hexdigest()


def test_a_json_file_that_does_not_parse_fails_the_effect(server: FakeServer, tmp_path: Path) -> None:
    doc = tmp_path / "bad.json"
    doc.write_text(f'{{"body": "{STATE_MARKER}"', encoding="utf-8")
    with pytest.raises(ValueError, match="not valid JSON") as excinfo:
        _plugin(server).execute(params=_file_params(doc, tmp_path), timeout_seconds=10)
    assert STATE_MARKER not in str(excinfo.value)
    assert server.requests == []


def test_a_json_file_with_a_non_finite_number_fails_the_effect(
    server: FakeServer, tmp_path: Path
) -> None:
    doc = tmp_path / "nan.json"
    doc.write_text('{"x": NaN}', encoding="utf-8")
    with pytest.raises(ValueError, match="not valid JSON"):
        _plugin(server).execute(params=_file_params(doc, tmp_path), timeout_seconds=10)


def test_a_file_that_is_not_utf8_fails_the_effect(server: FakeServer, tmp_path: Path) -> None:
    doc = tmp_path / "latin.txt"
    doc.write_bytes(b"caf\xe9")
    with pytest.raises(ValueError, match="not valid UTF-8"):
        _plugin(server).execute(params=_file_params(doc, tmp_path), timeout_seconds=10)


def test_a_missing_state_file_fails_the_effect(server: FakeServer, tmp_path: Path) -> None:
    with pytest.raises(ValueError, match="cannot be read"):
        _plugin(server).execute(params=_file_params(tmp_path / "nope.json", tmp_path), timeout_seconds=10)


def test_state_file_is_read_at_dispatch(server: FakeServer, tmp_path: Path) -> None:
    doc = tmp_path / "doc.txt"
    doc.write_text("first", encoding="utf-8")
    plugin = _plugin(server)
    params = _file_params(doc, tmp_path)
    server.reply_answers()
    plugin.execute(params=params, timeout_seconds=10)
    doc.write_text("second", encoding="utf-8")
    server.reply_answers()
    plugin.execute(params=params, timeout_seconds=10)
    assert [r.json()["state"] for r in server.requests] == ["first", "second"]


def test_a_state_file_outside_within_is_refused(server: FakeServer, tmp_path: Path) -> None:
    inside = tmp_path / "inside"
    inside.mkdir()
    outside = tmp_path / "outside.txt"
    outside.write_text(STATE_MARKER, encoding="utf-8")
    with pytest.raises(ValueError, match=r"outside params\.within"):
        _plugin(server).execute(params=_file_params(outside, inside), timeout_seconds=10)
    assert server.requests == []


def test_a_sibling_directory_sharing_a_prefix_is_outside(server: FakeServer, tmp_path: Path) -> None:
    inside = tmp_path / "data"
    inside.mkdir()
    sibling = tmp_path / "data-private"
    sibling.mkdir()
    secret = sibling / "s.txt"
    secret.write_text(STATE_MARKER, encoding="utf-8")
    with pytest.raises(ValueError, match=r"outside params\.within"):
        _plugin(server).execute(params=_file_params(secret, inside), timeout_seconds=10)


def test_dot_dot_segments_cannot_leave_within(server: FakeServer, tmp_path: Path) -> None:
    inside = tmp_path / "inside"
    inside.mkdir()
    (tmp_path / "secret.txt").write_text(STATE_MARKER, encoding="utf-8")
    with pytest.raises(ValueError, match=r"outside params\.within"):
        _plugin(server).execute(
            params=_file_params(inside / ".." / "secret.txt", inside), timeout_seconds=10
        )


def test_a_symlink_inside_within_pointing_out_is_refused(server: FakeServer, tmp_path: Path) -> None:
    inside = tmp_path / "inside"
    inside.mkdir()
    secret = tmp_path / "secret.txt"
    secret.write_text(STATE_MARKER, encoding="utf-8")
    link = inside / "link.txt"
    link.symlink_to(secret)
    with pytest.raises(ValueError, match=r"outside params\.within"):
        _plugin(server).execute(params=_file_params(link, inside), timeout_seconds=10)
    assert server.requests == []


def test_a_within_that_is_a_symlink_is_resolved_before_the_check(
    server: FakeServer, tmp_path: Path
) -> None:
    real = tmp_path / "real"
    real.mkdir()
    (real / "a.txt").write_text("ok", encoding="utf-8")
    alias = tmp_path / "alias"
    alias.symlink_to(real, target_is_directory=True)
    server.reply_answers()
    result = _plugin(server).execute(params=_file_params(alias / "a.txt", alias), timeout_seconds=10)
    assert result.ok is True


def test_state_file_must_be_a_regular_file(server: FakeServer, tmp_path: Path) -> None:
    with pytest.raises(ValueError, match="not a regular file"):
        _plugin(server).execute(params=_file_params(tmp_path, tmp_path), timeout_seconds=10)


def test_a_state_file_over_the_limit_is_refused_before_it_is_read(
    server: FakeServer, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    doc = tmp_path / "big.txt"
    doc.write_text("x" * 5000, encoding="utf-8")
    monkeypatch.setattr(systemone_module, "MAX_BODY_BYTES", 2000)
    result = _plugin(server).execute(params=_file_params(doc, tmp_path), timeout_seconds=10)
    assert result.ok is False
    assert result.raw["error_kind"] == "too_large"
    assert server.requests == []


@pytest.mark.parametrize(
    ("params", "message"),
    [
        ({"state": "s", "state_file": "/x", "within": "/"}, "exactly one"),
        ({}, "exactly one"),
        ({"state": "s", "within": "/data"}, "only applies"),
    ],
)
def test_state_and_state_file_are_mutually_exclusive(
    server: FakeServer, params: dict[str, Any], message: str
) -> None:
    base = {"model": "m", "questions": QUESTIONS}
    with pytest.raises(ValueError, match=message):
        _plugin(server).execute(params={**base, **params}, timeout_seconds=10)


@pytest.mark.parametrize("within", [None, "", "relative/dir"])
def test_state_file_needs_an_absolute_within(
    server: FakeServer, tmp_path: Path, within: str | None
) -> None:
    doc = tmp_path / "d.txt"
    doc.write_text("x", encoding="utf-8")
    params = _file_params(doc, tmp_path)
    if within is None:
        del params["within"]
    else:
        params["within"] = within
    with pytest.raises(ValueError, match=r"params\.within"):
        _plugin(server).execute(params=params, timeout_seconds=10)


def test_a_limited_document_confines_within_to_its_project(
    server: FakeServer, tmp_path: Path
) -> None:
    project = tmp_path / "project"
    (project / "data").mkdir(parents=True)
    (project / "data" / "a.txt").write_text("in", encoding="utf-8")
    elsewhere = tmp_path / "elsewhere"
    elsewhere.mkdir()
    (elsewhere / "b.txt").write_text("out", encoding="utf-8")
    plugin = _plugin(server, limited=True, confine_to=str(project))

    server.reply_answers()
    assert plugin.execute(
        params=_file_params(project / "data" / "a.txt", project / "data"), timeout_seconds=10
    ).ok is True

    with pytest.raises(ValueError, match="document's own project"):
        plugin.execute(params=_file_params(elsewhere / "b.txt", elsewhere), timeout_seconds=10)
    with pytest.raises(ValueError, match="document's own project"):
        plugin.execute(params=_file_params(elsewhere / "b.txt", tmp_path), timeout_seconds=10)
    assert len(server.requests) == 1


def test_a_trusted_document_may_name_any_within(server: FakeServer, tmp_path: Path) -> None:
    project = tmp_path / "project"
    project.mkdir()
    elsewhere = tmp_path / "elsewhere"
    elsewhere.mkdir()
    (elsewhere / "b.txt").write_text("out", encoding="utf-8")
    plugin = _plugin(server)
    server.reply_answers()
    assert plugin.execute(
        params=_file_params(elsewhere / "b.txt", elsewhere), timeout_seconds=10
    ).ok is True


# ---------------------------------------------------------------------------
# Host settings
# ---------------------------------------------------------------------------


@pytest.mark.parametrize(
    "host",
    ["localhost", "LOCALHOST", "127.0.0.1", "127.255.255.254", "::1"],
)
def test_loopback_hosts_are_accepted(host: str) -> None:
    assert is_loopback_host(host) is True


@pytest.mark.parametrize(
    "host",
    [
        "example.com",
        "api.typesafe.ai",
        "0.0.0.0",
        "10.0.0.1",
        "127.0.0.1.example.com",
        "localhost.example.com",
        "::ffff:127.0.0.1",
        "::2",
        "127.1",
        "0x7f.0.0.1",
        "",
    ],
)
def test_any_other_host_is_remote(host: str) -> None:
    assert is_loopback_host(host) is False


def _no_network(monkeypatch: pytest.MonkeyPatch) -> list[str]:
    attempts: list[str] = []

    def boom(self: Any, request: Any, *args: Any, **kwargs: Any) -> Any:
        attempts.append(request.full_url)
        raise AssertionError("a request was made")

    monkeypatch.setattr(urllib.request.OpenerDirector, "open", boom)
    return attempts


@pytest.mark.parametrize(
    "base_url",
    [
        "https://api.typesafe.ai",
        "http://example.com:8080",
        "http://10.1.2.3",
        "http://localhost.evil.test",
        "http://[::ffff:127.0.0.1]:80",
    ],
)
def test_a_remote_base_url_without_allow_remote_is_refused_before_any_request(
    monkeypatch: pytest.MonkeyPatch, base_url: str
) -> None:
    attempts = _no_network(monkeypatch)
    with pytest.raises(ValueError, match="not a loopback host") as excinfo:
        SystemOnePlugin(base_url=base_url).execute(params=_params(), timeout_seconds=5)
    assert "allow_remote" in str(excinfo.value)
    assert attempts == []


def test_allow_remote_accepts_a_remote_base_url() -> None:
    plugin = SystemOnePlugin(base_url="https://api.typesafe.ai/", allow_remote=True)
    assert plugin._server() == "https://api.typesafe.ai"
    assert plugin.check().ok is True


def test_allow_remote_is_false_unless_the_config_says_true() -> None:
    for value in (None, False, "false", "no", 0):
        plugin = build_plugin(
            plugin_name="systemone",
            runtime={"plugins": {"systemone": {"base_url": "http://example.com", "allow_remote": value}}},
        )
        assert isinstance(plugin, SystemOnePlugin)
        assert plugin.allow_remote is False, value


@pytest.mark.parametrize(
    ("base_url", "message"),
    [
        (None, "base_url is required"),
        ("", "base_url is required"),
        ("ftp://127.0.0.1", "http:// or https://"),
        ("127.0.0.1:8080", "http:// or https://"),
        ("http://user:pw@127.0.0.1", "must not carry credentials"),
        ("http://127.0.0.1?token=abc", "query or fragment"),
        ("http://127.0.0.1:notaport", "not a valid URL"),
    ],
)
def test_base_url_is_validated(base_url: str | None, message: str) -> None:
    with pytest.raises(ValueError, match=message) as excinfo:
        SystemOnePlugin(base_url=base_url).execute(params=_params(), timeout_seconds=5)
    assert "pw" not in str(excinfo.value)
    assert "abc" not in str(excinfo.value)


@pytest.mark.parametrize("name", ["base_url", "api_key_env", "allow_remote"])
def test_a_host_setting_as_an_effect_param_is_refused(server: FakeServer, name: str) -> None:
    with pytest.raises(ValueError, match=f"params.{name} is a host setting"):
        _plugin(server).execute(params=_params(**{name: "x"}), timeout_seconds=5)
    assert server.requests == []


def test_a_params_json_overlay_cannot_set_a_host_setting(server: FakeServer) -> None:
    defn = ToolDefinition(
        name="decide",
        provider="systemone",
        params=_params(),
        params_json='{"base_url": "http://127.0.0.1:1"}',
    )
    runtime = ToolRuntime(
        defn, runtime_config={"plugins": {"systemone": {"base_url": server.url}}}
    )
    with pytest.raises(ValueError, match="base_url is a host setting"):
        runtime.execute(store=Store({}), ctx={})
    assert server.requests == []


def test_a_params_json_overlay_cannot_set_within(server: FakeServer, tmp_path: Path) -> None:
    doc = tmp_path / "d.txt"
    doc.write_text("x", encoding="utf-8")
    params = _file_params(doc, tmp_path / "ok")
    defn = ToolDefinition(
        name="decide",
        provider="systemone",
        params=params,
        params_json=json.dumps({"within": str(tmp_path)}),
    )
    runtime = ToolRuntime(
        defn, runtime_config={"plugins": {"systemone": {"base_url": server.url}}}
    )
    with pytest.raises(ValueError, match="params_json must not set 'within'"):
        runtime.execute(store=Store({}), ctx={})


def test_check_is_offline_and_quiet_when_unconfigured() -> None:
    result = SystemOnePlugin().check()
    assert result.ok is True
    assert "not configured" in (result.message or "")


def test_check_reports_a_configured_remote_host_without_allow_remote() -> None:
    result = SystemOnePlugin(base_url="https://api.typesafe.ai").check()
    assert result.ok is False
    assert "allow_remote" in (result.message or "")


# ---------------------------------------------------------------------------
# The key
# ---------------------------------------------------------------------------


def test_the_key_is_sent_as_a_bearer_token_when_the_variable_exists(
    server: FakeServer, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("SYSTEMONE_TEST_KEY", f"  {KEY}\n")
    server.reply_answers()
    _plugin(server, api_key_env="SYSTEMONE_TEST_KEY").execute(params=_params(), timeout_seconds=10)
    assert server.requests[0].headers["authorization"] == f"Bearer {KEY}"


def test_no_key_is_sent_when_the_variable_is_unset_or_empty(
    server: FakeServer, monkeypatch: pytest.MonkeyPatch
) -> None:
    server.reply_answers()
    _plugin(server, api_key_env="SYSTEMONE_TEST_KEY").execute(params=_params(), timeout_seconds=10)
    monkeypatch.setenv("SYSTEMONE_TEST_KEY", "  ")
    server.reply_answers()
    _plugin(server, api_key_env="SYSTEMONE_TEST_KEY").execute(params=_params(), timeout_seconds=10)
    assert all("authorization" not in r.headers for r in server.requests)


def test_no_key_is_sent_when_api_key_env_is_not_set(
    server: FakeServer, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("SYSTEMONE_TEST_KEY", KEY)
    server.reply_answers()
    _plugin(server).execute(params=_params(), timeout_seconds=10)
    assert "authorization" not in server.requests[0].headers


def test_the_key_never_reaches_a_result_even_when_the_server_echoes_it(
    server: FakeServer, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("SYSTEMONE_TEST_KEY", KEY)
    server.reply(401, json.dumps({"error": f"bad key {KEY}"}))
    result = _plugin(server, api_key_env="SYSTEMONE_TEST_KEY").execute(
        params=_params(), timeout_seconds=10
    )
    assert result.ok is False
    assert KEY not in (result.stderr or "")
    assert KEY not in json.dumps(result.raw)
    assert result.raw["error_kind"] == "unauthorized"


def test_the_key_is_scrubbed_from_a_non_json_error_body(
    server: FakeServer, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("SYSTEMONE_TEST_KEY", KEY)
    server.reply(422, f"rejected credential {KEY} for this model")
    result = _plugin(server, api_key_env="SYSTEMONE_TEST_KEY").execute(
        params=_params(), timeout_seconds=10
    )
    assert KEY not in (result.stderr or "")
    assert "rejected credential" in (result.stderr or "")


@pytest.mark.parametrize("name", [KEY, "has space", "1LEADING", "a-b", "$VAR"])
def test_api_key_env_must_be_a_variable_name_and_is_never_echoed(
    server: FakeServer, name: str
) -> None:
    with pytest.raises(ValueError, match="NAME of an environment variable") as excinfo:
        _plugin(server, api_key_env=name).execute(params=_params(), timeout_seconds=5)
    assert name not in str(excinfo.value)
    assert server.requests == []


def test_a_key_that_cannot_be_a_header_is_refused_without_echoing_it(
    server: FakeServer, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("SYSTEMONE_TEST_KEY", f"{KEY}\nX-Evil: 1")
    with pytest.raises(ValueError, match="cannot be sent in a header") as excinfo:
        _plugin(server, api_key_env="SYSTEMONE_TEST_KEY").execute(params=_params(), timeout_seconds=5)
    assert KEY not in str(excinfo.value)
    assert server.requests == []


# ---------------------------------------------------------------------------
# cof check
# ---------------------------------------------------------------------------


def _doc(**params: Any) -> dict[str, Any]:
    effect: dict[str, Any] = {
        "type": "tool",
        "name": "decide",
        "provider": "systemone",
        "params": {"model": "m", "state": "s", "questions": QUESTIONS},
    }
    effect["params"].update(params)
    for key in [k for k, v in effect["params"].items() if v is _DROP]:
        del effect["params"][key]
    return {"effects": [effect]}


_DROP = object()


def _compile_error(orch: dict[str, Any]) -> str:
    with pytest.raises(ValueError) as excinfo:
        compile_orchestration(orch=orch)
    return str(excinfo.value)


def test_a_valid_effect_compiles(tmp_path: Path) -> None:
    compile_orchestration(orch=_doc())
    on_disk = {"document_dir": tmp_path, "confinement_root": tmp_path, "trusted": True}
    compile_orchestration(
        orch=_doc(state=_DROP, state_file="/data/a.json", within="/data"), **on_disk
    )
    compile_orchestration(
        orch=_doc(state=_DROP, state_file={"from": "input.f"}, within={"from": "input.dir"}),
        **on_disk,
    )
    compile_orchestration(orch=_doc(state={"from": "input.doc"}, questions={"from": "input.q"}))


def test_both_state_and_state_file_are_rejected() -> None:
    message = _compile_error(_doc(state_file="/data/a.json", within="/data"))
    assert "exactly one of params.state and params.state_file, not both" in message


def test_neither_state_nor_state_file_is_rejected() -> None:
    assert "exactly one of params.state and params.state_file" in _compile_error(_doc(state=_DROP))


def test_a_missing_model_is_rejected() -> None:
    assert "params.model is required" in _compile_error(_doc(model=_DROP))


def test_the_effect_level_model_counts() -> None:
    orch = _doc(model=_DROP)
    orch["effects"][0]["model"] = "typesafe-1"
    compile_orchestration(orch=orch)


def test_params_json_may_supply_what_the_literal_params_leave_out() -> None:
    orch = _doc(model=_DROP, state=_DROP, questions=_DROP)
    orch["effects"][0]["params_json"] = '{"model": "m", "state": "s", "questions": {}}'
    compile_orchestration(orch=orch)


def test_a_missing_questions_is_rejected() -> None:
    assert "params.questions is required" in _compile_error(_doc(questions=_DROP))


def test_an_unknown_question_type_is_rejected() -> None:
    questions = {"q": {"type": "essay", "instructions": "i"}}
    message = _compile_error(_doc(questions=questions))
    assert "question 'q'" in message
    assert "type must be one of noul, choice, score" in message


def test_a_question_without_instructions_is_rejected() -> None:
    message = _compile_error(_doc(questions={"q": {"type": "noul"}}))
    assert "question 'q': instructions are required" in message


@pytest.mark.parametrize(
    ("question", "fragment"),
    [
        ({"type": "choice", "instructions": "i"}, "must be a map of option to description"),
        ({"type": "choice", "instructions": "i", "criteria": ["a", "b"]}, "map of option"),
        (
            {"type": "choice", "instructions": "i", "criteria": {f"o{n}": "d" for n in range(256)}},
            "256 options",
        ),
        ({"type": "score", "instructions": "i"}, "array of 2 to 10 level descriptions"),
        ({"type": "score", "instructions": "i", "criteria": ["only one"]}, "2 to 10"),
        ({"type": "score", "instructions": "i", "criteria": [str(n) for n in range(11)]}, "2 to 10"),
        ({"type": "score", "instructions": "i", "criteria": {"0": "a", "1": "b"}}, "array"),
        ({"type": "noul", "instructions": "i", "criteria": {"yes": "a"}}, "only hold 'true' and 'false'"),
        ({"type": "noul", "instructions": "i", "criteria": ["a"]}, "only hold 'true' and 'false'"),
        ("not a map", "must be a map"),
    ],
)
def test_criteria_must_fit_the_question_type(question: Any, fragment: str) -> None:
    message = _compile_error(_doc(questions={"q": question}))
    assert "question 'q'" in message
    assert fragment in message


def test_255_options_and_the_score_level_bounds_are_accepted() -> None:
    compile_orchestration(
        orch=_doc(
            questions={
                "c": {"type": "choice", "instructions": "i", "criteria": {f"o{n}": None for n in range(255)}},
                "two": {"type": "score", "instructions": "i", "criteria": ["a", "b"]},
                "ten": {"type": "score", "instructions": "i", "criteria": [str(n) for n in range(10)]},
            }
        )
    )


@pytest.mark.parametrize("name", ["base_url", "api_key_env", "allow_remote"])
def test_a_host_setting_as_an_effect_param_is_a_check_error(name: str) -> None:
    assert f"params.{name} is a host setting" in _compile_error(_doc(**{name: "x"}))


@pytest.mark.parametrize(
    "within",
    [
        "relative/dir",
        "{{ input.dir }}",
        {"from": "prime.something"},
        {"from": "input"},
        {"from": "input.a.b"},
        {"from": "decide.value"},
        "",
    ],
)
def test_within_must_be_a_literal_absolute_path_or_an_input_reference(within: Any) -> None:
    message = _compile_error(_doc(state=_DROP, state_file="/data/a.json", within=within))
    assert "params.within" in message


def test_state_file_needs_within_and_within_needs_state_file() -> None:
    assert "params.state_file requires params.within" in _compile_error(
        _doc(state=_DROP, state_file="/data/a.json")
    )
    assert "params.within only applies to params.state_file" in _compile_error(
        _doc(within="/data")
    )


def test_cof_check_rejects_a_bad_effect(tmp_path: Path) -> None:
    typer = pytest.importorskip("typer")
    del typer
    from typer.testing import CliRunner

    from circuitry.cli.app import app

    good = tmp_path / "good.yml"
    good.write_text(yaml.dump(_doc()), encoding="utf-8")
    bad = tmp_path / "bad.yml"
    bad.write_text(yaml.dump(_doc(model=_DROP, state_file="/x", within="/")), encoding="utf-8")

    runner = CliRunner()
    assert runner.invoke(app, ["check", str(good), "--skip-preflight"]).exit_code == 0
    failed = runner.invoke(app, ["check", str(bad), "--skip-preflight"])
    assert failed.exit_code != 0
    output = " ".join(failed.output.split())
    assert "params.model is required" in output
    assert "not both" in output


# ---------------------------------------------------------------------------
# Whole runs: settings, scope, saved state
# ---------------------------------------------------------------------------


def _write_doc(path: Path, effect_params: dict[str, Any], *, runtime: dict[str, Any] | None = None) -> Path:
    doc: dict[str, Any] = {
        "effects": [
            {"type": "tool", "name": "decide", "provider": "systemone", "params": effect_params}
        ]
    }
    if runtime is not None:
        doc["runtime"] = runtime
    path.write_text(yaml.dump(doc), encoding="utf-8")
    return path


def _run(path: Path, config: CircuitryConfig, **kwargs: Any) -> Any:
    return run(
        RunRequest(
            orchestration_path=path,
            state_path=None,
            out_path=kwargs.pop("out_path", None),
            dry_run=False,
            validate_only=False,
            config=config,
            skip_preflight=True,
            **kwargs,
        )
    )


def test_a_run_never_saves_the_state_text_or_the_key(
    server: FakeServer, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("SYSTEMONE_TEST_KEY", KEY)
    project = tmp_path / "project"
    project.mkdir()
    (project / "doc.md").write_text(f"{STATE_MARKER}\n" * 50, encoding="utf-8")
    doc = _write_doc(
        project / "decide.yml",
        {
            "model": "typesafe-1",
            "state_file": str(project / "doc.md"),
            "within": str(project),
            "questions": QUESTIONS,
        },
    )
    config = CircuitryConfig(
        runtime={
            "plugins": {"systemone": {"base_url": server.url, "api_key_env": "SYSTEMONE_TEST_KEY"}}
        }
    )
    server.reply_answers()
    events = tmp_path / "events.jsonl"

    result = _run(doc, config, events_path=events, trust_document=True)

    assert result.ok, result.error
    assert result.state["prime"]["decide"]["value"] == ANSWERS
    meta = result.state["prime"]["decide"]["meta"]
    assert meta["raw"]["state_sha256"]
    assert meta["raw"]["state_bytes"] > 0
    assert (meta["tokens_sent"], meta["tokens_received"]) == (120, 15)
    assert server.requests[0].headers["authorization"] == f"Bearer {KEY}"
    assert STATE_MARKER in server.requests[0].body.decode()

    # `--out` is this state, serialized.
    for text in (json.dumps(result.state), events.read_text(encoding="utf-8")):
        assert STATE_MARKER not in text
        assert KEY not in text


def test_a_failed_run_does_not_save_the_key(
    server: FakeServer, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("SYSTEMONE_TEST_KEY", KEY)
    doc = _write_doc(tmp_path / "decide.yml", _params())
    config = CircuitryConfig(
        runtime={
            "plugins": {"systemone": {"base_url": server.url, "api_key_env": "SYSTEMONE_TEST_KEY"}}
        }
    )
    server.reply(401, json.dumps({"error": f"bad key {KEY}"}))
    events = tmp_path / "events.jsonl"

    result = _run(doc, config, events_path=events, trust_document=True)

    assert result.ok is False
    assert "HTTP 401" in (result.error or "")
    for text in (result.error or "", json.dumps(result.state), events.read_text(encoding="utf-8")):
        assert KEY not in text


def test_a_limited_document_cannot_set_the_host_settings(
    server: FakeServer, tmp_path: Path
) -> None:
    runtime = {"plugins": {"systemone": {"base_url": server.url}}}
    doc = _write_doc(tmp_path / "decide.yml", _params(), runtime=runtime)

    limited = _run(doc, CircuitryConfig(), trust_document=False)
    assert limited.ok is False
    assert "base_url is required" in (limited.error or "")
    assert any("runtime.plugins" in w for w in limited.warnings)
    assert server.requests == []

    server.reply_answers()
    trusted = _run(doc, CircuitryConfig(), trust_document=True)
    assert trusted.ok, trusted.error
    assert len(server.requests) == 1


def test_a_limited_document_cannot_widen_the_host_settings_the_host_set(
    server: FakeServer, tmp_path: Path
) -> None:
    runtime = {"plugins": {"systemone": {"base_url": "https://api.typesafe.ai", "allow_remote": True}}}
    doc = _write_doc(tmp_path / "decide.yml", _params(), runtime=runtime)
    config = CircuitryConfig(runtime={"plugins": {"systemone": {"base_url": server.url}}})

    server.reply_answers()
    result = _run(doc, config, trust_document=False)

    assert result.ok, result.error
    assert len(server.requests) == 1


def test_effective_settings_drop_a_limited_documents_systemone_block(server: FakeServer) -> None:
    orch = {"runtime": {"plugins": {"systemone": {"base_url": server.url, "allow_remote": True}}}}
    config = CircuitryConfig(runtime={"plugins": {"systemone": {"base_url": "http://127.0.0.1:1"}}})

    limited = resolve_effective_settings(cfg=config, orch=orch, trust_document=False)
    assert limited.runtime["plugins"]["systemone"] == {"base_url": "http://127.0.0.1:1"}

    trusted = resolve_effective_settings(cfg=config, orch=orch, trust_document=True)
    assert trusted.runtime["plugins"]["systemone"]["allow_remote"] is True


def _scoped_run(
    tmp_path: Path, server: FakeServer, *, trust_document: bool, within: Path
) -> Any:
    project = tmp_path / "project"
    project.mkdir(exist_ok=True)
    (project / "config.json").write_text("{}", encoding="utf-8")
    (project / "doc.md").write_text("inside", encoding="utf-8")
    shared = tmp_path / "shared"
    shared.mkdir(exist_ok=True)
    (shared / "doc.md").write_text("shared", encoding="utf-8")
    doc = _write_doc(
        project / "decide.yml",
        {
            "model": "m",
            "state_file": {"from": "input.file"},
            "within": {"from": "input.dir"},
            "questions": QUESTIONS,
        },
    )
    config = CircuitryConfig(runtime={"plugins": {"systemone": {"base_url": server.url}}})
    inputs = {"file": str(within / "doc.md"), "dir": str(within)}
    return _run(
        doc, config, trust_document=trust_document, initial_state={"input": inputs}
    )


def test_a_limited_run_confines_within_to_the_documents_project(
    server: FakeServer, tmp_path: Path
) -> None:
    server.reply_answers()
    inside = _scoped_run(tmp_path, server, trust_document=False, within=tmp_path / "project")
    assert inside.ok, inside.error
    assert server.requests[0].json()["state"] == "inside"

    outside = _scoped_run(tmp_path, server, trust_document=False, within=tmp_path / "shared")
    assert outside.ok is False
    assert "document's own project" in (outside.error or "")
    assert len(server.requests) == 1


def test_a_trusted_run_may_name_a_within_outside_its_project(
    server: FakeServer, tmp_path: Path
) -> None:
    server.reply_answers()
    result = _scoped_run(tmp_path, server, trust_document=True, within=tmp_path / "shared")
    assert result.ok, result.error
    assert server.requests[0].json()["state"] == "shared"


def test_a_use_child_is_confined_to_its_own_project(
    server: FakeServer, tmp_path: Path
) -> None:
    """A path child inherits its parent's trust; its project is its own."""
    project = tmp_path / "project"
    (project / "child").mkdir(parents=True)
    (project / "doc.md").write_text("parent-side", encoding="utf-8")
    child = _write_doc(
        project / "child" / "decide.yml",
        {
            "model": "m",
            "state_file": str(project / "doc.md"),
            "within": str(project),
            "questions": QUESTIONS,
        },
    )
    del child
    parent = project / "parent.yml"
    parent.write_text(
        yaml.dump({"effects": [{"type": "use", "name": "sub", "path": "child/decide.yml"}]}),
        encoding="utf-8",
    )
    config = CircuitryConfig(runtime={"plugins": {"systemone": {"base_url": server.url}}})

    server.reply_answers()
    adapter = MagicMock()
    adapter.name = "mock"
    trusted = _run(parent, config, trust_document=True, adapter=adapter)
    assert trusted.ok, trusted.error

    # Limited: the child's project is its nearest config file's directory, or
    # its own directory when there is none -- here `child/`, so the parent's
    # `doc.md` is outside it.
    limited = _run(parent, config, trust_document=False, adapter=adapter)
    assert limited.ok is False
    assert "document's own project" in (limited.error or "")


# ---------------------------------------------------------------------------
# YAML keys: a boolean or integer key is the string the request carries
# ---------------------------------------------------------------------------

NOUL_WITH_BOOLEAN_KEYS = """\
spam:
  type: noul
  instructions: Is it spam?
  criteria:
    true: It is spam.
    false: It is not spam.
"""

CHOICE_WITH_BOOLEAN_KEYS = """\
accept:
  type: choice
  instructions: Accept it?
  criteria:
    yes: Accept.
    no: Decline.
"""

CHOICE_WITH_INTEGER_KEYS = """\
rating:
  type: choice
  instructions: How many stars?
  criteria:
    1: One star.
    2: Two stars.
    3: Three stars.
    4: Four stars.
    5: Five stars.
"""


def _noul_answers(qid: str) -> dict[str, Any]:
    return {qid: {"type": "noul", "noul": 0.2}}


def _choice_answers(qid: str, chosen: str, options: list[str]) -> dict[str, Any]:
    share = 1 / len(options)
    return {
        qid: {
            "type": "choice",
            "choice": chosen,
            "probabilities": dict.fromkeys(options, share),
        }
    }


def test_unquoted_boolean_noul_keys_pass_the_check_and_round_trip(server: FakeServer) -> None:
    questions = yaml.safe_load(NOUL_WITH_BOOLEAN_KEYS)
    assert set(questions["spam"]["criteria"]) == {True, False}
    compile_orchestration(orch=_doc(questions=questions))

    server.reply_answers(_noul_answers("spam"))
    result = _plugin(server).execute(params=_params(questions=questions), timeout_seconds=10)

    assert result.ok, result.stderr
    sent = server.requests[0].json()["questions"]["spam"]["criteria"]
    assert sent == {"true": "It is spam.", "false": "It is not spam."}


@pytest.mark.parametrize(
    ("text", "qid", "options"),
    [
        (CHOICE_WITH_BOOLEAN_KEYS, "accept", ["true", "false"]),
        (CHOICE_WITH_INTEGER_KEYS, "rating", ["1", "2", "3", "4", "5"]),
    ],
)
def test_unquoted_boolean_and_integer_choice_options_round_trip_as_json_writes_them(
    server: FakeServer, text: str, qid: str, options: list[str]
) -> None:
    questions = yaml.safe_load(text)
    assert not any(isinstance(key, str) for key in questions[qid]["criteria"])
    compile_orchestration(orch=_doc(questions=questions))

    server.reply_answers(_choice_answers(qid, options[0], options))
    result = _plugin(server).execute(params=_params(questions=questions), timeout_seconds=10)

    assert result.ok, result.stderr
    assert list(server.requests[0].json()["questions"][qid]["criteria"]) == options
    assert result.value[qid]["choice"] == options[0]


def test_an_answer_naming_the_yaml_spelling_of_a_boolean_option_is_refused(server: FakeServer) -> None:
    questions = yaml.safe_load(CHOICE_WITH_BOOLEAN_KEYS)
    server.reply_answers(_choice_answers("accept", "yes", ["yes", "no"]))
    result = _plugin(server).execute(params=_params(questions=questions), timeout_seconds=10)
    assert result.ok is False
    assert result.raw["error_kind"] == "invalid_answer"


def test_a_key_that_repeats_another_once_written_is_refused() -> None:
    questions = {
        "q": {"type": "choice", "instructions": "?", "criteria": {1: "a", "1": "b"}}
    }
    message = _compile_error(_doc(questions=questions))
    assert "repeats another option" in message


def test_cof_check_accepts_unquoted_boolean_keys_from_a_yaml_file(tmp_path: Path) -> None:
    pytest.importorskip("typer")
    from typer.testing import CliRunner

    from circuitry.cli.app import app

    text = "effects:\n  - type: tool\n    name: decide\n    provider: systemone\n    params:\n"
    text += "      model: m\n      state: s\n      questions:\n"
    for line in (NOUL_WITH_BOOLEAN_KEYS + CHOICE_WITH_BOOLEAN_KEYS).splitlines():
        text += f"        {line}\n"
    doc = tmp_path / "decide.yml"
    doc.write_text(text, encoding="utf-8")

    checked = CliRunner().invoke(app, ["check", str(doc), "--skip-preflight"])

    assert checked.exit_code == 0, checked.output


# ---------------------------------------------------------------------------
# What the server sends back never carries the key
# ---------------------------------------------------------------------------


def test_the_key_is_scrubbed_from_the_value_and_raw_of_a_success(
    server: FakeServer, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("SYSTEMONE_TEST_KEY", KEY)
    answers = json.loads(json.dumps(ANSWERS))
    answers["quality"]["legend"] = {"0": f"bad {KEY}", "1": "ok", "2": "good"}
    answers["spam"]["extra"] = [f"seen {KEY}", {f"k-{KEY}": KEY}]
    server.reply(
        200,
        {
            "model": f"typesafe-{KEY}",
            "answers": answers,
            "usage": {**USAGE, "note": f"billed to {KEY}"},
        },
    )

    result = _plugin(server, api_key_env="SYSTEMONE_TEST_KEY").execute(
        params=_params(), timeout_seconds=10
    )

    assert result.ok, result.stderr
    assert KEY not in json.dumps(result.value)
    assert KEY not in json.dumps(result.raw)
    assert result.value["quality"]["legend"]["0"] == "bad [redacted]"
    assert result.raw["model"] == "typesafe-[redacted]"
    assert result.value["spam"]["noul"] == 0.1


def test_the_key_is_scrubbed_from_retry_after_and_a_failure_raw(
    server: FakeServer, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("SYSTEMONE_TEST_KEY", KEY)
    server.reply(429, {"error": "slow down"}, headers={"Retry-After": f"5 {KEY}"})
    result = _plugin(server, api_key_env="SYSTEMONE_TEST_KEY").execute(
        params=_params(), timeout_seconds=10
    )
    assert result.ok is False
    assert KEY not in json.dumps(result.raw)
    assert result.raw["headers"]["retry-after"] == "5 [redacted]"


def test_the_key_is_scrubbed_from_a_reply_with_the_wrong_answers(
    server: FakeServer, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("SYSTEMONE_TEST_KEY", KEY)
    server.reply(200, {"model": KEY, "answers": {KEY: {}}, "usage": {"x": KEY}})
    result = _plugin(server, api_key_env="SYSTEMONE_TEST_KEY").execute(
        params=_params(), timeout_seconds=10
    )
    assert result.ok is False
    assert KEY not in (result.stderr or "")
    assert KEY not in json.dumps(result.raw)


# ---------------------------------------------------------------------------
# A document with no file of its own cannot use state_file
# ---------------------------------------------------------------------------

NO_FILE_OF_ITS_OWN = "'file:' cannot be used here"


def test_a_document_with_no_file_cannot_use_state_file() -> None:
    message = _compile_error(_doc(state=_DROP, state_file="/data/a.json", within="/data"))
    assert NO_FILE_OF_ITS_OWN in message
    assert "params.state_file" in message


def test_a_document_with_no_file_may_still_use_state() -> None:
    compile_orchestration(orch=_doc(state={"from": "input.doc"}))


def test_a_use_inline_child_cannot_use_state_file(tmp_path: Path) -> None:
    child = yaml.dump(
        _doc(state=_DROP, state_file=str(tmp_path / "a.txt"), within=str(tmp_path))
    )
    parent = tmp_path / "parent.yml"
    parent.write_text(
        yaml.dump({"effects": [{"type": "use", "name": "sub", "inline": child}]}),
        encoding="utf-8",
    )
    adapter = MagicMock()
    adapter.name = "mock"

    result = _run(parent, CircuitryConfig(), trust_document=True, adapter=adapter)

    assert result.ok is False
    assert NO_FILE_OF_ITS_OWN in (result.error or "")
    assert "state_file" in (result.error or "")


def test_a_use_inline_child_may_use_state(server: FakeServer, tmp_path: Path) -> None:
    child = yaml.dump(_doc(state="inline text"))
    parent = tmp_path / "parent.yml"
    parent.write_text(
        yaml.dump({"effects": [{"type": "use", "name": "sub", "inline": child}]}),
        encoding="utf-8",
    )
    config = CircuitryConfig(runtime={"plugins": {"systemone": {"base_url": server.url}}})
    adapter = MagicMock()
    adapter.name = "mock"

    server.reply_answers()
    result = _run(parent, config, trust_document=True, adapter=adapter)

    assert result.ok, result.error
    assert server.requests[0].json()["state"] == "inline text"


def test_a_reflector_plan_cannot_use_state_file(tmp_path: Path) -> None:
    from circuitry.core.dynamic import DynamicRuntime

    plan = {
        "effects": _doc(state=_DROP, state_file=str(tmp_path / "a.txt"), within=str(tmp_path))[
            "effects"
        ],
    }
    adapter = MagicMock()
    adapter.name = "mock"
    reply = MagicMock()
    reply.text = yaml.dump(plan)
    reply.raw = {}
    reply.tokens_sent = 1
    reply.tokens_received = 1
    adapter.generate.return_value = reply
    orch = {
        "effects": [
            {
                "type": "reflector",
                "name": "planner",
                "max_effects": 5,
                "effects": [{"type": "prompt", "name": "propose_steps", "template": "Plan."}],
            }
        ]
    }

    with pytest.raises(RuntimeError, match="generated_exec_failed") as excinfo:
        DynamicRuntime(compile_orchestration(orch=orch), adapter=adapter, model="m").execute(
            store=Store(state={})
        )

    assert NO_FILE_OF_ITS_OWN in str(excinfo.value)
    assert "state_file" in str(excinfo.value)


# ---------------------------------------------------------------------------
# A library child is always limited; a path child inherits its parent's trust
# ---------------------------------------------------------------------------


def _library_child_setup(tmp_path: Path, server: FakeServer) -> tuple[Path, Path, CircuitryConfig]:
    """A root and a library child whose ``within`` is outside the child's project."""
    library = tmp_path / "library"
    shared = tmp_path / "shared"
    library.mkdir()
    shared.mkdir()
    (shared / "doc.md").write_text("shared text", encoding="utf-8")
    _write_doc(
        library / "child.yml",
        {
            "model": "m",
            "state_file": str(shared / "doc.md"),
            "within": str(shared),
            "questions": QUESTIONS,
        },
    )
    root = tmp_path / "root"
    root.mkdir()
    config = CircuitryConfig(
        default_model="m",
        runtime={
            "library": {"sources": [{"type": "folder", "name": "local", "path": str(library)}]},
            "plugins": {"systemone": {"base_url": server.url}},
        },
    )
    return root, library, config


def _use_parent(root: Path, **child: str) -> Path:
    parent = root / "parent.yml"
    parent.write_text(
        yaml.dump({"effects": [{"type": "use", "name": "sub", **child}]}), encoding="utf-8"
    )
    return parent


def test_a_library_child_is_limited_under_a_trusted_root(
    server: FakeServer, tmp_path: Path
) -> None:
    root, library, config = _library_child_setup(tmp_path, server)
    adapter = MagicMock()
    adapter.name = "mock"

    consent = frozenset({"network"})
    by_ref = _run(
        _use_parent(root, ref="local:child"),
        config,
        trust_document=True,
        adapter=adapter,
        allow_capabilities=consent,
    )
    assert by_ref.ok is False
    assert "own project" in (by_ref.error or "")
    assert server.requests == []

    server.reply_answers()
    by_path = _run(
        _use_parent(root, path=str(library / "child.yml")),
        config,
        trust_document=True,
        adapter=adapter,
        allow_capabilities=consent,
    )
    assert by_path.ok, by_path.error
    assert server.requests[0].json()["state"] == "shared text"


def test_a_path_child_inside_an_inline_child_is_limited(
    server: FakeServer, tmp_path: Path
) -> None:
    """An inline child is limited, so a path grandchild takes that, not the root's trust."""
    shared = tmp_path / "shared"
    shared.mkdir()
    (shared / "doc.md").write_text("shared text", encoding="utf-8")
    project = tmp_path / "project"
    project.mkdir()
    child = _write_doc(
        project / "child.yml",
        {
            "model": "m",
            "state_file": str(shared / "doc.md"),
            "within": str(shared),
            "questions": QUESTIONS,
        },
    )
    direct = _use_parent(project, path=str(child))
    inline = project / "inline.yml"
    inline.write_text(
        yaml.dump(
            {
                "effects": [
                    {
                        "type": "use",
                        "name": "middle",
                        "inline": yaml.dump(
                            {"effects": [{"type": "use", "name": "leaf", "path": str(child)}]}
                        ),
                    }
                ]
            }
        ),
        encoding="utf-8",
    )
    config = CircuitryConfig(runtime={"plugins": {"systemone": {"base_url": server.url}}})
    adapter = MagicMock()
    adapter.name = "mock"
    consent = frozenset({"network"})

    server.reply_answers()
    trusted = _run(
        direct, config, trust_document=True, adapter=adapter, allow_capabilities=consent
    )
    assert trusted.ok, trusted.error

    limited = _run(
        inline, config, trust_document=True, adapter=adapter, allow_capabilities=consent
    )
    assert limited.ok is False
    assert "own project" in (limited.error or "")
    assert len(server.requests) == 1


# ---------------------------------------------------------------------------
# cof check knows a limited document's project
# ---------------------------------------------------------------------------


def test_a_limited_documents_literal_within_is_checked_against_its_project(
    tmp_path: Path,
) -> None:
    project = tmp_path / "project"
    project.mkdir()
    inside = _doc(state=_DROP, state_file=str(project / "a.txt"), within=str(project))
    outside = _doc(state=_DROP, state_file=str(tmp_path / "a.txt"), within=str(tmp_path))
    on_disk = {"document_dir": project, "confinement_root": project}

    compile_orchestration(orch=inside, trusted=False, **on_disk)
    with pytest.raises(ValueError, match=r"params\.within: must lie inside the document's own project"):
        compile_orchestration(orch=outside, trusted=False, **on_disk)
    compile_orchestration(orch=outside, trusted=True, **on_disk)


def test_a_within_taken_from_input_is_left_to_the_run(tmp_path: Path) -> None:
    orch = _doc(state=_DROP, state_file={"from": "input.f"}, within={"from": "input.dir"})
    compile_orchestration(
        orch=orch, document_dir=tmp_path, confinement_root=tmp_path, trusted=False
    )


def test_validate_checks_within_for_a_document_the_caller_did_not_name(tmp_path: Path) -> None:
    from circuitry.cli.runtime_shim import validate

    project = tmp_path / "project"
    project.mkdir()
    doc = _write_doc(
        project / "decide.yml",
        {
            "model": "m",
            "state_file": str(tmp_path / "a.txt"),
            "within": str(tmp_path),
            "questions": QUESTIONS,
        },
    )

    named = validate(doc, trust_document=True, skip_preflight=True)
    assert named["ok"] is True, named

    unnamed = validate(doc, trust_document=False, skip_preflight=True)
    assert unnamed["ok"] is False
    assert any("document's own project" in e for e in unnamed["errors"])


# ---------------------------------------------------------------------------
# params_json cannot smuggle a state_file past the checks on within
# ---------------------------------------------------------------------------

_SMUGGLE = {"state": _DROP, "within": "/", "params_json": '{"state_file": "/any/file"}'}


def _smuggling_doc(**params: Any) -> dict[str, Any]:
    """A doc whose literal params name ``within`` and whose ``params_json`` adds ``state_file``."""
    doc = _doc(**{**_SMUGGLE, **params})
    effect = doc["effects"][0]
    effect["params_json"] = effect["params"].pop("params_json")
    return doc


def test_a_document_with_no_file_cannot_name_within_for_a_params_json_state_file() -> None:
    message = _compile_error(_smuggling_doc())
    assert NO_FILE_OF_ITS_OWN in message
    assert "params.within" in message

    reference = _compile_error(_smuggling_doc(within={"from": "input.dir"}))
    assert NO_FILE_OF_ITS_OWN in reference
    assert "params.within" in reference


def test_a_use_inline_child_cannot_smuggle_a_state_file_through_params_json(
    tmp_path: Path,
) -> None:
    secret = tmp_path / "secret.txt"
    secret.write_text("secret", encoding="utf-8")
    smuggle = _smuggling_doc(within="/")
    smuggle["effects"][0]["params_json"] = json.dumps({"state_file": str(secret)})
    parent = tmp_path / "parent.yml"
    parent.write_text(
        yaml.dump({"effects": [{"type": "use", "name": "sub", "inline": yaml.dump(smuggle)}]}),
        encoding="utf-8",
    )
    adapter = MagicMock()
    adapter.name = "mock"

    result = _run(parent, CircuitryConfig(), trust_document=True, adapter=adapter)

    assert result.ok is False
    assert NO_FILE_OF_ITS_OWN in (result.error or "")


def test_a_document_with_a_file_still_checks_within_when_params_json_adds_state_file(
    tmp_path: Path,
) -> None:
    on_disk = {"document_dir": tmp_path, "confinement_root": tmp_path, "trusted": False}
    for within in ("{{prime.llm.value}}", {"from": "prime.x.value"}, "relative"):
        message = _compile_error_in(_smuggling_doc(within=within), **on_disk)
        assert "params.within" in message
    outside = _smuggling_doc(within=str(tmp_path.parent))
    assert "own project" in _compile_error_in(outside, **on_disk)
    compile_orchestration(orch=_smuggling_doc(within=str(tmp_path)), **on_disk)
    compile_orchestration(
        orch=_smuggling_doc(within=str(tmp_path.parent)), **{**on_disk, "trusted": True}
    )


def _compile_error_in(orch: dict[str, Any], **location: Any) -> str:
    with pytest.raises(ValueError) as excinfo:
        compile_orchestration(orch=orch, **location)
    return str(excinfo.value)


def test_a_limited_effect_with_no_project_reads_no_file(
    server: FakeServer, tmp_path: Path
) -> None:
    """Fail closed: limited without a confinement root is not unconfined."""
    doc = tmp_path / "a.txt"
    doc.write_text("x", encoding="utf-8")
    defn = ToolDefinition(
        name="decide",
        provider="systemone",
        params=_file_params(doc, tmp_path),
        limited=True,
        confine_to=None,
    )
    runtime = ToolRuntime(
        defn, runtime_config={"plugins": {"systemone": {"base_url": server.url}}}
    )
    with pytest.raises(ValueError, match="no project"):
        runtime.execute(store=Store({}), ctx={})
    assert server.requests == []

    with pytest.raises(ValueError, match="no project"):
        _plugin(server, limited=True).execute(
            params=_file_params(doc, tmp_path), timeout_seconds=10
        )


def test_a_compiled_limited_document_with_no_project_is_limited_without_a_root() -> None:
    root = compile_orchestration(orch=_doc(state="s"), trusted=False)
    tool = root.effects[0]
    assert (tool.limited, tool.confine_to) == (True, None)
    trusted = compile_orchestration(orch=_doc(state="s"), trusted=True).effects[0]
    assert (trusted.limited, trusted.confine_to) == (False, None)


def test_question_ids_that_collide_once_written_are_reported() -> None:
    question = {"type": "noul", "instructions": "?"}
    message = _compile_error(_doc(questions={1: question, "1": question}))
    assert "repeats another id" in message


# ---------------------------------------------------------------------------
# A profile cannot switch a tool to or from systemone
# ---------------------------------------------------------------------------


def _profile_doc(tmp_path: Path, provider: str, params: dict[str, Any]) -> Path:
    project = tmp_path / "project"
    project.mkdir(exist_ok=True)
    (project / "profiles").mkdir(exist_ok=True)
    doc = project / "decide.yml"
    doc.write_text(
        yaml.dump(
            {"effects": [{"type": "tool", "name": "decide", "provider": provider, "params": params}]}
        ),
        encoding="utf-8",
    )
    return doc


def _write_profile(doc: Path, provider: str) -> None:
    (doc.parent / "profiles" / "swap.yml").write_text(
        yaml.dump({"effects": {"decide": {"provider": provider}}}), encoding="utf-8"
    )


def test_a_profile_cannot_switch_an_http_tool_to_systemone(
    server: FakeServer, tmp_path: Path
) -> None:
    secret = tmp_path / "secret.txt"
    secret.write_text("secret", encoding="utf-8")
    doc = _profile_doc(
        tmp_path,
        "http",
        {
            "url": "http://127.0.0.1:1/",
            "model": "m",
            "state_file": str(secret),
            "within": "/",
            "questions": QUESTIONS,
        },
    )
    _write_profile(doc, "systemone")
    config = CircuitryConfig(runtime={"plugins": {"systemone": {"base_url": server.url}}})

    result = _run(doc, config, trust_document=False, profile_name="swap")

    assert result.ok is False
    error = result.error or ""
    assert "swap" in error
    assert "decide" in error
    assert "systemone" in error
    assert server.requests == []


def test_a_profile_cannot_switch_a_systemone_tool_to_another_provider(
    server: FakeServer, tmp_path: Path
) -> None:
    doc = _profile_doc(tmp_path, "systemone", _params())
    _write_profile(doc, "http")
    config = CircuitryConfig(runtime={"plugins": {"systemone": {"base_url": server.url}}})

    result = _run(doc, config, trust_document=True, profile_name="swap")

    assert result.ok is False
    assert "cannot change its provider" in (result.error or "")
    assert server.requests == []


@pytest.mark.parametrize("new_provider", ["SystemOne ", " systemone"])
def test_the_provider_switch_check_ignores_case_and_whitespace(new_provider: str) -> None:
    from circuitry.core.compiler import apply_effect_overrides

    orch = {"effects": [{"type": "tool", "name": "t", "provider": "http", "params": {"url": "u"}}]}
    root = compile_orchestration(orch=orch)
    with pytest.raises(ValueError, match="cannot change its provider"):
        apply_effect_overrides(root, {"t": {"provider": new_provider}}, profile_name="p")


def test_a_profile_may_still_switch_between_two_other_providers() -> None:
    from circuitry.core.compiler import apply_effect_overrides

    orch = {"effects": [{"type": "tool", "name": "t", "provider": "http", "params": {"url": "u"}}]}
    root = compile_orchestration(orch=orch)
    swapped, matched = apply_effect_overrides(
        root, {"t": {"provider": "web_fetch"}}, profile_name="p"
    )
    assert matched == {"t"}
    assert swapped.effects[0].provider == "web_fetch"

    kept, _ = apply_effect_overrides(
        compile_orchestration(orch=_doc(state="s")), {"decide": {"provider": "systemone"}}
    )
    assert kept.effects[0].provider == "systemone"


# ---------------------------------------------------------------------------
# CIRCUITRY_SYSTEMONE_URL
# ---------------------------------------------------------------------------


def _config_file(tmp_path: Path, systemone: dict[str, Any]) -> Path:
    path = tmp_path / "config.json"
    path.write_text(
        json.dumps({"runtime": {"plugins": {"systemone": systemone}}}), encoding="utf-8"
    )
    return path


def _plugin_from(config: CircuitryConfig) -> SystemOnePlugin:
    plugin = build_plugin(plugin_name="systemone", runtime=config.runtime)
    assert isinstance(plugin, SystemOnePlugin)
    return plugin


def test_the_url_variable_wins_over_config_and_keeps_its_other_settings(
    server: FakeServer, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("SYSTEMONE_TEST_KEY", KEY)
    monkeypatch.setenv("CIRCUITRY_SYSTEMONE_URL", server.url)
    config = resolve_config(
        explicit_path=_config_file(
            tmp_path,
            {"base_url": f"http://127.0.0.1:{_closed_port()}", "api_key_env": "SYSTEMONE_TEST_KEY"},
        )
    )
    assert config.runtime["plugins"]["systemone"] == {
        "base_url": server.url,
        "api_key_env": "SYSTEMONE_TEST_KEY",
    }

    server.reply_answers()
    result = _run(_write_doc(tmp_path / "decide.yml", _params()), config, trust_document=True)

    assert result.ok, result.error
    assert len(server.requests) == 1
    assert server.requests[0].headers["authorization"] == f"Bearer {KEY}"


@pytest.mark.parametrize("value", [None, ""])
def test_the_config_url_applies_when_the_variable_is_unset_or_empty(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, value: str | None
) -> None:
    if value is not None:
        monkeypatch.setenv("CIRCUITRY_SYSTEMONE_URL", value)
    config = resolve_config(explicit_path=_config_file(tmp_path, {"base_url": "http://localhost:9"}))
    assert _plugin_from(config).base_url == "http://localhost:9"


def test_the_url_variable_sets_base_url_when_config_has_no_systemone_block(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("CIRCUITRY_SYSTEMONE_URL", "http://localhost:9")
    config_path = tmp_path / "config.json"
    config_path.write_text("{}", encoding="utf-8")
    assert resolve_config(explicit_path=config_path).runtime["plugins"]["systemone"] == {
        "base_url": "http://localhost:9"
    }


@pytest.mark.parametrize(
    ("value", "message"),
    [
        ("ftp://127.0.0.1", "http:// or https://"),
        ("127.0.0.1:8080", "http:// or https://"),
        ("http://user:pw@127.0.0.1", "must not carry credentials"),
        ("http://127.0.0.1?token=abc", "query or fragment"),
        ("http://127.0.0.1#abc", "query or fragment"),
    ],
)
def test_the_url_variable_is_validated_like_the_config_base_url(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, value: str, message: str
) -> None:
    attempts = _no_network(monkeypatch)
    monkeypatch.setenv("CIRCUITRY_SYSTEMONE_URL", value)
    config = resolve_config(explicit_path=_config_file(tmp_path, {"base_url": "http://localhost:9"}))
    with pytest.raises(ValueError, match=message) as excinfo:
        _plugin_from(config).execute(params=_params(), timeout_seconds=5)
    assert "runtime.plugins.systemone.base_url" in str(excinfo.value)
    assert "pw" not in str(excinfo.value)
    assert "abc" not in str(excinfo.value)
    assert attempts == []


def test_a_remote_url_from_the_variable_is_refused_unless_config_allows_remote(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    attempts = _no_network(monkeypatch)
    monkeypatch.setenv("CIRCUITRY_SYSTEMONE_URL", "https://api.typesafe.ai")

    config = resolve_config(explicit_path=_config_file(tmp_path, {"base_url": "http://localhost:9"}))
    with pytest.raises(ValueError, match="not a loopback host") as excinfo:
        _plugin_from(config).execute(params=_params(), timeout_seconds=5)
    assert "allow_remote" in str(excinfo.value)
    assert attempts == []

    allowed = resolve_config(
        explicit_path=_config_file(tmp_path, {"base_url": "http://localhost:9", "allow_remote": True})
    )
    plugin = _plugin_from(allowed)
    assert plugin.allow_remote is True
    assert plugin._server() == "https://api.typesafe.ai"
