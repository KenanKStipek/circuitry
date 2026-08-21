# Named Profiles

A profile is a YAML file that supplies run-level defaults, initial-state
inputs, per-effect model/provider overrides, and the persistence target for a
single `cof run --profile <name>` invocation — without editing the
orchestration itself.

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
    model: cheap
    provider: cyberdiner
  deep_analysis:
    model: good-fast
  my_reflector:
    enabled: false           # do not execute this effect for this run
persistence:                 # where this run's state snapshot lands
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

`CLI` here is `cof run --adapter <name>` / `--model <name>` (also available on
`cof run-library`). Environment variables (`CIRCUITRY_ADAPTER`,
`CIRCUITRY_MODEL`) overlay the *config* layer, so a profile beats them and a
flag beats both.

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

## Disabling Effects

`effects.<path>.enabled: false` switches an effect off for the run. It is not
executed, and its node is written as a skip marker mirroring `on_error: skip`:

```json
{ "value": null, "meta": { "disabled": true, "created_at": "...", "completed_at": "..." } }
```

`on_effect_start` and `on_effect_complete` still fire for that node, so
observability sees the skip.
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

This is `effects.<path>.enabled` — unrelated to the `persistence.enabled`
switch below, which toggles the run's persistence backend.

## Persistence

A profile's `persistence:` block selects and configures the persistence
backend for the run. It feeds the same `build_persistence_backend` path the
project config uses, so the run snapshot lands in the chosen store and a
later run rehydrates from it (`runtime.persistence.loaded_from_persistence`).

```yaml
# profiles/thorough.yml
persistence:
  backend: sqlite
  path: .circuitry/thorough-runs.db   # or db_path:
```

Backends and their keys:

| `backend`    | Required            | Optional                                       | Dependency |
| ------------ | ------------------- | ---------------------------------------------- | ---------- |
| `jsonl-file` | `path`              | —                                              | stdlib     |
| `sqlite`     | `path` / `db_path`  | `table` (default `circuitry_runs`)             | stdlib     |
| `mongodb`    | `uri`               | `database` (`circuitry`), `collection` (`circuitry_runs`) | `pip install circuitry-cof[mongodb]` |
| `postgres`   | `dsn`               | `table`, `sslmode`, `allow_insecure`           | `pip install psycopg[binary]` |

`jsonl-file` appends one record per run to a local file and reads the last
record for the orchestration back on load — no server required, and the log
stays greppable. It is independent of the write-only `jsonl-file` *runtime
plugin* (an event stream); both can be enabled at once.

Precedence:

```
profile persistence > orchestration runtime.persistence > project config > global config
```

The profile block **replaces** rather than merges with a lower-priority
block — backends take disjoint keys, so a partial overlay would produce a
chimera. `runtime.effective_settings.sources.persistence` records which layer
won (`profile` / `orchestration` / `config`). There is no CLI persistence
flag today; if one is added, it wins over the profile.

`enabled` defaults to **true** for a profile-supplied block — naming a
backend in a profile is the opt-in. Set `enabled: false` to run without
persistence even when the project config configures a backend. A profile
with no `persistence:` block changes nothing: the config/orchestration
backend (or no persistence at all) applies exactly as it does today.

`--out` is orthogonal and combinable — it still writes the final state to the
given file regardless of which backend persisted the snapshot.

Credential-bearing values (a Mongo URI's `user:pass@`, `password`-style keys)
are redacted everywhere state is serialized: `runtime.persistence`,
`runtime.effective_settings.runtime`, and
`runtime.effective_settings.profile.content`. The un-redacted value only
reaches the driver. Prefer pointing at env-var-supplied credentials over
writing them into a profile file — see [`docs/threat-model.md`](threat-model.md).

A backend that can't be reached fails the run with the same actionable error
the runtime-config path produces (`Failed to load persisted state: MongoDB
state load failed for orchestration ...`), and records
`runtime.persistence.status = "load_failed"` / `"save_failed"`.

When persistence hydrates state from a previous run, profile `inputs` remain
the lowest layer: they fill keys the persisted snapshot doesn't carry rather
than overwriting resumed values.

## Usage

```bash
cof run recipe --profile fast
```

Or edit one without touching the YAML: `cof tui`, then `9` for the profile
editor — the effect tree with a picker and a toggle per row, panels for the
run defaults, inputs and persistence, and save/duplicate/switch across named
profiles. See [Profiles (`9`)](tui.md#profiles-9--edit-a-named-profile).

## Runtime Metadata

When a profile is applied, run state includes:

```
runtime.effective_settings.profile.name
runtime.effective_settings.profile.content       # the full parsed profile, redacted
runtime.effective_settings.sources.model         # "profile" when the profile supplied it
runtime.effective_settings.sources.adapter       # "profile" when the profile supplied it
runtime.effective_settings.sources.persistence   # "profile" | "orchestration" | "config"
runtime.persistence.backend                      # selected backend + its describe() fields
runtime.persistence.loaded_from_persistence      # true when state was rehydrated
```

Credential-shaped values inside the profile are redacted the same way
`runtime.effective_settings.runtime` is (see `circuitry.cli.redaction`).
