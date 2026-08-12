# Stability & Versioning Policy

This document defines what is and is not part of Circuitry's public API
contract, and the versioning rules that apply to each. Read this before
depending on Circuitry from another package or designing tools that
introspect Circuitry internals.

## TL;DR

- Circuitry follows [Semantic Versioning](https://semver.org/) starting at
  `0.1.0`.
- During the `0.x` series (alpha), **minor** bumps may include breaking
  changes, called out in the [`CHANGELOG`](../CHANGELOG.md).
- Once `1.0.0` ships, breaking changes go in **major** bumps and get one
  minor release of deprecation warning beforehand.
- Anything in this document under "Public surface" is covered by these
  rules. Everything else is internal and may change at any time.

---

## Public surface

### 1. Python re-exports

The public Python API is exactly the set of names re-exported from these
three modules. Importing from anywhere else inside the `circuitry`
package counts as using internal API.

#### `circuitry`

From [`src/circuitry/__init__.py`](../src/circuitry/__init__.py):

- `run_orchestration`
- `run_shared_orchestration`
- `validate_orchestration`
- `inspect_orchestration`
- `inspect_divergence_paths`
- `CircuitryExecutionError`

#### `circuitry.adapters`

From [`src/circuitry/adapters/__init__.py`](../src/circuitry/adapters/__init__.py):

- `Adapter` (Protocol)
- `GenerateResult`
- `build_adapter`
- `OllamaAdapter`
- `OpenAIAdapter`
- `AnthropicAdapter`
- `LiteLLMAdapter`
- `validate_generate_result`

#### `circuitry.plugins`

From [`src/circuitry/plugins/__init__.py`](../src/circuitry/plugins/__init__.py):

- `ToolPlugin` (Protocol)
- `ToolResult`
- `validate_tool_result`
- `build_plugin`

### 2. The `cof` CLI

Every subcommand and every flag that appears in the [`README.md` "CLI
Reference"](../README.md#cli-reference) section is covered. The contract
covers:

- The subcommand name (`run`, `check`, `validate`, `inspect`, `list`,
  `info`, `eject`, `gen`, `setup`, `doctor`, `init`, `version`, `fetch`,
  `run-library`).
- Each documented flag, its short form, and the basic semantics described
  in `--help` text.
- The exit codes (`0` for success, non-zero for failure).
- The auto-pipe detection behavior (`--json` + quiet when stdout is not a
  TTY).
- The replay contract for `--last`.

What is **not** covered:

- Exact wording of console output, progress messages, or error strings —
  these can change to improve clarity.
- The shape of internal log lines (`logging.getLogger("circuitry")`).
- Color, markup, or panel layout (driven by Rich, may evolve).

### 3. Orchestration JSON Schema

The schema at
[`src/circuitry/schema/orchestration.schema.json`](../src/circuitry/schema/orchestration.schema.json)
is the canonical contract for orchestration YAML. Backwards-compatible
schema changes (new optional fields, relaxed constraints) are not breaking;
new required fields, removed types, or tightened constraints are.

### 3a. Profile JSON Schema

The schema at
[`src/circuitry/schema/profile.schema.json`](../src/circuitry/schema/profile.schema.json)
is the canonical contract for named profile files (`profiles/<name>.yml`,
see [Named Profiles](./profiles.md)). Same backwards-compatibility rules as
the orchestration schema above.

### 4. State path conventions

The deterministic state-path layout is part of the contract because
orchestrations interpolate against it. The covered shapes are:

```
prime.<effect>.value
prime.<effect>.meta.*
prime.<dynamic>.<effect>.value
prime.<loop>.iter_<n>.<effect>.value
prime.<loop>.collected.value
runtime.last_run.*
runtime.effective_settings.*
runtime.plugins.*
```

Built-in template variables available inside loop bodies:

- `{{_loop_index}}` — zero-based iteration index (both `each` and `while`)
- `{{<each.as>}}` — current collection element (`each` loops only)

Top-level template variables:

- `{{_run_id}}` — UUID for the current run
- `{{_timestamp}}` — UTC timestamp at run start (`YYYYMMDD_HHMMSS`)

---

## Internal surface

These modules are implementation detail. Direct imports work today, but
they may move, change signature, or disappear without a deprecation cycle.

- `circuitry.core.*` — compiler, runtime, store, persistence backends,
  effect runtimes. Use the API in `circuitry` (top-level) instead.
- `circuitry.cli.*` — CLI wiring, config loaders, runtime shim. The `cof`
  CLI is public; importing the implementing functions in Python is not.
- `circuitry.bundled.*` — packaged bundled assets. The packaged YAML files
  are stable shapes; the code that loads them is internal.
- `circuitry.rules.*` — author rules used by the gen workflow. Internal.
- `circuitry.schema.*` — schema-loading code. The JSON Schema file at
  `src/circuitry/schema/orchestration.schema.json` is public; the loading
  code is internal.
- `circuitry.service.*` — shared-library service plumbing. Internal.
- `circuitry.cli.redaction` — defense-in-depth redaction helper. Internal.
- Any module name starting with an underscore.

---

## Versioning policy

### Pre-1.0 (the `0.x` series)

Circuitry uses semver from `0.1.0` upward, with the following alpha
caveats:

- **Patch bumps** (`0.1.0 → 0.1.1`) are bug-fix only. They may include
  documentation, dependency updates, and internal refactors that have no
  public-API effect.
- **Minor bumps** (`0.1.0 → 0.2.0`) may include breaking changes to the
  public surface during `0.x`. Every breaking change is listed in the
  `CHANGELOG.md` for that release with a migration note. Where possible,
  we ship a deprecation warning in the prior minor release.
- **Major bump to `1.0.0`** requires no breaking changes for one full
  minor cycle and an explicit signal that the surface is frozen.

### Post-1.0

After `1.0.0`:

- Patch and minor bumps are backwards-compatible.
- Breaking changes go in **major** bumps and require:
  1. A `DeprecationWarning` in at least one minor release before removal.
  2. A migration note in the `CHANGELOG.md`.
  3. A grace period of one full minor release minimum.

---

## Deprecation policy

When something on the public surface needs to go away:

1. Mark it with a `DeprecationWarning` (Python) or a `[deprecated]` tag in
   `--help` (CLI). Reference the replacement.
2. Document the deprecation in `CHANGELOG.md` under the next release's
   `Deprecated` section.
3. Keep the deprecated thing functional for at least one minor release
   (or until the next major bump for the post-1.0 era).
4. Remove with a `BREAKING CHANGE:` footer in the commit and a migration
   note in `CHANGELOG.md`.

---

## Adapter SDK versions

Circuitry depends on adapter SDKs (`ollama`, optionally `openai`,
`anthropic`, `litellm`) with **lower-bound** version pins only. Upper
bounds are not pinned; CI tests against the latest released versions.
This means:

- A breaking change in an upstream SDK can break Circuitry for users on
  the latest version of that SDK without a Circuitry release.
- We ship a fix as soon as the breakage is reported, generally as a patch
  bump.
- Users who need lockstep stability should pin both Circuitry and the
  adapter SDK in their own environment.

---

## What you can rely on

If you are building on Circuitry:

- **Yes**: import from `circuitry`, `circuitry.adapters`, and
  `circuitry.plugins`. Build orchestrations against the JSON Schema. Read
  state at the documented paths. Use the `cof` CLI as documented.
- **Cautiously**: depend on bundled orchestrations by name (they are
  stable, but may move between categories or be renamed across major
  bumps).
- **No**: import from `circuitry.core`, `circuitry.cli`,
  `circuitry.service`, or any underscore-prefixed module. Do not parse
  console output. Do not depend on internal log line wording.

---

## Reporting a stability issue

If we shipped a patch release that broke something on the public surface,
that is a bug — please open a [bug report](https://github.com/kenankstipek/circuitry/issues/new?template=bug_report.md)
referencing this document and the version that broke. Security issues go
through [`SECURITY.md`](../SECURITY.md) instead.
