---
stepsCompleted: [1]
inputDocuments:
  - _bmad-output/planning-artifacts/prd.md
  - docs/index.md
  - docs/architecture.md
  - docs/development-guide.md
  - docs/project-overview.md
  - docs/project-scan-sections.md
  - docs/source-tree-analysis.md
  - docs/Circuitry 2c34435ec2e080c89fc0f253880c2612.md
  - docs/Circuitry Terminology 2c94435ec2e0808daeeff76f7ed1ed25.md
  - docs/Circuitry Type System 2f34435ec2e0808394e7ddbb86d14a89.md
  - docs/Conditional Cybernetic 2f34435ec2e08022864fd77c7f2d5b20.md
  - docs/Dynamic 2c94435ec2e0809d87eff08463c9ca97.md
  - docs/Example Orchestration 2f34435ec2e080ba995dcc11dd09047c.md
  - docs/Loop Cybernetic 2f34435ec2e080ea9024ca18becd5df1.md
  - docs/Prompt 2c94435ec2e080feb508d739b3272408.md
  - docs/Reflector 2c94435ec2e080468aa4ebb005c4c635.md
workflowType: 'architecture'
project_name: 'circuitry'
user_name: 'Kenan'
date: '2026-02-18'
---

# Architecture Decision Document

_This document builds collaboratively through step-by-step discovery. Sections are appended as we work through each architectural decision together._

## State Path Contract (MVP)

### Objective

Define and enforce a deterministic mapping from orchestration object names to runtime state paths so every execution record is addressable, inspectable, and stable across runs.

### Scope

- Applies to Prompt, Dynamic, Conditional, Loop, and Reflector execution records.
- Applies to runtime writes under the orchestration root (`prime` in current CLI/runtime implementation).
- Applies to both value and metadata records.

### Canonical State Hierarchy

- Root runtime execution container: `prime`
- Path composition rule: each named orchestration object appends one segment to its parent path.
- Current runtime behavior (from `src/circuitry/core/*` and `out.json`):
  - Prompt under root dynamic: `prime.say_hello`
  - Prompt inside named dynamic: `prime.onboarding.ask_name`
  - Named conditional + selected branch effect: `prime.check_role.admin_greeting`
  - Named loop iteration effect: `prime.explain_topics.iter_0.explain`

### Canonical Path Grammar

```
state.<root>.<named-step>[.<named-step>...][.<dynamic-segment>...]
```

- `root`: orchestration runtime root (`prime`)
- `named-step`: `name` field from orchestration objects (Prompt/Dynamic/Conditional/Loop/Reflector)
- `dynamic-segment`:
  - Loop iteration segment: `iter_<index>` (current implementation)
  - Future optional keyed iteration segment: `iter_<key>` (requires explicit normalization rule)

Example:

```
state.prime.book_character_stories.per_character.iter_3.gather_pages
```

### Naming and Stability Rules

- Every effect that writes a record MUST have a `name` (or an explicit deterministic fallback key for transparent controls).
- Sibling step names MUST be unique within the same parent scope.
- Names are immutable across patch/minor releases unless accompanied by a documented migration plan.
- Disallowed in names: `.`, whitespace-only names, and unstable generated suffixes.

### Record Shape Contract

Each terminal effect record path stores:

- `value`: effect output payload
- `meta.created_at`: RFC3339 timestamp
- `meta.completed_at`: RFC3339 timestamp
- `meta.error`: nullable error string
- `meta.adapter` and `meta.model` for prompt/model-driven effects when applicable

Named control records additionally store deterministic control metadata:

- Conditional: branch result and selected branch metadata
- Loop: iteration count, termination reason, and per-iteration grouping
- Dynamic/Reflector: aggregate execution completion metadata

### Deterministic Resolution Algorithm

Resolver behavior must be pure with respect to execution structure:

1. Start at `root = prime`.
2. For each effect execution, append its `name` segment relative to parent store scope.
3. For named loops, append `iter_<n>` per iteration before writing child effects.
4. Write only under the resolved path for that effect; no implicit writes to sibling branches.

Reference implementation locations:

- `src/circuitry/core/dynamic.py`
- `src/circuitry/core/prompt.py`
- `src/circuitry/core/conditional.py`
- `src/circuitry/core/loop.py`
- `src/circuitry/core/store/store.py`

### Loop and Conditional Path Rules

- Named loop:
  - Loop wrapper record path: `<parent>.<loop-name>`
  - Iteration path: `<parent>.<loop-name>.iter_<n>`
  - Body effect path: `<parent>.<loop-name>.iter_<n>.<effect-name>`
- Transparent loop (no name):
  - No wrapper segment; body effects resolve directly under current parent scope.
- Named conditional:
  - Wrapper record path: `<parent>.<conditional-name>`
  - Selected branch effects resolve under wrapper scope.
- Transparent conditional (no name):
  - No wrapper segment; selected branch effects resolve under current parent scope.

### Backward Compatibility Policy

- State-path format is a compatibility surface.
- Any change to segment format, root name, or loop iteration keying requires:
  - versioned migration notes,
  - compatibility tests against historical fixtures,
  - explicit release notes entry.

### Test and Validation Requirements

Must be enforced in automated tests:

- Determinism: same orchestration + same inputs -> identical state paths.
- Collision safety: no duplicate path writes from distinct siblings in same scope.
- Coverage for chain/tree, nested dynamic, conditional (named/transparent), loop (named/transparent).
- Contract fixtures for examples (`examples/*.yml`) and regression fixtures from `out.json`-style outputs.

### Cross-References to Story Execution

The following stories must implement and validate this contract:

- Epic 1 Story 1.1 (name/path validation)
- Epic 1 Story 1.2 (CLI state-path visibility)
- Epic 2 Story 2.2 (loop/conditional path semantics)
- Epic 2 Story 2.3 (nested composition path determinism)
- Epic 2 Story 2.4 (state-path conformance tests)
