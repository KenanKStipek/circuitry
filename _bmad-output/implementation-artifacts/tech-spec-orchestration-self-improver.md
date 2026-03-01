---
title: 'Orchestration Self-Improver'
slug: 'orchestration-self-improver'
created: '2026-02-28'
status: 'Completed'
stepsCompleted: [1, 2, 3, 4]
tech_stack: ['Python 3', 'YAML', 'circuitry SDK (run_orchestration)', 'hashlib (stdlib)', 'argparse (stdlib)']
files_to_modify: []
code_patterns:
  - 'Orchestration chain: dynamic(chain) > prompt(json) > prompt > prompt — mirrors meta_orchestrator.yml'
  - 'Script uses run_orchestration() with raise_on_error=False and result.state directly'
  - 'State saved per-iteration via json.dumps(result.state) to tmp/ (gitignored)'
  - 'venv detection: check .venv/bin/python, shebang #!/usr/bin/env python3'
test_patterns: []
---

# Tech-Spec: Orchestration Self-Improver

**Created:** 2026-02-28

## Overview

### Problem Statement

Once an orchestration is written, there is no automated way to iteratively refine it toward higher quality. Users must manually review and revise YAML, which is slow and inconsistent. There is no feedback loop that pushes an orchestration toward richer prompts, better use of primitives, and closer alignment to the original intent.

### Solution

Two deliverables working together:

1. A new `orchestration_improver.yml` orchestration that takes an existing orchestration YAML string and the original user prompt as input, analyzes weaknesses, and produces a single improved version of the YAML.
2. An `improve-orchestration` script that drives the iteration loop externally: calls `run_orchestration` once per iteration, saves state to disk after each step, computes a hash of the output YAML, runs an LLM judge if the hash differs from previous iterations, and stops when the last N iterations converge.

### Scope

**In Scope:**
- `orchestrations/orchestration_improver.yml` — single-step improvement orchestration (analyze → revise)
- `scripts/improve-orchestration` — Python script driving the iteration loop with per-iteration state saves and convergence detection
- Convergence detection: SHA-256 hash of output YAML first; if hash differs from last N, call LLM judge; stop when converged
- Script accepts `--orchestration <path>` and `--prompt <string>` as inputs

**Out of Scope:**
- Shell/exec effect type (tracked as separate future work)
- Modifications to `meta_orchestrator.yml`
- Running or evaluating the generated orchestration's live outputs
- Web UI or service layer

## Context for Development

### Codebase Patterns

- Orchestrations are YAML files in `orchestrations/`. Each starts with a header comment block (see `meta_orchestrator.yml` lines 1–77): name, description, usage, input state keys, output state paths, model recommendation.
- Orchestration YAML top-level fields: `adapter`, `model`, `effects`. No other top-level keys.
- YAML-generating orchestrations follow the `meta_orchestrator.yml` pattern: `dynamic(chain) → prompt(json) [analyze] → prompt [draft] → prompt [review/final]`. Output always at `prime.<dynamic_name>.<final_prompt_name>.value`.
- Templates use Mustache: `{{key}}` for initial state keys, `{{prime.name.value}}` for effect outputs. Nested effect output paths include parent dynamic name: `{{prime.generate.analyze.value}}`.
- CEL expressions must use the full prefix: `state.prime.<name>.value` (not `prime.<name>.value`).
- Scripts in `scripts/` are executable files with no extension. Bash scripts use `#!/usr/bin/env bash` + `set -euo pipefail`. Python scripts use `#!/usr/bin/env python3`.
- `run_orchestration()` is keyword-only: `orchestration_path` (str|Path), `state` (dict), `dry_run`, `verbose`, `raise_on_error` (default True). Returns `RunResult(ok, state, warnings, error)`. Use `raise_on_error=False` in scripts that handle errors manually.
- `result.state` is the complete hierarchical state dict. The script accesses output YAML via `result.state["prime"]["generate"]["final_yaml"]["value"]`.
- State save per-iteration: `Path(out_dir / f"iter-{i}.json").write_text(json.dumps(result.state, indent=2))`. The `tmp/` directory is gitignored and already exists in the project.
- The `run_orchestration()` function does NOT write to disk unless `out_path` is passed — the script captures `result.state` in memory and writes it manually.

### Files to Reference

| File | Purpose |
| ---- | ------- |
| `orchestrations/meta_orchestrator.yml` | Primary pattern reference: YAML-generating orchestration (analyze → draft → review chain) |
| `src/circuitry/__init__.py` | `run_orchestration()` signature and `RunResult` import |
| `src/circuitry/cli/runtime_shim.py` | `RunRequest`/`RunResult` dataclass definitions, `run()` implementation |
| `src/circuitry/api.py` | `run_orchestration()` full implementation (wraps `runtime_shim.run()`) |
| `scripts/run-orchestrations` | Bash script style reference |
| `scripts/circuitry` | venv detection pattern reference |

### Technical Decisions

- **Script-driven loop** (not Circuitry Loop primitive): enables per-iteration state saves and deterministic hash computation in Python. The loop lives entirely in the script; the orchestration handles one improvement step per invocation.
- **Two new orchestration files**: `orchestration_improver.yml` (the improver) and `orchestration_improver_judge.yml` (the convergence judge). Keeps concerns separated and both are independently reusable.
- **Convergence check order**: SHA-256 hash of output YAML string first (fast, zero LLM cost); if hash differs from all hashes in the last N iterations, run the judge orchestration. Stop when hash matches any recent hash, or judge returns `true` (converged).
- **Judge output**: `prime.judge.value` (boolean) — `true` means "essentially the same, stop iterating."
- **N (convergence window)**: default 3, configurable via `--convergence-window`.
- **Max iterations**: default 10, configurable via `--max-iterations` to prevent runaway loops.
- **State save path**: `tmp/improve-<slug>/iter-{N}.json` (full state) and `tmp/improve-<slug>/iter-{N}.yml` (extracted YAML only) per iteration.
- **Output YAML state path** (from improver): `prime.generate.final_yaml.value` — mirrors `meta_orchestrator.yml` convention.
- **Script is Python** (not bash): needs `hashlib`, `argparse`, and `circuitry` API. Shebang: `#!/usr/bin/env python3`. Users run from activated venv.

## Implementation Plan

### Tasks

- [x] Task 1: Create `orchestrations/orchestration_improver.yml`
  - File: `orchestrations/orchestration_improver.yml` (new file)
  - Action: Create YAML orchestration with full header comment block (name, description, usage, input state keys, output state paths, model recommendation — see `meta_orchestrator.yml` lines 1–77 for exact format). Top-level: `adapter: ollama`, `model: qwen3:8b`. Single `dynamic` named `generate` with `flow: chain` containing three prompts in order:
    1. `analyze_quality` (`prompt_type: json`): reads `{{input_yaml}}` and `{{original_prompt}}` from initial state. Schema: `{issues: [string], strengths: [string], improvement_areas: [string], alignment_score: number}` (all required, no additionalProperties). Prompt instructs model to assess: (a) does the orchestration fully achieve the original intent, (b) are prompts rich and specific, (c) are primitives used appropriately, (d) are there missing steps, (e) are templates clear.
    2. `draft_yaml` (`prompt_type: text`): reads `{{original_prompt}}`, `{{input_yaml}}`, and `{{prime.generate.analyze_quality.value.issues}}`, `{{prime.generate.analyze_quality.value.strengths}}`, `{{prime.generate.analyze_quality.value.improvement_areas}}`. Instructs model to produce an improved YAML incorporating the analysis while preserving strengths. Includes the 8-rule Circuitry YAML rules block (adapter/model at top, Mustache syntax, CEL full-path, etc.). Ends: "Return ONLY the YAML. No markdown fences."
    3. `final_yaml` (`prompt_type: text`): reads `{{original_prompt}}` and `{{prime.generate.draft_yaml.value}}`. Runs the same 7-check self-review from `meta_orchestrator.yml` (adapter present, state paths, loop each.in, CEL prefix, Mustache vars, all required fields, nested dynamic paths). Ends: "Return ONLY the final YAML. No markdown fences."
  - Notes: Primary output is `prime.generate.final_yaml.value`. Document this path in the header comment block.

- [x] Task 2: Create `orchestrations/orchestration_improver_judge.yml`
  - File: `orchestrations/orchestration_improver_judge.yml` (new file)
  - Action: Create minimal YAML orchestration with header comment block. Top-level: `adapter: ollama`, `model: qwen3:8b`. Single `prompt` effect named `judge` with `prompt_type: boolean`. Template: instructs model to compare `{{yaml_a}}` and `{{yaml_b}}` and answer `true` if the two orchestrations are functionally equivalent or only trivially different (whitespace, wording), `false` if meaningfully different and iteration should continue. Input state keys: `yaml_a` (previous iteration YAML), `yaml_b` (current iteration YAML). Output: `prime.judge.value` (boolean).
  - Notes: No `dynamic` wrapper needed — single top-level prompt. Keep the prompt simple and binary. Document output path in header.

- [x] Task 3: Create `scripts/improve-orchestration`
  - File: `scripts/improve-orchestration` (new file)
  - Action: Create executable Python script. Structure:
    ```
    #!/usr/bin/env python3
    """improve-orchestration — iteratively improve a Circuitry orchestration YAML until convergence."""
    ```
    Imports: `argparse`, `hashlib`, `json`, `sys` from stdlib; `Path` from `pathlib`; `run_orchestration` from `circuitry`.

    **CLI args (all keyword via argparse):**
    - `--orchestration` (required): path to input orchestration YAML
    - `--prompt` (required): original intent string
    - `--max-iterations` (int, default 10)
    - `--convergence-window` (int, default 3): how many recent hashes to compare
    - `--verbose` (flag): passed through to `run_orchestration`
    - `--dry-run` (flag): passed through to `run_orchestration` (allows testing loop structure without LLM calls)

    **Setup:**
    - Read input YAML: `current_yaml = Path(args.orchestration).read_text()`
    - Compute slug: `slug = Path(args.orchestration).stem`
    - Create output dir: `out_dir = Path("tmp") / f"improve-{slug}"` — `out_dir.mkdir(parents=True, exist_ok=True)`
    - Resolve improver/judge paths relative to script location: `IMPROVER = Path(__file__).parent.parent / "orchestrations" / "orchestration_improver.yml"` (same for judge)
    - Init: `recent_hashes: list[str] = []`, `iteration = 0`

    **Iteration loop** (`while iteration < args.max_iterations`):
    1. Print `[iter {iteration+1}/{args.max_iterations}] Running improver...` to stderr
    2. Call `run_orchestration(orchestration_path=IMPROVER, state={"input_yaml": current_yaml, "original_prompt": args.prompt}, verbose=args.verbose, dry_run=args.dry_run, raise_on_error=False)`
    3. If `not result.ok`: print error to stderr, `sys.exit(1)`
    4. Extract output: `output_yaml = result.state.get("prime", {}).get("generate", {}).get("final_yaml", {}).get("value", "")`
    5. If not output_yaml: print "No output YAML produced", `sys.exit(1)`
    6. Save state: `(out_dir / f"iter-{iteration}.json").write_text(json.dumps(result.state, indent=2))` and `(out_dir / f"iter-{iteration}.yml").write_text(output_yaml)`
    7. Compute hash: `current_hash = hashlib.sha256(output_yaml.encode()).hexdigest()`
    8. **Hash convergence check**: if `current_hash in recent_hashes[-args.convergence_window:]`: print "Converged (hash match) at iteration {iteration+1}", break
    9. **LLM judge check** (only if enough history): if `len(recent_hashes) >= args.convergence_window`:
       - Call `run_orchestration(orchestration_path=JUDGE, state={"yaml_a": current_yaml, "yaml_b": output_yaml}, raise_on_error=False)`
       - If judge `result.ok` and `result.state.get("prime", {}).get("judge", {}).get("value") is True`: print "Converged (LLM judge) at iteration {iteration+1}", break
    10. Append `current_hash` to `recent_hashes`; update `current_yaml = output_yaml`; `iteration += 1`

    **After loop:** Print summary: final YAML path (`out_dir/iter-{iteration-1}.yml`), total iterations, stop reason.

  - Notes: Make the file executable after creation (`chmod +x scripts/improve-orchestration`). Orchestration paths are resolved relative to the script file, not the working directory, so the script can be run from any directory.

- [x] Task 4: Make script executable
  - File: `scripts/improve-orchestration`
  - Action: Run `chmod +x scripts/improve-orchestration` after creating the file.
  - Notes: Required for `./scripts/improve-orchestration` invocation style matching other scripts.

- [x] Task 5: Update `orchestrations/manifest.json`
  - File: `orchestrations/manifest.json`
  - Action: The manifest is scoped strictly to `_*.yml` primitive examples — a test (`test_example_manifest_covers_curated_set`) enforces exact equality with the curated set. The new orchestrations follow the same convention as `meta_orchestrator.yml` (not listed in manifest). No manifest changes needed.
  - Notes: Functional orchestrations are not added to the manifest by project convention.

### Acceptance Criteria

- [ ] AC 1: Given `orchestrations/orchestration_improver.yml` exists, when `./scripts/circuitry validate orchestrations/orchestration_improver.yml` is run, then it exits 0 with no errors.

- [ ] AC 2: Given `orchestrations/orchestration_improver_judge.yml` exists, when `./scripts/circuitry validate orchestrations/orchestration_improver_judge.yml` is run, then it exits 0 with no errors.

- [ ] AC 3: Given the `improve-orchestration` script exists, when it is run without `--orchestration` or `--prompt`, then it prints a usage error and exits non-zero.

- [ ] AC 4: Given a valid `--orchestration` path and `--prompt` string, when `scripts/improve-orchestration` runs one iteration successfully, then `tmp/improve-<stem>/iter-0.json` and `tmp/improve-<stem>/iter-0.yml` are created on disk.

- [ ] AC 5: Given `--max-iterations 2` is passed, when the script runs and does not converge, then it stops after exactly 2 iterations and exits 0.

- [ ] AC 6: Given two consecutive iterations produce identical output YAML and `--convergence-window 1`, when the hash of the second output matches the first, then the script stops immediately with a "Converged (hash match)" message.

- [ ] AC 7: Given the LLM judge returns `true` for two semantically equivalent YAMLs with different hashes, when the convergence window is met, then the script stops with a "Converged (LLM judge)" message.

- [ ] AC 8: Given the improver orchestration fails (adapter unavailable, model error), when `result.ok` is False, then the script prints the error from `result.error` to stderr and exits with code 1 without crashing.

- [ ] AC 9: Given a working adapter and a real input orchestration, when a live run completes at least one iteration, then `prime.generate.final_yaml.value` contains non-empty text that is valid YAML (manually verifiable by running `./scripts/circuitry validate` on the extracted file).

- [ ] AC 10: Given `--dry-run` is passed, when the script runs, then it executes the iteration loop structure (creates output dir, attempts orchestration calls) without making real LLM API calls.

## Additional Context

### Dependencies

- `circuitry` Python package (already installed in `.venv`) — `run_orchestration` import
- `hashlib`, `argparse`, `json`, `sys`, `pathlib` — all Python stdlib, no new installs required

### Testing Strategy

- **Structural validation** (no adapter needed): `./scripts/circuitry validate orchestrations/orchestration_improver.yml` and `./scripts/circuitry validate orchestrations/orchestration_improver_judge.yml`
- **Script argument validation** (no adapter needed): run `scripts/improve-orchestration` with missing args, bad paths — verify error messages and exit codes
- **Dry-run loop test** (no adapter needed): run `scripts/improve-orchestration --orchestration orchestrations/_prompt.yml --prompt "test" --dry-run --max-iterations 2` — verify output dir created, iter files written, loop terminates
- **Live integration test** (requires configured adapter): run against `orchestrations/_prompt.yml` with a real adapter, observe iteration output and convergence behavior; manually validate final YAML with `./scripts/circuitry validate`

## Review Notes

- Adversarial review completed
- Findings: 18 total, 18 fixed (auto-fix applied)
- Resolution approach: auto-fix
- Key fixes applied: Python 3.9 guard, `out_dir` pinned to project root, `--convergence-window` validation, markdown fence stripping, dry-run graceful exit, `None` judge value warning, `alignment_score` min/max schema constraints, sharpened judge convergence prompt to prevent premature early stopping

### Notes

- **Future work**: `exec` / shell effect type for the Circuitry DSL would allow hash computation and convergence detection to live inside the orchestration loop natively — tracked as separate work.
- **Model recommendation**: A capable model (claude-haiku, gpt-4o-mini, qwen3:8b+) is strongly recommended. Small models may produce structurally invalid YAML in the improve/draft step.
- **Convergence sensitivity**: Tight convergence windows (N=1) may cause early stopping on coincidental hash matches. Default N=3 provides robustness.
- **Judge reliability**: The boolean LLM judge can misfire on edge cases. The hash check provides a reliable primary signal; the judge is a secondary heuristic.
- **Path resolution**: The script resolves orchestration file paths relative to its own location (`__file__`) rather than the working directory, so it works correctly when run from any directory.
