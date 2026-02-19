# Postgres Persistence

Circuitry supports optional runtime state persistence to Postgres.
For local integration testing, a SQLite backend is also available.

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
- `backend`: `postgres` (production) or `sqlite` (local/integration)
- `dsn`: Postgres connection string
- `db_path`: SQLite database path (required when backend is `sqlite`)
- `table`: storage table name (default `circuitry_runs`)
- `sslmode`: recommended `require`, `verify-ca`, or `verify-full`
- `allow_insecure`: optional local-dev override for `sslmode=disable|allow|prefer`

## Runtime Behavior

When enabled:
1. Runtime optionally hydrates state from latest persisted snapshot for the same orchestration path (when no `--state` or in-memory initial state is provided).
2. Runtime writes a full run snapshot after execution.
3. `runtime.persistence` metadata records backend, status, run id, and errors.

## Schema

Schema is created automatically on first use:
- `run_id TEXT PRIMARY KEY`
- `orchestration_path TEXT`
- `created_at TIMESTAMPTZ`
- `ok BOOLEAN`
- `error TEXT`
- `state_json JSONB`

Index:
- `(orchestration_path, created_at DESC)`

## Dependencies

Install psycopg for Postgres support:

```bash
pip install "psycopg[binary]"
```

## Failure Diagnostics

Persistence failures are non-silent and surfaced in:
- command/API error result
- `runtime.persistence.status`
- `runtime.persistence.error`
