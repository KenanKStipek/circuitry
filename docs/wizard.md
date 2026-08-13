# The Wizard — building orchestrations by talking

`curation/agents/wizard.yml` is the orchestration that writes orchestrations.
You describe what you want; it either asks you something or hands back a draft
of the target YAML. It never hands back YAML that has not been validated.

The orchestration handles **one turn**. The host owns the conversation: it keeps
the transcript and the current draft, and re-runs the orchestration per turn with
the updated state. That split is deliberate — a turn is a pure function of the
conversation so far, so a CLI, a TUI chat view, and a test harness all drive it
the same way.

```
state {goal, conversation, draft}  →  wizard.yml  →  {say, yaml, done, valid, errors}
```

## The turn contract

| Output   | State path                              | Meaning |
| -------- | --------------------------------------- | ------- |
| `say`    | `prime.turn.decide.respond.value.say`   | What to show the human this turn. |
| `yaml`   | `prime.turn.decide.check.value.yaml`    | The validated draft; `null` on a question turn. |
| `done`   | `prime.turn.decide.done.value`          | `true` only when the wizard is finished **and** the draft is valid. |
| `valid`  | `prime.turn.decide.check.value.ok`      | Whether this turn's draft validated; `null` on a question turn. |
| `errors` | `prime.turn.decide.check.value.errors`  | Validator errors, if any survived the revision loop. |

Inputs are `goal` (string, required), `conversation` (array of `{role, content}`,
oldest first, empty on the first turn), and `draft` (the current YAML, or `""`).

## Driving it headlessly

The whole host is this loop:

```python
from circuitry import run_orchestration
from circuitry.cli.config import find_config_path, load_config

WIZARD = "src/circuitry/curation/agents/wizard.yml"
config = load_config(find_config_path(explicit_path=None, cwd="."))


def dig(state, path):
    cursor = state
    for segment in path.split("."):
        if not isinstance(cursor, dict) or segment not in cursor:
            return None
        cursor = cursor[segment]
    return cursor


conversation, draft = [], ""
goal = "Summarize an article, then translate the summary"

while True:
    result = run_orchestration(
        orchestration_path=WIZARD,
        state={"goal": goal, "conversation": conversation, "draft": draft},
        config=config,
    )
    say = dig(result.state, "prime.turn.decide.respond.value.say")
    yaml_text = dig(result.state, "prime.turn.decide.check.value.yaml")
    done = bool(dig(result.state, "prime.turn.decide.done.value"))

    print("wizard:", say)
    if yaml_text:
        draft = yaml_text          # only ever a validated document
    conversation.append({"role": "wizard", "content": say})
    if done:
        break

    reply = input("you: ")
    conversation.append({"role": "user", "content": reply})

print(draft)
```

`scripts/wizard-chat` is that loop, ready to run:

```sh
scripts/wizard-chat --goal "Summarize an article, then translate the summary" \
                    --out my_orch.yml
cof check my_orch.yml
```

Pass `--reply answers.txt` (one reply per line) to drive it without a terminal —
that is how the transcript below is reproduced.

The TUI's [Chat view](./tui.md#chat-8--build-an-orchestration-by-talking-to-it)
(`cof tui`, then `8`) is the same loop with a screen around it:
`circuitry.tui.wizard_host` holds the transcript, re-validates every draft, and
owns the two save paths. Nothing in the loop is specific to either host.

## Inside a turn

```
interpret → decide: ask or draft → [draft branch] validate → revise (≤3) → done gate
```

1. **interpret** — reads the transcript and reports the intent, what is still
   unknown, and whether it could draft right now. Separate from the answer so the
   branch below has something concrete to weigh.
2. **decide** — a model conditional. One question, or a draft? The prompt is
   biased toward drafting: a draft the human can react to beats an interrogation.
3. **Question branch** — emits the question. Its schema pins `yaml` to `null`
   and `done` to `false`, so a question turn cannot end the session.
4. **Draft branch** — emits `{say, yaml, done}`, then validates the YAML with the
   `validate_yaml` tool (YAML parse → JSON Schema → compiler).
5. **Revision loop** — while the draft is invalid, feed the errors and the exact
   rejected document back for repair and re-validate. Bounded at 3 passes. The
   human never sees the rejected drafts.
6. **Done gate** — a CEL conditional plus a `json` tool. `done` is true only when
   the model claims completion *and* the final draft validated.

Two design notes worth keeping in mind if you edit the file:

- **The revision loop is unnamed on purpose.** A named loop writes each pass to
  `prime.<loop>.iter_<N>.<effect>.value`, where the `while` condition cannot
  reach it — the loop would run to `max_iterations` every time. An unnamed loop
  merges its body into the parent scope, so each pass overwrites `check` at a
  stable path and the CEL condition observes the loop's own latest result.
- **YAML-bearing paths are interpolated with a triple-stache.** `{{x}}` HTML-
  escapes, which turns every quote in a draft into `&quot;` and guarantees a
  parse failure. `{{{x}}}` inserts the raw string.

`WIZARD_PRIME_V1` (in `circuitry/core/primes.py`) is the DSL cheat-sheet the
drafting prompts carry: the seven primitives, the naming rules, the state-path
grammar, the interface block. The wizard YAML embeds it verbatim so the file
stands alone; `tests/orchestrations/test_wizard_agent.py` guards the two copies
against drift and spot-checks the prime against `orchestration.schema.json`.

## The validate_yaml tool

A general-purpose tool provider, not wizard-specific:

```yaml
- type: tool
  name: check
  provider: validate_yaml
  params:
    yaml: "{{{prime.draft.value}}}"
```

| Param          | Default | Meaning |
| -------------- | ------- | ------- |
| `yaml`         | —       | The document to validate. Empty is *invalid*, not an error. |
| `strip_fences` | `true`  | Strip markdown fences and stray `---` before parsing. |
| `compile`      | `true`  | Run the compiler too — catches duplicate names, reserved `iter_N` names, and per-type required fields the schema cannot express. |
| `max_errors`   | `20`    | Cap on reported errors, so a cascade cannot swamp the prompt it feeds. |

It writes `{ok, errors, yaml}`, where `yaml` echoes the cleaned document that was
actually validated — that is what a revision prompt feeds back to the model, and
what the wizard hands to the host.

## Demo

A scripted three-turn conversation (asserted end-to-end in
`test_scripted_conversation_converges_on_a_valid_orchestration`):

```
goal    Draft three sections about a brief, score them, and branch on the score.

wizard  How many sections should it draft, and what should happen when the
        review score is low?
you     Three sections. If the score is 7 or below, list what to fix.

wizard  Built a three-section pipeline: plan the topics, draft each in a loop,
        score the result, then branch on the score.
        (draft: 42 lines, valid=True)
        [internal: 1 revision pass, never surfaced]
you     That's it. Ship it.

wizard  Unchanged — the pipeline already covers the goal.
        (draft: 42 lines, valid=True)

wizard is done.
```

The second turn's first attempt named a loop `2_sections`, which the schema
rejects. The turn repaired it internally and surfaced only the valid version:

```yaml
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
```

```console
$ cof check sections.yml
Circuitry · Validate
Valid
```
