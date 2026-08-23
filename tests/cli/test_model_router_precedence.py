"""The full model precedence chain, with the router in it.

```
profile effect override  >  per-effect model:  >  --model  >  ROUTER
                         >  orchestration default  >  config default
```

The router's own rank is what this file's subject added; everything above it
was already true and is unchanged. Note the top of the chain against the order
issue #108 sketched: a per-effect ``model:`` beats ``--model``, not the other
way round. That is what ``resolved_model = self.defn.model or self.model`` has
always done — ``--model`` sets the *run default*, and an effect that names a
model has always opted out of the run default whatever supplied it. Reordering
those two is a change to non-routing behaviour, which the same issue rules out
("with routing disabled, model selection is byte-identical to today"), so it is
pinned here as-is rather than quietly altered. The acceptance criterion the
issue actually states about the router — that it never overrides any of the
three — holds either way, and is asserted below.

Six layers is too many to argue about one example at a time, so the core of
this file is a matrix: every combination of the four layers a caller controls
(CLI, per-effect, profile effect override, routing on/off), each asserted
against the model the adapter was *actually called with*. The expectation
column is written out per row rather than computed from the same precedence
function under test — a table that derives its own answers proves nothing.

Everything here goes through :func:`circuitry.cli.runtime_shim.run`, because
the layers only exist as separate things above the runtime: by the time
``PromptRuntime`` sees a model it is a string, and the question "who chose it"
has already been answered.
"""

from __future__ import annotations

import itertools
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import pytest
import yaml

from circuitry.adapters.base import GenerateResult
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.effective_settings import resolve_effective_settings
from circuitry.cli.runtime_shim import RunRequest, run

CONFIG_MODEL = "config-model"
ORCH_MODEL = "orch-model"
CLI_MODEL = "cli-model"
EFFECT_MODEL = "effect-model"
PROFILE_EFFECT_MODEL = "profile-effect-model"
ROUTED_MODEL = "routed-model"

#: One catch-all band, so the routed answer is the same for any score and the
#: matrix below is about precedence only. Band *selection* is
#: ``tests/core/test_router.py``'s subject.
ROUTING_BLOCK: dict[str, Any] = {
    "complexity": {
        "scoring": {"enabled": True},
        "routing": {
            "enabled": True,
            "bands": [{"name": "everything", "model": ROUTED_MODEL}],
        },
    }
}


@dataclass
class RecordingAdapter:
    name: str = "recording"
    calls: list[str] = field(default_factory=list)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.calls.append(model)
        return GenerateResult(text=f"ok:{model}", raw={})


def _write_orch(
    tmp_path: Path, *, effect_model: str | None, routing: bool
) -> Path:
    effect: dict[str, Any] = {
        "type": "prompt",
        "name": "task",
        "template": "analyze the corpus and cross-reference it",
    }
    if effect_model is not None:
        effect["model"] = effect_model

    orch: dict[str, Any] = {"model": ORCH_MODEL, "effects": [effect]}
    if routing:
        orch["runtime"] = ROUTING_BLOCK

    path = tmp_path / "orch.yml"
    path.write_text(yaml.safe_dump(orch, sort_keys=False), encoding="utf-8")
    return path


def _run_case(
    tmp_path: Path,
    *,
    cli: bool,
    effect: bool,
    profile: bool,
    routing: bool,
) -> tuple[RecordingAdapter, Any]:
    orch_path = _write_orch(
        tmp_path, effect_model=EFFECT_MODEL if effect else None, routing=routing
    )
    adapter = RecordingAdapter()
    result = run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state={},
            adapter=adapter,
            skip_preflight=True,
            config=CircuitryConfig(default_model=CONFIG_MODEL),
            model_override=CLI_MODEL if cli else None,
            profile_record=(
                {
                    "name": "overlay",
                    "content": {"effects": {"task": {"model": PROFILE_EFFECT_MODEL}}},
                }
                if profile
                else None
            ),
        )
    )
    assert result.ok, result.error
    return adapter, result


def _expected(*, cli: bool, effect: bool, profile: bool, routing: bool) -> str:
    """The precedence chain spelled out as prose, in order."""
    # A profile effect override is overlaid *onto* the effect definition, so
    # when both are present the profile is the one that wins — that is what an
    # override is. Both are "the effect names its own model", which has always
    # outranked the run default, `--model` included.
    if profile:
        return PROFILE_EFFECT_MODEL
    if effect:
        return EFFECT_MODEL
    if cli:
        return CLI_MODEL
    if routing:
        return ROUTED_MODEL
    return ORCH_MODEL


@pytest.mark.parametrize(
    ("cli", "effect", "profile", "routing"),
    list(itertools.product([False, True], repeat=4)),
)
def test_model_precedence_matrix(
    tmp_path: Path, cli: bool, effect: bool, profile: bool, routing: bool
) -> None:
    adapter, result = _run_case(
        tmp_path, cli=cli, effect=effect, profile=profile, routing=routing
    )
    expected = _expected(cli=cli, effect=effect, profile=profile, routing=routing)

    assert adapter.calls == [expected]
    node = result.state["prime"]["task"]["meta"]
    assert node["model"] == expected

    # `model_reason` has to agree with the model, or the explain-routing line
    # and every post-mortem read off this node lies about who decided.
    if expected == ROUTED_MODEL:
        assert node["model_reason"] == "router"
    elif expected in (EFFECT_MODEL, PROFILE_EFFECT_MODEL):
        assert node["model_reason"] == "explicit"
    else:
        assert node["model_reason"] == "default"


@pytest.mark.parametrize(
    ("layer", "expected", "reason"),
    [
        pytest.param("cli", CLI_MODEL, "default", id="--model"),
        pytest.param("effect", EFFECT_MODEL, "explicit", id="per-effect-model"),
        pytest.param(
            "profile",
            PROFILE_EFFECT_MODEL,
            "explicit",
            id="profile-effect-override",
        ),
    ],
)
def test_the_router_never_displaces_an_explicit_choice(
    tmp_path: Path, layer: str, expected: str, reason: str
) -> None:
    """Called out separately from the matrix because it is *the* criterion,
    not three of its rows — and because it asserts something the matrix does
    not: the band is still computed and recorded, so the run can say what
    routing would have picked while dispatching what the human asked for.
    """
    adapter, result = _run_case(
        tmp_path,
        cli=layer == "cli",
        effect=layer == "effect",
        profile=layer == "profile",
        routing=True,
    )

    assert adapter.calls == [expected]
    meta = result.state["prime"]["task"]["meta"]
    assert meta["model"] == expected
    assert meta["model_reason"] == reason
    assert meta["complexity"]["band"] == {
        "name": "everything",
        "model": ROUTED_MODEL,
    }


# --------------------------------------------------------------------------
# the source table
# --------------------------------------------------------------------------


def _effective(
    *,
    routing: dict[str, Any] | None,
    cli_model: str | None = None,
    orch_model: str | None = ORCH_MODEL,
    config_model: str | None = CONFIG_MODEL,
) -> Any:
    orch: dict[str, Any] = {}
    if orch_model is not None:
        orch["model"] = orch_model
    if routing is not None:
        orch["runtime"] = {
            "complexity": {"scoring": {"enabled": True}, "routing": routing}
        }
    return resolve_effective_settings(
        cfg=CircuitryConfig(default_model=config_model),
        orch=orch,
        cli_model=cli_model,
    )


def test_sources_model_is_router_when_the_router_decides() -> None:
    effective = _effective(
        routing={"enabled": True, "bands": [{"model": ROUTED_MODEL}]}
    )

    assert effective.sources["model"] == "router"
    assert effective.model_locked is False
    # The recorded value stays the run default: the router's real answer is
    # per effect, and this is what a non-prompt effect — or a prompt with no
    # usable score — still runs on.
    assert effective.model == ORCH_MODEL


@pytest.mark.parametrize(
    ("routing", "expected"),
    [
        pytest.param(None, "orchestration", id="no-block"),
        pytest.param(
            {"enabled": False, "bands": [{"model": ROUTED_MODEL}]},
            "orchestration",
            id="table-present-switch-off",
        ),
    ],
)
def test_sources_model_is_unchanged_when_routing_is_off(
    routing: dict[str, Any] | None, expected: str
) -> None:
    assert _effective(routing=routing).sources["model"] == expected


def test_the_cli_keeps_the_source_entry_it_already_had() -> None:
    effective = _effective(
        routing={"enabled": True, "bands": [{"model": ROUTED_MODEL}]},
        cli_model=CLI_MODEL,
    )

    assert effective.sources["model"] == "cli"
    assert effective.model == CLI_MODEL
    # The flag the runtime reads to know the difference between this and an
    # inherited default — the one thing `sources` can no longer say once the
    # router is allowed to win that entry.
    assert effective.model_locked is True


def test_respect_explicit_false_gives_the_router_the_source_entry() -> None:
    effective = _effective(
        routing={
            "enabled": True,
            "respect_explicit": False,
            "bands": [{"model": ROUTED_MODEL}],
        },
        cli_model=CLI_MODEL,
    )

    assert effective.sources["model"] == "router"


def test_a_band_table_is_a_sufficient_model_configuration(
    tmp_path: Path,
) -> None:
    """Routing on and no default model anywhere used to fail with "no model
    resolved". The catch-all band — whose whole job is "the model for anything
    not otherwise matched" — is exactly the run default that run was missing.
    """
    effective = _effective(
        routing={
            "enabled": True,
            "bands": [
                {"name": "low", "max": 40, "model": "small-model"},
                {"name": "high", "model": "big-model"},
            ],
        },
        orch_model=None,
        config_model=None,
    )

    assert effective.model == "big-model"
    assert effective.sources["model"] == "router"

    # And it really runs — the error this replaces was raised at adapter
    # resolution, not at settings resolution.
    orch_path = tmp_path / "orch.yml"
    orch_path.write_text(
        yaml.safe_dump(
            {
                "runtime": {
                    "complexity": {
                        "scoring": {"enabled": True},
                        "routing": {
                            "enabled": True,
                            "bands": [{"name": "all", "model": ROUTED_MODEL}],
                        },
                    }
                },
                "effects": [
                    {"type": "prompt", "name": "task", "template": "analyze it"}
                ],
            },
            sort_keys=False,
        ),
        encoding="utf-8",
    )
    adapter = RecordingAdapter()
    result = run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state={},
            adapter=adapter,
            skip_preflight=True,
            config=CircuitryConfig(),
        )
    )

    assert result.ok, result.error
    assert adapter.calls == [ROUTED_MODEL]


def test_a_malformed_band_table_fails_before_the_run_not_during_it(
    tmp_path: Path,
) -> None:
    """Same layer as an unknown weight key: config resolution, with a message
    naming the offending entry. Nothing dispatches."""
    orch_path = tmp_path / "orch.yml"
    orch_path.write_text(
        yaml.safe_dump(
            {
                "runtime": {
                    "complexity": {
                        "scoring": {"enabled": True},
                        "routing": {
                            "enabled": True,
                            "bands": [
                                {"max": 60, "model": "mid-model"},
                                {"max": 40, "model": "small-model"},
                                {"model": "big-model"},
                            ],
                        },
                    }
                },
                "effects": [
                    {"type": "prompt", "name": "task", "template": "analyze it"}
                ],
            },
            sort_keys=False,
        ),
        encoding="utf-8",
    )
    adapter = RecordingAdapter()
    result = run(
        RunRequest(
            orchestration_path=orch_path,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state={},
            adapter=adapter,
            skip_preflight=True,
            config=CircuitryConfig(default_model=CONFIG_MODEL),
        )
    )

    assert result.ok is False
    assert "ordered by ascending 'max'" in (result.error or "")
    assert "bands[1]" in (result.error or "")
    assert adapter.calls == []
    assert "prime" not in result.state
