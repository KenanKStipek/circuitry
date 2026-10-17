"""General-purpose HTTP tool plugin.

Issues a single HTTP request and returns the response. Uses stdlib
``urllib.request`` so circuitry stays dependency-free for this plugin —
unlike the spec's pre-mortem default of ``requests``, the standard
library is sufficient for the plugin's contract (single request, JSON
or text body, basic auth headers) and keeps the dependency footprint
narrow.

Params:
  - ``url`` (required): full URL.
  - ``method`` (optional, default ``"GET"``): HTTP verb.
  - ``headers`` (optional): mapping of header name → value.
  - ``json`` (optional): dict serialised as the request body and a
    ``Content-Type: application/json`` header is added if not present.
  - ``body`` (optional): raw string body, sent as-is (mutually exclusive
    with ``json``).
  - ``params`` (optional): mapping appended as the query string.
  - ``parse`` (optional, ``"json"`` | ``"text"`` | ``"auto"``, default
    ``"auto"``): controls how the response body becomes ``ToolResult.value``.
    ``auto`` parses JSON when the response Content-Type advertises it,
    otherwise returns text.
  - ``fail_on_error`` (optional, bool, default ``True``): a 4xx/5xx response
    sets ``ToolResult.ok = False``, which fails the effect (``meta.error``,
    ``on_error`` applies) exactly like a raised exception. Set ``false`` to
    restore the old behaviour of always succeeding and letting the
    orchestration branch on ``meta.status_code`` itself.
  - ``redirects`` (optional, ``"follow"`` | ``"never"`` | ``"strip_auth"``):
    ``follow`` follows a redirect with the same headers (urllib's own rule);
    ``never`` follows none, and a 3xx response fails like a 4xx;
    ``strip_auth`` follows only to ``https``, and to another origin (scheme,
    host or port) without ``Authorization`` or a header that came from a
    secret. A redirect it refuses fails like a 4xx. The default is
    ``strip_auth`` when a header came from a secret (``secret_headers``),
    ``follow`` otherwise.
  - ``save_to`` (optional, a relative path): write a 2xx body to this file
    as raw bytes instead of decoding it (#602). It resolves against
    ``within`` (an absolute directory; only a document run by path may name
    one) or else the working directory, and must stay inside it once
    symlinks are resolved; missing directories inside it are created. Every
    path problem is refused before the request. ``overwrite`` (default
    ``False``) replaces an existing file; ``max_bytes`` (default
    :data:`DEFAULT_SAVE_TO_MAX_BYTES`) caps the body. The body goes to a
    temporary file next to the target, moved into place only once it is
    complete, so a failure leaves no partial file. A non-2xx response writes
    nothing.

ToolResult shape:
  - ``value``: parsed body per ``parse`` (dict/list for JSON, str for text).
    With ``save_to``: ``{path, bytes, sha256, content_type, status}``, with
    ``path`` and ``sha256`` ``None`` (and ``bytes`` 0) when nothing was written.
  - ``raw``: ``{"status": int, "headers": {...}, "url": str, "body": str}``,
    without ``body`` under ``save_to``.
  - ``stdout``: ``None``. ``stderr``: error message on non-2xx, including a
    bounded, redacted excerpt of the response body when one is available
    (the JSON ``error``/``message``/``detail`` field, or the first ~500
    characters).
  - ``exit_code``: always ``None`` — this plugin issues no process. The HTTP
    status lands on ``meta.status_code`` (from ``raw["status"]``), not here.
  - ``ok``: ``False`` on a 4xx/5xx response, on a redirect ``redirects``
    refused, and on any 3xx under ``redirects: never``, unless
    ``fail_on_error: false``.
"""

from __future__ import annotations

import contextlib
import hashlib
import http.client
import importlib.util
import json as _json
import os
import urllib.error
import urllib.parse
import urllib.request
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import Any

from ..core.prompt_files import path_is_inside
from ..preflight import CheckResult
from .base import ToolResult, _as_bool, http_error_excerpt
from .systemone import param_reference, within_param_errors

REDIRECT_MODES = ("follow", "never", "strip_auth")

#: The note a refused redirect's failure carries, by ``redirects`` mode.
_REFUSED_REDIRECT_NOTES = {
    "never": "redirect not followed (redirects: never)",
    "strip_auth": "redirect not followed (redirects: strip_auth follows https only)",
}

_DEFAULT_PORTS = {"http": 80, "https": 443}

#: The largest body ``save_to`` writes unless ``max_bytes`` says otherwise.
DEFAULT_SAVE_TO_MAX_BYTES = 100 * 1024 * 1024

#: How much of a ``save_to`` body is read at a time.
_SAVE_CHUNK_BYTES = 64 * 1024


def _origin(url: str) -> tuple[str, str, int | None] | None:
    """``(scheme, host, port)`` of *url*, a default port filled in; ``None``
    when the URL has no host or an unreadable port, which matches no origin."""
    parts = urllib.parse.urlsplit(url)
    try:
        port = parts.port
    except ValueError:
        return None
    if not parts.hostname:
        return None
    scheme = parts.scheme.lower()
    return scheme, parts.hostname, port if port is not None else _DEFAULT_PORTS.get(scheme)


class _RedirectPolicy(urllib.request.HTTPRedirectHandler):
    """urllib's redirect handling, under ``redirects: never`` or
    ``strip_auth``. A refused redirect returns no request, so urllib raises
    the 3xx as an ``HTTPError``; :attr:`refused` records that it was refused."""

    def __init__(self, mode: str, auth_headers: frozenset[str]) -> None:
        self.mode = mode
        self.auth_headers = auth_headers
        self.refused = False

    def redirect_request(
        self,
        req: urllib.request.Request,
        fp: Any,
        code: int,
        msg: str,
        headers: Any,
        newurl: str,
    ) -> urllib.request.Request | None:
        if self.mode == "never" or urllib.parse.urlsplit(newurl).scheme.lower() != "https":
            self.refused = True
            return None
        new = super().redirect_request(req, fp, code, msg, headers, newurl)
        origin = _origin(req.full_url)
        if new is not None and (origin is None or origin != _origin(newurl)):
            for header in list(new.headers):
                if header.lower() in self.auth_headers:
                    del new.headers[header]
        return new


#: Why a limited document may not name ``save_to``: it would write files
#: under the ``network`` capability alone (#602).
_LIMITED_SAVE_TO = (
    "only a document run by path may save a response to a file (a limited "
    "document, a library entry or a generated plan cannot)"
)


def static_param_errors(
    params: Mapping[str, Any], *, params_json: str | None, trusted: bool
) -> list[str]:
    """Problems ``cof check`` can see in an ``http`` effect's ``save_to``
    params (#602): only a trusted document may name ``save_to``, in its
    params or in a raw ``params_json`` that parses as JSON, and only its
    literal params may set ``within``. A ``{from: ...}`` value or a template
    is checked when the effect dispatches instead."""
    overlay: Any = None
    if params_json is not None:
        try:
            overlay = _json.loads(params_json)
        except ValueError:
            overlay = None
    overlay_keys = set(overlay) if isinstance(overlay, dict) else set()
    errors: list[str] = []
    if "within" in overlay_keys:
        errors.append("params_json must not set within: only the literal params may")
    if not trusted:
        errors.extend(
            f"params.{key}: {_LIMITED_SAVE_TO}" for key in ("save_to", "within") if key in params
        )
        if "save_to" in overlay_keys:
            errors.append(f"params_json sets save_to: {_LIMITED_SAVE_TO}")
        return errors
    if "save_to" not in params and not (params_json is not None and "within" in params):
        return errors
    save_to = params.get("save_to")
    if "save_to" in params and param_reference(save_to) is None:
        if not isinstance(save_to, str) or not save_to.strip():
            errors.append("params.save_to must be a non-empty relative path")
        elif os.path.isabs(save_to):
            errors.append(f"params.save_to must be a relative path, got {save_to!r}")
    if "within" in params:
        errors.extend(within_param_errors(params["within"]))
    max_bytes = params.get("max_bytes")
    if (
        "max_bytes" in params
        and param_reference(max_bytes) is None
        and not (isinstance(max_bytes, str) and "{{" in max_bytes)
        and not (isinstance(max_bytes, int) and not isinstance(max_bytes, bool) and max_bytes > 0)
    ):
        errors.append(f"params.max_bytes must be a positive integer, got {max_bytes!r}")
    parse = params.get("parse")
    if "save_to" in params and isinstance(parse, str) and parse.lower() != "auto":
        errors.append(
            "params.parse must be auto (or unset) with params.save_to: the body is "
            "saved as bytes, never parsed"
        )
    return errors


def reject_limited_save_to(params: Mapping[str, Any]) -> None:
    """A limited document's ``http`` effect never saves a file, whatever
    supplied ``save_to`` or ``within`` (``params_json``, or a profile that
    switched the provider to ``http``); checked at dispatch."""
    for key in ("save_to", "within"):
        if key in params:
            raise ValueError(f"HttpPlugin: params['{key}']: {_LIMITED_SAVE_TO}.")


@dataclass(frozen=True)
class _SaveTarget:
    """Where a ``save_to`` body goes: :attr:`path` (its directory resolved,
    inside :attr:`base`), checked before the request."""

    base: Path
    path: Path
    overwrite: bool
    max_bytes: int


def _save_target(params: Mapping[str, Any]) -> _SaveTarget | None:
    """``params.save_to`` as a checked :class:`_SaveTarget`, or ``None``
    without one. Raises ``ValueError`` for every path the effect may not
    write, before anything is requested."""
    if "save_to" not in params:
        return None
    raw = params["save_to"]
    if not isinstance(raw, str) or not raw.strip():
        raise ValueError(
            f"HttpPlugin: params['save_to'] must be a non-empty relative path, got {raw!r}."
        )
    if "\0" in raw:
        raise ValueError("HttpPlugin: params['save_to'] contains a NUL byte.")
    if os.path.isabs(raw):
        raise ValueError(f"HttpPlugin: params['save_to'] must be a relative path, got {raw!r}.")
    within = params.get("within")
    if within is None:
        base = Path.cwd().resolve()
    else:
        if not isinstance(within, str) or not os.path.isabs(within):
            raise ValueError(
                f"HttpPlugin: params['within'] must be an absolute directory path, got {within!r}."
            )
        base = Path(within).resolve()
        if not base.is_dir():
            raise ValueError(f"HttpPlugin: params['within'] {within!r} is not a directory.")
    where = "params['within']" if within is not None else "the working directory"

    lexical = base / raw
    if lexical.name in ("", ".", ".."):
        raise ValueError(f"HttpPlugin: params['save_to'] {raw!r} does not name a file.")
    directory = lexical.parent.resolve()
    if not path_is_inside(directory, base):
        raise ValueError(
            f"HttpPlugin: params['save_to'] {raw!r} resolves outside {where} ({base})."
        )
    path = directory / lexical.name
    overwrite = _as_bool(params.get("overwrite"), default=False)
    if path.is_symlink():
        raise ValueError(
            f"HttpPlugin: params['save_to'] {raw!r} is a symlink; it is never replaced."
        )
    if path.exists():
        if not path.is_file():
            raise ValueError(
                f"HttpPlugin: params['save_to'] {raw!r} exists and is not a regular file."
            )
        if not overwrite:
            raise ValueError(
                f"HttpPlugin: params['save_to'] {raw!r} already exists; set overwrite: true "
                "to replace it."
            )

    max_bytes = params.get("max_bytes", DEFAULT_SAVE_TO_MAX_BYTES)
    if isinstance(max_bytes, str) and max_bytes.strip().isdigit():
        max_bytes = int(max_bytes.strip())
    if not isinstance(max_bytes, int) or isinstance(max_bytes, bool) or max_bytes <= 0:
        raise ValueError(
            f"HttpPlugin: params['max_bytes'] must be a positive integer, got {max_bytes!r}."
        )
    return _SaveTarget(base=base, path=path, overwrite=overwrite, max_bytes=max_bytes)


def _save_body(resp: Any, target: _SaveTarget, url: str) -> tuple[int, str]:
    """Write *resp*'s body to *target* and return its size and SHA-256.

    The body goes to a temporary file next to the target, which is removed
    on every failure; only a complete body is moved into place. No error
    quotes any of it."""
    declared_text = resp.headers.get("Content-Length")
    declared = (
        int(declared_text.strip())
        if declared_text is not None and declared_text.strip().isdigit()
        else None
    )
    if declared is not None and declared > target.max_bytes:
        raise RuntimeError(
            f"HttpPlugin: the response body is {declared} bytes, over max_bytes "
            f"({target.max_bytes}); nothing was saved."
        )
    directory = _make_directories(target)
    temp = directory / f".{target.path.name}.{os.urandom(8).hex()}.part"
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    fd = os.open(temp, flags, 0o666)
    try:
        digest = hashlib.sha256()
        size = 0
        with os.fdopen(fd, "wb") as handle:
            while True:
                try:
                    chunk = resp.read(_SAVE_CHUNK_BYTES)
                except (OSError, http.client.HTTPException) as exc:
                    # A ConnectionError, so the effect's retries apply as for
                    # a connection that failed before the response.
                    raise ConnectionError(
                        f"HTTP request to {url} failed while reading the body after {size} "
                        f"bytes ({type(exc).__name__}); nothing was saved."
                    ) from None
                if not chunk:
                    break
                size += len(chunk)
                if size > target.max_bytes:
                    raise RuntimeError(
                        f"HttpPlugin: the response body is over max_bytes ({target.max_bytes}); "
                        "nothing was saved."
                    )
                digest.update(chunk)
                handle.write(chunk)
        if declared is not None and size != declared:
            # A read with a size returns what arrived before the close
            # without raising, so a cut-off body is caught here.
            raise ConnectionError(
                f"HTTP request to {url} failed while reading the body after {size} of "
                f"{declared} bytes (the connection closed); nothing was saved."
            )
        _move_into_place(temp, target)
        return size, digest.hexdigest()
    finally:
        with contextlib.suppress(FileNotFoundError):
            os.unlink(temp)


def _make_directories(target: _SaveTarget) -> Path:
    """Create the target's missing directories one at a time, each checked
    inside the base once symlinks are resolved, and return the last."""
    current = target.base
    for part in target.path.parent.relative_to(target.base).parts:
        current = current / part
        with contextlib.suppress(FileExistsError):
            current.mkdir()
        current = current.resolve()
        if not path_is_inside(current, target.base) or not current.is_dir():
            raise RuntimeError(
                f"HttpPlugin: {current} is not a directory inside {target.base}; nothing was saved."
            )
    if current != target.path.parent:
        raise RuntimeError(
            f"HttpPlugin: {target.path.parent} changed during the download; nothing was saved."
        )
    return current


def _move_into_place(temp: Path, target: _SaveTarget) -> None:
    """Move the complete body into place: replacing a regular file under
    ``overwrite``, and otherwise only where nothing exists yet."""
    path = target.path
    if path.is_symlink() or (path.exists() and not path.is_file()):
        raise RuntimeError(
            f"HttpPlugin: {path} became a symlink or a directory during the download; "
            "nothing was saved."
        )
    try:
        if target.overwrite:
            os.replace(temp, path)
        else:
            try:
                os.link(temp, path)
            except FileExistsError:
                raise
            except OSError:
                # No hard links here (FAT, exFAT, some network shares):
                # claim the name exclusively, then replace the placeholder.
                _replace_exclusively(temp, path)
    except FileExistsError:
        raise RuntimeError(
            f"HttpPlugin: {path} appeared during the download; nothing was replaced."
        ) from None
    except OSError as exc:
        raise RuntimeError(f"HttpPlugin: could not save {path}: {exc}") from None


def _replace_exclusively(temp: Path, path: Path) -> None:
    """Move *temp* to *path* only if nothing is there: an empty placeholder
    created with ``O_EXCL`` (``FileExistsError`` otherwise), then replaced;
    the placeholder is removed again if the replace fails."""
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    os.close(os.open(path, flags, 0o666))
    try:
        os.replace(temp, path)
    except OSError:
        with contextlib.suppress(OSError):
            os.unlink(path)
        raise


@dataclass(frozen=True)
class HttpPlugin:
    name: str = "http"

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
        secret_headers: frozenset[str] = frozenset(),
        scrub_secrets: Callable[[str], str] | None = None,
    ) -> ToolResult:
        """*secret_headers*: the (lower-cased) names of the headers whose
        value came from a secret (``core.tool`` resolves them, #598).
        *scrub_secrets* replaces their values in an error body before the
        excerpt cuts it, so no part of a value survives the cut."""
        url = params.get("url")
        if not isinstance(url, str) or not url.strip():
            raise ValueError("HttpPlugin requires params['url'] as a non-empty string.")
        url = url.strip()

        method = str(params.get("method", "GET")).upper()
        headers = dict(params.get("headers") or {})
        query_params = params.get("params")
        json_body = params.get("json")
        raw_body = params.get("body")
        parse = str(params.get("parse", "auto")).lower()
        fail_on_error = _as_bool(params.get("fail_on_error"), default=True)
        redirects = params.get("redirects")
        if redirects is None:
            redirects = "strip_auth" if secret_headers else "follow"
        if redirects not in REDIRECT_MODES:
            raise ValueError(
                "HttpPlugin: params['redirects'] must be one of 'follow', 'never', "
                f"'strip_auth', got {redirects!r}."
            )

        if json_body is not None and raw_body is not None:
            raise ValueError(
                "HttpPlugin: pass either params['json'] or params['body'], not both."
            )
        save = _save_target(params)
        if save is not None and parse != "auto":
            raise ValueError(
                "HttpPlugin: params['parse'] must be 'auto' (or unset) with params['save_to']: "
                "the body is saved as bytes, never parsed."
            )

        if isinstance(query_params, dict) and query_params:
            sep = "&" if "?" in url else "?"
            url = url + sep + urllib.parse.urlencode(query_params, doseq=True)

        body_bytes: bytes | None = None
        if json_body is not None:
            body_bytes = _json.dumps(json_body).encode("utf-8")
            headers.setdefault("Content-Type", "application/json")
        elif raw_body is not None:
            body_bytes = (
                raw_body.encode("utf-8") if isinstance(raw_body, str) else bytes(raw_body)
            )

        req = urllib.request.Request(url, data=body_bytes, method=method)
        for k, v in headers.items():
            req.add_header(str(k), str(v))

        status: int = 0
        resp_headers: dict[str, str] = {}
        text: str = ""
        stderr: str | None = None
        saved: tuple[int, str] | None = None

        policy: _RedirectPolicy | None = None
        open_request: Any = urllib.request.urlopen
        if redirects != "follow":
            policy = _RedirectPolicy(redirects, frozenset({"authorization", *secret_headers}))
            open_request = urllib.request.build_opener(policy).open

        try:
            with open_request(req, timeout=int(timeout_seconds)) as resp:
                status = int(resp.status)
                resp_headers = {k.lower(): v for k, v in resp.headers.items()}
                if save is None:
                    text = resp.read().decode("utf-8", errors="replace")
                elif 200 <= status < 300:
                    saved = _save_body(resp, save, url)
        except urllib.error.HTTPError as exc:
            # 4xx/5xx still produce a body — surface to caller, don't raise.
            status = int(exc.code)
            resp_headers = {k.lower(): v for k, v in (exc.headers or {}).items()}
            try:
                text = exc.read().decode("utf-8", errors="replace") if exc.fp else ""
            except Exception:
                text = ""
            stderr = f"HTTP {status}: {exc.reason}"
            if policy is not None and policy.refused:
                stderr = f"{stderr}: {_REFUSED_REDIRECT_NOTES[redirects]}"
            excerpt = http_error_excerpt(scrub_secrets(text) if scrub_secrets else text)
            if excerpt:
                stderr = f"{stderr}: {excerpt}"
        except urllib.error.URLError as exc:
            # DNS / connection failure — true execution error.
            raise RuntimeError(f"HTTP request to {url} failed: {exc.reason}") from exc

        ok = not (
            fail_on_error
            and (
                status >= 400
                or (policy is not None and policy.refused)
                or (redirects == "never" and 300 <= status < 400)
            )
        )
        if save is not None:
            # The body is in the file, never in the result (#602).
            return ToolResult(
                value={
                    "path": str(save.path) if saved is not None else None,
                    "bytes": saved[0] if saved is not None else 0,
                    "sha256": saved[1] if saved is not None else None,
                    "content_type": resp_headers.get("content-type"),
                    "status": status,
                },
                raw={"status": status, "headers": resp_headers, "url": url},
                stdout=None,
                stderr=stderr,
                exit_code=None,
                ok=ok,
            )

        # Parse body per `parse`.
        content_type = (resp_headers.get("content-type") or "").lower()
        looks_like_json = "json" in content_type
        value: Any
        if parse == "json" or (parse == "auto" and looks_like_json):
            try:
                value = _json.loads(text) if text else None
            except _json.JSONDecodeError as exc:
                if parse == "json":
                    raise RuntimeError(
                        f"HttpPlugin: parse='json' but response is not JSON: {exc}"
                    ) from exc
                value = text
        else:
            value = text

        return ToolResult(
            value=value,
            raw={
                "status": status,
                "headers": resp_headers,
                "url": url,
                "body": text,
            },
            stdout=None,
            stderr=stderr,
            exit_code=None,
            ok=ok,
        )

    def check(self) -> CheckResult:
        # urllib is stdlib; nothing else required. Spec-format `library:`
        # marker is omitted because there's no install step possible.
        if importlib.util.find_spec("urllib") is None:
            return CheckResult(ok=False, missing=["library:urllib"])
        return CheckResult(ok=True, missing=[])
