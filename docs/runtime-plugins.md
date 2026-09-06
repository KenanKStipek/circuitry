# Runtime Plugin Catalog

This catalogs the bundled `circuitry.runtime_plugins.*` sidecars that
persist run telemetry. See [`plugins.md`](plugins.md) for the hook
contract itself (`on_run_start` / `on_effect_start` / `on_effect_complete`
/ `on_run_success` / `on_run_failure`, registration, failure isolation).

## `surrealdb`

Persists each run as one `runs` row plus one `effects` row per effect,
mirroring the SQL B-prime schema (`runs` + `effect_results` in
`_sql_schema.py`) but written through SurrealQL via the official
`surrealdb` SDK rather than a DBAPI cursor — the same driver-model split
as the bundled `clickhouse` plugin.

Enable it via `enabled_plugins`:

```json
{
  "plugins": ["circuitry.runtime_plugins.surrealdb"],
  "enabled_plugins": ["circuitry.runtime_plugins.surrealdb"]
}
```

Install the optional dependency: `pip install circuitry-cof[surrealdb]`.

### Schema

`runs` — one row per run:

| field | type | notes |
| --- | --- | --- |
| `run_id` | string | |
| `orchestration_path` | string | |
| `status` | string | `running` → `success` \| `failed` |
| `started_at` | string (ISO 8601) | |
| `ended_at` | string (ISO 8601) \| null | set on completion |
| `error` | string \| null | |
| `inputs` | object | captured `{{input.*}}` values, stored as a native object (not JSON text) |

`effects` — one row per effect result:

| field | type | notes |
| --- | --- | --- |
| `run_id` | string | foreign key to `runs.run_id` |
| `state_path` | string | canonical dotted path, e.g. `prime.my_loop.iter_3.handle` |
| `effect_name` | string | last path segment |
| `effect_type` | string | `prompt` \| `tool` \| `dynamic` \| `loop` \| `effect` |
| `parent_path` | string \| null | |
| `iteration_index` | int \| null | set inside loop bodies |
| `value` | any | native object/array/scalar |
| `raw` | any \| null | full provider response; redacted in prod (see below) |
| `tokens_sent` / `tokens_received` | int \| null | |
| `started_at` / `ended_at` | string (ISO 8601) | |
| `status` | string | `success` \| `failed` |
| `error` | string \| null | |

Both tables are `DEFINE TABLE ... SCHEMALESS` (bootstrapped idempotently
on every `on_run_start`) plus a unique index on `runs.run_id` and a
lookup index on `effects.run_id`.

Unlike the SQL dialects, `inputs` / `value` / `raw` are stored as native
SurrealDB objects rather than JSON-encoded text, and `effects` has no
managed `id` column — SurrealDB assigns its own record id per `CREATE`.

### Prod redaction

`environment: prod` (`CIRCUITRY_ENV=prod` or `CIRCUITRY_ENVIRONMENT=prod`)
omits `effects.raw` — the same `store_raw` cascade as the SQL plugins:

1. Env var `CIRCUITRY_SURREALDB_STORE_RAW` (truthy: `1`, `true`, `yes`, `y`, `on`).
2. `runtime.runtime_plugins.surrealdb.store_raw` in config.json.
3. Environment default: `true` in `dev`, `false` otherwise.

### Connection / auth

Config sources (env wins, then `runtime.runtime_plugins.surrealdb.*` in
config.json, then the default):

| setting | env var | config key | default |
| --- | --- | --- | --- |
| URL | `SURREAL_URL` | `url` | `ws://localhost:8000/rpc` |
| Namespace | `SURREAL_NAMESPACE` | `namespace` | `circuitry` |
| Database | `SURREAL_DATABASE` | `database` | `circuitry` |
| Token | `SURREAL_TOKEN` | `token` | — |
| User | `SURREAL_USER` | `user` | — |
| Password | `SURREAL_PASS` | `password` | — |

When a token is set it takes priority (`authenticate(token)`); otherwise
`user` + `password` sign in via `signin(...)`. Neither set means an
unauthenticated connection (fine for a local dev instance with auth
disabled). These are the same conventions the `circuitry.plugins.surrealdb`
tool plugin uses.

### Demo

```
cof run learn/hello
```

then, in `surreal sql` or via the SDK:

```sql
SELECT * FROM runs ORDER BY started_at DESC LIMIT 1;
SELECT * FROM effects WHERE run_id = $run_id;
```
