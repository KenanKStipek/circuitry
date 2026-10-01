# Runtime Plugin Catalog

This catalogs every bundled `circuitry.runtime_plugins.*` sidecar that
persists or forwards run telemetry. See [`plugins.md`](plugins.md) for the
hook contract itself (`on_run_start` / `on_effect_start` / `on_effect_complete`
/ `on_run_success` / `on_run_failure`, registration, failure isolation).

Enable any of them via `plugins` + `enabled_plugins` in config.json (or an
orchestration's own `runtime:` block, subject to the trust rules in
[orchestration-reference.md](orchestration-reference.md#file-structure)):

```json
{
  "plugins": ["circuitry.runtime_plugins.sqlite"],
  "enabled_plugins": ["circuitry.runtime_plugins.sqlite"]
}
```

Most need an optional dependency; install it with the plugin's own extra,
e.g. `pip install circuitry-cof[postgres]`. A missing dependency is a
`library:<dep>` preflight failure naming the install command, the same as
any other optional plugin. Two plugins — `jsonl-file` and `sqlite` — need
nothing beyond the standard library and are the easiest way to try
persistence without installing anything.

## Zero-dependency plugins

### `jsonl-file`

Appends one JSON line per lifecycle event (`run_start`, `effect_complete`,
`run_success`, `run_failure`) to a local file — an event stream, not a
latest-state snapshot, which makes it well suited to `grep`/`jq` and CI
logs. Stdlib only.

| setting | env var | config key (`runtime.runtime_plugins.jsonl-file.*`) | default |
| --- | --- | --- | --- |
| Output path | `CIRCUITRY_JSONL_PATH` | `path` | `./circuitry-runs.jsonl` |
| Include per-effect lines | `CIRCUITRY_JSONL_INCLUDE_EFFECTS` | `include_effects` | `true` |

### `sqlite`

Persists each run as a `runs` row plus one `effect_results` row per effect
— see [SQL family](#sql-family-postgres-mysql-mssql-cockroachdb-duckdb-clickhouse-sqlite)
below for the shared schema. Stdlib `sqlite3` only. Suitable for
single-machine development; multi-process workloads should reach for
postgres or mysql.

| setting | env var | config key (`runtime.runtime_plugins.sqlite.*`) | default |
| --- | --- | --- | --- |
| Database file | `CIRCUITRY_SQLITE_PATH` | `db_path` | `./circuitry-runs.db` |
| Store raw provider response | `CIRCUITRY_SQLITE_STORE_RAW` | `store_raw` | `true` in `CIRCUITRY_ENV=dev`, else `false` |

## SQL family (`postgres`, `mysql`, `mssql`, `cockroachdb`, `duckdb`, `clickhouse`, `sqlite`)

These seven plugins (plus `sqlite` above) all persist the same **B-prime**
shape — a `runs` row per run and one `effect_results` row per effect —
defined once in `_sql_schema.py`. Six of them (`postgres`, `mysql`, `mssql`,
`cockroachdb`, `duckdb`, `sqlite`) extend the shared `SqlPersistenceBase`
and talk to the database through a DBAPI `cursor.execute()`; `clickhouse`
implements the same schema against `clickhouse-connect`'s `Client`
interface instead, since ClickHouse has no DBAPI cursor semantics.
`cockroachdb` is wire-compatible with PostgreSQL and reuses the Postgres
plugin's behaviour with its own dialect.

`runs` — one row per run: `run_id`, `orchestration_path`, `status`
(`running` → `success`/`failed`), `started_at`/`ended_at`, `error`,
`inputs` (captured `{{input.*}}` values, JSON-encoded).

`effect_results` — one row per effect: `run_id`, `state_path`,
`effect_name`, `effect_type` (`prompt`/`tool`/`dynamic`/`loop`/`effect`),
`parent_path`, `iteration_index`, `value`, `raw` (full provider response;
redacted in prod — see [Prod redaction](#prod-redaction-sql-and-surrealdb)
below), `tokens_sent`/`tokens_received`, `started_at`/`ended_at`, `status`,
`error`.

| plugin | extra | connection | config keys |
| --- | --- | --- | --- |
| `postgres` | `psycopg2-binary` (`[postgres]`) | `DATABASE_URL` or `CIRCUITRY_POSTGRES_DSN`, else standard libpq vars (`PGHOST`/`PGUSER`/`PGPASSWORD`/`PGDATABASE`/`PGPORT`) | `runtime.runtime_plugins.postgres.dsn` |
| `mysql` | `mysql-connector-python` (`[mysql]`) | `MYSQL_URL`, else `MYSQL_HOST`/`MYSQL_USER`/`MYSQL_PASSWORD`/`MYSQL_DATABASE`/`MYSQL_PORT` | `runtime.runtime_plugins.mysql.{dsn,host,user,password,database,port}` |
| `mssql` | `pyodbc` + an ODBC driver (`[mssql]`) | `MSSQL_URL` (full ODBC connection string) | `runtime.runtime_plugins.mssql.connection_string` |
| `cockroachdb` | `psycopg2-binary` (`[cockroachdb]`) | `CRDB_URL`, else `DATABASE_URL` | `runtime.runtime_plugins.cockroachdb.dsn` |
| `duckdb` | `duckdb` (`[duckdb]`) | — (embedded, single-file) | `CIRCUITRY_DUCKDB_PATH` / `runtime.runtime_plugins.duckdb.db_path` (default `./circuitry-runs.duckdb`) |
| `clickhouse` | `clickhouse-connect` (`[clickhouse]`) | `CLICKHOUSE_URL`, else `CLICKHOUSE_HOST`+`CLICKHOUSE_USER`+`CLICKHOUSE_PASSWORD`+`CLICKHOUSE_DATABASE`+`CLICKHOUSE_PORT` | `runtime.runtime_plugins.clickhouse.{url,host,...}` |
| `sqlite` | stdlib | — (local file) | see [above](#sqlite) |

Each of `postgres`/`mysql`/`mssql`/`cockroachdb`/`sqlite` also reads a
`store_raw` setting (env `CIRCUITRY_<NAME>_STORE_RAW`, or
`runtime.runtime_plugins.<name>.store_raw`) with the same
[prod-redaction](#prod-redaction-sql-and-surrealdb) default as `sqlite`.

## `surrealdb`

Persists each run as one `runs` row plus one `effects` row per effect,
mirroring the SQL B-prime schema above but written through SurrealQL via
the official `surrealdb` SDK rather than a DBAPI cursor — the same
driver-model split as `clickhouse`. It shares the `surrealdb` extra
(`pip install circuitry-cof[surrealdb]`) with the `circuitry.plugins.surrealdb`
*tool* plugin.

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

### Prod redaction (SQL and SurrealDB)

`environment: prod` omits the raw-response column/field (`effect_results.raw`
for the SQL family, `effects.raw` for SurrealDB):

1. Env var `CIRCUITRY_<NAME>_STORE_RAW` (truthy: `1`, `true`, `yes`, `y`, `on`),
   e.g. `CIRCUITRY_SURREALDB_STORE_RAW`, `CIRCUITRY_SQLITE_STORE_RAW`.
2. `runtime.runtime_plugins.<name>.store_raw` in config.json.
3. Environment default: `true` in `dev`, `false` otherwise — the environment
   itself is `CIRCUITRY_ENV`/`CIRCUITRY_ENVIRONMENT`, then `"environment"` in
   config.json (`CircuitryConfig.environment`), then `"dev"`. Either env var,
   when set, wins over config.json's `environment` key.

The raw field is the tool plugin's own `ToolResult.raw` for a tool effect —
redacted and size-capped the same way as `meta.raw` in state (see
[Errors](guidebook/05-errors.md) /
[orchestration-reference.md](orchestration-reference.md#tool)). It is
always absent for a prompt effect.

### Demo

```
cof run learn/hello
```

then, in `surreal sql` or via the SDK:

```sql
SELECT * FROM runs ORDER BY started_at DESC LIMIT 1;
SELECT * FROM effects WHERE run_id = $run_id;
```

## Document / KV / object-storage family

These eleven plugins all extend `SnapshotPersistenceBase`: each writes (or
overwrites) one whole-state JSON snapshot per run — keyed, indexed, or
named by `run_id` — rather than a row per effect. There is no cross-run
query surface built in; each store's own tooling (a `SELECT`, a `scan`, a
bucket listing) is how you look at more than one run at a time.

| plugin | extra | storage shape | required config | optional config |
| --- | --- | --- | --- | --- |
| `mongodb` | `pymongo` (`[mongodb]`) | one document, `_id = run_id`, upserted via `replace_one` | — (defaults below) | `MONGODB_URI` (default `mongodb://localhost:27017`), `MONGODB_DATABASE` (default `circuitry`), `MONGODB_COLLECTION` (default `runs`) |
| `couchdb` | `requests` (`[couchdb]`) | one document at `<db>/<run_id>`, `_rev`-tracked (MVCC) | `COUCHDB_URL`, or `COUCHDB_HOST`+`COUCHDB_USER`+`COUCHDB_PASSWORD`+`COUCHDB_DATABASE` | `runtime.runtime_plugins.couchdb.{url,host,...}` |
| `firestore` | `google-cloud-firestore` (`[firestore]`) | one document at `<collection>/<run_id>` | — (defaults below) | `FIRESTORE_COLLECTION` (default `runs`), `FIRESTORE_PROJECT`, `FIRESTORE_DATABASE` (default `(default)`) |
| `elasticsearch` | `elasticsearch` (`[elasticsearch]`) | one document indexed at `<index>/_doc/<run_id>`, overwritten via `client.index` | `ES_URL` / `ELASTICSEARCH_URL` | `ELASTICSEARCH_API_KEY`, `runtime.runtime_plugins.elasticsearch.index` (default `circuitry-runs`) |
| `opensearch` | `opensearch-py` (`[opensearch]`) | same snapshot semantics as `elasticsearch`, different driver | `OPENSEARCH_URL` | `OPENSEARCH_USER`+`OPENSEARCH_PASSWORD` (basic auth), `runtime.runtime_plugins.opensearch.index` (default `circuitry-runs`) |
| `redis` | `redis` (`[redis]`) | JSON snapshot at key `run:<run_id>`, optional TTL | — (default `redis://localhost:6379`) | `REDIS_URL`, `runtime.runtime_plugins.redis.ttl_seconds`, `runtime.runtime_plugins.redis.key_prefix` |
| `memcached` | `pymemcache` (`[memcached]`) | JSON snapshot at key `run:<run_id>`, optional expiration — ephemeral, no scan | — (default `localhost:11211`) | `MEMCACHED_URL`, `runtime.runtime_plugins.memcached.ttl_seconds` |
| `s3` (runtime) | `boto3` (`[s3-runtime]`) | one blob at `runs/<run_id>.json` in the bucket | `S3_BUCKET` | `S3_PREFIX` (default `runs/`), `AWS_REGION`, standard AWS credential chain |
| `gcs` | `google-cloud-storage` (`[gcs]`) | one blob at `runs/<run_id>.json` | `GCS_BUCKET` | `GCS_PREFIX` (default `runs/`), `GCS_PROJECT`, auth via `GOOGLE_APPLICATION_CREDENTIALS` or the Google Cloud auth chain |
| `azure-blob` | `azure-storage-blob` + `azure-identity` (`[azure-blob]`) | one blob at `runs/<run_id>.json` in the container | `AZURE_STORAGE_CONNECTION_STRING` or `AZURE_STORAGE_ACCOUNT_URL` (`DefaultAzureCredential` chain), plus `AZURE_BLOB_CONTAINER` | `runtime.runtime_plugins.azure-blob.prefix` (default `runs/`) |
| `r2` | `boto3` (`[r2]`) | S3-API-compatible; one blob per run (same shape as `s3`) | `R2_ACCOUNT_ID`, `R2_ACCESS_KEY_ID`, `R2_SECRET_ACCESS_KEY`, `R2_BUCKET` | `runtime.runtime_plugins.r2.prefix` (default `runs/`), `runtime.runtime_plugins.r2.endpoint_url` |
| `dynamodb` | `boto3` (`[dynamodb]`) | one item per run, partition key `run_id` — table must pre-exist | `DYNAMODB_TABLE` | `DYNAMODB_REGION`, standard boto3 credential chain |

Every required setting above also has a `runtime.runtime_plugins.<name>.<key>`
config.json spelling (e.g. `runtime.runtime_plugins.s3.bucket`); the env var
wins when both are set. The runtime `s3` plugin and the LLM-callable `s3`
*tool* plugin share a name but live in different registries
(`runtime_plugins/` vs. `PLUGIN_REGISTRY`) and are configured separately.

## Pub/sub family (`kafka`, `rabbitmq`, `nats`)

These three publish JSON-encoded lifecycle events to a topic/queue/exchange
instead of persisting anything themselves — a downstream consumer is
responsible for storage. All extend the shared `PubSubBase`.

| plugin | extra | broker setting | topic (default `circuitry-runs`) | notes |
| --- | --- | --- | --- | --- |
| `kafka` | `confluent-kafka` (`[kafka]`) | `KAFKA_BROKERS` / `KAFKA_BOOTSTRAP_SERVERS` (required) | `runtime.runtime_plugins.kafka.topic` | `runtime.runtime_plugins.kafka.publish_per_effect` (bool, default `true`) |
| `rabbitmq` | `pika` (`[rabbitmq]`) | `RABBITMQ_URL` (default `amqp://guest:guest@localhost:5672/`) | `runtime.runtime_plugins.rabbitmq.topic` | also `exchange` (default `""`) and `exchange_type` (default `direct`) — the default exchange routes by topic-as-queue-name |
| `nats` | `nats-py` (`[nats]`) | `NATS_URL` (default `nats://localhost:4222`) | `runtime.runtime_plugins.nats.topic` | runs its own asyncio event loop on a dedicated thread per run, since `nats-py` is async-only |

## Observability family

These seven emit traces, metrics, logs or error events rather than
persisting run state.

| plugin | extra | signal | required config | notes |
| --- | --- | --- | --- | --- |
| `opentelemetry` | `opentelemetry-api`/`-sdk`/`-exporter-otlp-proto-http` (`[opentelemetry]`) | one span per run, child spans per effect | — | standard `OTEL_*` env vars (`OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_SERVICE_NAME`, ...); falls back to a console exporter when no OTLP endpoint is set |
| `honeycomb` | same OTel packages (`[honeycomb]`) | same span schema as `opentelemetry`, exported to Honeycomb's OTLP ingest | `HONEYCOMB_API_KEY` | `HONEYCOMB_DATASET` (default `circuitry`), `HONEYCOMB_API_HOST` (default `https://api.honeycomb.io`) |
| `sentry` | `sentry-sdk` (`[sentry]`) | exceptions on run failure, tagged with `run_id`/`orchestration_path`; completed effects become breadcrumbs | `SENTRY_DSN` | — |
| `datadog` | `datadog` (`[datadog]`) | count metrics (`circuitry.runs.started`, `circuitry.effects.completed`, `circuitry.runs.succeeded`/`failed`) plus token-count histograms | `DD_API_KEY` | `DD_APP_KEY`, `DD_SITE` (default `datadoghq.com`) |
| `prometheus` | `prometheus-client` (`[prometheus]`) | counters (`circuitry_runs_total{status=...}`, `circuitry_effects_total`) and token histograms | — | Pushgateway mode when `PROMETHEUS_PUSHGATEWAY` is set; otherwise accumulates in the default registry for an external scrape endpoint |
| `cloudwatch` | `boto3` (`[cloudwatch]`) | JSON lifecycle events as CloudWatch Logs lines | `CLOUDWATCH_LOG_GROUP` (log group must pre-exist) | `CLOUDWATCH_REGION`, `runtime.runtime_plugins.cloudwatch.log_stream` (default `circuitry-{run_id}`) |
| `loki` | `requests` (`[loki]`) | JSON lifecycle events pushed to Loki's `/loki/api/v1/push`, labelled `run_id`+`event` | `LOKI_URL` | `LOKI_USER`/`LOKI_PASSWORD` (basic auth), `runtime.runtime_plugins.loki.labels` (extra static labels) |
