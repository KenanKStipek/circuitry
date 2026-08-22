"""A scripted adapter that plays the wizard, so the chat view can be driven end to end.

The chat view is adapter-agnostic by construction — it hands
``api.run_orchestration`` whatever adapter it was given. That makes a full
conversation testable for real: this adapter answers the three model calls one
wizard turn makes (interpret, ask-or-draft, respond) from a script, and
everything else in the turn — the ``validate_yaml`` tool, the revision loop,
the deterministic ``done`` gate — runs exactly as it does in production.

Dispatch is on markers in the rendered prompts, which is what keeps the script
declarative: a turn is either ``ask("...")`` or ``draft(say, yaml, done)``.
"""

from __future__ import annotations

import json
from dataclasses import dataclass

from circuitry.adapters.base import GenerateResult
from circuitry.preflight import CheckResult

#: Marker in the wizard's `interpret` template. Seeing it means a new turn has
#: begun, which is how the script advances without the host telling it to.
INTERPRET_MARKER = "Decide what is known and what is missing"

#: Every model conditional's prompt opens with this (see core/conditional.py).
CONDITION_MARKER = "Evaluate the following condition"

#: A minimal, genuinely valid orchestration — the wizard's own `validate_yaml`
#: tool and the view's draft pane both have to pass it.
VALID_DRAFT = """# Summarize an article, then translate the summary.
# Inputs: article (string). Primary output: prime.translate.value
effects:
  - type: prompt
    name: summarize
    template: "Summarize this article in three sentences: {{article}}"
  - type: prompt
    name: translate
    template: "Translate to French: {{prime.summarize.value}}"
"""

#: Structurally invalid: `iter_0` is reserved for loop iteration segments, so
#: the schema rejects it. Used to prove the pane goes red and saving is refused.
INVALID_DRAFT = """effects:
  - type: prompt
    name: iter_0
    template: "This name is reserved."
"""


@dataclass(frozen=True)
class ScriptedTurn:
    """One turn of the script: ask a question, or hand back a draft."""

    question: str | None = None
    say: str = ""
    yaml: str | None = None
    done: bool = False

    @property
    def asks(self) -> bool:
        return self.question is not None


def ask(question: str) -> ScriptedTurn:
    return ScriptedTurn(question=question)


def draft(say: str, yaml: str = VALID_DRAFT, *, done: bool = False) -> ScriptedTurn:
    return ScriptedTurn(say=say, yaml=yaml, done=done)


class ScriptedWizardAdapter:
    """An adapter that plays a scripted wizard. Satisfies the Adapter protocol."""

    name = "scripted"

    def __init__(self, turns: list[ScriptedTurn]) -> None:
        self.turns = list(turns)
        self.index = -1
        #: Every prompt the wizard sent, in order — a test can assert the
        #: transcript actually reached the model.
        self.prompts: list[str] = []

    @property
    def turn(self) -> ScriptedTurn:
        return self.turns[min(self.index, len(self.turns) - 1)]

    def generate(
        self, *, model: str, prompt: str, timeout_seconds: int = 120
    ) -> GenerateResult:
        self.prompts.append(prompt)

        if prompt.startswith(CONDITION_MARKER):
            # The ask-or-draft branch. "yes" means ask.
            return GenerateResult(text="yes" if self.turn.asks else "no", raw={})

        if INTERPRET_MARKER in prompt:
            self.index += 1
            return GenerateResult(
                text=json.dumps(
                    {
                        "intent": "Summarize an article, then translate the summary.",
                        "unknowns": [],
                        "ready_to_draft": True,
                        "reason": "The goal is concrete enough to draft.",
                    }
                ),
                raw={},
            )

        turn = self.turn
        if turn.asks:
            payload = {"say": turn.question, "yaml": None, "done": False}
        else:
            payload = {"say": turn.say, "yaml": turn.yaml, "done": turn.done}
        return GenerateResult(text=json.dumps(payload), raw={})

    def check(self) -> CheckResult:
        return CheckResult(ok=True, message="scripted")
