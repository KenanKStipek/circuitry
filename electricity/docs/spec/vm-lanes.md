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
| `electricity-bytecode` | `src/refusal.rs` | `first_unsupported` | A | Final and complete, including its own tool-provider refusal (any `tool` effect whose `provider:` isn't `json` or isn't in *extra_allowed_providers*, normalized `.strip().lower()`) -- lane D wires this into `electricity::run_orchestration`: call it after every check error (check errors always come first, exactly as `cof run` orders them), before any state, `--out`, `--events` or `--live-state` file is written; the refusal message keeps the existing preview marker phrase and adds the effect path and the reason; the conformance runner keeps skipping on the marker alone. |
| `electricity-config` | `src/lib.rs` | `CircuitryConfig`, `ConfigError`, `resolve_config` | A (stub) | Lane D implements the real `SANE_DEFAULTS`/deep-merge/env-overlay port, and adds `enabled_tools`/`enabled_adapters`/`enabled_plugins` to `CircuitryConfig` and the `check_allowlist` seam (`runtime_shim.py`'s own call, between `resolve_config` and `effective_settings`) -- as either a new function here or a field `effective_settings`'s own caller fills in first; lane A leaves this unfixed deliberately rather than inventing a signature nothing yet needs. |
| `electricity-config` | `src/lib.rs` | `merge_runtime` | A (stub) | Lane D implements the real ceiling-aware deep merge, and switches `electricity_compiler::pipeline`'s own `merged_runtime_block` over to it. |
| `electricity-config` | `src/lib.rs` | `EffectiveSettings`, `effective_settings` | A (stub) | `sources` is an `IndexMap` (insertion order, matching `--out`'s own `json.dumps`) and `effective_settings` returns `Result<_, ConfigError>` (Python's own function raises) -- both lane A fixed already, so lane D's real port doesn't have to widen either. Lane D still adds `out`/`plugins`/`warnings` fields to `EffectiveSettings` itself. |
| `electricity-config` | `src/lib.rs` | `validate_complexity` | A (stub, no-op) | Lane D: the real `resolve_complexity_settings` validation `electricity-compiler`'s own crate docs already list as a known divergence. |
| `electricity-config` | `src/lib.rs` | `validate_persistence` | A (stub, no-op) | Lane D: the real `build_persistence_backend` validation, same known divergence. |
| `electricity-vm` | `src/store.rs` | `NodeRef`, `Store::new` | A | `NodeRef` type and an empty-root constructor are final; `ensure_dict`/`child`/`parallel_branches`/`merge` are lane B stubs -- lane B also owns the internal convention for `last`-style aliasing inside a node's own `IndexMap<Value, Value>` (not fixed by lane A). |
| `electricity-vm` | `src/cancel.rs` | `CancellationToken` | A | Final and complete -- a self-contained, `Send + Sync` token tree (`Arc`/`AtomicI32`, not `Rc`/`Cell`: lane D's signal handler cancels a run from a thread or task other than the one blocked in the VM), with an awaitable `cancelled()` lane B/C select on; no lane B/C/D work needed. |
| `electricity-vm` | `src/limiter.rs` | `Limiter`, `SlotGuard` | A (stub) | Lane B implements the real group-then-global async semaphore. Python reports `waiting_for` by writing it into `meta` on the store, not through a callback -- add an *additive* `try_acquire` (non-blocking) beside `acquire` so a caller can write `waiting_for` itself on a miss before falling back to the blocking call; don't add an observer parameter or a store write inside this crate. |
| `electricity-vm` | `src/observer.rs` | `RunObserver`, `NullObserver` | A | Final and complete. |
| `electricity-vm` | `src/lib.rs` | `RunContext` | A | Final shape: the registry/limiter/model/adapter/runtime-config bag `execute_root`, `run_tool` and `execute_tool` read -- lane D's `run_orchestration` is the first caller that builds a real one. |
| `electricity-vm` | `src/exec/tool.rs` | `run_tool` | A | Dispatch seam final (lane C never edits this file to add a provider); *provider* normalization already matches `plugins/factory.py::build_plugin`'s `.strip().lower()`, but the exact "unknown provider" wording still diverges -- lane C may widen it once it has the full registry to word the supported-list half of that message from. |
| `electricity-vm` | `src/exec/tool.rs` | `execute_tool` | C (stub) | Lane C's own entry point for a `tool` effect: the `meta` reset per attempt, rendering `params`/`params_json` (`src/params.rs`, new, also lane C's), `retries`/backoff, a limiter slot held per attempt and released before backoff, `expect`, `run_tool` dispatch, redaction/the 64 KiB `raw` cap, the store write, and checking *token* between attempts. `exec::dynamic`/`exec::conditional` (lane B) call this once per `tool` node; its own signature should not need to change for that. |
| `electricity-vm` | `src/params.rs` | tool/`use` params rendering | C (new file) | `{from: ...}` resolved, Mustache leaves rendered, `params_json` deep-merged through `JsonAwareCtx` -- `execute_tool`'s own caller for this half of `core/tool.py::ToolRuntime.run`. |
| `electricity-vm` | `src/lib.rs` | `execute_root` | A (stub) | Lane B's own tree-walking interpreter (`src/exec/mod.rs`, `src/exec/dynamic.rs`, `src/exec/conditional.rs`, all new lane B files), now reading *run_ctx* (`RunContext`) for the registry/limiter/model/adapter/runtime config. Runs tree branches as futures inside one `FuturesUnordered` (gated by `Limiter` for `max_concurrency`), not `tokio::task::spawn_local` -- `execute_root`'s borrowed arguments aren't `'static`, which `spawn_local` requires. |
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
