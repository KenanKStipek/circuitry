"""`cof check` warns when a prompt's image assets go to an adapter that
cannot send images (issue #250)."""

from __future__ import annotations

from pathlib import Path

from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import image_asset_warnings, validate

IMAGE = [{"kind": "image", "ref": "shot.png"}]


def _prompt(name: str, **extra: object) -> dict[str, object]:
    return {"type": "prompt", "name": name, "template": "What is this?", **extra}


def test_warns_for_the_default_adapter_and_each_fallback_that_cannot_take_images() -> None:
    orch = {
        "effects": [
            {
                "type": "loop",
                "name": "pass",
                "max_iterations": 1,
                "body": [_prompt("look", assets=IMAGE, provider_fallbacks=["watsonx:granite"])],
            }
        ]
    }
    assert image_asset_warnings(orch, default_adapter="replicate") == [
        f"prompt 'look' has image assets, but adapter '{name}' cannot send images; "
        "it will run without them"
        for name in ("replicate", "watsonx")
    ]


def test_quiet_for_image_capable_adapters_and_prompts_without_images() -> None:
    orch = {
        "effects": [
            _prompt("look", assets=IMAGE, provider="ollama:llava"),
            _prompt("read", assets=[{"kind": "file", "ref": "notes.txt"}]),
            _prompt("plain"),
        ]
    }
    assert image_asset_warnings(orch, default_adapter="replicate") == []


def test_host_claude_cannot_take_images() -> None:
    orch = {"effects": [_prompt("look", assets=IMAGE)]}
    assert len(image_asset_warnings(orch, default_adapter="host_claude")) == 1


def test_cof_check_reports_it_using_the_config_default_adapter(tmp_path: Path) -> None:
    doc = tmp_path / "look.yml"
    doc.write_text(
        "effects:\n"
        "  - type: prompt\n"
        "    name: look\n"
        "    template: What is this?\n"
        "    assets: [{kind: image, ref: shot.png}]\n",
        encoding="utf-8",
    )
    result = validate(
        doc, config=CircuitryConfig(default_adapter="replicate"), skip_preflight=True
    )
    assert result["ok"] is True
    assert any(
        "adapter 'replicate' cannot send images" in w for w in result["warnings"]
    )

    result = validate(doc, config=CircuitryConfig(default_adapter="ollama"), skip_preflight=True)
    assert not any("cannot send images" in w for w in result["warnings"])
