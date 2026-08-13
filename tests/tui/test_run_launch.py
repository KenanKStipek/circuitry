"""Launch logic: discovery, the typed form, overrides, and the run session.

Everything here is Textual-free — these are the rules a run obeys, tested
without booting an app. Widget behaviour lives in ``test_run_view.py``.
"""

from __future__ import annotations

import json
import threading
from dataclasses import dataclass
from pathlib import Path
from typing import Any

import pytest
import yaml

from circuitry.adapters.base import GenerateResult
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, RunResult
from circuitry.cli.runtime_shim import run as shim_run
from circuitry.tui.launch import (
    CANCEL_MESSAGE,
    InputError,
    InputField,
    OrchestrationChoice,
    RunSession,
    adapter_models,
    adapter_options,
    build_initial_state,
    coerce_input,
    default_text,
    discover_orchestrations,
    input_fields,
    load_form,
    model_options,
    placeholder_for,
)

TWO_INPUT_ORCHESTRATION: dict[str, Any] = {
    "adapter": "echo",
    "model": "echo-1",
    "interface": {
        "inputs": {
            "text": {
                "type": "string",
                "required": True,
                "description": "The text to compress.",
            },
            "max_words": {
                "type": "number",
                "required": False,
                "default": 20,
                "description": "Soft word cap.",
            },
        }
    },
    "effects": [
        {"type": "prompt", "name": "summarize", "template": "{{text}} / {{max_words}}"}
    ],
}


@dataclass(frozen=True)
class EchoAdapter:
    """Deterministic stand-in for a real backend: the prompt comes back."""

    name: str = "echo"

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        return GenerateResult(text=prompt, raw={"model": model})


@pytest.fixture
def orchestration(tmp_path: Path) -> Path:
    path = tmp_path / "summarize.yml"
    path.write_text(_dump(TWO_INPUT_ORCHESTRATION), encoding="utf-8")
    return path


def _dump(orch: dict[str, Any]) -> str:
    """Serialize preserving key order — declaration order is form order."""
    return str(yaml.dump(orch, sort_keys=False))


def _choice(path: Path) -> OrchestrationChoice:
    return OrchestrationChoice(
        key=str(path), label=path.name, path=path, source="local"
    )


# -- the generated form ------------------------------------------------------


def test_interface_inputs_become_fields_in_declaration_order() -> None:
    fields = input_fields(TWO_INPUT_ORCHESTRATION)
    assert [f.name for f in fields] == ["text", "max_words"]
    assert (fields[0].type, fields[0].required) == ("string", True)
    assert (fields[1].type, fields[1].required) == ("number", False)
    assert fields[0].description == "The text to compress."


@pytest.mark.parametrize(
    "orch",
    [{}, {"interface": None}, {"interface": {}}, {"interface": {"inputs": []}}],
)
def test_orchestrations_without_declared_inputs_generate_no_fields(
    orch: dict[str, Any],
) -> None:
    assert input_fields(orch) == []


def test_unknown_types_fall_back_to_string() -> None:
    fields = input_fields({"interface": {"inputs": {"x": {"type": "widget"}}}})
    assert fields[0].type == "string"


def test_load_form_reads_the_file_and_its_declared_backend(orchestration: Path) -> None:
    form = load_form(_choice(orchestration))
    assert [f.name for f in form.fields] == ["text", "max_words"]
    assert (form.adapter, form.model) == ("echo", "echo-1")


def test_required_marker_and_hints_reach_the_widgets() -> None:
    required, optional = input_fields(TWO_INPUT_ORCHESTRATION)
    assert required.label == "text * (string)"
    assert optional.label == "max_words (number)"
    assert placeholder_for(required) == "The text to compress."
    assert placeholder_for(InputField("n", "array")) == "JSON array, e.g. [1, 2]"


def test_defaults_are_rendered_into_the_form() -> None:
    _, optional = input_fields(TWO_INPUT_ORCHESTRATION)
    assert default_text(optional) == "20"
    assert default_text(InputField("flag", "boolean", default=False)) == "false"
    assert default_text(InputField("tags", "array", default=["a"])) == '["a"]'
    assert default_text(InputField("text")) == ""


# -- type validation ---------------------------------------------------------


@pytest.mark.parametrize(
    ("type_name", "raw", "expected"),
    [
        ("string", "  hello  ", "hello"),
        ("number", "20", 20),
        ("number", "1.5", 1.5),
        ("number", "-3", -3),
        ("boolean", "true", True),
        ("boolean", "NO", False),
        ("boolean", "1", True),
        ("array", "[1, 2]", [1, 2]),
        ("object", '{"k": "v"}', {"k": "v"}),
    ],
)
def test_values_are_coerced_to_their_declared_type(
    type_name: str, raw: str, expected: Any
) -> None:
    assert coerce_input(InputField("x", type_name), raw) == expected


@pytest.mark.parametrize(
    ("type_name", "raw"),
    [
        ("number", "twenty"),
        ("number", ""),
        ("boolean", "maybe"),
        ("array", "not json"),
        ("array", '{"k": "v"}'),  # a JSON object is not an array
        ("object", "[1]"),
    ],
)
def test_bad_values_raise_with_the_field_name(type_name: str, raw: str) -> None:
    with pytest.raises(InputError, match="x:"):
        coerce_input(InputField("x", type_name), raw)


def test_required_inputs_are_enforced_before_launch() -> None:
    fields = input_fields(TWO_INPUT_ORCHESTRATION)
    state, errors = build_initial_state(fields, {"text": "   ", "max_words": "5"})
    assert errors == {"text": "text is required"}
    assert "text" not in state


def test_defaults_are_honored_when_an_optional_field_is_left_blank() -> None:
    fields = input_fields(TWO_INPUT_ORCHESTRATION)
    state, errors = build_initial_state(fields, {"text": "hi", "max_words": ""})
    assert errors == {}
    assert state == {"text": "hi", "max_words": 20}


def test_blank_optional_without_a_default_is_left_out_entirely() -> None:
    state, errors = build_initial_state([InputField("note")], {"note": ""})
    assert (state, errors) == ({}, {})


def test_type_errors_are_reported_per_field() -> None:
    fields = input_fields(TWO_INPUT_ORCHESTRATION)
    state, errors = build_initial_state(fields, {"text": "hi", "max_words": "lots"})
    assert set(errors) == {"max_words"}
    assert state == {"text": "hi"}


def test_defaults_are_copied_not_shared() -> None:
    field = InputField("tags", "array", default=["a"])
    state, _ = build_initial_state([field], {"tags": ""})
    state["tags"].append("b")
    assert field.default == ["a"]


# -- discovery ---------------------------------------------------------------


def test_local_files_are_discovered_and_non_orchestrations_ignored(
    tmp_path: Path,
) -> None:
    (tmp_path / "good.yml").write_text(_dump(TWO_INPUT_ORCHESTRATION), encoding="utf-8")
    (tmp_path / "config.json").write_text(json.dumps({"default_model": "x"}), "utf-8")
    (tmp_path / "broken.yml").write_text("::: not yaml :::", encoding="utf-8")
    (tmp_path / "notes.txt").write_text("hello", encoding="utf-8")

    local = [c for c in discover_orchestrations(tmp_path) if c.source == "local"]
    assert [c.label for c in local] == ["good.yml"]


def test_nested_orchestrations_directory_is_scanned(tmp_path: Path) -> None:
    nested = tmp_path / "orchestrations"
    nested.mkdir()
    (nested / "child.yml").write_text(_dump(TWO_INPUT_ORCHESTRATION), encoding="utf-8")
    local = [c for c in discover_orchestrations(tmp_path) if c.source == "local"]
    assert [c.label for c in local] == [str(Path("orchestrations") / "child.yml")]


def test_bundled_orchestrations_are_offered_and_resolvable(tmp_path: Path) -> None:
    bundled = [c for c in discover_orchestrations(tmp_path) if c.source == "bundled"]
    assert bundled, "the curation library should supply picker entries"
    assert all(c.path.exists() for c in bundled)
    assert any(c.key.endswith("summarize") for c in bundled)


def test_picker_labels_carry_source_and_description(tmp_path: Path) -> None:
    choice = OrchestrationChoice("k", "utilities/summarize", tmp_path, "bundled", "Sums")
    assert choice.option == "[bundled] utilities/summarize — Sums"


# -- adapter / model options -------------------------------------------------


def test_adapter_options_come_from_configured_adapters() -> None:
    cfg = CircuitryConfig(
        default_adapter="ollama",
        runtime={"adapters": {"openai": {}, "anthropic": {}}},
    )
    assert adapter_options(cfg) == ["anthropic", "ollama", "openai"]


def test_adapter_options_respect_the_allowlist() -> None:
    cfg = CircuitryConfig(
        runtime={"adapters": {"openai": {}, "anthropic": {}}},
        enabled_adapters=["openai"],
    )
    assert adapter_options(cfg) == ["openai"]


def test_adapter_options_never_offer_a_runtime_injected_adapter() -> None:
    cfg = CircuitryConfig(runtime={"adapters": {"host_claude": {}, "openai": {}}})
    assert adapter_options(cfg) == ["openai"]


def test_adapter_options_fall_back_to_the_registry_when_unconfigured() -> None:
    options = adapter_options(CircuitryConfig())
    assert "ollama" in options and "host_claude" not in options


def test_model_options_gather_config_and_orchestration_models() -> None:
    cfg = CircuitryConfig(
        default_model="llama3.1:8b",
        runtime={"adapters": {"openai": {"default_model": "gpt-4o-mini"}}},
    )
    assert model_options(cfg, TWO_INPUT_ORCHESTRATION) == [
        "echo-1",
        "gpt-4o-mini",
        "llama3.1:8b",
    ]


def test_model_options_fold_in_what_an_adapter_reported() -> None:
    """Adapter-reported models join the config-derived ones, sorted, deduped."""
    cfg = CircuitryConfig(default_model="llama3.1:8b")
    assert model_options(cfg, None, extra=["phi3:mini", " llama3.1:8b ", ""]) == [
        "llama3.1:8b",
        "phi3:mini",
    ]


def test_adapter_models_asks_the_named_adapter(monkeypatch: pytest.MonkeyPatch) -> None:
    seen: list[dict[str, Any]] = []

    def fake(*, adapter_name: str, runtime: dict[str, Any]) -> list[str]:
        seen.append({"adapter": adapter_name, "runtime": runtime})
        return ["phi3:mini"]

    monkeypatch.setattr("circuitry.tui.launch.list_adapter_models", fake)
    cfg = CircuitryConfig(runtime={"adapters": {"ollama": {"base_url": "http://x"}}})

    assert adapter_models(cfg, "Ollama") == ["phi3:mini"]
    assert seen[0]["adapter"] == "ollama"
    assert seen[0]["runtime"] == cfg.runtime


def test_adapter_models_skips_the_sentinel_and_unbuildable_adapters(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    def explode(*, adapter_name: str, runtime: dict[str, Any]) -> list[str]:
        raise AssertionError("should not be asked")

    monkeypatch.setattr("circuitry.tui.launch.list_adapter_models", explode)
    cfg = CircuitryConfig()
    assert adapter_models(cfg, None) == []
    assert adapter_models(cfg, "") == []
    assert adapter_models(cfg, "host_claude") == []


def test_overrides_outrank_the_orchestrations_own_settings(orchestration: Path) -> None:
    """The dropdowns are a real override, not a suggestion."""
    result = shim_run(
        RunRequest(
            orchestration_path=orchestration,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state={"text": "hi", "max_words": 5},
            config=CircuitryConfig(),
            adapter=EchoAdapter(),
            model_override="override-model",
        )
    )
    assert result.ok, result.error
    settings = result.state["runtime"]["effective_settings"]
    assert (settings["model"], settings["sources"]["model"]) == ("override-model", "cli")


# -- the run session ---------------------------------------------------------


def _request(path: Path, **kwargs: Any) -> RunRequest:
    return RunRequest(
        orchestration_path=path,
        state_path=None,
        out_path=None,
        dry_run=False,
        validate_only=False,
        initial_state={"text": "hello", "max_words": 5},
        config=CircuitryConfig(),
        adapter=EchoAdapter(),
        **kwargs,
    )


#: Run identity and wall-clock stamps differ between any two runs.
VOLATILE = frozenset(
    {"_run_id", "_timestamp", "run_id", "created_at", "completed_at", "started_at"}
)


def _stable(value: Any) -> Any:
    """Strip the fields that legitimately differ between two runs."""
    if isinstance(value, dict):
        return {k: _stable(v) for k, v in value.items() if k not in VOLATILE}
    if isinstance(value, list):
        return [_stable(item) for item in value]
    return value


def test_the_run_happens_off_the_calling_thread(orchestration: Path) -> None:
    seen: list[int] = []

    def runner(request: RunRequest) -> RunResult:
        seen.append(threading.get_ident())
        return RunResult(ok=True, state={}, warnings=[])

    session = RunSession(_request(orchestration), runner=runner)
    session.start()
    session.join(timeout=10)
    assert seen and seen[0] != threading.get_ident()


def test_observed_state_is_reported_and_the_result_handed_back(
    orchestration: Path,
) -> None:
    snapshots: list[dict[str, Any]] = []
    finished: list[RunResult] = []

    session = RunSession(
        _request(orchestration), on_state=snapshots.append, on_finish=finished.append
    )
    session.start()
    session.join(timeout=30)

    assert len(finished) == 1
    assert finished[0].ok, finished[0].error
    assert snapshots, "the observer should see at least the opening snapshot"
    assert finished[0].state["prime"]["summarize"]["value"] == "hello / 5"


def test_the_observer_is_pure(orchestration: Path) -> None:
    """A TUI-driven run must end where a plain ``cof run`` ends.

    The observer only ever reads, so attaching it cannot change the run —
    same orchestration, same inputs, same fake adapter, same final state.
    """
    snapshots: list[dict[str, Any]] = []
    session = RunSession(_request(orchestration), on_state=snapshots.append)
    session.start()
    session.join(timeout=30)
    observed = session.result
    assert observed is not None and observed.ok, observed

    plain = shim_run(_request(orchestration))
    assert plain.ok, plain.error
    assert _stable(observed.state) == _stable(plain.state)

    # Mutating what the observer handed us must not reach the run's state.
    snapshots[-1]["prime"] = "tampered"
    assert observed.state["prime"] != "tampered"


def test_cancelling_unwinds_the_run_into_a_failed_result(orchestration: Path) -> None:
    finished: list[RunResult] = []
    started = threading.Event()

    def on_state(_state: dict[str, Any]) -> None:
        started.set()

    session = RunSession(
        _request(orchestration), on_state=on_state, on_finish=finished.append
    )
    session.cancel()  # cancel before the first write: the earliest possible ask
    session.start()
    session.join(timeout=30)

    assert not session.running
    assert len(finished) == 1
    assert finished[0].ok is False
    assert CANCEL_MESSAGE in str(finished[0].error)
    assert session.cancelled


def test_a_worker_that_explodes_still_reports_a_result(orchestration: Path) -> None:
    def runner(request: RunRequest) -> RunResult:
        raise MemoryError("boom")

    finished: list[RunResult] = []
    session = RunSession(
        _request(orchestration), on_finish=finished.append, runner=runner
    )
    session.start()
    session.join(timeout=10)
    assert finished[0].ok is False
    assert "boom" in str(finished[0].error)
