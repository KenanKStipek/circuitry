# Plan 03: Fix Race Conditions in Parallel (Tree) Execution

## Problem

Three related race conditions in parallel execution paths:

1. **dynamic.py (lines 194-217):** `ThreadPoolExecutor` submits effects that all mutate shared `child_store` dicts concurrently without locking.
2. **loop.py (line 662):** Shallow dict copy `{**ctx, **iter_store.state}` allows nested mutations to bleed across iterations.
3. **`on_write` callback:** Fired concurrently from multiple threads without synchronization.

## Fix Strategy: Per-Thread Isolation + Store Locking

### Step 1: Add thread-safe Store wrapper

**File:** `src/circuitry/core/store/store.py`

- Add `threading.RLock` to `Store` (reentrant, since `set()` calls `ensure_dict()`)
- Wrap `ensure_dict()` and `set()` in `with self._lock:`
- Add `child(path)` method that returns a child `Store` sharing the parent's lock
- Pass parent lock via optional `_lock` parameter

### Step 2: Isolate per-thread state in dynamic tree flow

**File:** `src/circuitry/core/dynamic.py`

- Give each thread its own isolated `Store({})` with no `on_write`
- After all futures complete, merge results sequentially into `child_store`
- Fire `on_write` once after merge

### Step 3: Fix shallow copy in loop tree flow

**File:** `src/circuitry/core/loop.py`

- Replace `dict(ctx)` with `deepcopy(ctx)` for parallel iterations (line 188)
- Use per-thread isolated stores (same as Step 2)
- Merge results after executor completes

### Step 4: Serialize `on_write` callbacks

Deferred to after merge (one call from main thread). No additional serialization needed with per-thread isolation approach.

### Step 5: Add concurrency tests

**File:** `tests/core/test_tree_concurrency.py` (new)

1. `test_tree_effects_do_not_share_mutable_state`
2. `test_tree_effects_see_snapshot_not_sibling_writes`
3. `test_loop_tree_iterations_isolated`
4. `test_loop_tree_deep_context_isolation`
5. `test_on_write_called_once_after_tree_merge`
6. `test_stress_concurrent_store_ensure_dict` (50 threads)

### Step 6: Update all `Store(node, on_write=...)` call sites

Replace manual construction with `store.child(name)` in:
- `dynamic.py` (line 126)
- `loop.py` (lines 144, 504)
- `conditional.py` (line 117)
- `reflector.py` (lines 72, 205)

## Performance

- Single `RLock` per root store: uncontended acquisition ~50ns, negligible vs network I/O
- `deepcopy` cost: sub-millisecond for typical orchestration state
- Per-thread isolation eliminates lock contention during parallel phase entirely
- `on_write` batching avoids N redundant file writes

## Files to Change

| File | Change |
|------|--------|
| `src/circuitry/core/store/store.py` | Add `RLock`, wrap mutations, add `child()` |
| `src/circuitry/core/dynamic.py` | Per-thread isolated stores, merge after completion |
| `src/circuitry/core/loop.py` | `deepcopy` for iter contexts, per-thread isolated stores |
| `src/circuitry/core/conditional.py` | Use `store.child()` |
| `src/circuitry/core/reflector.py` | Use `store.child()` |
| `tests/core/test_tree_concurrency.py` | New file with 6 tests |
