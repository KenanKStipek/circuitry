from __future__ import annotations

# Default "prime directive" for reflectors.
# Keep this short, strong, and versionable.
REFLECTOR_PRIME_V1 = """\
CRITICAL: Your output will be parsed by a YAML parser and then validated as a Circuitry orchestration.

You MUST output ONE valid YAML document, and NOTHING ELSE.
Do NOT wrap in ``` fences.
Do NOT output '---' or multiple YAML documents.
Do NOT include any markdown formatting (no **, *, backticks, headings, bullet prose).

OUTPUT SHAPE (required):
done: <true|false>
effects:
  - type: prompt
    name: <snake_case>
    template: <string>
  - type: dynamic
    name: <snake_case>
    flow: chain
    effects: [ ... ]
  - type: use
    name: <snake_case>
    orchestration: <name_or_path>
    inputs: {{ ... }}

HARD RULES:
- The top-level YAML MUST be a dict with these keys: done, effects.
- effects MUST be a YAML list.
- Each effect MUST be a YAML dict containing at minimum:
  - type (one of: prompt, dynamic, loop, if, use, tool)
  - name (snake_case, letters/numbers/underscore only)
  - AND the required fields for that type:
    - prompt: template (non-empty string)
    - dynamic: effects (a list), optional flow (chain|tree)
    - loop: body (a list of effects), plus each or while
    - if: if (condition), then (effects list)
    - use: orchestration (name/path) or inline (YAML template)
    - tool: provider (plugin name)
- NEVER emit alternative schemas such as:
  - plan:
  - plan: {{ effects: ... }}
  - step_1:
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
- Use `use` to invoke existing orchestrations by name.

EXAMPLE (this is the exact style you must follow; do not copy the content literally):
done: false
effects:
  - type: prompt
    name: clarify_requirements
    template: "Ask 3 short questions to clarify the user's goal and constraints."

  - type: dynamic
    name: draft_plan
    flow: chain
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
