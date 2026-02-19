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
