# Plan 12: Safe Table Name Handling in Persistence Backends

## Problem

SQLite and Postgres backends interpolate table names via f-strings. SQLite has a regex guard; Postgres has none. Defense-in-depth requires proper identifier quoting at the SQL layer.

## Steps

### Step 1: Create `src/circuitry/core/store/_identifiers.py`

```python
_TABLE_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]{0,62}$")

def validate_table_name(name: str) -> str:
    """Validate and return a table name, or raise ValueError."""

def quote_sqlite_identifier(name: str) -> str:
    """Double-quote a SQLite identifier, escaping internal double-quotes."""
    return '"' + name.replace('"', '""') + '"'
```

### Step 2: Refactor `sqlite.py`

- Remove module-level `_TABLE_RE`
- Import from `_identifiers`
- Replace inline regex with `validate_table_name(table)` in `from_config`
- Add `_quoted_table` property using `quote_sqlite_identifier`
- Replace all `f"...{self.table}..."` with `f"...{self._quoted_table}..."`

### Step 3: Refactor `postgres.py`

- Import `validate_table_name` from `_identifiers`
- **Add validation in `from_config`** (currently missing entirely)
- Rewrite all SQL to use `psycopg.sql.SQL` + `psycopg.sql.Identifier`:

```python
cur.execute(sql.SQL("""
    CREATE TABLE IF NOT EXISTS {} (...)
""").format(sql.Identifier(self.table)))
```

### Step 4: Add tests

**`tests/core/test_identifiers.py`** (new):
- `validate_table_name` accepts/rejects correctly
- `quote_sqlite_identifier` handles clean names and embedded quotes

**`tests/core/test_sqlite_persistence_config.py`**:
- SQL injection characters rejected
- Over-length names rejected

**`tests/core/test_postgres_persistence_config.py`**:
- Invalid names now rejected (new behavior)
- SQL metacharacters rejected

### Step 5: Verify integration test passes

`tests/integration/test_sqlite_persistence_integration.py` uses `table: "circuitry_runs"` -- clean identifier, should pass.

## Files to Change

| File | Change |
|------|--------|
| `src/circuitry/core/store/_identifiers.py` | **Create** -- shared validation + quoting |
| `src/circuitry/core/store/sqlite.py` | Use shared validation + identifier quoting |
| `src/circuitry/core/store/postgres.py` | Add validation + `psycopg.sql.Identifier` |
| `tests/core/test_identifiers.py` | **Create** -- unit tests |
| `tests/core/test_sqlite_persistence_config.py` | Add injection tests |
| `tests/core/test_postgres_persistence_config.py` | Add validation tests |
