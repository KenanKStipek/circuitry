# Named Profiles

A profile is a YAML file that supplies run-level defaults, initial-state
inputs, and per-effect model/provider overrides for a single `cof run
--profile <name>` invocation — without editing the orchestration itself.

## File Layout

```
profiles/<name>.yml
```

Discovery order (first match wins):

1. `<orchestration_dir>/profiles/<name>.yml` — orchestration-scoped
2. `<cwd>/profiles/<name>.yml` — project-level

`.yml` and `.yaml` are both accepted. Orchestration-scoped profiles win over
project-level ones with the same name.

## Format

```yaml
# profiles/fast.yml
adapter: ollama            # optional run-level default adapter
model: llama3.2             # optional run-level default model
inputs:
  topic: "circuit design"   # merged into the initial state (CLI -e wins)
effects:                    # keyed by dotted effect path, as in state
  summarize:
    model: tier-1
    provider: cyberdiner
  deep_analysis:
    model: tier-4
  my_reflector:
    enabled: false           # do not execute this effect for this run
persistence:                 # parsed/validated here; backend selection behavior TBD
  backend: jsonl-file
  path: runs.jsonl
```

Validated against
[`src/circuitry/schema/profile.schema.json`](../src/circuitry/schema/profile.schema.json)
(JSON Schema Draft-07). Unknown top-level or per-effect keys fail with an
actionable schema error. Effect paths under `effects:` are additionally
checked against the target orchestration — an unknown path fails with the
list of valid paths for that orchestration.

Effect paths follow the same dotted convention as runtime state paths
(`prime.<effect>.value`, minus the `prime.` root) — e.g. `summarize`, or
`my_dynamic.child_effect` for an effect nested inside a named `dynamic`
container. Anonymous (unnamed) `conditional`/`loop` wrappers are transparent
and contribute no path segment.

## Precedence

`model` and `adapter` resolve as:

```
CLI > profile > orchestration > project config > global config > default
```

(Project config already layers over global config before this — see
`resolve_config` in `circuitry.cli.config`.) A run with no `--profile` is
unaffected by this feature — profile resolution is skipped entirely.

`inputs` are merged into the initial run state as a base layer; `--state`
and `-e` values always win over profile inputs.

## Per-Effect Overrides

`effects.<path>.model` / `effects.<path>.provider` are applied as a
compile-time overlay: after the orchestration compiles to its `*Definition`
tree, matching nodes are rebuilt via `dataclasses.replace` (the tree stays
frozen/immutable — a new tree is returned, nothing is mutated in place).
Overrides targeting an effect type that has no `model`/`provider` field
(e.g. `use`) are a no-op for that field.

`persistence` is parsed and validated but backend selection is implemented by
a sibling task.

## Disabling Effects

`effects.<path>.enabled: false` switches an effect off for the run. It is not
executed, and its node is written as a skip marker mirroring `on_error: skip`:

```json
{ "value": null, "meta": { "disabled": true, "created_at": "...", "completed_at": "..." } }
```

`on_effect_complete` still fires for that node, so observability sees the skip.
Disabling a container (`dynamic`/`loop`/`conditional`/`reflector`) disables its
whole subtree.

The flagship use is turning agentic planning off for a single run:

```yaml
# profiles/no-planning.yml
effects:
  my_reflector:
    enabled: false
```

```bash
cof run recipe                          # reflector plans and executes
cof run recipe --profile no-planning    # reflector is skipped; the rest is unchanged
```

A conditional's `if` and a loop's `while` are conditions, not effects — they
cannot be disabled, and targeting `<name>.if` / `<name>.while` /
`<name>.condition` fails validation with an error naming the container to
disable instead. See
[Disabling Effects](orchestration-reference.md#disabling-effects) in the
orchestration reference for the full rule table and downstream (template/CEL)
behavior.

## Usage

```bash
cof run recipe --profile fast
```

## Runtime Metadata

When a profile is applied, run state includes:

```
runtime.effective_settings.profile.name
runtime.effective_settings.profile.content   # the full parsed profile, redacted
runtime.effective_settings.sources.model     # "profile" when the profile supplied it
runtime.effective_settings.sources.adapter   # "profile" when the profile supplied it
```

Credential-shaped values inside the profile are redacted the same way
`runtime.effective_settings.runtime` is (see `circuitry.cli.redaction`).
