"""Unit tests for `mock_http.MockHttpServer`: fixture validation, matcher
consumption order, and request recording -- independent of any conformance
case or subprocess, the lowest-level proof the server itself behaves as
`electricity/DESIGN.md` §12 and the README describe."""

from __future__ import annotations

import json
import urllib.error
import urllib.request
from pathlib import Path

import pytest

from .mock_http import MockHttpFixtureError, MockHttpServer


def _write_fixture(tmp_path: Path, text: str, name: str = "fixture.yaml") -> Path:
    path = tmp_path / name
    path.write_text(text, encoding="utf-8")
    return path


def test_responds_with_fixtures_response_and_records_the_request(tmp_path: Path) -> None:
    fixture = _write_fixture(
        tmp_path,
        """
        - request:
            method: GET
            path: /hello
          response:
            status: 200
            reason: OK
            headers:
              Content-Type: text/plain
            body: "hi"
        """,
    )
    with MockHttpServer(fixture) as server:
        resp = urllib.request.urlopen(
            f"http://127.0.0.1:{server.port}/hello?x=1", timeout=5
        )
        assert resp.status == 200
        assert resp.read() == b"hi"

    assert len(server.requests) == 1
    recorded = server.requests[0]
    assert recorded.method == "GET"
    assert recorded.path == "/hello"
    assert recorded.query == [["x", "1"]]
    assert "user-agent" not in {k.lower() for k in recorded.headers}
    assert recorded.body == ""


def test_never_sends_a_date_or_server_header(tmp_path: Path) -> None:
    fixture = _write_fixture(
        tmp_path,
        """
        - request: {method: GET, path: /x}
          response: {status: 200, body: "ok"}
        """,
    )
    with MockHttpServer(fixture) as server:
        resp = urllib.request.urlopen(f"http://127.0.0.1:{server.port}/x", timeout=5)
        header_names = {k.lower() for k in resp.headers}
    assert "date" not in header_names
    assert "server" not in header_names


def test_matchers_are_consumed_strictly_in_order(tmp_path: Path) -> None:
    fixture = _write_fixture(
        tmp_path,
        """
        - request: {method: GET, path: /first}
          response: {status: 200, body: "1"}
        - request: {method: GET, path: /second}
          response: {status: 200, body: "2"}
        """,
    )
    with MockHttpServer(fixture) as server:
        first = urllib.request.urlopen(f"http://127.0.0.1:{server.port}/first", timeout=5)
        assert first.read() == b"1"
        second = urllib.request.urlopen(f"http://127.0.0.1:{server.port}/second", timeout=5)
        assert second.read() == b"2"


def test_a_request_that_does_not_match_the_next_matcher_gets_a_diagnostic_response(
    tmp_path: Path,
) -> None:
    fixture = _write_fixture(
        tmp_path,
        """
        - request: {method: GET, path: /expected}
          response: {status: 200, body: "ok"}
        """,
    )
    with MockHttpServer(fixture) as server:
        with pytest.raises(urllib.error.HTTPError) as exc_info:
            urllib.request.urlopen(f"http://127.0.0.1:{server.port}/unexpected", timeout=5)
        assert exc_info.value.code == 599
        body = exc_info.value.read().decode("utf-8")
        assert "/expected" in body
        assert "/unexpected" in body


def test_a_request_with_no_matchers_left_gets_a_diagnostic_response(tmp_path: Path) -> None:
    fixture = _write_fixture(tmp_path, "[]")
    with MockHttpServer(fixture) as server:
        with pytest.raises(urllib.error.HTTPError) as exc_info:
            urllib.request.urlopen(f"http://127.0.0.1:{server.port}/anything", timeout=5)
        assert exc_info.value.code == 599


@pytest.mark.parametrize(
    "text",
    [
        "{}",
        json.dumps([{"request": {"method": "GET", "path": "/x"}}]),
        json.dumps(
            [{"request": {"method": "GET", "path": "/x"}, "response": {"headers": "nope"}}]
        ),
        json.dumps([{"request": {"method": "GET"}, "response": {"status": 200}}]),
        json.dumps(
            [{"request": {"method": "GET", "path": "/x"}, "response": {"status": 200, "huh": 1}}]
        ),
    ],
    ids=[
        "not-a-list",
        "response-missing-status",
        "response-headers-not-a-mapping",
        "request-missing-path",
        "response-unknown-key",
    ],
)
def test_rejects_malformed_fixtures(tmp_path: Path, text: str) -> None:
    fixture = _write_fixture(tmp_path, text)
    with pytest.raises(MockHttpFixtureError):
        MockHttpServer(fixture)
