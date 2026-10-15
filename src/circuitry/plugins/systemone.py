"""Typed-decision tool plugin: ``POST /v1/systemone``.

Asks typed questions (``noul``, ``choice``, ``score``) about a piece of state
and checks that the answers match the questions before they reach the
document. Speaks the wire format of TypeSafe's hosted API
(https://docs.typesafe.ai/api) and of the local servers that copy it. Uses
stdlib ``urllib`` like the ``http`` plugin, with the same error excerpts and
the same status classification (see ``core.tool``'s HTTP family).

Host settings (``runtime.plugins.systemone``, never an effect param):
  - ``base_url`` (required): the server, e.g. ``http://127.0.0.1:8080``.
  - ``api_key_env`` (optional): the NAME of an environment variable holding
    the key, sent only as ``Authorization: Bearer <key>``.
  - ``allow_remote`` (optional, default ``false``): unless true, only a
    loopback host is accepted (``localhost``, ``127.0.0.0/8``, ``[::1]``).

Params:
  - ``model`` (required).
  - ``questions`` (required): ``{id: {type, instructions, criteria}}``.
  - exactly one of ``state`` (any JSON value) or ``state_file`` (a file read
    at dispatch: ``.json`` is parsed, anything else is sent as a string).
  - ``within`` (required with ``state_file``): an absolute path the file must
    be inside once symlinks are resolved.

ToolResult shape:
  - ``value``: the response's ``answers`` map, as returned.
  - ``raw``: ``{model, usage, status, state_sha256, state_bytes,
    question_ids}``, plus ``error_kind`` on a failure. Never the state text.
  - ``tokens_sent`` / ``tokens_received``: from ``usage``.
  - ``ok``: ``False`` for any HTTP error, a redirect (never followed), a body
    over the size limit, and an answer that does not match its question.
"""

from __future__ import annotations

import hashlib
import http.client
import ipaddress
import json
import math
import os
import re
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

ENDPOINT_PATH = "/v1/systemone"

#: Host settings live under ``runtime.plugins.systemone`` only; an effect
#: param with one of these names is an error.
HOST_SETTING_NAMES = ("base_url", "api_key_env", "allow_remote")

QUESTION_TYPES = ("noul", "choice", "score")

#: The API's request limits.
MAX_BODY_BYTES = 32 * 1024 * 1024
MAX_NESTING_LEVELS = 64
MAX_CHOICE_OPTIONS = 255
MIN_SCORE_LEVELS = 2
MAX_SCORE_LEVELS = 10

#: A response bigger than the request limit is not a response worth reading.
_MAX_RESPONSE_BYTES = MAX_BODY_BYTES

#: Probabilities must sum to 1 within this.
_PROBABILITY_SUM_TOLERANCE = 0.01

_ENV_NAME = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")


class _TooLarge(Exception):
    """The request body would be over :data:`MAX_BODY_BYTES`; nothing is sent."""


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    """Never follow a redirect: the key would travel with it."""

    def redirect_request(self, *args: Any, **kwargs: Any) -> None:
        return None


def is_loopback_host(host: str) -> bool:
    """True for the literal hosts ``localhost``, ``127.0.0.0/8`` and ``::1``.

    Any other name is remote; no DNS lookup decides it.
    """
    if host.lower() == "localhost":
        return True
    try:
        address = ipaddress.ip_address(host)
    except ValueError:
        return False
    if isinstance(address, ipaddress.IPv4Address):
        return address in ipaddress.ip_network("127.0.0.0/8")
    return address == ipaddress.IPv6Address("::1")


def param_reference(value: Any) -> Any:
    """``core.tool.param_reference``, imported late: ``core.tool`` imports this package."""
    from ..core.tool import param_reference as _param_reference

    return _param_reference(value)


def _number(value: Any) -> float | None:
    """*value* as a finite float, or ``None`` when it is not a finite number."""
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    try:
        number = float(value)
    except OverflowError:
        return None
    return number if math.isfinite(number) else None


def json_key(key: Any) -> Any:
    """*key* as ``json.dumps`` writes an object key, when that is a string.

    YAML reads an unquoted ``true``, ``no`` or ``on`` key as a boolean and
    ``1:`` as an integer; the request carries ``"true"``, ``"false"`` and
    ``"1"``. A key this does not know how to write is returned unchanged.
    """
    if isinstance(key, str):
        return key
    if key is True:
        return "true"
    if key is False:
        return "false"
    if key is None:
        return "null"
    if isinstance(key, int):
        return str(key)
    if isinstance(key, float) and math.isfinite(key):
        return float.__repr__(key)
    return key


class _RepeatedKey:
    """A key that, written as JSON, repeats an earlier one (``1`` after ``"1"``).

    Not a string, so validation reports it instead of the request silently
    carrying two entries under one name.
    """

    def __init__(self, key: Any) -> None:
        self.key = key

    def __repr__(self) -> str:
        return repr(self.key)


def _json_keys(mapping: dict[Any, Any]) -> dict[Any, Any]:
    """*mapping* with every key written as ``json_key`` does."""
    out: dict[Any, Any] = {}
    for key, value in mapping.items():
        written = json_key(key)
        out[_RepeatedKey(key) if written in out else written] = value
    return out


def normalize_questions(questions: Any) -> Any:
    """*questions* with every question id and criteria key as the request writes it.

    The one place a document's non-string keys become strings, called before
    both the static check and dispatch, so the answers are validated against
    the very keys the server receives. Anything that is not a map is returned
    as it is for validation to report.
    """
    if not isinstance(questions, dict):
        return questions
    out: dict[Any, Any] = {}
    for qid, question in _json_keys(questions).items():
        if isinstance(question, dict) and isinstance(question.get("criteria"), dict):
            out[qid] = {**question, "criteria": _json_keys(question["criteria"])}
        else:
            out[qid] = question
    return out


def _is_json_text(value: Any) -> bool:
    """A string, object or array: what the API takes as free-form text."""
    return isinstance(value, (str, dict, list))


# ---------------------------------------------------------------------------
# Question validation (cof check and dispatch share it)
# ---------------------------------------------------------------------------


def question_errors(questions: Any, *, static: bool) -> list[str]:
    """Why *questions* is not a valid ``{id: {type, instructions, criteria}}`` map.

    ``static`` is ``cof check`` reading the document before anything renders:
    a ``{from: ...}`` reference stands for a value that is not known yet and
    is accepted wherever a value is expected.
    """
    if static and param_reference(questions) is not None:
        return []
    if not isinstance(questions, dict):
        return ["must be a map of question id to {type, instructions, criteria}"]
    errors: list[str] = []
    for qid, question in questions.items():
        errors.extend(_question_errors(str(qid), question, static=static))
    return errors


def _question_errors(qid: str, question: Any, *, static: bool) -> list[str]:
    where = f"question {qid!r}"
    if static and param_reference(question) is not None:
        return []
    if not isinstance(question, dict):
        return [f"{where}: must be a map with type, instructions and criteria"]

    errors: list[str] = []
    qtype = question.get("type")
    typed: str | None = None
    if static and (param_reference(qtype) is not None or (isinstance(qtype, str) and "{{" in qtype)):
        pass
    elif qtype in QUESTION_TYPES:
        typed = qtype
    else:
        errors.append(f"{where}: type must be one of {', '.join(QUESTION_TYPES)}, got {qtype!r}")

    instructions = question.get("instructions")
    if not (static and param_reference(instructions) is not None):
        if instructions is None:
            errors.append(f"{where}: instructions are required")
        elif not _is_json_text(instructions):
            errors.append(f"{where}: instructions must be a string, object or array")

    criteria = question.get("criteria")
    if typed is not None and not (static and param_reference(criteria) is not None):
        errors.extend(_criteria_errors(where, typed, criteria))
    return errors


def _criteria_errors(where: str, qtype: str, criteria: Any) -> list[str]:
    if qtype == "noul":
        if criteria is None:
            return []
        if not isinstance(criteria, dict) or not set(criteria) <= {"true", "false"}:
            return [f"{where}: criteria for a noul question may only hold 'true' and 'false'"]
        if not all(_is_json_text(v) for v in criteria.values()):
            return [f"{where}: noul criteria 'true' and 'false' must be a string, object or array"]
        return []
    if qtype == "choice":
        if not isinstance(criteria, dict):
            return [f"{where}: criteria for a choice question must be a map of option to description"]
        unwritable = [key for key in criteria if not isinstance(key, str)]
        if unwritable:
            return [
                (
                    f"{where}: option {unwritable[0]!r} is not text, or repeats another "
                    "option once written as JSON"
                )
            ]
        if len(criteria) > MAX_CHOICE_OPTIONS:
            return [
                (
                    f"{where}: {len(criteria)} options; a choice question takes at most "
                    f"{MAX_CHOICE_OPTIONS}"
                )
            ]
        if not all(v is None or _is_json_text(v) for v in criteria.values()):
            return [f"{where}: each option's description must be a string, object, array or null"]
        return []
    if (
        not isinstance(criteria, list)
        or not MIN_SCORE_LEVELS <= len(criteria) <= MAX_SCORE_LEVELS
    ):
        return [
            (
                f"{where}: criteria for a score question must be an array of "
                f"{MIN_SCORE_LEVELS} to {MAX_SCORE_LEVELS} level descriptions"
            )
        ]
    if not all(_is_json_text(v) for v in criteria):
        return [f"{where}: each score level description must be a string, object or array"]
    return []


# ---------------------------------------------------------------------------
# Static param validation (cof check)
# ---------------------------------------------------------------------------


def static_param_errors(
    params: Mapping[str, Any],
    *,
    has_params_json: bool = False,
    has_document_file: bool = True,
    confine_to: str | None = None,
) -> list[str]:
    """Problems ``cof check`` can see in a systemone effect's literal params.

    *params* is the effect's ``params`` block with the effect-level ``model``
    folded in, before anything is rendered. A ``{from: ...}`` reference is
    accepted where a value is expected. With *has_params_json* a missing
    ``model``, ``questions`` or state may arrive in the rendered overlay, so
    only what the literal params do say is checked.

    *has_document_file* is ``False`` for a document with no file of its own
    (generated at run time, stdin, an SDK string, a ``use: inline`` child),
    which cannot use ``state_file``. *confine_to* is the project of a limited
    document (``ToolDefinition.confine_to``); a literal ``within`` is checked
    against it. ``None`` skips that check: a trusted document may name any
    ``within``.
    """
    errors = [
        f"params.{key} is a host setting; set it under runtime.plugins.systemone in config.json"
        for key in HOST_SETTING_NAMES
        if key in params
    ]

    model = params.get("model")
    if ("model" in params or not has_params_json) and (
        param_reference(model) is None and not (isinstance(model, str) and model.strip())
    ):
        errors.append("params.model is required: the model name as a non-empty string")

    if "questions" not in params:
        if not has_params_json:
            errors.append("params.questions is required")
    else:
        errors.extend(
            f"params.questions: {e}"
            for e in question_errors(normalize_questions(params["questions"]), static=True)
        )

    has_state = "state" in params
    has_file = "state_file" in params
    if has_state and has_file:
        errors.append("give exactly one of params.state and params.state_file, not both")
    elif not has_state and not has_file and not has_params_json:
        errors.append("give exactly one of params.state and params.state_file")

    if has_file:
        if not has_document_file:
            errors.append(
                "params.state_file: 'file:' cannot be used here — this document has no "
                "file of its own (it was generated at run time, or has no path: stdin, "
                "an SDK string, a 'use: inline' child); give the text in params.state."
            )
        state_file = params["state_file"]
        if param_reference(state_file) is None and not (
            isinstance(state_file, str) and state_file.strip()
        ):
            errors.append("params.state_file must be a non-empty path string")
        if "within" not in params:
            errors.append("params.state_file requires params.within")
        else:
            within_errors = _within_errors(params["within"])
            errors.extend(within_errors)
            if (
                not within_errors
                and confine_to is not None
                and param_reference(params["within"]) is None
                and not _is_inside(Path(params["within"]), Path(confine_to))
            ):
                errors.append(f"params.within: {_OUTSIDE_PROJECT}")
    elif "within" in params and not has_params_json:
        errors.append("params.within only applies to params.state_file")
    return errors


#: Why a limited document may not name this ``within``.
_OUTSIDE_PROJECT = (
    "must lie inside the document's own project (a document the caller did not "
    "name by path is confined to it)"
)


def _is_inside(path: Path, root: Path) -> bool:
    """Whether *path* is *root* or under it once symlinks are resolved."""
    return path_is_inside(path.resolve(strict=False), root.resolve(strict=False))


def _within_errors(within: Any) -> list[str]:
    reference = param_reference(within)
    if reference is not None:
        root, _, rest = reference.path.partition(".")
        if root != "input" or not rest:
            return ["params.within must be a literal absolute path or {from: input.<name>}"]
        return []
    if not isinstance(within, str) or not within.strip() or "{{" in within or "}}" in within:
        return ["params.within must be a literal absolute path or {from: input.<name>}"]
    if not os.path.isabs(within):
        return [f"params.within must be an absolute path, got {within!r}"]
    return []


def reject_overlay_within(overlay: Mapping[str, Any]) -> None:
    """``params_json`` is built at run time and can carry generated content,
    so it never sets ``within``: only the document's own params do.
    """
    if "within" in overlay:
        raise ValueError(
            "params_json must not set 'within': it is only honoured from a "
            "document's literal params block."
        )


# ---------------------------------------------------------------------------
# Answer validation
# ---------------------------------------------------------------------------


def answer_errors(questions: Mapping[str, Any], answers: Any) -> list[str]:
    """Why *answers* does not answer *questions*; each message names the id."""
    if not isinstance(answers, dict):
        return ["response 'answers' is not a map"]
    asked = {str(qid): question for qid, question in questions.items()}
    errors: list[str] = []
    for qid, question in asked.items():
        if qid not in answers:
            errors.append(f"question {qid!r}: no answer")
            continue
        errors.extend(_answer_errors(qid, question, answers[qid]))
    errors.extend(
        f"answer {answered!r}: no such question was asked"
        for answered in answers
        if answered not in asked
    )
    return errors


def _answer_errors(qid: str, question: Mapping[str, Any], answer: Any) -> list[str]:
    where = f"question {qid!r}"
    if not isinstance(answer, dict):
        return [f"{where}: answer is not an object"]
    qtype = question["type"]
    if answer.get("type") != qtype:
        return [f"{where}: answer type {answer.get('type')!r} differs from the question's {qtype!r}"]

    errors: list[str] = []
    if qtype == "noul":
        value = _number(answer.get("noul"))
        if value is None or not 0 <= value <= 1:
            errors.append(f"{where}: noul {answer.get('noul')!r} is not a number in [0, 1]")
    elif qtype == "choice":
        options = question["criteria"]
        chosen = answer.get("choice")
        if not isinstance(chosen, str) or chosen not in options:
            errors.append(f"{where}: choice {chosen!r} is not one of the question's options")
        errors.extend(_probability_errors(where, answer.get("probabilities"), set(options), "option"))
    else:
        levels = len(question["criteria"])
        score = _number(answer.get("score"))
        if score is None or not 0 <= score <= levels - 1:
            errors.append(
                f"{where}: score {answer.get('score')!r} is not a number in [0, {levels - 1}]"
            )
        valid_levels = {str(level) for level in range(levels)}
        errors.extend(_probability_errors(where, answer.get("probabilities"), valid_levels, "level"))

    if "confidence" in answer:
        confidence = _number(answer["confidence"])
        if confidence is None or not 0 <= confidence <= 1:
            errors.append(
                f"{where}: confidence {answer['confidence']!r} is not a number in [0, 1]"
            )
    return errors


def _probability_errors(where: str, probabilities: Any, known: set[str], noun: str) -> list[str]:
    if not isinstance(probabilities, dict):
        return [f"{where}: probabilities are missing or not a map"]
    errors: list[str] = []
    total = 0.0
    for key, raw in probabilities.items():
        if key not in known:
            errors.append(f"{where}: probabilities name {key!r}, which is not an {noun} of the question")
        value = _number(raw)
        if value is None or not 0 <= value <= 1:
            errors.append(f"{where}: probability for {key!r} is {raw!r}, not a number in [0, 1]")
        else:
            total += value
    if not errors and abs(total - 1) > _PROBABILITY_SUM_TOLERANCE:
        errors.append(f"{where}: probabilities sum to {total:g}, not 1 (±{_PROBABILITY_SUM_TOLERANCE:g})")
    return errors


# ---------------------------------------------------------------------------
# Request building
# ---------------------------------------------------------------------------


def _nesting_levels(value: Any) -> int:
    """How many containers deep *value* goes; stops counting past the limit."""
    deepest = 0
    stack: list[tuple[Any, int]] = [(value, 1)]
    while stack:
        item, level = stack.pop()
        if isinstance(item, dict):
            children: Any = item.values()
        elif isinstance(item, (list, tuple)):
            children = item
        else:
            continue
        deepest = max(deepest, level)
        if deepest > MAX_NESTING_LEVELS:
            break
        stack.extend((child, level + 1) for child in children)
    return deepest


def _json_bytes(value: Any, what: str) -> bytes:
    try:
        text = json.dumps(value, ensure_ascii=False, allow_nan=False, separators=(",", ":"))
        return text.encode("utf-8")
    except (TypeError, ValueError) as exc:
        raise ValueError(f"systemone: {what} cannot be sent as JSON: {exc}") from exc


def _reject_constant(name: str) -> Any:
    raise ValueError(f"{name} is not a finite number")


def _read_state_file(path_text: str, within_text: Any, confine_to: str | None) -> Any:
    """The state a ``state_file`` holds, once it is proven to lie inside *within*."""
    if not isinstance(within_text, str) or not within_text.strip():
        raise ValueError("systemone: params.within is required with params.state_file")
    if not os.path.isabs(within_text) or "\x00" in within_text:
        raise ValueError("systemone: params.within must be an absolute path")
    if "\x00" in path_text:
        raise ValueError("systemone: params.state_file must not contain a null byte")

    within = Path(within_text).resolve(strict=False)
    if confine_to is not None and not _is_inside(within, Path(confine_to)):
        raise ValueError(f"systemone: params.within {_OUTSIDE_PROJECT}")

    try:
        resolved = Path(path_text).resolve(strict=True)
    except OSError as exc:
        raise ValueError(f"systemone: params.state_file {path_text!r} cannot be read: {exc.strerror}") from exc
    if resolved != within and not resolved.is_relative_to(within):
        raise ValueError(
            f"systemone: params.state_file {path_text!r} resolves outside params.within"
        )
    if not resolved.is_file():
        raise ValueError(f"systemone: params.state_file {path_text!r} is not a regular file")
    if resolved.stat().st_size > MAX_BODY_BYTES:
        raise _TooLarge(
            f"systemone: params.state_file {path_text!r} is over the {MAX_BODY_BYTES}-byte request limit"
        )
    try:
        text = resolved.read_text(encoding="utf-8-sig")
    except UnicodeDecodeError as exc:
        raise ValueError(f"systemone: params.state_file {path_text!r} is not valid UTF-8") from exc
    except OSError as exc:
        raise ValueError(f"systemone: params.state_file {path_text!r} cannot be read: {exc.strerror}") from exc

    if resolved.suffix.lower() != ".json":
        return text
    try:
        return json.loads(text, parse_constant=_reject_constant)
    except (ValueError, RecursionError) as exc:
        raise ValueError(f"systemone: params.state_file {path_text!r} is not valid JSON: {exc}") from exc


def _error_kind(status: int, body_text: str) -> str:
    if 300 <= status < 400:
        return "redirect"
    if status == 400:
        return "context_exceeded" if "max_tokens_exceeded" in body_text else "invalid"
    named = {
        401: "unauthorized",
        403: "forbidden",
        404: "not_found",
        408: "timeout",
        413: "too_large",
        415: "unsupported_media_type",
        422: "invalid",
        429: "rate_limited",
        529: "overloaded",
    }
    if status in named:
        return named[status]
    return "server_error" if 500 <= status < 600 else "http_error"


@dataclass(frozen=True)
class SystemOnePlugin:
    name: str = "systemone"
    base_url: str | None = None
    api_key_env: str | None = None
    allow_remote: bool = False
    #: The project a limited document's ``within`` must stay inside
    #: (``ToolDefinition.confine_to``); ``None`` for a trusted document or a
    #: plugin called outside a run.
    confine_to: str | None = None

    # -- host settings ------------------------------------------------------

    def _server(self) -> str:
        """The validated server URL, without a trailing slash."""
        base_url = self.base_url
        if not isinstance(base_url, str) or not base_url.strip():
            raise ValueError(
                "systemone: runtime.plugins.systemone.base_url is required "
                "(set it in config.json, e.g. http://127.0.0.1:8080)"
            )
        base_url = base_url.strip().rstrip("/")
        try:
            parts = urllib.parse.urlsplit(base_url)
            hostname = parts.hostname
            _ = parts.port  # raises ValueError on a bad port
        except ValueError:
            raise ValueError(
                "systemone: runtime.plugins.systemone.base_url is not a valid URL"
            ) from None
        if parts.scheme not in ("http", "https") or not hostname:
            raise ValueError(
                "systemone: runtime.plugins.systemone.base_url must be an http:// or https:// URL"
            )
        if parts.username is not None or parts.password is not None:
            raise ValueError(
                "systemone: runtime.plugins.systemone.base_url must not carry credentials; "
                "name the key's environment variable in api_key_env instead"
            )
        if parts.query or parts.fragment:
            raise ValueError(
                "systemone: runtime.plugins.systemone.base_url must not have a query or fragment"
            )
        if not self.allow_remote and not is_loopback_host(hostname):
            raise ValueError(
                f"systemone: base_url host {hostname!r} is not a loopback host; set "
                "runtime.plugins.systemone.allow_remote to true to use a remote server"
            )
        return base_url

    def _api_key(self) -> str | None:
        name = self.api_key_env
        if name is None or name == "":
            return None
        if not isinstance(name, str) or not _ENV_NAME.match(name):
            raise ValueError(
                "systemone: runtime.plugins.systemone.api_key_env must be the NAME of an "
                "environment variable, not the key itself"
            )
        key = (os.environ.get(name) or "").strip()
        if not key:
            return None
        if not (key.isascii() and key.isprintable()):
            raise ValueError(
                f"systemone: the key in ${name} has characters that cannot be sent in a header"
            )
        return key

    # -- request ------------------------------------------------------------

    def _state(self, params: Mapping[str, Any]) -> Any:
        has_state = "state" in params
        has_file = "state_file" in params
        if has_state == has_file:
            raise ValueError("systemone: give exactly one of params.state and params.state_file")
        if has_state:
            if "within" in params:
                raise ValueError("systemone: params.within only applies to params.state_file")
            return params["state"]
        path_text = params["state_file"]
        if not isinstance(path_text, str) or not path_text.strip():
            raise ValueError("systemone: params.state_file must be a non-empty path string")
        return _read_state_file(path_text, params.get("within"), self.confine_to)

    def execute(
        self,
        *,
        params: dict[str, Any],
        timeout_seconds: int = 300,
    ) -> ToolResult:
        host_params = [k for k in HOST_SETTING_NAMES if k in params]
        if host_params:
            raise ValueError(
                f"systemone: params.{host_params[0]} is a host setting; set it under "
                "runtime.plugins.systemone in config.json"
            )
        server = self._server()
        key = self._api_key()

        model = params.get("model")
        if not isinstance(model, str) or not model.strip():
            raise ValueError("systemone: params.model is required as a non-empty string")
        questions = normalize_questions(params.get("questions"))
        problems = question_errors(questions, static=False)
        if problems:
            raise ValueError("systemone: " + "; ".join(problems))
        assert isinstance(questions, dict)
        question_ids = [str(qid) for qid in questions]

        raw: dict[str, Any] = {"model": model, "question_ids": question_ids}
        try:
            state = self._state(params)
            state_json = _json_bytes(state, "the state")
            raw["state_sha256"] = hashlib.sha256(state_json).hexdigest()
            raw["state_bytes"] = len(state_json)
            body = {"model": model, "state": state, "questions": questions}
            levels = _nesting_levels(body)
            if levels > MAX_NESTING_LEVELS:
                raise ValueError(
                    f"systemone: the request nests more than {MAX_NESTING_LEVELS} levels deep"
                )
            payload = _json_bytes(body, "the request")
            if len(payload) > MAX_BODY_BYTES:
                raise _TooLarge(
                    f"systemone: the request is {len(payload)} bytes, over the "
                    f"{MAX_BODY_BYTES}-byte limit"
                )
        except _TooLarge as exc:
            raw["error_kind"] = "too_large"
            return ToolResult(value=None, raw=raw, stderr=str(exc), ok=False)

        return self._post(
            server=server,
            key=key,
            payload=payload,
            raw=raw,
            questions=questions,
            timeout_seconds=timeout_seconds,
        )

    def _post(
        self,
        *,
        server: str,
        key: str | None,
        payload: bytes,
        raw: dict[str, Any],
        questions: dict[str, Any],
        timeout_seconds: int,
    ) -> ToolResult:
        request = urllib.request.Request(server + ENDPOINT_PATH, data=payload, method="POST")
        request.add_header("Content-Type", "application/json")
        request.add_header("Accept", "application/json")
        if key is not None:
            request.add_header("Authorization", f"Bearer {key}")

        handlers: list[Any] = [_NoRedirect]
        hostname = urllib.parse.urlsplit(server).hostname or ""
        if is_loopback_host(hostname):
            handlers.append(urllib.request.ProxyHandler({}))
        opener = urllib.request.build_opener(*handlers)

        def scrub(text: str) -> str:
            return text.replace(key, "[redacted]") if key else text

        def scrubbed(value: Any) -> Any:
            """*value* with the key removed from every string in it, keys included."""
            if isinstance(value, str):
                return scrub(value)
            if isinstance(value, dict):
                return {scrubbed(k): scrubbed(v) for k, v in value.items()}
            if isinstance(value, list):
                return [scrubbed(item) for item in value]
            return value

        status = 0
        headers: dict[str, str] = {}
        body = b""
        try:
            with opener.open(request, timeout=int(timeout_seconds)) as response:
                status = int(response.status)
                headers = {k.lower(): v for k, v in response.headers.items()}
                body = response.read(_MAX_RESPONSE_BYTES + 1)
        except urllib.error.HTTPError as exc:
            status = int(exc.code)
            headers = {k.lower(): v for k, v in (exc.headers or {}).items()}
            try:
                body = exc.read(_MAX_RESPONSE_BYTES + 1) if exc.fp else b""
            except (OSError, http.client.HTTPException):
                body = b""
            finally:
                exc.close()
        except urllib.error.URLError as exc:
            if isinstance(exc.reason, TimeoutError):
                raise RuntimeError(
                    f"systemone request to {server} timed out after {int(timeout_seconds)}s"
                ) from exc
            raise RuntimeError(f"systemone server not reachable at {server}") from exc
        except TimeoutError as exc:
            raise RuntimeError(
                f"systemone request to {server} timed out after {int(timeout_seconds)}s"
            ) from exc
        except (ConnectionError, http.client.HTTPException) as exc:
            raise RuntimeError(f"systemone server at {server} dropped the connection") from exc

        raw["status"] = status
        retry_after = headers.get("retry-after")
        if retry_after:
            raw["headers"] = {"retry-after": scrub(retry_after[:100])}
        text = body.decode("utf-8", errors="replace")

        if not 200 <= status < 300:
            raw["error_kind"] = _error_kind(status, text)
            if 300 <= status < 400:
                message = (
                    f"systemone server at {server} answered HTTP {status}; "
                    "a redirect is never followed"
                )
            else:
                message = f"systemone HTTP {status} ({raw['error_kind']})"
                excerpt = http_error_excerpt(text)
                if excerpt:
                    message = f"{message}: {excerpt}"
            return ToolResult(value=None, raw=raw, stderr=scrub(message), ok=False)

        return self._answers(raw, questions, text, scrubbed)

    def _answers(
        self,
        raw: dict[str, Any],
        questions: dict[str, Any],
        text: str,
        scrubbed: Callable[[Any], Any],
    ) -> ToolResult:
        def invalid(kind: str, message: str) -> ToolResult:
            raw["error_kind"] = kind
            return ToolResult(value=None, raw=raw, stderr=scrubbed(message), ok=False)

        try:
            reply = json.loads(text)
        except (ValueError, RecursionError):
            return invalid("invalid_response", "systemone response is not valid JSON")
        if not isinstance(reply, dict) or "answers" not in reply:
            return invalid("invalid_response", "systemone response has no 'answers'")

        if isinstance(reply.get("model"), str):
            raw["model"] = scrubbed(reply["model"])
        usage = reply.get("usage")
        raw["usage"] = scrubbed(usage) if isinstance(usage, dict) else None

        problems = answer_errors(questions, reply["answers"])
        if problems:
            return invalid("invalid_answer", "systemone answers do not match the questions: " + "; ".join(problems))

        def tokens(field: str) -> int | None:
            value = usage.get(field) if isinstance(usage, dict) else None
            return value if isinstance(value, int) and not isinstance(value, bool) and value >= 0 else None

        return ToolResult(
            value=scrubbed(reply["answers"]),
            raw=raw,
            stdout=None,
            stderr=None,
            exit_code=None,
            ok=True,
            tokens_sent=tokens("input_tokens"),
            tokens_received=tokens("output_tokens"),
        )

    def check(self) -> CheckResult:
        # Offline: an unconfigured plugin is fine (every enabled tool is
        # checked, configured or not); a configured one must be usable.
        if self.base_url is None or (isinstance(self.base_url, str) and not self.base_url.strip()):
            return CheckResult(
                ok=True,
                missing=[],
                message="not configured (runtime.plugins.systemone.base_url)",
            )
        try:
            self._server()
            self._api_key()
        except ValueError as exc:
            return CheckResult(ok=False, missing=[], message=str(exc))
        return CheckResult(ok=True, missing=[])


def build(cfg: Mapping[str, Any], confine_to: str | None = None) -> SystemOnePlugin:
    """The plugin for ``runtime.plugins.systemone`` (*cfg*), confining ``within`` to *confine_to*."""
    return SystemOnePlugin(
        base_url=cfg.get("base_url"),
        api_key_env=cfg.get("api_key_env"),
        allow_remote=_as_bool(cfg.get("allow_remote"), default=False),
        confine_to=confine_to,
    )
