"""Tests for cof setup command."""

from __future__ import annotations

import json
import re
import stat
from pathlib import Path

import pytest
from typer.testing import CliRunner

from circuitry.cli import setup as cli_setup
from circuitry.cli.app import app
from circuitry.cli.detect import BackendStatus, DetectionResult

_CANARY_KEY = "sk-CANARYvalueNeverShown1234567890"

runner = CliRunner()


def _mode(path: Path) -> int:
    return stat.S_IMODE(path.stat().st_mode)


def _extract_json(output: str) -> list:
    """Extract the JSON array from setup --json output (skips Rich panel header)."""
    # Find the first '[' which starts the JSON array
    match = re.search(r"\[", output)
    assert match, f"No JSON array found in output: {output[:200]}"
    return json.loads(output[match.start():])


def test_setup_json_runs_without_error() -> None:
    """--json mode is non-interactive and should always work."""
    result = runner.invoke(app, ["setup", "--json"])
    assert result.exit_code == 0
    data = _extract_json(result.output)
    assert isinstance(data, list)
    names = {entry["name"] for entry in data}
    assert "ollama" in names
    assert "ffmpeg" in names


def test_setup_json_is_valid_json_with_no_banner(tmp_path: Path) -> None:
    """`--json` must print parseable JSON and nothing else — the Rich banner
    used to print unconditionally before the `--json` branch checked the
    flag, so piping into a JSON parser failed on the banner text (#265 part 5)."""
    result = runner.invoke(app, ["setup", "--json"])
    assert result.exit_code == 0
    data = json.loads(result.output)
    assert isinstance(data, list)


def test_setup_json_has_expected_fields() -> None:
    result = runner.invoke(app, ["setup", "--json"])
    assert result.exit_code == 0
    data = _extract_json(result.output)
    for entry in data:
        assert "name" in entry
        assert "available" in entry
        assert isinstance(entry["available"], bool)
        assert "detail" in entry
        assert "models" in entry
        assert isinstance(entry["models"], list)


def test_setup_json_never_shows_key_characters() -> None:
    """#350: a canary key's characters never appear in --json output."""
    result = runner.invoke(
        app,
        ["setup", "--json"],
        env={"OPENAI_API_KEY": _CANARY_KEY, "ANTHROPIC_API_KEY": _CANARY_KEY},
    )
    assert result.exit_code == 0
    assert _CANARY_KEY not in result.output
    for i in range(len(_CANARY_KEY) - 6):
        assert _CANARY_KEY[i : i + 6] not in result.output
    data = _extract_json(result.output)
    openai_entry = next(e for e in data if e["name"] == "openai")
    anthropic_entry = next(e for e in data if e["name"] == "anthropic")
    assert openai_entry["detail"] == "API key set (OPENAI_API_KEY)"
    assert anthropic_entry["detail"] == "API key set (ANTHROPIC_API_KEY)"


# ---------------------------------------------------------------------------
# #264 part 4 — config.json / .env written private, directory created private
# ---------------------------------------------------------------------------


def test_write_config_creates_private_directory_and_file() -> None:
    config_path = cli_setup._write_config({"default_model": "m"})

    assert config_path == cli_setup.GLOBAL_CONFIG_PATH
    assert _mode(config_path) == 0o600
    assert _mode(config_path.parent) == 0o700
    assert json.loads(config_path.read_text()) == {"default_model": "m"}


def test_write_config_tightens_mode_of_existing_file() -> None:
    cli_setup.GLOBAL_CONFIG_DIR.mkdir(parents=True, exist_ok=True)
    cli_setup.GLOBAL_CONFIG_PATH.write_text("{}", encoding="utf-8")
    cli_setup.GLOBAL_CONFIG_PATH.chmod(0o644)

    cli_setup._write_config({"default_model": "m"})

    assert _mode(cli_setup.GLOBAL_CONFIG_PATH) == 0o600


def test_write_env_file_creates_private_directory_and_file(
    monkeypatch: object,
) -> None:
    from circuitry.cli.detect import BackendStatus, DetectionResult

    monkeypatch.setattr("typer.confirm", lambda *a, **k: True)  # type: ignore[union-attr]
    monkeypatch.setattr("typer.prompt", lambda *a, **k: "sk-test")  # type: ignore[union-attr]

    result = DetectionResult(backends=[BackendStatus(name="openai", available=False, detail="")])
    env_path = cli_setup._write_env_file(result)

    assert env_path is not None
    assert _mode(env_path) == 0o600
    assert _mode(env_path.parent) == 0o700
    assert "OPENAI_API_KEY=sk-test" in env_path.read_text()


def test_write_env_file_tightens_mode_of_existing_file(monkeypatch: object) -> None:
    from circuitry.cli.detect import BackendStatus, DetectionResult

    cli_setup.GLOBAL_CONFIG_DIR.mkdir(parents=True, exist_ok=True)
    env_path = cli_setup.GLOBAL_CONFIG_DIR / ".env"
    env_path.write_text("EXISTING=1\n", encoding="utf-8")
    env_path.chmod(0o644)

    monkeypatch.setattr("typer.confirm", lambda *a, **k: True)  # type: ignore[union-attr]
    monkeypatch.setattr("typer.prompt", lambda *a, **k: "sk-test")  # type: ignore[union-attr]

    result = DetectionResult(backends=[BackendStatus(name="openai", available=False, detail="")])
    cli_setup._write_env_file(result)

    assert _mode(env_path) == 0o600
    assert "EXISTING=1" in env_path.read_text()


# ---------------------------------------------------------------------------
# #266 part 2 — execution coverage for the wizard's detection/prompt/write flow
# ---------------------------------------------------------------------------


def test_detect_urls_from_existing_config_defaults_when_absent() -> None:
    assert not cli_setup.GLOBAL_CONFIG_PATH.exists()

    ollama_url, comfyui_url = cli_setup._detect_urls_from_existing_config()

    assert ollama_url == "http://localhost:11434"
    assert comfyui_url == "http://localhost:8188"


def test_detect_urls_from_existing_config_reads_custom_urls() -> None:
    cli_setup.GLOBAL_CONFIG_DIR.mkdir(parents=True, exist_ok=True)
    cli_setup.GLOBAL_CONFIG_PATH.write_text(
        json.dumps(
            {
                "runtime": {
                    "adapters": {"ollama": {"base_url": "http://ollama.local:1234"}},
                    "plugins": {"comfyui": {"base_url": "http://comfy.local:8188"}},
                }
            }
        ),
        encoding="utf-8",
    )

    ollama_url, comfyui_url = cli_setup._detect_urls_from_existing_config()

    assert ollama_url == "http://ollama.local:1234"
    assert comfyui_url == "http://comfy.local:8188"


def test_detect_urls_from_existing_config_falls_back_on_malformed_json() -> None:
    cli_setup.GLOBAL_CONFIG_DIR.mkdir(parents=True, exist_ok=True)
    cli_setup.GLOBAL_CONFIG_PATH.write_text("not json", encoding="utf-8")

    ollama_url, comfyui_url = cli_setup._detect_urls_from_existing_config()

    assert ollama_url == "http://localhost:11434"
    assert comfyui_url == "http://localhost:8188"


def test_print_detection_renders_backend_table(capsys: pytest.CaptureFixture[str]) -> None:
    result = DetectionResult(
        backends=[
            BackendStatus(name="ollama", available=True, detail="http://localhost:11434"),
            BackendStatus(name="openai", available=False, detail="OPENAI_API_KEY not set"),
        ]
    )

    cli_setup._print_detection(result)

    out = capsys.readouterr().out
    assert "ollama" in out
    assert "openai" in out
    assert "OPENAI_API_KEY not set" in out


def test_print_models_skips_when_no_backend_has_models(
    capsys: pytest.CaptureFixture[str],
) -> None:
    result = DetectionResult(
        backends=[BackendStatus(name="ollama", available=True, detail="", models=[])]
    )

    cli_setup._print_models(result)

    assert capsys.readouterr().out == ""


def test_print_models_lists_models_and_truncates_long_lists(
    capsys: pytest.CaptureFixture[str],
) -> None:
    models = [f"model-{i}" for i in range(12)]
    result = DetectionResult(
        backends=[BackendStatus(name="ollama", available=True, detail="", models=models)]
    )

    cli_setup._print_models(result)

    out = capsys.readouterr().out
    assert "model-0" in out
    assert "(+2 more)" in out


def test_print_capability_match_skips_when_no_curated_entries(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setattr(cli_setup, "load_index", list)

    cli_setup._print_capability_match(DetectionResult(backends=[]))

    assert capsys.readouterr().out == ""


def test_print_capability_match_maps_available_llm_backend(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setattr(
        cli_setup,
        "load_index",
        lambda: [
            {"name": "hello", "category": "example", "backends": ["llm"]},
            {"name": "video", "category": "example", "backends": ["comfyui"]},
            {"name": "a_template", "category": "template", "backends": ["llm"]},
        ],
    )
    result = DetectionResult(
        backends=[BackendStatus(name="ollama", available=True, detail="")]
    )

    cli_setup._print_capability_match(result)

    out = capsys.readouterr().out
    assert "hello" in out
    assert "video" in out
    assert "a_template" not in out


def test_pick_adapter_and_model_prompts_when_no_llm_backend(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr("typer.prompt", lambda label, default=None: default)
    result = DetectionResult(backends=[])

    adapter, model = cli_setup._pick_adapter_and_model(result)

    assert adapter == "ollama"
    assert model == "llama3:latest"


def test_pick_adapter_and_model_single_backend_prompts_for_model_only(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    prompts: list[tuple[str, object]] = []

    def fake_prompt(label, default=None):
        prompts.append((label, default))
        return default

    monkeypatch.setattr("typer.prompt", fake_prompt)
    result = DetectionResult(
        backends=[
            BackendStatus(
                name="ollama", available=True, detail="", models=["llama3:8b"]
            )
        ]
    )

    adapter, model = cli_setup._pick_adapter_and_model(result)

    assert adapter == "ollama"
    assert model == "llama3:8b"
    assert prompts == [("Model", "llama3:8b")]


def test_pick_adapter_and_model_multiple_backends_prompts_for_backend(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    def fake_prompt(label, default=None):
        return default

    monkeypatch.setattr("typer.prompt", fake_prompt)
    result = DetectionResult(
        backends=[
            BackendStatus(name="ollama", available=True, detail="", models=["llama3"]),
            BackendStatus(name="openai", available=True, detail="", models=[]),
        ]
    )

    adapter, model = cli_setup._pick_adapter_and_model(result)

    assert adapter == "ollama"
    assert model == "llama3"


def test_build_config_uses_detected_ollama_url() -> None:
    result = DetectionResult(
        backends=[BackendStatus(name="ollama", available=True, detail="http://detected:11434")]
    )

    config = cli_setup._build_config("ollama", "llama3", result)

    assert config["default_adapter"] == "ollama"
    assert config["default_model"] == "llama3"
    assert config["runtime"]["adapters"]["ollama"]["base_url"] == "http://detected:11434"


def test_build_config_prompts_for_ollama_url_when_not_detected(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr("typer.prompt", lambda label, default=None: "http://manual:11434")
    result = DetectionResult(backends=[])

    config = cli_setup._build_config("ollama", "llama3", result)

    assert config["runtime"]["adapters"]["ollama"]["base_url"] == "http://manual:11434"


def test_build_config_includes_comfyui_plugin_when_available() -> None:
    result = DetectionResult(
        backends=[
            BackendStatus(name="ollama", available=True, detail="http://localhost:11434"),
            BackendStatus(name="comfyui", available=True, detail="http://localhost:8188"),
        ]
    )

    config = cli_setup._build_config("ollama", "llama3", result)

    assert config["runtime"]["plugins"]["comfyui"]["base_url"] == "http://localhost:8188"


def test_build_config_omits_comfyui_plugin_when_unavailable() -> None:
    result = DetectionResult(backends=[])

    config = cli_setup._build_config("openai", "gpt-4o-mini", result)

    assert "plugins" not in config["runtime"]


def test_setup_cmd_writes_config_end_to_end(monkeypatch: pytest.MonkeyPatch) -> None:
    """Full interactive wizard: detect -> pick adapter/model -> write config."""
    fake = DetectionResult(
        backends=[
            BackendStatus(
                name="ollama",
                available=True,
                detail="http://localhost:11434",
                models=["llama3:latest"],
            ),
            BackendStatus(name="openai", available=False, detail="OPENAI_API_KEY not set"),
            BackendStatus(name="anthropic", available=False, detail="ANTHROPIC_API_KEY not set"),
        ]
    )
    monkeypatch.setattr(cli_setup, "detect_all", lambda **_: fake)
    monkeypatch.setattr("typer.prompt", lambda label, default=None: default)
    monkeypatch.setattr("typer.confirm", lambda *a, **k: False)  # decline .env setup

    result = runner.invoke(app, ["setup"])

    assert result.exit_code == 0, result.output
    assert cli_setup.GLOBAL_CONFIG_PATH.exists()
    written = json.loads(cli_setup.GLOBAL_CONFIG_PATH.read_text())
    assert written["default_adapter"] == "ollama"
    assert written["default_model"] == "llama3:latest"


def test_setup_cmd_keeps_existing_config_when_overwrite_declined(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    cli_setup.GLOBAL_CONFIG_DIR.mkdir(parents=True, exist_ok=True)
    cli_setup.GLOBAL_CONFIG_PATH.write_text(
        json.dumps({"default_adapter": "keep-me"}), encoding="utf-8"
    )
    fake = DetectionResult(backends=[])
    monkeypatch.setattr(cli_setup, "detect_all", lambda **_: fake)
    monkeypatch.setattr("typer.confirm", lambda *a, **k: False)  # decline overwrite

    result = runner.invoke(app, ["setup"])

    assert result.exit_code == 0, result.output
    assert "Keeping existing config" in result.output
    written = json.loads(cli_setup.GLOBAL_CONFIG_PATH.read_text())
    assert written == {"default_adapter": "keep-me"}


def test_setup_cmd_overwrites_existing_config_when_confirmed(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    cli_setup.GLOBAL_CONFIG_DIR.mkdir(parents=True, exist_ok=True)
    cli_setup.GLOBAL_CONFIG_PATH.write_text(
        json.dumps({"default_adapter": "old"}), encoding="utf-8"
    )
    fake = DetectionResult(backends=[])
    monkeypatch.setattr(cli_setup, "detect_all", lambda **_: fake)
    monkeypatch.setattr("typer.confirm", lambda *a, **k: True)  # overwrite, decline .env
    monkeypatch.setattr("typer.prompt", lambda label, default=None: default)

    result = runner.invoke(app, ["setup"])

    assert result.exit_code == 0, result.output
    written = json.loads(cli_setup.GLOBAL_CONFIG_PATH.read_text())
    assert written["default_adapter"] == "ollama"


def test_setup_cmd_writes_env_file_when_confirmed(monkeypatch: pytest.MonkeyPatch) -> None:
    fake = DetectionResult(backends=[])
    monkeypatch.setattr(cli_setup, "detect_all", lambda **_: fake)
    monkeypatch.setattr("typer.prompt", lambda label, default=None: default or "sk-test")
    monkeypatch.setattr("typer.confirm", lambda *a, **k: True)  # confirm every prompt

    result = runner.invoke(app, ["setup"])

    assert result.exit_code == 0, result.output
    env_path = cli_setup.GLOBAL_CONFIG_DIR / ".env"
    assert env_path.exists()
    assert "OPENAI_API_KEY=sk-test" in env_path.read_text()
    assert "ANTHROPIC_API_KEY=sk-test" in env_path.read_text()


# ---------------------------------------------------------------------------
# #346 — `cof setup` prints the installed example files' location
# ---------------------------------------------------------------------------


def test_setup_cmd_prints_examples_dir_after_writing_private_files(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    fake = DetectionResult(backends=[])
    monkeypatch.setattr(cli_setup, "detect_all", lambda **_: fake)
    monkeypatch.setattr("typer.prompt", lambda label, default=None: default or "sk-test")
    monkeypatch.setattr("typer.confirm", lambda *a, **k: True)

    result = runner.invoke(app, ["setup"])

    assert result.exit_code == 0, result.output
    # Rich wraps the path across lines at test-runner width; collapse all
    # whitespace (including the wrap-inserted newlines) out of both sides.
    collapsed_output = "".join(result.output.split())
    assert "".join(str(cli_setup.examples_dir()).split()) in collapsed_output
