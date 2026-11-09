"""Streamed-call failure semantics match the non-streamed path's (#674):
an error body, a timeout and a mid-stream disconnect classify, and read,
exactly the same whether or not the request streamed. Real curl against a
real loopback server throughout -- the streaming code path always calls
``run_curl_streaming``, which `on_line`-reads from curl's own real
``stdout`` pipe, so a mocked ``subprocess.Popen`` (as the plain
curl-hygiene tests use) cannot stand in for it.
"""

from __future__ import annotations

import http.server
import json

import pytest
from curl_test_support import local_server

from circuitry.adapters._retry import AdapterCallError
from circuitry.adapters.anthropic import AnthropicAdapter
from circuitry.adapters.groq import GroqAdapter
from circuitry.adapters.ollama import OllamaAdapter


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
        import time

        time.sleep(5)

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


def test_streamed_400_error_body_matches_non_streamed_text() -> None:
    with local_server(_JsonErrorHandler) as base_url:
        streamed = AnthropicAdapter(base_url=base_url)
        plain = AnthropicAdapter(base_url=base_url, streams=False)
        import os

        os.environ["ANTHROPIC_API_KEY"] = "test-key"
        try:
            with pytest.raises(AdapterCallError) as streamed_exc:
                streamed.generate(model="m", prompt="x")
            with pytest.raises(AdapterCallError) as plain_exc:
                plain.generate(model="m", prompt="x")
        finally:
            del os.environ["ANTHROPIC_API_KEY"]

        assert str(streamed_exc.value) == str(plain_exc.value)
        assert "bad request" in str(streamed_exc.value)
        assert streamed_exc.value.retry_info.retryable is False
        assert streamed_exc.value.retry_info.status == 400


def test_anthropic_stream_level_error_event_classifies_like_an_http_error() -> None:
    with local_server(_AnthropicSseErrorHandler) as base_url:
        import os

        os.environ["ANTHROPIC_API_KEY"] = "test-key"
        try:
            adapter = AnthropicAdapter(base_url=base_url)
            with pytest.raises(AdapterCallError) as exc:
                adapter.generate(model="m", prompt="x")
        finally:
            del os.environ["ANTHROPIC_API_KEY"]

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


def test_finish_reason_length_is_read_from_the_final_sse_event() -> None:
    with local_server(_SseLengthHandler) as base_url:
        adapter = GroqAdapter(base_url=base_url)
        import os

        os.environ["GROQ_API_KEY"] = "test-key"
        try:
            result = adapter.generate(model="m", prompt="x")
        finally:
            del os.environ["GROQ_API_KEY"]

        assert result.text == "cut off"
        assert result.finish_reason == "length"
        assert result.tokens_sent == 3
        assert result.tokens_received == 2
