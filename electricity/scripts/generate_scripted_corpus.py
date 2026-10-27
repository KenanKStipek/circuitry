#!/usr/bin/env python3
"""Generate the golden corpus for lane F1 of issue #448 (#478):
electricity's own Rust `scripted` adapter, replayed against Circuitry's
real `circuitry.adapters.scripted.ScriptedAdapter` (`check()`/`generate()`,
driven through real replies files -- never a private helper reached into
directly, the same convention `generate_stdlib_tools_corpus.py`/
`generate_tool_runtime_corpus.py` already follow: this corpus drives the
adapter's own public surface, not `_parse_replies`/`_load_raw` by hand).

Every replies-file case runs with its process `cwd` set to the file's own
fresh temp directory and `replies_file` given as a bare, relative filename
(`"replies.yaml"`), so a recorded message's own `str(Path(...))` is never
an absolute path the generating machine happens to have -- it is always
exactly the literal filename, just as any contributor's or CI's own
generated corpus would record, with nothing to redact.

`ScriptedAdapter.generate()` reads `core.effect_identity.current_call_path()`,
a `contextvars.ContextVar` normally set by `core.effect_identity.model_call`
for the duration of one dispatch; this script sets it directly (the same
context manager without needing a real `Store`) rather than building one
just to get a call path into scope.

Must be run with Python 3.11 (the lane venv locally; `actions/setup-python`
3.11 in CI). Usage: python3 generate_scripted_corpus.py [--check]
"""

from __future__ import annotations

import contextlib
import json
import os
import sys
from pathlib import Path
from tempfile import TemporaryDirectory
from typing import Any

from circuitry.adapters._retry import AdapterCallError
from circuitry.adapters.base import (
    ChatMessage,
    GenerateOptions,
    ImageInput,
    ignored_options_warning,
)
from circuitry.adapters.factory import _build_host_claude
from circuitry.adapters.scripted import (
    ScriptedAdapter,
    ScriptedRepliesError,
    _merge_leftover_replies,
)
from circuitry.core import effect_identity

OUTPUT = (
    Path(__file__).resolve().parent.parent
    / "crates"
    / "electricity-adapters"
    / "tests"
    / "golden"
    / "scripted_corpus.json"
)


@contextlib.contextmanager
def _call_path(path: str | None):
    """`core.effect_identity.model_call`'s own effect, without a `Store` --
    sets/resets the identical `ContextVar` that function uses, for exactly
    the duration of one `generate()` call."""
    token = effect_identity._CALL_PATH.set(path)
    try:
        yield
    finally:
        effect_identity._CALL_PATH.reset(token)


def _encode_retry(exc: AdapterCallError) -> dict[str, Any]:
    info = exc.retry_info
    return {
        "retryable": info.retryable,
        "status": info.status,
        "retry_after": info.retry_after,
        "error_code": exc.error_code,
    }


def _run_calls(adapter: ScriptedAdapter, calls: list[dict[str, Any]]) -> list[dict[str, Any]]:
    results = []
    for call in calls:
        if call["kind"] == "check":
            # `check()` itself only ever catches `ScriptedRepliesError`
            # (folding it to `str(e)`) -- any other exception
            # (`UnicodeDecodeError`, `TypeError`) propagates out of it
            # uncaught, for `preflight.call_check` to fold to
            # `check() raised {type(exc).__name__}: {exc}`. Recording
            # that exact folded text here, rather than a separate
            # "raised" field, lets the Rust replay compare `message`
            # directly for every case regardless of which path produced
            # it.
            try:
                result = adapter.check()
            except Exception as exc:
                results.append(
                    {
                        "kind": "check",
                        "ok": False,
                        "missing": [],
                        "message": f"check() raised {type(exc).__name__}: {exc}",
                    }
                )
                continue
            results.append(
                {
                    "kind": "check",
                    "ok": result.ok,
                    "missing": result.missing,
                    "message": result.message,
                }
            )
            continue
        assert call["kind"] == "generate"
        options = call.get("options")
        with _call_path(call["path"]):
            try:
                reply = adapter.generate(
                    model="m", prompt="p", timeout_seconds=30, options=options
                )
            except AdapterCallError as e:
                results.append(
                    {
                        "kind": "generate",
                        "path": call["path"],
                        "error_class": "AdapterCallError",
                        "message": str(e),
                        "retry": _encode_retry(e),
                    }
                )
            except ScriptedRepliesError as e:
                results.append(
                    {
                        "kind": "generate",
                        "path": call["path"],
                        "error_class": "ScriptedRepliesError",
                        "message": str(e),
                    }
                )
            except RuntimeError as e:
                results.append(
                    {
                        "kind": "generate",
                        "path": call["path"],
                        "error_class": "RuntimeError",
                        "message": str(e),
                    }
                )
            except TypeError as e:
                results.append(
                    {
                        "kind": "generate",
                        "path": call["path"],
                        "error_class": "TypeError",
                        "message": str(e),
                    }
                )
            else:
                results.append(
                    {
                        "kind": "generate",
                        "path": call["path"],
                        "ok": {
                            "text": reply.text,
                            "tokens_sent": reply.tokens_sent,
                            "tokens_received": reply.tokens_received,
                            "finish_reason": reply.finish_reason,
                            "warnings": list(reply.warnings),
                        },
                    }
                )
    return results


def _replies_case(
    name: str,
    *,
    filename: str,
    contents: str,
    calls: list[dict[str, Any]],
    raw_bytes: bytes | None = None,
) -> dict[str, Any]:
    """*raw_bytes*, when given, is written to *filename* instead of
    *contents* (which still records in the corpus for a reader's own
    reference) -- the one way to get invalid UTF-8 onto disk, since
    *contents* is itself a Python `str`."""
    with TemporaryDirectory(prefix="electricity-scripted-corpus-") as tmp:
        previous_cwd = os.getcwd()
        os.chdir(tmp)
        try:
            if raw_bytes is not None:
                (Path(tmp) / filename).write_bytes(raw_bytes)
            else:
                (Path(tmp) / filename).write_text(contents, encoding="utf-8")
            adapter = ScriptedAdapter(replies_file=filename)
            results = _run_calls(adapter, calls)
        finally:
            os.chdir(previous_cwd)
    return {
        "case": name,
        "filename": filename,
        "contents": contents,
        # Hex-encoded, since JSON text can't carry invalid UTF-8 bytes
        # directly -- only this one case ever sets it; every other case
        # is `None` and the Rust replay writes `contents` verbatim.
        "raw_bytes_hex": raw_bytes.hex() if raw_bytes is not None else None,
        "calls": results,
    }


def build_replies_cases() -> list[dict[str, Any]]:
    cases: list[dict[str, Any]] = []

    cases.append(
        _replies_case(
            "text_reply_with_tokens_and_finish_reason",
            filename="replies.yaml",
            contents=(
                "prime.x:\n"
                "  - text: hello\n"
                "    tokens_sent: 3\n"
                "    tokens_received: 5\n"
                "    finish_reason: stop\n"
            ),
            calls=[{"kind": "generate", "path": "prime.x"}],
        )
    )

    cases.append(
        _replies_case(
            "dot_json_suffix_parses_as_plain_json",
            filename="replies.json",
            contents='{"prime.x": [{"text": "from json"}]}',
            calls=[{"kind": "generate", "path": "prime.x"}],
        )
    )

    cases.append(
        _replies_case(
            "a_non_json_suffix_parses_as_yaml_even_with_json_like_content",
            filename="replies.txt",
            contents='{"prime.x": [{"text": "from yaml flow mapping"}]}',
            calls=[{"kind": "generate", "path": "prime.x"}],
        )
    )

    cases.append(
        _replies_case(
            "an_exhausted_queue_then_fails_without_naming_the_replies_file",
            filename="replies.yaml",
            contents="prime.x:\n  - text: only\n",
            calls=[
                {"kind": "generate", "path": "prime.x"},
                {"kind": "generate", "path": "prime.x"},
            ],
        )
    )

    cases.append(
        _replies_case(
            "an_unconfigured_path_fails_without_naming_the_replies_file",
            filename="replies.yaml",
            contents="prime.known:\n  - text: x\n",
            calls=[{"kind": "generate", "path": "prime.unknown"}],
        )
    )

    cases.append(
        _replies_case(
            "an_error_reply_is_consumed_even_though_it_fails",
            filename="replies.yaml",
            contents=(
                "prime.flaky:\n"
                "  - error:\n"
                "      kind: server_error\n"
                "  - text: recovered\n"
            ),
            calls=[
                {"kind": "generate", "path": "prime.flaky"},
                {"kind": "generate", "path": "prime.flaky"},
            ],
        )
    )

    for kind, status_yaml in [
        ("rate_limited", ""),
        ("server_error", ""),
        ("invalid_request", ""),
        ("unauthorized", ""),
        ("not_found", ""),
        ("timeout", ""),
        ("connection", ""),
        ("http", "\n      status: 503"),
    ]:
        cases.append(
            _replies_case(
                f"error_kind_default_status_{kind}",
                filename="replies.yaml",
                contents=f"prime.x:\n  - error:\n      kind: {kind}{status_yaml}\n",
                calls=[{"kind": "generate", "path": "prime.x"}],
            )
        )

    cases.extend(
        _replies_case(
            f"{kind}_kind_keeps_an_explicit_status_and_stays_retryable",
            filename="replies.yaml",
            contents=f"prime.x:\n  - error:\n      kind: {kind}\n      status: 503\n",
            calls=[{"kind": "generate", "path": "prime.x"}],
        )
        for kind in ("timeout", "connection")
    )

    cases.append(
        _replies_case(
            "an_empty_message_falls_back_to_the_generated_default",
            filename="replies.yaml",
            contents="prime.x:\n  - error:\n      kind: rate_limited\n      message: ''\n",
            calls=[{"kind": "generate", "path": "prime.x"}],
        )
    )

    cases.append(
        _replies_case(
            "an_explicit_message_overrides_the_generated_default",
            filename="replies.yaml",
            contents=(
                "prime.x:\n  - error:\n      kind: rate_limited\n      message: slow down\n"
            ),
            calls=[{"kind": "generate", "path": "prime.x"}],
        )
    )

    cases.append(
        _replies_case(
            "an_error_codes_value_reaches_the_adapter_call_error",
            filename="replies.yaml",
            contents="prime.x:\n  - error:\n      kind: invalid_request\n      code: bad_schema\n",
            calls=[{"kind": "generate", "path": "prime.x"}],
        )
    )

    cases.append(
        _replies_case(
            "an_error_without_a_code_leaves_it_none",
            filename="replies.yaml",
            contents="prime.x:\n  - error:\n      kind: timeout\n",
            calls=[{"kind": "generate", "path": "prime.x"}],
        )
    )

    cases.append(
        _replies_case(
            "an_overridden_status_still_classifies_by_status_not_kind_name",
            filename="replies.yaml",
            contents="prime.x:\n  - error:\n      kind: server_error\n      status: 400\n",
            calls=[{"kind": "generate", "path": "prime.x"}],
        )
    )

    cases.append(
        _replies_case(
            "a_status_beyond_u16_still_classifies_by_the_full_value",
            filename="replies.yaml",
            contents=(
                "prime.x:\n  - error:\n      kind: server_error\n"
                "      status: 99999999999999999999999999\n"
            ),
            calls=[{"kind": "generate", "path": "prime.x"}],
        )
    )

    cases.append(
        _replies_case(
            "an_unhashable_kind_raises_pythons_own_type_error",
            filename="replies.yaml",
            contents="prime.x:\n  - error:\n      kind: [1, 2]\n",
            calls=[{"kind": "generate", "path": "prime.x"}],
        )
    )

    # Load-time (structural) validation failures -- every one of these
    # fails at `check()`, before any path is ever dispatched.
    structural_cases = [
        ("a_scalar_document_is_not_a_mapping", "just a string\n"),
        ("an_empty_key_is_rejected", '{"": [{"text": "x"}]}'),
        ("an_empty_list_is_rejected", "prime.x: []\n"),
        ("a_non_list_value_is_rejected", "prime.x: 5\n"),
        ("a_non_mapping_reply_is_rejected", "prime.x:\n  - 5\n"),
        (
            "both_text_and_error_is_rejected",
            "prime.x:\n  - text: x\n    error:\n      kind: timeout\n",
        ),
        ("an_unknown_text_reply_key_is_rejected", "prime.x:\n  - text: x\n    token_sent: 1\n"),
        ("a_missing_text_is_rejected", "prime.x:\n  - finish_reason: stop\n"),
        ("a_negative_tokens_sent_is_rejected", "prime.x:\n  - text: x\n    tokens_sent: -1\n"),
        (
            "a_boolean_tokens_sent_is_rejected",
            "prime.x:\n  - text: x\n    tokens_sent: true\n",
        ),
        (
            "a_non_string_finish_reason_is_rejected",
            "prime.x:\n  - text: x\n    finish_reason: 5\n",
        ),
        ("error_without_a_kind_is_rejected", "prime.x:\n  - error:\n      message: oops\n"),
        (
            "an_unknown_error_key_is_rejected",
            "prime.x:\n  - error:\n      kind: timeout\n      bogus: 1\n",
        ),
        (
            "an_unknown_error_kind_is_rejected",
            "prime.x:\n  - error:\n      kind: bogus\n",
        ),
        (
            "a_non_int_status_is_rejected",
            "prime.x:\n  - error:\n      kind: server_error\n      status: not-a-number\n",
        ),
        (
            "a_null_status_on_a_kind_that_needs_one_is_rejected",
            "prime.x:\n  - error:\n      kind: server_error\n      status: null\n",
        ),
        (
            "a_non_string_message_is_rejected",
            "prime.x:\n  - error:\n      kind: timeout\n      message: 5\n",
        ),
        (
            "an_empty_code_is_rejected",
            "prime.x:\n  - error:\n      kind: timeout\n      code: ''\n",
        ),
        (
            "a_non_string_code_is_rejected",
            "prime.x:\n  - error:\n      kind: timeout\n      code: 5\n",
        ),
        (
            "an_error_replys_sibling_keys_are_not_checked",
            "prime.x:\n  - error:\n      kind: timeout\n    tokens_sent: 5\n",
        ),
        (
            "a_non_string_int_key_is_rejected",
            "5:\n  - text: x\n",
        ),
        (
            "two_unknown_text_reply_keys_sort_by_string_value_not_repr",
            # Python's `sorted({"b", "it's"})` gives `['b', "it's"]` --
            # sorting by `repr()` instead would put `"it's"` first (a
            # double quote sorts before `b`).
            "prime.x:\n  - text: x\n    b: 1\n    \"it's\": 2\n",
        ),
    ]
    for name, contents in structural_cases:
        cases.append(
            _replies_case(name, filename="replies.yaml", contents=contents, calls=[{"kind": "check"}])
        )

    # A `null` document (an empty file, or a literal `null`) means `{}` --
    # legal, not a structural failure, so it is a `generate` case (which
    # fails for the ordinary "no reply configured" reason), not a `check`
    # case.
    cases.append(
        _replies_case(
            "a_null_document_means_an_empty_mapping",
            filename="replies.yaml",
            contents="null\n",
            calls=[{"kind": "generate", "path": "prime.x"}],
        )
    )

    # `_load_raw`'s own three failure texts, each compared exactly since
    # every path here is the fixed, relative filename this generator
    # always uses -- never the generating machine's own absolute path.
    cases.append(
        _replies_case(
            "invalid_json_syntax_reports_cpythons_exact_jsondecodeerror_text",
            filename="replies.json",
            contents="{not valid json",
            calls=[{"kind": "check"}],
        )
    )
    cases.append(
        _replies_case(
            "invalid_yaml_syntax_reports_the_parsers_exact_text",
            filename="replies.yaml",
            contents="prime.x: [unterminated\n",
            calls=[{"kind": "check"}],
        )
    )
    cases.append(
        _replies_case(
            "invalid_utf8_raises_pythons_own_unicodedecodeerror_text",
            filename="replies.yaml",
            contents="placeholder",
            raw_bytes=b"\xff\xfe",
            calls=[{"kind": "check"}],
        )
    )

    return cases


def _merge_case(
    name: str, *, filename: str, contents: str, instances_calls: list[list[dict[str, Any]]]
) -> dict[str, Any]:
    """`_merge_leftover_replies` (`adapters/scripted.py`) over *len(instances_calls)*
    independent instances loading one shared file -- each instance runs its own
    *calls* (discarding their own results; only the merged leftover counts matter
    here), the same `_run_calls` helper the replies cases use."""
    with TemporaryDirectory(prefix="electricity-scripted-corpus-") as tmp:
        previous_cwd = os.getcwd()
        os.chdir(tmp)
        try:
            (Path(tmp) / filename).write_text(contents, encoding="utf-8")
            instances = [ScriptedAdapter(replies_file=filename) for _ in instances_calls]
            for instance, calls in zip(instances, instances_calls, strict=True):
                _run_calls(instance, calls)
            # Recorded per-instance so the Rust replay can feed them
            # straight into its own pure merge-arithmetic helper
            # (`electricity_adapters::merge_remaining_counts`) without
            # ever touching its process-wide export registry -- that
            # registry is append-only for the process's whole
            # lifetime, so replaying several independent scenarios
            # against it directly, in one test binary, would let one
            # scenario's registrations leak into the next's result.
            per_instance_counts = [instance._remaining_counts() for instance in instances]
            merged = _merge_leftover_replies(instances)
        finally:
            os.chdir(previous_cwd)
    return {
        "case": name,
        "filename": filename,
        "contents": contents,
        "per_instance_counts": per_instance_counts,
        "merged": merged,
    }


def build_merge_cases() -> list[dict[str, Any]]:
    shared_contents = (
        "prime.x:\n  - text: one\n  - text: two\nprime.y:\n  - text: only\n"
        "prime.z:\n  - text: untouched\n"
    )
    return [
        _merge_case(
            "the_minimum_across_instances_wins_never_the_sum",
            filename="replies.yaml",
            contents=shared_contents,
            instances_calls=[
                # `a` consumes one of `prime.x`'s two replies and all of
                # `prime.y`'s one.
                [
                    {"kind": "generate", "path": "prime.x"},
                    {"kind": "generate", "path": "prime.y"},
                ],
                # `b` only calls `check()` -- a preflight-only instance,
                # full queue, never consumed -- its vote must never pull
                # `prime.x`/`prime.y`'s reported count back up.
                [{"kind": "check"}],
            ],
        ),
        _merge_case(
            "a_path_fully_consumed_by_every_instance_is_omitted_not_zero",
            filename="replies.yaml",
            contents="prime.x:\n  - text: only\n",
            instances_calls=[[{"kind": "generate", "path": "prime.x"}]],
        ),
        _merge_case(
            "a_single_preflight_only_instance_reports_every_path_at_its_full_count",
            filename="replies.yaml",
            contents=shared_contents,
            instances_calls=[[{"kind": "check"}]],
        ),
    ]


def options_case(
    name: str,
    *,
    temperature: float | None = None,
    max_tokens: int | None = None,
    stop: tuple[str, ...] = (),
    params: dict[str, Any] | None = None,
    deterministic: bool = False,
    messages: tuple[ChatMessage, ...] = (),
    images: tuple[ImageInput, ...] = (),
    used: frozenset[str] = frozenset(),
) -> dict[str, Any]:
    options = GenerateOptions(
        temperature=temperature,
        max_tokens=max_tokens,
        stop=stop,
        params=params or {},
        deterministic=deterministic,
        messages=messages,
        images=images,
    )
    return {
        "case": name,
        "input": {
            "temperature": temperature,
            "max_tokens": max_tokens,
            "stop": list(stop),
            "params": params or {},
            "messages_count": len(messages),
            "images_count": len(images),
            "used": sorted(used),
        },
        "set_fields": options.set_fields(),
        "warnings": list(ignored_options_warning("scripted", options, used=used)),
    }


def build_options_cases() -> list[dict[str, Any]]:
    return [
        options_case("no_fields_set"),
        options_case("temperature_only", temperature=0.7),
        options_case(
            "every_field_set_scripted_honours_none_of_it",
            temperature=0.1,
            max_tokens=10,
            stop=("END",),
            params={"b": 1, "a": 2},
            deterministic=True,
            messages=(ChatMessage(role="user", content="hi"),),
            images=(ImageInput(data=b"x", media_type="image/png"),),
        ),
        options_case(
            "used_removes_matching_fields_by_first_word",
            params={"seed": 1},
            messages=(ChatMessage(role="user", content="hi"),),
            used=frozenset({"params"}),
        ),
        options_case(
            "params_keys_are_sorted_by_code_point",
            params={"zeta": 1, "alpha": 2, "Beta": 3},
        ),
    ]


def render(corpus: dict) -> str:
    return json.dumps(corpus, indent=2, ensure_ascii=False, sort_keys=True) + "\n"


def write_or_check(path: Path, text: str, *, check: bool) -> bool:
    path.parent.mkdir(parents=True, exist_ok=True)
    if check:
        current = path.read_text() if path.exists() else ""
        if current != text:
            print(f"{path} is stale; run without --check to regenerate", file=sys.stderr)
            return False
        return True
    path.write_text(text)
    print(f"wrote {path}")
    return True


def main() -> int:
    check = "--check" in sys.argv[1:]
    corpus = {
        "replies_cases": build_replies_cases(),
        "options_cases": build_options_cases(),
        "merge_cases": build_merge_cases(),
        "host_claude_message": _host_claude_message_text(),
    }
    text = render(corpus)
    return 0 if write_or_check(OUTPUT, text, check=check) else 1


def _host_claude_message_text() -> str:
    try:
        _build_host_claude({})
    except RuntimeError as e:
        return str(e)
    raise AssertionError("_build_host_claude({}) was expected to always raise")


if __name__ == "__main__":
    raise SystemExit(main())
