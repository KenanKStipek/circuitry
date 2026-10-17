"""``http``'s ``save_to``: a binary response body saved to a file (#602).

Against the local server pair in ``http_server_support``. The body is a small
PNG carrying a text marker, so a test can show that no part of it reaches
state, events or output: the file holds it, nothing else does.
"""

from __future__ import annotations

import errno
import hashlib
import json
import os
import struct
import subprocess
import sys
import zlib
from collections.abc import Iterator
from pathlib import Path
from typing import Any

import pytest
import yaml
from http_server_support import PROXY_ENV_VARS, Route, ServerPair, serve_pair

from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, run, validate

BODY_MARKER = b"save-to-body-marker-602"
SECRET_ENV = "CIRCUITRY_TEST_SECRET_602"
TOKEN = "s3cr3t-token-602-abcdef"

_CREDENTIAL_ENV_VARS = ("OPENAI_API_KEY", "CYBERDINER_TOKEN", "CYBERDINER_EXPO_URL")


def _png() -> bytes:
    """A 2x2 PNG with a tEXt chunk holding BODY_MARKER: bytes that are not
    UTF-8, NULs included, and one marker a record would show if it held any
    of the body."""

    def chunk(kind: bytes, data: bytes) -> bytes:
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    pixels = b"\x00\xff\x00\x00\x00\x00\xff" + b"\x00\x00\x00\xff\xff\xff\xff"
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", 2, 2, 8, 2, 0, 0, 0))
        + chunk(b"tEXt", b"Comment\x00" + BODY_MARKER)
        + chunk(b"IDAT", zlib.compress(pixels))
        + chunk(b"IEND", b"")
    )


PNG = _png()
PNG_SHA256 = hashlib.sha256(PNG).hexdigest()


@pytest.fixture
def servers(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Iterator[ServerPair]:
    yield from serve_pair(tmp_path, monkeypatch)


@pytest.fixture
def work(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    """The run's working directory, which a ``save_to`` without ``within``
    resolves against."""
    directory = tmp_path / "work"
    directory.mkdir()
    monkeypatch.chdir(directory)
    return directory.resolve()


def _write(directory: Path, name: str, content: str) -> Path:
    path = directory / name
    path.write_text(content.lstrip(), encoding="utf-8")
    return path


def _doc(directory: Path, params: str, *, on_error: str = "fail", name: str = "doc.yml") -> Path:
    return _write(
        directory,
        name,
        f"""
effects:
  - type: tool
    name: download
    provider: http
    on_error: {on_error}
    params:
{params}
""",
    )


def _run(path: Path, *, trusted: bool = True, events: Path | None = None, **runtime: Any) -> Any:
    return run(
        RunRequest(
            orchestration_path=path,
            state_path=None,
            out_path=None,
            events_path=events,
            dry_run=False,
            validate_only=False,
            config=CircuitryConfig(runtime=runtime),
            skip_preflight=True,
            trust_document=trusted,
        )
    )


def _files(directory: Path) -> list[str]:
    """Every file under *directory* (temporary ones included), relative."""
    return sorted(str(path.relative_to(directory)) for path in directory.rglob("*") if path.is_file())


def _cof(cwd: Path, *args: str) -> subprocess.CompletedProcess[str]:
    home = cwd / "home"
    home.mkdir(exist_ok=True)
    env = {
        key: value
        for key, value in os.environ.items()
        if key not in _CREDENTIAL_ENV_VARS and key not in PROXY_ENV_VARS
    }
    env.update(HOME=str(home), COLUMNS="200")
    return subprocess.run(
        [sys.executable, "-m", "circuitry.cli.app", *args],
        capture_output=True,
        text=True,
        env=env,
        cwd=cwd,
        timeout=120,
        check=False,
    )


# ---------------------------------------------------------------------------
# The body goes to the file, and only there.
# ---------------------------------------------------------------------------


def test_a_png_is_saved_byte_identical_and_no_record_holds_the_body(
    tmp_path: Path, servers: ServerPair
) -> None:
    """Acceptance 1 and 2, through `cof run`."""
    servers.routes["/image.png"] = Route(headers={"Content-Type": "image/png"}, body=PNG)
    work = tmp_path / "work"
    work.mkdir()
    doc = _doc(work, f'      url: "{servers.http}/image.png"\n      save_to: images/out.png')
    out, events = work / "out.json", work / "events.jsonl"

    proc = _cof(work, "run", str(doc), "--out", str(out), "--events", str(events), "--verbose")

    assert proc.returncode == 0, proc.stdout + proc.stderr
    saved = work.resolve() / "images" / "out.png"
    assert saved.read_bytes() == PNG
    state = json.loads(out.read_text())
    download = state["prime"]["download"]
    assert download["value"] == {
        "path": str(saved),
        "bytes": len(PNG),
        "sha256": PNG_SHA256,
        "content_type": "image/png",
        "status": 200,
    }
    assert "body" not in download["meta"]["raw"]
    for text in (out.read_text(), events.read_text(), proc.stdout, proc.stderr):
        assert BODY_MARKER.decode() not in text
        assert "IHDR" not in text
    assert _files(work / "images") == ["out.png"]


def test_a_cross_origin_redirect_under_strip_auth_saves_without_the_secret_header(
    work: Path, servers: ServerPair, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Acceptance 3, with the header from a secret (strip_auth by default)."""
    monkeypatch.setenv(SECRET_ENV, TOKEN)
    servers.routes["/attachment"] = Route(302, {"Location": f"{servers.https_localhost}/blob"})
    servers.routes["/blob"] = Route(headers={"Content-Type": "image/png"}, body=PNG)
    doc = _doc(
        work,
        f'      url: "{servers.https}/attachment"\n'
        "      headers: {Authorization: {secret: tok, prefix: \"Bearer \"}}\n"
        "      save_to: attachment.png",
    )

    result = _run(doc, secrets={"tok": {"env": SECRET_ENV}})

    assert result.ok, result.error
    assert (work / "attachment.png").read_bytes() == PNG
    assert result.state["prime"]["download"]["value"]["sha256"] == PNG_SHA256
    assert servers.arrived("/attachment").headers["authorization"] == f"Bearer {TOKEN}"
    assert "authorization" not in servers.arrived("/blob").headers


def test_save_to_creates_missing_directories_and_overwrite_replaces_a_file(
    work: Path, servers: ServerPair
) -> None:
    servers.routes["/image.png"] = Route(body=PNG)
    target = work / "a" / "b" / "image.png"
    target.parent.mkdir(parents=True)
    target.write_bytes(b"old")
    doc = _doc(
        work,
        f'      url: "{servers.http}/image.png"\n      save_to: a/b/image.png\n      overwrite: true',
    )

    result = _run(doc)

    assert result.ok, result.error
    assert target.read_bytes() == PNG
    assert _files(work / "a") == ["b/image.png"]


def test_within_resolves_save_to_inside_a_directory_from_input(
    tmp_path: Path, work: Path, servers: ServerPair
) -> None:
    downloads = tmp_path / "downloads"
    downloads.mkdir()
    servers.routes["/image.png"] = Route(body=PNG)
    doc = _write(
        work,
        "doc.yml",
        f"""
interface:
  inputs:
    dir: {{type: string, default: "{downloads}"}}
effects:
  - type: tool
    name: download
    provider: http
    params:
      url: "{servers.http}/image.png"
      within: {{from: input.dir}}
      save_to: image.png
""",
    )

    result = _run(doc)

    assert result.ok, result.error
    assert (downloads / "image.png").read_bytes() == PNG
    assert result.state["prime"]["download"]["value"]["path"] == str(downloads.resolve() / "image.png")


# ---------------------------------------------------------------------------
# Refused before the request.
# ---------------------------------------------------------------------------


def _refused(doc: Path, servers: ServerPair, message: str) -> None:
    result = _run(doc)
    assert not result.ok
    assert message in result.state["prime"]["download"]["meta"]["error"]
    assert servers.received == []


@pytest.mark.parametrize(
    ("save_to", "message"),
    [
        ("../outside.png", "params['save_to'] '../outside.png' resolves outside the working directory"),
        ("out/../../outside.png", "resolves outside the working directory"),
        ("link/escape.png", "params['save_to'] 'link/escape.png' resolves outside the working directory"),
        ("link", "params['save_to'] 'link' is a symlink; it is never replaced."),
        ("inner-link", "params['save_to'] 'inner-link' is a symlink; it is never replaced."),
        ("folder", "params['save_to'] 'folder' exists and is not a regular file."),
        ("folder/..", "params['save_to'] 'folder/..' does not name a file."),
    ],
)
def test_a_path_outside_the_working_directory_or_through_a_symlink_is_refused_before_the_request(
    tmp_path: Path, work: Path, servers: ServerPair, save_to: str, message: str
) -> None:
    """Acceptance 4."""
    outside = tmp_path / "outside"
    outside.mkdir()
    (work / "link").symlink_to(outside)
    (work / "kept.png").write_bytes(b"kept")
    (work / "inner-link").symlink_to(work / "kept.png")
    (work / "folder").mkdir()
    servers.routes["/image.png"] = Route(body=PNG)
    doc = _doc(
        work,
        f'      url: "{servers.http}/image.png"\n      save_to: "{save_to}"\n      overwrite: true',
    )

    _refused(doc, servers, message)

    assert list(outside.iterdir()) == []
    assert (work / "kept.png").read_bytes() == b"kept"
    assert not (tmp_path / "outside.png").exists()


def test_an_absolute_save_to_from_state_is_refused_before_the_request(
    tmp_path: Path, work: Path, servers: ServerPair
) -> None:
    target = tmp_path / "absolute.png"
    doc = _write(
        work,
        "doc.yml",
        f"""
interface:
  inputs:
    target: {{type: string, default: "{target}"}}
effects:
  - type: tool
    name: download
    provider: http
    params:
      url: "{servers.http}/image.png"
      save_to: {{from: input.target}}
""",
    )

    _refused(doc, servers, f"params['save_to'] must be a relative path, got {str(target)!r}.")
    assert not target.exists()


def test_a_save_to_escaping_within_is_refused_before_the_request(
    tmp_path: Path, work: Path, servers: ServerPair
) -> None:
    downloads = tmp_path / "downloads"
    downloads.mkdir()
    doc = _doc(
        work,
        f'      url: "{servers.http}/image.png"\n      within: "{downloads}"\n'
        "      save_to: ../escape.png",
    )

    _refused(doc, servers, "params['save_to'] '../escape.png' resolves outside params['within']")
    assert not (tmp_path / "escape.png").exists()


def test_an_existing_file_without_overwrite_is_refused_before_the_request(
    work: Path, servers: ServerPair
) -> None:
    """Acceptance 5."""
    (work / "image.png").write_bytes(b"old")
    doc = _doc(work, f'      url: "{servers.http}/image.png"\n      save_to: image.png')

    _refused(
        doc,
        servers,
        "params['save_to'] 'image.png' already exists; set overwrite: true to replace it.",
    )
    assert (work / "image.png").read_bytes() == b"old"


# ---------------------------------------------------------------------------
# Failures leave no file.
# ---------------------------------------------------------------------------


@pytest.mark.parametrize("omit_content_length", [False, True], ids=["declared", "streamed"])
def test_a_body_over_max_bytes_fails_and_leaves_no_file(
    work: Path, servers: ServerPair, omit_content_length: bool
) -> None:
    """Acceptance 6: refused from the Content-Length when the server sends
    one, and while reading otherwise; no partial or temporary file stays."""
    servers.routes["/image.png"] = Route(body=PNG, omit_content_length=omit_content_length)
    doc = _doc(
        work,
        f'      url: "{servers.http}/image.png"\n      save_to: out/image.png\n'
        f"      max_bytes: {len(PNG) - 1}",
    )

    result = _run(doc)

    assert not result.ok
    error = result.state["prime"]["download"]["meta"]["error"]
    assert f"over max_bytes ({len(PNG) - 1}); nothing was saved." in error
    assert _files(work) == ["doc.yml"]


def test_a_connection_lost_mid_body_fails_and_leaves_no_file(
    work: Path, servers: ServerPair
) -> None:
    servers.routes["/image.png"] = Route(body=PNG, content_length=len(PNG) + 1000)
    doc = _doc(work, f'      url: "{servers.http}/image.png"\n      save_to: image.png')

    result = _run(doc)

    assert not result.ok
    error = result.state["prime"]["download"]["meta"]["error"]
    assert "failed while reading the body after" in error
    assert "nothing was saved" in error
    assert BODY_MARKER.decode() not in json.dumps(result.state)
    assert _files(work) == ["doc.yml"]


def test_a_404_writes_nothing(work: Path, servers: ServerPair) -> None:
    """Acceptance 7: the effect fails with today's text excerpt; with
    fail_on_error: false it succeeds, with nothing written and the status
    on value."""
    servers.routes["/missing.png"] = Route(404, {"Content-Type": "text/plain"}, "no such image")
    doc = _write(
        work,
        "doc.yml",
        f"""
effects:
  - type: tool
    name: strict
    provider: http
    on_error: continue
    params: {{url: "{servers.http}/missing.png", save_to: strict.png}}
  - type: tool
    name: lenient
    provider: http
    params: {{url: "{servers.http}/missing.png", save_to: lenient.png, fail_on_error: false}}
""",
    )

    result = _run(doc)

    assert result.ok, result.error
    strict = result.state["prime"]["strict"]
    assert strict["meta"]["error"] == "HTTP 404: Not Found: no such image"
    lenient = result.state["prime"]["lenient"]
    assert lenient["meta"]["error"] is None
    assert lenient["value"] == {
        "path": None,
        "bytes": 0,
        "sha256": None,
        "content_type": "text/plain",
        "status": 404,
    }
    assert "body" not in lenient["meta"]["raw"]
    assert _files(work) == ["doc.yml"]


def test_a_refused_redirect_writes_nothing(work: Path, servers: ServerPair) -> None:
    """Under `redirects: never`, and under `strip_auth` to `http`: the
    redirect is not followed and nothing is written, with or without
    fail_on_error."""
    servers.routes["/start"] = Route(302, {"Location": "/image.png"}, body="moved")
    servers.routes["/secure-start"] = Route(302, {"Location": f"{servers.http}/image.png"}, body="")
    servers.routes["/image.png"] = Route(headers={"Content-Type": "image/png"}, body=PNG)
    doc = _write(
        work,
        "doc.yml",
        f"""
effects:
  - type: tool
    name: strict
    provider: http
    on_error: continue
    params: {{url: "{servers.http}/start", redirects: never, save_to: strict.png}}
  - type: tool
    name: lenient
    provider: http
    params:
      url: "{servers.http}/start"
      redirects: never
      save_to: lenient.png
      fail_on_error: false
  - type: tool
    name: to_http
    provider: http
    on_error: continue
    params: {{url: "{servers.https}/secure-start", redirects: strip_auth, save_to: to-http.png}}
""",
    )

    result = _run(doc)

    assert result.ok, result.error
    prime = result.state["prime"]
    assert prime["strict"]["meta"]["error"] == (
        "HTTP 302: Found: redirect not followed (redirects: never): moved"
    )
    assert prime["lenient"]["meta"]["error"] is None
    assert prime["lenient"]["value"]["path"] is None
    assert prime["lenient"]["value"]["status"] == 302
    assert prime["to_http"]["meta"]["error"] == (
        "HTTP 302: Found: redirect not followed (redirects: strip_auth follows https only)"
    )
    assert [request.path for request in servers.received] == ["/start", "/start", "/secure-start"]
    assert _files(work) == ["doc.yml"]


@pytest.mark.parametrize(
    "code", [errno.ENOTSUP, errno.EPERM, errno.EXDEV], ids=["ENOTSUP", "EPERM", "EXDEV"]
)
def test_without_hard_links_the_file_is_still_saved(
    work: Path, servers: ServerPair, monkeypatch: pytest.MonkeyPatch, code: int
) -> None:
    """A filesystem without hard links (FAT, exFAT, some shares) saves
    through an exclusively created placeholder instead."""

    def no_link(src: Any, dst: Any) -> None:
        raise OSError(code, os.strerror(code))

    monkeypatch.setattr(os, "link", no_link)
    servers.routes["/image.png"] = Route(body=PNG)
    doc = _doc(work, f'      url: "{servers.http}/image.png"\n      save_to: image.png')

    result = _run(doc)

    assert result.ok, result.error
    assert (work / "image.png").read_bytes() == PNG
    assert _files(work) == ["doc.yml", "image.png"]


def test_without_hard_links_a_file_that_appears_during_the_download_is_kept(
    work: Path, servers: ServerPair, monkeypatch: pytest.MonkeyPatch
) -> None:
    def no_link_and_a_racing_writer(src: Any, dst: Any) -> None:
        Path(dst).write_bytes(b"theirs")
        raise OSError(errno.ENOTSUP, os.strerror(errno.ENOTSUP))

    monkeypatch.setattr(os, "link", no_link_and_a_racing_writer)
    servers.routes["/image.png"] = Route(body=PNG)
    doc = _doc(work, f'      url: "{servers.http}/image.png"\n      save_to: image.png')

    result = _run(doc)

    assert not result.ok
    assert "appeared during the download; nothing was replaced." in (
        result.state["prime"]["download"]["meta"]["error"]
    )
    assert (work / "image.png").read_bytes() == b"theirs"
    assert _files(work) == ["doc.yml", "image.png"]


def test_without_hard_links_a_failed_replace_leaves_no_placeholder(
    work: Path, servers: ServerPair, monkeypatch: pytest.MonkeyPatch
) -> None:
    def no_link(src: Any, dst: Any) -> None:
        raise OSError(errno.ENOTSUP, os.strerror(errno.ENOTSUP))

    def no_replace(src: Any, dst: Any) -> None:
        raise OSError(errno.EIO, os.strerror(errno.EIO))

    monkeypatch.setattr(os, "link", no_link)
    monkeypatch.setattr(os, "replace", no_replace)
    servers.routes["/image.png"] = Route(body=PNG)
    doc = _doc(work, f'      url: "{servers.http}/image.png"\n      save_to: image.png')

    result = _run(doc)

    assert not result.ok
    assert "could not save" in result.state["prime"]["download"]["meta"]["error"]
    assert _files(work) == ["doc.yml"]


# ---------------------------------------------------------------------------
# Only a document run by path saves a file.
# ---------------------------------------------------------------------------


_LIMITED = (
    "only a document run by path may save a response to a file (a limited document, "
    "a library entry or a generated plan cannot)"
)


@pytest.mark.parametrize(
    ("params", "message"),
    [
        ('{url: "URL", save_to: image.png}', f"params.save_to: {_LIMITED}"),
        ('{url: "URL", save_to: {from: input.name}}', f"params.save_to: {_LIMITED}"),
        (
            '{url: "URL"}\n    params_json: \'{"save_to": "image.png"}\'',
            f"params_json sets save_to: {_LIMITED}",
        ),
        (
            '{url: "URL"}\n    params_json: \'{"{{input.key}}": "image.png"}\'',
            f"HttpPlugin: params['save_to']: {_LIMITED}.",
        ),
    ],
    ids=["literal", "from", "params_json", "params_json-rendered"],
)
def test_a_limited_document_never_saves_a_file(
    tmp_path: Path, servers: ServerPair, params: str, message: str
) -> None:
    """Through `cof run` by library name: refused when it compiles, or, for a
    key only rendering reveals, when the effect dispatches; never a request
    and never a file."""
    servers.routes["/image.png"] = Route(body=PNG)
    folder = tmp_path / "lib"
    folder.mkdir()
    _write(
        folder,
        "shared_doc.yml",
        f"""
interface:
  inputs:
    name: {{type: string, default: image.png}}
    key: {{type: string, default: save_to}}
effects:
  - type: tool
    name: download
    provider: http
    params: {params.replace("URL", f"{servers.http}/image.png")}
""",
    )
    config = _write(
        tmp_path,
        "config.json",
        json.dumps(
            {"runtime": {"library": {"sources": [{"type": "folder", "name": "local", "path": str(folder)}]}}}
        ),
    )

    proc = _cof(tmp_path, "run", "shared_doc", "--config", str(config), "--skip-preflight")

    assert proc.returncode != 0
    assert message in proc.stdout + proc.stderr
    assert servers.received == []
    assert not (tmp_path / "image.png").exists()


@pytest.mark.parametrize(
    ("params", "message"),
    [
        ("save_to: x.bin\n      parse: json", "params.parse must be auto (or unset) with params.save_to"),
        ("save_to: /srv/x.bin", "params.save_to must be a relative path, got '/srv/x.bin'"),
        ('save_to: ""', "params.save_to must be a non-empty relative path"),
        ("save_to: x.bin\n      max_bytes: 0", "params.max_bytes must be a positive integer, got 0"),
        ("save_to: x.bin\n      within: relative/dir", "params.within must be an absolute path"),
        (
            "save_to: x.bin\n    params_json: '{\"within\": \"/srv\"}'",
            "params_json must not set within",
        ),
    ],
)
def test_check_rejects_a_malformed_save_to(tmp_path: Path, params: str, message: str) -> None:
    doc = _doc(tmp_path, f'      url: "https://example.com/x.bin"\n      {params}')
    report = validate(doc, trust_document=True)
    assert not report["ok"]
    assert message in report["errors"][0]


@pytest.mark.parametrize("provider", ["webhook", "web_fetch"])
def test_a_profile_switching_a_limited_documents_effect_to_http_never_saves_a_file(
    tmp_path: Path, work: Path, servers: ServerPair, provider: str
) -> None:
    """A limited document's effect keeps its document's trust when a
    profile switches its provider to http, so its save_to is refused at
    dispatch, before any request."""
    servers.routes["/image.png"] = Route(body=PNG)
    project = tmp_path / "project"
    (project / "profiles").mkdir(parents=True)
    doc = _doc(
        project,
        f'      url: "{servers.http}/image.png"\n      save_to: image.png',
        name="decide.yml",
    )
    (project / "profiles" / "swap.yml").write_text(
        yaml.dump({"effects": {"download": {"provider": "http"}}}), encoding="utf-8"
    )
    doc.write_text(doc.read_text().replace("provider: http", f"provider: {provider}"))

    result = run(
        RunRequest(
            orchestration_path=doc,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            config=CircuitryConfig(),
            skip_preflight=True,
            trust_document=False,
            profile_name="swap",
        )
    )

    assert not result.ok
    assert result.state["prime"]["download"]["meta"]["error"] == (
        f"HttpPlugin: params['save_to']: {_LIMITED}."
    )
    assert servers.received == []
    assert sorted(path.name for path in project.rglob("*") if path.is_file()) == [
        "decide.yml",
        "swap.yml",
    ]
    assert _files(work) == []


def test_a_limited_document_may_not_name_within(tmp_path: Path, servers: ServerPair) -> None:
    downloads = tmp_path / "downloads"
    downloads.mkdir()
    doc = _doc(tmp_path, f'      url: "{servers.http}/image.png"\n      within: "{downloads}"')

    report = validate(doc, trust_document=False)

    assert not report["ok"]
    assert report["errors"] == [
        f"Tool effect 'download' (http) at 'prime.effects[0]': params.within: {_LIMITED}"
    ]
    result = _run(doc, trusted=False)
    assert not result.ok
    assert servers.received == []
    assert validate(doc, trust_document=True)["ok"]
