# Dynamic

Two prompts raise the question of what the second knows about the first. The answer is one state object every effect writes to and reads from — which is the subject of the next chapter. But before state, there is a smaller question: what is *two prompts*? Not a list. A list has no semantics: it does not say whether the second waits for the first, or whether the two can run at once, or what the pair is called when a third effect wants to read it.

The **dynamic** is the answer: composition as a first-class operator. It takes a list of effects and produces one composite effect with a name, a flow, and a place in state. It is the second base monad — `prompt` is `unit`, `dynamic` is `bind` made visible — and it is everywhere in the language whether you write it or not. The root of every orchestration is a dynamic. The body of every loop is a dynamic. Each branch of every conditional is a dynamic. A reflector's inner template is a dynamic. Name a composition and you can reuse it; that is the entire trick.

## The shape

```
Dynamic ::= { type: 'dynamic', name: NAME, effects: Effect+,
              flow?: 'chain' | 'tree',                — default chain
              max_concurrency?: INT≥1,                — tree only
              stop_on_error?: BOOL,
              on_error?: 'fail'|'skip'|'continue',
              labels?: MAP, description?: STRING }
```

`name` and a non-empty `effects` list are required. A dynamic *always* contains effects and *never* decides anything — it is pure structure. That is what separates it from the three cybernetic effects in Part II, which also contain effects but exist to read state and steer.

## Chain

```yaml
- type: dynamic
  name: menu
  flow: chain
  effects:
    - type: prompt
      name: main_course
      template: "Suggest a main course for {{input.occasion}}."
    - type: prompt
      name: wine
      template: "Pick a wine to pair with: {{prime.menu.main_course.value}}"
```

`chain` is sequential: each effect runs after the previous one and sees everything it wrote. This is monadic bind, and it is the shape of chain-of-thought — each step conditioned on the last. It is the default flow, so `flow: chain` can be omitted; writing it is a courtesy to the reader.

Inside the container, the children's paths gain the container's name: `wine` writes to `prime.menu.wine.value`, not `prime.wine.value`. Two spellings reach a sibling from inside the same dynamic — the absolute path, `{{prime.menu.main_course.value}}`, and the container-relative short form, `{{menu.main_course.value}}`. Write the absolute one; it is the same spelling a reader outside the container uses, and it never depends on where the template sits.

What does *not* work is the root spelling. `main_course` lives under `menu`, so `{{prime.main_course.value}}` names a node that does not exist. It does not fail — Mustache renders a missing reference as an empty string — which makes this the first of the silent errors this guidebook will keep pointing at:

```yaml
# ✗ runtime — renders empty: main_course lives at prime.menu.main_course, not prime.main_course
- type: dynamic
  name: menu
  effects:
    - type: prompt
      name: main_course
      template: "Suggest a main course for {{input.occasion}}."
    - type: prompt
      name: wine
      template: "Pick a wine to pair with: {{prime.main_course.value}}"
```

The run goes green, the wine prompt reads "Pick a wine to pair with: ", and the model picks a wine for nothing. `prime.menu.wine.meta.prompt_sent` is where you would catch it.

## Tree

```yaml
- type: dynamic
  name: courses
  flow: tree
  max_concurrency: 3
  effects:
    - type: prompt
      name: starter
      template: "Suggest a starter to precede {{input.main}}."
    - type: prompt
      name: dessert
      template: "Suggest a dessert to follow {{input.main}}."
    - type: prompt
      name: wine
      template: "Pick a wine for {{input.main}}."
```

`tree` is parallel: every child launches against the *same* snapshot of state, taken when the dynamic begins. This is the applicative shape — independent computations over shared input — and it is the shape of tree-of-thought: several branches explored at once, joined afterwards. `max_concurrency` bounds the worker pool; leave it unset and every child runs at once.

The consequence of the shared snapshot is the rule that defines `tree`: **siblings cannot read each other.** `dessert` cannot see `starter`, because when `dessert` was launched `starter` had not written anything yet. A template that tries renders empty, exactly like the wrong-root example above. If one child needs another's output, they are not siblings in a tree — they are steps in a chain.

The two flows compose. A chain whose second step is a tree fans out; a tree whose branches are each a chain runs several pipelines at once; a chain after the tree joins them:

```yaml
- type: dynamic
  name: plan
  flow: chain
  effects:
    - type: prompt
      name: main_course
      template: "Suggest a main course for {{input.occasion}}."
    - type: dynamic
      name: around_it
      flow: tree
      effects:
        - type: prompt
          name: starter
          template: "Suggest a starter to precede {{prime.plan.main_course.value}}."
        - type: prompt
          name: dessert
          template: "Suggest a dessert to follow {{prime.plan.main_course.value}}."
    - type: prompt
      name: menu_card
      template: |
        Write the menu card.
        Starter: {{prime.plan.around_it.starter.value}}
        Main: {{prime.plan.main_course.value}}
        Dessert: {{prime.plan.around_it.dessert.value}}
```

Nesting deepens the path one segment per container: `prime.plan.around_it.starter.value`. The rule is the same at every depth.

## The root is a dynamic

An orchestration's top-level `effects:` list is the body of an implicit root dynamic, and the document's optional top-level `flow:` is that dynamic's flow. A document with `flow: tree` at the top runs all its top-level effects in parallel. The root node is `prime` itself, which is why a top-level prompt writes to `prime.<name>.value` with no container segment — the container is the root.

```yaml
flow: tree
effects:
  - type: prompt
    name: starter
    template: "Suggest a starter for {{input.occasion}}."
  - type: prompt
    name: dessert
    template: "Suggest a dessert for {{input.occasion}}."
```

## Errors inside a container

A dynamic has its own `on_error`, and one field the leaf effects do not: `stop_on_error`. In a tree, `stop_on_error: true` halts the remaining children when one fails; by default the others run to completion and the failures are collected. Per-effect `on_error` covers the same intent one effect at a time and is usually the better tool — see [Errors](05-errors.md).

## What lands in state

```
prime.<dynamic>.value                 # true when the container completed
prime.<dynamic>.meta.flow             # "chain" | "tree"
prime.<dynamic>.meta.error            # null, or the failure (with a breadcrumb into the child)
prime.<dynamic>.<child>.value         # each child, one segment deeper
```

`labels` — free-form key/value metadata — rides along on the node and into traces; it is observability tagging, not behaviour.

## Anti-patterns

**A dynamic that decides.** If you find yourself wanting a dynamic to run *some* of its children depending on state, you want an [`if`](06-if.md). If you want it to run its children *again*, you want a [`loop`](07-loop.md). The dynamic's whole value is that it never surprises you.

**Prescribing a fixed number of children.** Let the problem dictate the decomposition. A "planning" dynamic with exactly three prompts because three felt right is a chain that will be wrong in both directions.

**Tree siblings that read each other.** They render empty — see above. When a branch depends on another branch, restructure: chain first, then tree.

**Forgetting the container in the path.** `{{prime.wine.value}}` for a `wine` inside `menu`. Every path is the full path from `prime`.

## See also

- [Orchestration Reference → `dynamic`](../orchestration-reference.md#dynamic).
- [`learn/dynamic_chain`](../../src/circuitry/curation/learn/dynamic_chain.yml) and [`learn/dynamic_tree`](../../src/circuitry/curation/learn/dynamic_tree.yml).
