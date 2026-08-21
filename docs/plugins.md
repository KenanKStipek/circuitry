# Plugin Extension Guide

Circuitry exposes stable runtime plugin hooks for adding sidecar capabilities without patching core execution logic.

## Contract Version

- Current plugin contract version: `1`
- Reported in run state at `runtime.plugins.contract_version`

## Hook Interface

A plugin must implement all hooks:

```python
class MyPlugin:
    name = "my-plugin"

    def on_run_start(self, *, state, context):
        ...

    def on_run_success(self, *, state, context):
        ...

    def on_run_failure(self, *, state, context, error):
        ...
```

Context fields:
- `run_id`
- `orchestration_path`
- `dry_run`
- `validate_only`
- `runtime_config`

## Per-effect hooks (optional)

Two further hooks bracket every effect. Both are optional and dispatched
behind the same guard: a plugin may implement either, both, or neither, and
one written before these hooks existed keeps working untouched.

```python
    def on_effect_start(self, *, state, context, effect_path, effect_node):
        ...

    def on_effect_complete(self, *, state, context, effect_path, effect_result):
        ...
```

- `effect_path` is the canonical dotted path of the effect in state
  (`prime.spin.iter_0.tick`) — identical for both halves of a pair.
- `on_effect_start` fires immediately **before** the effect dispatches,
  carrying the effect's state node as it stands at that moment: the resolved
  adapter and model, the rendered prompt, and — when complexity scoring is
  enabled — the score. Observing a decision *at* dispatch (a routed model, a
  complexity band) happens here.
- `on_effect_complete` fires once the node's `value` is finalized.

The pair is balanced: an effect that fires one fires the other, in that
order, including when the effect fails (the completion then carries
`meta.error`) and for effects inside loops and conditional branches, which
nest inside their container's pair. Effects that fire neither are the ones
that never dispatch: a tool in `--dry-run`, and the bodies of `flow: tree`
and parallel loops, whose isolated per-thread stores carry no callbacks.

Embedded callers get the same two events without writing a plugin, via
`RunRequest.effect_start_observer` and `RunRequest.effect_observer` — both
`(effect_path, effect_node)` callables, composed with (not in place of) any
configured plugins.

## Registration

Configure plugin identifiers in `config.json` or orchestration-level `plugins`:

```json
{
  "plugins": [
    "my_package.plugins:make_plugin"
  ]
}
```

Supported identifier forms:
- `module:attr`
- `module` with exported `plugin` symbol

If `attr` is callable, it is invoked as a zero-arg factory.

## Failure Isolation

Plugin load/hook failures are non-fatal by default and recorded at:
- `runtime.plugins.events[*]`

Each event includes:
- `plugin`
- `hook`
- `ok`
- `error`

This isolates extension failures from core runtime determinism while keeping failures observable.

## Compatibility Guidance

- Keep plugins idempotent where possible.
- Avoid mutating execution-critical state paths unless explicitly intended.
- Treat hook signatures and contract version as stable API surface.
- Validate plugin behavior with automated tests against both success and failure runs.
