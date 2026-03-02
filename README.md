# Circuitry

A cybernetic orchestration framework for AI systems.

Circuitry structures AI model invocations as closed-loop control systems. Each effect writes to a deterministic state path, and downstream effects observe that state through interpolation to decide what happens next. This creates genuine feedback: loops that re-evaluate continuation based on what the model just produced, conditionals that branch on accumulated output, and reflectors that plan from observed state.

## How It Works

An orchestration is a YAML file declaring **effects** that execute in order. Each effect writes its output to a **deterministic state path** derived from its name. Subsequent effects read that state through **template interpolation**, creating feedback chains.

```yaml
adapter: ollama
model: llama3
effects:
  - type: prompt
    name: draft
    template: "Write a short intro for {{input.topic}}."

  - type: prompt
    name: critique
    template: "Critique this draft and suggest improvements: {{prime.draft.value}}"

  - type: prompt
    name: revise
    template: |
      Original: {{prime.draft.value}}
      Feedback: {{prime.critique.value}}
      Write the final version incorporating the feedback.
```

The second prompt reads the first prompt's output. The third reads both. Each step adapts based on what came before — this is the feedback loop that makes orchestrations cybernetic rather than static pipelines.

## Core Primitives

### Prompt

The atomic unit. One model invocation, one typed result, written to state at a deterministic path.

```yaml
- type: prompt
  name: greet_user
  template: "Say hello to {{input.user_name}} in one sentence."
```

State path: `prime.greet_user.value`

### Dynamic

A named scope that composes effects with explicit control flow topology.

```yaml
- type: dynamic
  name: onboarding
  flow: chain
  effects:
    - type: prompt
      name: ask_name
      template: "What is your name?"
    - type: prompt
      name: greet
      template: "Nice to meet you, {{onboarding.ask_name.value}}!"
```

Flow models:
- **chain** (aliases: `chain_of_thought`, `cot`) — sequential, each effect sees prior state
- **tree** (aliases: `tree_of_thought`, `tot`) — parallel execution

### Conditional

Cybernetic branching. The system inspects its own state and selects a path.

```yaml
- type: if
  name: check_role
  if:
    mode: cel
    expr: "state.input.role == 'admin'"
  then:
    - type: prompt
      name: admin_view
      template: "Render admin panel."
  else:
    - type: prompt
      name: user_view
      template: "Render user panel."
```

Evaluation modes:
- **model** — the LLM reads state and decides the branch (cybernetic evaluation)
- **cel** — deterministic evaluation using CEL expressions

### Loop

Cybernetic iteration. The loop body executes, writes state, and the continuation condition evaluates against that new state.

```yaml
- type: loop
  name: refine
  while:
    mode: model
    condition: "Is the draft still below quality threshold? {{prime.refine.iter_{{_iter}}.improve.value}}"
  body:
    - type: prompt
      name: improve
      template: "Improve this draft: {{prime.refine.iter_{{_prev_iter}}.improve.value}}"
```

A `while` loop with `mode: model` is the purest cybernetic primitive — the model observes accumulated state and decides whether to keep going.

Loop modes:
- **while** — continue while condition holds (model or CEL evaluated)
- **each** — iterate over a collection, optionally in parallel (`flow: tree`)

Loops support `collect` to aggregate body outputs across iterations:

```yaml
- type: loop
  name: process_items
  each:
    in: state.input.items
    as: item
  collect: process
  body:
    - type: prompt
      name: process
      template: "Process item: {{item}}"
```

Collected at: `prime.process_items.collected.value` (array)

### Reflector

A planning component that reads state and produces execution plans. Reflectors observe the system's current state and generate dynamics — the highest-level cybernetic feedback.

```yaml
- type: reflector
  name: planner
  max_effects: 5
  effects:
    - type: prompt
      name: propose_steps
      template: "Generate next steps based on: {{prime.goal.value}}"
```

### Tool

Extends orchestrations beyond model invocations. Tool effects invoke external systems through plugins.

```yaml
- type: tool
  name: render_video
  provider: ffmpeg
  params:
    input: "{{prime.generate_frames.value}}"
    output: "./output/video.mp4"
```

Built-in providers: `ffmpeg`, `comfyui`

### Prompt Types

Prompts support typed outputs with optional schema validation:

```yaml
- type: prompt
  name: extract_data
  prompt_type: json
  schema:
    type: object
    properties:
      items:
        type: array
        items:
          type: string
    required: [items]
  template: "Extract items as JSON..."
```

Available types: `text`, `json`, `boolean`, `number`, `array`, `object`

## State Paths

Every effect writes to a deterministic path derived from its name. This is what makes interpolation — and therefore cybernetic feedback — reliable.

```
prime.<effect>.value                              # top-level prompt
prime.<dynamic>.<effect>.value                    # inside a dynamic
prime.<loop>.iter_<n>.<effect>.value              # inside a loop iteration
prime.<loop>.collected.value                      # loop collect aggregation
```

Inspect paths from any run:

```bash
circuitry run orchestrations/loop_example.yml --out out.json --pretty
```

## Installation

```bash
pip install -e .
```

## Quick Start

Dry-run first to verify install without requiring a live model:

```bash
circuitry run orchestrations/hello.yml --dry-run --out out.json --pretty
```

Expected result:
- Exits with code `0`
- `out.json` contains `runtime.last_run`, `runtime.effective_settings`, and `prime.*` state keys

Then run live (requires a configured adapter):

```bash
circuitry run orchestrations/hello.yml
```

### CLI

```bash
# Dry run (no model calls)
circuitry run orchestrations/hello.yml --dry-run

# Live run
circuitry run orchestrations/hello.yml

# Verbose output
circuitry run orchestrations/hello.yml --verbose
```

### Programmatic

```python
from circuitry import run_orchestration

result = run_orchestration(
    orchestration_path="orchestrations/hello.yml",
    state={"input": {"user_name": "World"}},
    dry_run=True,
)
print(result.ok)                 # True/False
print(result.state["runtime"])   # runtime metadata + outputs
```

Additional embedded API:
- `run_shared_orchestration` — shared-library assets
- `validate_orchestration` — compiler-backed validation
- `inspect_orchestration` — orchestration metadata
- `inspect_divergence_paths` — deterministic failure-path diagnostics

See `docs/api-reference.md` for signatures and integration guidance.

### Multi-Provider Configuration

Configure adapters in `config.json`:

```json
{
  "default_adapter": "openai",
  "default_model": "gpt-4o-mini",
  "runtime": {
    "adapters": {
      "ollama": {
        "base_url": "http://localhost:11434",
        "timeout_seconds": 120
      },
      "openai": {
        "base_url": "https://api.openai.com/v1",
        "default_model": "gpt-4o-mini",
        "timeout_seconds": 60
      },
      "anthropic": {
        "base_url": "https://api.anthropic.com",
        "default_model": "claude-sonnet-4-20250514",
        "max_tokens": 4096
      },
      "litellm": {
        "default_model": "openai/gpt-4o-mini",
        "timeout": 120
      }
    }
  }
}
```

Selection precedence:
- Model: CLI override > orchestration `model` > config `default_model`
- Adapter: CLI override > orchestration `adapter` > config `default_adapter`

Every run records resolved values in `runtime.effective_settings`.

## Orchestration Library

See `orchestrations/` for pre-built examples:

- `hello.yml` — single prompt
- `dynamic_hello.yml` — sequential prompts with interpolation
- `conditional_example.yml` — branching with CEL conditions
- `loop_example.yml` — collection iteration
- `typed_prompt_example.yml` — typed prompts with JSON schema
- `reflector_v1.yml` — reflector-driven planning
- `multi_primitive_story.yml` — dynamic + loop + conditional composition
- `meta_orchestrator.yml` — generate new orchestrations from natural language

Full index: `orchestrations/README.md` and `orchestrations/manifest.json`

## Architecture

```
Orchestration YAML
       |
    Compiler ──> Definition Objects
       |
    Runtime ──> Effect Execution ──> State Writes
       |              |
    Store        Adapter Layer ──> Model Provider
  (feedback)
```

- **Compiler** — parses YAML into typed definition objects with JSON Schema validation
- **Runtime** — executes definitions, manages state feedback between effects
- **Store** — hierarchical state with deterministic path resolution
- **Adapters** — normalize provider transport (Ollama, OpenAI, Anthropic, LiteLLM)
- **Plugins** — tool effect providers (ffmpeg, ComfyUI) and runtime lifecycle hooks

## Design Principles

1. **Cybernetic Feedback** — effects observe state written by prior effects and adapt; the system steers itself
2. **Deterministic State Paths** — orchestration structure maps to known state keys; feedback is reliable because paths are predictable
3. **Explicit Control Flow** — no implicit branching or hidden reasoning; topology is declared in YAML
4. **Full Auditability** — every effect, branch decision, and iteration is recorded to state
5. **Model Agnostic** — adapters abstract provider differences; orchestrations are portable

## License

MIT
