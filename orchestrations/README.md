# Orchestration Library

This directory is the pre-built orchestration library — runnable orchestrations for core Circuitry primitives, composition patterns, and community-contributed workflows.

Compatibility:
- Runtime contract: current `main` branch runtime.
- Versioning source: `orchestrations/manifest.json`.
- Policy: orchestrations are versioned; behavior-affecting changes require manifest version bump and compatibility note.

## Example Index

| File | Intent | Primitives | Difficulty | Expected State Highlights |
|---|---|---|---|---|
| `_prompt.yml` | prompt primitive — text, json, number, boolean output types | prompt | beginner | `prime.greeting.value`, `prime.entities.value.words`, `prime.word_count.value`, `prime.is_short.value` |
| `_dynamic.yml` | dynamic chain — sequential steps, each sees prior outputs | dynamic(chain), prompt | beginner | `prime.pipeline.topic.value`, `prime.pipeline.explain.value`, `prime.pipeline.question.value` |
| `_dynamic_tree.yml` | dynamic tree — independent parallel steps share same input snapshot | dynamic(tree), prompt | beginner | `prime.analysis.summary.value`, `prime.analysis.analogy.value`, `prime.analysis.question.value` |
| `_conditional.yml` | if primitive — CEL expression branching | if(cel), prompt | intermediate | `prime.check_role.admin_greeting.value` or `prime.check_role.user_greeting.value` |
| `_loop.yml` | loop primitive — each iteration over a JSON array | loop(each), prompt(json) | intermediate | `prime.explain.iter_0.summary.value`, … |
| `_reflector.yml` | reflector primitive — runtime effect generation | reflector, prompt | advanced | `prime.goal.value`, `prime.plan.*` runtime records |
| `_composition.yml` | all primitives composed — dynamic, loop, if, prompt | dynamic(chain), loop(each), if(cel), prompt | advanced | `prime.pipeline.summarize.iter_N.line.value`, `prime.pipeline.finalize.success_note.value` |
| `meta_orchestrator.yml` | generate a new orchestration YAML from a natural language prompt | dynamic(chain), prompt(json), loop(each), if(cel), prompt | advanced | `prime.generate.final_yaml.value`, `prime.generate.analyze_intent.value` |

## Run and Inspect

```bash
./scripts/run-orchestrations
./scripts/circuitry inspect orchestrations/_composition.yml
```

Live adapter test (optional):

```bash
./scripts/run-orchestrations --live
```

Custom output paths:

```bash
./scripts/run-orchestrations --out /tmp/examples-dry.json
./scripts/run-orchestrations --live --out /tmp/examples-dry.json --live-out /tmp/examples-live.json
```

Notes:
- `./scripts/run-orchestrations` validates and dry-runs all orchestration files.
- `--live` runs a small subset that performs actual model inference.
- `--out` and `--live-out` control where state files are written.
- Default outputs are repo-relative and ignored by git: `tmp/circuitry-example-state.json` and `tmp/circuitry-example-live-state.json`.
- Live runs require your configured adapter endpoint (default `ollama` at `http://localhost:11434`).

## Maintenance Rule

When runtime behavior changes:
1. Re-run `pytest -q tests/orchestrations/test_examples_smoke.py`.
2. Update `orchestrations/manifest.json` compatibility notes.
3. Update this index if intent, state paths, or prerequisites changed.
