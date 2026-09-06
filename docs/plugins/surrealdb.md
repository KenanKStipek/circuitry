# surrealdb

Reads and writes SurrealDB from an orchestration: raw SurrealQL plus the four
record operations (`select`, `create`, `upsert`, `delete`).

```bash
pip install "circuitry-cof[surrealdb]"
```

## When to use

Multi-model (document / graph / relational) persistence inside an
orchestration — writing structured results a later effect (or a later run)
reads back, or querying an existing SurrealDB with SurrealQL.

This is a **tool plugin** (an LLM-callable effect), not a persistence runtime
plugin. It does not store run state; see [plugins.md](../plugins.md) for that.

## Configuration

Non-secret connection settings live under `runtime.plugins.surrealdb` in your
config file:

```json
{
  "runtime": {
    "plugins": {
      "surrealdb": {
        "url": "ws://localhost:8000/rpc",
        "namespace": "app",
        "database": "main"
      }
    }
  }
}
```

| Key | Default | Meaning |
| --- | --- | --- |
| `url` | `ws://localhost:8000/rpc` | RPC endpoint (`ws://`, `wss://`, `http://`, `https://`) |
| `namespace` | — | bound with `USE` after signin; required |
| `database` | — | bound with `USE` after signin; required |

`namespace` and `database` can be overridden per effect via params.

## Credentials

Credentials are read from the **environment only** — never from config, so they
cannot reach `runtime.effective_settings`, `--out` state, `last-run.json`, or a
persistence backend:

- `SURREAL_USER` + `SURREAL_PASS` — signin as a system/namespace/database user, or
- `SURREAL_TOKEN` — a pre-issued JWT (takes precedence when set).

## Params

- `mode` (string, default `query`) — `query` | `select` | `create` | `upsert` | `delete`
- `query` (string, `query`) — SurrealQL statement
- `params` (dict, `query`, optional) — bound SurrealQL variables (`$name`)
- `target` (string, `select`) — table name or record id (aliases: `record`, `table`)
- `table` (string, `create`) + `data` (dict)
- `record` (string, `upsert`) + `data` (dict)
- `record` (string, `delete`)
- `namespace` / `database` (string, optional) — override the configured pair

Values are coerced to JSON-safe types (record ids and datetimes become strings)
so results survive serialization into run state.

## Examples

```yaml
- type: tool
  name: save_person
  provider: surrealdb
  params:
    mode: create
    table: person
    data:
      name: "{{input.name}}"
      score: 10

- type: tool
  name: top_people
  provider: surrealdb
  params:
    mode: query
    query: "SELECT name FROM person WHERE score > $floor ORDER BY score DESC"
    params:
      floor: 5
```

## Readiness

`cof doctor` reports this plugin's `check()`, which surfaces:

- `library:surrealdb` — SDK not installed
- `env:SURREAL_USER` / `env:SURREAL_PASS` — no credentials in the environment
  (satisfied by `SURREAL_TOKEN` alone)
- `host:<url>` — the configured endpoint is not accepting connections

## Errors

Failures map to actionable messages and mark the run `ok=False`:

| Condition | Message shape |
| --- | --- |
| server unreachable | `surrealdb: cannot connect to <url> (...). Is the server running ...` |
| bad credentials | `surrealdb: authentication rejected by <url> (...)` |
| wrong namespace/database | `surrealdb: USE <ns> <db> failed (...)` |
| SurrealQL error | `surrealdb <mode> failed: ...` / `SurrealQL error: ...` |

## Running the integration tests

```bash
docker run --rm -p 8000:8000 surrealdb/surrealdb:latest start --user root --pass root

export CIRCUITRY_RUN_INTEGRATION=1
export SURREAL_URL=ws://localhost:8000/rpc
export SURREAL_USER=root SURREAL_PASS=root
pytest -m integration tests/integration/test_surrealdb_integration.py
```

In CI the same suite runs against a SurrealDB container. GitHub `services:`
entries cannot pass a command, and the SurrealDB image needs `start ...`, so
the container is launched as a step:

```yaml
  surrealdb-integration:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-python@v5
        with:
          python-version: "3.11"

      - name: Start SurrealDB
        run: |
          docker run -d --name surrealdb -p 8000:8000 surrealdb/surrealdb:latest \
            start --user root --pass root --bind 0.0.0.0:8000
          for _ in $(seq 1 30); do
            curl -sf http://localhost:8000/health && break
            sleep 1
          done

      - name: Install dependencies
        run: |
          pip install -e ".[tools,surrealdb]"
          pip install -r requirements-dev.txt

      - name: Run SurrealDB integration tests
        env:
          CIRCUITRY_RUN_INTEGRATION: "1"
          SURREAL_URL: ws://localhost:8000/rpc
          SURREAL_USER: root
          SURREAL_PASS: root
        run: pytest -m integration tests/integration/test_surrealdb_integration.py
```
