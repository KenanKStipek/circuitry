# Plan 11: Fix Insecure `tempfile.mktemp()` in generate-orchestration

## Problem

`scripts/generate-orchestration:143` uses deprecated `tempfile.mktemp()` which has a TOCTOU race condition.

## Fix

Replace with `tempfile.NamedTemporaryFile(delete=False)`.

**Before:**
```python
if args.validate:
    import tempfile
    tmp = Path(tempfile.mktemp(suffix=".yml"))
    tmp.write_text(yaml_text + "\n", encoding="utf-8")
    vresult = validate(tmp)
    tmp.unlink(missing_ok=True)
```

**After:**
```python
if args.validate:
    import tempfile
    with tempfile.NamedTemporaryFile(
        mode="w", suffix=".yml", encoding="utf-8", delete=False
    ) as f:
        f.write(yaml_text + "\n")
        tmp = Path(f.name)
    try:
        vresult = validate(tmp)
    finally:
        tmp.unlink(missing_ok=True)
```

Bonus: `try/finally` ensures cleanup even if `validate()` raises (original code would leak the file).

## Files to Change

| File | Change |
|------|--------|
| `scripts/generate-orchestration` | Replace `mktemp` with `NamedTemporaryFile` |
