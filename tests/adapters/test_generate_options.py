"""GenerateOptions on the adapter contract: the call_generate shim, and how
ollama, the OpenAI-compatible family, openai and anthropic map options,
role-tagged turns and images onto their wire formats (curl mocked, no
network). See issue #250.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from typing import Any

import pytest

from circuitry.adapters import (
    AnthropicAdapter,
    GroqAdapter,
    HostClaudeAdapter,
    OllamaAdapter,
    OpenAIAdapter,
)
from circuitry.adapters.base import (
    DETERMINISTIC_SEED,
    ChatMessage,
    GenerateOptions,
    GenerateResult,
    ImageInput,
    adapter_accepts_images,
    call_generate,
)
from circuitry.adapters.conformance import validate_generate_result
from circuitry.adapters.factory import ADAPTER_REGISTRY, build_adapter

PNG = ImageInput(data=b"\x89PNG\r\n\x1a\nfake", media_type="image/png")
PNG_B64 = "iVBORw0KGgpmYWtl"

TURNS = (
    ChatMessage(role="system", content="Be terse."),
    ChatMessage(role="user", content="Hi"),
    ChatMessage(role="assistant", content="Hello."),
    ChatMessage(role="user", content="What is this?"),
)


@dataclass(frozen=True)
class FakeProc:
    returncode: int
    stdout: str = ""
    stderr: str = ""


class CurlRecorder:
    """Stands in for ``subprocess.run``; keeps the command and the JSON body
    sent on stdin."""

    def __init__(self, response: dict[str, Any]) -> None:
        self.response = response
        self.cmd: list[str] = []
        self.stdin: str | None = None

    def __call__(self, cmd: list[str], **kwargs: Any) -> FakeProc:
        self.cmd = cmd
        self.stdin = kwargs.get("input")
        return FakeProc(returncode=0, stdout=json.dumps(self.response))

    @property
    def body(self) -> dict[str, Any]:
        assert self.cmd[self.cmd.index("--data-binary") + 1] == "@-"
        assert self.stdin is not None
        return json.loads(self.stdin)

    @property
    def url(self) -> str:
        return self.cmd[-1]

    @property
    def max_time(self) -> str:
        return self.cmd[self.cmd.index("--max-time") + 1]


def _record(monkeypatch: pytest.MonkeyPatch, response: dict[str, Any]) -> CurlRecorder:
    recorder = CurlRecorder(response)
    monkeypatch.setattr("subprocess.run", recorder)
    return recorder


# ---------- the call_generate shim ----------


@dataclass(frozen=True)
class LegacyAdapter:
    """An out-of-tree adapter written before ``options`` existed."""

    name: str = "legacy"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        return GenerateResult(text=prompt, raw={"timeout": timeout_seconds})


@dataclass(frozen=True)
class OptionsAdapter:
    name: str = "modern"

    def generate(
        self,
        *,
        model: str,
        prompt: str,
        timeout_seconds: int = 120,
        options: GenerateOptions | None = None,
    ) -> GenerateResult:
        return GenerateResult(text=prompt, raw={"options": options})


def test_call_generate_passes_options_to_an_adapter_that_takes_them() -> None:
    options = GenerateOptions(temperature=0.2)
    result = call_generate(
        OptionsAdapter(), model="m", prompt="p", timeout_seconds=5, options=options
    )
    assert result.raw["options"] is options
    assert result.warnings == ()


def test_call_generate_runs_a_legacy_adapter_and_names_what_it_ignored() -> None:
    options = GenerateOptions(
        temperature=0.0,
        params={"top_p": 0.9},
        messages=(ChatMessage(role="user", content="x"),),
        images=(PNG,),
    )
    result = call_generate(
        LegacyAdapter(), model="m", prompt="p", timeout_seconds=7, options=options
    )
    assert result.text == "p"
    assert result.raw == {"timeout": 7}
    assert len(result.warnings) == 1
    warning = result.warnings[0]
    assert "adapter 'legacy' ignored" in warning
    for name in ("temperature", "params (top_p)", "messages", "images"):
        assert name in warning


def test_call_generate_without_options_adds_no_warning() -> None:
    result = call_generate(
        LegacyAdapter(), model="m", prompt="p", timeout_seconds=7, options=GenerateOptions()
    )
    assert result.warnings == ()


# ---------- ollama ----------


def test_ollama_template_prompt_maps_options_onto_generate(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    curl = _record(
        monkeypatch,
        {"response": " Once ", "done_reason": "length", "prompt_eval_count": 3, "eval_count": 5},
    )
    result = OllamaAdapter().generate(
        model="smollm2:135m",
        prompt="Write a story.",
        timeout_seconds=9,
        options=GenerateOptions(
            temperature=0.0,
            max_tokens=5,
            stop=("END",),
            params={"num_ctx": 4096, "keep_alive": 0, "think": False, "format": "json"},
            deterministic=True,
            images=(PNG,),
        ),
    )
    assert curl.url == "http://localhost:11434/api/generate"
    assert curl.max_time == "9"
    assert curl.body == {
        "model": "smollm2:135m",
        "stream": False,
        "prompt": "Write a story.",
        "images": [PNG_B64],
        "keep_alive": 0,
        "think": False,
        "format": "json",
        "options": {
            "temperature": 0.0,
            "num_predict": 5,
            "stop": ["END"],
            "num_ctx": 4096,
            "seed": DETERMINISTIC_SEED,
        },
    }
    assert result.text == "Once"
    assert result.finish_reason == "length"
    assert validate_generate_result(result, adapter_name="ollama") == []


def test_ollama_explicit_seed_wins_over_deterministic(monkeypatch: pytest.MonkeyPatch) -> None:
    curl = _record(monkeypatch, {"response": "x"})
    OllamaAdapter().generate(
        model="m",
        prompt="p",
        options=GenerateOptions(temperature=0.0, params={"seed": 42}, deterministic=True),
    )
    assert curl.body["options"]["seed"] == 42


def test_ollama_messages_use_chat_with_images_on_last_user_turn(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    curl = _record(
        monkeypatch, {"message": {"role": "assistant", "content": "A cat."}, "done_reason": "stop"}
    )
    result = OllamaAdapter().generate(
        model="m", prompt="flattened", options=GenerateOptions(messages=TURNS, images=(PNG,))
    )
    assert curl.url == "http://localhost:11434/api/chat"
    assert curl.body["messages"] == [
        {"role": "system", "content": "Be terse."},
        {"role": "user", "content": "Hi"},
        {"role": "assistant", "content": "Hello."},
        {"role": "user", "content": "What is this?", "images": [PNG_B64]},
    ]
    assert "prompt" not in curl.body
    assert result.text == "A cat."
    assert result.finish_reason == "stop"


def test_ollama_without_options_sends_the_original_request(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    curl = _record(monkeypatch, {"response": "hi"})
    OllamaAdapter().generate(model="m", prompt="p")
    assert curl.url.endswith("/api/generate")
    assert curl.body == {"model": "m", "prompt": "p", "stream": False}


def test_ollama_refuses_an_image_url(monkeypatch: pytest.MonkeyPatch) -> None:
    _record(monkeypatch, {"response": "hi"})
    with pytest.raises(RuntimeError, match="cannot fetch an image URL"):
        OllamaAdapter().generate(
            model="m",
            prompt="p",
            options=GenerateOptions(images=(ImageInput(url="https://x.test/a.png"),)),
        )


# ---------- OpenAI-compatible family (groq stands in) and openai ----------


def _chat_response(text: str = "ok", finish_reason: str = "stop") -> dict[str, Any]:
    return {
        "choices": [{"message": {"content": text}, "finish_reason": finish_reason}],
        "usage": {"prompt_tokens": 4, "completion_tokens": 2},
    }


def test_openai_compat_maps_options_turns_and_images(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("GROQ_API_KEY", "k")
    curl = _record(monkeypatch, _chat_response("cut", "length"))
    result = GroqAdapter().generate(
        model="llama",
        prompt="flattened",
        options=GenerateOptions(
            temperature=0.3,
            max_tokens=50,
            stop=("\n\n",),
            params={"top_p": 0.9},
            deterministic=True,
            messages=(*TURNS, ChatMessage(role="tool", content="42")),
            images=(PNG, ImageInput(url="https://x.test/b.jpg")),
        ),
    )
    body = curl.body
    assert body["temperature"] == 0.3
    assert body["max_tokens"] == 50
    assert body["stop"] == ["\n\n"]
    assert body["top_p"] == 0.9
    # The family gets no automatic seed: not every provider in it takes one.
    assert "seed" not in body
    assert body["messages"] == [
        {"role": "system", "content": "Be terse."},
        {"role": "user", "content": "Hi"},
        {"role": "assistant", "content": "Hello."},
        {"role": "user", "content": "What is this?"},
        {
            "role": "user",
            "content": [
                {"type": "text", "text": "tool: 42"},
                {"type": "image_url", "image_url": {"url": f"data:image/png;base64,{PNG_B64}"}},
                {"type": "image_url", "image_url": {"url": "https://x.test/b.jpg"}},
            ],
        },
    ]
    assert result.finish_reason == "length"
    assert validate_generate_result(result, adapter_name="groq") == []


def test_openai_compat_without_options_sends_one_user_turn(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("GROQ_API_KEY", "k")
    curl = _record(monkeypatch, _chat_response())
    GroqAdapter().generate(model="llama", prompt="p")
    assert curl.body == {"model": "llama", "messages": [{"role": "user", "content": "p"}]}


def test_openai_sends_max_completion_tokens_and_a_deterministic_seed(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("OPENAI_API_KEY", "sk-test")
    curl = _record(monkeypatch, _chat_response("hi", "stop"))
    result = OpenAIAdapter().generate(
        model="gpt-4o-mini",
        prompt="p",
        timeout_seconds=3,
        options=GenerateOptions(temperature=0.0, max_tokens=8, deterministic=True, images=(PNG,)),
    )
    body = curl.body
    assert body["max_completion_tokens"] == 8
    assert "max_tokens" not in body
    assert body["temperature"] == 0.0
    assert body["seed"] == DETERMINISTIC_SEED
    assert body["messages"] == [
        {
            "role": "user",
            "content": [
                {"type": "text", "text": "p"},
                {"type": "image_url", "image_url": {"url": f"data:image/png;base64,{PNG_B64}"}},
            ],
        }
    ]
    assert curl.max_time == "3"
    assert result.finish_reason == "stop"


# ---------- anthropic ----------


def test_anthropic_system_field_turns_options_and_image_blocks(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("ANTHROPIC_API_KEY", "sk-ant-test")
    curl = _record(
        monkeypatch,
        {
            "content": [{"type": "text", "text": "A cat."}],
            "stop_reason": "max_tokens",
            "usage": {"input_tokens": 9, "output_tokens": 3},
        },
    )
    result = AnthropicAdapter().generate(
        model="claude-sonnet-5",
        prompt="flattened",
        options=GenerateOptions(
            temperature=0.0,
            max_tokens=3,
            stop=("###",),
            params={"top_k": 5},
            deterministic=True,
            messages=TURNS,
            images=(PNG, ImageInput(url="https://x.test/c.webp")),
        ),
    )
    assert curl.body == {
        "model": "claude-sonnet-5",
        "max_tokens": 3,
        "system": "Be terse.",
        "temperature": 0.0,
        "stop_sequences": ["###"],
        "top_k": 5,
        "messages": [
            {"role": "user", "content": "Hi"},
            {"role": "assistant", "content": "Hello."},
            {
                "role": "user",
                "content": [
                    {
                        "type": "image",
                        "source": {"type": "base64", "media_type": "image/png", "data": PNG_B64},
                    },
                    {"type": "image", "source": {"type": "url", "url": "https://x.test/c.webp"}},
                    {"type": "text", "text": "What is this?"},
                ],
            },
        ],
    }
    assert result.finish_reason == "max_tokens"
    assert validate_generate_result(result, adapter_name="anthropic") == []


def test_anthropic_without_options_keeps_the_configured_max_tokens(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("ANTHROPIC_API_KEY", "sk-ant-test")
    curl = _record(monkeypatch, {"content": [{"type": "text", "text": "hi"}]})
    AnthropicAdapter(max_tokens=777).generate(model="claude-sonnet-5", prompt="p")
    assert curl.body == {
        "model": "claude-sonnet-5",
        "max_tokens": 777,
        "messages": [{"role": "user", "content": "p"}],
    }


def test_anthropic_system_only_messages_go_out_as_a_user_turn(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setenv("ANTHROPIC_API_KEY", "sk-ant-test")
    curl = _record(monkeypatch, {"content": [{"type": "text", "text": "hi"}]})
    AnthropicAdapter(max_tokens=777).generate(
        model="claude-sonnet-5",
        prompt="system: Say hi.",
        options=GenerateOptions(messages=(ChatMessage(role="system", content="Say hi."),)),
    )
    assert curl.body == {
        "model": "claude-sonnet-5",
        "max_tokens": 777,
        "messages": [{"role": "user", "content": "Say hi."}],
    }


# ---------- request bodies travel on stdin ----------


@pytest.mark.parametrize(
    "adapter,env_var,response",
    [
        (OllamaAdapter(), None, {"response": "ok"}),
        (GroqAdapter(), "GROQ_API_KEY", _chat_response()),
        (OpenAIAdapter(), "OPENAI_API_KEY", _chat_response()),
        (AnthropicAdapter(), "ANTHROPIC_API_KEY", {"content": [{"type": "text", "text": "ok"}]}),
    ],
    ids=["ollama", "openai_compat", "openai", "anthropic"],
)
def test_a_large_image_goes_on_stdin_not_in_argv(
    monkeypatch: pytest.MonkeyPatch,
    adapter: Any,
    env_var: str | None,
    response: dict[str, Any],
) -> None:
    """A single argv element is capped at 128 KiB on Linux; a real image's
    base64 body must not be one."""
    if env_var:
        monkeypatch.setenv(env_var, "test-key")
    curl = _record(monkeypatch, response)
    image = ImageInput(data=b"\x89PNG" + b"\x00" * 300_000, media_type="image/png")
    adapter.generate(model="m", prompt="What is this?", options=GenerateOptions(images=(image,)))
    assert curl.stdin is not None and len(curl.stdin) > 400_000
    assert max(len(arg) for arg in curl.cmd) < 1_000
    assert curl.body["model"] == "m"


# ---------- adapters that accept options but cannot use them ----------


def test_host_claude_accepts_options_and_warns_once() -> None:
    adapter = HostClaudeAdapter(request_handler=lambda request: "done")
    result = call_generate(
        adapter,
        model="",
        prompt="p",
        timeout_seconds=5,
        options=GenerateOptions(temperature=0.0, images=(PNG,)),
    )
    assert result.text == "done"
    assert result.warnings == ("adapter 'host_claude' ignored: temperature; images",)


# ---------- image capability ----------


IMAGE_ADAPTERS = {
    "ollama", "openai", "anthropic", "gemini", "groq", "openrouter", "perplexity",
    "xai", "deepseek", "together", "fireworks", "nvidia-nim", "vllm", "llamacpp",
    "lmstudio", "mistral", "ai21", "huggingface-inference", "tgi", "databricks",
    "qwen-dashscope", "cohere", "cloudflare-workers-ai", "azure-openai",
}  # fmt: skip


@pytest.mark.parametrize("name", sorted(set(ADAPTER_REGISTRY) - {"host_claude"}))
def test_adapter_image_capability(name: str) -> None:
    adapter = build_adapter(adapter_name=name, runtime={})
    assert adapter_accepts_images(adapter) is (name in IMAGE_ADAPTERS)
