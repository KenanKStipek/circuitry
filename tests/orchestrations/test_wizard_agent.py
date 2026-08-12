"""Tests for curation/agents/wizard.yml — the orchestration that writes orchestrations.

The wizard is driven exactly the way a host drives it: one whole turn per run,
through the runtime shim, with a scripted adapter standing in for the model. The
adapter dispatches on markers in the rendered prompt rather than on call order,
so a test that changes how many model calls a turn makes still fails loudly on
the *content* rather than silently shifting a queue.

What must hold, whatever the model says:
  * a turn never surfaces YAML that did not validate,
  * the internal revision loop is bounded,
  * `done` is true only when the draft is valid.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any

import pytest
import yaml

from circuitry.adapters.base import GenerateResult
from circuitry.cli.config import CircuitryConfig
from circuitry.cli.runtime_shim import RunRequest, RunResult, run
from circuitry.core.compiler import compile_orchestration
from circuitry.core.primes import WIZARD_PRIME_V1
from circuitry.plugins.validate_yaml import ValidateYamlPlugin

WIZARD_PATH = Path("src/circuitry/curation/agents/wizard.yml")

# Markers that identify which prompt the wizard is issuing. They are phrases
# from the templates themselves, so a template rewrite that drops one is a test
# failure rather than a silent mis-route.
INTERPRET_MARKER = "You are interpreting a design conversation"
DECIDE_MARKER = "Should the wizard ask ONE clarifying question"
DRAFT_MARKER = "=== THIS TURN ==="
REPAIR_MARKER = "=== REPAIR TASK ==="
QUESTION_MARKER = "You have decided to ask ONE clarifying question"

# ── Target shapes the wizard is expected to be able to produce ───────────────

SIMPLE_CHAIN = """\
# Summarize an article in two passes.
# Inputs: article (string). Primary output: prime.polish.value
effects:
  - type: prompt
    name: summarize
    template: |
      Summarize the following article in five sentences.

      {{article}}
  - type: prompt
    name: polish
    template: |
      Tighten this summary to three sentences.

      {{prime.summarize.value}}
"""

LOOP_CONDITIONAL_PIPELINE = """\
# Draft a section per topic, then branch on the review score.
# Inputs: brief (string). Primary output: prime.verdict.value
effects:
  - type: prompt
    name: plan
    prompt_type: json
    schema:
      type: array
      items:
        type: string
    template: |
      List 3 section topics for: {{brief}}
      Return ONLY a JSON array of strings.
  - type: loop
    name: sections
    collect: draft
    each:
      in: prime.plan.value
      as: topic
    body:
      - type: prompt
        name: draft
        template: "Write one paragraph about {{topic}}."
  - type: prompt
    name: score
    prompt_type: number
    template: |
      Rate these sections 1-10. Return ONLY the number.

      {{prime.sections.collected.value}}
  - type: if
    if:
      mode: cel
      expr: "state.prime.score.value > 7"
    then:
      - type: prompt
        name: verdict
        template: "Explain what makes these sections strong."
    else:
      - type: prompt
        name: verdict
        template: "List what must be fixed in these sections."
"""

# Two independent defects: a name the pattern rejects, and a prompt with no
# template. The first is caught by the schema, the second by the oneOf.
INVALID_DRAFT = """\
effects:
  - type: prompt
    name: 1_bad_name
    template: "Do the thing."
  - type: prompt
    name: no_template
"""


# ── Scripted adapter ─────────────────────────────────────────────────────────


@dataclass
class ScriptedAdapter:
    """Answers each wizard prompt from a per-marker queue of replies.

    The last reply for a marker is reused if the wizard asks again, which is
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


def _interpretation(
    *, intent: str = "Summarize an article.", ready: bool = True
) -> str:
    return json.dumps(
        {
            "intent": intent,
            "unknowns": [],
            "ready_to_draft": ready,
            "reason": "The goal is concrete enough to draft.",
        }
    )


def _json_turn(*, say: str, yaml_text: str | None, done: bool) -> str:
    return json.dumps({"say": say, "yaml": yaml_text, "done": done})


def _run_turn(
    adapter: ScriptedAdapter,
    *,
    goal: str = "Summarize an article.",
    conversation: list[dict[str, str]] | None = None,
    draft: str = "",
) -> RunResult:
    """One wizard turn, driven exactly as a host would drive it."""
    return run(
        RunRequest(
            orchestration_path=WIZARD_PATH,
            state_path=None,
            out_path=None,
            dry_run=False,
            validate_only=False,
            initial_state={
                "goal": goal,
                "conversation": conversation or [],
                "draft": draft,
            },
            adapter=adapter,
            config=CircuitryConfig(default_adapter="scripted", default_model="test"),
            skip_preflight=True,
        )
    )


def _turn_output(result: RunResult) -> dict[str, Any]:
    """Read the turn contract off the state, using the manifest's paths."""
    decide = result.state["prime"]["turn"]["decide"]
    check = decide.get("check") or {}
    return {
        "say": decide["respond"]["value"]["say"],
        "yaml": check.get("value", {}).get("yaml") if check else None,
        "done": decide["done"]["value"],
        "valid": check.get("value", {}).get("ok") if check else None,
        "errors": check.get("value", {}).get("errors") if check else None,
    }


def _assert_compiles(yaml_text: str) -> None:
    """The bar the issue sets: schema-clean AND compilable."""
    plugin = ValidateYamlPlugin()
    verdict = plugin.execute(params={"yaml": yaml_text}).value
    assert verdict["ok"], verdict["errors"]
    compile_orchestration(orch=yaml.safe_load(yaml_text), root_name="prime")


def _drafting_adapter(*, drafts: list[str], repairs: list[str] | None = None):
    replies = {
        INTERPRET_MARKER: [_interpretation()],
        DECIDE_MARKER: ["no"],
        DRAFT_MARKER: drafts,
    }
    if repairs is not None:
        replies[REPAIR_MARKER] = repairs
    return ScriptedAdapter(replies=replies)


# ── Turn contract ────────────────────────────────────────────────────────────


def test_question_turn_asks_and_never_finishes() -> None:
    """When the wizard chooses to ask, it emits no YAML and cannot be done."""
    adapter = ScriptedAdapter(
        replies={
            INTERPRET_MARKER: [_interpretation(ready=False)],
            DECIDE_MARKER: ["yes"],
            QUESTION_MARKER: [
                _json_turn(
                    say="What should the orchestration do with the summary?",
                    yaml_text=None,
                    done=False,
                )
            ],
        }
    )

    result = _run_turn(adapter, goal="Something with articles.")

    assert result.ok, result.error
    out = _turn_output(result)
    assert out["say"].endswith("?")
    assert out["yaml"] is None
    assert out["done"] is False
    # Nothing was drafted, so nothing was validated.
    assert out["valid"] is None
    assert adapter.prompts_matching(DRAFT_MARKER) == []


def test_question_turn_cannot_claim_done() -> None:
    """The question branch's schema pins done to false — a model that says
    otherwise fails the effect rather than ending the conversation."""
    adapter = ScriptedAdapter(
        replies={
            INTERPRET_MARKER: [_interpretation(ready=False)],
            DECIDE_MARKER: ["yes"],
            QUESTION_MARKER: [
                _json_turn(say="All finished!", yaml_text=None, done=True)
            ],
        }
    )

    result = _run_turn(adapter)

    assert result.ok is False
    assert "Schema validation failed" in (result.error or "")


@pytest.mark.parametrize(
    "shape, target",
    [("simple chain", SIMPLE_CHAIN), ("loop+conditional", LOOP_CONDITIONAL_PIPELINE)],
)
def test_draft_turn_produces_a_valid_orchestration(shape: str, target: str) -> None:
    """Both target shapes come out of a turn valid, first try, unrevised."""
    adapter = _drafting_adapter(
        drafts=[_json_turn(say=f"Built a {shape}.", yaml_text=target, done=True)]
    )

    result = _run_turn(adapter)

    assert result.ok, result.error
    out = _turn_output(result)
    assert out["valid"] is True
    assert out["errors"] == []
    assert out["done"] is True
    _assert_compiles(out["yaml"])
    assert adapter.prompts_matching(REPAIR_MARKER) == []


def test_draft_turn_strips_fences_before_returning_yaml() -> None:
    """A fenced draft is cleaned by the validator, and the cleaned document —
    not the model's raw string — is what the turn hands back."""
    fenced = f"```yaml\n{SIMPLE_CHAIN}```"
    adapter = _drafting_adapter(
        drafts=[_json_turn(say="Built it.", yaml_text=fenced, done=True)]
    )

    result = _run_turn(adapter)

    out = _turn_output(result)
    assert "```" not in out["yaml"]
    assert out["valid"] is True
    _assert_compiles(out["yaml"])


def test_draft_turn_carries_conversation_and_draft_into_the_prompts() -> None:
    """Accumulated state reaches the model — the host's only job is to pass it."""
    adapter = _drafting_adapter(
        drafts=[_json_turn(say="Revised.", yaml_text=SIMPLE_CHAIN, done=False)]
    )

    _run_turn(
        adapter,
        conversation=[
            {"role": "user", "content": "It should also translate the summary."},
            {"role": "wizard", "content": "Into which language?"},
        ],
        draft="effects: []\n",
    )

    drafted = adapter.prompts_matching(DRAFT_MARKER)[0]
    assert "[user] It should also translate the summary." in drafted
    assert "[wizard] Into which language?" in drafted
    assert "effects: []" in drafted


# ── The revision loop ────────────────────────────────────────────────────────


def test_invalid_draft_is_revised_not_surfaced() -> None:
    """The failing draft never reaches the host; the repair does."""
    adapter = _drafting_adapter(
        drafts=[_json_turn(say="Built it.", yaml_text=INVALID_DRAFT, done=True)],
        repairs=[_json_turn(say="Built it.", yaml_text=SIMPLE_CHAIN, done=True)],
    )

    result = _run_turn(adapter)

    assert result.ok, result.error
    out = _turn_output(result)
    assert out["valid"] is True
    assert out["done"] is True
    assert "1_bad_name" not in out["yaml"]
    _assert_compiles(out["yaml"])

    # Exactly one repair pass, and it was told what was wrong.
    repairs = adapter.prompts_matching(REPAIR_MARKER)
    assert len(repairs) == 1
    assert "1_bad_name" in repairs[0]
    assert "$.effects[0].name" in repairs[0]
    assert "template" in repairs[0]


def test_revision_loop_is_bounded_and_refuses_to_finish() -> None:
    """When every repair fails, the loop stops at max_iterations and the done
    gate stays false however emphatically the model claims otherwise."""
    adapter = _drafting_adapter(
        drafts=[_json_turn(say="Built it.", yaml_text=INVALID_DRAFT, done=True)],
        repairs=[_json_turn(say="Fixed it.", yaml_text=INVALID_DRAFT, done=True)],
    )

    result = _run_turn(adapter)

    assert result.ok, result.error
    out = _turn_output(result)
    assert len(adapter.prompts_matching(REPAIR_MARKER)) == _max_iterations()
    assert out["valid"] is False
    assert out["errors"]
    assert out["done"] is False


def test_empty_yaml_counts_as_invalid_and_triggers_revision() -> None:
    """A draft turn that produces no YAML is a defect the loop repairs, not a
    result the host has to detect."""
    adapter = _drafting_adapter(
        drafts=[_json_turn(say="Here you go.", yaml_text="", done=True)],
        repairs=[_json_turn(say="Here you go.", yaml_text=SIMPLE_CHAIN, done=True)],
    )

    result = _run_turn(adapter)

    out = _turn_output(result)
    assert out["valid"] is True
    assert out["done"] is True
    assert len(adapter.prompts_matching(REPAIR_MARKER)) == 1


def test_done_gate_is_false_when_the_model_is_not_finished() -> None:
    """A valid draft the model calls unfinished keeps the conversation open."""
    adapter = _drafting_adapter(
        drafts=[_json_turn(say="First pass.", yaml_text=SIMPLE_CHAIN, done=False)]
    )

    result = _run_turn(adapter)

    out = _turn_output(result)
    assert out["valid"] is True
    assert out["done"] is False


# ── File-level contracts ─────────────────────────────────────────────────────


def _wizard() -> dict[str, Any]:
    return yaml.safe_load(WIZARD_PATH.read_text(encoding="utf-8"))


def _draft_branch() -> list[dict[str, Any]]:
    decide = _wizard()["effects"][0]["effects"][1]
    return decide["else"]


def _max_iterations() -> int:
    loop = next(e for e in _draft_branch() if e["type"] == "loop")
    return int(loop["max_iterations"])


def test_wizard_embeds_the_prime_verbatim() -> None:
    """The YAML stands alone, so it carries its own copy of the prime. This is
    the drift guard between that copy and core.primes."""
    used = [
        effect["inputs"]["wizard_prime"]
        for effect in _walk_prompts(_wizard())
        if effect.get("inputs", {}).get("wizard_prime")
    ]
    assert used, "no prompt injects wizard_prime"
    for text in used:
        assert text == WIZARD_PRIME_V1


def test_prime_is_injected_unescaped() -> None:
    """A double-stache would HTML-escape the cheat-sheet's quotes and angle
    brackets into nonsense — every injection must be a triple-stache."""
    for effect in _walk_prompts(_wizard()):
        if effect.get("inputs", {}).get("wizard_prime"):
            assert "{{{wizard_prime}}}" in effect["template"]


def test_yaml_bearing_state_paths_are_injected_unescaped() -> None:
    """Same trap, higher stakes: an escaped draft can never parse."""
    import re

    text = WIZARD_PATH.read_text(encoding="utf-8")
    for path in (
        "prime.turn.decide.respond.value.yaml",
        "prime.turn.decide.check.value.yaml",
        "prime.turn.decide.check.value.errors",
    ):
        uses = re.findall(r"\{{2,3}" + re.escape(path) + r"\}{2,3}", text)
        assert uses, f"{path} is never interpolated"
        for use in uses:
            assert use.startswith("{{{"), f"{path} is interpolated escaped: {use}"


def test_revision_loop_is_unnamed_so_its_condition_can_see_itself() -> None:
    """A named loop buries each pass under iter_<N>, out of reach of the CEL
    condition. Naming this loop would silently make it run to max_iterations."""
    loop = next(e for e in _draft_branch() if e["type"] == "loop")
    assert "name" not in loop
    assert loop["while"]["mode"] == "cel"
    assert loop["max_iterations"] >= 1
    body_names = [e["name"] for e in loop["body"]]
    assert body_names == ["respond", "check"]


def test_done_gate_is_deterministic() -> None:
    """`done` is decided by CEL over the validation result, never by a prompt."""
    gate = _draft_branch()[-1]
    assert gate["type"] == "if"
    assert gate["if"]["mode"] == "cel"
    assert "check.value.ok == true" in gate["if"]["expr"]
    assert "respond.value.done == true" in gate["if"]["expr"]
    for branch in ("then", "else"):
        (effect,) = gate[branch]
        assert effect["type"] == "tool"
        assert effect["name"] == "done"
    assert gate["then"][0]["params"]["input"] == "true"
    assert gate["else"][0]["params"]["input"] == "false"


def test_interface_outputs_match_the_manifest() -> None:
    import json

    manifest = json.loads(
        Path("src/circuitry/curation/manifest.json").read_text(encoding="utf-8")
    )
    entry = next(e for e in manifest["entries"] if e["name"] == "agents/wizard")
    declared = _wizard()["interface"]["outputs"]

    assert set(entry["outputs"]) == set(declared)
    for name, spec in entry["outputs"].items():
        assert spec["path"] == declared[name]["path"]
    assert set(entry["inputs"]) == set(_wizard()["interface"]["inputs"])


def _walk_prompts(node: Any) -> list[dict[str, Any]]:
    """Every prompt effect in the tree, at any depth."""
    found: list[dict[str, Any]] = []
    if isinstance(node, dict):
        if node.get("type") == "prompt":
            found.append(node)
        for key in ("effects", "then", "else", "body"):
            found.extend(_walk_prompts(node.get(key)))
    elif isinstance(node, list):
        for item in node:
            found.extend(_walk_prompts(item))
    return found


# ── WIZARD_PRIME covers the schema surface ───────────────────────────────────


def _schema() -> dict[str, Any]:
    import json

    return json.loads(
        Path("src/circuitry/schema/orchestration.schema.json").read_text(
            encoding="utf-8"
        )
    )


def test_prime_documents_every_effect_type() -> None:
    defs = _schema()["$defs"]
    types = defs["EffectDef"]["else"]["else"]["else"]["else"]["else"]["else"]["else"][
        "properties"
    ]["type"]["enum"]
    for effect_type in types:
        assert f" {effect_type} " in WIZARD_PRIME_V1 or f"{effect_type} —" in (
            WIZARD_PRIME_V1
        ), f"WIZARD_PRIME does not document effect type {effect_type!r}"


def test_prime_documents_the_naming_rule_and_reserved_names() -> None:
    pattern = _schema()["$defs"]["NamePattern"]["allOf"][0]["pattern"]
    assert pattern in WIZARD_PRIME_V1
    assert "iter_" in WIZARD_PRIME_V1


def test_prime_documents_prompt_types_and_flows() -> None:
    defs = _schema()["$defs"]
    for prompt_type in defs["PromptEffect"]["properties"]["prompt_type"]["enum"]:
        assert prompt_type in WIZARD_PRIME_V1
    for flow in defs["FlowModel"]["enum"]:
        assert flow in WIZARD_PRIME_V1
    for policy in defs["OnErrorLoop"]["enum"]:
        assert policy in WIZARD_PRIME_V1


def test_prime_documents_the_interface_block() -> None:
    interface = _schema()["properties"]["interface"]["properties"]
    assert "interface:" in WIZARD_PRIME_V1
    for key in interface:
        assert f"{key}:" in WIZARD_PRIME_V1
    assert "path:" in WIZARD_PRIME_V1


def test_prime_forbids_fences_and_multiple_documents() -> None:
    assert "NO markdown fences" in WIZARD_PRIME_V1
    assert "ONE YAML document" in WIZARD_PRIME_V1
