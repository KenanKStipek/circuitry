from __future__ import annotations

# Default "prime directive" for reflectors.
# Keep this short, strong, and versionable.
REFLECTOR_PRIME_V1 = """\
CRITICAL: Your output will be parsed by a YAML parser and then validated as Circuitry effects.

You MUST output ONE valid YAML document, and NOTHING ELSE.
Do NOT wrap in ``` fences.
Do NOT output '---' or multiple YAML documents.
Do NOT include any markdown formatting (no **, *, backticks, headings, bullet prose).

OUTPUT SHAPE (required):
done: <true|false>
effects:
  - type: prompt|dynamic|reflector
    name: <snake_case>
    template: <string>        # required if type: prompt
  - type: dynamic|reflector
    name: <snake_case>
    effects: [ ... ]          # required if type: dynamic or reflector

HARD RULES:
- The top-level YAML MUST be a dict with exactly these keys: done, effects.
- effects MUST be a YAML list.
- Each effect MUST be a YAML dict containing:
  - type (one of: prompt, dynamic, reflector)
  - name (snake_case, letters/numbers/underscore only)
  - AND:
    - if type == prompt: template (non-empty string)
    - if type in (dynamic, reflector): effects (a list; may be empty)
- NEVER emit alternative schemas such as:
  - plan:
  - plan: { effects: ... }
  - step_1:
  - validate_input:
  - id/action/description-only effects
- NEVER include '*' characters outside of a YAML quoted string (single or double quotes).
- Keep descriptions inside YAML strings only.

CONTEXT:
Goal:
{goal}

Additional Context (may be empty):
{context}

TASK:
Generate Circuitry effects to advance the Goal.
- Keep the total number of top-level effects <= {max_effects}.
- Prefer a small number of high-leverage effects.
- Use prompts to ask for missing info or to produce artifacts.
- Use dynamics to group related prompts.

EXAMPLE (this is the exact style you must follow; do not copy the content literally):
done: false
effects:
  - type: prompt
    name: clarify_requirements
    template: "Ask 3 short questions to clarify the user's goal and constraints."

  - type: dynamic
    name: draft_plan
    effects:
      - type: prompt
        name: propose_architecture
        template: "Propose a minimal architecture and key components."

      - type: prompt
        name: define_milestones
        template: "List 3 milestones with acceptance criteria."

  - type: prompt
    name: summarize_next_actions
    template: "Summarize next actions in 5 bullets inside a single YAML string."

END. Output YAML only.
"""
