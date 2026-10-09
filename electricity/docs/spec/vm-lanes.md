# M0-H VM lanes (issue #431)

Lane A (the gate) created every new crate and every cross-lane signature below so that lanes B
(VM core), C (tool runtime, `json`, redaction) and D (config, run wiring, outputs, CLI, signals)
can each build against a stable seam without editing a file outside their own crate (or a file
this document lists as another lane's).

This table is the authoritative "who owns what" map for the M0-H VM crates. Keep it in sync with
reality: a lane that fills in a stub should update the row(s) it touches in the same PR.

| Crate | File | Function/type | Lane | Status after lane A |
|---|---|---|---|---|
| `electricity-compiler` | `src/pipeline.rs` | `prepare_document` | A | Final: load errors (step 5 of #431's run-wiring table). |
| `electricity-compiler` | `src/pipeline.rs` | `pre_state_checks` | A | Final signature; body final except the `electricity_config::validate_complexity`/`validate_persistence` hooks' own real logic (lane D). |
| `electricity-compiler` | `src/pipeline.rs` | `post_state_checks` | A | Final: structural checks, compile, groups, cycles, digest (step 14). |
| `electricity-compiler` | `src/pipeline.rs` | `check_for_run` | A | Final: the three phases' exact composition -- unchanged behaviour. |
| `electricity-compiler` | `src/pipeline.rs` | `build_input_namespace` | A | Final: returns the coerced/defaulted namespace, not just a verdict. |
| `electricity-bytecode` | `src/param.rs` | `ParamNode::Map` | A | Final: keyed by `Value`, not `String`. |
| `electricity-bytecode` | `src/program.rs` | `DocumentInfo::digest` | A | Final: `Option<String>`. |
| `electricity-bytecode` | `src/path.rs` | `EffectPath::{Display,concretize}` | A | Final and complete -- no further lane needed. |
| `electricity-bytecode` | `src/op.rs` | `Op::python_type_name`/`python_type_name` | A | Final and complete. |
| `electricity-bytecode` | `src/region.rs` | `Region::dynamic_flow`, `Condition::mode`, `LoopFlow::as_str` | A | Final and complete. |
| `electricity-bytecode` | `src/refusal.rs` | `first_unsupported` | A | Final and complete -- lane D's CLI calls this to refuse unsupported content before any state is written. |
| `electricity-config` | `src/lib.rs` | `CircuitryConfig`, `ConfigError`, `resolve_config` | A (stub) | Lane D implements the real `SANE_DEFAULTS`/deep-merge/env-overlay port. |
| `electricity-config` | `src/lib.rs` | `merge_runtime` | A (stub) | Lane D implements the real ceiling-aware deep merge, and switches `electricity_compiler::pipeline`'s own `merged_runtime_block` over to it. |
| `electricity-config` | `src/lib.rs` | `EffectiveSettings`, `effective_settings` | A (stub) | Lane D. |
| `electricity-config` | `src/lib.rs` | `validate_complexity` | A (stub, no-op) | Lane D: the real `resolve_complexity_settings` validation `electricity-compiler`'s own crate docs already list as a known divergence. |
| `electricity-config` | `src/lib.rs` | `validate_persistence` | A (stub, no-op) | Lane D: the real `build_persistence_backend` validation, same known divergence. |
| `electricity-vm` | `src/store.rs` | `NodeRef`, `Store::new` | A | `NodeRef` type and an empty-root constructor are final; `ensure_dict`/`child`/`parallel_branches`/`merge` are lane B stubs -- lane B also owns the internal convention for `last`-style aliasing inside a node's own `IndexMap<Value, Value>` (not fixed by lane A). |
| `electricity-vm` | `src/cancel.rs` | `CancellationToken` | A | Final and complete -- a self-contained token tree, no lane B/C/D work needed. |
| `electricity-vm` | `src/limiter.rs` | `Limiter`, `SlotGuard` | A (stub) | Lane B implements the real group-then-global async semaphore. |
| `electricity-vm` | `src/observer.rs` | `RunObserver`, `NullObserver` | A | Final and complete. |
| `electricity-vm` | `src/lib.rs` | `RunContext` | A | Final shape: the registry/limiter/model/adapter/runtime-config bag `execute_root` and `run_tool` read -- lane D's `run_orchestration` is the first caller that builds a real one. |
| `electricity-vm` | `src/exec/tool.rs` | `run_tool` | A | Dispatch seam final (lane C never edits this file to add a provider); *provider* normalization and the exact "unknown provider" wording still diverge from `plugins/factory.py::build_plugin` -- lane C may change either once it has the full registry to word the supported-list half of that message from. |
| `electricity-vm` | `src/lib.rs` | `execute_root` | A (stub) | Lane B's own tree-walking interpreter (`exec::dynamic`, `exec::conditional`, both new lane B files), now reading *run_ctx* (`RunContext`) for the registry/limiter/model/adapter/runtime config. |
| `electricity-tools` | `src/lib.rs` | `ToolPlugin`, `ToolResult`, `ToolError`, `CheckResult`, `ToolRegistry` | A | Final and complete. |
| `electricity-tools` | `src/json.rs` | `JsonTool` | A (stub) | Lane C implements `parse`/`stringify`/`extract`. |
| `electricity-tools` | `src/test_tools.rs` (`test-tools` feature) | `SleepTool`, `FailTool` | A (stub) | Lane D's own signal tests implement the real blocking bodies. |
| `electricity-redaction` | `src/lib.rs` | `redact` | A (stub, pass-through) | Lane C implements the real deny-list port of `cli/redaction.py`. |
| `electricity-redaction` | `src/lib.rs` | `cap_raw`, `RawCapMarker` | A | Final and complete -- the 64 KiB `meta.raw` cap has no redaction logic of its own. |
| `electricity` (lib) | `src/run.rs` | `RunRequest`, `RunResult` | A | Final request/result shapes; lane D's own `run_orchestration` (steps 4-19 of #431's run-wiring table) replaces the current preview-refusal `run_orchestration` in `src/lib.rs`. |
| `electricity-cli` | `src/main.rs` | argument parsing | A | Final: every flag from #431's usage parses in any position; `--out`/`--pretty`/`--live-state`/`--events` route to the preview-refusal stub lane D replaces with the real output contract. |

## Reading this table

- **"A" / final**: lane A's own file and implementation; later lanes should not need to touch it
  at all (barring a genuine bug).
- **"A (stub)"**: lane A wrote the final public signature and a body that errors (or is an honest,
  narrow no-op/pass-through) rather than `unimplemented!()`. The owning lane replaces the body in
  place -- the signature should not need to change.
- A lane that finds a stub's signature doesn't fit what it needs should raise that as a
  cross-lane question (the orchestrator session), not change it unilaterally out from under a
  sibling lane that may already be building against it.
