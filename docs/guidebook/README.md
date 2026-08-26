# The Circuitry Guidebook

> "Control mechanisms that lay their own plans." — Gordon Pask, *An Approach to Cybernetics* (1961)

This is the long-form companion to the [README](../../README.md). The README gets you running and gives you the mental model; the guidebook is the exhaustive tour — every primitive, every state path, every switch, and the methodology that ties them together. It is written as a curriculum: each chapter assumes only the ones before it, and one running example (a dinner party) threads through all of them, so by the last chapter you have seen every part of the language in one document.

The arc has three acts:

**I. Basics — the machine.** The two base monads the whole language is built from, the state they communicate through, and the config and error handling that keep a run honest.

**II. Cybernetics — the machine steering itself.** The three effects that read state and steer: branching, iterating, and planning. This is what the framework is named for.

**III. The machine in the world.** Orchestrations composing orchestrations, the runtime measuring and routing its own prompts, oversized prompts decomposing themselves, and every surface the outside world arrives through.

## Chapters

### Part I — Basics: the machine

1. [Prompt](01-prompt.md) — one model call, one typed value, one deterministic state path.
2. [Dynamic](02-dynamic.md) — composition as a first-class operator: `chain` and `tree`.
3. [State](03-state.md) — the three namespaces, the two reading languages, and the addressing rules.
4. [Configuration](04-configuration.md) — where capability lives, how it resolves, and how the choice is recorded.
5. [Errors](05-errors.md) — `on_error`, retries, provider fallbacks, timeouts: degrading deliberately.

### Part II — Cybernetics: the machine steering itself

6. [If](06-if.md) — a predicate over state, with the model as sensor or CEL as the deterministic gate.
7. [Loop](07-loop.md) — `each` and `while`, `collect`, and the four read forms for loop state.
8. [Reflector](08-reflector.md) — effects generated at runtime from observed state, bounded and validated.

### Part III — The machine in the world

9. [Composition](09-composition.md) — `use`, `interface`, and the library: orchestrations as organs.
10. [Complexity](10-complexity.md) — scoring every prompt and routing it to the cheapest capable model.
11. [Decomposition](11-decomposition.md) — over-threshold prompts that plan their own fan-out and merge.
12. [Surfaces](12-surfaces.md) — CLI, TUI, SDK, MCP server, and observability.
13. [Tools and persistence](13-tools-and-persistence.md) — tool plugins, adapters, runtime plugins, and durable state.
14. [The whole meal](14-the-whole-meal.md) — every primitive in one document.

## How to read the examples

Every YAML block in this guidebook is checked by the test suite: positive examples validate with `cof check`, and every anti-pattern is shown under a **✗** and provably fails validation (or, where the validator can only warn, is marked **⚠** and provably produces that warning). A handful of anti-patterns are wrong at *run* time rather than validation time — the document is well-formed, the run goes green, and the result is silently wrong. Those are marked **✗ runtime**, and the text says exactly what goes wrong.

Two documents sit alongside the guidebook and are referenced from it throughout:

- [Orchestration Reference](../orchestration-reference.md) — the field-by-field table for every effect type. The guidebook explains; the reference enumerates.
- [Grammar](grammar.md) — the formal grammar of the orchestration language, derived from `orchestration.schema.json`.

## Other formats

The guidebook is also built as a single PDF and an EPUB for e-readers — see [Building the book](build.md). Prebuilt copies live next to this file: [`circuitry-guidebook.pdf`](circuitry-guidebook.pdf) and [`circuitry-guidebook.epub`](circuitry-guidebook.epub).
