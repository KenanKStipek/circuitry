"""``runtime.state.prompt_sent``: what a prompt's ``meta`` keeps of its text (#577)."""

from __future__ import annotations

import hashlib
from dataclasses import dataclass, field
from typing import Any

import pytest

from circuitry.adapters.base import GenerateOptions, GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.prompt_record import (
    PromptSentSetting,
    parse_prompt_sent_setting,
    prompt_sent_fields,
)
from circuitry.core.store import Store

MARKER = "PROMPT-ONLY-MARKER-7f3a"


@dataclass
class RecordingAdapter:
    name: str = "rec"
    fail_first: int = 0
    calls: list[dict[str, Any]] = field(default_factory=list)

    def generate(
        self,
        *,
        model: str,
        prompt: str,
        timeout_seconds: int = 120,
        options: GenerateOptions | None = None,
    ) -> GenerateResult:
        self.calls.append({"model": model, "prompt": prompt, "options": options})
        if len(self.calls) <= self.fail_first:
            raise TimeoutError("boom")
        return GenerateResult(text="ok", raw={})


def _run(
    effect: dict[str, Any],
    *,
    prompt_sent: str | None = None,
    adapter: RecordingAdapter | None = None,
    dry_run: bool = False,
    state: dict[str, Any] | None = None,
) -> tuple[dict[str, Any], RecordingAdapter]:
    adapter = adapter or RecordingAdapter()
    runtime_config: dict[str, Any] = {}
    if prompt_sent is not None:
        runtime_config["state"] = {"prompt_sent": prompt_sent}
    root = compile_orchestration(
        orch={"effects": [{"type": "prompt", "name": "ask", **effect}]}, root_name="prime"
    )
    store = Store(state or {})
    DynamicRuntime(
        root, adapter=adapter, model="m", runtime_config=runtime_config, dry_run=dry_run
    ).execute(store=store)
    return store.get("prime.ask"), adapter


# ---------- parsing ----------


@pytest.mark.parametrize(
    ("value", "expected"),
    [
        (None, PromptSentSetting("full")),
        ("full", PromptSentSetting("full")),
        ("sha256", PromptSentSetting("sha256")),
        ("head:1", PromptSentSetting("head", 1)),
        ("head:200", PromptSentSetting("head", 200)),
    ],
)
def test_a_valid_setting_parses(value: Any, expected: PromptSentSetting) -> None:
    assert parse_prompt_sent_setting(value) == (expected, [])


@pytest.mark.parametrize(
    "value",
    ["head:0", "head:-3", "head:", "head:x", "head:1.5", "head:+2", "HEAD:3", "none", "", True, 5, ["full"]],
)
def test_any_other_setting_is_an_error_naming_the_key(value: Any) -> None:
    _, errors = parse_prompt_sent_setting(value)
    assert len(errors) == 1
    assert errors[0].startswith("runtime.state.prompt_sent must be")


# ---------- the record ----------


def test_the_fields_describe_the_whole_text_in_code_points_and_utf8() -> None:
    text = "héllo 🙂 wörld"
    fields = prompt_sent_fields(text, PromptSentSetting("sha256"))
    assert fields == {
        "prompt_sent": None,
        "prompt_sent_sha256": hashlib.sha256(text.encode("utf-8")).hexdigest(),
        "prompt_chars": 13,
    }


def test_head_keeps_n_code_points_then_a_marker_saying_how_many_were_left_out() -> None:
    fields = prompt_sent_fields("a🙂bcdef", PromptSentSetting("head", 3))
    assert fields["prompt_sent"] == "a🙂b\n[… 4 more characters not recorded]"
    assert fields["prompt_chars"] == 7


@pytest.mark.parametrize("head", [5, 6, 100])
def test_head_keeps_a_text_of_n_characters_or_fewer_whole(head: int) -> None:
    assert prompt_sent_fields("abcde", PromptSentSetting("head", head))["prompt_sent"] == "abcde"


# ---------- the effect ----------


def test_the_default_records_the_whole_text_and_the_two_new_fields() -> None:
    node, _ = _run({"template": "say {{input.what}}"}, state={"input": {"what": "hi"}})
    meta = node["meta"]
    assert meta["prompt_sent"] == "say hi"
    assert meta["prompt_chars"] == 6
    assert meta["prompt_sent_sha256"] == hashlib.sha256(b"say hi").hexdigest()


def test_sha256_leaves_null_and_the_adapter_still_gets_the_whole_text() -> None:
    node, adapter = _run({"template": f"{MARKER} body"}, prompt_sent="sha256")
    assert node["meta"]["prompt_sent"] is None
    assert node["meta"]["prompt_chars"] == len(f"{MARKER} body")
    assert adapter.calls[0]["prompt"] == f"{MARKER} body"


def test_head_records_a_prefix_and_the_adapter_still_gets_the_whole_text() -> None:
    node, adapter = _run({"template": "abcdefghij"}, prompt_sent="head:4")
    assert node["meta"]["prompt_sent"] == "abcd\n[… 6 more characters not recorded]"
    assert adapter.calls[0]["prompt"] == "abcdefghij"


def test_a_messages_prompt_records_the_flattened_form() -> None:
    messages = [
        {"role": "system", "content": "be brief"},
        {"role": "user", "content": f"{MARKER} hello"},
    ]
    flat = f"system: be brief\n\nuser: {MARKER} hello"
    full, _ = _run({"messages": messages})
    assert full["meta"]["prompt_sent"] == flat

    node, adapter = _run({"messages": messages}, prompt_sent="sha256")
    assert node["meta"]["prompt_sent"] is None
    assert node["meta"]["prompt_chars"] == len(flat)
    assert node["meta"]["prompt_sent_sha256"] == hashlib.sha256(flat.encode()).hexdigest()
    assert adapter.calls[0]["prompt"] == flat
    assert [m.content for m in adapter.calls[0]["options"].messages] == ["be brief", f"{MARKER} hello"]


def test_a_dry_run_records_it_the_same_way() -> None:
    node, adapter = _run({"template": f"{MARKER}"}, prompt_sent="sha256", dry_run=True)
    assert node["meta"]["prompt_sent"] is None
    assert node["meta"]["prompt_chars"] == len(MARKER)
    assert adapter.calls == []


def test_a_failed_dispatch_under_on_error_continue_records_it() -> None:
    adapter = RecordingAdapter(fail_first=5)
    node, _ = _run(
        {"template": f"{MARKER} text", "on_error": "continue"},
        prompt_sent="head:3",
        adapter=adapter,
    )
    meta = node["meta"]
    assert meta["error"]
    total = len(f"{MARKER} text")
    assert meta["prompt_sent"] == f"PRO\n[… {total - 3} more characters not recorded]"
    assert meta["prompt_chars"] == total


def test_retries_and_fallback_records_never_carry_the_prompt_text() -> None:
    adapter = RecordingAdapter(fail_first=1)
    node, _ = _run(
        {"template": f"{MARKER} text", "retries": {"max_attempts": 2, "backoff_ms": 1}},
        prompt_sent="sha256",
        adapter=adapter,
    )
    assert len(adapter.calls) == 2
    assert all(call["prompt"] == f"{MARKER} text" for call in adapter.calls)
    assert MARKER not in repr(node)


def test_sha256_leaves_no_copy_of_the_text_anywhere_in_the_node() -> None:
    node, _ = _run({"template": f"{MARKER} text"}, prompt_sent="sha256")
    assert MARKER not in repr(node)


def test_the_same_document_gets_the_same_adapter_input_in_every_mode() -> None:
    inputs = []
    for mode in ("full", "sha256", "head:3"):
        node, adapter = _run({"template": "abcdefghij", "params": {"temperature": 0.2}}, prompt_sent=mode)
        call = adapter.calls[0]
        inputs.append((call["prompt"], call["options"], node["value"], node["meta"]["prompt_chars"]))
    assert inputs[0] == inputs[1] == inputs[2]
