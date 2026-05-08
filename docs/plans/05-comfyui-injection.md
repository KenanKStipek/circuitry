# Plan 05: Fix Command Injection and Path Traversal in ComfyUI Plugin

## Problem

1. **Command injection via `image_path` (line 114):** `_upload_image` passes `image_path` to curl `-F` flag without validation. Paths like `../../etc/passwd` or paths with curl form-data metacharacters (`;type=`) can be exploited.
2. **Path traversal via `image_dir` (line 347):** User-supplied `image_dir` flows into `os.makedirs()` and `os.path.join()` without confinement.

## Steps

### Step 1: Add `_validate_image_path()` helper

```python
_ALLOWED_IMAGE_EXTENSIONS = frozenset({
    ".png", ".jpg", ".jpeg", ".gif", ".bmp", ".webp", ".tiff", ".tif",
})

def _validate_image_path(image_path: str) -> Path:
    # 1. Type check: non-empty string
    # 2. No null bytes
    # 3. No semicolons (curl form-data metacharacter)
    # 4. Resolve and verify exists + is_file
    # 5. Extension whitelist check
    return resolved
```

Update `_upload_image` to call `_validate_image_path` and use the resolved path.

### Step 2: Add `_validate_image_dir()` helper

```python
def _validate_image_dir(image_dir: str, *, allowed_base: Path | None = None) -> Path:
    # 1. Type check: non-empty string
    # 2. No null bytes
    # 3. No shell metacharacters (&&, ||, |, ;, >, <, `, $()
    # 4. Resolve and verify within allowed_base (default: cwd)
    return resolved
```

Update `execute` to call `_validate_image_dir` before `os.makedirs`.

### Step 3: Add `__post_init__` for base_url validation

Ensure `base_url` starts with `http://` or `https://`.

### Step 4: URL-encode query parameters in `view_url`

Replace f-string interpolation with `urllib.parse.urlencode` for `filename`, `subfolder`, `type` values.

### Step 5: Create `tests/plugins/test_comfyui.py`

**`_validate_image_path` tests:**
- Rejects empty string, null bytes, semicolons, nonexistent files, directories, disallowed extensions
- Accepts valid image, resolves relative paths

**`_validate_image_dir` tests:**
- Rejects empty string, null bytes, shell metacharacters, path traversal (`../../etc/cron.d`)
- Accepts subdir of cwd, respects `allowed_base`

**`_upload_image` tests:**
- Calls curl with resolved path
- Rejects traversal paths before subprocess is called

**`execute` tests:**
- Validates `image_dir`, writes to validated dir, URL-encodes view_url

**`__post_init__` tests:**
- Rejects non-http schemes, accepts http/https

## Files to Change

| File | Change |
|------|--------|
| `src/circuitry/plugins/comfyui.py` | Add validators, `__post_init__`, URL encoding |
| `tests/plugins/test_comfyui.py` | **Create** -- ~20 tests |
