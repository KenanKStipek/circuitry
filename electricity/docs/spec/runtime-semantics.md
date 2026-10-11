# Circuitry Runtime Semantics

**Source of truth.** Circuitry (Python `cof`), git commit
`82239cd8b93e197dc103fd67a87f521ecc5001a1`
("remove the step cache (cache:, cof cache, --no-cache) (#353)"). All file:line
citations below are against that commit. All JSON examples were produced by
running this checkout's own code, in a local Python virtual environment with a scratch `HOME`,
against small scratch orchestrations — not hand-written — using a scripted
adapter (`GenerateResult(text=..., tokens_sent=..., tokens_received=...)`) and
the real `shell` tool plugin. Timestamps/hashes are real but non-reproducible;
everything else is exact.

**Scope.** This document specifies what `cof run <doc> -c <config>` *does*:
loading, compilation, the state model, template/CEL evaluation, every effect
type's execution contract, error handling, concurrency, and run lifecycle —
precisely enough that a from-scratch engine (electricity) can produce a
byte-for-byte identical final state (`--out` JSON) for the same document,
config, inputs, and scripted model/tool replies. Tool plugins and model
adapters are referenced only at their interface (`ToolResult`, `GenerateResult`);
their own behavior is a separate specification, out of scope here.

**Terms.** "Effect" = one compiled node in the orchestration tree (prompt,
tool, use, loop, dynamic/conditional container, reflector). "Node" = the
`{value, meta}` dict an effect writes into state. "Document" = the YAML/JSON
orchestration file.

---

## Table of contents

1. [Loading](#1-loading)
2. [The state model](#2-the-state-model)
3. [Templates](#3-templates)
4. [CEL](#4-cel)
5. [Effects](#5-effects)
6. [The error model](#6-the-error-model)
7. [Concurrency](#7-concurrency)
8. [Run lifecycle and outputs](#8-run-lifecycle-and-outputs)
9. [Quirks and apparent bugs](#9-quirks-and-apparent-bugs)
10. [Conformance suite](#10-conformance-suite)

---

## 1. Loading

### 1.1 YAML — PyYAML `safe_load`, minus silent duplicate keys

`core/yaml_load.py:52` `load_yaml()` runs `yaml.load()` with a custom
`_UniqueKeyLoader(yaml.SafeLoader)` (`core/yaml_load.py:28`) that overrides
`construct_mapping` to raise `DuplicateKeyError` (`core/yaml_load.py:24`,
subclass of `yaml.YAMLError`) the moment the *same* mapping defines a key
twice — citing both line/column locations. A `<<:` merge key is explicitly
exempted (`core/yaml_load.py:34`, checks `key_node.tag == "tag:yaml.org,2002:merge"`):
merging is not duplication, and the merged-in keys are overridden by the
mapping's own keys, exactly as YAML merge-key semantics define (confirmed:
`base: {x:1,y:2}` merged with `{<<: *base, y: 3}` yields `{x: 1, y: 3}`).

Because it is otherwise plain `SafeLoader`, every YAML 1.1 quirk PyYAML's
safe loader implements is in play — confirmed by direct experiment:

| YAML text | Python value | Type |
|---|---|---|
| `on` / `yes` | `True` | `bool` |
| `off` / `no` | `False` | `bool` |
| `0x1A` | `26` | `int` (hex) |
| `017` | `15` | `int` (octal — **not** `0o17`, which PyYAML leaves as the string `"0o17"`) |
| `1:30:00` | `5400` | `int` (sexagesimal: `1*3600+30*60+0`) |
| `2024-01-01` | `datetime.date(2024, 1, 1)` | `date` |
| `.inf` / `.nan` | `inf` / `nan` | `float` |

An electricity YAML loader that accepts only YAML 1.2 booleans/octal would
silently diverge on any document using `on/off/yes/no` or leading-zero octal
— both common in hand-written orchestrations.

Every `.yml`/`.yaml` document the runtime loads — a file named on the command
line, a library ref, a `use: {path: ...}` child, a `use: {inline: ...}`
child's rendered text — goes through this same loader
(`core/use.py:514` for inline, `cli/orchestration_loader.py` for files).

### 1.2 JSON — `json.loads`, minus silent duplicate keys

`.json` documents go through `core/json_load.py:57` `load_json()`, which uses
`object_pairs_hook` to build `_RawObject` markers bottom-up
(`core/json_load.py:24`) and then walks top-down (`_materialize`) raising
`DuplicateKeyError` (`core/json_load.py:20`, subclass of `ValueError`) naming
the key and its dotted path (e.g. `nested.x`) the first time a key repeats in
one object. Confirmed: `{"a":1,"nested":{"x":1,"x":2}}` raises
`duplicate key 'x' in nested; JSON would silently keep only the last one.`

### 1.3 Schema, structural checks, and which ones block a run

`core/document_check.py:339` `structural_errors()` is the one gate `cof run`,
`cof check`, and every `use` child (`core/use.py` → `structural_errors`) all
call, in this order, and *any* non-empty result blocks the run with
`"Orchestration validation failed:\n  - ..."`:

1. **Unknown-key errors** (`core/document_check.py:201` `unknown_key_errors`) —
   a key that is a *near miss* (edit-distance ≥0.8, or a hardcoded
   confusable like `adapter` on an effect meaning `provider`,
   `document_check.py`'s `_MISTAKEN_FOR`) of a key the effect type/top level
   actually has. E.g. `whlie:` on a loop, `max_iteration:` instead of
   `max_iterations:`. These are **errors**, not warnings — the schema itself
   allows unknown keys (effects and the top level are schema-open), so a typo
   would otherwise be silently ignored.
2. **JSON-schema errors** (`core/document_check.py:116` `schema_errors`) —
   Draft7 validation against `schema/orchestration.schema.json`, reported as
   `<json-path>: <message>` with the deepest `oneOf` sub-error appended when
   one exists (effect-type dispatch uses nested `if/then`/`oneOf`, so the
   top-level message is often uninformative alone). `electricity-schema`
   (DESIGN.md §4 step 3, issue #380) matches `<json-path>: <message>`
   exactly but does not append this suffix: the `jsonschema` crate version
   it uses (0.26) carries no sub-error list on its `oneOf`/`anyOf` error
   variants to pick the best match from, unlike Python's own `jsonschema`.
3. **`group:` placement errors** (`core/document_check.py:211`
   `group_field_errors`) — `group:` is only legal on `tool`/`prompt` (leaf)
   effects; a container (`dynamic`/`loop`/`if`/`reflector`/`use`) never
   dispatches itself so it can never hold the concurrency-group slot the key
   would name (#274).
4. **`interface.inputs.<k>.type` unknown** (`core/document_check.py:269`) —
   must be one of `string|number|integer|boolean|array|object`.
5. **`interface.inputs.<k>.default` type mismatch** (`core/document_check.py:302`)
   — a YAML/JSON default is already typed data, not CLI text to coerce, so
   `default: "3"` for `type: integer` is a hard error (with a hint to unquote
   if the unquoted text would itself satisfy the type).

Everything **not** in that list is a **warning**, never blocking:
`unknown_key_warnings()` (`core/document_check.py:206`, a key resembling no
known key at all — may be deliberate) and all of `core/lint.py:74`
`lint_orchestration()`'s advisory warnings — deprecated effect-type/flow
aliases (`conditional`→`if`, `cot`/`chain_of_thought`→`chain`,
`tot`/`tree_of_thought`→`tree`), an effect named after its own type
(`name: loop`), `params.args` entries YAML silently coerced off string
(unquoted `0x1`→`1`, `off`→`False`), `min_iterations` on an `each` loop (no
effect), `threshold:` on an `if`/`conditional` (recorded but never consulted
by the built-in evaluator), and the loop-body reference footguns —
`prime.<loop>.iter_<N>.<step>` referenced *inside that same loop*
(resolves to stale data from pass N, never fails), `prime.<loop>.last.<step>`
inside the body (resolves to the *previous* run's last pass at best), and
`prime.<loop>.<body_step>` (never resolves at all — `prime.<loop>` is the
loop's own node, not a scope its body lives in).

### 1.4 Compilation

`core/compiler.py:302` `compile_orchestration()` turns the raw dict into an
immutable `DynamicDefinition` tree rooted at `root_name="prime"`
(always `"prime"` for a top-level run; a `use` child also compiles with
`root_name="prime"`, isolated in its own state — see §5.3). Per-effect
compilation (`_compile_prompt`/`_compile_tool`/`_compile_use`/`_compile_loop`/
`_compile_conditional`, `core/compiler.py:1289/1083/1169/822/723`) additionally
raises on things the schema itself cannot express:

- A malformed Mustache template anywhere a template renders (`_check_templates`,
  walks every string leaf of `params`/`template`/`messages[].content`/
  `inline`/`expect.template`/`while.template`/`if.template` recursively) —
  `core/templates.py:32` `template_syntax_error()`, via `chevron.tokenizer.tokenize`.
- A `mode: cel` expression that does not parse CEL syntax
  (`cel_eval.validate_cel_syntax`, `core/cel_eval.py:212`) for every `if.expr`,
  `while.expr`, and `expect` CEL string/`{mode: cel, expr: ...}`.
- A `state.<key>` reference where `<key>` is not `input`/`prime`/`runtime`
  or a name an *enclosing loop* binds (`validate_cel_expr`,
  `core/state_ns.py:172`) — this is a hard **compile-time** error, not a
  runtime warning; it inspects the CEL parse tree (`state_paths`,
  `core/cel_eval.py:253`), so a quoted string `'state.foo'` is exempt.
- A `{from: <path>}` by-reference leaf (tool `params`, `use` `inputs`) whose
  path is not rooted at a namespace or loop binding (`validate_reference_path`,
  `core/state_ns.py:93`), and the `each.in` equivalent (`validate_each_in_path`,
  `core/state_ns.py:130`).
- A bare `{{name}}` template reference that collides with a name this
  document's own `interface.inputs` declares (`validate_bare_input_refs`,
  `core/state_ns.py:221`) — caller inputs live under `input.`, so a bare
  reference to a declared input name is always the author's mistake, not a
  loop/sibling binding (those are never declared in `interface.inputs`).
- Effect-name validation (`_validate_name`, `core/compiler.py:54`): non-empty,
  no leading/trailing/internal whitespace, no `.`, not the pattern `iter_\d+`
  (reserved for loop passes), not one of `{value, meta, input, prime, runtime}`
  (collides with structural slots the runtime itself writes), and must match
  `^[A-Za-z_][A-Za-z0-9_]*$`. Duplicate names within one scope are a compile
  error (`core/compiler.py` `_compile_effects_in_scope`, tracked per scope via
  `seen_names`, shared between a container's own body and its `finally:` list
  since both land in the same state node).
- A security-sensitive param (`allowed_commands`, the shell plugin's own
  allowlist) must be a literal list of strings — no `{{` template, no
  `{from: ...}` reference, anywhere inside it (`_check_security_sensitive_param_leaf`,
  `core/compiler.py`; `_SECURITY_SENSITIVE_PARAM_KEYS = {"allowed_commands"}`,
  `core/tool.py:271`) — enforced again at dispatch (`_reject_templated_security_params`,
  `core/tool.py:274`).
- `group:` names must exist in `runtime.concurrency_groups`
  (`unknown_concurrency_group_errors`, `core/compiler.py`) — checked once the
  run's concurrency limiter is built, after compilation, including inside a
  `use` child against the *same* run-wide group table (`core/use.py` §5.3).
- A `use`/tool `expect` must set exactly one of a bare CEL string or
  `{mode: cel, expr}`/`{mode: model, template}` (`_compile_expect`,
  `core/compiler.py`).
- A tree-flow (`flow: tree`) `each` loop body must not reference
  `prime.<loop>.prev` (`core/compiler.py` `_tree_loop_prev_references`) —
  tree passes run in parallel, so there is no well-defined previous pass.
- A `use` effect must set exactly one of `ref`/`path`/`orchestration`
  (deprecated)/`inline`.
- Prompt composition (#396, `core/prompt_compose.py` `check_prompt_composition()`):
  a `{{> name}}` naming neither a declared `prompts:` entry nor an effect
  anywhere in the document; a name that is both a declared prompt and an
  effect name; a `{{> name}}` naming an effect that is neither `yield` nor a
  `prompt` with `prompt_type` unset/`text`; a cycle among declared prompts'
  own `{{> name}}` references. A `yield` effect additionally rejects the
  model-only keys listed in §5.9 at compile time (schema
  `additionalProperties: false` on `YieldEffect`). A `{file: <path>}` prompt
  source (declared prompt, `template`, `messages[].content`) is read and
  validated at this same compile step — see §3.6.

### 1.5 `interface.inputs`

Declared per-input `{type, required, default}`, enforced identically at
every entry point — a top-level run (`cli/runtime_shim.py:618` `run()`, right
after `state["input"]` is established) and a `use` child
(`core/use.py:446` `_check_interface`) — by one shared function,
`core/interface_inputs.py:67` `check_interface_inputs()`:

- A key **absent or `None`** (an unresolved `{from: ...}`, a `-e x=null`, a
  `"x": null` in `--state`) gets its `default:` filled in (deep-copied); if
  there is none and `required: true`, raises; if there is none and not
  required, the key is dropped entirely (an optional unset input is not
  passed at all, not passed as `None`).
- The six recognized `type`s — `string, number, integer, boolean, array, object`
  — are checked with `_matches_type` (`core/interface_inputs.py:23`): `number`
  accepts `int` or `float` but not `bool`; `integer` accepts `int` but not
  `bool`; `boolean` requires exactly `bool`.
- A value that's already the wrong *Python* type but is a `str` is coerced
  (`_coerce`, `core/interface_inputs.py:39`): `number`→`int` then `float`;
  `integer`→`int`; `boolean`→ lenient word list
  `{true,t,yes,y,on,1}`/`{false,f,no,n,off,0}` (case/whitespace-insensitive;
  anything else raises); `array`/`object`→`json.loads`. This is how a CLI
  `-e start=10` (arrives as the Python `int` `10` after `-e`'s own JSON
  sniffing — see §8) *for a declared* `type: string` input is turned back
  into the text `"10"` (the inverse direction, `number/boolean→string`, uses
  `json.dumps`, not `str()` — so `True`→`"true"`, not `"True"`).
- A value that is already the right type, or cannot be coerced, is left
  alone or raises `ValueError` prefixed with the caller's `label` (e.g.
  `"Use effect 'sub': "`).

### 1.6 `-e key=value` and `--state`

`cli/app.py:345` `_parse_env_vars()`: for each `KEY=VALUE`, try `json.loads(VALUE)`
first (so `-e n=5` → Python `int 5`, `-e flag=true` → `bool True`,
`-e items='[1,2]'` → `list`); on any `json.JSONDecodeError`/`ValueError`, fall
back to the literal string. This means `-e start=06` (no quotes) is first
JSON-sniffed — `json.loads("06")` is **invalid** JSON (leading zero), so it
falls back to the string `"06"` — but `-e x=1.50` JSON-parses to the `float`
`1.5`, silently dropping the trailing zero, *unless* `interface.inputs.x.type`
is `string`, in which case `cli/app.py:372` `_restore_raw_text_for_string_inputs`
peeks the document's interface and substitutes the original unparsed text
back in for any key declared `type: string` — best-effort, never blocks the
run if the peek itself fails. The parsed values land under `input.` as
ordinary extra state before `interface.inputs` checking runs. `--state <file>`
loads a whole JSON state tree (`cli/runtime_shim.py:352` `_load_state`) through
the same `migrate_legacy_state`/`link_last_refs` pipeline every state source
goes through (see §2.1, §2.3).

---

## 2. The state model

### 2.1 Root namespaces

Exactly three (`core/state_ns.py:47` `NAMESPACES = ("input", "prime", "runtime")`):
`input` (caller-supplied — CLI `-e`/`--state`, profile `inputs`, `use.inputs`,
REST/scheduler payloads), `prime` (effect outputs, rooted at the compiled
orchestration root), `runtime` (framework metadata — `last_run`,
`effective_settings`, `plugins`, `persistence`). A legacy state dict with
bare root keys (no `input` key at all) is migrated once, idempotently, by
`migrate_legacy_state()` (`core/state_ns.py:58`): every root key that is
neither a namespace nor `_`-prefixed (`_run_id`, `_timestamp`) is moved under
`input`. This runs at every hydration point: CLI `--state`, `-e`-only runs
with no `--state` file at all (confirmed: `{"foo": 1, "bar": {"x": 2}}` →
`{"input": {"foo": 1, "bar": {"x": 2}}}`), and persisted-state resume.

Outside CEL, every path is root-relative (`input.items`, `prime.x.value`).
Inside CEL, `state` binds to the state root, so the equivalent is
`state.input.items`. There is no "sugar" layer reconciling the two — a
`state.`-prefixed string **outside** CEL, or a bare key with no namespace and
no enclosing-loop binding, is a hard compile-time error (§1.4).

### 2.2 Node shape

Every effect writes `{"value": <any>, "meta": {...}}` at its own name in the
enclosing scope. `meta` fields that are *common to every effect type*:
`created_at`/`completed_at` (ISO-8601 UTC, `completed_at: null` until the
effect finishes — the resume contract, §6.5/§8.5, keys off exactly this pair
plus `error`), `error` (`None` on success, else `str(exception)`).
Container effects (dynamic/loop/conditional) additionally always carry
`meta.labels` (the `labels:` map, or `null`) — free-form observability
tagging with zero runtime effect.

Per-effect-type `meta` (confirmed field sets from live runs):

- **prompt** (`core/prompt.py:453` docstring, confirmed live):
  `adapter, model, model_reason ("explicit"|"router"|"default"), prompt_type,
  prompt_sent, tokens_sent, tokens_received, tokens_sent_total,
  tokens_received_total, dry_run, fallback_attempts[], fallback_recovered,
  waiting_for, retries_used?, answer? (boolean/number prompt_type only),
  complexity? (scoring enabled only), band? nested under complexity,
  decomposition? (decomposition triggered only), finish_reason?, warnings?,
  assets?`.
- **tool** (`core/tool.py:399` docstring, confirmed live): `provider,
  params_rendered (redacted), stdout, stderr, exit_code, binary? (process
  plugins), status_code? (HTTP-family only), raw (redacted + size-capped),
  waiting_for, retries_used?, expect?`.
- **use** (`core/use.py:271` `UseDefinition`, confirmed live): `orchestration,
  inline (bool), resolved_path, validation_errors, child_errors,
  inputs (redacted-free copy of rendered child inputs), orchestration_sha256,
  library_ref? (pin, ref: children only), expect?, retries_used?,
  finally_error?`.
- **loop** (named only — unnamed loops write no node of their own, see §2.4):
  `mode ("each"|model/cel string for while), each_in_path?, each_as?,
  max_iterations?, min_iterations, labels, completed_passes[], failed_passes?,
  progress {done, total, elapsed_s, eta_s}, answer/adapter/model/tokens_* (
  while mode:model only), error, each_in_error? (collection_unresolved only)`.
  Node `value`: `{"iterations": N, "termination": {"reason": ..., "unvisited"?,
  "detail"?}, "effects_by_iteration": [...]}`.
- **dynamic** (`core/dynamic.py:94`): `adapter, model, tokens_sent/received
  (always null — a dynamic never dispatches itself), flow, dry_run, labels,
  finally_error?`. Node `value`: `True` on success, `False` on any failure
  (body or `finally:`).
- **conditional/if** (named only, `core/conditional.py:54`): `mode, threshold,
  labels, condition_result, branch ("then"|"else"), answer/adapter/model/
  tokens_* (mode:model only)`. Node `value`:
  `{"result": bool, "branch": "then"|"else", "effects": [{"index","type","name"}]}`.
- **reflector** (`core/reflector.py:72`): `meta.iterations` — a list of one
  record per iteration (`{i, plan_from_step, done, stop, parsed, error,
  plan_text}`). Node `value`: `True`/`False`. Generated effects run under
  `prime.<reflector>.<generated_key>.iter_<i>.*` (`generated_key` default
  `"generated"`).

### 2.3 Loop nodes: `iter_N`, `last`, `collected`, `termination`

A **named** loop creates a node `prime.<loop>` holding, per pass, a child
key `iter_0`, `iter_1`, ... (one per completed *or* failed pass, in pass
order) each shaped like an ordinary container's children — confirmed live
(see §5.4 for the full example). `last` is an **alias**, not a copy
(`core/loop.py:1059` `_link_last`): `node["last"] is node["iter_<final>"]`
in memory, where `<final>` is the index of the last pass that completed
without error (skipping a `continue`/`break`-absorbed failure). A
zero-iteration loop writes no `last` key at all. `collected` (only present
when `collect:` is set) is `{"value": [...]}`, one entry per pass that
*completed* (via `completed_indices`, tracked live, never by scanning stale
`iter_N` keys that may predate this run — important under `--state` resume
or an unnamed-outer-loop reuse of the same node) and whose `collect:` target
itself produced a value — a disabled target, or a pass that failed before
reaching it, is elided entirely, not recorded as `None`
(`core/loop.py:1079` `_collect_values`).

`termination.reason` is one of: `collection_exhausted` (each, ran every
item), `max_iterations_reached` (hit the cap — either a `while`/`each
truncate: true` cap, or an each-loop whose collection was longer than the cap
under `on_error: break/continue`), `collection_unresolved` (each.in path
didn't resolve to a list at all — distinct from "exhausted" so a misspelled
path isn't silently mistaken for zero items; `meta.each_in_error` names the
failure), `condition_false` (while, condition genuinely became false),
`condition_error` (while, the condition itself raised, absorbed by
`on_error: break/continue`), `error` (a body pass failed under
`on_error: break`, or the each-loop bounds check failed under
`on_error: break/continue` — `termination.detail` carries the message in
that one case, since there is no failed per-pass node to carry it).

**`$ref` compaction in saved state.** Because `last` aliases a sibling in
memory, a plain `json.dumps` would serialize that pass's content *twice*
(and recursively double for nested loops). `core/saved_state.py:34`
`compact_last_aliases()` rewrites every `last` that *is* (by `is`, not `==`)
one of its own siblings into `{"$ref": "iter_<N>"}` before any save
(`--out`, `--print`, `--live-state`, persistence); `link_last_refs()`
(`core/saved_state.py:83`) is the inverse, applied on every load path
(`--state`, persistence resume, SDK `initial_state`). A `last` that holds a
genuinely independent dict — a user-named effect literally called `last`, or
state saved before this form existed — is left untouched (checked by
identity against the actual sibling values, not key-name pattern-matching).

### 2.4 Scope overlays

**Named vs. unnamed**: a *named* loop/conditional/dynamic gets its own node
and its body/branch runs in a `child_store` nested under that name. An
*unnamed* loop/conditional (`name: null`) is "transparent control" — its body
writes directly into the **enclosing** scope, with no node of its own at
all; only loops and conditionals support this (dynamic and use always
require a name).

**`scope_ctx`** (`core/scope.py:7`) is how a loop body / conditional branch
exposes a sibling's output to the *next* sibling in the same body/branch,
under **two** spellings simultaneously: the bare top-level form
(`{{step.value}}`) and the canonical `prime`-nested form
(`{{prime.step.value}}`). It is a **shallow** merge: `{**ctx, **local}`, plus
the same `local` merged one level inside `ctx["prime"]`. This overlay is
rebuilt from the original `ctx` after *every* sibling in a loop body /
conditional branch (never layered on the previous overlay) — confirmed in
`core/loop.py` (`_execute_body`, `base_ctx = ctx` then rebuild per iteration)
and `core/conditional.py` (`_decide_and_run`, same pattern).

**Shadowing**: `local_writes()` (`core/scope.py:30`) keeps a name "local" to
the current container if it is in `own_names` (the body/branch's own effect
names) even when that name was already present in the *baseline* (keys that
existed before this body/branch ran) — so a body step reusing an enclosing
name wins inside the body, and the enclosing name returns outside it.

**`prime.<loop>.prev`** — chain flow only, named loops only
(`core/loop.py` `_with_prev`): the *previous completed pass's* own writes,
exposed at `prime.<loop>.prev.<step>.value` inside the next pass's body
(and nowhere else). Absent — not an empty node — before the first completed
pass, so `{{prime.<loop>.prev.x.value}}` renders empty and CEL `has()` reads
false on pass 0.

**`iter.index` / `iter.count`** — a loop's CEL/template-visible iteration
counter, pushed into `ctx` per pass: for `each`, `iter.index` is the current
0-based element index (confirmed via `_loop_index`/`iter` keys set in
`core/loop.py`'s each-mode iteration-context build). For `while`, the
*condition* (evaluated *between* passes) sees
`iter: {index: iteration_count - 1, count: iteration_count}` — i.e. the pass
that just finished (−1 before the first pass ever runs), matching the
`+1`-compensated convention older conditions already wrote; the **body**
sees the uncompensated `iter.index = iteration_count` (the pass about to
run). This asymmetry is deliberate (`core/loop.py`, extensive inline
comments) and must be reproduced exactly, including the off-by-one between
condition-time and body-time `iter.index` for `while` loops.

**Top-level root is NOT a scope-overlay container.** A plain `dynamic`
container — including the document root itself, which compiles to a
`DynamicDefinition` named `"prime"` — never calls `scope_ctx`/`local_writes`
at all (confirmed by source inspection of `core/dynamic.py`: no reference to
either). Its `ctx` for every child is simply `store.state` **by reference**
(`core/dynamic.py` `execute`: `ctx = store.state if ctx_override is None else
{**ctx_override, **store.state}`), so a later sibling's fully-qualified
`{{prime.<earlier>.value}}` resolves because it is reading the *same live
dict* earlier siblings already wrote into — but the **bare** form
`{{<earlier>.value}}` does **not** resolve at plain top level, because no
overlay ever puts `<earlier>` at the top of `ctx`. Confirmed live:

```yaml
effects:
  - {type: tool, name: first, provider: shell, params: {command: echo, args: [AAA]}}
  - {type: prompt, name: second, template: "bare={{first.value}} dotted={{prime.first.value}}"}
```
renders `prompt_sent: "bare= dotted=AAA\n"` — the bare form is empty, the
dotted form resolves. **This only matches the documented "bare form also
works" rule inside a loop body or an `if` branch** (where `scope_ctx` *is*
applied) — see §9 Quirk Q1, a critical parity risk.

---

## 3. Templates

### 3.1 Engine and escaping

Mustache via `chevron` (`core/templates.py`). `{{name}}` HTML-escapes;
`{{{name}}}`/`{{&name}}` does not. Confirmed: `{{s}}` on `"<b>&'\""` →
`"&lt;b&gt;&amp;'&quot;"` (both `<`/`>`/`&` and `"` escaped, `'` left alone —
this is `chevron`'s own escaping table). This is per-call, not a global
setting (#397): `render_template(..., escape=False)` makes *every* `{{name}}`
in that one render behave like `{{{name}}}` — implemented
(`core/templates.py`) as a `contextvars.ContextVar` read by a module-level
replacement of chevron's own `_html_escape`, set/reset around the one
`chevron.render()` call, so it never leaks across a concurrent render on
another thread. `render_template`'s own default is unchanged (`escape=True`);
only the call sites in §3.4 marked "prompt text" pass `escape=False`. Sections
`{{#x}}...{{/x}}` iterate a list (binding each element as the section's
context — `{{.}}` for a scalar element, `{{field}}` for a dict element) or
render once if `x` is truthy-and-not-a-list (a dict or `True`); inverted
sections `{{^x}}...{{/x}}` render iff `x` is falsy/empty/absent. Dotted names
(`{{a.b.c}}`) walk nested dicts and render empty on any missing/absent
segment — never an error (confirmed: `{{a.b.c}}` on `{"a":{"b":{}}}` → `""`).
A template referencing a key entirely absent from context also renders empty,
not an error (confirmed: `{{missing}}` on `{}` → `""`).

### 3.2 Python value → text

Confirmed exact renderings (same for `{{}}` content and `{{{}}}`, modulo
escaping):

| Python value | Rendered text |
|---|---|
| `True` | `True` |
| `False` | `False` |
| `None` | *(empty string)* |
| `42` (int) | `42` |
| `1.0` (float) | `1.0` |
| `1e-05` (float) | `1e-05` |
| `[1, 2, 3]` (list) | `[1, 2, 3]` (Python `repr`-ish via `str()`) |
| `{"a": 1}` (dict) | `{'a': 1}` (Python `str()`, single-quoted — **not** JSON) |

A list/dict in an ordinary Mustache context renders via plain `str()` —
Python repr syntax, not JSON — **unless** the value was wrapped by
`core/tool.py`'s `_JsonAwareDict`/`_JsonAwareList` (`core/tool.py:195`),
which override `__str__` to emit real `json.dumps(..., default=str)`. That
wrapping is applied **only** to the context used for rendering a tool's
`params_json` template (`_json_aware_ctx`, `core/tool.py:208`) — a plain
`template:`/`messages[].content` prompt field, or a tool's ordinary `params`
leaves, get the Python-`repr` rendering for any list/dict value that reaches
a `{{{...}}}` splice. This asymmetry is deliberate and must be preserved
exactly — see §9 Quirk Q2.

### 3.3 Render errors

`render_template()` (`core/templates.py:42`) raises `TemplateError`
(`ValueError` subclass) on a `ChevronError` (malformed tag — but this is
already caught at compile time, §1.4, for every static template; a render
error at runtime means the *rendered-against* data, not the template text,
is somehow unrenderable) or any other exception chevron raises. A render
failure is this **effect's own failure**, handled by that effect's
`on_error` exactly like a dispatch failure — never silently sent as raw,
unrendered text (confirmed in `core/prompt.py:453` `execute()`: a
`_materialize_input`/`_render_messages` exception sets `meta.error`, fires
`effect_complete`, and re-raises under `on_error: fail`).

### 3.4 Where templates render

Every string-bearing field below is Mustache-rendered against the current
`ctx` (root state plus whatever the enclosing scope overlay/sibling-merge
adds, §2.4). Each is marked **prompt text** (`escape=False`, §3.1; `{{>
name}}` composition, §3.6 — both together) or **escaped** (chevron's
default; `{{> name}}` composition still applies to the four marked so, but
anything else in the same template keeps escaping):

| Field | Escaping | `{{> name}}` |
|---|---|---|
| prompt `template` | prompt text | yes |
| prompt `messages[].content` | prompt text | yes |
| `yield.template` (§5.9) | prompt text | yes |
| declared `prompts.<name>` (§3.6) | prompt text | yes |
| tool `prompt` | escaped | yes |
| tool `params` (every string leaf, recursively, except `{from:...}`, §3.5) | escaped | yes |
| tool `params_json` (one template, `json.loads`-parsed after rendering — not leaf-by-leaf) | escaped | yes |
| `use.inline` (rendered **once**, against the *parent's* context, before the result is parsed as YAML — §9 Quirk Q3) | escaped | yes |
| `use.inputs` (every string leaf that is not a `{from:...}` reference) | escaped | yes |
| `if.template`/`while.template` (model-mode conditions) | escaped | no — unconditional partial error |
| `expect.template` (model-mode expect) | escaped | no — unconditional partial error |
| asset `ref` (image paths/URLs) | escaped | no — unconditional partial error |

"No — unconditional partial error": these three never reach §3.6's expansion
at all, so a `{{> name}}` tag in one of them is always the plain partial
error §3.3/§1.4 already describe for any other malformed tag — identical
before and after #396.

### 3.5 `{from: path}` references

A leaf exactly `{"from": "<path>"}` (optionally `{"from": ..., "default": ...}`)
in a tool's `params` or a `use` effect's `inputs` is a **by-reference** value
— the resolved value at `<path>` (walked via `core/use.py:159`
`_resolve_reference`, which also indexes into lists by integer segment) is
passed through **deep-copied**, **typed**, **never rendered as a template**.
If the path doesn't resolve (any segment missing, or resolves to `None`):
with `default:` present, use the default (deep-copied); without it, raise
(tool: `"'{from: X}' did not resolve to a value"`; use: the input is set to
`None`, letting the ordinary required/type check in `interface.inputs` catch
it, or — if that input is itself `required` in the child's
`interface.inputs` — a more specific `"required input 'X' resolved to
nothing from 'PATH'"` message, `core/use.py:446`). `{from: ...}` is rejected
at compile time for any value that is syntactically a mapping with keys
other than exactly `{from}` or `{from, default}` — any other mapping shape
(extra keys) is a literal value, passed through unrendered as a plain dict.

### 3.6 Prompt composition: `{{> name}}`, declared `prompts:`, and prompt files (#396)

`core/prompt_compose.py`. Expanded **before** a template ever reaches the
ordinary Mustache render (§3.1) — `render_template()` itself is unchanged
and still unconditionally rejects an unexpanded `{{> name}}` with today's
message (`core/templates.py` `_reject_partials`); electricity's own
template corpus is generated from that function, so this expansion is a
separate, earlier pass, not a change to it.

**Declared prompts.** A document's top-level `prompts:` is a map of name to
either a string or `{file: <path>}` (§3.6.2). Compiled once per document into
`{name: raw_template_text}` — `file:` sources are read at **compile** time
(§1.4), the text itself is not rendered until it is actually spliced in.
Carried on the compiled root `DynamicDefinition.prompts` (`core/dynamic.py`)
and threaded into the run's shared `runtime_config` dict under the private
key `core/prompt_compose.py:RUNTIME_CONFIG_KEY` (`"_prompts"`) — the same
ambient-dict convention as `_orchestration_dir`/the concurrency limiter.
Every leaf effect reads it from there at render time
(`declared_prompts(self.runtime_config)`). A `use` child gets this key
**overwritten**, never merged, with its own document's `prompts:` (`{}` if
it declares none) — "a child document sees only its own declared prompts".
A document generated at run time (a reflector/decompose plan, a
`use: inline` child) compiles with no file of its own (§3.6.2), but *can*
declare its own `prompts:` inline — only `file:` sources require a file.

**Expansion algorithm**, run against a template string *T* and a context
*ctx* (the same ctx `render_template` would use), before *T* reaches
`render_template`. The whole expansion — including every declared prompt
*T* pulls in, recursively — shares one synthetic-key counter and one
`extra` dict of resolved effect values (step 2b below): a per-call counter
would let two sibling references, nested inside two different declared
prompts, mint the same key and overwrite each other's value.

1. Find every `{{> name}}` tag in *T* — matched the same way chevron's own
   tokenizer does (the sigil `>` must immediately follow `{{`, no leading
   space; `name` is everything up to `}}`, stripped). A name must match
   `^[A-Za-z_][A-Za-z0-9_]*(\.[A-Za-z_][A-Za-z0-9_]*)*$` (optionally dotted);
   anything else is a render-time error (`TemplateError`).
2. For each tag, in turn:
   - **`name` (no dot) is a declared prompt (2a):** recursively run this
     same algorithm on that prompt's own raw text, against the *same* ctx
     and the *same* shared counter/`extra` — producing a rewritten
     fragment (zero `{{> ...}}` tags left in it, but still carrying
     whatever `{{x}}`/`{{{x}}}`/`{{#section}}` tags it had, unrendered).
     Re-entering a name already being expanded (directly or through
     another declared prompt) is a cycle — render-time error; `cof check`
     (§1.4) already rejects a cycle that is purely among declared prompts
     statically, so reaching this at run time means a generated document.
     Rewrite every `variable`/`no escape` tag in the fragment to an
     explicit no-escape tag (`{{x}}` and `{{{x}}}` alike become `{{&x}}`,
     reusing chevron's own tokenizer to reproduce sections/comments
     correctly) — a declared prompt's own tags never HTML-escape, even one
     spliced into a field that otherwise does (#397) — and reject a
     `set delimiter` token the same way `cof check` does statically (a
     declared prompt may not change the delimiters of whatever template it
     lands in). Drop exactly one trailing `\n`/`\r\n` from the fragment,
     then **splice its text directly into *T*'s own source**, replacing
     the tag's span character-for-character — not a value bound to a
     context key: chevron will tokenize and render this text as if it had
     always been part of *T*, seeing whatever Mustache section scope the
     `{{> name}}` tag itself sat inside (a fragment reused inside a
     `{{#list}}` section sees each item, not one shared top-level context —
     *not* a once-only, outer-ctx-only simplification).
   - **Otherwise, an effect reference (2b):** walk `ctx["prime"]` by
     `name`'s dot-separated segments, then `.value` — exactly the same
     dict/list walk `{from: path}` references use (§3.5): a `Mapping`
     indexes by key, a list/tuple by integer segment, anything else or a
     missing key stops the walk. If the **first** segment is not even a
     key of `ctx["prime"]`: a real effect name somewhere else in the
     *compiled* document (its first segment is in the compiled root's own
     effect-name set, gathered once, regardless of nesting — an untaken
     `if` branch, an unscheduled `flow: tree` sibling, one later in a
     chain) resolves to `""`, the same as a `None` below; genuinely unknown
     — not even a name anywhere in the document — is the
     render-time-backstop error (`TemplateError`), for a generated
     document not statically checked (§1.4's static check already catches
     an unknown name for anything that *is* statically checkable).
     Otherwise, a walk that ends in `None` (missing deeper segment, or an
     effect whose `value` is `None` — skipped, or failed under
     `on_error: continue`) resolves to `""`; otherwise `str()` of whatever
     it finds (same stringification §3.2 describes for an ordinary
     `{{{...}}}` splice). Drop exactly one trailing `\n`/`\r\n` from the
     resolved text. Bind it to a fresh synthetic context key (the one
     counter shared by the whole expansion, e.g. `__circuitry_partial_0__`)
     added to a **shallow copy** of *ctx* — the original
     `input`/`prime`/`runtime` objects are untouched, only new top-level
     keys are added — then replace the tag's own span with `{{{<synthetic
     key>}}}`: a same-shaped triple-stache tag, not a text splice, so the
     resolved text (which may itself contain literal `{{`/`}}`, e.g. a
     model reply) is inserted **verbatim** by the render in step 3 below —
     chevron appends a triple-stache value directly to its output
     accumulator without re-tokenizing it, so it can never be parsed as a
     new tag, even one that looks like `{{=<% %>=}}`.
3. Once every `{{> name}}` tag — *T*'s own, and every declared prompt it
   pulled in, recursively — has been replaced this way, render the
   rewritten template (now containing zero `{{> ...}}` tags, so
   `render_template`'s own partial check passes trivially) against *ctx*
   plus every synthetic key step 2b added, with whatever `escape` the
   surrounding field would have used anyway (§3.4's table). A declared
   prompt's own tags, rewritten to explicit no-escape in step 2a, never
   escape regardless of that surrounding `escape`; an effect reference's
   synthetic key is likewise never escaped, since §3.4's triple-stache
   splice already never is.

**`cof check`'s static checks** (`check_prompt_composition()`, §1.4) scan
only the exact fields §3.4 marks `{{> name}}`: yes — not `if.template`/
`while.template`/`expect.template`/asset `ref`, and (field-scoped per effect
type, not blanket) a `prompt`/`yield`'s `inputs:` is **not** scanned (those
values are merged into context as-is, never rendered, so prose that happens
to contain `{{> ...}}`-looking text there is not a composition tag) while a
`use` effect's `inputs:` **is** (those values *are* rendered, §3.4).

#### 3.6.1 `type: yield` — see §5.9.

#### 3.6.2 Prompt files: `{file: <path>}`

Can replace the text of a declared prompt, a `prompt`/`yield` effect's
`template`, or a message's `content`. Resolved and read at **compile** time
(§1.4), never mid-run — the resulting string is used exactly as inline text
would be from that point on (including by §3.6's expansion, which never
knows or cares whether a declared prompt's text came from a literal string
or a file).

- `<path>` must be a literal string (no `{{`/`}}` substring) and relative
  (not absolute). Resolved against the **directory of the document that
  names it** — not the project root.
- After resolving symlinks, the resolved path must equal, or be a
  descendant of, a **confinement root**: for an ordinary filesystem
  document (run by path, or a `use: path:`/legacy `orchestration:` child),
  the directory of the nearest `circuitry.config.json`/`config.json` at or
  above the document's own directory, or the document's own directory when
  none is found. For a `use: ref:`/`cof run-library` document served from a
  refreshable (`github`-type) library source, the confinement root is that
  source's cached tree at the pinned commit instead (never a
  `circuitry.config.json` search) — any other library source (`folder`,
  `curation`) falls back to the same nearest-config-file rule as an
  ordinary filesystem document.
- Violations are compile-time errors naming the field: an absolute path, a
  path (after symlink resolution) outside the confinement root, a missing
  file, an unreadable file, a file that is not valid UTF-8, a file over
  1 MiB (`core/prompt_files.py:MAX_PROMPT_FILE_BYTES`).
- A document with no file of its own — generated at run time (a
  reflector/decompose plan, a `use: inline` child), or with no path at all
  (stdin, an SDK string) — cannot use `file:` anywhere: compile error,
  independent of the checks above (there is no document directory to
  resolve against).
- Anything that identifies a document by its content (consent-by-digest,
  §9's `--resume` document-hash check) includes every prompt file's content
  it loaded, not just the orchestration YAML's own bytes.

---

## 4. CEL

### 4.1 Environment and bindings

`cel-python` (`celpy`), one shared `celpy.Environment()`
(`core/cel_eval.py:_ENV`), compiled expressions cached process-wide by source
string (`_compile`, `core/cel_eval.py`). Two call shapes:

- **`evaluate_cel(expr, ctx, strict=False)`** (`core/cel_eval.py:98`) — used
  by `if`/`while` CEL mode. Binds exactly one root name, `state`, to `ctx`
  (the run state) converted via `_to_cel`. Returns a Python `bool`.
- **`evaluate_cel_expect(expr, value, meta, state)`** (`core/cel_eval.py:163`)
  — used by tool/use `expect:`. Binds **three** roots: `value` and `meta`
  (this effect's own outcome, unprefixed — `has(value.prompt_id)`, not
  `state.value...`) plus `state` (the full run state, same as above).

### 4.2 Python → CEL type mapping

`_to_cel()` (`core/cel_eval.py:533`), confirmed:

| Python | CEL |
|---|---|
| `None` | `null` |
| `bool` | `celtypes.BoolType` |
| `int` | `celtypes.IntType` |
| `float` | `celtypes.DoubleType` |
| `str` | `celtypes.StringType` |
| `bytes` | `celtypes.BytesType` |
| `dict`/any `Mapping` | `celtypes.MapType` (keys/values recursively converted) |
| `list`/`tuple`/`set`/`frozenset` | `celtypes.ListType` |
| `datetime.datetime` | `celtypes.TimestampType` |
| `datetime.timedelta` | `celtypes.DurationType` |
| anything else (no CEL counterpart) | `null` (never the raw Python object — this is the sandbox boundary; no expression can reach an attribute/method/class through state) |

### 4.3 Functions and operators

Standard CEL library as cel-python implements it: `has()`, `size()`,
`matches()`, string functions, the comprehension macros (`all`, `exists`,
`exists_one`, `map`, `filter`), ternary `?:`, logical `!`/`&&`/`||`. Two
custom overrides (`core/cel_eval.py` `_FUNCTIONS = {"_==_": _spec_eq, "_!=_":
_spec_ne}`) implement CEL's own **heterogeneous-equality** spec (cel-spec
#103) exactly, which cel-python's base functions do not: cross-type `==`
between non-numeric types is `False` rather than an error (confirmed:
`1 == true` → `False`, `"a" == 1` → `False`), while `int`/`uint`/`double` are
one numeric family and *do* compare across subtype (confirmed: `1.0 == 1` →
`True`). This is the opposite of Python's own `1 == True` (`True`) — an
engine that reused Python's native `==` for CEL equality would diverge here.

### 4.4 Absent-path convention

**Reading an unset path is not an expression error — it makes the whole
expression `False`**, decided *structurally* (by walking the parse tree for
every dotted `state.` read — `_collect_state_paths`, `core/cel_eval.py`, —
then checking each against `ctx` *before* evaluating), not by catching an
exception during evaluation. A `logger.warning` fires naming the unresolved
path (confirmed: `"CEL expression %r reads unset state path %r; condition is
false"`). Two escape hatches, both confirmed live:

- Any path appearing as an argument to `has()` **anywhere** in the
  expression is exempt (`has(state.input.missing)` → `False`, no warning,
  no effect on the rest of the expression).
  `has(state.x) && state.x > 1` evaluates properly instead of collapsing.
- `strict: true` on the `if`/`while`/loop definition turns an unresolved path
  back into a raised `CelEvaluationError` instead of a silent `False`
  (confirmed: `strict=True` on `state.input.missing == null` raises
  `"... reads unset state path 'state.input.missing' and is marked strict."`).

A genuinely malformed/unparseable expression, an unknown function, or a real
type error always raises `CelEvaluationError` — never silently `False`
(deliberate: a typo in a condition must not quietly route every run down
`else`).

### 4.5 Errors

`CelError` base; `CelValidationError` (compile-time, `cof check`/compiler)
and `CelEvaluationError` (runtime) both carry the offending `.expression`.
Max expression length 4096 chars (`core/cel_eval.py:_MAX_EXPR_LENGTH`); an
empty/whitespace-only expression also raises immediately.

---

## 5. Effects

Every leaf/container runtime shares one dispatch contract: `execute(store, ctx)`
first calls `store.ensure_dict(name)` (or, for an unnamed loop/conditional,
writes straight into the enclosing scope), sets `node["value"]` and populates
`meta` with everything knowable *before* dispatch (so a complexity score or a
rendered prompt is visible even on a failed effect), fires
`store.fire_effect_start`, executes, fires `store.fire_effect_complete` on
**every** exit path (success, absorbed failure, or re-raise) so the
start/complete pair is always balanced, then applies `on_error`.

### 5.1 `prompt`

**`prompt_type` decoding** (`core/prompt.py:1409` `_decode_output`): `text` →
the stripped text, no further processing. `boolean`/`number` → the shared
lenient parser (`core/answers.py`, §6 below) — these alone bypass the
"empty text → `None`" short-circuit, so an empty reply to a boolean prompt
still attempts to parse (and fails loudly, `AnswerParseError`) rather than
silently returning `None`. `json`/`object`/`array` →
`_parse_json` (`core/prompt.py`): try a direct `json.loads`; then
```` ```json ... ``` ```` / ```` ``` ... ``` ```` fenced blocks; then the
widest `{...}`/`[...]` substring; `None` if nothing parses — then,
**if `schema:` is set**, validated with `jsonschema.validate` (silently
skipped if `jsonschema` isn't installed); a `None` decode or a schema
failure both raise `SchemaValidationError` (carries `raw_response_text`).
`tool` → returned as-is (no decoding defined yet in this runtime).

**Schema validation** only applies when `prompt_type` is `json`/`object`/
`array` **and** `schema:` is set; the compiler requires `schema:` whenever
`prompt_type` is one of those three (§1.4).

**Retries and their classification** (`core/prompt.py:453` `execute`):
`retries: {max_attempts, backoff_ms}` per-effect, else
`runtime.default_prompt_retries` (config), else `1` (no retry). A failed
*pass* (one full `provider`+`provider_fallbacks` chain attempt) retries only
if the **last** failure in that chain's attempt log classifies as retryable
— `classify_exception()` (adapters/_retry.py): HTTP 429/408/5xx, a timeout,
a dropped connection → retryable; 400/401/403/404/422, a missing API key →
not. **Exception**: a decode/schema failure (`AnswerParseError`/
`SchemaValidationError`) is *always* classified retryable at the pass level
(`_retry_info_for`, `core/prompt.py:99`) regardless of HTTP status — "maybe a
re-ask gets a parseable reply" — but is treated inside the **fallback chain**
itself as "this provider's reply can't be used, try the next provider in the
chain" rather than aborting the whole pass (`#281`; see next paragraph).
Backoff is exponential with full jitter from `backoff_ms`
(`next_backoff_delay_ms`), capped at 60s, **overridden outright** by a
provider's own `Retry-After` header when the failing adapter can supply one
(litellm-backed adapters only today).

**`provider_fallbacks`**: `_build_attempts()` (`core/prompt.py:1044`) builds
an ordered `(adapter, model)` list: this effect's own `provider:` (if set)
first, then the run-default adapter+model, then every `provider_fallbacks`
entry, each parsed as `"adapter"` or `"adapter:model"` (bare model falls
back to the resolved default model), de-duplicated by `(adapter, model)`
pair. `_generate_with_fallbacks()` (`core/prompt.py:1208`) tries each in
order within **one retry pass**: the first reply that is *both* a successful
adapter call **and** decodes/validates wins; a reply a provider actually
returned but can't be used (unreadable boolean/number, failing schema) is
logged into `attempts_meta` with `status: "decode_failed"`/`"schema_invalid"`
and the **next fallback is tried immediately**, without consuming a retry —
only once every fallback in the chain is exhausted does the pass as a whole
fail and (if retryable) the outer retry loop sleeps and starts a **fresh**
`(adapter, model)` chain from the top again. An `AllowlistError` from
resolving a fallback's adapter always propagates immediately, never logged
as "merely failed" — a denied provider must not silently fall through to
whatever's next in the chain.

**Messages/assets**: `messages:` (list of `{role, content}`) is mutually
exclusive with `template:` (compiler requires exactly one). Each `content`
is rendered independently; `_materialize_input()`
(`core/prompt.py`) flattens messages as `"role: content"` joined by `\n\n`
for `meta.prompt_sent`/non-chat adapters, while a chat-capable adapter
receives the structured `messages` tuple via `GenerateOptions`. `assets:`
(images only — any other `kind` is dropped with a `meta.warnings` entry,
never sent) resolve `ref` via Mustache render, then load: `http(s)://` passes
through as a URL, anything else is read as a local file; the media type is
sniffed from magic bytes only (PNG/JPEG/GIF/WEBP), **never from the
filename**; `meta.assets` records `{kind, ref, media_type, size, sha256}` —
never the raw bytes.

**`timeout_ms`**: resolved per-attempt (`_attempt_timeout_seconds`,
`core/prompt.py`) as `min(ceil(timeout_ms/1000), adapter's own configured
timeout for *that* attempt's adapter)` — not the run-default adapter's
timeout, since a `provider:`/fallback can dispatch to a *different* adapter
than the run default.

**Token accounting**: `tokens_sent`/`tokens_received` = the attempt that
actually answered. `tokens_sent_total`/`tokens_received_total` = the sum
across **every** attempt this `execute()` call made — failed, retried, and
fallen-back-from alike, across every retry pass, confirmed live (two attempts
of 10/5 tokens each in a single-pass success still only shows one attempt;
a multi-pass retry sums all passes' attempts). Each `fallback_attempts[]`
entry carries its own `tokens_sent`/`tokens_received` (`None` if the call
never got a reply at all).

**`meta` fields written before vs. after dispatch**: everything *except*
`value`, `tokens_sent`/`tokens_received` (the winning attempt's own, not the
`_total`s — those accumulate as passes run), `completed_at`, and `error` is
written **before** the first dispatch attempt — confirmed live (`adapter`,
`model`, `model_reason`, `prompt_type`, `prompt_sent`, `dry_run` are all set
before any network/adapter call). This is what makes a complexity score (and
the fully-rendered prompt) visible on a prompt that then fails.

**Routing/scoring/decomposition** — see §5.8.

**Confirmed live node** (`prompt_type: text`, no retries/fallbacks/scoring):
```json
{
  "meta": {
    "adapter": "scripted", "model": "test-model", "model_reason": "default",
    "prompt_type": "text", "prompt_sent": "hi world",
    "tokens_sent": 10, "tokens_received": 5,
    "tokens_sent_total": 10, "tokens_received_total": 5,
    "dry_run": false, "error": null, "waiting_for": null,
    "fallback_attempts": [{"adapter":"scripted","model":"test-model","status":"succeeded","error":null,"tokens_sent":10,"tokens_received":5}],
    "fallback_recovered": false,
    "created_at": "...", "completed_at": "..."
  },
  "value": "Hello!"
}
```

### 5.2 `tool`

**Param rendering** (`core/tool.py:163` `_render_params`): every leaf of
`params` is either a `{from: path}` reference (§3.5) or Mustache-rendered;
`params_json` (if set) is rendered as one whole-string template against a
*JSON-aware* context wrapper (`_json_aware_ctx`, §3.2), parsed as JSON, then
**deep-merged onto** the rendered `params` (`_deep_merge_params` —
`params_json` keys win, nested dicts merge recursively rather than replacing
wholesale). A top-level `prompt:`/`model:` field on the tool effect (if set)
is rendered/passed separately and merged in *before* `params`, so `params`
always wins a key collision with the top-level fields.

**`allowed_commands` rules**: a security-sensitive param
(`_SECURITY_SENSITIVE_PARAM_KEYS = {"allowed_commands"}`) must be a
**literal list of strings** in the document's own unrendered `params:` block
— enforced at both compile time (§1.4) and again at dispatch
(`_reject_templated_security_params`, `core/tool.py:274`: rejects a `{{`
substring in any list item, a `{from:...}` reference as the whole value or
any item, or a non-string item) — and **never** settable via `params_json`
(`_reject_params_json_security_overrides`, `core/tool.py:317`, raises if the
rendered `params_json` overlay tries to set this key at all). This is a hard
security boundary: nothing computed at runtime, from state, from a model
reply, or from a caller input can widen what a `shell` effect is allowed to
run.

**Result contract**: the plugin returns a `ToolResult` (`ok, value, stdout,
stderr, exit_code, raw`). `meta.exit_code` is only meaningful for
process-backed plugins (`None` otherwise). A tool **fails** — `meta.error`
set, `on_error` applies — whenever the plugin raises, **or** returns
`ok=False`. HTTP-family plugins (`http, web_fetch, webhook, linear`,
`_HTTP_FAMILY_PROVIDERS`, `core/tool.py:41`) default to `ok=False` on a
4xx/5xx response and set `meta.status_code` to the real HTTP status; each has
its own per-effect opt-out param (plugin-level, out of scope here).
"Soft-failure" plugins (e.g. `wikipedia`, `dns`, `port_check`,
`validate_yaml`) report their own outcome via `value`/`raw` fields while
`ok` stays `True` — these never trip `on_error` at the tool-runtime level at
all. `meta.raw` is always set for a tool effect that returned a result
(never for a prompt) — redacted, then capped at 64 KiB
(`_RAW_META_MAX_BYTES`, `core/tool.py:47`) with a `{"_truncated": true,
"_original_bytes", "_preview"}` marker replacing an over-cap `raw`.

**Retries**: identical shape/defaults to prompt (`retries: {max_attempts,
backoff_ms}`, default **1 attempt, no retry** — unlike prompt there is no
`runtime.default_*_retries` fallback for tools). Retryability
(`_is_retryable_failure`, `core/tool.py`): HTTP-family tools classify by
status exactly like an adapter dispatch (429/408/5xx retryable); any other
tool (`shell`, `ffmpeg`, `comfyui`, ...) retries on **any** failure — there
is no status to classify, and a process failing once (a transient GPU
watchdog kill) is exactly the motivating case.

**`expect:`**: evaluated **only after** `ok=True` (a plugin-level failure
never reaches it). Mode `cel` binds `value`/`meta`/`state` (§4.1); mode
`model` renders `expect.template` against `{**ctx, value, meta}` and asks a
yes/no the same way a model-mode `if` does, on the run's own adapter/model.
A **false or unreadable** expectation is **always retried** (unlike an HTTP
status it carries no classification of its own) — it fails the current
attempt with `"expect failed: <expr-or-template-summary>"`, and `retries`/
`on_error` apply exactly like any other attempt failure. Outcome recorded at
`meta.expect`.

**`meta.raw` cap / redaction of stored params**: `meta.params_rendered` is
always `redact()`-ed before storage (`cli/redaction.py`, a deny-list of
credential-shaped key names and value shapes — API-key-looking strings, JWTs,
URL userinfo); the **plugin itself still receives the unredacted values** —
redaction only protects what lands in state/`--out`/`--live-state`.

**Confirmed live node** (`shell` provider, `echo hello {{input.x}}`):
```json
{
  "meta": {
    "provider": "shell", "params_rendered": {"command": "echo", "args": ["hello", "world"]},
    "stdout": "hello world\n", "stderr": "", "exit_code": 0,
    "binary": "/bin/echo",
    "raw": {"args": ["hello", "world"], "binary": "/bin/echo", "cwd": null},
    "error": null, "waiting_for": null, "created_at": "...", "completed_at": "..."
  },
  "value": "hello world\n"
}
```

### 5.3 `use`

**path/inline/ref resolution** (`core/use.py:346` `_resolve_orchestration`,
`core/use.py:491` `_load_child_orch`): exactly one of `ref` (library lookup
across configured sources, records a pin for reproducibility),
`path` (filesystem — tried absolute, then cwd-relative, then relative to the
*parent orchestration's own directory*, which is rewritten per-child as
composition descends — so a nested `use: {path:}` inside a child resolves
against *that child's* directory, not the root document's), `orchestration`
(deprecated, tries filesystem then library), or `inline` (a Mustache template
that must render to YAML text). A `ref`/`path`/`orchestration`-loaded child
gets the **same** `structural_errors()` check (§1.3) the root document got
from `cof check` — a `use` child is never exempt. An `inline` child gets the
same check **only if** `validate: true` (the default) — set `validate: false`
to skip it (confirmed by `UseDefinition.validate: bool = True`).

**Inputs**: rendered via `_render_inputs()` (`core/use.py:179`) — a
`{from: path}` leaf resolves + deep-copies (§3.5); any other string is
Mustache-rendered **against the parent's ctx**; anything else passes through
unchanged — **then** checked/defaulted/coerced against the child's own
`interface.inputs` (§1.5) **before** the child ever runs. **Critical:** for
an `inline` child, the whole `inline:` *template string itself* is rendered
against the **parent's** ctx *before* it is even parsed as YAML — so a
`{{input.x}}` written inside the inline YAML text resolves to the *parent's*
`input.x`, never the child's own rendered `inputs.x`, no matter what the
`inputs:` mapping sets (§9 Quirk Q3).

**Child state placement**: the child runs in a **fresh, isolated** state
dict `{"input": <rendered+checked inputs>}` — no `prime`/`runtime` carried
over at all — compiled and executed exactly like a top-level run
(`compile_orchestration(orch=child_orch, root_name="prime")` +
`DynamicRuntime(...).execute(store=child_store)`). Results map back to the
parent's `use` node by one of three modes, in this precedence: (1) **explicit
`outputs:`** on the `use` effect — `node["value"] = {name: resolved-child-path
for each}` (§3.5's "canonical output" shape, `core/outputs.py:29`
`normalize_outputs`, accepts both `{path: ...}` and bare-string shorthand);
(2) **auto-generated from the child's own `interface.outputs`**, only when
the `use` effect sets no explicit `outputs:` at all; (3) **full-namespace
mode** (neither present) — every key of the child's `prime` node (except
`value`/`meta`, i.e. the child document's own effects) is copied **directly
onto** the `use` node itself, so a child effect named `inner` surfaces at
`prime.<use_name>.inner.value`, exactly the nesting convention a `dynamic`
container gives its own children. `runtime.state.record_children` (config)
additionally grafts the child's full effect record under the `use` node even
in explicit/auto-outputs mode, for observability.

**Child errors**: `meta.child_errors` is a flat list of `{path, error}` for
**every** node anywhere in the child's tree whose own `meta.error` is set —
walked recursively, so a deeply-nested effect whose own `on_error: skip`
already absorbed its exception (invisible by just reading `value`) still
surfaces here. A **top-level** child failure (one that actually propagates
out of `DynamicRuntime.execute`) is wrapped as `RuntimeError(f"use '{name}'
-> {label}: {e}")` and handled by the `use` effect's own `on_error`.

**Validation**: `structural_errors()` (file/path/ref children, or inline with
`validate: true`) plus allowlist enforcement (`_check_allowlists` — a child
referencing an adapter/tool the run disallows is refused before it runs) plus
capability-consent re-checking for a `ref:` child (independent of whatever
trust the parent document already has) plus cycle detection (resolved-path
identity tracked through `runtime_config["_use_call_stack"]`, built fresh
per call-path rather than mutated shared state, so concurrent tree-flow
branches never see siblings as false-positive ancestors).

**Confirmed live node** (full-namespace mode):
```json
{
  "value": null,
  "meta": {"child_errors": null, "inline": true, "inputs": {},
           "orchestration_sha256": "9ee5...81fd", "resolved_path": null,
           "validation_errors": null, "error": null, "created_at": "...", "completed_at": "..."},
  "inner": {"value": "hi\n", "meta": {"provider": "shell", "stdout": "hi\n", ...}}
}
```
Note `node["value"]` is `null` in full-namespace mode — the mapped value only
exists in explicit/auto-outputs mode.

### 5.4 `loop`

**each vs. while** — compiler requires exactly one of `each`/`while`
(§1.4). **each**: `each.in` (root-relative or loop-binding path to a JSON
array; resolved once at loop start, `_resolve_collection`,
`core/loop.py:1116`), `each.as` (binding name, default `item`),
`each.truncate` (opt-in to processing only the first `max_iterations` and
recording the rest as `unvisited` — **without** it, a collection longer than
`max_iterations` is a **start-time failure** before any pass runs,
`LoopBoundsError`, `core/loop.py:128`, gated by `on_error` exactly like a
pass failure: `fail` raises, `break`/`continue` record
`termination.reason: "error"` with `termination.detail` and run 0
iterations). **while**: `while.mode` (`model`|`cel`) + `template`/`expr`;
evaluated **between** passes (so pass N's body has already run before the
condition that stops pass N+1 is checked) — see §2.4 for the `iter.index`
off-by-one this implies.

**chain vs. tree**: `flow: chain` (default) — sequential, each pass sees
`prime.<loop>.prev` (chain only) and the live, already-populated
`iter_<N-1>` sibling. `flow: tree` — **each only** (while always runs
sequentially regardless of `flow:`) — every pass dispatches concurrently on
a `ThreadPoolExecutor`, each with its own **isolated** `Store`
(`Store.parallel_branches`), merged back into the loop's node in **original
index order** once every future completes; bounded by `max_concurrency`
(unset = every pass at once). A `prime.<loop>.prev` reference inside a tree
`each` body is a **compile-time** error (§1.4), not a runtime quirk — there
is no well-defined "previous" pass when passes run in parallel.

**`collect:`**: names a body effect; see §2.3 for the exact aggregation rule
(completed passes only, by live-tracked index, elides a disabled/skipped
target).

**`min_iterations`/`max_iterations`**: `min_iterations` (while only — lint
warns it's a no-op on `each`, §1.3) forces that many passes **without even
evaluating** the condition (not evaluated-and-discarded — never evaluated at
all, so a condition that reads not-yet-written state can't spuriously warn,
and a model-mode condition doesn't waste a call). `max_iterations` is an
absolute cap for `while`; for `each` it is a **collection-size** cap (see
above).

**`on_error`**: `fail` (default, propagates — stops the whole run unless an
ancestor container absorbs it), `break` (stop the loop now,
`termination.reason: "error"`, whatever ran stays recorded), `continue`
(skip this pass, keep going — termination reason is whatever the loop would
have reached anyway, `continue` doesn't change it).

**`prev`**: §2.4.

**Progress**: `meta.progress = {done, total, elapsed_s, eta_s}`, recomputed
after every pass (including a failed one — `done` always counts a pass that
finished running, success or failure, so ETA doesn't freeze/overestimate
across `on_error: continue` runs). `total` is `None` for an uncapped `while`
loop; for `each` it is always the **uncapped** collection length (even under
`truncate: true`, where fewer passes will actually run). `eta_s` is `None`
before the first pass completes or when `total` is unknown.

**Confirmed live node** (`each`, 3-item collection, `collect: sq`):
```json
{
  "value": {
    "iterations": 3,
    "termination": {"reason": "collection_exhausted"},
    "effects_by_iteration": [{"count":1,"executed_effects":[{"name":"sq","type":"ToolDefinition"}]}, "..."]
  },
  "collected": {"value": ["a\n", "b\n", "c\n"]},
  "last": {"$ref": "iter_2"}
}
```
(`"$ref"` form is the **saved** representation, §2.3; in memory `last` is the
same dict object as `iter_2`.)

### 5.5 `dynamic`

**chain vs. tree**: `chain` (default) runs `effects:` sequentially, stopping
at the first unabsorbed failure. `tree` runs every child **concurrently**
against a **shared, deterministic snapshot taken at dynamic start**
(`tree_ctx = dict(ctx)`, a *shallow* copy — safe because every write a branch
makes lands in its own isolated `Store`, never back into the shared `ctx`
dict, and any value a branch reads out of `ctx` and hands onward is already
re-copied by template rendering/`params_json`'s JSON round-trip by the time
it leaves the branch's own thread) — **not** each other's sibling writes, in
contrast to chain flow where each sibling's writes are immediately visible
to the next (because, at plain top level, `ctx` *is* `store.state` by
reference — §2.4).

**`max_concurrency`**: tree flow only, bounds the `ThreadPoolExecutor`'s
`max_workers`; unset = every child at once.

**`stop_on_error`**: tree flow only — once one branch fails, cancel every
future that hasn't started yet (best-effort: an already-running branch is
never interrupted, the pool still waits for it on shutdown). Chain flow
already stops at the first failure by construction, so this flag has no
meaning there.

**`on_error`**: `fail` (default) propagates a body/`finally:` failure to this
dynamic's own parent. `skip`/`continue` record the failure on **this
dynamic's own** `meta.error` and let its parent continue to the next sibling
— the same degradation a leaf's `on_error` gives, applied one level up.

**`labels`**: free-form, `meta.labels`, zero runtime effect.

**`finally:`** — §6.4.

### 5.6 `if`/`conditional`

**mode**: `cel` (§4) or `model` (asks "Evaluate ... respond with ONLY 'yes'
or 'no'" via the run's adapter/model, parsed by the same lenient
`parse_boolean_answer` tool/loop conditions use, §6.3). Exactly one branch
(`then`/`else`) runs — `else:` is optional and defaults to an empty list (no
branch at all if the condition is false and there's no `else:`).
`threshold:` is schema-valid but **never consulted** by the built-in
evaluator (lint warns, §1.3) — it is recorded on `meta.threshold` purely for
a future/custom evaluator.

**on_error**: `fail` (default, a broken condition propagates — `continue`'s
documented "silently take else" semantic is an explicit opt-in, never the
default, precisely because a typo'd CEL expression used to silently always
take one branch is the motivating bug class). `skip`: records the failure,
writes `{"result": null, "branch": null, "effects": {}}`, no branch runs.
`continue`: logs a warning and **forces the `else` branch** (not "no
branch") — confirmed in source (`core/conditional.py` `_decide_and_run`:
`result = False` after the warning, then normal branch dispatch continues).

### 5.7 `reflector`

Internally **sugar over `use: {inline: ...}`** (`core/reflector.py` module
docstring) with three additions: a **prime directive** (critical-formatting
warning + the planning goal/context) prepended to the configured
`plan_from_step` prompt's own template; an iteration loop with a `done:` flag
the generated plan may set; and automatic wiring from the planning prompt's
output to the executed plan.

Each `max_iterations` pass: (1) render the prime directive — `goal` =
`prime.goal.value` at the **true run root** (survives arbitrary nesting
depth via `Store.true_root_state`, §2.4), `context` = a size-capped,
redacted JSON summary of `runtime.effective_settings.{model, adapter,
plugins}` only (never the full settings object); (2) run the `inner`
dynamic (the reflector's own `effects:`, with the prime directive
spliced into `plan_from_step`'s template) — a failure here is
`inner_failed`; (3) extract `done`/the YAML plan text from
`plan_from_step`'s own `.value`, strip markdown fences, strip a `done:` key
out of the parsed YAML (not part of the orchestration schema); if the plan
has **zero** top-level effects, stop with no execution; if `done &&
stop_on_done` (default `stop_on_done: true`), stop **without executing** the
plan at all (even though it was parsed/recorded); a plan over `max_effects`
top-level effects is an `invalid_plan` failure, **never silently truncated**;
(4) execute the (non-stopping, within-cap) plan via a fresh
`UseDefinition(inline=plan_yaml, validate=True, on_error="fail")` at
`prime.<reflector>.<generated_key>.iter_<i>`.

**Decomposition/routing/scoring are off by default** for the reflector's own
planning prompt — `core/decompose.py`'s own planner run explicitly disables
decomposition for itself (its planning prompt is deliberately huge and would
otherwise trigger the very feature it's generating). This is the one
documented exception; everywhere else decomposition/scoring/routing follow
the ordinary `runtime.complexity.*` config (§5.8).

### 5.8 Decomposition, routing, scoring

**All three default to `enabled: false`** (`cli/complexity_config.py`
confirmed: every `*Settings` dataclass's `enabled` field defaults `False`).
A run turns them on via `runtime.complexity.scoring.enabled: true` /
`runtime.complexity.routing.enabled: true` /
`runtime.complexity.decomposition.enabled: true` in config/orchestration
`runtime:` (gated: decomposition and routing both *require* scoring to be on
— config resolution rejects the other combination).

**Scoring**: `core/complexity.py` computes a pure, no-IO score per prompt
(weighted signals: prompt size, keyword matches, structural depth, etc.),
recorded at `meta.complexity` **only when enabled** — the key is entirely
absent (not `null`) when off, so toggling the feature leaves every other
byte of state identical.

**Routing**: a band table (`runtime.complexity.routing.bands`, each
`{name, model, threshold}` with a mandatory catch-all) maps a score to a
model. **Precedence** (`core/router.py` module docstring, confirmed):
`--model` (CLI) > per-effect `model:` > profile effect `model` override >
profile effect `routing` pin > **router** > orchestration default > config
default. The router returns `None` ("no opinion, keep what's already
resolved") whenever anything above it in that list already made a
deliberate choice — it never overrules a human. When it does act,
`meta.model_reason` becomes `"router"` and `meta.complexity.band = {name,
model}` records which row. The band is recorded **even when the router
doesn't act** (an explicit model was named) — `band` alone never implies the
router's choice was *applied*; `model_reason` is the field that says that.

**Decomposition** (`core/decompose.py` module docstring): when a prompt's
score strictly exceeds `threshold`, the single over-complex call is replaced
by plan → validate → execute: the bundled planner orchestration
(`curation/agents/decompose.yml`) runs isolated (decomposition switched off
for *this* inner run, to avoid infinite recursion on its own giant prompt),
emits a YAML plan that must pass schema validation, stay within
`[2, max_chunks]` fan-out, and honor a merge contract (a top-level effect
that writes the planner-reported `result_path`); the emitted orchestration
then runs as a `use`-child-shaped, state-isolated run (same cycle guard, same
namespaced observability) and the value at `result_path` is written back as
if it were the original prompt's own `value` — so nothing downstream can
tell the substitution happened. Recursion is bounded by `max_depth`; at the
ceiling the effect "routes up" (dispatches once on the routing table's
catch-all model) unless routing is off, in which case it just runs as-is.
Failure semantics (`on_failure: route_up` default, or `fail`) are recorded
in `meta.decomposition.{decomposed, outcome, reason, score, threshold, depth,
max_depth, plan?, chunk_count?, yaml?, result_path?, fallback_model?,
error?}` — present **only** when the attempt was actually triggered (score
over threshold); absent otherwise, same all-or-nothing-key rule as scoring.

---

## 6. The error model

### 6.1 `on_error` matrix

| Effect | Values | `fail` (always the default) |
|---|---|---|
| prompt | `fail, skip, continue` | propagate; `skip`/`continue` clear `node["value"]` to `None`, keep `meta.error` |
| tool | `fail, skip, continue` | same as prompt |
| use | `fail, skip, continue` | same; a config-shaped failure (cycle, bad reference, bad interface input) is **never retried** regardless of `retries:`, even under `fail` — see §6.2 |
| loop | `fail, break, continue` | `break` stops the loop (`termination.reason: "error"`); `continue` skips the pass |
| dynamic | `fail, skip, continue` | `skip`/`continue` degrade *this dynamic's own* failure one level, same as a leaf |
| conditional/if | `fail, skip, continue` | `skip` writes a null-result node, no branch runs; `continue` **forces the else branch** (not "no branch") |
| reflector | *(no `on_error` — always propagates)* | — |

### 6.2 Exact `meta.error` / `child_errors` text

`meta.error` is always `str(exception)`, never a structured object — so the
**exact exception message text** is part of the observable contract an
electricity reimplementation must match for the conformance suite
(§10) to be meaningful; it is **not** meant to be parsed by downstream
orchestration logic (no CEL/template site reads `meta.error`'s text
structurally), but a literal byte-for-byte test is still the simplest
parity oracle. Representative exact strings, confirmed live:

- Tool dispatch failure wrapped by the enclosing dynamic:
  `"prime.second: shell: args[1] contains forbidden char '\\n'."`
  (format: `"<dotted-path-to-failing-effect>: <original exception message>"` —
  `RuntimeError(f"{effect_path}: {e}")`, `core/dynamic.py` chain-flow loop).
- `expect:` failure: `"expect failed: value == 'nope'"` (CEL) or
  `"expect failed: <first 80 chars of the model template, collapsed to one line>..."`
  (model mode) — `expect_failure_summary()`, `core/expect.py:45`.
- Answer parse failure: `"could not parse a number from 'about 42'"` /
  `"could not parse a yes/no answer from 'maybe'"` — `core/answers.py`.
- `use` child cycle: `"use '<name>': cycle detected — <path> → <path> → ..."`.
- Loop collection bounds: `"each loop '<name>' (<in_path>): collection has N
  items but max_iterations is M — raise max_iterations, bound the
  collection, or set each.truncate: true to process the first M and record
  the rest as unvisited"`.

`meta.child_errors` (use effect only) is `[{"path": "<dotted-path-relative-to-
the-child-root>", "error": "<meta.error text>"}, ...]`, one entry per node
anywhere in the child's tree whose own `meta.error` is non-empty, walked
recursively regardless of whether that child node's own `on_error` already
absorbed the exception (so an entirely "successful" `use` effect — no
exception propagated to the parent — can still carry `child_errors`
entries for swallowed grandchild failures). `null` when there are none.

### 6.3 Lenient answer parsing (shared by boolean/number decode and model-mode conditions)

`core/answers.py` strips surrounding whitespace, a single layer (repeated)
of matching quote chars (`"'`` `) or markdown emphasis (`**`, `__`, `*`, `_`,
`` ` ``), then takes the first whitespace-separated token and strips trailing
punctuation (`.,!?;:`) from *that token only*. `parse_boolean_answer`:
`{true,yes,y,1}` → `True`, `{false,no,n,0}` → `False` (case-insensitive on
the stripped token), anything else raises `AnswerParseError` (confirmed:
`"**Yes**, because x."` → `True`; `"maybe"` raises).
`parse_number_answer`: the stripped-and-punctuation-trimmed leading token
must be the **entire** remaining text (any trailing words after it — `"about
42"`, `"42 degrees"` — raise, since which number was meant is now
ambiguous); tries `int()` then `float()`; rejects non-finite results
(`inf`/`nan`). Confirmed: `"42."` → `42` (int — trailing `.` stripped as
punctuation *before* the int/float attempt); `"about 42"` raises (leading
word, not trailing, so the *token* itself is `"about"`, which isn't a
number — raises before even reaching the remainder check).

### 6.4 `finally:`

Legal only on `dynamic` and the document root (compiler rejects it anywhere
else, §1.4). Shares the **same state node/scope** as the dynamic's own body
(not a separate `.finally` segment) — a `finally:` effect reusing a body
effect's name is a duplicate-name **compile** error, same as two body
siblings colliding. Always runs **sequentially**, regardless of the
dynamic's own `flow:`, and **never** skipped by `--resume` (cleanup must
re-run every time the body finishes, resumed or not). Runs on **every** exit
from the body — success, an absorbed failure, or an unabsorbed one — and
also on cancellation (best-effort: `finally:` runs even for a
`KeyboardInterrupt`/`SigTermInterrupt`, caught via `except BaseException`,
specifically so cleanup still fires, then the cancellation **still
re-raises** regardless of this dynamic's own `on_error`, which only degrades
an *ordinary* failure). If the body failed **and** `finally:` also fails,
the body's own error is what's reported at `meta.error`; the `finally:`
failure rides along as a **second**, non-overriding `meta.finally_error`. If
the body **succeeded** but `finally:` failed, that failure is treated as
*this dynamic's own* failure (subject to this dynamic's own `on_error`, same
as a body failure would be).

### 6.5 Run-level failure and cancellation

A document-root failure (anything that escapes the root `DynamicRuntime`)
is caught by `cli/runtime_shim.py:385` `run()`'s single
`except (Exception, KeyboardInterrupt) as e:` — note the explicit tuple:
`KeyboardInterrupt` is **not** an `Exception` subclass in Python, so it
must be named separately to be caught at this top level at all.
`RunResult.ok = False`, `.error = str(e)` (or the literal text
`"Interrupted (Ctrl-C/SIGINT)"` / `"Interrupted (SIGTERM)"` for a
cancellation, never the raw exception text), `.interrupted` /`.sigterm` flags
set by `isinstance` checks. **Every** failure path — ordinary or
cancellation — still: records `runtime.last_run.completed_at`/`.totals`
(computed from whatever effects *did* complete, via the live
`_TotalsAccumulator`, §8.1), fires `on_run_failure` plugin hooks, writes
`--out` (so a cancelled/crashed run is exactly as resumable as a normal
failure, #270 F6), and attempts a `persistence.save_run_snapshot(ok=False, ...)`.

`SigTermInterrupt` (`cli/interrupts.py`) is a `KeyboardInterrupt` subclass
the CLI installs for the duration of one `run`/`run-library` command only
(main thread only — a no-op off-thread), so the *entire* existing
Ctrl-C-is-resumable code path (the `except (Exception, KeyboardInterrupt)`
above, and the `except BaseException` around `finally:` in `core/dynamic.py`
§6.4) handles SIGTERM identically to SIGINT with zero additional branching —
only the **exit code** differs (§8.4).

---

## 7. Concurrency

### 7.1 Where concurrency is bounded

`loop.max_concurrency` (tree-flow `each` only) and `dynamic.max_concurrency`
(tree flow only) each bound their **own** `ThreadPoolExecutor`'s
`max_workers` — unset means every pass/branch at once. These are purely
*local* caps (how many of *this* container's own children can be pending),
independent of the run-wide limiter below.

### 7.2 `runtime.max_concurrency` / `concurrency_groups`

A **single**, run-wide `RunConcurrencyLimiter`
(`core/concurrency.py:84`) is built **once per run**
(`cli/runtime_shim.py:385` `run()`, from `runtime.max_concurrency` — a global
semaphore cap — and `runtime.concurrency_groups` — named semaphores, each a
positive-int limit) and threaded through every `*Runtime` via the shared
`runtime_config` dict under the private key `_concurrency_limiter`
(`RUNTIME_CONFIG_KEY`, `core/concurrency.py:24`) — including across a `use`
child's own `runtime_config` copy (same dict reference), so the cap/groups
are **run-wide**, not per-document. **Only leaf effects** (`tool`, `prompt`)
ever acquire a slot (`group:` is schema-rejected on anything else, §1.3) —
a container never holds a slot one of its own children is waiting on, which
is the deadlock-freedom argument the source docstring makes explicit.

**Acquisition order is always group-first, then global**
(`RunConcurrencyLimiter.acquire`, `core/concurrency.py:149`) — this fixed
order is what prevents two leaves from ever holding the two resources in
opposite order (the classic two-lock deadlock). `on_wait`/`on_acquired`
fire at most once per resource, and only if the acquire actually blocked
(never for an immediately-free slot) — observable at `meta.waiting_for`
(set to the resource label, then cleared to `None` on acquisition).

### 7.3 Slot held per attempt; released during backoff and before model-mode `expect:`

The limiter slot is acquired **fresh for each individual attempt** — not
once for the whole retry loop — and **released** (via the `with` block)
**before** that attempt's backoff sleep runs (so a retrying effect never
sleeps while still holding a group slot another effect needs) and **before**
a tool's model-mode `expect:` check runs (same reasoning — asking a model
yes/no must not hold a tool's own concurrency slot). This is explicit,
cross-referenced source commentary in both `core/prompt.py` (around the
retry loop's `with concurrency_cm, live_cm:`) and `core/tool.py` (around its
own `with concurrency_cm:`), tagged against issues #333/#273/#281 — an
electricity reimplementation that holds the slot across the whole retry loop
(rather than per-attempt) would under-parallelize and, in a group-starved
run, could behave observably differently (more waiting_for events, different
interleaving in `--live-state`, though not a different *final* state for a
deterministic scripted-reply conformance test).

---

## 8. Run lifecycle and outputs

### 8.1 `runtime.last_run`

Written at `cli/runtime_shim.py:385` `run()`, **before** the root
`DynamicRuntime` ever dispatches (so even a run that fails during
preflight/adapter-resolution still gets a fresh `run_id`/timestamp rather
than one carried over from a `--state`/persistence seed):

```json
"last_run": {
  "run_id": "<uuid4>",
  "orchestration_path": "<str path>",
  "document_hash": "<document_content_digest, or null if unreadable>",
  "dry_run": false, "validate_only": false, "verbose": false,
  "started_at": "<iso8601>", "completed_at": null
}
```
`document_hash` is `document_content_digest`: SHA-256 over the document
file's own bytes; then, for each prompt file it references (`{file: ...}`),
in ascending order of resolved absolute paths compared component by
component (Python's `Path` ordering) — its label (UTF-8, `/`-separated,
relative to the document's directory, which may start with `../`), its
byte length as 8-byte BIG-ENDIAN, then its bytes. The label is
location-independent: it never depends on where the project sits on disk,
a `../` reference included. Not just every referenced file's bytes
concatenated, so moving bytes across a prompt-file boundary still changes
the digest. An error while resolving or reading a prompt file silently
stops adding files, so the digest covers only what was hashed up to that
point. `--resume` also accepts a document hash written by the pre-#407
algorithm (document bytes plus every referenced prompt file's bytes, with
no path label or length) against a saved state that recorded one, kept as
a private, temporary compatibility fallback.

`completed_at` and `totals` are filled in on **every** exit path (success or
failure, §6.5): `totals = {"wall_time_s", "effects_run", "tokens_sent",
"tokens_received", "cost_usd"}`, computed **live** by `_TotalsAccumulator`
(`cli/runtime_shim.py:308`) from the same `effect_complete` event stream
`--live-state` consumes — **not** by walking the finished state tree
after the fact (`_run_totals`, `cli/runtime_shim.py:273`, exists as a
standalone utility for re-deriving totals from a saved snapshot, but `run()`
itself never calls it) — because a `use` child in declared-outputs mode
never leaves its effects in the final tree at all, and an unnamed loop
overwrites the same node every pass; only the live event stream sees every
leaf that actually ran. `document_hash` is what `--resume`'s "did the
document change" safety check compares against (§8.5).

### 8.2 The warnings channel

A `list[str]` returned alongside `RunResult`, accumulated from (in order):
config-resolution warnings (a skipped/untrusted project config), effective-
settings warnings (orchestration `runtime:`/`plugins:` keys dropped for an
untrusted document, §1 — out of electricity's current scope under its
explicit-config-only design, but the *document*-level warnings like lint
still apply), preflight soft-failures, image-asset warnings. **Never**
blocks a run; printed to **stderr** (`cli/app.py` `_print_run_warnings`) —
deliberately, so `--json`/`--tail`/a piped stdout stay machine-readable and
`--quiet` does not silence a warning that might matter.

electricity does not run preflight at all yet (lane R of #448, M1's run
wiring v2, ports it) — rather than silently skip a check `cof run` would
have failed a document on, it refuses outright, with the preview marker,
any document naming a top-level `adapter:` (electricity/DESIGN.md §13,
`electricity-bytecode::refusal::RefusalReason::DocumentAdapter`).

### 8.3 `--out` serialization

`core/saved_state.py:118` `dumps_saved_state(state, pretty=False)`:
`compact_last_aliases(state)` (§2.3) then `json.dumps`. **Not pretty**
(`--out` without `--pretty`... actually `_write_state_json` in
`cli/app.py:212` always calls `dumps_saved_state(state, pretty=pretty)`,
`pretty` being the CLI's own `--pretty` flag): compact form is
`json.dumps(saved)` — **default key order** (Python dict insertion order,
*not* sorted), **no indentation**, `ensure_ascii=True` (the `json.dumps`
default — non-ASCII characters are escaped as `\uXXXX`, confirmed by
reading the call site: neither `dumps_saved_state` call passes
`ensure_ascii=False`). Pretty form (`--pretty`) is
`json.dumps(saved, indent=2, sort_keys=True)` — **sorted** keys, 2-space
indent, same `ensure_ascii=True` default. **This is an asymmetry an
electricity reimplementation must reproduce exactly**: plain `--out` is
insertion-ordered, `--out --pretty` is alphabetically sorted — these are two
genuinely different serializations of the same state, not just
whitespace-different. A trailing `"\n"` is always appended
(`cli/app.py:213` `_write_state_json`).

### 8.4 Exit codes

| Condition | Exit code |
|---|---|
| Run succeeded | `0` |
| Run failed (ordinary error) | `1` |
| Run interrupted by Ctrl-C/SIGINT | `130` (128+SIGINT) |
| Run interrupted by SIGTERM | `143` (128+SIGTERM) |
| Run interrupted by SIGHUP | `129` (128+SIGHUP) |

(`cli/app.py:1423`: `raise typer.Exit(code=143 if result.sigterm else 129 if
result.sighup else 130 if result.interrupted else 1)`, only reached when
`not result.ok` — the success
path falls through to Typer's default `0`.) All three interrupted cases still
write `--out`/the `--last` stash exactly like an ordinary failure — the exit
code alone distinguishes them; state/resumability is identical. SIGHUP never
counts as the *second* signal during cleanup (§6.5) — only SIGINT/SIGTERM do
— and if it is ignored at process start (`nohup`), it stays ignored for the
whole run (electricity/DESIGN.md §6.9; issue #431's Signals section).

electricity's own non-TTY stdout matches this reference's byte for byte, with one
deliberate narrowing: a run failure's error goes out only in the stdout JSON
payload, never duplicated onto stderr. A config error, caught before a run
ever starts, is the one case that prints on stderr alone (`Error: <text>`)
and nothing on stdout. electricity also reproduces this reference's own
Python `logging` warnings (its CLI's default `WARNING` level, never
changed by a `--verbose`/`--quiet` of its own) as the same `WARNING: ...`
lines, through a `log::Log` `electricity-cli`'s own `main` installs (issue
#442) -- the `core/dynamic.py`/`core/conditional.py` on_error/`finally:`
degradation warnings, `core/tool.py`'s invalid-timeout warning,
`cli/config.py`'s "Unknown environment" warning, and `cli/live_state.py`/
`cli/events.py`'s mid-run write-failure warnings, all reachable from
M0-H's own supported subset, in order for a chain and sorted for a tree --
both `tests/warnings.rs` and the conformance suite now compare these
lines against a real `cof run`. See electricity/DESIGN.md §6.9, "CLI
output", for the full statement and its own narrower, still-open gaps.

### 8.5 `--resume` rules

Three state sources (`cli/app.py:490` `_resolve_resume_state`):
`--state <file>` (explicit, bypasses every stash lookup); `--resume last`
(the most recent run's own `--out` file, found via the `--last` stash — the
stashed `-e` args are **not** replayed on top, since the loaded state already
carries its own resolved `input.*`); `--resume <run-id>` (looked up via the
orchestration's own configured `runtime.persistence` backend — raises if
none is configured). **Two safety checks**, both raising `typer.BadParameter`
unless `--force`: (1) the loaded state's `runtime.last_run.document_hash`
must equal the **current** document's hash — a state with no such field at
all (pre-dates the field, or wasn't written by `cof run`) always raises
without `--force`; (2) for a **bare run-id** resume only (neither `--state`
nor `last`, since those two already carry their own resolved inputs), **every**
key the loaded state's `input` namespace has must be re-passed via `-e` —
a single unrelated `-e` is not enough (closes a prior gap where any `-e` at
all satisfied the check).

Engine-level skip behavior (`core/resume.py`, `effect_completed_ok()`,
§6.1/§2.2): an effect is resumable (skipped, node reused as-is) exactly when
`meta.completed_at` is truthy **and** `meta.error` is falsy — "never
started", "mid-flight when the process died", and "finished with an error"
all rerun. This is **positional** within a chain-flow `dynamic`'s own
`effects:` list (`core/dynamic.py` `execute`, `resume_active` flag) — the
moment one effect in the chain is *not* resumed whole, every later sibling
in that same chain reruns too, regardless of what its own node might show
(a later node's apparent completeness from an old run is either stale or sits
after an `on_error`-absorbed failure the chain already moved past). A named
**loop** resumes at the first *gap* in its own `meta.completed_passes`
contiguous-from-0 prefix (not by eyeballing whether `iter_<N>` merely *looks*
finished — a pass that died partway through writing its own node must not
be mistaken for complete) — chain flow only (every `while`, or an `each` not
running `flow: tree`); a tree-flow `each` loop and an unnamed loop always
rerun whole. A `finally:` effect is **never** skipped by resume (§6.4).

### 8.6 Config keys that affect a run

Under electricity's explicit-config-only design ("no discovery, no `cof
trust`"), electricity's config surface is narrower than `cof`'s full
discovery/trust system; the keys that matter for *parity of execution*
(not discovery mechanics) are: `default_model`, `default_adapter`,
`enabled_adapters`/`enabled_plugins`/`enabled_tools` (allowlists),
`environment`, and the `runtime:` block (`adapters.<name>.*`,
`plugins.<name>.*`, `max_concurrency`, `concurrency_groups`, `complexity.*`,
`tools.timeout_seconds`, `state.record_children`, `persistence.*`). An
orchestration's **own** `runtime:` block may only ever set
`ORCHESTRATION_RUNTIME_KEYS = {"complexity", "state"}`
(`cli/effective_settings.py:62`) unless the whole document is **trusted**
(run by path, not a bare library name) — every other `runtime.<key>` in the
document is dropped with a warning (§8.2) in the untrusted case. This
document-trust distinction is explicitly a `cof`-only authoring-time concern;
electricity's "operator-chosen = trusted" model means this
split likely collapses, but the **effective merge semantics** (document
`runtime:` keys layered over config, key-by-key, not whole-block replacement)
is still the behavior to match when a document *is* trusted.

### 8.7 Event stream (`--events`)

Required for VM milestone **M0-H** (`electricity-vm`'s `fire_effect_start`/
`fire_effect_complete` are exactly the two call sites this stream reads).
electricity's own `--live-state` also lands in M0-H (issue #431), alongside
`--events` — not M3-B as an earlier draft of this section said (#419, #418
Q2): a live-state mirror needs only the store's own write hook, nothing
`loop`-specific, so both ship together with the rest of M0-H's run wiring.
Only loop progress (`meta.progress`, DESIGN.md §10.5's second bullet) still
waits for `loop` (M1-E).

`cof run --events <file>` (also `run-library`) writes a JSONL stream of
effect starts and ends: one complete JSON object per line, UTF-8, created
(or truncated) at `cli/events.py` `EventLog.__init__` before the first
effect. Every event is one `write()` of one whole line, flushed at once,
under one lock (`EventLog`'s own `threading.Lock`) — a reader tailing the
file never sees a torn line except a trailing one still being written.

Each line's own JSON text is exactly `json.dumps(payload, separators=(",",
":"))` (`EventLog._write_line`): no space after either `,` or `:`, the keys
in the insertion order the table below lists them in (never sorted),
`ensure_ascii` left at its default `True` (so a non-ASCII `path` or `error`
writes as a `\uXXXX` escape, not a literal UTF-8 byte sequence) — distinct
from both `--out` serializations (§8.3), which space their separators the
way a bare `json.dumps(payload)` call does.

```json
{"v":1,"seq":0,"ts":"2026-10-08T19:56:22.433Z","ev":"run_start","run_id":"…","orchestration":"do-thing.yml","engine":"cof 0.2.0","pid":4242}
{"v":1,"seq":7,"ts":"…","ev":"dispatch","path":"prime.each_tree","branches":3,"concurrency":2}
{"v":1,"seq":8,"ts":"…","ev":"start","id":8,"path":"prime.each_tree.iter_0.t_nap"}
{"v":1,"seq":12,"ts":"…","ev":"end","id":8,"path":"prime.each_tree.iter_0.t_nap","ok":true,"ms":1008}
{"v":1,"seq":20,"ts":"…","ev":"end","id":15,"path":"prime.always_fails","ok":false,"ms":5,"error":"/bin/ls failed (exit 1): ls: …"}
{"v":1,"seq":99,"ts":"…","ev":"run_end","ok":false,"error":"Interrupted (Ctrl-C/SIGINT)","signal":"SIGINT"}
```

| Field | Meaning |
|---|---|
| `v` | Format version, `1`. |
| `seq` | Strictly increasing in file order, starting at `0`. |
| `ts` | Wall-clock UTC, millisecond precision, `isoformat(timespec="milliseconds")` with the `+00:00` suffix replaced by `Z`. |
| `ev` | `run_start`, `dispatch`, `start`, `end` or `run_end`. |
| `id` | Present on `start`/`end` only. Unique per effect *instance* — a loop pass or a tree branch each get their own — from one counter, incremented under the same lock as every write. An unnamed loop's repeated pass, or several `flow: tree` branches sharing one path, still pair `start` with the right `end`: `EventLog` keeps a per-thread stack keyed by path (`threading.local`), since one effect instance's `start` and `end` always arrive on the same thread (including nested instances) — `on_start` pushes, `on_complete` pops. **An `end` whose `start` was not seen carries `id: null`** (and no `ms`) rather than erroring — popping from an empty per-path stack is simply treated as a double-fire or an unseen start, not a bug to raise on. None of the reference's own `fire_effect_complete` call sites double-fire for one instance today, so this cannot actually happen from `cof` itself; it is a defensive invariant for any reader, and for any other engine (or any future composed observer) that might call `on_complete` without a matching `on_start`. |
| `path` | The absolute dotted state path, exactly as in `--live-state` and in scripted-replies keys (`electricity/docs/spec/scripted-replies.md`). |
| `dispatch` | `path`/`branches`/`concurrency`. Sent once by a tree loop or tree `dynamic` before its branches start — the reference composes this with the pre-existing `Store.concurrent_dispatch` callback (`RunRequest.concurrent_dispatch_observer`) rather than replacing it; both observers fire. `branches` is the true branch count — the loop's item total, or the dynamic's `len(effects)`. `concurrency` is the ceiling the pool actually enforces: every branch when `max_concurrency` is unset (so `branches == concurrency`), else `min(max_concurrency, branches)` — a 3-item `each` loop under `max_concurrency: 2` sends `"branches":3,"concurrency":2`. MCP's `RunManager` (the one other listener on the underlying `Store.concurrent_dispatch` callback) uses only `concurrency`, unchanged by this split. |
| `ok` | On `end`: `node["meta"]["error"] is None`. On `run_end`: whether the run succeeded. |
| `error` | Present only when `ok` is `false`: the first 500 characters of the effect's (`end`) or the run's (`run_end`) error text — the exact same string already in `meta.error`/`RunResult.error`, never a new message. |
| `signal` | `run_end` only, present only after an interruption: `"SIGINT"`, `"SIGTERM"` or `"SIGHUP"` — the reference reads it from the already-built `RunResult`'s `sigterm`/`sighup`/`interrupted` flags (`run()`'s `finally:`), the same flags `--live-state`'s own distinction already relies on (§6.5); those flags are themselves set from the cancellation token. |

**Ordering.**
- `run_start` is always the first line.
- A container's `start` comes before any of its children's `start`s; every
  child's `end` comes before its container's `end` — true for `dynamic`,
  named `loop`, `if`, `reflector` and `use`, since each fires its own
  `fire_effect_start`/`fire_effect_complete` bracketing everything it runs
  (§5's per-effect-type sections; `disabled.py`'s `enabled: false` skip
  fires both with no gap, same pairing). Tree-flow siblings may interleave
  freely with each other.
- `run_end` is always the last line, and is written only *after*
  `--live-state`'s own final write (`LiveStateMirror.close`, in `run()`'s
  `finally:`) — seeing `run_end` means that final snapshot is already on
  disk.

**Failures never fail the run.** A failure to open the file at construction
(the parent directory is created first, like `--live-state`), or any later
failure writing a line — not just an `OSError`, *any* exception a write
raises — is logged once (a warning) and then ignored for the rest of that
run: every public method on the reference's own `EventLog` catches its own
exceptions and disables further writes, exactly the same contract
`--live-state` already has. `run()` folds that into one warning on
`RunResult.warnings`, the same way a `--live-state` write failure does.

**Abort.** A second SIGINT/SIGTERM/SIGHUP during cleanup ends the process at
once via `os._exit` (`cli/interrupts.py`, §6.5) — before `run()`'s own
`finally:` (and so before `run_end`) ever runs. A reader sees end-of-file
with no `run_end`, plus a dead process, and treats that as aborted, not as a
clean failure.

What the format leaves out, deliberately: no effect `value`, no prompt text,
no effect *kind* (a reader gets that from the compiled plan, or from the
shape of `meta` for a generated child) — those stay in `--live-state`/`--out`.

---

## 9. Quirks and apparent bugs

Each is a real, confirmed behavior of the reference implementation. An
electricity reimplementation must pick, explicitly, to copy or diverge from
each — silently doing neither is the actual risk.

**Q1 — Bare sibling references only work inside a loop body / `if` branch,
never at plain top level or inside a plain `dynamic` container.**
Documented ("Referencing a sibling within an iteration", orchestration
reference) only in the context of loop bodies, but easy to over-generalize.
Minimal repro (§2.4): a two-step top-level `effects:` list where step 2
writes `{{first.value}}` — renders **empty**; `{{prime.first.value}}` in the
same template renders correctly. Root cause: `core/dynamic.py`'s chain
executor never calls `scope_ctx`/`local_writes` — only `core/loop.py` and
`core/conditional.py` do. **Decision needed**: copy this distinction exactly
(bare form resolves only inside loop/if scope-overlay containers) or
normalize it (bare form always works, or never works). Copying it exactly is
what byte-identical parity requires; normalizing it is a deliberate,
documented divergence from `cof`.

**Q2 — `params_json`'s JSON-aware rendering vs. every other template site's
Python-`repr` rendering of lists/dicts.** A `{{{val}}}` splice of a native
list/dict renders as Python `str()` (`['a', 'b']`, `{'a': 1}` — single-quoted,
not valid JSON) **everywhere except** inside a tool's `params_json` field,
where the whole context is first wrapped in `_JsonAwareDict`/`_JsonAwareList`
so the same splice yields real `json.dumps` output. Minimal repro: a prompt
`template: "{{{mylist}}}"` with `mylist: [1, "a"]` in context renders
`[1, 'a']` (Python repr, invalid JSON) rather than `[1, "a"]`. **Decision
needed**: reproduce this asymmetry exactly (two different stringification
paths depending on which field is rendering) or unify on one (likely
JSON-everywhere, which would be a deliberate, documented improvement but a
parity break).

**Q3 — `use.inline`'s template renders against the *parent's* context,
before the child orchestration even exists; `use.inputs` has no effect on
what the inline template itself can reference.** Minimal repro (confirmed
live, §5.3): `inline: "... args: ['{{input.v}}'] ..."` with
`inputs: {v: "CHILD-VALUE"}` on a parent whose own `input.v = "PARENT-VALUE"`
produces a child tool call with the literal argument `"PARENT-VALUE"` — the
`{{input.v}}` inside the inline string is consumed by the **outer**
Mustache render (against the parent's `ctx`) before the resulting YAML text
is even parsed, so it can never see the child's own `input` namespace
(which doesn't exist yet at render time). To reference a `use` child's own
rendered input *inside* its own inline body, the inline YAML must **not**
template it at the top level — it has no way to defer that reference past
its own compile step, since `inline` is rendered exactly once, synchronously,
by the parent. **Decision needed**: match this exactly (`inline` is a single
parent-scoped render pass, full stop) — any attempt to "fix" this by
rendering `inline` twice (once against parent, once against child) would be
a new, undocumented behavior, not a bugfix of an existing one, and must not
be invented silently.

**Q4 — `while` loop's condition and body disagree by exactly one on
`iter.index`.** The condition (evaluated between passes) sees
`iter.index = iteration_count - 1` (the pass that just finished, `-1` before
any pass has run); the body of the pass about to run sees
`iter.index = iteration_count` (uncompensated). This is explicitly
deliberate in source commentary (matching a long-standing `+1`-compensated
convention authors already wrote), not an oversight — but it is exactly the
kind of one-line discrepancy a reimplementation's test suite must assert on
both sides of the loop, not just the body.

**Q5 — A tool's security-sensitive `allowed_commands` can only ever be a
literal list of strings — never from `params_json`, never from a `{from:}`
reference, never containing `{{`** — enforced identically at compile time
*and* dispatch time (two separate functions,
`_check_security_sensitive_param_leaf` / `_reject_templated_security_params`,
checking the same rule twice). This duplication is intentional defense in
depth (compile time catches a static document; dispatch time catches a
`use`-generated or reflector-generated document that was never statically
checked the same way) and both checks must be reproduced, not just one.

**Q6 — `expect:`'s "absent-is-false" exemption does not apply to `expect:`
itself**, only to `if`/`while` CEL conditions. `evaluate_cel_expect` has no
absent-path convention at all (`core/cel_eval.py:163` docstring, explicit):
a path that doesn't resolve on `value`/`meta` raises
`CelEvaluationError`, which the caller treats as "expectation failed" (not
"expectation vacuously true/false"). Reusing the `evaluate_cel`
absent-path-is-false logic for `expect:` would be a subtle, silent
divergence.

**Q7 — `collected` silently drops a failed or disabled pass's would-be
value**, even if that pass's `collect:` target itself successfully produced
a value before a *later* step in the same pass failed. The aggregation walks
only `completed_indices` (passes this `execute()` call itself finished
end-to-end), never "did the target step itself succeed" independently.

**Q8 — Octal YAML is `017`, not `0o17`.** PyYAML's safe loader implements
YAML 1.1's sexagesimal/octal/hex int resolvers, which do **not** match
Python's own `0o`-prefixed octal literal syntax — `0o17` parses as the
**string** `"0o17"`, not the integer `15`. An electricity YAML loader built
on a modern (YAML-1.2-leaning) library would need this resolver added back
explicitly, or every orchestration using legacy octal silently breaks.

**Q9 — `--out` (no `--pretty`) is insertion-ordered; `--out --pretty` is
alphabetically sorted.** Two genuinely different serializations of the same
state (§8.3) — not interchangeable modulo whitespace. A conformance check
that only ever runs with `--pretty` (because it's "easier to read the diff")
would never catch a key-ordering regression in the default, unpretty form,
which is what most automated tooling actually consumes.

**Q10 — A decode/schema-validation failure retries *through the fallback
chain*, not through the outer retry loop, on its first occurrence** — only
once every configured fallback has been tried and *all* failed does the
outer per-pass retry/backoff loop engage. A naive port that treats "model
replied, but I couldn't parse/validate it" as an ordinary dispatch failure
(triggering an immediate outer retry, skipping remaining fallbacks) changes
both the *order* of what gets tried and the final `fallback_attempts` log —
observable in state even when it happens to produce the same final `value`.

---

## 10. Conformance suite

Each entry: a minimal document + scripted model/tool replies + the exact
state (or sub-path) to diff against a reference `cof run` on this same
commit. "Scripted" means a fixed adapter reply list / fixed tool stdout, so
the run is fully deterministic.

| # | Document shape | Scripted inputs | Property compared |
|---|---|---|---|
| C1 | `interface.inputs` with every `type`, `default`, `required` combination; `-e` for each | `-e n=5 -e flag=true -e s=1.50` with `s` declared `type: string` | `state.input` matches type coercion + string-restoration rules exactly (§1.5–1.6) |
| C2 | Document with `on/off/yes/no`, `017`, `0x1A`, `1:30:00`, a `<<` merge, a duplicate top-level key | none (load-only, `validate_only`) | YAML 1.1 value types (§1.1); duplicate key raises with exact message |
| C3 | Two effects with the same name in one scope; an effect named `value` | n/a | Compile-time `ValueError` with exact message (§1.4) |
| C4 | Prompt with `template` referencing `{{missing}}`, `{{a.b.c}}` on a partial dict, `{{#list}}...{{/list}}`, `{{^flag}}...{{/flag}}` | scripted echo | `meta.prompt_sent` matches Mustache semantics table exactly (§3.1–3.2) |
| C5 | Tool `params_json` splicing a native list from a prior prompt's `array` output, vs. the same list spliced via `{{{...}}}` in an ordinary `template:` | scripted JSON array reply | `params_rendered`/`prompt_sent` show the JSON-vs-repr asymmetry (Q2) |
| C6 | `if: {mode: cel, expr: "has(state.input.x) && state.input.x > 1"}` run with `x` absent, `x: 0`, `x: 5`; same with `strict: true` | n/a | `condition_result` / raised-error exactly matches the absent-path table (§4.4) |
| C7 | `if`/`while` CEL: `1 == true`, `1.0 == 1`, `"a" == 1` | n/a | Heterogeneous-equality results (§4.3) |
| C8 | Named `each` loop, `flow: chain`, body reads `prime.<loop>.prev.<step>.value` on pass 2; `collect:` on a step that is `disabled: true` via profile override on one pass | scripted tool replies, 3 items | `prev` absent on pass 0, correct on later passes; `collected` elides the disabled pass (§2.3, §2.4, Q7) |
| C9 | Named `each` loop, `flow: tree`, 4 items, `max_concurrency: 2` | scripted tool replies | `iter_0..3` all present, `last == iter_3`, order-independent of actual completion order |
| C10 | `while: {mode: cel}` loop with `min_iterations: 2`, condition reading a path only the body writes | scripted tool replies | Condition never evaluated on the forced passes (no absent-path warning in logs); `iter.index` off-by-one between condition and body (Q4) |
| C11 | `each` loop, collection longer than `max_iterations`, once with `each.truncate: true` and once without, both `on_error: fail` and `on_error: continue` | n/a (fails before dispatch) | `LoopBoundsError` message exactly; `termination.reason`/`unvisited`/`detail` per combination (§5.4) |
| C12 | Tool with `provider: shell`, `params.allowed_commands` set via (a) literal list, (b) `{{templated}}` item, (c) `{from: ...}`, (d) via `params_json` | n/a | (a) succeeds, (b)/(c) raise at compile or dispatch, (d) raises "must not set" (Q5) |
| C13 | Prompt with `provider_fallbacks`, first fallback returns unparseable JSON for `prompt_type: json` with `schema:`, second fallback returns valid JSON | scripted: [bad-json, good-json] | Single pass succeeds (no outer retry consumed); `fallback_attempts` shows `decode_failed` then `succeeded`, in that order (Q10, §5.1) |
| C14 | Same as C13 but every fallback fails decode | scripted: [bad, bad] | Pass fails as a whole; raised error is the decode error itself, not a generic "all attempts failed" wrapper (§5.1) |
| C15 | `expect:` CEL reading `value.missing_key` | n/a | Raises `CelEvaluationError`, treated as expectation failed — not vacuously true/false (Q6) |
| C16 | `use: {inline: "...{{input.v}}...", inputs: {v: "X"}}` on a parent with its own `input.v = "Y"` | n/a | Child tool receives literally `"Y"`, never `"X"` (Q3) |
| C17 | `use` with explicit `outputs:`, with auto `interface.outputs`, and with neither | n/a | Three different node shapes (mapped value / mapped value / full-namespace passthrough) exactly as §5.3 |
| C18 | `use` child whose own nested effect fails under `on_error: skip` | n/a | Parent `use` node succeeds (`meta.error: null`) but `meta.child_errors` lists the swallowed failure (§6.2) |
| C19 | `dynamic flow: tree` with `stop_on_error: true`, 3 children, the first to finish fails | scripted: one slow success, one fast failure, one slow success | The still-pending (not-yet-started) future is cancelled; already-running ones finish; final state only has nodes for children that actually ran |
| C20 | Two leaf effects sharing a `group:` with `concurrency_groups: {g: 1}`, dispatched from a tree loop | scripted, deliberately slow | Serialization is observable only via timing/`waiting_for`, never via final `value` — a conformance check here is "did not deadlock", not a value diff (§7) |
| C21 | A document resumed via `--state <previous --out>` where effect 2 of a 3-step chain previously failed | re-run with the same scripted replies | Effect 1 skipped (`resumed`), effect 2 reruns, effect 3 runs fresh — never skips past the stale "finished-looking" node of effect 3 if present from a different timeline (§8.5) |
| C22 | Named chain loop, 5-pass `each`, process "killed" (test harness stops) after pass 3's node is fully written but before pass 4 starts; `--resume` | re-run | Resumes at pass 4 exactly, using `meta.completed_passes`, not by eyeballing `iter_4`'s absence (§8.5) |
| C23 | `--out` and `--out --pretty` of the same run | n/a | Byte-for-byte diff against reference: insertion order (plain) vs. sorted keys + 2-space indent (pretty); both `ensure_ascii`-escaped (Q9, §8.3) |
| C24 | Named loop whose `last` aliases `iter_2` in memory | n/a | `--out` writes `"last": {"$ref": "iter_2"}`; loading that file back and re-saving round-trips byte-identical (§2.3) |
| C25 | Ctrl-C (SIGINT) and SIGTERM delivered mid-run, each with a `finally:` on the root | signal harness | `finally:` runs in both cases; exit codes 130 vs 143 respectively; `--out` written in both cases with `runtime.last_run.completed_at` set (§6.5, §8.4) |
| C26 | `runtime.complexity.scoring.enabled` true vs. absent, identical document/inputs otherwise | n/a | `meta.complexity` key present/absent — every *other* byte of state identical (§5.8) |
| C27 | Top-level plain `effects:` list, step 2 reads step 1 via bare `{{step1.value}}` and via `{{prime.step1.value}}` | n/a | Bare form empty, dotted form resolves (Q1) — the single most important parity assertion in this list |
| C28 | `type: yield` with a plain template, no `model`/`adapter` configured at all | n/a | `prime.<name>.value` is the rendered text; no adapter call is attempted (§3.6, §5.9) |
| C29 | Declared `prompts:` map, `{{> name}}` splicing a declared prompt into another declared prompt and into a `yield` template, with the declared prompt's text ending in `\n` | n/a | Nested declared-prompt expansion; the one-trailing-newline-drop rule (§3.6) |
| C30 | Two `{{> name}}` tags concatenated in one tool `params` string, each naming a different `yield` effect | n/a | Both splice in, unescaped by the surrounding (escaped) tool-param context (§3.4, §3.6) |
| C31 | `yield` template `{{input.x}}`, vs. the same value through a tool `params` string | `-e x='"<b>&'` | Prompt text (`yield.template`) renders `{{input.x}}` unescaped; the tool param still HTML-escapes it (§3.1, §3.4) |
| C32 | Declared `prompts:` map with one entry sourced from `{file: <path>}`, spliced via `{{> name}}` into a `yield` template alongside an ordinary `{{input...}}` reference | `-e topic=circuitry` | The file's own text is read at compile time and used exactly as inline text would be (§3.6.2) |
| C33 | Two declared prompts, each nesting a different `yield` effect's reference, used side by side (`{{> a}}-{{> b}}`) and in reverse order (`{{> y}} {{> a}}`) | n/a | Both resolve independently (`X-Y`, `Y X`) — the whole recursive expansion shares one synthetic-key counter, so two sibling references nested inside different declared prompts never mint the same key (§3.6) |

---

*End of document. electricity's own v1 scope decisions (no caching, no
TUI/wizard, explicit config only) bound which parts of this spec electricity
v1 must implement versus may defer.*
