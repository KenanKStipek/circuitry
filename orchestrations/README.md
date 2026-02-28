# Orchestration Library

This directory is the pre-built orchestration library — runnable orchestrations for core Circuitry primitives, composition patterns, and community-contributed workflows.

Compatibility:
- Runtime contract: current `main` branch runtime.
- Versioning source: `orchestrations/manifest.json`.
- Policy: orchestrations are versioned; behavior-affecting changes require manifest version bump and compatibility note.

## Example Index

| File | Intent | Primitives | Prerequisites | Difficulty | Expected State Highlights |
|---|---|---|---|---|---|
| `hello.yml` | Smallest runnable prompt | prompt | none (use `--dry-run`) | beginner | `prime.say_hello.value` |
| `dynamic_hello.yml` | Sequential composition and interpolation | dynamic(chain), prompt | none (use `--dry-run`) | beginner | `prime.onboarding.ask_name.value`, `prime.onboarding.greet.value` |
| `conditional_example.yml` | Branching based on condition | prompt, if(cel) | none (use `--dry-run`) | beginner | `prime.check_role.admin_greeting.value` or `prime.check_role.user_greeting.value` |
| `loop_example.yml` | Collection iteration with named loop body | prompt(json), loop(each) | none (use `--dry-run`) | intermediate | `prime.explain_topics.iter_0.explain.value` |
| `typed_prompt_example.yml` | Typed prompt outputs and schema-oriented flow | prompt(json/number/boolean) | none (use `--dry-run`) | intermediate | `prime.extract_entities.value.entities`, `prime.count_entities.value`, `prime.is_location_present.value` |
| `reflector_v1.yml` | Reflector planning primitive | prompt, reflector | none (use `--dry-run`) | advanced | `prime.goal.value`, `prime.plan.*` runtime records |
| `multi_primitive_story.yml` | Realistic mixed control flow | dynamic(chain), prompt, loop(each), if(cel), typed prompt | none (use `--dry-run`) | advanced | `prime.pipeline.summarize.value`, loop iteration paths, branch path under `finalize` |
| `meta_orchestrator.yml` | Generate a new orchestration YAML from a natural language prompt | dynamic(chain), prompt(json), loop(each), if(cel), prompt | `--state input.json` required; 7B+ model recommended (phi3:mini unreliable — see file header) | advanced | `prime.generate.final_yaml.value` (generated YAML), `prime.generate.analyze_intent.value` (intent analysis), `prime.generate.elaborate_steps.iter_N.yaml_stub.value` (per-step stubs) |

## Run and Inspect

```bash
./scripts/run-orchestrations
./scripts/circuitry inspect orchestrations/multi_primitive_story.yml
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
