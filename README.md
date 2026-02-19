# Circuitry

Circuitry is a deterministic orchestration framework for AI model invocations. It provides a declarative YAML-based DSL for composing prompts, dynamics, conditionals, loops, and reflectors into auditable execution plans.

## Core Concepts

### State

State is the single source of truth. It is a hierarchical, serializable structure containing **domain state** and **runtime state**. Circuitry writes only to runtime state; domain state is never mutated implicitly.

### Prompt

A Prompt is the atomic execution unit. It performs exactly one model invocation, produces a typed result, and writes both its value and execution metadata to state at a deterministic path.

```yaml
effects:
  - type: prompt
    name: greet_user
    template: "Say hello to {{input.user_name}} in one sentence."
```

### Dynamic

A Dynamic is a named execution structure that composes Prompts, Conditionals, Loops, and other Dynamics. It defines control flow topology and aggregates execution metadata.

```yaml
effects:
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

### Flow Models

Dynamics support explicit control flow models:

- **chain** (aliases: `chain_of_thought`, `cot`) - Sequential execution
- **tree** (aliases: `tree_of_thought`, `tot`) - Parallel execution

### Conditional

A Conditional evaluates an `if` condition and selects exactly one branch to execute.

```yaml
effects:
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
- **model** - Cybernetic evaluation using a model invocation
- **cel** - Deterministic evaluation using CEL expressions

### Loop

A Loop repeatedly executes effects while a condition is true or over a collection.

```yaml
effects:
  - type: loop
    name: process_items
    each:
      in: state.input.items
      as: item
    body:
      - type: prompt
        name: process
        template: "Process item: {{item}}"
```

Loop modes:
- **while** - Continue while condition is true (model or CEL)
- **each** - Iterate over a collection

### Reflector

A Reflector is an optional planning component that reads state, applies heuristics, and produces Prime Dynamics. Reflectors do not execute effects directly.

```yaml
effects:
  - type: reflector
    name: planner
    max_effects: 5
    effects:
      - type: prompt
        name: propose_steps
        template: "Generate next steps based on: {{prime.goal.value}}"
```

### Prompt Types

Prompts support typed outputs with optional schema validation:

```yaml
effects:
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

Available types: `text`, `json`, `boolean`, `number`, `array`, `object`, `tool`

## Installation

```bash
pip install -e .
```

## Usage

### Quick Start (2 Minutes)

Use a dry run first to verify install and runtime wiring without requiring live model calls.

Dry-run expectations:
- No network/model provider required
- Validates orchestration loading, compile path, and deterministic state writes

Live-run expectations:
- Adapter/provider credentials must be configured where applicable (`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`)
- Ollama must be running locally if using default runtime config

Dry run command:

```bash
circuitry run examples/hello.yml --dry-run --out out.json --pretty
```

Expected result:
- Command exits successfully
- `out.json` contains `runtime.last_run`, `runtime.effective_settings`, and `prime.*` state keys

Common setup pitfalls:
- `circuitry: command not found`
  - run via module: `python -m circuitry.cli.app run examples/hello.yml --dry-run`
- Import/runtime dependency issues
  - install deps: `pip install -e . && pip install -r requirements-dev.txt`

Quick verification checklist:
1. `circuitry run examples/hello.yml --dry-run` exits with code `0`.
2. `out.json` (or stdout state) shows `prime.say_hello.value`.
3. Programmatic snippet below returns `result.ok == True`.
4. Optional live run (`circuitry run examples/hello.yml`) works after adapter setup.

### CLI

```bash
# Recommended first run (no model invocations)
circuitry run examples/hello.yml --dry-run

# Run with live model invocation
circuitry run examples/hello.yml

# With verbose output
circuitry run examples/hello.yml --verbose
```

### Programmatic

```python
from circuitry import run_orchestration

result = run_orchestration(
    orchestration_path="examples/hello.yml",
    state={"input": {"user_name": "World"}},
    dry_run=True,
)
print(result.ok)                 # True/False
print(result.state["runtime"])   # runtime metadata + outputs
```

Additional embedded API surface:
- `run_shared_orchestration` for shared-library assets
- `validate_orchestration` for compiler-backed validation
- `inspect_orchestration` for orchestration metadata
- `inspect_divergence_paths` for deterministic failure-path diagnostics

See `docs/api-reference.md` for signatures and integration guidance.

### Multi-Provider Runtime Configuration

Configure provider adapters in `config.json` under `runtime.adapters` and set defaults:

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
- Runtime: orchestration `runtime` overrides config `runtime` (shallow merge)

Every run records the resolved values and sources in `runtime.effective_settings`.

## Examples

See the `examples/` directory for sample orchestration files:

- `hello.yml` - Simple single prompt
- `dynamic_hello.yml` - Sequential prompts with interpolation
- `conditional_example.yml` - Branching with CEL conditions
- `loop_example.yml` - Collection iteration
- `typed_prompt_example.yml` - Typed prompts with JSON schema
- `reflector_v1.yml` - Reflector-driven planning
- `multi_primitive_story.yml` - Combined dynamic + loop + conditional composition

Curated examples index, expected outputs, and compatibility/versioning notes:
- `examples/README.md`
- `examples/manifest.json`

### State Path Walkthrough

Runtime state paths follow orchestration names under `prime`.

- Prompt: `prime.say_hello.value`
- Nested dynamic prompt: `prime.onboarding.ask_name.value`
- Named loop iteration prompt: `prime.explain_topics.iter_0.explain.value`

Use CLI output state files to inspect paths:

```bash
circuitry run examples/loop_example.yml --out out.json --pretty
```

## Architecture

```
State + Prime Dynamic --> Prime --> Runtime --> Updated State
                           |
                           v
                    Adapter Layer --> Model Provider
```

- **Compiler** - Parses YAML into definition objects
- **Runtime** - Executes definitions deterministically
- **Store** - Manages hierarchical state
- **Adapters** - Translate to provider-specific APIs

## Design Principles

1. **Deterministic Execution** - Given the same inputs, execution follows the same path
2. **Explicit Control Flow** - No implicit branching or reasoning
3. **Full Auditability** - All effects and metadata recorded to state
4. **Separation of Concerns** - Planning (Reflectors) vs Execution (Runtime)
5. **Model Agnostic** - Adapters abstract provider differences

## License

MIT
