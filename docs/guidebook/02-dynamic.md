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
  name: investigate
  flow: chain
  effects:
    - type: prompt
      name: error
      template: "Copy the error message out of this issue, without the values in it. Reply with the message only. Issue: {{input.issue}}"
    - type: prompt
      name: cause
      template: "Name the most likely cause of this error, in one sentence: {{prime.investigate.error.value}}"
```

`chain` is sequential: each effect runs after the previous one and sees everything it wrote. This is monadic bind, and it is the shape of chain-of-thought — each step conditioned on the last. It is the default flow, so `flow: chain` can be omitted; writing it is a courtesy to the reader. A chain always runs one child at a time, by construction — `max_concurrency` is a tree-flow setting and has no meaning here.

Inside the container, the children's paths gain the container's name: `cause` writes to `prime.investigate.cause.value`, not `prime.cause.value`. Two spellings reach a sibling from inside the same dynamic — the absolute path, `{{prime.investigate.error.value}}`, and the container-relative short form, `{{investigate.error.value}}`. Write the absolute one; it is the same spelling a reader outside the container uses, and it never depends on where the template sits.

What does *not* work is the root spelling. `error` lives under `investigate`, so `{{prime.error.value}}` names a node that does not exist. It does not fail — Mustache renders a missing reference as an empty string — which makes this the first of the silent errors this guidebook will keep pointing at:

```yaml
# ✗ runtime — renders empty: error lives at prime.investigate.error, not prime.error
- type: dynamic
  name: investigate
  effects:
    - type: prompt
      name: error
      template: "Copy the error message out of this issue, without the values in it. Reply with the message only. Issue: {{input.issue}}"
    - type: prompt
      name: cause
      template: "Name the most likely cause of this error, in one sentence: {{prime.error.value}}"
```

The run goes green, the cause prompt reads "Name the most likely cause of this error, in one sentence: ", and the model names a cause for no error at all. `prime.investigate.cause.meta.prompt_sent` is where you would catch it.

## Tree

Before an agent changes anything it looks around: it searches the code for the error message, reads the recent history, and checks for related issues. These are `tool` effects: a tool calls a plugin — here `ripgrep`, `git` and `gh` — instead of a model, and composes like any other leaf ([Tools and persistence](13-tools-and-persistence.md) covers them). And none of the three needs the others:

```yaml
- type: dynamic
  name: context
  flow: tree
  max_concurrency: 3
  effects:
    - type: tool
      name: search
      provider: ripgrep
      params: {args: [--line-number, --fixed-strings, "{{input.error}}", "."], cwd: "{{input.repo}}"}
    - type: tool
      name: history
      provider: git
      params: {args: [log, --oneline, "-5"], cwd: "{{input.repo}}"}
    - type: tool
      name: related
      provider: gh
      params: {args: [issue, list, --search, "{{input.error}}"], cwd: "{{input.repo}}"}
```

`tree` is parallel: every child launches against the *same* snapshot of state, taken when the dynamic begins. This is the applicative shape — independent computations over shared input — and it is the shape of tree-of-thought: several branches explored at once, joined afterwards. `max_concurrency` bounds the worker pool; leave it unset and every child runs at once. Each child reports `on_effect_start`/`on_effect_complete` to the TUI, runtime plugins, and `cof run --events <file>` as it dispatches and lands — `--events` is what shows a fan-out's progress while the slow branches are still running. `--live-state` only ever shows a branch once it has finished; a still-running one has no node there yet.

The consequence of the shared snapshot is the rule that defines `tree`: **siblings cannot read each other.** `history` cannot see `search`, because when `history` was launched `search` had not written anything yet. A template that tries renders empty, exactly like the wrong-root example above. If the history should cover the files the search found, the two are not siblings in a tree — they are steps in a chain.

The two flows compose. A chain whose second step is a tree fans out; a tree whose branches are each a chain runs several pipelines at once; a chain after the tree joins them:

```yaml
- type: dynamic
  name: gather
  flow: chain
  effects:
    - type: prompt
      name: error
      template: "Copy the error message out of this issue, without the values in it. Reply with the message only. Issue: {{input.issue}}"
    - type: dynamic
      name: context
      flow: tree
      effects:
        - type: tool
          name: search
          provider: ripgrep
          params: {args: [--line-number, --fixed-strings, "{{prime.gather.error.value}}", "."], cwd: "{{input.repo}}"}
        - type: tool
          name: related
          provider: gh
          params: {args: [issue, list, --search, "{{prime.gather.error.value}}"], cwd: "{{input.repo}}"}
    - type: prompt
      name: brief
      template: |
        Write a short brief for the engineer who will fix this issue.
        Error: {{prime.gather.error.value}}
        Where it is raised: {{{prime.gather.context.search.value}}}
        Related issues: {{{prime.gather.context.related.value}}}
```

Nesting deepens the path one segment per container: `prime.gather.context.search.value`. The rule is the same at every depth.

## The root is a dynamic

An orchestration's top-level `effects:` list is the body of an implicit root dynamic, and the document's optional top-level `flow:` is that dynamic's flow. A document with `flow: tree` at the top runs all its top-level effects in parallel. The root node is `prime` itself, which is why a top-level prompt writes to `prime.<name>.value` with no container segment — the container is the root.

```yaml
flow: tree
effects:
  - type: prompt
    name: kind
    template: "Is this issue a bug, a feature or a question? Answer with the one word, nothing else. Issue: {{input.issue}}"
  - type: prompt
    name: title
    template: "Write a short, specific title for this issue. Reply with the title only. Issue: {{input.issue}}"
```

## Errors inside a container

A dynamic has its own `on_error`, with the same three values and the same meaning a leaf effect's does (see [Errors](05-errors.md)): a child's failure that nothing inside absorbed makes this dynamic itself fail, and `on_error` decides whether that propagates to *this dynamic's own* parent (`fail`, the default) or is recorded and swallowed there, letting the parent carry on to its next effect (`skip`/`continue` — the two are the same degradation for a dynamic, exactly as they are for a leaf effect).

`stop_on_error` is the one field the leaf effects do not have, and it is tree-only: by default a tree runs every child to completion and collects whichever failed; `stop_on_error: true` instead cancels every child that has not started yet as soon as one fails. A child already running when that happens cannot be stopped — no thread can be killed from outside — so `stop_on_error` only cuts work that `max_concurrency` was holding back from starting. Any child that was already running when the cancellation happened still finishes, but its own failure (if it has one) is not added to the dynamic's own `meta.error`, and a cancelled child leaves no node and fires no hooks at all — only the triggering failure is recorded. In a chain, a failing child already stops the ones after it (it is sequential), so `stop_on_error` does nothing there. Per-effect `on_error` on the children themselves covers the same intent one effect at a time and is usually the better tool.

A child blocked acquiring a `runtime.max_concurrency`/`concurrency_groups` slot (see [Configuration](04-configuration.md)) is, from this container's own point of view, already started — the pool has dispatched it, it just hasn't passed the run-wide gate yet — so `stop_on_error` cannot cancel it either, the same as a child already running. After the triggering failure, every other child still queued on that same cap or group keeps waiting its turn and runs, one at a time, exactly as it would have without the failure.

## What lands in the shadow state

```
prime.<dynamic>.value                 # true when the container completed
prime.<dynamic>.meta.flow             # "chain" | "tree"
prime.<dynamic>.meta.error            # null, or the failure, naming the failing child's path in both flows
prime.<dynamic>.<child>.value         # each child, one segment deeper
```

`labels` — free-form key/value metadata — rides along on the node and into traces; it is observability tagging, not behaviour.

## Anti-patterns

**A dynamic that decides.** If you find yourself wanting a dynamic to run *some* of its children depending on state, you want an [`if`](06-if.md). If you want it to run its children *again*, you want a [`loop`](07-loop.md). The dynamic's whole value is that it never surprises you.

**Prescribing a fixed number of children.** Let the problem dictate the decomposition. A "planning" dynamic with exactly three prompts because three felt right is a chain that will be wrong in both directions.

**Tree siblings that read each other.** They render empty — see above. When a branch depends on another branch, restructure: chain first, then tree.

**Forgetting the container in the path.** `{{prime.error.value}}` for an `error` inside `investigate`. Every path is the full path from `prime`.

## See also

- [Orchestration Reference → `dynamic`](../orchestration-reference.md#dynamic).
- [`learn/dynamic_chain`](../../src/circuitry/curation/learn/dynamic_chain.yml) and [`learn/dynamic_tree`](../../src/circuitry/curation/learn/dynamic_tree.yml).
