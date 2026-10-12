#!/usr/bin/env python3
"""A differential corpus for lane F2's real LLM provider adapters
(issue #503), covering the pure functions and transport-outcome
decision tables electricity's port has to match exactly:

- the request-building halves (`adapters/ollama.py::_request`,
  `adapters/_openai_compat.py::chat_messages`/`sampling_fields`,
  `adapters/anthropic.py::_request_body`), driven with `run_curl`
  monkeypatched to record `(url, headers, data, timeout_seconds)` and
  return a scripted `CompletedProcess`;
- the outcome-interpretation halves (each adapter's own handling of a
  curl `(returncode, stdout, stderr)` triple -- JSON error-body
  shapes, the exit-code-to-hint table, token coercion, secret
  masking), driven by scripted triples through the same monkeypatch;
- the pure helpers in `curl_support.py` (`parse_error_body`,
  `_mask_credential_query_params`, `_strip_url_userinfo`,
  `curl_failure_message`, and `run_curl`'s own control-character and
  `--config`-size checks).

Must be run with Python 3.11 (the lane venv locally; `actions/setup-python`
3.11 in CI, per every other `generate_*.py`). Usage:

    python3 generate_adapter_corpus.py [--check]
"""

from __future__ import annotations

import json
import sys
from pathlib import Path
from subprocess import CompletedProcess
from typing import Any
from unittest import mock

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
sys.path.insert(0, str(REPO_ROOT / "src"))

from circuitry.adapters._retry import AdapterCallError, classify_curl_exit  # noqa: E402
from circuitry.adapters.anthropic import AnthropicAdapter  # noqa: E402
from circuitry.adapters.base import (  # noqa: E402
    ChatMessage,
    GenerateOptions,
    ImageInput,
)
from circuitry.adapters.lmstudio import LMStudioAdapter  # noqa: E402
from circuitry.adapters.ollama import OllamaAdapter  # noqa: E402
from circuitry.adapters.openai import OpenAIAdapter  # noqa: E402
from circuitry.adapters.replicate import ReplicateAdapter  # noqa: E402
from circuitry.curl_support import (  # noqa: E402
    _MAX_CONFIG_BYTES,
    _curl_config_file,
    _mask_credential_query_params,
    _strip_url_userinfo,
    _validate_no_control_chars,
    curl_failure_message,
    parse_error_body,
)

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-adapters"
    / "tests"
    / "golden"
    / "adapter_corpus.json"
)


# ---------------------------------------------------------------------
# Options encoding: a plain JSON shape both this generator and the
# Rust replay build a real `GenerateOptions`/`GenerateOptions` from.
# `temperature` may be the literal string `"NaN"` -- the one non-JSON-
# standard float this corpus needs to round-trip; every other field
# stays within strict JSON (int/float/str/bool/null/list/dict).
# ---------------------------------------------------------------------


def _options_from_json(spec: dict[str, Any]) -> GenerateOptions:
    temperature = spec.get("temperature")
    if temperature == "NaN":
        temperature = float("nan")
    messages = tuple(
        ChatMessage(role=m["role"], content=m["content"]) for m in spec.get("messages", [])
    )
    images = tuple(
        ImageInput(
            data=bytes.fromhex(img["data_hex"]) if img.get("data_hex") is not None else None,
            url=img.get("url"),
            media_type=img.get("media_type") or "",
        )
        for img in spec.get("images", [])
    )
    return GenerateOptions(
        temperature=temperature,
        max_tokens=spec.get("max_tokens"),
        stop=tuple(spec.get("stop", [])),
        params=spec.get("params", {}),
        deterministic=spec.get("deterministic", False),
        messages=messages,
        images=images,
    )


def _retry_info_json(exc: BaseException) -> dict[str, Any]:
    if isinstance(exc, AdapterCallError):
        info = exc.retry_info
    else:
        info = classify_curl_exit(0, "")
    return {"retryable": info.retryable, "status": info.status, "retry_after": info.retry_after}


def _fake_completed(returncode: int, stdout: str, stderr: str) -> CompletedProcess[str]:
    return CompletedProcess(args=["curl"], returncode=returncode, stdout=stdout, stderr=stderr)


def _call_generate(
    module_path: str,
    build_call: Any,
    *,
    returncode: int = 0,
    stdout: str = "{}",
    stderr: str = "",
) -> tuple[dict[str, Any], dict[str, Any]]:
    """Patches `<module_path>.run_curl`, invokes *build_call* (a
    zero-arg callable that runs `adapter.generate(...)`), and returns
    `(captured_run_curl_kwargs, result_json)`.
    """
    captured: dict[str, Any] = {}

    def fake_run_curl(**kwargs: Any) -> CompletedProcess[str]:
        captured.update(kwargs)
        return _fake_completed(returncode, stdout, stderr)

    with mock.patch(f"{module_path}.run_curl", fake_run_curl):
        try:
            result = build_call()
            result_json: dict[str, Any] = {
                "ok": True,
                "text": result.text,
                "raw": result.raw,
                "tokens_sent": result.tokens_sent,
                "tokens_received": result.tokens_received,
                "finish_reason": result.finish_reason,
                "warnings": list(result.warnings),
            }
        except Exception as exc:
            result_json = {
                "ok": False,
                "class": type(exc).__name__,
                "message": str(exc),
                "retry": _retry_info_json(exc),
            }
    return captured, result_json


# ---------------------------------------------------------------------
# 1. Pure helpers (`curl_support.py`).
# ---------------------------------------------------------------------


def _parse_error_body_cases() -> list[dict[str, Any]]:
    bodies = [
        "",
        "   ",
        '{"error": {"message": "Invalid API Key"}}',
        '{"error": {"message": ""}}',
        '{"error": "model \\"m\\" not found, try pulling it first"}',
        '{"error": {}}',
        '{"unrelated": "field"}',
        "not json at all {",
        "x" * 250,
        "é" * 250,
        "[1, 2, 3]",
        "null",
    ]
    return [{"stdout": body, "expected": parse_error_body(body)} for body in bodies]


def _mask_credential_query_params_cases() -> list[dict[str, Any]]:
    urls = [
        "https://api.example.com/search",
        "https://api.example.com/search?key=secret&q=weather",
        "http://127.0.0.1:8080/v1/chat/completions?api_key=s3cr3t",
        "https://x.com/?Authorization=tok&other=1",
        "https://x.com/?a=1&a=2",
        "https://x.com/?weird%20key=value+with+space",
    ]
    return [{"url": url, "expected": _mask_credential_query_params(url)} for url in urls]


def _strip_url_userinfo_cases() -> list[dict[str, Any]]:
    texts = [
        "connect to http://user:pass@127.0.0.1:1 failed",
        "no credentials here at all",
        "two: http://a:b@x.com and https://c:d@y.com",
        "http://justuser@host/path",
    ]
    return [{"text": text, "expected": _strip_url_userinfo(text)} for text in texts]


def _curl_failure_message_cases() -> list[dict[str, Any]]:
    cases = [
        {
            "source": "ollama",
            "model": None,
            "url": "http://localhost:11434/api/generate",
            "returncode": 7,
            "stdout": "",
            "stderr": "",
            "hint": "Ollama at http://localhost:11434 is not reachable.",
            "secrets": (),
        },
        {
            "source": "OpenAI-compatible",
            "model": "m",
            "url": "http://127.0.0.1:1/v1/chat/completions?api_key=s3cr3t",
            "returncode": 22,
            "stdout": '{"error": {"message": "bad key s3cr3t"}}',
            "stderr": "",
            "hint": "",
            "secrets": ("s3cr3t",),
        },
        {
            "source": "openai",
            "model": "gpt",
            "url": "https://api.openai.com/v1/chat/completions",
            "returncode": 22,
            "stdout": "",
            "stderr": "curl: (22) The requested URL returned error: 401",
            "hint": "",
            "secrets": ("sk-live-abc",),
        },
        {
            "source": "watsonx-iam",
            "model": None,
            "url": "http://user:pass@127.0.0.1:1/identity/token",
            "returncode": 7,
            "stdout": "",
            "stderr": "",
            "hint": "",
            "secrets": ("apikey123",),
        },
    ]
    out = []
    for case in cases:
        expected = curl_failure_message(
            source=case["source"],
            model=case["model"],
            url=case["url"],
            returncode=case["returncode"],
            stdout=case["stdout"],
            stderr=case["stderr"],
            hint=case["hint"],
            secrets=case["secrets"],
        )
        out.append({**case, "secrets": list(case["secrets"]), "expected": expected})
    return out


def _control_char_cases() -> list[dict[str, Any]]:
    values = [
        ("url", "http://example.com/ok"),
        ("url", "http://example.com/\r\nbad"),
        ("header name", "X-Good"),
        ("header name", "X-Bad\n"),
        ("header value", "fine"),
        ("header value", "bad\0value"),
    ]
    cases = []
    for label, value in values:
        try:
            _validate_no_control_chars(value, label)
            error = None
        except ValueError as exc:
            error = str(exc)
        cases.append({"label": label, "value": value, "error": error})
    return cases


def _config_size_cases() -> list[dict[str, Any]]:
    small = [("Content-Type", "application/json")]
    huge = [("X-Big", "x" * 70_000)]
    cases = []
    for headers in (small, huge):
        config_bytes = _curl_config_file("http://example.com/x", headers).encode()
        over = len(config_bytes) > _MAX_CONFIG_BYTES
        cases.append(
            {
                "headers": [list(h) for h in headers],
                "config_bytes": len(config_bytes),
                "over_limit": over,
            }
        )
    return cases


# ---------------------------------------------------------------------
# 2. Native Ollama: request building and outcome interpretation.
# ---------------------------------------------------------------------


def _ollama_request_cases() -> list[dict[str, Any]]:
    specs: list[dict[str, Any]] = [
        {"model": "m", "prompt": "hello", "options": {}},
        {
            "model": "m",
            "prompt": "hello",
            "options": {
                "temperature": 0.2,
                "max_tokens": 50,
                "stop": ["END"],
                "deterministic": True,
                "params": {"num_ctx": 4096, "format": "json", "keep_alive": "5m", "think": False},
            },
        },
        {
            "model": "m",
            "prompt": "hello",
            "options": {
                "messages": [
                    {"role": "system", "content": "sys"},
                    {"role": "user", "content": "hi"},
                    {"role": "assistant", "content": "hi back"},
                    {"role": "tool", "content": "tool out"},
                ],
            },
        },
        {
            "model": "m",
            "prompt": "hello",
            "options": {
                "messages": [{"role": "user", "content": "hi"}],
                "images": [{"data_hex": "89504e47", "media_type": "image/png", "url": None}],
            },
        },
        {
            "model": "m",
            "prompt": "hello",
            "options": {
                "messages": [{"role": "system", "content": "sys"}],
                "images": [{"data_hex": "ff", "media_type": "image/png", "url": None}],
            },
        },
        {
            "model": "",
            "prompt": "héllo wörld",
            "options": {
                "params": {"seed": 7},
                "deterministic": True,
            },
        },
        {
            "model": "m",
            "prompt": "hi",
            "options": {"temperature": "NaN"},
        },
        {
            "model": "m",
            "prompt": "hi",
            "options": {
                "params": {"format": {"type": "object", "properties": {}}},
            },
        },
    ]
    cases = []
    for spec in specs:
        adapter = OllamaAdapter(base_url="http://localhost:11434")
        options = _options_from_json(spec["options"])
        captured, _result = _call_generate(
            "circuitry.adapters.ollama",
            lambda a=adapter, m=spec["model"], p=spec["prompt"], o=options: a.generate(
                model=m, prompt=p, options=o
            ),
        )
        cases.append(
            {
                "spec": spec,
                "url": captured.get("url"),
                "headers": captured.get("headers"),
                "data": captured.get("data"),
            }
        )
    return cases


def _ollama_outcome_cases() -> list[dict[str, Any]]:
    scripted = [
        (0, '{"response": " hi ", "prompt_eval_count": 3, "eval_count": 5, "done_reason": "stop"}', ""),
        (0, '{"message": {"content": "chat reply"}}', ""),
        (0, "<html>not json</html>", ""),
        (0, "[1,2,3]", ""),
        (
            22,
            '{"error": "model \\"m\\" not found, try pulling it first"}',
            "curl: (22) The requested URL returned error: 404",
        ),
        (22, '{"error": {"message": "bad request"}}', "curl: (22) The requested URL returned error: 400"),
        (7, "", ""),
        (28, "", ""),
        (52, "", ""),
        (6, "", ""),
        (35, "", ""),
        (60, "", ""),
    ]
    cases = []
    for returncode, stdout, stderr in scripted:
        adapter = OllamaAdapter(base_url="http://localhost:11434")
        _captured, result = _call_generate(
            "circuitry.adapters.ollama",
            lambda a=adapter: a.generate(model="m", prompt="hi", timeout_seconds=30),
            returncode=returncode,
            stdout=stdout,
            stderr=stderr,
        )
        cases.append(
            {
                "returncode": returncode,
                "stdout": stdout,
                "stderr": stderr,
                "model": "m",
                "timeout_seconds": 30,
                "base_url": "http://localhost:11434",
                "result": result,
            }
        )
    return cases


# ---------------------------------------------------------------------
# 3. The OpenAI-compatible family (via `lmstudio`, no API key, so the
#    request/response shapes are isolated from the key-env precheck).
# ---------------------------------------------------------------------


def _openai_compat_request_cases() -> list[dict[str, Any]]:
    specs: list[dict[str, Any]] = [
        {"model": "m", "prompt": "hello", "options": {}},
        {
            "model": "m",
            "prompt": "hello",
            "options": {
                "messages": [{"role": "tool", "content": "tool out"}],
                "temperature": 0.5,
                "max_tokens": 10,
                "stop": ["END"],
                "params": {"model": "override", "seed": 1},
            },
        },
        {
            "model": "m",
            "prompt": "hello",
            "options": {
                "messages": [{"role": "user", "content": "describe"}],
                "images": [{"data_hex": "89504e47", "media_type": "image/png", "url": None}],
            },
        },
        {
            "model": "m",
            "prompt": "hello",
            "options": {"deterministic": True},
        },
    ]
    cases = []
    for spec in specs:
        adapter = LMStudioAdapter(base_url="http://localhost:1234/v1")
        options = _options_from_json(spec["options"])
        captured, _result = _call_generate(
            "circuitry.adapters._openai_compat",
            lambda a=adapter, m=spec["model"], p=spec["prompt"], o=options: a.generate(
                model=m, prompt=p, options=o
            ),
        )
        cases.append(
            {
                "spec": spec,
                "url": captured.get("url"),
                "headers": captured.get("headers"),
                "data": captured.get("data"),
            }
        )
    return cases


def _openai_compat_outcome_cases() -> list[dict[str, Any]]:
    scripted = [
        (0, '{"choices": [{"message": {"content": " hi "}, "finish_reason": "stop"}], "usage": {"prompt_tokens": 3, "completion_tokens": 5}}', ""),
        (0, '{"choices": []}', ""),
        (0, "not json", ""),
        (
            22,
            '{"error": {"message": "Invalid API Key"}}',
            "curl: (22) The requested URL returned error: 401",
        ),
        (29, "", ""),
    ]
    cases = []
    for returncode, stdout, stderr in scripted:
        adapter = LMStudioAdapter(base_url="http://localhost:1234/v1")
        _captured, result = _call_generate(
            "circuitry.adapters._openai_compat",
            lambda a=adapter: a.generate(model="m", prompt="hi", timeout_seconds=30),
            returncode=returncode,
            stdout=stdout,
            stderr=stderr,
        )
        cases.append(
            {
                "returncode": returncode,
                "stdout": stdout,
                "stderr": stderr,
                "model": "m",
                "result": result,
            }
        )
    return cases


# ---------------------------------------------------------------------
# 4. `openai` -- the own-file response coercion (`max_completion_tokens`,
#    `int(x)`-coerced usage counts).
# ---------------------------------------------------------------------


def _openai_request_cases() -> list[dict[str, Any]]:
    """Issue #515 finding 5's first item: `openai` had an outcome-
    interpretation case but no request-building one -- its own
    `max_completion_tokens`/deterministic-`seed` placement, distinct
    from the rest of the OpenAI-compatible family, was untested here."""
    specs: list[dict[str, Any]] = [
        {"model": "m", "prompt": "hello", "options": {}},
        {
            "model": "m",
            "prompt": "hello",
            "options": {
                "messages": [{"role": "tool", "content": "tool out"}],
                "temperature": 0.5,
                "max_tokens": 10,
                "stop": ["END"],
            },
        },
        {
            "model": "m",
            "prompt": "hello",
            "options": {"deterministic": True},
        },
        {
            "model": "m",
            "prompt": "hello",
            "options": {"params": {"seed": 7}, "deterministic": True},
        },
    ]
    cases = []
    for spec in specs:
        adapter = OpenAIAdapter(base_url="https://api.openai.com/v1")
        options = _options_from_json(spec["options"])
        with mock.patch.dict("os.environ", {"OPENAI_API_KEY": "circuitry-test-fake-key"}):
            captured, _result = _call_generate(
                "circuitry.adapters.openai",
                lambda a=adapter, m=spec["model"], p=spec["prompt"], o=options: a.generate(
                    model=m, prompt=p, options=o
                ),
            )
        cases.append(
            {
                "spec": spec,
                "url": captured.get("url"),
                "headers": captured.get("headers"),
                "data": captured.get("data"),
            }
        )
    return cases


def _openai_outcome_cases() -> list[dict[str, Any]]:
    scripted = [
        (
            0,
            ('{"choices": [{"message": {"content": "hi"}, "finish_reason": "stop"}], '
            '"usage": {"prompt_tokens": "7", "completion_tokens": 7.9}}'),
            "",
        ),
        (
            0,
            '{"choices": [{"message": {"content": "hi"}}], "usage": {"prompt_tokens": true}}',
            "",
        ),
        (
            0,
            '{"choices": [{"message": {"content": "hi"}}], "usage": {"prompt_tokens": "x"}}',
            "",
        ),
        (22, '{"error": {"message": "bad key"}}', "curl: (22) The requested URL returned error: 401"),
    ]
    cases = []
    for returncode, stdout, stderr in scripted:
        adapter = OpenAIAdapter(base_url="https://api.openai.com/v1")
        with mock.patch.dict("os.environ", {"OPENAI_API_KEY": "circuitry-test-fake-key"}):
            _captured, result = _call_generate(
                "circuitry.adapters.openai",
                lambda a=adapter: a.generate(model="m", prompt="hi", timeout_seconds=30),
                returncode=returncode,
                stdout=stdout,
                stderr=stderr,
            )
        cases.append(
            {"returncode": returncode, "stdout": stdout, "stderr": stderr, "model": "m", "result": result}
        )
    return cases


# ---------------------------------------------------------------------
# 5. `anthropic` -- system hoisting, image-before-text, token coercion.
# ---------------------------------------------------------------------


def _anthropic_request_cases() -> list[dict[str, Any]]:
    specs: list[dict[str, Any]] = [
        {"options": {}},
        {
            "options": {
                "messages": [
                    {"role": "system", "content": "be nice"},
                    {"role": "user", "content": "hi"},
                    {"role": "tool", "content": "tool out"},
                ],
                "stop": ["END"],
                "temperature": 0.3,
            },
        },
        {"options": {"messages": [{"role": "system", "content": "only system"}]}},
        {
            "options": {
                "messages": [{"role": "user", "content": "describe"}],
                "images": [{"data_hex": "89504e47", "media_type": "image/png", "url": None}],
            },
        },
        {"options": {"max_tokens": 50}},
    ]
    cases = []
    for spec in specs:
        adapter = AnthropicAdapter(base_url="https://api.anthropic.com", max_tokens=4096)
        options = _options_from_json(spec["options"])
        with mock.patch.dict("os.environ", {"ANTHROPIC_API_KEY": "circuitry-test-fake-key"}):
            captured, _result = _call_generate(
                "circuitry.adapters.anthropic",
                lambda a=adapter, o=options: a.generate(model="m", prompt="hi", options=o),
            )
        cases.append(
            {
                "spec": spec,
                "url": captured.get("url"),
                "headers": captured.get("headers"),
                "data": captured.get("data"),
            }
        )
    return cases


def _anthropic_outcome_cases() -> list[dict[str, Any]]:
    scripted = [
        (
            0,
            ('{"content": [{"type": "text", "text": " hi "}], "usage": {"input_tokens": 3, '
            '"output_tokens": 5}, "stop_reason": "end_turn"}'),
            "",
        ),
        (
            0,
            ('{"content": [{"type": "text", "text": "a"}, {"type": "text", "text": "b"}], '
            '"stop_reason": "max_tokens"}'),
            "",
        ),
        (22, '{"error": {"message": "overloaded"}}', "curl: (22) The requested URL returned error: 529"),
    ]
    cases = []
    for returncode, stdout, stderr in scripted:
        adapter = AnthropicAdapter(base_url="https://api.anthropic.com", max_tokens=4096)
        with mock.patch.dict("os.environ", {"ANTHROPIC_API_KEY": "circuitry-test-fake-key"}):
            _captured, result = _call_generate(
                "circuitry.adapters.anthropic",
                lambda a=adapter: a.generate(model="m", prompt="hi"),
                returncode=returncode,
                stdout=stdout,
                stderr=stderr,
            )
        cases.append(
            {"returncode": returncode, "stdout": stdout, "stderr": stderr, "model": "m", "result": result}
        )
    return cases


# ---------------------------------------------------------------------
# 6. `replicate` -- its own non-OpenAI-compatible wire shape.
# ---------------------------------------------------------------------


def _replicate_request_cases() -> list[dict[str, Any]]:
    cases = []
    adapter = ReplicateAdapter(base_url="https://api.replicate.com/v1")
    with mock.patch.dict("os.environ", {"REPLICATE_API_TOKEN": "circuitry-test-fake-token"}):
        captured, _result = _call_generate(
            "circuitry.adapters.replicate",
            lambda a=adapter: a.generate(model="meta/llama", prompt="hi", timeout_seconds=90),
        )
    cases.append(
        {
            "model": "meta/llama",
            "timeout_seconds": 90,
            "url": captured.get("url"),
            "headers": captured.get("headers"),
            "data": captured.get("data"),
        }
    )
    return cases


def _replicate_outcome_cases() -> list[dict[str, Any]]:
    scripted = [
        (0, '{"status": "succeeded", "output": ["a", "b", "c"]}', ""),
        (0, '{"status": "succeeded", "output": "plain"}', ""),
        (0, '{"status": "processing", "id": "abc123"}', ""),
        (0, '{"status": "failed", "error": "boom"}', ""),
        (22, '{"detail": "unauthorized"}', "curl: (22) The requested URL returned error: 401"),
        # Issue #515 finding 4: a non-string `id` reprs as Python shows it
        # (`None`, not the Rust port's old `'?'`).
        (0, '{"status": "processing", "id": null}', ""),
        # A non-string `status` with no `error` key -- `err = raw.get("error")
        # or status` falls through to `status` itself (`None`), formatted
        # with plain `str()`.
        (0, '{"status": null}', ""),
    ]
    cases = []
    for returncode, stdout, stderr in scripted:
        adapter = ReplicateAdapter(base_url="https://api.replicate.com/v1")
        with mock.patch.dict("os.environ", {"REPLICATE_API_TOKEN": "circuitry-test-fake-token"}):
            _captured, result = _call_generate(
                "circuitry.adapters.replicate",
                lambda a=adapter: a.generate(model="meta/llama", prompt="hi", timeout_seconds=90),
                returncode=returncode,
                stdout=stdout,
                stderr=stderr,
            )
        cases.append(
            {
                "returncode": returncode,
                "stdout": stdout,
                "stderr": stderr,
                "model": "meta/llama",
                "wait": 60,
                "result": result,
            }
        )
    return cases


def build_corpus() -> dict[str, Any]:
    return {
        "parse_error_body_cases": _parse_error_body_cases(),
        "mask_credential_query_params_cases": _mask_credential_query_params_cases(),
        "strip_url_userinfo_cases": _strip_url_userinfo_cases(),
        "curl_failure_message_cases": _curl_failure_message_cases(),
        "control_char_cases": _control_char_cases(),
        "config_size_cases": _config_size_cases(),
        "ollama_request_cases": _ollama_request_cases(),
        "ollama_outcome_cases": _ollama_outcome_cases(),
        "openai_compat_request_cases": _openai_compat_request_cases(),
        "openai_compat_outcome_cases": _openai_compat_outcome_cases(),
        "openai_request_cases": _openai_request_cases(),
        "openai_outcome_cases": _openai_outcome_cases(),
        "anthropic_request_cases": _anthropic_request_cases(),
        "anthropic_outcome_cases": _anthropic_outcome_cases(),
        "replicate_request_cases": _replicate_request_cases(),
        "replicate_outcome_cases": _replicate_outcome_cases(),
    }


def main() -> int:
    data = (json.dumps(build_corpus(), indent=2, ensure_ascii=False, allow_nan=True) + "\n").encode(
        "utf-8"
    )
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    if "--check" in sys.argv[1:]:
        current = OUTPUT.read_bytes() if OUTPUT.exists() else b""
        if current != data:
            print(f"{OUTPUT} is stale; run without --check to regenerate", file=sys.stderr)
            return 1
        return 0
    OUTPUT.write_bytes(data)
    print(f"wrote corpus to {OUTPUT}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
