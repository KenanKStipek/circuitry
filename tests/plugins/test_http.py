"""Tests for the http tool plugin."""

from __future__ import annotations

import io
import json as _json
from typing import Any
from urllib.error import HTTPError, URLError

import pytest

from circuitry.plugins import build_plugin
from circuitry.plugins.base import validate_tool_result
from circuitry.plugins.http import HttpPlugin

# ---------------------------------------------------------------------------
# urlopen mock helpers
# ---------------------------------------------------------------------------


class _FakeResponse:
    def __init__(
        self, *, status: int, body: bytes, headers: dict[str, str]
    ) -> None:
        self.status = status
        self._body = body
        self.headers = headers

    def read(self) -> bytes:
        return self._body

    def __enter__(self) -> _FakeResponse:
        return self

    def __exit__(self, *args: Any) -> None:
        return None


def _install_fake_urlopen(
    monkeypatch: pytest.MonkeyPatch,
    *,
    status: int = 200,
    body: str = "",
    headers: dict[str, str] | None = None,
    capture: dict[str, Any] | None = None,
) -> None:
    headers = headers or {}

    def fake_urlopen(req: Any, timeout: int = 0) -> _FakeResponse:
        if capture is not None:
            capture["url"] = req.full_url
            capture["method"] = req.get_method()
            capture["headers"] = dict(req.header_items())
            data = req.data
            capture["body"] = data.decode("utf-8") if isinstance(data, bytes) else data
            capture["timeout"] = timeout
        return _FakeResponse(
            status=status, body=body.encode("utf-8"), headers=headers
        )

    monkeypatch.setattr("urllib.request.urlopen", fake_urlopen)


# ---------------------------------------------------------------------------
# Happy paths
# ---------------------------------------------------------------------------


def test_get_returns_json_when_content_type_says_json(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    capture: dict[str, Any] = {}
    _install_fake_urlopen(
        monkeypatch,
        status=200,
        body=_json.dumps({"hello": "world"}),
        headers={"Content-Type": "application/json"},
        capture=capture,
    )

    plugin = HttpPlugin()
    result = plugin.execute(params={"url": "https://example.test/api"})

    assert validate_tool_result(result, plugin_name="http") == []
    assert result.value == {"hello": "world"}
    assert result.exit_code is None
    assert result.ok is True
    assert result.stderr is None
    assert result.raw["status"] == 200
    assert capture["method"] == "GET"


def test_get_text_when_content_type_is_html(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _install_fake_urlopen(
        monkeypatch,
        status=200,
        body="<html>hi</html>",
        headers={"Content-Type": "text/html"},
    )

    result = HttpPlugin().execute(params={"url": "https://example.test/page"})

    assert result.value == "<html>hi</html>"
    assert result.exit_code is None
    assert result.raw["status"] == 200
    assert validate_tool_result(result, plugin_name="http") == []


def test_post_with_json_body(monkeypatch: pytest.MonkeyPatch) -> None:
    capture: dict[str, Any] = {}
    _install_fake_urlopen(
        monkeypatch,
        status=201,
        body=_json.dumps({"created": True}),
        headers={"Content-Type": "application/json"},
        capture=capture,
    )

    result = HttpPlugin().execute(
        params={
            "url": "https://example.test/items",
            "method": "POST",
            "json": {"name": "x"},
        }
    )

    assert capture["method"] == "POST"
    assert capture["body"] == _json.dumps({"name": "x"})
    # urllib lowercases header keys via header_items but Request is case-
    # preserving in the items list — match either via lower lookup.
    hdrs = {k.lower(): v for k, v in capture["headers"].items()}
    assert hdrs.get("content-type") == "application/json"
    assert result.value == {"created": True}
    assert result.exit_code is None
    assert result.raw["status"] == 201
    assert result.ok is True


def test_query_params_are_appended(monkeypatch: pytest.MonkeyPatch) -> None:
    capture: dict[str, Any] = {}
    _install_fake_urlopen(monkeypatch, status=200, body="ok", capture=capture)

    HttpPlugin().execute(
        params={
            "url": "https://example.test/search",
            "params": {"q": "yaml dsl", "page": 2},
        }
    )
    assert (
        capture["url"]
        == "https://example.test/search?q=yaml+dsl&page=2"
    )


def test_explicit_parse_json_overrides_content_type(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """API returning text/plain that's actually JSON — caller forces parse=json."""
    _install_fake_urlopen(
        monkeypatch,
        status=200,
        body=_json.dumps([1, 2, 3]),
        headers={"Content-Type": "text/plain"},
    )
    result = HttpPlugin().execute(
        params={"url": "https://example.test/", "parse": "json"}
    )
    assert result.value == [1, 2, 3]


def test_explicit_parse_text_keeps_string_for_json_content(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    body = _json.dumps({"a": 1})
    _install_fake_urlopen(
        monkeypatch,
        status=200,
        body=body,
        headers={"Content-Type": "application/json"},
    )
    result = HttpPlugin().execute(
        params={"url": "https://example.test/", "parse": "text"}
    )
    assert result.value == body


# ---------------------------------------------------------------------------
# Error paths
# ---------------------------------------------------------------------------


def _fake_urlopen_404(req: Any, timeout: int = 0) -> Any:
    raise HTTPError(
        url=req.full_url,
        code=404,
        msg="Not Found",
        hdrs={"Content-Type": "application/json"},  # type: ignore[arg-type]
        fp=io.BytesIO(b'{"error":"not found"}'),
    )


def test_http_4xx_sets_ok_false_and_status_in_raw_not_exit_code(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """4xx/5xx fails the tool by default (ok=False); exit_code stays None."""
    monkeypatch.setattr("urllib.request.urlopen", _fake_urlopen_404)

    result = HttpPlugin().execute(params={"url": "https://example.test/missing"})

    assert result.exit_code is None
    assert result.raw["status"] == 404
    assert result.ok is False
    assert "HTTP 404" in (result.stderr or "")
    assert result.value == {"error": "not found"}


def test_http_4xx_with_fail_on_error_false_stays_ok(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The opt-out param restores the old always-succeeds behaviour."""
    monkeypatch.setattr("urllib.request.urlopen", _fake_urlopen_404)

    result = HttpPlugin().execute(
        params={"url": "https://example.test/missing", "fail_on_error": False}
    )

    assert result.ok is True
    assert result.raw["status"] == 404
    assert result.value == {"error": "not found"}


def test_http_4xx_fail_on_error_templated_false_string_stays_ok(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """A Mustache-rendered 'False' string must coerce to boolean False too."""
    monkeypatch.setattr("urllib.request.urlopen", _fake_urlopen_404)

    result = HttpPlugin().execute(
        params={"url": "https://example.test/missing", "fail_on_error": "False"}
    )

    assert result.ok is True


def _fake_urlopen_400_with_reason_and_secret(req: Any, timeout: int = 0) -> Any:
    raise HTTPError(
        url=req.full_url,
        code=400,
        msg="Bad Request",
        hdrs={"Content-Type": "application/json"},  # type: ignore[arg-type]
        fp=io.BytesIO(
            _json.dumps(
                {
                    "error": {
                        "message": "Validation failed: 'amount' is required",
                        "api_key": "sk-canary-DO-NOT-LEAK-0123456789",
                    }
                }
            ).encode()
        ),
    )


def test_http_4xx_stderr_includes_reason_but_not_a_canary_secret(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """Regression for #317: the failure message now includes a bounded,
    redacted excerpt of the response body (the JSON error/message/detail
    field) — a validation reason makes it in, a sibling credential-shaped
    field does not."""
    monkeypatch.setattr("urllib.request.urlopen", _fake_urlopen_400_with_reason_and_secret)

    result = HttpPlugin().execute(params={"url": "https://example.test/items"})

    assert result.ok is False
    assert "HTTP 400" in (result.stderr or "")
    assert "Validation failed" in (result.stderr or "")
    assert "canary" not in (result.stderr or "")


def test_url_error_raises_actionable_runtime_error(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    def fake_urlopen(req: Any, timeout: int = 0) -> Any:
        raise URLError("Name or service not known")

    monkeypatch.setattr("urllib.request.urlopen", fake_urlopen)

    with pytest.raises(RuntimeError, match="Name or service not known"):
        HttpPlugin().execute(params={"url": "https://nonsuch.test/"})


def test_missing_url_raises() -> None:
    with pytest.raises(ValueError, match="params\\['url'\\]"):
        HttpPlugin().execute(params={})


def test_json_and_body_together_raises() -> None:
    with pytest.raises(ValueError, match="not both"):
        HttpPlugin().execute(
            params={
                "url": "https://example.test/",
                "json": {"a": 1},
                "body": "x",
            }
        )


def test_explicit_parse_json_with_invalid_body_raises(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    _install_fake_urlopen(monkeypatch, status=200, body="not-json")
    with pytest.raises(RuntimeError, match="parse='json'"):
        HttpPlugin().execute(
            params={"url": "https://example.test/", "parse": "json"}
        )


# ---------------------------------------------------------------------------
# Factory + check
# ---------------------------------------------------------------------------


def test_factory_builds_http_plugin() -> None:
    plugin = build_plugin(plugin_name="http", runtime={})
    assert plugin.name == "http"
    assert isinstance(plugin, HttpPlugin)


def test_check_returns_ok() -> None:
    r = HttpPlugin().check()
    assert r.ok is True
    assert r.missing == []
