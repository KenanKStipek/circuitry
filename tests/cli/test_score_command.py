"""``cof score`` — the static per-effect complexity preview."""

from __future__ import annotations

import json
from pathlib import Path

import pytest

pytest.importorskip("typer")
from typer.testing import CliRunner

from circuitry.cli.app import app
from circuitry.cli.complexity_config import SCORER_SIGNAL_NAMES, ScoringSettings
from circuitry.cli.score import ESTIMATE_NOTICE
from circuitry.core.complexity import SIGNAL_NAMES

runner = CliRunner()


def _write(tmp_path: Path, name: str, content: str) -> Path:
    path = tmp_path / name
    path.write_text(content, encoding="utf-8")
    return path


# An orchestration exercising every shape the walker has to address: a
# top-level prompt, a loop body, both conditional branches, a reflector's
# authored inner prompt plus its runtime-generated effects, a tool and a `use`.
FULL_ORCH = """\
runtime:
  complexity:
    scoring:
      enabled: true
effects:
  - type: prompt
    name: intro
    template: "Summarize {{topic}} for {{audience}}."
  - type: loop
    name: refine
    while:
      mode: cel
      expr: "false"
    body:
      - type: prompt
        name: critique
        template: "Critique {{draft}} and explain why each claim holds."
  - type: conditional
    name: decide
    if:
      mode: cel
      expr: "true"
    then:
      - type: prompt
        name: approve
        template: "Approve {{draft}}."
    else:
      - type: prompt
        name: reject
        template: "Reject {{draft}}."
  - type: reflector
    name: planner
    effects:
      - type: prompt
        name: propose_steps
        template: "Plan the steps needed for {{goal}}."
  - type: tool
    name: fetch
    provider: http
    params:
      url: "https://example.invalid"
  - type: use
    name: helper
    path: ./child.yml
"""

SIMPLE_ORCH = """\
runtime:
  complexity:
    scoring:
      enabled: true
effects:
  - type: prompt
    name: greet
    template: "Hello {{name}}"
"""

DISABLED_ORCH = """\
effects:
  - type: prompt
    name: greet
    template: "Hello {{name}}"
"""

BANDED_ORCH = """\
runtime:
  complexity:
    scoring:
      enabled: true
    routing:
      enabled: true
      bands:
        - name: small
          max: 20
          model: tiny-model
        - name: large
          model: big-model
effects:
  - type: prompt
    name: greet
    template: "Hello {{name}}"
"""


def _empty_config(tmp_path: Path) -> Path:
    """A config that defines nothing, so the orchestration decides.

    Passed explicitly on every invocation: without ``--config`` the loader
    would discover whatever config the machine running the tests happens to
    have, and the assertions here are about the orchestration's settings.
    """
    return _write(tmp_path, "config.json", json.dumps({}))


def _score_json(orch_path: Path, config: Path, *args: str) -> dict:
    result = runner.invoke(
        app, ["score", str(orch_path), "--config", str(config), "--json", *args]
    )
    assert result.exit_code == 0, result.stdout
    return json.loads(result.stdout)


# ---------------------------------------------------------------------------
# table output
# ---------------------------------------------------------------------------


def test_table_lists_every_effect_with_scores(tmp_path: Path) -> None:
    orch = _write(tmp_path, "full.yml", FULL_ORCH)
    result = runner.invoke(
        app, ["score", str(orch), "--config", str(_empty_config(tmp_path))]
    )

    assert result.exit_code == 0, result.stdout
    assert "intro" in result.stdout
    assert "refine.critique" in result.stdout
    assert "decide.approve" in result.stdout
    assert "decide.reject" in result.stdout


def test_table_states_scores_are_estimates(tmp_path: Path) -> None:
    orch = _write(tmp_path, "simple.yml", SIMPLE_ORCH)
    result = runner.invoke(
        app, ["score", str(orch), "--config", str(_empty_config(tmp_path))]
    )

    assert result.exit_code == 0, result.stdout
    # Rich wraps the footer, so match on a phrase that survives one line break
    # rather than the whole sentence.
    assert "estimates" in result.stdout
    assert "template" in result.stdout


def test_table_names_dominant_signals(tmp_path: Path) -> None:
    orch = _write(tmp_path, "simple.yml", SIMPLE_ORCH)
    result = runner.invoke(
        app, ["score", str(orch), "--config", str(_empty_config(tmp_path))]
    )

    assert result.exit_code == 0, result.stdout
    assert "output_type" in result.stdout or "state_references" in result.stdout


def test_band_column_only_appears_with_a_band_table(tmp_path: Path) -> None:
    config = _empty_config(tmp_path)
    plain = _write(tmp_path, "simple.yml", SIMPLE_ORCH)
    banded = _write(tmp_path, "banded.yml", BANDED_ORCH)

    plain_result = runner.invoke(app, ["score", str(plain), "--config", str(config)])
    banded_result = runner.invoke(app, ["score", str(banded), "--config", str(config)])

    assert "Band" not in plain_result.stdout
    assert "Band" in banded_result.stdout
    assert "small" in banded_result.stdout


# ---------------------------------------------------------------------------
# --json
# ---------------------------------------------------------------------------


def test_json_shape(tmp_path: Path) -> None:
    orch = _write(tmp_path, "simple.yml", SIMPLE_ORCH)
    payload = _score_json(orch, _empty_config(tmp_path))

    assert payload["mode"] == "static"
    assert payload["estimated"] is True
    assert payload["notice"] == ESTIMATE_NOTICE
    assert payload["max_score"] == 100.0
    assert payload["orchestration"] == str(orch)
    assert payload["profile"] is None

    (row,) = payload["effects"]
    assert row["path"] == "greet"
    assert row["type"] == "prompt"
    assert row["scoreable"] is True
    assert row["reason"] is None
    assert 0.0 <= row["score"] <= 100.0
    assert row["dominant_signals"]

    assert payload["summary"] == {
        "effects": 1,
        "scored": 1,
        "unscoreable": 0,
        "highest": {"path": "greet", "score": row["score"]},
    }


def test_json_includes_the_full_breakdown(tmp_path: Path) -> None:
    orch = _write(tmp_path, "simple.yml", SIMPLE_ORCH)
    payload = _score_json(orch, _empty_config(tmp_path))

    breakdown = payload["effects"][0]["breakdown"]
    assert breakdown["mode"] == "static"
    assert breakdown["estimated"] is True
    assert [signal["name"] for signal in breakdown["signals"]] == list(SIGNAL_NAMES)
    for signal in breakdown["signals"]:
        assert {"raw", "normalized", "weight", "contribution", "detail", "note"} <= set(
            signal
        )

    # The contributions reconstruct the total — that is the scorer's contract,
    # and the preview must not launder it away.
    total = sum(signal["contribution"] for signal in breakdown["signals"])
    assert total == pytest.approx(payload["effects"][0]["score"], abs=1e-6)


def test_json_round_trips_through_a_file(tmp_path: Path) -> None:
    orch = _write(tmp_path, "full.yml", FULL_ORCH)
    payload = _score_json(orch, _empty_config(tmp_path))

    dumped = tmp_path / "scores.json"
    dumped.write_text(json.dumps(payload), encoding="utf-8")
    assert json.loads(dumped.read_text(encoding="utf-8")) == payload


def test_json_reports_the_configured_weights_under_scorer_names(
    tmp_path: Path,
) -> None:
    orch = _write(tmp_path, "simple.yml", SIMPLE_ORCH)
    payload = _score_json(orch, _empty_config(tmp_path))

    assert set(payload["weights"]) == set(SIGNAL_NAMES)


# ---------------------------------------------------------------------------
# nesting and dotted paths
# ---------------------------------------------------------------------------


def _paths(payload: dict) -> list[str]:
    return [row["path"] for row in payload["effects"]]


def test_nested_effects_get_dotted_paths(tmp_path: Path) -> None:
    orch = _write(tmp_path, "full.yml", FULL_ORCH)
    payload = _score_json(orch, _empty_config(tmp_path))

    assert _paths(payload) == [
        "intro",
        "refine.critique",
        "decide.approve",
        "decide.reject",
        "planner.propose_steps",
        "planner.generated",
        "fetch",
        "helper",
    ]


def test_anonymous_containers_contribute_no_path_segment(tmp_path: Path) -> None:
    orch = _write(
        tmp_path,
        "anon.yml",
        """\
runtime:
  complexity:
    scoring:
      enabled: true
effects:
  - type: conditional
    if:
      mode: cel
      expr: "true"
    then:
      - type: prompt
        name: bare
        template: "Hi"
""",
    )
    payload = _score_json(orch, _empty_config(tmp_path))
    assert _paths(payload) == ["bare"]


def test_loop_body_is_scored_as_being_inside_a_loop(tmp_path: Path) -> None:
    orch = _write(tmp_path, "full.yml", FULL_ORCH)
    payload = _score_json(orch, _empty_config(tmp_path))

    row = next(r for r in payload["effects"] if r["path"] == "refine.critique")
    structure = next(
        s for s in row["breakdown"]["signals"] if s["name"] == "structure"
    )
    assert structure["detail"]["loop_depth"] == 1
    assert structure["detail"]["depth"] == 1


# ---------------------------------------------------------------------------
# unscoreable effects
# ---------------------------------------------------------------------------


def test_reflector_generated_effects_are_reported_unscoreable(
    tmp_path: Path,
) -> None:
    orch = _write(tmp_path, "full.yml", FULL_ORCH)
    payload = _score_json(orch, _empty_config(tmp_path))

    row = next(r for r in payload["effects"] if r["path"] == "planner.generated")
    assert row["scoreable"] is False
    assert row["score"] is None
    assert row["breakdown"] is None
    assert "propose_steps" in row["reason"]
    assert "runtime" in row["reason"]


def test_reflector_authored_effects_are_still_scored(tmp_path: Path) -> None:
    orch = _write(tmp_path, "full.yml", FULL_ORCH)
    payload = _score_json(orch, _empty_config(tmp_path))

    row = next(r for r in payload["effects"] if r["path"] == "planner.propose_steps")
    assert row["scoreable"] is True
    assert row["score"] is not None


def test_use_children_are_reported_unscoreable(tmp_path: Path) -> None:
    orch = _write(tmp_path, "full.yml", FULL_ORCH)
    payload = _score_json(orch, _empty_config(tmp_path))

    row = next(r for r in payload["effects"] if r["path"] == "helper")
    assert row["scoreable"] is False
    assert "compiles" in row["reason"]


def test_tool_effects_are_reported_unscoreable(tmp_path: Path) -> None:
    orch = _write(tmp_path, "full.yml", FULL_ORCH)
    payload = _score_json(orch, _empty_config(tmp_path))

    row = next(r for r in payload["effects"] if r["path"] == "fetch")
    assert row["scoreable"] is False
    assert "not a prompt" in row["reason"].lower()


def test_unscoreable_effects_show_their_reason_in_the_table(tmp_path: Path) -> None:
    orch = _write(tmp_path, "full.yml", FULL_ORCH)
    result = runner.invoke(
        app, ["score", str(orch), "--config", str(_empty_config(tmp_path))]
    )

    assert result.exit_code == 0, result.stdout
    assert "planner.generated" in result.stdout
    assert "not scoreable" in result.stdout


def test_summary_counts_unscoreable_effects(tmp_path: Path) -> None:
    orch = _write(tmp_path, "full.yml", FULL_ORCH)
    payload = _score_json(orch, _empty_config(tmp_path))

    assert payload["summary"]["scored"] == 5
    assert payload["summary"]["unscoreable"] == 3


# ---------------------------------------------------------------------------
# --config / --profile
# ---------------------------------------------------------------------------


def test_config_supplies_the_scoring_settings(tmp_path: Path) -> None:
    orch = _write(tmp_path, "plain.yml", DISABLED_ORCH)
    config = _write(
        tmp_path,
        "enabled.json",
        json.dumps({"runtime": {"complexity": {"scoring": {"enabled": True}}}}),
    )

    payload = _score_json(orch, config)
    assert payload["config"] == str(config)
    assert payload["effects"][0]["path"] == "greet"


def test_config_weights_change_the_score(tmp_path: Path) -> None:
    orch = _write(tmp_path, "plain.yml", DISABLED_ORCH)
    default_config = _write(
        tmp_path,
        "default.json",
        json.dumps({"runtime": {"complexity": {"scoring": {"enabled": True}}}}),
    )
    skewed_config = _write(
        tmp_path,
        "skewed.json",
        json.dumps(
            {
                "runtime": {
                    "complexity": {
                        "scoring": {
                            "enabled": True,
                            "weights": {"state_references": 100.0},
                        }
                    }
                }
            }
        ),
    )

    default_score = _score_json(orch, default_config)["effects"][0]["score"]
    skewed_score = _score_json(orch, skewed_config)["effects"][0]["score"]
    assert default_score != skewed_score


def test_profile_disabled_effects_are_reported_not_scored(tmp_path: Path) -> None:
    orch = _write(tmp_path, "full.yml", FULL_ORCH)
    profiles = tmp_path / "profiles"
    profiles.mkdir()
    (profiles / "quick.yml").write_text(
        "effects:\n  refine:\n    enabled: false\n", encoding="utf-8"
    )

    payload = _score_json(
        orch, _empty_config(tmp_path), "--profile", "quick"
    )
    assert payload["profile"] == "quick"

    row = next(r for r in payload["effects"] if r["path"] == "refine.critique")
    assert row["scoreable"] is False
    assert "will not execute" in row["reason"]


def test_unknown_profile_exits_with_a_message(tmp_path: Path) -> None:
    orch = _write(tmp_path, "simple.yml", SIMPLE_ORCH)
    result = runner.invoke(
        app,
        [
            "score",
            str(orch),
            "--config",
            str(_empty_config(tmp_path)),
            "--profile",
            "nope",
        ],
    )
    assert result.exit_code == 1
    assert "not found" in result.output


# ---------------------------------------------------------------------------
# scoring disabled
# ---------------------------------------------------------------------------


def test_disabled_scoring_exits_with_a_clear_message(tmp_path: Path) -> None:
    orch = _write(tmp_path, "plain.yml", DISABLED_ORCH)
    result = runner.invoke(
        app, ["score", str(orch), "--config", str(_empty_config(tmp_path))]
    )

    assert result.exit_code == 1
    assert "scoring.enabled" in result.output
    assert "Traceback" not in result.output


def test_disabled_scoring_json_is_still_machine_readable(tmp_path: Path) -> None:
    orch = _write(tmp_path, "plain.yml", DISABLED_ORCH)
    result = runner.invoke(
        app,
        ["score", str(orch), "--config", str(_empty_config(tmp_path)), "--json"],
    )

    assert result.exit_code == 1
    payload = json.loads(result.stdout)
    assert payload["ok"] is False
    assert payload["scoring_enabled"] is False
    assert "scoring.enabled" in payload["error"]


# ---------------------------------------------------------------------------
# edge cases
# ---------------------------------------------------------------------------


def test_orchestration_with_no_effects(tmp_path: Path) -> None:
    orch = _write(
        tmp_path,
        "empty.yml",
        "runtime:\n  complexity:\n    scoring:\n      enabled: true\neffects: []\n",
    )
    result = runner.invoke(
        app, ["score", str(orch), "--config", str(_empty_config(tmp_path))]
    )

    assert result.exit_code == 0, result.stdout
    assert "No effects to score" in result.stdout


def test_uncompilable_orchestration_reports_the_compiler_error(
    tmp_path: Path,
) -> None:
    orch = _write(
        tmp_path,
        "broken.yml",
        """\
runtime:
  complexity:
    scoring:
      enabled: true
effects:
  - type: prompt
    template: "no name"
""",
    )
    result = runner.invoke(
        app, ["score", str(orch), "--config", str(_empty_config(tmp_path))]
    )

    assert result.exit_code == 1
    assert "missing required field 'name'" in result.output


def test_score_is_deterministic(tmp_path: Path) -> None:
    orch = _write(tmp_path, "full.yml", FULL_ORCH)
    config = _empty_config(tmp_path)
    assert _score_json(orch, config) == _score_json(orch, config)


# ---------------------------------------------------------------------------
# the config -> scorer weight-name translation
# ---------------------------------------------------------------------------


def test_scorer_weights_translate_config_signal_names() -> None:
    weights = ScoringSettings(weights={"prompt_type": 3.0}).scorer_weights()
    assert weights == {"output_type": 3.0}


def test_every_config_signal_maps_onto_a_scorer_signal() -> None:
    assert set(SCORER_SIGNAL_NAMES.values()) == set(SIGNAL_NAMES)
