# Curated Orchestration Examples

This directory contains runnable example orchestrations for core Circuitry primitives and composition patterns.

Compatibility:
- Runtime contract: current `main` branch runtime.
- Versioning source: `examples/manifest.json`.
- Policy: examples are versioned; behavior-affecting changes require manifest version bump and compatibility note.

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

## Run and Inspect

```bash
python -m circuitry.cli.app run examples/multi_primitive_story.yml --dry-run --out out.json --pretty
python -m circuitry.cli.app inspect examples/multi_primitive_story.yml
```

## Maintenance Rule

When runtime behavior changes:
1. Re-run `pytest -q tests/examples/test_examples_smoke.py`.
2. Update `examples/manifest.json` compatibility notes.
3. Update this index if intent, state paths, or prerequisites changed.
