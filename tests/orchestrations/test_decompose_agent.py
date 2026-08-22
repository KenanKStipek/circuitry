"""Tests for curation/agents/decompose.yml — the bundled decomposition planner.

The planner is driven exactly the way a host drives it: one whole run through
the runtime shim with a scripted adapter standing in for the model. The adapter
dispatches on markers in the rendered prompt rather than on call order, so a
test that changes how many model calls a run makes still fails on the *content*
rather than silently shifting a queue.

What must hold, whatever the model says:
  * a run never surfaces YAML that did not validate,
  * the chunk budget is enforced, not requested,
  * the revision loop is bounded and `done` is deterministic,
  * the merged result is always at `prime.merge.value`,
  * and — the criterion the whole feature rests on — the chunks it emits
    actually score BELOW the source on `core.complexity`. A fan-out whose
    chunks are each as hard as the original validates, runs, and accomplishes
    nothing; only the scorer can tell the difference.
"""

from __future__ import annotations

import json
import re
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import pytest
import yaml

from circuitry.adapters.base import GenerateResult
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.profiles import collect_orchestration_effect_paths
from circuitry.cli.runtime_shim import RunRequest, RunResult, run
from circuitry.core.compiler import apply_effect_overrides, compile_orchestration
from circuitry.core.complexity import ComplexityScore, score
from circuitry.core.primes import DECOMPOSE_PRIME_V1, WIZARD_PRIME_V1
from circuitry.core.prompt import PromptDefinition
from circuitry.plugins.validate_yaml import ValidateYamlPlugin

DECOMPOSE_PATH = Path("src/circuitry/curation/agents/decompose.yml")

#: The path the emitted orchestration must expose its merged result at. #115
#: maps this back onto the original effect's state path, so it is a contract,
#: not a convention — every layer that names it is checked against this one.
MERGE_PATH = "prime.merge.value"

# Markers identifying which prompt the planner is issuing. They are phrases
# from the templates themselves, so a template rewrite that drops one is a test
# failure rather than a silent mis-route.
PLAN_MARKER = "=== THIS TASK ==="
REPAIR_MARKER = "=== REPAIR TASK ==="

# Fences off the worked example carried inside DECOMPOSE_PRIME_V1.
EXAMPLE_SOURCE_FENCE = "--- EXAMPLE SOURCE EFFECT ---"
EXAMPLE_EMITTED_FENCE = "--- EXAMPLE EMITTED ORCHESTRATION ---"
EXAMPLE_END_FENCE = "--- END EXAMPLE ---"


# ── The source prompt under test: deliberately over-complex ──────────────────
#
# Six inputs, five numbered jobs, a nested output schema, and most of the
# scorer's expensive keywords. This is the shape the planner exists to break up.

SOURCE_EFFECT: dict[str, Any] = {
    "type": "prompt",
    "name": "market_brief",
    "prompt_type": "object",
    "schema": {
        "type": "object",
        "properties": {
            "competitors": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": {"type": "string"},
                        "segment": {"type": "string"},
                        "threat": {"type": "string"},
                    },
                    "required": ["name", "segment", "threat"],
                },
            },
            "pricing_gaps": {"type": "array", "items": {"type": "string"}},
            "sentiment": {"type": "string"},
            "recommendation": {"type": "string"},
            "justification": {"type": "string"},
        },
        "required": [
            "competitors",
            "pricing_gaps",
            "sentiment",
            "recommendation",
            "justification",
        ],
        "additionalProperties": False,
    },
    "template": """\
You are the strategy desk. Read every document below and produce the whole
market brief in a single pass.

Competitor filings:
{{filings}}

Our current price sheet:
{{price_sheet}}

Their published price sheets:
{{rival_prices}}

Support tickets from the last quarter:
{{tickets}}

Analyst commentary:
{{commentary}}

Last quarter's brief:
{{prior_brief}}

Do all of the following, in order:
1. Extract every competitor named anywhere in the filings or the analyst
   commentary, together with the segment each one plays in, including
   competitors that are only referred to obliquely.
2. Classify how much of a threat each competitor is, and explain the
   classification wherever it is not self-evident from the filings.
3. Cross-reference our price sheet against theirs and derive every gap where
   we are priced above the market, weighing volume tiers against list price.
4. Analyze the support tickets and synthesize the customer sentiment into one
   paragraph, reconciling contradictory tickets against each other.
5. Infer a single recommendation. Reason step by step from the competitors,
   the pricing gaps and the sentiment to the recommendation, compare it
   against last quarter's brief, and justify why you rejected the
   alternatives.
""",
}

SOURCE_INTERFACE = yaml.safe_dump(
    {
        "inputs": {
            name: {"type": "string", "required": True}
            for name in (
                "filings",
                "price_sheet",
                "rival_prices",
                "tickets",
                "commentary",
                "prior_brief",
            )
        }
    },
    sort_keys=False,
)

SOURCE_OUTPUT = yaml.safe_dump(
    {"prompt_type": SOURCE_EFFECT["prompt_type"], "schema": SOURCE_EFFECT["schema"]},
    sort_keys=False,
)


# ── What a good decomposition of it looks like ───────────────────────────────

GOOD_DECOMPOSITION = """\
# Market brief, decomposed: four narrow readings, then one assembly.
# Inputs: filings, price_sheet, rival_prices, tickets, commentary, prior_brief.
# Merged result: prime.merge.value
interface:
  inputs:
    filings: {type: string, required: true}
    price_sheet: {type: string, required: true}
    rival_prices: {type: string, required: true}
    tickets: {type: string, required: true}
    commentary: {type: string, required: true}
    prior_brief: {type: string, required: false}
  outputs:
    result:
      type: object
      path: prime.merge.value
      description: The market brief, in the shape the original prompt produced.

effects:
  # The fan-out. Each chunk reads one document and answers one question.
  - type: dynamic
    name: parts
    flow: tree
    effects:
      # Unit 1 — who is in the market.
      - type: prompt
        name: competitors
        prompt_type: array
        schema:
          type: array
          items: {type: string}
        template: |
          Name each company mentioned in these filings.

          {{filings}}

          Return ONLY a JSON array of strings.

      # Unit 2 — where our prices sit.
      - type: prompt
        name: pricing_gaps
        prompt_type: array
        schema:
          type: array
          items: {type: string}
        template: |
          Which lines are priced higher here than there?

          Ours:
          {{price_sheet}}

          Theirs:
          {{rival_prices}}

          Return ONLY a JSON array of line names.

      # Unit 3 — what customers said.
      - type: prompt
        name: sentiment
        prompt_type: text
        template: |
          Sum up the mood of these support tickets in two sentences.

          {{tickets}}

      # Unit 4 — what the analysts said.
      - type: prompt
        name: commentary_note
        prompt_type: text
        template: |
          Sum up this analyst commentary in two sentences.

          {{commentary}}

  # The merge. Top level, named `merge`, same output shape as the original.
  - type: prompt
    name: merge
    prompt_type: object
    schema:
      type: object
      properties:
        competitors:
          type: array
          items:
            type: object
            properties:
              name: {type: string}
              segment: {type: string}
              threat: {type: string}
            required: [name, segment, threat]
        pricing_gaps:
          type: array
          items: {type: string}
        sentiment: {type: string}
        recommendation: {type: string}
        justification: {type: string}
      required: [competitors, pricing_gaps, sentiment, recommendation, justification]
      additionalProperties: false
    template: |
      Assemble the market brief from the parts below.

      Companies: {{prime.parts.competitors.value}}
      Price gaps: {{prime.parts.pricing_gaps.value}}
      Customer mood: {{prime.parts.sentiment.value}}
      Analyst view: {{prime.parts.commentary_note.value}}

      Return ONLY the JSON object.
"""

GOOD_CHUNKS = [
    {"name": "competitors", "job": "Name the companies in the filings."},
    {"name": "pricing_gaps", "job": "Compare our price sheet to theirs."},
    {"name": "sentiment", "job": "Summarize the support tickets."},
    {"name": "commentary_note", "job": "Summarize the analyst commentary."},
]

# Two independent defects: a name the pattern rejects, and a prompt with no
# template. The first is caught by the schema, the second by the oneOf.
INVALID_DECOMPOSITION = """\
effects:
  - type: prompt
    name: 1_bad_name
    template: "Do part of the thing."
  - type: prompt
    name: merge
"""


def _wide_decomposition(count: int) -> tuple[str, list[dict[str, str]]]:
    """A schema-valid fan-out `count` chunks wide — for budget tests."""
    parts = [
        f"""\
      - type: prompt
        name: part_{i}
        prompt_type: text
        template: |
          Summarize section {i}.

          {{{{filings}}}}
"""
        for i in range(count)
    ]
    document = (
        "# Wide fan-out.\n"
        "# Merged result: prime.merge.value\n"
        "interface:\n"
        "  outputs:\n"
        "    result:\n"
        "      path: prime.merge.value\n"
        "effects:\n"
        "  - type: dynamic\n"
        "    name: parts\n"
        "    flow: tree\n"
        "    effects:\n" + "".join(parts) + "  - type: prompt\n"
        "    name: merge\n"
        "    template: |\n"
        "      Assemble the brief from the parts.\n"
    )
    chunks = [{"name": f"part_{i}", "job": f"Section {i}."} for i in range(count)]
    return document, chunks


# ── Scripted adapter ─────────────────────────────────────────────────────────


@dataclass
class ScriptedAdapter:
    """Answers each planner prompt from a per-marker queue of replies.

    The last reply for a marker is reused if the planner asks again, which is
    what makes the "every revision still fails" case expressible.
    """

    replies: dict[str, list[str]]
    name: str = "scripted"
    prompts: list[str] = field(default_factory=list)

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.prompts.append(prompt)
        for marker, queue in self.replies.items():
            if marker in prompt:
                text = queue.pop(0) if len(queue) > 1 else queue[0]
                return GenerateResult(text=text, raw={"marker": marker})
        raise AssertionError(f"Unscripted prompt:\n{prompt[:400]}")

    def check(self) -> Any:  # pragma: no cover - never exercised in tests
        from circuitry.preflight import CheckResult

        return CheckResult(ok=True, missing=[])

    def prompts_matching(self, marker: str) -> list[str]:
        return [p for p in self.prompts if marker in p]


def _plan_reply(
    *,
    yaml_text: str,
    chunks: list[dict[str, str]],
    say: str = "Split it four ways.",
) -> str:
    return json.dumps({"say": say, "chunks": chunks, "yaml": yaml_text})


def _planner(
    *,
    plans: list[str],
    repairs: list[str] | None = None,
) -> ScriptedAdapter:
    replies = {PLAN_MARKER: plans}
    if repairs is not None:
        replies[REPAIR_MARKER] = repairs
    return ScriptedAdapter(replies=replies)


def _run_plan(
    adapter: ScriptedAdapter,
    *,
    max_chunks: int = 5,
    source_template: str | None = None,
) -> RunResult:
    """One planner run, driven exactly as a host would drive it."""
    return run(
        RunRequest(
            orchestration_path=DECOMPOSE_PATH,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state={
                "source_template": (
                    SOURCE_EFFECT["template"]
                    if source_template is None
                    else source_template
                ),
                "source_interface": SOURCE_INTERFACE,
                "source_output": SOURCE_OUTPUT,
                "max_chunks": max_chunks,
            },
            adapter=adapter,
            config=CircuitryConfig(default_adapter="scripted", default_model="test"),
            skip_preflight=True,
        )
    )


def _output(result: RunResult) -> dict[str, Any]:
    """Read the run contract off the state, using the manifest's paths."""
    prime = result.state["prime"]
    check = prime.get("check") or {}
    return {
        "say": prime["plan"]["value"]["say"],
        "chunks": prime["plan"]["value"]["chunks"],
        "yaml": check.get("value", {}).get("yaml"),
        "ok": check.get("value", {}).get("ok"),
        "errors": check.get("value", {}).get("errors"),
        "result_path": prime["result_path"]["value"],
        "done": prime["done"]["value"],
    }


def _assert_compiles(yaml_text: str) -> None:
    """The bar the issue sets: schema-clean AND compilable."""
    verdict = ValidateYamlPlugin().execute(params={"yaml": yaml_text}).value
    assert verdict["ok"], verdict["errors"]
    compile_orchestration(orch=yaml.safe_load(yaml_text), root_name="prime")


# ── Scoring helpers ──────────────────────────────────────────────────────────


def _prompt_effects(node: Any, depth: int = 0) -> list[tuple[dict[str, Any], int]]:
    """Every prompt effect in a document, paired with its nesting depth."""
    found: list[tuple[dict[str, Any], int]] = []
    if isinstance(node, dict):
        if node.get("type") == "prompt":
            found.append((node, depth))
        for key in ("effects", "then", "else", "body"):
            if key in node:
                found.extend(_prompt_effects(node[key], depth + 1))
    elif isinstance(node, list):
        for item in node:
            found.extend(_prompt_effects(item, depth))
    return found


def _chunk_scores(document: str) -> dict[str, ComplexityScore]:
    """Score every prompt in an emitted orchestration, keyed by effect name."""
    parsed = yaml.safe_load(document)
    return {
        effect["name"]: score(effect, structure={"depth": depth})
        for effect, depth in _prompt_effects(parsed["effects"])
    }


def _example_blocks() -> tuple[dict[str, Any], str]:
    """The worked example carried inside DECOMPOSE_PRIME_V1.

    Returned as (source effect mapping, emitted orchestration text). This is
    the exemplar the planning prompt teaches from, so it is the artifact whose
    simplification has to be measured — not just whatever a mock returned.
    """
    body = DECOMPOSE_PRIME_V1.split(EXAMPLE_SOURCE_FENCE)[1]
    source_text, rest = body.split(EXAMPLE_EMITTED_FENCE)
    emitted_text = rest.split(EXAMPLE_END_FENCE)[0]
    return yaml.safe_load(source_text), emitted_text


# ── The run contract ─────────────────────────────────────────────────────────


def test_plan_run_emits_a_validated_fan_out() -> None:
    """The happy path: one planning call, valid first try, unrevised."""
    adapter = _planner(
        plans=[_plan_reply(yaml_text=GOOD_DECOMPOSITION, chunks=GOOD_CHUNKS)]
    )

    result = _run_plan(adapter)

    assert result.ok, result.error
    out = _output(result)
    assert out["ok"] is True
    assert out["errors"] == []
    assert out["done"] is True
    assert len(out["chunks"]) == len(GOOD_CHUNKS)
    _assert_compiles(out["yaml"])
    assert adapter.prompts_matching(REPAIR_MARKER) == []


def test_emitted_orchestration_exposes_the_merged_result_at_one_known_path() -> None:
    """#115 maps this path back onto the original effect's state path, so it is
    checked three ways: the effect exists, it is top level, and the emitted
    interface points at it."""
    adapter = _planner(
        plans=[_plan_reply(yaml_text=GOOD_DECOMPOSITION, chunks=GOOD_CHUNKS)]
    )

    out = _output(_run_plan(adapter))
    emitted = yaml.safe_load(out["yaml"])

    top_level = [e.get("name") for e in emitted["effects"]]
    assert "merge" in top_level, f"no top-level `merge` effect: {top_level}"
    assert emitted["interface"]["outputs"]["result"]["path"] == MERGE_PATH
    # The planner reports the path rather than making the caller hardcode it.
    assert out["result_path"] == MERGE_PATH


def test_source_inputs_survive_onto_the_emitted_document() -> None:
    """Chunks read the original inputs under the original names, so the
    emitted orchestration is a drop-in for the source."""
    adapter = _planner(
        plans=[_plan_reply(yaml_text=GOOD_DECOMPOSITION, chunks=GOOD_CHUNKS)]
    )

    out = _output(_run_plan(adapter))
    emitted = yaml.safe_load(out["yaml"])

    for name in ("filings", "price_sheet", "rival_prices", "tickets", "commentary"):
        assert name in emitted["interface"]["inputs"], f"{name} was dropped"


def test_the_source_reaches_the_planning_prompt() -> None:
    """The planner is told what it is decomposing — template, inputs, output
    shape and budget all reach the model."""
    adapter = _planner(
        plans=[_plan_reply(yaml_text=GOOD_DECOMPOSITION, chunks=GOOD_CHUNKS)]
    )

    _run_plan(adapter, max_chunks=4)

    planned = adapter.prompts_matching(PLAN_MARKER)[0]
    assert "You are the strategy desk." in planned
    assert "rival_prices" in planned
    assert "at most 4 chunks" in planned


# ── The revision loop ────────────────────────────────────────────────────────


def test_invalid_first_draft_is_revised_not_surfaced() -> None:
    """The failing draft never reaches the host; the repair does."""
    adapter = _planner(
        plans=[_plan_reply(yaml_text=INVALID_DECOMPOSITION, chunks=GOOD_CHUNKS)],
        repairs=[_plan_reply(yaml_text=GOOD_DECOMPOSITION, chunks=GOOD_CHUNKS)],
    )

    result = _run_plan(adapter)

    assert result.ok, result.error
    out = _output(result)
    assert out["ok"] is True
    assert out["done"] is True
    assert "1_bad_name" not in out["yaml"]
    _assert_compiles(out["yaml"])

    # Exactly one repair pass, and it was told what was wrong.
    repairs = adapter.prompts_matching(REPAIR_MARKER)
    assert len(repairs) == 1
    assert "1_bad_name" in repairs[0]
    assert "$.effects[0].name" in repairs[0]


def test_revision_loop_is_bounded_and_refuses_to_finish() -> None:
    """When every repair still fails, the loop stops at max_iterations and the
    done gate stays false — an invalid decomposition is never handed back as a
    finished one."""
    adapter = _planner(
        plans=[_plan_reply(yaml_text=INVALID_DECOMPOSITION, chunks=GOOD_CHUNKS)],
        repairs=[_plan_reply(yaml_text=INVALID_DECOMPOSITION, chunks=GOOD_CHUNKS)],
    )

    result = _run_plan(adapter)

    assert result.ok, result.error
    out = _output(result)
    assert len(adapter.prompts_matching(REPAIR_MARKER)) == _max_iterations()
    assert out["ok"] is False
    assert out["errors"]
    assert out["done"] is False


def test_empty_yaml_counts_as_invalid_and_triggers_revision() -> None:
    """A plan that produces no YAML is a defect the loop repairs, not a result
    the host has to detect."""
    adapter = _planner(
        plans=[_plan_reply(yaml_text="", chunks=GOOD_CHUNKS)],
        repairs=[_plan_reply(yaml_text=GOOD_DECOMPOSITION, chunks=GOOD_CHUNKS)],
    )

    out = _output(_run_plan(adapter))

    assert out["ok"] is True
    assert out["done"] is True
    assert len(adapter.prompts_matching(REPAIR_MARKER)) == 1


def test_fenced_yaml_is_cleaned_before_it_is_returned() -> None:
    """A fenced plan is cleaned by the validator, and the cleaned document —
    not the model's raw string — is what the run hands back."""
    adapter = _planner(
        plans=[
            _plan_reply(
                yaml_text=f"```yaml\n{GOOD_DECOMPOSITION}```", chunks=GOOD_CHUNKS
            )
        ]
    )

    out = _output(_run_plan(adapter))

    assert "```" not in out["yaml"]
    assert out["ok"] is True
    _assert_compiles(out["yaml"])


# ── The chunk budget ─────────────────────────────────────────────────────────


def test_over_budget_plan_is_revised_even_though_it_validates() -> None:
    """A nine-way fan-out under a budget of three is schema-perfect and still
    wrong. The budget is enforced by the same gate as validation, so it is
    caught here rather than by the caller."""
    wide_yaml, wide_chunks = _wide_decomposition(9)
    adapter = _planner(
        plans=[_plan_reply(yaml_text=wide_yaml, chunks=wide_chunks)],
        repairs=[_plan_reply(yaml_text=GOOD_DECOMPOSITION, chunks=GOOD_CHUNKS[:3])],
    )

    result = _run_plan(adapter, max_chunks=3)

    assert result.ok, result.error
    out = _output(result)
    # The over-wide draft validated on its own terms...
    assert out["ok"] is True
    # ...and was still sent back, because the budget said so.
    repairs = adapter.prompts_matching(REPAIR_MARKER)
    assert len(repairs) == 1
    assert "at most 3 chunks" in repairs[0]
    assert len(out["chunks"]) == 3
    assert out["done"] is True


def test_budget_that_is_never_honored_refuses_to_finish() -> None:
    """A planner that keeps busting the budget runs out of revisions and the
    gate reports failure rather than returning an over-wide fan-out as done."""
    wide_yaml, wide_chunks = _wide_decomposition(9)
    adapter = _planner(
        plans=[_plan_reply(yaml_text=wide_yaml, chunks=wide_chunks)],
        repairs=[_plan_reply(yaml_text=wide_yaml, chunks=wide_chunks)],
    )

    out = _output(_run_plan(adapter, max_chunks=3))

    assert len(adapter.prompts_matching(REPAIR_MARKER)) == _max_iterations()
    assert out["ok"] is True  # the YAML was fine throughout
    assert len(out["chunks"]) == 9
    assert out["done"] is False  # ...but the budget never was


@pytest.mark.parametrize("budget", [2, 4, 8])
def test_a_plan_inside_the_budget_is_left_alone(budget: int) -> None:
    """The gate is a ceiling, not a target — it never revises a plan that fits."""
    narrow_yaml, narrow_chunks = _wide_decomposition(2)
    adapter = _planner(
        plans=[_plan_reply(yaml_text=narrow_yaml, chunks=narrow_chunks)]
    )

    out = _output(_run_plan(adapter, max_chunks=budget))

    assert adapter.prompts_matching(REPAIR_MARKER) == []
    assert out["done"] is True


def test_a_single_chunk_is_not_a_decomposition() -> None:
    """One chunk is the original prompt with extra scaffolding. The floor is
    enforced the same way the ceiling is."""
    one_yaml, one_chunk = _wide_decomposition(1)
    adapter = _planner(
        plans=[_plan_reply(yaml_text=one_yaml, chunks=one_chunk)],
        repairs=[_plan_reply(yaml_text=GOOD_DECOMPOSITION, chunks=GOOD_CHUNKS)],
    )

    out = _output(_run_plan(adapter, max_chunks=5))

    assert len(adapter.prompts_matching(REPAIR_MARKER)) == 1
    assert len(out["chunks"]) == len(GOOD_CHUNKS)
    assert out["done"] is True


# ── The measured criterion: the chunks are actually simpler ──────────────────


def test_the_taught_example_decomposes_into_strictly_simpler_prompts() -> None:
    """The worked example inside DECOMPOSE_PRIME_V1 is the exemplar the model
    is shown, so it is the artifact that has to survive measurement.

    Every prompt in the emitted orchestration — the chunks *and* the merge —
    must score below the source on `core.complexity`. The merge is included
    deliberately: it carries the source's output schema, so if the difficulty
    had merely been relocated rather than reduced, this is where it would show
    up.
    """
    source_effect, emitted_text = _example_blocks()
    source = score(source_effect)
    scores = _chunk_scores(emitted_text)

    assert scores, "the worked example emits no prompt effects"
    assert "merge" in scores, "the worked example has no merge step"

    too_hard = {
        name: result.score
        for name, result in scores.items()
        if result.score >= source.score
    }
    assert not too_hard, (
        f"source scores {source.score:.2f}; these are not simpler: {too_hard}\n"
        f"{source.explain()}"
    )


def test_the_taught_example_is_itself_a_valid_orchestration() -> None:
    """A worked example that would not validate teaches the model to produce
    drafts the revision loop then has to repair."""
    _, emitted_text = _example_blocks()
    _assert_compiles(emitted_text)


def test_emitted_chunks_score_below_the_source_end_to_end() -> None:
    """Same measurement, on what actually came out of a run.

    This is the criterion the feature stands on: a decomposition whose chunks
    are individually as hard as the original has done nothing, and nothing
    else in the pipeline can see that.
    """
    adapter = _planner(
        plans=[_plan_reply(yaml_text=GOOD_DECOMPOSITION, chunks=GOOD_CHUNKS)]
    )

    out = _output(_run_plan(adapter))

    source = score(SOURCE_EFFECT)
    scores = _chunk_scores(out["yaml"])

    planned = {chunk["name"] for chunk in out["chunks"]}
    assert planned <= set(scores), (
        f"plan names chunks that are not in the YAML: {planned - set(scores)}"
    )

    for name in planned:
        assert scores[name].score < source.score, (
            f"chunk {name!r} scores {scores[name].score:.2f}, "
            f"source scores {source.score:.2f} — no simplification"
        )
    assert scores["merge"].score < source.score


def test_the_scorer_would_catch_a_fan_out_that_simplifies_nothing() -> None:
    """The guard above is only worth having if it can fail. A "decomposition"
    that copies the source prompt into every chunk is exactly the failure mode
    the acceptance criterion is aimed at — assert the scorer flags it."""
    restated = {
        **SOURCE_EFFECT,
        "name": "part_0",
    }
    source = score(SOURCE_EFFECT)
    assert score(restated, structure={"depth": 1}).score >= source.score


# ── File-level contracts ─────────────────────────────────────────────────────


def _decompose() -> dict[str, Any]:
    return yaml.safe_load(DECOMPOSE_PATH.read_text(encoding="utf-8"))


def _plan_effects() -> list[dict[str, Any]]:
    """Both instances of the planning prompt: the first pass and the repair."""
    return [e for e, _ in _prompt_effects(_decompose()["effects"])]


def _loop() -> dict[str, Any]:
    return next(e for e in _decompose()["effects"] if e["type"] == "loop")


def _max_iterations() -> int:
    return int(_loop()["max_iterations"])


def test_planning_is_a_single_prompt_effect() -> None:
    """The seam a purpose-built planner model gets swapped in at. Planning
    spread across several effects would close it."""
    effects = _decompose()["effects"]
    top_level_prompts = [e for e in effects if e["type"] == "prompt"]
    assert [e["name"] for e in top_level_prompts] == ["plan"]

    # The repair is the same effect re-run, not a second planner.
    assert [e["name"] for e in _plan_effects()] == ["plan", "plan"]


def test_the_planner_model_is_settable_from_a_profile() -> None:
    """`plan` is a valid profile key, and because the revision loop is unnamed
    both the first pass and the repair sit at that one path — so pointing the
    planner at a different model is a config change, not a rewrite."""
    orch = _decompose()
    assert "plan" in collect_orchestration_effect_paths(orch)

    compiled = compile_orchestration(orch=orch, root_name="prime")
    overlaid, matched = apply_effect_overrides(
        compiled, {"plan": {"model": "my-planner-7b", "provider": "ollama"}}
    )
    assert matched == {"plan"}

    retargeted = [
        node
        for node in _walk_definitions(overlaid)
        if isinstance(node, PromptDefinition) and node.name == "plan"
    ]
    assert len(retargeted) == 2, "the repair pass did not share the planner's path"
    for node in retargeted:
        assert node.model == "my-planner-7b"
        assert node.provider == "ollama"


def test_no_model_or_adapter_is_hardcoded() -> None:
    """House style: the user's config (or a profile) supplies them."""
    orch = _decompose()
    assert "model" not in orch
    assert "adapter" not in orch
    for effect in _plan_effects():
        assert "model" not in effect
        assert "provider" not in effect


def test_revision_loop_is_unnamed_so_its_condition_can_see_itself() -> None:
    """A named loop buries each pass under iter_<N>, out of reach of the CEL
    condition. Naming this loop would silently make it run to max_iterations —
    and would give the repair pass a different profile path."""
    loop = _loop()
    assert "name" not in loop
    assert loop["while"]["mode"] == "cel"
    assert loop["max_iterations"] >= 1
    assert [e["name"] for e in loop["body"]] == ["plan", "check"]


def test_the_revision_condition_covers_validity_and_budget() -> None:
    """Both defects are revisable, and both are decided by CEL rather than by
    asking the model whether it did well."""
    expr = _loop()["while"]["expr"]
    assert "state.prime.check.value.ok == false" in expr
    assert "size(state.prime.plan.value.chunks) > state.max_chunks" in expr
    assert "size(state.prime.plan.value.chunks) < 2" in expr


def test_done_gate_is_deterministic() -> None:
    """`done` is decided by CEL over the validation result and the chunk count,
    never by a prompt."""
    gate = next(
        e
        for e in _decompose()["effects"]
        if e["type"] == "if" and e["then"][0].get("name") == "done"
    )
    assert gate["if"]["mode"] == "cel"
    expr = gate["if"]["expr"]
    assert "state.prime.check.value.ok == true" in expr
    assert "size(state.prime.plan.value.chunks) <= state.max_chunks" in expr
    assert "size(state.prime.plan.value.chunks) >= 2" in expr
    for branch in ("then", "else"):
        (effect,) = gate[branch]
        assert effect["type"] == "tool"
        assert effect["name"] == "done"
    assert gate["then"][0]["params"]["input"] == "true"
    assert gate["else"][0]["params"]["input"] == "false"


def test_validation_is_a_tool_not_a_prompt() -> None:
    """Never returns YAML that failed its own check — and the check is the
    deterministic one, not a model's opinion of the draft."""
    checks = [
        e
        for e in _prompt_or_tool(_decompose()["effects"])
        if e.get("name") == "check"
    ]
    assert len(checks) == 2, "the repair pass is not re-validated"
    for effect in checks:
        assert effect["type"] == "tool"
        assert effect["provider"] == "validate_yaml"


def test_result_path_is_a_constant_not_a_model_call() -> None:
    """The one known output path is baked in, so it cannot drift per run."""
    effect = next(
        e for e in _decompose()["effects"] if e.get("name") == "result_path"
    )
    assert effect["type"] == "tool"
    assert effect["provider"] == "json"
    assert json.loads(effect["params"]["input"]) == MERGE_PATH


def test_the_merge_path_is_spelled_the_same_everywhere() -> None:
    """The file header, the prime, and the constant effect all name one path."""
    text = DECOMPOSE_PATH.read_text(encoding="utf-8")
    assert MERGE_PATH in text
    assert MERGE_PATH in DECOMPOSE_PRIME_V1
    # And nothing teaches a rival spelling.
    rivals = set(re.findall(r"prime\.merge\.[a-z_.]+", DECOMPOSE_PRIME_V1))
    assert rivals == {MERGE_PATH}


def test_primes_are_embedded_verbatim() -> None:
    """The YAML stands alone, so it carries its own copies. This is the drift
    guard between those copies and core.primes."""
    for effect in _plan_effects():
        assert effect["inputs"]["wizard_prime"] == WIZARD_PRIME_V1
        assert effect["inputs"]["decompose_prime"] == DECOMPOSE_PRIME_V1


def test_primes_are_injected_unescaped() -> None:
    """A double-stache would HTML-escape the cheat-sheets' quotes and angle
    brackets into nonsense — every injection must be a triple-stache."""
    for effect in _plan_effects():
        assert "{{{wizard_prime}}}" in effect["template"]
        assert "{{{decompose_prime}}}" in effect["template"]


def test_yaml_bearing_state_paths_are_injected_unescaped() -> None:
    """Same trap, higher stakes: an escaped draft can never parse."""
    text = DECOMPOSE_PATH.read_text(encoding="utf-8")
    for path in (
        "prime.plan.value.yaml",
        "prime.check.value.yaml",
        "prime.check.value.errors",
    ):
        uses = re.findall(r"\{{2,3}" + re.escape(path) + r"\}{2,3}", text)
        assert uses, f"{path} is never interpolated"
        for use in uses:
            assert use.startswith("{{{"), f"{path} is interpolated escaped: {use}"


def test_the_prime_teaches_canonical_dsl_spellings_only() -> None:
    """#90 canonicalized the DSL; the corpus this planner writes must not
    reintroduce the deprecated aliases."""
    from circuitry.core.lint import (
        DEPRECATED_EFFECT_TYPE_ALIASES,
        DEPRECATED_FLOW_ALIASES,
    )

    spellings = [f"type: {alias}" for alias in DEPRECATED_EFFECT_TYPE_ALIASES]
    spellings += [f"flow: {alias}" for alias in DEPRECATED_FLOW_ALIASES]
    for spelling in spellings:
        assert spelling not in DECOMPOSE_PRIME_V1, (
            f"DECOMPOSE_PRIME teaches the deprecated spelling {spelling!r}"
        )


def test_interface_matches_the_manifest() -> None:
    manifest = json.loads(
        Path("src/circuitry/curation/manifest.json").read_text(encoding="utf-8")
    )
    entry = next(
        e for e in manifest["entries"] if e["name"] == "agents/decompose"
    )
    interface = _decompose()["interface"]

    assert set(entry["outputs"]) == set(interface["outputs"])
    for name, spec in entry["outputs"].items():
        assert spec["path"] == interface["outputs"][name]["path"]
    assert set(entry["inputs"]) == set(interface["inputs"])


def _prompt_or_tool(node: Any) -> list[dict[str, Any]]:
    found: list[dict[str, Any]] = []
    if isinstance(node, dict):
        if node.get("type") in ("prompt", "tool"):
            found.append(node)
        for key in ("effects", "then", "else", "body"):
            if key in node:
                found.extend(_prompt_or_tool(node[key]))
    elif isinstance(node, list):
        for item in node:
            found.extend(_prompt_or_tool(item))
    return found


def _walk_definitions(node: Any) -> list[Any]:
    """Every compiled definition in the tree, at any depth."""
    found = [node]
    for attr in ("effects", "then_effects", "else_effects", "body"):
        children = getattr(node, attr, None)
        if children:
            for child in children:
                found.extend(_walk_definitions(child))
    return found
