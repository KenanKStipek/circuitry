# Plan 09: Remove Hardcoded Private IP from config.json

## Problem

Committed `config.json` contains `192.168.1.173` for Ollama and ComfyUI. Non-portable, leaks network topology.

## Key Insight

Config resolution and plugin factory already have correct localhost defaults. The only problem is the committed file overriding them.

## Steps

### Step 1: Create `config.example.json`

Copy of current structure with `localhost` replacing private IP. Serves as documentation and template.

### Step 2: Add `config.json` to `.gitignore`

### Step 3: Remove `config.json` from Git tracking

`git rm --cached config.json` -- stops tracking without deleting local copy.

### Step 4: Add `CIRCUITRY_COMFYUI_URL` env var support

In `config.py` `_apply_env_vars()`, add handling for `CIRCUITRY_COMFYUI_URL` (parallel to existing `CIRCUITRY_ADAPTER_URL`).

### Step 5: Add ComfyUI to `SANE_DEFAULTS`

```python
"plugins": {
    "comfyui": {
        "base_url": "http://localhost:8188",
    },
},
```

### Step 6: Add tests

- `test_apply_env_vars_comfyui_url`
- `test_apply_env_vars_comfyui_url_noop_when_unset`
- `test_sane_defaults_include_comfyui`

### Step 7: Update README

Document `CIRCUITRY_COMFYUI_URL`, `config.example.json` workflow.

## Migration

- **Existing users:** `git pull` deletes tracked file, local copy untouched. Zero disruption.
- **New clones:** No `config.json`, sane defaults apply. Copy `config.example.json` if customization needed.
- **Non-localhost users:** Set env vars or create local `config.json` (gitignored).

## Files to Change

| File | Change |
|------|--------|
| `config.example.json` | **Create** -- template with localhost defaults |
| `config.json` | Remove from tracking (`git rm --cached`) |
| `.gitignore` | Add `config.json` |
| `src/circuitry/cli/config.py` | Add `CIRCUITRY_COMFYUI_URL` support, expand `SANE_DEFAULTS` |
| `tests/cli/test_config_resolution.py` | Add tests |
| `README.md` | Document new env var and workflow |
