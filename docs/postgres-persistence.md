# Postgres Persistence

Circuitry supports optional runtime state persistence to Postgres.
For local integration testing, a SQLite backend is also available, plus
`jsonl-file` (stdlib append log) and `mongodb` (`pip install
circuitry-cof[mongodb]`). Any of the four can also be selected per run from a
named profile — see [Named Profiles → Persistence](profiles.md#persistence).

## Configuration

Add persistence config under `runtime.persistence`:

```json
{
  "runtime": {
    "persistence": {
      "enabled": true,
      "backend": "postgres",
      "dsn": "postgres://user:password@host:5432/dbname",
      "table": "circuitry_runs",
      "sslmode": "require"
    }
  }
}
```

Fields:
- `enabled`: enable/disable persistence
- `backend`: `postgres` (production), `sqlite` (local/integration),
  `jsonl-file` (stdlib append log), or `mongodb`
- `dsn`: Postgres connection string
- `db_path`: SQLite database path (required when backend is `sqlite`; `path` is accepted as an alias)
- `path`: JSONL log path (required when backend is `jsonl-file`)
- `uri`: MongoDB connection string (required when backend is `mongodb`)
- `database` / `collection`: MongoDB targets (default `circuitry` / `circuitry_runs`)
- `table`: storage table name (default `circuitry_runs`)
- `sslmode`: recommended `require`, `verify-ca`, or `verify-full`
- `allow_insecure`: optional local-dev override for `sslmode=disable|allow|prefer`

## Runtime Behavior

When enabled:
1. Runtime optionally hydrates state from latest persisted snapshot for the same orchestration path (when no `--state` or in-memory initial state is provided).
2. Runtime writes a full run snapshot after execution.
3. `runtime.persistence` metadata records backend, status, run id, and errors.

## Schema

Postgres/SQLite schema is created automatically on first use:
- `run_id TEXT PRIMARY KEY`
- `orchestration_path TEXT`
- `created_at TIMESTAMPTZ`
- `ok BOOLEAN`
- `error TEXT`
- `state_json JSONB`

Index:
- `(orchestration_path, created_at DESC)`

`jsonl-file` writes one JSON object per line (`run_id`,
`orchestration_path`, `created_at`, `ok`, `error`, `state`) and reads the
last matching record back. `mongodb` writes one document per run with
`_id = run_id` and the same fields, loading the latest by `created_at`.

## Dependencies

Install psycopg for Postgres support:

```bash
pip install "psycopg[binary]"
```

MongoDB support:

```bash
pip install "circuitry-cof[mongodb]"
```

`sqlite` and `jsonl-file` are stdlib-only.

## Failure Diagnostics

Persistence failures are non-silent and surfaced in:
- command/API error result
- `runtime.persistence.status`
- `runtime.persistence.error`
