"""Streamed-call failure semantics match the non-streamed path's (#674):
an error body, a timeout and a mid-stream disconnect classify, and read,
exactly the same whether or not the request streamed; an incomplete or
errored stream never succeeds with partial text. Real curl against a real
loopback server throughout -- the streaming code path always calls
``run_curl_streaming``, which `on_line`-reads from curl's own real
``stdout`` pipe, so a mocked ``subprocess.Popen`` (as the plain
curl-hygiene tests use) cannot stand in for it.
"""

from __future__ import annotations

import http.server
import json
import time

import pytest
from curl_test_support import local_server

from circuitry.adapters._retry import AdapterCallError
from circuitry.adapters.anthropic import AnthropicAdapter
from circuitry.adapters.base import GenerateOptions
from circuitry.adapters.groq import GroqAdapter
from circuitry.adapters.ollama import OllamaAdapter
from circuitry.adapters.openai import OpenAIAdapter


class _JsonErrorHandler(http.server.BaseHTTPRequestHandler):
    status = 400
    body = b'{"error": {"message": "bad request", "type": "invalid_request_error"}}'

    def do_POST(self) -> None:
        length = int(self.headers.get("Content-Length", 0))
        self.rfile.read(length)
        self.send_response(self.status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(self.body)))
        self.end_headers()
        self.wfile.write(self.body)

    def log_message(self, *args: object) -> None:
        pass


class _AnthropicSseErrorHandler(http.server.BaseHTTPRequestHandler):
    def do_POST(self) -> None:
        length = int(self.headers.get("Content-Length", 0))
        self.rfile.read(length)
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        event = {"type": "error", "error": {"type": "overloaded_error", "message": "overloaded"}}
        self.wfile.write(f"data: {json.dumps(event)}\n\n".encode())

    def log_message(self, *args: object) -> None:
        pass


class _NeverRespondsHandler(http.server.BaseHTTPRequestHandler):
    def do_POST(self) -> None:
        length = int(self.headers.get("Content-Length", 0))
        self.rfile.read(length)
        time.sleep(5)

    def log_message(self, *args: object) -> None:
        pass


class _OllamaTwoDeltasThenHangsHandler(http.server.BaseHTTPRequestHandler):
    """Two real NDJSON deltas, then silence past ``--max-time`` -- the
    shape a mid-stream timeout takes once the model has already answered
    something (#674)."""

    def do_POST(self) -> None:
        length = int(self.headers.get("Content-Length", 0))
        self.rfile.read(length)
        self.send_response(200)
        self.send_header("Content-Type", "application/x-ndjson")
        self.end_headers()
        self.wfile.write(b'{"response": "hello ", "done": false}\n')
        self.wfile.write(b'{"response": "there", "done": false}\n')
        self.wfile.flush()
        time.sleep(5)

    def log_message(self, *args: object) -> None:
        pass


class _OllamaShortContentLengthHandler(http.server.BaseHTTPRequestHandler):
    """Declares a ``Content-Length`` far larger than what it actually
    sends, then closes the connection -- a real mid-stream disconnect
    (curl exit 18), after two genuine deltas."""

    def do_POST(self) -> None:
        length = int(self.headers.get("Content-Length", 0))
        self.rfile.read(length)
        body = b'{"response": "hello ", "done": false}\n{"response": "there", "done": false}\n'
        self.send_response(200)
        self.send_header("Content-Type", "application/x-ndjson")
        self.send_header("Content-Length", str(len(body) + 1000))
        self.end_headers()
        self.wfile.write(body)
        self.wfile.flush()
        self.connection.close()

    def log_message(self, *args: object) -> None:
        pass


class _OllamaErrorMidStreamHandler(http.server.BaseHTTPRequestHandler):
    """A real delta, then ``{"error": ...}`` mid-stream over an HTTP 200
    response -- never caught by curl's own ``--fail-with-body``, since the
    status line was already 200."""

    def do_POST(self) -> None:
        length = int(self.headers.get("Content-Length", 0))
        self.rfile.read(length)
        self.send_response(200)
        self.send_header("Content-Type", "application/x-ndjson")
        self.end_headers()
        self.wfile.write(b'{"response": "hello ", "done": false}\n')
        self.wfile.write(b'{"error": "model is overloaded"}\n')

    def log_message(self, *args: object) -> None:
        pass


class _OllamaIncompleteNoDoneHandler(http.server.BaseHTTPRequestHandler):
    """Two real deltas, then the connection closes cleanly -- never a
    ``done: true`` line at all, as if the server simply stopped writing."""

    def do_POST(self) -> None:
        length = int(self.headers.get("Content-Length", 0))
        self.rfile.read(length)
        self.send_response(200)
        self.send_header("Content-Type", "application/x-ndjson")
        self.end_headers()
        self.wfile.write(b'{"response": "hello ", "done": false}\n')
        self.wfile.write(b'{"response": "there", "done": false}\n')

    def log_message(self, *args: object) -> None:
        pass


class _OpenAICompatibleSseErrorHandler(http.server.BaseHTTPRequestHandler):
    """A real delta, then an OpenRouter/vLLM-shaped ``{"error": ...}``
    mid-stream, over an HTTP 200 SSE response."""

    def do_POST(self) -> None:
        length = int(self.headers.get("Content-Length", 0))
        self.rfile.read(length)
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        self.wfile.write(b'data: {"choices": [{"delta": {"content": "hi "}}]}\n\n')
        error = {"error": {"message": "rate limited", "code": 429}}
        self.wfile.write(f"data: {json.dumps(error)}\n\n".encode())

    def log_message(self, *args: object) -> None:
        pass


class _OpenAICompatibleIncompleteNoSentinelHandler(http.server.BaseHTTPRequestHandler):
    """Real deltas, then the connection closes cleanly with no ``data:
    [DONE]`` sentinel at all."""

    def do_POST(self) -> None:
        length = int(self.headers.get("Content-Length", 0))
        self.rfile.read(length)
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        self.wfile.write(b'data: {"choices": [{"delta": {"content": "hi "}}]}\n\n')
        self.wfile.write(b'data: {"choices": [{"delta": {"content": "there"}}]}\n\n')

    def log_message(self, *args: object) -> None:
        pass


class _OpenAISseSuccessHandler(http.server.BaseHTTPRequestHandler):
    """``openai.py``'s own streamed success path -- distinct from the
    OpenAI-compatible family's shared ``chat_completion`` (exercised
    above via ``groq``/``lmstudio``), since ``OpenAIAdapter.generate``
    builds its own request and result rather than delegating."""

    def do_POST(self) -> None:
        length = int(self.headers.get("Content-Length", 0))
        self.rfile.read(length)
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        self.wfile.write(b'data: {"choices": [{"delta": {"content": "hello "}}]}\n\n')
        self.wfile.write(
            b'data: {"choices": [{"delta": {"content": "there"}, "finish_reason": "stop"}]}\n\n'
        )
        usage = (
            '{"choices": [], "usage": {"prompt_tokens": 4, "completion_tokens": 2}}'
        )
        self.wfile.write(f"data: {usage}\n\n".encode())
        self.wfile.write(b"data: [DONE]\n\n")

    def log_message(self, *args: object) -> None:
        pass


class _SseLengthHandler(http.server.BaseHTTPRequestHandler):
    def do_POST(self) -> None:
        length = int(self.headers.get("Content-Length", 0))
        self.rfile.read(length)
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        length_event = (
            '{"choices": [{"delta": {}, "finish_reason": "length"}], '
            '"usage": {"prompt_tokens": 3, "completion_tokens": 2}}'
        )
        lines = [
            'data: {"choices": [{"delta": {"content": "cut off"}}]}\n\n',
            f"data: {length_event}\n\n",
            "data: [DONE]\n\n",
        ]
        self.wfile.write("".join(lines).encode())

    def log_message(self, *args: object) -> None:
        pass


def test_streamed_400_error_body_matches_non_streamed_text(monkeypatch: pytest.MonkeyPatch) -> None:
    with local_server(_JsonErrorHandler) as base_url:
        streamed = AnthropicAdapter(base_url=base_url)
        plain = AnthropicAdapter(base_url=base_url, streams=False)
        monkeypatch.setenv("ANTHROPIC_API_KEY", "test-key")

        with pytest.raises(AdapterCallError) as streamed_exc:
            streamed.generate(model="m", prompt="x")
        with pytest.raises(AdapterCallError) as plain_exc:
            plain.generate(model="m", prompt="x")

        assert str(streamed_exc.value) == str(plain_exc.value)
        assert "bad request" in str(streamed_exc.value)
        assert streamed_exc.value.retry_info.retryable is False
        assert streamed_exc.value.retry_info.status == 400


def test_anthropic_stream_level_error_event_classifies_like_an_http_error(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    with local_server(_AnthropicSseErrorHandler) as base_url:
        monkeypatch.setenv("ANTHROPIC_API_KEY", "test-key")
        adapter = AnthropicAdapter(base_url=base_url)
        with pytest.raises(AdapterCallError) as exc:
            adapter.generate(model="m", prompt="x")

        assert "overloaded" in str(exc.value)
        # overloaded_error maps to Anthropic's own 529 -- 5xx is retryable.
        assert exc.value.retry_info.retryable is True
        assert exc.value.retry_info.status == 529


def test_streamed_timeout_classifies_the_same_as_a_non_streamed_one() -> None:
    with local_server(_NeverRespondsHandler) as base_url:
        streamed = OllamaAdapter(base_url=base_url)
        plain = OllamaAdapter(base_url=base_url, streams=False)

        with pytest.raises(AdapterCallError) as streamed_exc:
            streamed.generate(model="m", prompt="x", timeout_seconds=1)
        with pytest.raises(AdapterCallError) as plain_exc:
            plain.generate(model="m", prompt="x", timeout_seconds=1)

        assert streamed_exc.value.retry_info.retryable is True
        assert plain_exc.value.retry_info.retryable is True
        assert "curl exit 28" in str(streamed_exc.value)
        assert "curl exit 28" in str(plain_exc.value)


def test_streamed_timeout_after_real_deltas_never_leaks_partial_text() -> None:
    """A timeout reached only after the model has
    already answered something must fail with the same text an empty-body
    timeout gives -- never the partial reply sitting in curl's own
    ``--no-buffer``-flushed stdout."""
    with local_server(_OllamaTwoDeltasThenHangsHandler) as base_url:
        adapter = OllamaAdapter(base_url=base_url)
        with pytest.raises(AdapterCallError) as exc:
            adapter.generate(model="m", prompt="x", timeout_seconds=1)

        message = str(exc.value)
        assert "curl exit 28" in message
        assert "hello" not in message
        assert "there" not in message
        assert exc.value.retry_info.retryable is True


def test_streamed_disconnect_after_real_deltas_never_leaks_partial_text() -> None:
    """Same as above, for a genuine mid-stream disconnect (curl exit 18)
    rather than a timeout."""
    with local_server(_OllamaShortContentLengthHandler) as base_url:
        adapter = OllamaAdapter(base_url=base_url)
        with pytest.raises(AdapterCallError) as exc:
            adapter.generate(model="m", prompt="x", timeout_seconds=5)

        message = str(exc.value)
        assert "curl exit 18" in message
        assert "hello" not in message
        assert "there" not in message
        assert exc.value.retry_info.retryable is True


def test_ollama_stream_level_error_mid_stream_fails_like_an_http_error() -> None:
    with local_server(_OllamaErrorMidStreamHandler) as base_url:
        adapter = OllamaAdapter(base_url=base_url)
        with pytest.raises(AdapterCallError) as exc:
            adapter.generate(model="m", prompt="x")

        assert "model is overloaded" in str(exc.value)
        assert exc.value.retry_info.retryable is False


def test_ollama_incomplete_stream_without_done_fails_retryably_and_discards_partial_text() -> None:
    with local_server(_OllamaIncompleteNoDoneHandler) as base_url:
        adapter = OllamaAdapter(base_url=base_url)
        with pytest.raises(AdapterCallError) as exc:
            adapter.generate(model="m", prompt="x")

        assert exc.value.retry_info.retryable is True
        assert "curl exit 18" in str(exc.value)


def test_openai_compatible_stream_level_error_mid_stream_fails_like_an_http_error(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    with local_server(_OpenAICompatibleSseErrorHandler) as base_url:
        monkeypatch.setenv("GROQ_API_KEY", "test-key")
        adapter = GroqAdapter(base_url=base_url)
        with pytest.raises(AdapterCallError) as exc:
            adapter.generate(model="m", prompt="x")

        assert "rate limited" in str(exc.value)
        assert exc.value.retry_info.retryable is True
        assert exc.value.retry_info.status == 429


def test_openai_compatible_incomplete_sse_stream_fails_retryably(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    with local_server(_OpenAICompatibleIncompleteNoSentinelHandler) as base_url:
        monkeypatch.setenv("GROQ_API_KEY", "test-key")
        adapter = GroqAdapter(base_url=base_url)
        with pytest.raises(AdapterCallError) as exc:
            adapter.generate(model="m", prompt="x")

        assert exc.value.retry_info.retryable is True
        assert "curl exit 18" in str(exc.value)


def test_finish_reason_length_is_read_from_the_final_sse_event(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    with local_server(_SseLengthHandler) as base_url:
        monkeypatch.setenv("GROQ_API_KEY", "test-key")
        adapter = GroqAdapter(base_url=base_url)
        result = adapter.generate(model="m", prompt="x")

        assert result.text == "cut off"
        assert result.finish_reason == "length"
        assert result.tokens_sent == 3
        assert result.tokens_received == 2


def test_openai_adapters_own_streamed_success_assembles_text_and_usage(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    with local_server(_OpenAISseSuccessHandler) as base_url:
        monkeypatch.setenv("OPENAI_API_KEY", "sk-test")
        adapter = OpenAIAdapter(base_url=base_url)
        deltas: list[str] = []
        result = adapter.generate(
            model="m",
            prompt="x",
            options=GenerateOptions(on_progress=lambda p: deltas.append(str(p.delta_chars))),
        )

        assert result.text == "hello there"
        assert result.finish_reason == "stop"
        assert result.tokens_sent == 4
        assert result.tokens_received == 2
        assert deltas == ["6", "5"]
