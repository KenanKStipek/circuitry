# State

Every effect writes to a deterministic path derived from its name, and every later effect reads it there. That single rule is what makes interpolation — and therefore cybernetic feedback — reliable. It is worth being precise about, because the rest of the language rests on it.

State is a real-time graph that shadows the run. The YAML fixes the graph's shape before execution begins — every effect name is a node address — and execution fills it in. A snapshot at any instant is the run's current truth: what has completed, what it produced, which branch was taken, which pass a loop is on, which model answered and why. `--live-state` streams that graph to a file as it grows; `--out` writes the finished one.

## Three namespaces

State has exactly three root namespaces. Every path starts with one of them.

| Namespace | Holds | Written by |
| --- | --- | --- |
| `input` | What the caller supplied | CLI `-e` / `--state`, a profile's `inputs`, a parent's `use.inputs`, the REST or scheduler payload, the TUI launch form |
| `prime` | What effects produced | Every effect, at the path its name and position dictate |
| `runtime` | What the framework recorded | The runtime: `last_run`, `effective_settings`, `plugins`, `persistence`, `library_refs` |

`input` is read-only from the orchestration's point of view; `prime` is where the run happens; `runtime` is the audit trail. Every entry point wraps caller-supplied keys under `input` for you — `cof run dinner.yml -e occasion=anniversary` puts `anniversary` at `input.occasion` — so the spelling inside the document is always `input.<name>`, whether or not the document declares the input in an [interface](09-composition.md).

## Where effects write

| Effect | Writes | Notes |
| --- | --- | --- |
| `prompt`, `tool` | `prime.<name>.value` | plus `meta` |
| `use` | `prime.<name>.value` | a dict of declared outputs — or the child's whole `prime` subtree under `prime.<name>.<child_effect>` when nothing is declared; `meta.inputs` records what the child received |
| `dynamic` | `prime.<name>.<child>…` | one segment per container |
| named `if` | `prime.<name>.<branch_effect>…` | plus the decision under `value` and `meta` |
| unnamed `if` | into the parent scope | the branch's effects write as if they were siblings of the `if` |
| named `loop` | `prime.<name>.iter_<n>.<step>…`, `…last…`, `…collected` | plus iteration count and termination reason under `value` |
| unnamed `loop` | into the parent scope | each pass overwrites the same paths |
| `reflector` | `prime.<name>.inner…`, `prime.<name>.generated.iter_<n>…` | the planning prompt and the plans it ran |

The rule beneath the table: **the path is the full path from `prime`, one segment per named container.** A `wine` prompt inside a `menu` dynamic inside a `plan` dynamic is `prime.plan.menu.wine.value` from anywhere in the document.

## Two reading languages, one namespace

**Mustache**, inside any `template:` — a prompt's template, a tool's `params`, a `use` effect's `inputs`, a model-mode condition, an `inline` orchestration:

```
{{input.occasion}}                    caller input
{{prime.suggest_dish.value}}          a top-level effect's value
{{prime.menu.wine.value}}             a value inside a dynamic
{{prime.parse_recipe.value.servings}} a field of a structured value
{{prime.courses.last.cook.value}}     the final pass of a loop
{{{prime.draft.value}}}               the same read, without HTML escaping
```

**CEL**, inside any `expr:` — a `cel`-mode `if`, a `cel`-mode `while`. Here `state` is bound to the root, so every path gains that prefix:

```
state.input.guests > 6
state.prime.suggest_dish.value != ""
state.prime.parse_recipe.value.servings >= 8 && size(state.prime.parse_recipe.value.sides) >= 2
!(state.input.diet == 'vegetarian')
```

This is real [CEL](https://github.com/google/cel-spec), evaluated by cel-python: comparisons, `&& || !`, the `?:` ternary, `has()`, the macros (`all`, `exists`, `exists_one`, `map`, `filter`), and the standard functions (`size()`, `int()`, `string()`, `matches()`, …). Equality is typed the way CEL specifies it, so `1 == true` is `false`. `cof check` parses every expression, so one that does not parse is an error before the run. `state` is the only binding; `cof run learn/cel_showcase` runs one branch per construct.

One rule is Circuitry's own. **An expression that reads an unset path is `false` as a whole**, whatever the operator: a `state.` path that is missing or resolves through `null` makes the whole expression false, and the runtime logs a warning that names the path. So `state.prime.fetch_recipe.value != ""` is false after the fetch was skipped, and so is `state.prime.fetch_recipe.value == null`. Only `has()` is exempt: `has(state.input.diet)` asks whether the key is there. [If](06-if.md) covers `strict: true`, which makes an unset path an error.

## Three spellings that fail — and two that fail only at run time

The compiler enforces the namespace rule wherever it can see the path. Three shapes are hard errors from `cof check`, each with the fix spelled out in the message:

```yaml
# ✗ each.in must be rooted at input. / prime. / runtime. (or at an enclosing loop's binding)
- type: loop
  each: {in: menu, as: course}
  body:
    - type: prompt
      name: cook
      template: "Write the cooking steps for: {{course}}"
```

```yaml
# ✗ in CEL, state.<key> must name one of the three namespaces
- type: if
  if: {mode: cel, expr: "state.diet == 'vegetarian'"}
  then:
    - type: prompt
      name: main_course
      template: "Suggest a vegetarian main course."
```

```yaml
# ✗ a declared interface input is read as {{input.<name>}}, never bare
interface:
  inputs:
    occasion: {type: string, required: true}
effects:
  - type: prompt
    name: suggest_dish
    template: "Suggest one main course for {{occasion}}."
```

Two more shapes are well-formed YAML that the compiler cannot flag. A template is free text and a missing reference is legal Mustache, and the validator checks the paths written after `state.`, not the names written without it. Both pass `cof check`:

```yaml
# ✗ runtime — state. is a CEL binding; in a template it resolves to nothing
- type: prompt
  name: suggest_dish
  template: "Suggest one main course for {{state.input.occasion}}."
```

```yaml
# ✗ runtime — the CEL root is missing, so the first evaluation fails the run
- type: if
  if: {mode: cel, expr: "input.diet == 'vegetarian'"}
  then:
    - type: prompt
      name: main_course
      template: "Suggest a vegetarian main course."
  else:
    - type: prompt
      name: main_course
      template: "Suggest a main course."
```

The first goes green and renders "Suggest one main course for ." The second fails the run the first time it is evaluated. `input` is not a name CEL knows (`state` is the only binding), so the `if` records a CEL evaluation error on its `meta.error`, and the run ends with `ok` false. The second failure is loud; the first is silent. For both, read what the run recorded: `meta.prompt_sent` on the prompt, and `meta.error` on the `if`.

## Rendering values

A `text` value interpolates as itself. A structured value is a dict or a list, and interpolating the whole thing renders its string form — `{'name': 'Roast', 'sides': ['greens', 'bread']}` — which is rarely what a prompt wants. Read fields: `{{prime.dish.value.name}}`, `{{prime.dish.value.sides}}`. To hand a whole collection to the model, prefer iterating it with a [loop](07-loop.md), or ask the producing prompt for prose in the first place.

A node without `.value` is also a legal read and renders the whole node, `meta` included. Nearly always a mistake:

```yaml
# ✗ runtime — renders the whole node, timestamps and all; you meant .value
- type: prompt
  name: card
  template: "Tonight: {{prime.suggest_dish}}"
```

**Escaping.** `{{…}}` HTML-escapes what it interpolates: `Roast & Co` becomes `Roast &amp; Co`. That is correct for HTML and wrong for a prompt. When the value is prose, code, or anything that may contain `& < > "`, use triple-stache — `{{{prime.draft.value}}}` — and the text passes through untouched.

## Inside a loop

Two bare bindings exist only inside a loop body: `{{<each.as>}}` (the current element of an `each` loop, `{{item}}` by default) and `{{_loop_index}}` (the zero-based pass number, in `each` and `while` loops). They reach every effect in the body however deeply it is nested — inside an `if` branch, a grouping dynamic, an inner loop.

CEL has the same two bindings, one level down: in an `if` condition inside the body, `state.<each.as>` is the current element and `state.iter.index` is the pass number. They are legal only inside the loop that binds them; outside it, `state.course` is the namespace error above. An inner loop's `each.in` can also start at an outer loop's binding (`course.steps`), and [Loop](07-loop.md) shows it.

Within the body, `{{prime.<step>.value}}` means *this pass's* `<step>`. Resolution is a scope chain: the current iteration first, then the enclosing scope, then root — so a root input or an effect that ran before the loop keeps resolving, and a body step with the same name as an outer effect shadows it for the length of the body. The four read forms for loop state — this pass, a fixed pass, the final pass, every pass — are the subject of [Loop](07-loop.md); they are the most-consulted table in the reference.

## The `runtime` namespace

```
runtime.last_run                    # run_id, orchestration_path, dry_run, started_at, completed_at
runtime.effective_settings          # every resolved setting …
runtime.effective_settings.sources  # … and which layer supplied each one
runtime.plugins                     # contract version, loaded plugins, hook events
runtime.persistence                 # backend, status, loaded_from_persistence
runtime.library_refs                # every ref: a use effect resolved, with its pin
```

`runtime.effective_settings.sources` deserves a sentence of its own. For every setting — model, adapter, `out`, the complexity switches — it records `cli`, `profile`, `orchestration`, `config`, `default`, or `router`. Every model decision is auditable after the fact, from the state file alone. [Configuration](04-configuration.md) explains the layers.

`runtime.*` in a document merges over config with the orchestration winning, which is how an orchestration can carry its own `runtime.complexity` block. CEL can read `state.runtime.<key>` and a loop can iterate a `runtime.` path, though there is rarely a reason to.

## Watching it fill in

```bash
cof run dinner.yml -e occasion=anniversary --live-state ./dinner.live.json
cof run dinner.yml -e occasion=anniversary --out ./dinner.json --pretty
cof run dinner.yml -e occasion=anniversary --json | jq '.prime.suggest_dish'
```

`--live-state` mirrors the graph to the file while the run goes. It writes the first snapshot at once, then at most every half second, and the last write at the end of the run makes the mirror equal to the `--out` state. Each write is atomic, so a viewer pointed at the file watches the graph grow. `--out` is the finished record. `--json` (automatic when stdout is not a terminal) prints it.

A saved state holds each loop's final pass only once. In `--out`, `--json`, and the live mirror, a named loop's `last` is written as a reference to the pass it aliases, `"last": {"$ref": "iter_2"}`. Every reader that loads a state file links it back: `--state`, a persistence resume, and the TUI's Runs view. A run-time read of `{{prime.courses.last.cook.value}}` does not change. Only a tool that reads the file directly must follow the reference: `jq '.prime.courses | .[.last["$ref"]]'`.

And when a run diverges from what you expected, `inspect_divergence_paths(state)` from the SDK walks the whole tree and returns every node with a `meta.error`, in path order — [Troubleshooting State Paths](../troubleshooting-state-paths.md) is the workflow built around it.

## Anti-patterns

**Bare keys.** `{{occasion}}` for a caller input, `each: {in: menu}`, `state.diet` in CEL. The document has three namespaces; spell them. Undeclared bare template keys are tolerated for backward compatibility, but nothing in this guidebook writes one, and a declared input written bare is an error.

**Reading a container as if it were a leaf.** `prime.menu.value` is `true` when the dynamic finished; it is not the wine. `prime.courses.value` is the loop's iteration count and termination reason; it is not the collected courses.

**Depending on the string form of a structure.** Ask for fields, or iterate.

**Forgetting the escaping.** One ampersand in a draft, and every downstream prompt reads `&amp;`. Triple-stache prose.

## See also

- [Orchestration Reference → State Path Addressing](../orchestration-reference.md#state-path-addressing).
- [Troubleshooting Deterministic State Paths](../troubleshooting-state-paths.md).
