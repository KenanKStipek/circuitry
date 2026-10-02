# Grammar

Formal grammar of the Circuitry orchestration language, derived from [`src/circuitry/schema/orchestration.schema.json`](../../src/circuitry/schema/orchestration.schema.json) — the validation authority. Where this page and the schema disagree, the schema wins and this page has a bug. EBNF-style over YAML semantics: `?` optional, `+` one-or-more, `*` zero-or-more, `|` alternation, `⊕` exclusive choice (exactly one). Map keys may appear in any order.

## Terminals

```
NAME        ::= /^[A-Za-z_][A-Za-z0-9_]*$/          — never iter_<N> (reserved);
                                                       'last' reserved under loops;
                                                       'value'/'meta'/'input'/'prime'/'runtime'
                                                       reserved everywhere (structural slots)
STRING, INT(≥0 unless noted), NUMBER, BOOL
TEMPLATE    ::= STRING with Mustache: {{input.<k>}} {{prime.<path>.value}} {{<loop_var>}} {{_loop_index}}
                (triple-stache {{{…}}} to skip HTML escaping)
CEL         ::= STRING in CEL (cel-python); 'state' bound to the shadow state's root, the only binding;
                inside a loop's own body also state.<each.as> and state.iter.index;
                an unset or null state. path makes the whole expression false (unless strict).
                Exception: a Tool/Use Expect's CEL binds 'value' and 'meta' directly (this
                effect's own result, not state.value/state.meta) plus 'state' as usual.
STATE_PATH  ::= dot-delimited path rooted at input. | prime. | runtime.
BINDING_PATH ::= dot-delimited path rooted at an enclosing loop's each.as (or iter)
REF         ::= { from: STATE_PATH | BINDING_PATH }     — passes the value unchanged
JSONSCHEMA  ::= a JSON-Schema (draft-07) object
```

## Document

```
Orchestration ::= { effects: Effect+,                  — required, executed in order
                    finally?: Effect+,                  — cleanup, after effects win or lose; best
                                                          effort on cancellation; also legal on Dynamic
                    interface?: Interface,
                    flow?: Flow,                        — root topology, default chain
                    version?: STRING,                   — author's own; runtime ignores
                    adapter?: STRING, model?: STRING }  — legal, discouraged: capability
                                                          belongs to config/router

Flow          ::= 'chain' | 'tree'
OnError       ::= 'fail' | 'skip' | 'continue'          — effects
OnErrorLoop   ::= 'fail' | 'break' | 'continue'         — loops

Interface     ::= { inputs?:  { NAME: InputDecl … },
                    outputs?: { NAME: OutputDecl … } }
InputDecl     ::= { type?: 'string'|'number'|'boolean'|'array'|'object',
                    required?: BOOL,                    — default false
                    description?: STRING }
OutputDecl    ::= { path: STATE_PATH, type?: STRING, description?: STRING }
                | STATE_PATH                            — shorthand for {path: …}
```

## Effect — discriminated by `type`, seven productions

```
Effect ::= Prompt | Dynamic | If | Loop | Tool | Use | Reflector
```

### Prompt — one model call → `prime.<name>.value`

```
Prompt ::= { type: 'prompt', name: NAME,
             template: TEMPLATE ⊕ messages: Message+,
             prompt_type?: 'text'|'json'|'object'|'array'|'number'|'boolean'|'tool',   — default text
             schema: JSONSCHEMA,                        — REQUIRED iff prompt_type ∈ {json,object,array}
             model?: STRING, provider?: STRING, provider_fallbacks?: STRING*,
             params?: MAP, inputs?: MAP,                — inputs: prompt-local template vars
             assets?: Asset*, retries?: Retry,
             timeout_ms?: INT, deterministic?: BOOL,
             on_error?: OnError, description?: STRING }

Message ::= { role: 'system'|'user'|'assistant'|'tool', content: STRING }
Asset   ::= { kind: STRING, ref: STRING }
Retry   ::= { max_attempts?: INT≥1, backoff_ms?: INT }
```

### Dynamic — named container → `prime.<name>.<child>.value`

```
Dynamic ::= { type: 'dynamic', name: NAME, effects: Effect+,
              finally?: Effect+,                       — cleanup, after effects win or lose; best
                                                          effort on cancellation; always sequential
              flow?: Flow, max_concurrency?: INT≥1,     — tree only
              stop_on_error?: BOOL, on_error?: OnError,
              labels?: MAP, description?: STRING }
```

### If — one branch runs

```
If        ::= { type: 'if', if: Condition, then: Effect*,
                else?: Effect*, name?: NAME,             — named ⇒ nested paths + decision meta;
                                                           unnamed ⇒ branch effects merge into parent scope
                threshold?: NUMBER∈[0,1],                — deprecated, no effect: see If
                on_error?: OnError, labels?: MAP, description?: STRING }

Condition ::= { mode: 'model', template: TEMPLATE }      — LLM answers yes/no
            | { mode: 'cel',   expr: CEL,                — deterministic
                strict?: BOOL }                          — default false; true ⇒ unset path is an error
```

### Loop — each xor while

```
Loop ::= { type: 'loop', body: Effect+,
           each: { in: STATE_PATH | BINDING_PATH,        — resolves to an array
                   as?: NAME,                            — default 'item'
                   truncate?: BOOL }                     — default false; see max_iterations
           ⊕ while: Condition,                           — checked before each pass; always sequential
           name?: NAME,                                  — named ⇒ iter_<N>/last/collected nodes;
                                                           unnamed ⇒ body overwrites at stable paths
           collect?: NAME,                               — a body step; needs a named loop
           flow?: Flow, max_concurrency?: INT≥1,         — each-loops only
           max_iterations?: INT≥1,                       — no default (uncapped); each: a longer collection
                                                           fails at start unless each.truncate
           min_iterations?: INT,                        — while loops only
           on_error?: OnErrorLoop, labels?: MAP, description?: STRING }
```

Read grammar (post-loop unless noted): `prime.<step>.value` (this pass, body/while only) ·
`prime.<loop>.prev.<step>.value` (previous *completed* pass, body only — chain flow; absent
on the first pass; a `flow: tree` body referencing it is a `cof check` error) ·
`prime.<loop>.iter_<N>.<step>.value` · `prime.<loop>.last.<step>.value` (last *completed* pass) ·
`prime.<loop>.collected.value`. In a saved shadow state `last` is `{"$ref": "iter_<N>"}`, linked back on load.
Termination (`prime.<loop>.value.termination.reason`): `collection_exhausted` · `condition_false` ·
`max_iterations_reached` (+ `unvisited` when truncated) · `collection_unresolved` · `condition_error` ·
`error` (+ `detail`, on both a raised pass and the each-loop bounds check).

### Tool — deterministic plugin call → `prime.<name>.value`

```
Tool ::= { type: 'tool', name: NAME, provider: PLUGIN_NAME,
           prompt?: TEMPLATE, model?: STRING,
           params?: MAP,                                 — string values Mustache-rendered; wins over prompt/model
           params_json?: TEMPLATE,                       — renders to a JSON object; deep-merged over params
           timeout_ms?: INT, retries?: Retry, expect?: Expect,
           on_error?: OnError, description?: STRING }

Expect ::= CEL                                           — shorthand for {mode: cel, expr: CEL}
         | { mode: 'cel',   expr: CEL }                  — over this effect's own value/meta, plus state
         | { mode: 'model', template: TEMPLATE }         — LLM answers yes/no, on the run's adapter/model
```

### Use — child orchestration in isolation

```
Use ::= { type: 'use', name: NAME,
          ref: LIBRARY_REF ⊕ path: FILE_PATH ⊕ inline: TEMPLATE,   — inline renders to YAML at runtime
          validate?: BOOL,                               — default true; schema-gates inline YAML
          inputs?: { NAME: value|TEMPLATE|REF … },       — become the child's input.*; TEMPLATE arrives as text
          outputs?: { NAME: OutputDecl … },              — omitted ⇒ full child namespace exposed
          retries?: Retry,                               — a retry re-runs the whole child from scratch
          expect?: Expect,                                — over this effect's own mapped value/meta, plus state
          on_error?: OnError, description?: STRING }

LIBRARY_REF ::= '<category>/<name>' | '<source>:<category>/<name>'
```

### Reflector — plans its own effects

```
Reflector ::= { type: 'reflector', name: NAME, effects: Effect+,   — base template per cycle
                plan_from_step?: NAME,                   — default propose_steps
                max_iterations?: INT≥1,                  — default 1
                generated_key?: STRING,                  — default generated
                stop_on_done?: BOOL,                     — default true
                max_effects?: INT≥1,                     — default 8
                prime_template?: STRING, flow?: Flow, description?: STRING }
```

## Static semantics (beyond shape)

1. Sibling names unique; names are path segments, never paths; never type-keywords (the validator rejects duplicates and warns on type-keyword names).
2. `each.in`, CEL paths, `use` input references, and template refs must root at a namespace — or, inside a loop, at an enclosing loop's binding (`suite.tests` for `each.in` and references, `state.test` / `state.iter.index` in CEL); a declared `interface.inputs` name is referenced only as `{{input.<name>}}` / `state.input.<name>` — anything else is a compile error. Every CEL expression must parse.
3. `schema` required ⇔ structured `prompt_type`; `template` required ⇔ `mode: model`; `expr` required ⇔ `mode: cel`.
4. `collect` must name a body step, and needs a named loop to have anywhere to write — on an unnamed loop this is a `cof check` error naming the fix (give the loop a `name`). Chain-order reads only; tree siblings can't read each other.
5. `use` cycle guard: an orchestration cannot (transitively) `use` itself.
6. `runtime.*` in a document merges over config (orch wins) — the complexity block rides this.
7. `finally` is legal only on the document root and on a Dynamic effect — anywhere else is a compile error. A failure inside `finally` never hides the main `effects`' own failure (reported as-is, with the `finally` failure added as a second note); on an otherwise-successful run, a `finally` failure fails it too, unless that `finally` effect's own `on_error` is `skip`/`continue`.
8. A false or unreadable `expect` fails the attempt (`retries`/`on_error` apply) with `expect failed: <expr or template summary>`.

## Cybernetic overlay (config grammar, `runtime.complexity`)

```
Complexity ::= { scoring?:       { enabled?: BOOL, weights?: MAP, keywords?: MAP },
                 routing?:       { enabled?: BOOL, respect_explicit?: BOOL,
                                   bands?: Band+ },      — ordered; final band omits max (catch-all, required)
                 decomposition?: { enabled?: BOOL, threshold?: NUMBER, max_chunks?: INT,
                                   max_depth?: INT, on_failure?: 'route_up'|'fail' } }
Band       ::= { name: STRING, max?: NUMBER, model: STRING }   — max inclusive; models are opaque strings
```
