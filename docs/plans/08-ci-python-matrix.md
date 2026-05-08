# Plan 08: CI Python Version Matrix, Coverage, and Pytest Markers

## Problem

CI tests only Python 3.11 despite `requires-python = ">=3.9"`. No coverage reporting. No pytest markers defined in `pyproject.toml` despite code using `@pytest.mark.integration`.

## Compatibility Assessment

- All 41 source files use `from __future__ import annotations` (neutralizes PEP 604/585)
- No `match`/`case` (3.10+) or `ExceptionGroup` (3.11+) usage
- All dependencies support Python 3.9+

## Steps

### Step 1: Add `[tool.pytest.ini_options]` to `pyproject.toml`

```toml
[tool.pytest.ini_options]
testpaths = ["tests"]
markers = [
    "integration: marks tests that require local runtime dependencies (opt-in)",
]
addopts = ["--strict-markers"]
```

### Step 2: Add `pytest-cov` to dev dependencies

```
pytest-cov>=5.0
```

### Step 3: Restructure `quality.yml` with matrix

Split into two jobs:

**`test` job** -- runs across matrix `["3.9", "3.10", "3.11", "3.12", "3.13"]`:
- `fail-fast: false` to see all failures
- `pytest -q --cov=circuitry --cov-report=term-missing --cov-report=xml:coverage.xml`
- Upload coverage artifact on 3.11 only

**`lint-typecheck` job** -- runs once on 3.11:
- `ruff check .`
- `mypy src`

### Step 4 (Optional): Coverage threshold gate

Add `--cov-fail-under=70` once baseline is established.

### Step 5 (Optional): Coverage config in `pyproject.toml`

```toml
[tool.coverage.run]
source = ["circuitry"]
omit = ["src/circuitry/bundled/*"]

[tool.coverage.report]
exclude_lines = ["pragma: no cover", "if TYPE_CHECKING:", "if __name__ == .__main__."]
```

## Files to Change

| File | Change |
|------|--------|
| `.github/workflows/quality.yml` | Split into `test` (matrix) + `lint-typecheck` jobs |
| `requirements-dev.txt` | Add `pytest-cov>=5.0` |
| `pyproject.toml` | Add `[tool.pytest.ini_options]`, optionally `[tool.coverage]` |
