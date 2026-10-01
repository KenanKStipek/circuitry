"""A prompt's params, deterministic, timeout_ms, messages and assets reach
the adapter, and its reply's finish_reason lands in meta (issue #250)."""

from __future__ import annotations

import hashlib
import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import pytest

from circuitry.adapters.base import GenerateOptions, GenerateResult
from circuitry.core.compiler import compile_orchestration
from circuitry.core.dynamic import DynamicRuntime
from circuitry.core.store import Store

PNG_BYTES = b"\x89PNG\r\n\x1a\n" + b"\x00" * 24


@dataclass
class RecordingAdapter:
    """Takes ``options`` and remembers every call."""

    name: str = "rec"
    reply: str = "ok"
    finish_reason: str | None = "stop"
    calls: list[dict[str, Any]] = field(default_factory=list)

    def generate(
        self,
        *,
        model: str,
        prompt: str,
        timeout_seconds: int = 120,
        options: GenerateOptions | None = None,
    ) -> GenerateResult:
        self.calls.append(
            {"model": model, "prompt": prompt, "timeout": timeout_seconds, "options": options}
        )
        return GenerateResult(text=self.reply, raw={}, finish_reason=self.finish_reason)


@dataclass(frozen=True)
class LegacyAdapter:
    name: str = "legacy"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        return GenerateResult(text="ok", raw={})


def _run(
    effect: dict[str, Any],
    adapter: Any,
    *,
    timeout_seconds: int = 120,
    state: dict[str, Any] | None = None,
) -> dict[str, Any]:
    root = compile_orchestration(
        orch={"effects": [{"type": "prompt", "name": "ask", **effect}]}, root_name="prime"
    )
    store = Store(state or {})
    DynamicRuntime(
        root, adapter=adapter, model="m", timeout_seconds=timeout_seconds
    ).execute(store=store)
    return store.get("prime.ask")


def test_params_are_split_into_portable_knobs_and_provider_extras() -> None:
    adapter = RecordingAdapter()
    _run(
        {
            "template": "hi",
            "params": {"temperature": 0.4, "max_tokens": 5, "stop": "END", "num_ctx": 8192},
        },
        adapter,
    )
    options = adapter.calls[0]["options"]
    assert options.temperature == 0.4
    assert options.max_tokens == 5
    assert options.stop == ("END",)
    assert dict(options.params) == {"num_ctx": 8192}
    assert options.deterministic is False


def test_deterministic_means_temperature_zero() -> None:
    adapter = RecordingAdapter()
    _run({"template": "hi", "deterministic": True}, adapter)
    options = adapter.calls[0]["options"]
    assert options.temperature == 0.0
    assert options.deterministic is True


def test_params_temperature_outranks_deterministic() -> None:
    adapter = RecordingAdapter()
    _run({"template": "hi", "deterministic": True, "params": {"temperature": 0.7}}, adapter)
    assert adapter.calls[0]["options"].temperature == 0.7


def test_a_prompt_without_options_sends_none() -> None:
    adapter = RecordingAdapter()
    node = _run({"template": "hi"}, adapter)
    # Empty options are not passed at all, so the call is the pre-#250 call.
    assert adapter.calls[0]["options"] is None
    assert "warnings" not in node["meta"]
    assert "assets" not in node["meta"]


@pytest.mark.parametrize(
    ("params", "match"),
    [
        ({"temperature": "hot"}, "temperature must be a number"),
        ({"max_tokens": 0}, "max_tokens must be a positive integer"),
        ({"stop": [1]}, "stop must be a string or a list of strings"),
    ],
)
def test_malformed_params_fail_the_effect(params: dict[str, Any], match: str) -> None:
    adapter = RecordingAdapter()
    node = _run({"template": "hi", "params": params, "on_error": "continue"}, adapter)
    assert match in node["meta"]["error"]
    assert adapter.calls == []


@pytest.mark.parametrize(
    ("timeout_ms", "adapter_timeout", "expected"),
    [
        (None, 120, 120),
        (0, 120, 120),
        (1, 120, 1),
        (1500, 120, 2),
        (30_000, 120, 30),
        (600_000, 120, 120),
    ],
)
def test_timeout_ms_is_capped_by_the_adapter_timeout(
    timeout_ms: int | None, adapter_timeout: int, expected: int
) -> None:
    adapter = RecordingAdapter()
    effect: dict[str, Any] = {"template": "hi"}
    if timeout_ms is not None:
        effect["timeout_ms"] = timeout_ms
    _run(effect, adapter, timeout_seconds=adapter_timeout)
    assert adapter.calls[0]["timeout"] == expected


def test_messages_keep_their_roles_and_prompt_sent_stays_flattened() -> None:
    adapter = RecordingAdapter()
    node = _run(
        {
            "messages": [
                {"role": "system", "content": "You triage issues."},
                {"role": "user", "content": "Is this a bug? {{input.issue}}"},
            ]
        },
        adapter,
        state={"input": {"issue": "crash on start"}},
    )
    call = adapter.calls[0]
    assert [(m.role, m.content) for m in call["options"].messages] == [
        ("system", "You triage issues."),
        ("user", "Is this a bug? crash on start"),
    ]
    flattened = "system: You triage issues.\n\nuser: Is this a bug? crash on start"
    assert call["prompt"] == flattened
    assert node["meta"]["prompt_sent"] == flattened


def test_image_assets_are_read_and_recorded_without_their_bytes(tmp_path: Path) -> None:
    image = tmp_path / "shot.png"
    image.write_bytes(PNG_BYTES)
    adapter = RecordingAdapter()
    node = _run(
        {
            "template": "What is this?",
            "assets": [
                {"kind": "image", "ref": "{{input.dir}}/shot.png"},
                {"kind": "image", "ref": "https://x.test/remote.jpg"},
            ],
        },
        adapter,
        state={"input": {"dir": str(tmp_path)}},
    )
    images = adapter.calls[0]["options"].images
    assert images[0].data == PNG_BYTES
    assert images[0].media_type == "image/png"
    assert images[1].url == "https://x.test/remote.jpg"
    assert images[1].data is None
    assert node["meta"]["assets"] == [
        {
            "kind": "image",
            "ref": str(image),
            "media_type": "image/png",
            "size": len(PNG_BYTES),
            "sha256": hashlib.sha256(PNG_BYTES).hexdigest(),
        },
        {"kind": "image", "ref": "https://x.test/remote.jpg"},
    ]
    assert "AAAA" not in json.dumps(node)  # base64 of the zero bytes


def test_a_missing_image_fails_the_effect(tmp_path: Path) -> None:
    adapter = RecordingAdapter()
    node = _run(
        {
            "template": "What is this?",
            "assets": [{"kind": "image", "ref": str(tmp_path / "nope.png")}],
            "on_error": "continue",
        },
        adapter,
    )
    assert "could not be read" in node["meta"]["error"]
    assert adapter.calls == []


def test_a_non_image_asset_is_skipped_with_a_warning(tmp_path: Path) -> None:
    adapter = RecordingAdapter()
    node = _run(
        {"template": "hi", "assets": [{"kind": "audio", "ref": "a.wav"}]}, adapter
    )
    assert adapter.calls[0]["options"] is None
    assert node["meta"]["warnings"] == [
        "asset kind 'audio' is not sent to any adapter; only images are (a.wav)"
    ]


def test_finish_reason_is_recorded_and_truncation_warned() -> None:
    node = _run({"template": "hi"}, RecordingAdapter(finish_reason="length"))
    assert node["meta"]["finish_reason"] == "length"
    assert node["meta"]["warnings"] == [
        (
            "the reply was cut off by the length limit (finish_reason: length); "
            "raise params.max_tokens"
        )
    ]


def test_a_normal_stop_records_finish_reason_without_a_warning() -> None:
    node = _run({"template": "hi"}, RecordingAdapter(finish_reason="end_turn"))
    assert node["meta"]["finish_reason"] == "end_turn"
    assert "warnings" not in node["meta"]


def test_no_finish_reason_means_no_key() -> None:
    node = _run({"template": "hi"}, RecordingAdapter(finish_reason=None))
    assert "finish_reason" not in node["meta"]


def test_a_truncated_reply_that_fails_to_parse_still_carries_the_warning() -> None:
    node = _run(
        {
            "template": "hi",
            "prompt_type": "json",
            "schema": {"type": "object"},
            "on_error": "continue",
        },
        RecordingAdapter(reply='{"severity": "hi', finish_reason="length"),
    )
    assert node["meta"]["error"]
    assert node["meta"]["finish_reason"] == "length"
    assert any("cut off" in w for w in node["meta"]["warnings"])


def test_a_legacy_adapter_runs_and_the_ignored_options_are_named() -> None:
    node = _run(
        {"template": "hi", "deterministic": True, "params": {"num_ctx": 2048}},
        LegacyAdapter(),
    )
    assert node["value"] == "ok"
    assert node["meta"]["warnings"] == [
        "adapter 'legacy' ignored: temperature; params (num_ctx)"
    ]
