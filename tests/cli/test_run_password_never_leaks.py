"""A full `cof run` with a password embedded in a URL: no surface the run
writes or prints may ever carry it (issue #819) -- stdout, stderr, `--out`,
`--events`, `--live-state`, and `last-run.json`, the effective-settings
snapshot included.
"""

from __future__ import annotations

import http.server
import json
import threading
from pathlib import Path

import pytest
from typer.testing import CliRunner

from circuitry.cli import app as app_module
from circuitry.cli.app import app

PASSWORD = "zq8mx2vt7h3kqf4w"


def _assert_no_piece_leaks(text: str, secret: str, *, min_len: int = 4, label: str = "") -> None:
    """No contiguous piece of *secret* at least *min_len* characters long
    appears anywhere in *text* -- stronger than checking for the whole
    secret, which would miss a redactor that only cuts part of it."""
    for start in range(len(secret) - min_len + 1):
        piece = secret[start : start + min_len]
        assert piece not in text, f"password fragment {piece!r} leaked via {label}"


class _OkHandler(http.server.BaseHTTPRequestHandler):
    def do_GET(self) -> None:
        body = b"ok"
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args: object) -> None:
        return None


@pytest.fixture
def mock_server() -> int:
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), _OkHandler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield server.server_address[1]
    finally:
        server.shutdown()
        server.server_close()


def test_password_in_url_never_reaches_any_output_surface(
    monkeypatch: pytest.MonkeyPatch, tmp_path: Path, mock_server: int
) -> None:
    fake_last_run = tmp_path / "config-home" / "last-run.json"
    fake_last_run.parent.mkdir(parents=True, exist_ok=True)
    monkeypatch.setattr(app_module, "_LAST_RUN_PATH", fake_last_run)

    # A password hidden behind a `/`, past a REAL host:port the request
    # actually reaches: the colon-before-a-later-@ rule never
    # special-cases a port-shaped `:` suffix, so the credential has to
    # sit on its own colon further down the path, never on the real
    # authority's own (valid, numeric) one -- a plain `user:pass@host`
    # shape is already covered by the ordinary-string redaction.
    url = f"http://127.0.0.1:{mock_server}/path:{PASSWORD}@decoyhost/secret"

    # The URL is a static literal in the document, not a `-e` input:
    # `-e`-supplied values are the user's own data and are always
    # echoed verbatim into `input` by design (that's what `-e key=value`
    # means), so a password passed that way is expected to show up
    # there -- this test instead exercises every surface Circuitry
    # itself derives and writes (meta.raw, meta.params_rendered, the
    # events/live-state/out files, last-run.json), the ones issue #819
    # is actually about.
    orch_path = tmp_path / "orch.yml"
    orch_path.write_text(
        (
            "effects:\n"
            "  - type: tool\n"
            "    name: result\n"
            "    provider: http\n"
            "    params:\n"
            f"      url: \"{url}\"\n"
            "      method: GET\n"
        ),
        encoding="utf-8",
    )

    # The configured adapter's own base_url also carries the credential
    # (issue #819), so the effective-settings snapshot path is covered
    # too, not just the tool's own params/raw.
    # A credential query key, no userinfo at all -- the query-masking
    # half of the rule, covered separately from the userinfo cut above.
    cfg_path = tmp_path / "config.json"
    cfg_path.write_text(
        json.dumps(
            {
                "default_adapter": "ollama",
                "default_model": "llama3.1:8b",
                "runtime": {
                    "adapters": {
                        "ollama": {
                            "base_url": (
                                f"http://127.0.0.1:{mock_server}/?api_key={PASSWORD}"
                            )
                        }
                    }
                },
            }
        ),
        encoding="utf-8",
    )

    out_path = tmp_path / "out.json"
    events_path = tmp_path / "events.jsonl"
    live_state_path = tmp_path / "live.json"

    runner = CliRunner()
    result = runner.invoke(
        app,
        [
            "run",
            str(orch_path),
            "--config",
            str(cfg_path),
            "--out",
            str(out_path),
            "--events",
            str(events_path),
            "--live-state",
            str(live_state_path),
            "--quiet",
        ],
    )
    assert result.exit_code == 0, result.output

    surfaces = {
        "stdout_and_stderr": result.output,
        "out": out_path.read_text(encoding="utf-8"),
        "events": events_path.read_text(encoding="utf-8"),
        "live_state": live_state_path.read_text(encoding="utf-8"),
        "last_run": fake_last_run.read_text(encoding="utf-8"),
    }
    for name, text in surfaces.items():
        _assert_no_piece_leaks(text, PASSWORD, label=name)
