# Composition

> "Observers are men, animals, or machines able to learn about their environment." — Gordon Pask, *An Approach to Cybernetics* (1961)

Part II closed the loop inside one document. Part III is the machine meeting the world, and the first thing it meets is *other documents*. An orchestration that does one thing well — critique a draft, extract fields, pick one of several candidates — is worth calling from other orchestrations, and calling it should be as safe as calling a function: it gets the inputs you hand it, nothing else; it hands back the outputs it declared, nothing else. That is the `use` effect, and the contract it calls through is the `interface`.

Orchestrations as organs. The curation library is the organ bank. And — the reason this chapter sits where it does — composition is the prerequisite for [decomposition](11-decomposition.md), where the runtime uses the same mechanism to run plans it wrote itself.

## `use` — the composition effect

```
Use ::= { type: 'use', name: NAME,
          ref: LIBRARY_REF ⊕ path: FILE_PATH ⊕ inline: TEMPLATE,
          validate?: BOOL,                               — default true; gates inline YAML
          inputs?: { NAME: value | TEMPLATE … },         — become the child's input.*
          outputs?: { NAME: OutputDecl … },              — omitted ⇒ full child namespace exposed
          on_error?: 'fail'|'skip'|'continue', description?: STRING }

LIBRARY_REF ::= '<category>/<name>' | '<source>:<category>/<name>'
```

A `use` runs another orchestration as an isolated sub-step. The child runs in its own store: only the mapped `inputs` pass in, only the mapped outputs come back. Exactly one of `ref`, `path`, or `inline` says which document.

```yaml
- type: use
  name: sauce
  path: ./make_sauce.yml            # a sauce is a sub-recipe: its own document
  inputs:
    base: "{{prime.main_course.value}}"
    style: pan
  outputs:
    recipe: {path: prime.compose.value, type: string}
```

**`inputs`** is a map of child input name to value; string values are Mustache-rendered in the parent before they cross. Inside the child they are `input.base` and `input.style`, indistinguishable from values a caller would have passed on the command line. The child cannot see the parent's `prime` — there is no path from inside `make_sauce.yml` to the parent's `main_course`, which is the point: a child that could read its caller's state could not be reasoned about on its own.

**`outputs`** is a map of parent-side name to a path *in the child*: `{path: prime.compose.value, type: string, description: …}`. When present, `prime.sauce.value` is a flat dict of those names — `{{prime.sauce.value.recipe}}` downstream. (A bare string, `recipe: prime.compose.value`, is accepted shorthand for `{path: …}`; write the object form — it is the one the library uses and the one with somewhere to put a `type`.)

### Three ways to name the child

**`ref:`** — a library lookup, slash-delimited. `ref: utilities/critique` resolves through every configured [library source](#the-library) in precedence order and takes the first match; `ref: hub:utilities/critique` resolves in exactly the source named `hub` and never falls through. This is the form for anything in the library. Every resolved ref is recorded in `runtime.library_refs` (and on the effect as `meta.library_ref`) with its source, path, and — for a GitHub source — the commit SHA it was served from, so a later run can reproduce the identical tree with no network at all.

**`path:`** — a filesystem path: absolute, working-directory-relative, or relative to the parent orchestration's own directory. For project-local helpers that are not library entries.

**`inline:`** — a Mustache template that renders to orchestration YAML at run time. This is how an LLM-generated plan runs: a prompt writes the document, and a `use` executes it, after validating it against the full schema (`validate: true`, the default — keep it):

```yaml
- type: prompt
  name: plan_prep
  template: "Write a Circuitry orchestration YAML that preps the kitchen for: {{input.dish}}. Output YAML only."

- type: use
  name: run_prep
  inline: "{{{prime.plan_prep.value}}}"
  validate: true
```

Triple-stache matters here: the plan is YAML, and `{{…}}` would HTML-escape its quotes. An `inline` child is the mechanism under the [reflector](08-reflector.md) and under [decomposition](11-decomposition.md); they add the planning directive and the bookkeeping, and delegate the execution to exactly this.

(A fourth field, `orchestration:`, is a deprecated hybrid of `ref` and `path`. It still parses and warns. Write `ref` or `path`.)

### Output modes

| Mode | When | What lands |
| --- | --- | --- |
| Declared outputs | the `use` has `outputs:`, **or** the child declares `interface.outputs` | `prime.<use>.value` is a flat dict of the declared names |
| Full namespace | neither | the child's entire `prime` subtree at `prime.<use>.<child_effect>.value` |

An explicit `outputs:` on the `use` wins over the child's `interface.outputs`. Full-namespace mode is convenient for a quick project-local child and wrong for a library entry: a caller that reaches into `prime.critique_step.critique.value` is coupled to the child's internal effect names, which is precisely what the interface exists to hide.

## `interface` — the contract

```
Interface ::= { inputs?:  { NAME: InputDecl … },
                outputs?: { NAME: OutputDecl … } }
InputDecl  ::= { type?: 'string'|'number'|'boolean'|'array'|'object', required?: BOOL, description?: STRING }
OutputDecl ::= { path: STATE_PATH, type?: STRING, description?: STRING }
```

An orchestration declares its contract with a top-level `interface:` block — the child's mise en place, everything it needs laid out and named before any heat is applied:

```yaml
# make_sauce.yml
interface:
  inputs:
    base:
      type: string
      required: true
      description: The dish the sauce accompanies.
    style:
      type: string
      required: false
  outputs:
    recipe:
      type: string
      path: prime.compose.value

effects:
  - type: prompt
    name: compose
    template: "Compose a {{input.style}} sauce for {{input.base}}."
```

Two things happen when a `use` calls a document with an interface. **Required inputs are validated** — a caller that omits `base` fails with *missing required input 'base' declared in orchestration interface* before the child runs a single effect. And **outputs are auto-generated**: the caller's mapping is the child's `interface.outputs`, so callers do not repeat dot-paths, and the child can rename its internal effects without breaking anyone. The `use` in the first example could drop its `outputs:` block entirely.

The interface is also what `cof info` prints, what `cof list` shows as inputs, and what the [wizard](12-surfaces.md) reads when it composes library entries into a draft. Declare one on anything reusable-shaped.

A declared input has one spelling inside the document — `{{input.base}}` in templates, `state.input.base` in CEL — and the compiler holds you to it:

```yaml
# ✗ a declared input read bare is a compile error
interface:
  inputs:
    base: {type: string, required: true}
effects:
  - type: prompt
    name: compose
    template: "Compose a sauce for {{base}}."
```

## Isolation and observation

Isolated *state*, shared *observation*. The child's effects are reported to the parent run's observers — `--live-state`, the TUI, every runtime plugin's `on_effect_start` / `on_effect_complete` — at paths namespaced under the `use` node: a child effect `compose` inside `use: sauce` announces as `prime.sauce.compose`, and a `use` inside a `use` composes the same way. Watchers see the whole tree unfolding; what actually lands in parent state is still exactly what the output mode says.

**Cycles are rejected.** The compiler walks the static `use` graph across sources — A in a folder, B in the hub cache, A again — and refuses at validation. The runtime keeps a per-execution stack of resolved paths and hashes rendered inline YAML, so a cycle that closes only at run time is caught too. Two utilities that legitimately need each other are a sign of a third utility they both need.

**Unfetched sources fail early.** A `ref` into a GitHub source whose cache was never populated fails at validation, naming the command that fixes it (`cof library refresh hub`) — never mid-run. A genuinely unknown ref (a typo) surfaces as the `use` effect's own error when it runs.

Errors inside the child are the child's, under its own effects' `on_error`; a child that fails as a whole fails the `use`, under the `use`'s `on_error`. Nothing partial ever lands — a failed child's scratch state is discarded whole.

## The library

`ref:` resolves through `runtime.library.sources`, an ordered list where **order is precedence**. With no configuration the list is `[{type: curation}]`: the bundled library, organised by category.

| Category | What lives there |
| --- | --- |
| `learn/` | single-primitive demonstrations, one concept per file — the first read |
| `utilities/` | composable, single-output orchestrations with interfaces: `summarize`, `critique`, `refine`, `judge`, `classify`, `decompose`, `extract`, `route` |
| `patterns/` | multi-primitive templates: `critique_refine_loop`, `parallel_then_judge`, `classify_then_route`, `all_primitives` |
| `recipes/` | full workflows: `article_summarizer`, `research_brief`, `code_review`, `meeting_notes`, `comic_strip` |
| `agents/` | orchestrations that build or improve orchestrations: `wizard`, `meta_orchestrator`, `improver`, `improver_judge`, `decompose` |

`cof list` browses it; `cof info recipes/article_summarizer` shows an entry's interface; `cof eject recipes/article_summarizer` copies it into the working directory for editing; `cof run utilities/critique -e content=… -e criteria=…` runs one directly.

Two more source types extend the list. A **`folder`** source scans a directory of your own `*.yml` files — entry names are paths relative to the folder root, categories are its subdirectories, and an optional `manifest.json` adds descriptions and inputs. A **`github`** source serves a subtree of a repository from a SHA-pinned local cache; `cof library refresh <name>` is the only command that ever touches the network, so a run can never stall on a fetch or silently pick up a different version than the one you listed. This is the consumption half of a publish-by-PR shared library.

```json
{
  "runtime": {
    "library": {
      "sources": [
        {"type": "curation"},
        {"type": "folder", "name": "local", "path": "./orchestrations"},
        {"type": "github", "name": "hub", "repo": "owner/name", "ref": "main", "path": "library/", "token_env": "GITHUB_TOKEN"}
      ]
    }
  }
}
```

[Library Sources](../library-sources.md) has the full field table; [Shared Library](../shared-library.md) and its companions cover the contribution workflow and the older `cof fetch` / `cof run-library` retrieval commands.

## Composing the dinner

The utilities are written to chain. A draft menu, critiqued against named criteria, refined on the critique:

```yaml
- type: prompt
  name: draft_menu
  template: "Draft a three-course menu for {{input.occasion}}, one line per course."

- type: use
  name: critique_step
  ref: utilities/critique
  inputs:
    content: "{{{prime.draft_menu.value}}}"
    criteria: "balance, seasonality, brevity"

- type: use
  name: refine_step
  ref: utilities/refine
  inputs:
    content: "{{{prime.draft_menu.value}}}"
    feedback: "{{prime.critique_step.value.critique.issues}}"

- type: prompt
  name: menu_card
  template: "Write the menu card from: {{{prime.refine_step.value.refined}}}"
```

`critique` declares one output, `critique` (an object with `score`, `issues`, `strengths`), so `prime.critique_step.value.critique.issues` is the list of things to fix; `refine` declares `refined`. Neither caller repeats a path into the child, and either utility can be reworked internally without touching this document. `patterns/critique_refine_loop` wraps the same two calls in a `while` loop with the model judging when to stop.

## Anti-patterns

**Passing state by reaching.** A `ref` pointing at a state path, or a child template that expects `{{prime.<parent_effect>.value}}`. The only door is `inputs:`.

**Full-namespace coupling to a library entry.** Declare outputs, or rely on the child's interface.

**A `use` with prompt fields.** `template`, `schema`, `prompt_type` do not belong on a `use`. If the step is a model call, it is a `prompt`.

**Naming the effect `use`.** Name it for what the composed step does — `critique_step`, `sauce`.

**Re-deriving a utility inline.** If a library entry's interface fits, call it; the entry has been tuned and its output shape is stable.

## See also

- [Orchestration Reference → `use`](../orchestration-reference.md#use) and [Outputs](../orchestration-reference.md#outputs).
- [Library Sources](../library-sources.md) · [Shared Library](../shared-library.md).
- [`patterns/parallel_then_judge`](../../src/circuitry/curation/patterns/parallel_then_judge.yml) — a tree of candidates and a `judge` utility choosing one.
