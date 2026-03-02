---
title: 'Image Generation Adapter and Prompt Type'
slug: 'image-generation-adapter-and-prompt-type'
created: '2026-03-01'
status: 'ready-for-dev'
stepsCompleted: [1, 2, 3, 4]
tech_stack:
  - Python 3.13
  - curl-based HTTP via subprocess (no requests library — matches all existing adapters)
  - pytest + monkeypatch (all adapter tests mock subprocess.run)
  - frozen dataclasses (all adapters and result types)
  - Protocol structural typing (adapter contracts, no ABC inheritance)
files_to_modify:
  - src/circuitry/adapters/base.py
  - src/circuitry/adapters/conformance.py
  - src/circuitry/adapters/factory.py
  - src/circuitry/adapters/__init__.py
  - src/circuitry/core/prompt.py
  - src/circuitry/core/compiler.py
  - src/circuitry/schema/orchestration.schema.json
files_to_create:
  - src/circuitry/adapters/automatic1111.py
  - tests/adapters/test_automatic1111.py
  - tests/core/test_image_prompt_type.py
code_patterns:
  - Adapters are frozen dataclasses; config fields set via dataclass fields not __init__
  - Protocol structural typing — ImageAdapter parallel to Adapter, no shared base
  - All HTTP via _curl_json() subprocess+curl pattern (see OllamaAdapter._curl_json)
  - build_adapter() in factory.py is sole construction point; reads runtime.adapters.<name>
  - _resolve_adapter() in PromptRuntime calls build_adapter() by adapter name string
  - prompt_type drives _decode_output(); image branch bypasses _decode_output entirely
  - conformance.py + validate_generate_result() validates output contract per adapter
test_patterns:
  - All adapter tests monkeypatch subprocess.run to return FakeProc(returncode, stdout)
  - Frozen dataclass stub adapters (echo/fail pattern) for PromptRuntime unit tests
  - validate_image_result() called in every A1111 conformance test
  - pytest.raises(RuntimeError) for error path assertions
---

# Tech-Spec: Image Generation Adapter and Prompt Type

**Created:** 2026-03-01

## Overview

### Problem Statement

Circuitry only supports text-in/text-out LLM calls. There is no way to invoke local text-to-image models (e.g. Stable Diffusion via Automatic1111) from an orchestration effect. The `Adapter` protocol, `GenerateResult`, and `prompt_type` system are all text-centric with no image generation surface.

### Solution

Add `prompt_type: image` and a parallel `ImageAdapter` protocol + `ImageResult` dataclass. `Automatic1111Adapter` implements `ImageAdapter` via the A1111 REST API (`/sdapi/v1/txt2img`). `PromptRuntime` branches on `prompt_type == "image"` to call `generate_image()` instead of `generate()`. Output value format (file path / base64 string / URL) is configurable globally in `config.json` under the adapter block, with per-effect overrides via new `PromptDefinition` fields.

### Scope

**In Scope:**
- `ImageResult` dataclass + `ImageAdapter` Protocol in `base.py` (parallel to `GenerateResult` / `Adapter`)
- `Automatic1111Adapter` in `adapters/automatic1111.py` — POSTs to `/sdapi/v1/txt2img`, decodes base64 response, handles output format
- `validate_image_result()` in `conformance.py` (parallel to `validate_generate_result()`)
- Register `"automatic1111"` in `factory.py`; export new types from `adapters/__init__.py`
- Add `"image"` to `PromptType` literal in `prompt.py`
- New `PromptDefinition` fields: `image_output: "path"|"base64"|"url"` and `image_dir: str`
- `PromptRuntime.execute()` branch: when `prompt_type == "image"`, resolve an `ImageAdapter` and call `generate_image()`; store result value as path/base64/url per config; return early
- Compiler (`compiler.py`): recognize `"image"` as valid `prompt_type`; compile `image_output` and `image_dir` fields
- JSON Schema (`orchestration.schema.json`): add `"image"` to `prompt_type` enum; add `image_output` and `image_dir` to prompt effect schema

**Out of Scope (future):**
- Vision/multimodal (text + image input → text output)
- img2img
- Negative prompts, sampler/scheduler params (users can pass via existing `params:` field)
- Image/video manipulation adapter (resize, crop, filter, ffmpeg-style transforms — a future "image magic" adapter type)

## Context for Development

### Codebase Patterns

- **All adapters are frozen dataclasses** — no custom `__init__`, config goes in dataclass fields (e.g. `base_url: str = "http://localhost:7860"`). `Automatic1111Adapter` follows this exactly.
- **Protocol structural typing** — `Adapter` in `base.py` is a `Protocol`; `ImageAdapter` is a parallel `Protocol` with `generate_image()` instead of `generate()`. They share no base class.
- **All HTTP via subprocess+curl** — `OllamaAdapter._curl_json()` is the exact pattern to copy. No `requests`, no `httpx`. Zero extra dependencies.
- **`build_adapter()` is the sole construction point** — reads `runtime.adapters.<adapter_name>` from the runtime config dict. `Automatic1111Adapter` will be registered here with `base_url`, `default_image_output`, and `image_dir` config keys.
- **`_resolve_adapter()` in `PromptRuntime`** — looks up adapter by string name, calls `build_adapter()` if not the default. The image branch reuses `_parse_provider_token()` → `_resolve_adapter()` to find the `ImageAdapter`.
- **`prompt_type` drives output decoding** — `_decode_output()` is called on text results only. `prompt_type: image` bypasses it entirely; `PromptRuntime` directly writes the image value (path/base64/url string) to `store` and returns early.
- **`conformance.py`** — `validate_generate_result()` validates `GenerateResult` contract. A parallel `validate_image_result()` is added for `ImageResult`.
- **Unknown `prompt_type` currently falls back to `"text"` silently** — do not change this behaviour; only add `"image"` to the explicit allowlist.

### Files to Reference

| File | Purpose |
| ---- | ------- |
| `src/circuitry/adapters/base.py` | Add `ImageResult` dataclass + `ImageAdapter` Protocol |
| `src/circuitry/adapters/ollama.py` | Copy `_curl_json()` pattern verbatim into `Automatic1111Adapter` |
| `src/circuitry/adapters/conformance.py` | Add `validate_image_result()` parallel to `validate_generate_result()` |
| `src/circuitry/adapters/factory.py` | Register `"automatic1111"`; update `SUPPORTED_ADAPTERS` |
| `src/circuitry/adapters/__init__.py` | Export `ImageResult`, `ImageAdapter`, `Automatic1111Adapter`, `validate_image_result` |
| `src/circuitry/core/prompt.py` | Add `"image"` to `PromptType`; add `image_output`/`image_dir` to `PromptDefinition`; image branch in `PromptRuntime.execute()` |
| `src/circuitry/core/compiler.py` | `_compile_prompt`: add `"image"` to valid types; compile `image_output` and `image_dir` |
| `src/circuitry/schema/orchestration.schema.json` | `PromptEffect`: add `"image"` to `prompt_type` enum; add `image_output` + `image_dir` properties |
| `tests/adapters/test_conformance.py` | Reference for adapter test structure and `FakeProc` pattern |
| `tests/core/test_prompt_fallbacks.py` | Reference for `PromptRuntime` stub adapter pattern |

### Technical Decisions

- **Output format: global default + per-effect override** — `config.json` → `runtime.adapters.automatic1111.default_image_output` (default `"path"`); per-effect `image_output:` field overrides it. Precedence: effect field > adapter config > hardcoded default `"path"`.
- **`image_dir` default**: `./output/images` relative to CWD; `PromptRuntime` creates it with `os.makedirs(exist_ok=True)` before writing.
- **`ImageAdapter` is a separate `Protocol`** — `PromptRuntime` checks `hasattr(adapter, "generate_image")` when `prompt_type == "image"` and raises a clear `RuntimeError` if the adapter doesn't support it.
- **A1111 API response** — `POST /sdapi/v1/txt2img` returns `{"images": ["<base64_png_string>", ...]}`. Adapter decodes `images[0]` with `base64.b64decode()` → stores in `ImageResult.image_bytes`.
- **`ImageResult` shape**: `image_bytes: bytes | None`, `image_url: str | None`, `raw: dict[str, Any]`.
- **State value written to store per mode**:
  - `"path"` → save bytes to `<image_dir>/<effect_name>_<YYYYMMDDTHHMMSS>.png`; store path string as `value`
  - `"base64"` → store `base64.b64encode(image_bytes).decode()` string as `value`
  - `"url"` → store `image_url` from result as `value` (A1111 doesn't return URLs natively; reserved for future adapters)
- **Filename for path mode**: `<effect_name>_<YYYYMMDDTHHMMSS>.png` — deterministic and debuggable.

## Implementation Plan

### Tasks

- [ ] Task 1: Add `ImageResult` dataclass and `ImageAdapter` Protocol to `base.py`
  - File: `src/circuitry/adapters/base.py`
  - Action: Append after the existing `GenerateResult` and `Adapter` definitions. Add `@dataclass(frozen=True) class ImageResult` with fields `image_bytes: bytes | None`, `image_url: str | None`, `raw: dict[str, Any]`. Add `class ImageAdapter(Protocol)` with `name -> str` property and `generate_image(*, model: str, prompt: str, params: dict[str, Any] | None, timeout_seconds: int) -> ImageResult` method signature.
  - Notes: Import `Any` from `typing` is already present. No changes to existing code.

- [ ] Task 2: Create `Automatic1111Adapter`
  - File: `src/circuitry/adapters/automatic1111.py` (new file)
  - Action: Frozen dataclass with fields: `name: str = "automatic1111"`, `base_url: str = "http://localhost:7860"`, `default_image_output: str = "path"`, `image_dir: str = "./output/images"`. Copy `_curl_json()` from `OllamaAdapter` verbatim (same curl+subprocess pattern, same error handling). Implement `generate_image()`: POST to `{base_url}/sdapi/v1/txt2img` with payload `{"prompt": prompt, "steps": 20, **(params or {})}`, decode `response["images"][0]` with `base64.b64decode()`, return `ImageResult(image_bytes=decoded, image_url=None, raw=response)`.
  - Notes: Import `base64` from stdlib. `steps: 20` is a safe default; users can override via `params:`. Raise `RuntimeError` if `response.get("images")` is empty or missing.

- [ ] Task 3: Add `validate_image_result()` to `conformance.py`
  - File: `src/circuitry/adapters/conformance.py`
  - Action: Add function `validate_image_result(result: ImageResult, *, adapter_name: str) -> list[str]`. Checks: `raw` is `dict`; `image_bytes` is `bytes | None`; `image_url` is `str | None`; at least one of `image_bytes` or `image_url` is not `None`. Return list of diagnostic strings, empty if all pass.
  - Notes: Import `ImageResult` from `.base`. Follow the same diagnostic message format as `validate_generate_result()` (e.g. `f"{adapter_name}: 'raw' must be dict, got {type(result.raw).__name__}"`).

- [ ] Task 4: Register `Automatic1111Adapter` in `factory.py` and export from `__init__.py`
  - File: `src/circuitry/adapters/factory.py`
  - Action: Add `from .automatic1111 import Automatic1111Adapter` import. Add `"automatic1111"` to `SUPPORTED_ADAPTERS` tuple. Add branch in `build_adapter()`: `if adapter_name == "automatic1111": cfg = adapters_cfg.get("automatic1111") or {}; return Automatic1111Adapter(base_url=cfg.get("base_url") or "http://localhost:7860", default_image_output=cfg.get("default_image_output") or "path", image_dir=cfg.get("image_dir") or "./output/images")`.
  - File: `src/circuitry/adapters/__init__.py`
  - Action: Add imports and exports for `Automatic1111Adapter`, `ImageResult`, `ImageAdapter` (from `.base`), `validate_image_result` (from `.conformance`). Add all four to `__all__`.

- [ ] Task 5: Update `PromptType`, `PromptDefinition`, and `PromptRuntime` in `prompt.py`
  - File: `src/circuitry/core/prompt.py`
  - Action (a) — `PromptType` literal at line ~64: add `"image"` → `Literal["text", "json", "boolean", "tool", "number", "array", "object", "image"]`
  - Action (b) — `PromptDefinition` dataclass: add two fields after `assets`: `image_output: Optional[Literal["path", "base64", "url"]] = None` and `image_dir: Optional[str] = None`. Add `Literal` to the `typing` import if not already present.
  - Action (c) — `PromptRuntime.execute()`: after `prompt_sent = self._materialize_input(effective_ctx)` and the meta setup block, add an early-return image branch before the `if self.dry_run:` check:
    ```python
    if self.defn.prompt_type == "image":
        self._execute_image(store=store, node=node, meta=meta, prompt_sent=prompt_sent, t0=t0, target=target)
        return
    ```
  - Action (d) — Add `_execute_image()` method to `PromptRuntime`: resolves adapter via `_resolve_adapter()` (using primary adapter name from `_build_attempts()[0]`), checks `hasattr(adapter, "generate_image")` and raises `RuntimeError(f"Adapter '{adapter.name}' does not support image generation (no generate_image method). Use an ImageAdapter such as 'automatic1111'.")` if not, determines `image_output` and `image_dir` (effect field > adapter config attr > hardcoded default), calls `adapter.generate_image(model=resolved_model, prompt=prompt_sent, params=self.defn.params, timeout_seconds=self.timeout_seconds)`, converts result to store value, writes `node["value"]` and updates `meta` (`completed_at`, `adapter`, `model`), handles verbose output.
  - Notes: For `"path"` mode: `os.makedirs(image_dir, exist_ok=True)`, filename = `f"{self.defn.name}_{datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%S')}.png"`, write `result.image_bytes` to `os.path.join(image_dir, filename)`, store path as value. For `"base64"` mode: `base64.b64encode(result.image_bytes).decode()`. For `"url"` mode: `result.image_url`. Import `os` and `base64` at top of file.

- [ ] Task 6: Update `_compile_prompt` in `compiler.py`
  - File: `src/circuitry/core/compiler.py`
  - Action (a) — In the `prompt_type_raw not in (...)` allowlist (~line 463), add `"image"` to the tuple.
  - Action (b) — After the `assets` compilation block (~line 514), add: `image_output_raw = effect.get("image_output"); image_output = image_output_raw if image_output_raw in ("path", "base64", "url") else None` and `image_dir = str(effect.get("image_dir")) if effect.get("image_dir") else None`.
  - Action (c) — Pass `image_output=image_output, image_dir=image_dir` to the `PromptDefinition(...)` constructor call (~line 538).

- [ ] Task 7: Update JSON Schema
  - File: `src/circuitry/schema/orchestration.schema.json`
  - Action (a) — In `$defs.PromptEffect.properties.prompt_type.enum` (~line 156): add `"image"` to the array.
  - Action (b) — In `$defs.PromptEffect.properties` (~line 188, after `on_error`): add two new properties:
    ```json
    "image_output": {
      "type": "string",
      "enum": ["path", "base64", "url"],
      "description": "How to store the generated image in state. path=save file, store path string; base64=store encoded string; url=store URL returned by adapter. Defaults to adapter config or 'path'."
    },
    "image_dir": {
      "type": "string",
      "description": "Directory to save images when image_output is 'path'. Created automatically if absent. Defaults to adapter config or './output/images'."
    }
    ```

- [ ] Task 8: Write tests for `Automatic1111Adapter`
  - File: `tests/adapters/test_automatic1111.py` (new file)
  - Action: Write four tests following `test_conformance.py` pattern:
    1. `test_automatic1111_generate_image_happy_path` — monkeypatch `subprocess.run` to return `FakeProc(returncode=0, stdout=json.dumps({"images": [base64.b64encode(b"PNG_BYTES").decode()]}))`, assert `result.image_bytes == b"PNG_BYTES"`, assert `validate_image_result(result, adapter_name="automatic1111") == []`
    2. `test_automatic1111_generate_image_passes_params` — assert payload includes extra keys when `params={"steps": 50}` passed
    3. `test_automatic1111_generate_image_curl_error` — monkeypatch `subprocess.run` to return `FakeProc(returncode=28, stderr="timeout")`, assert `pytest.raises(RuntimeError)` with `"curl failed"` in message
    4. `test_validate_image_result_catches_missing_image_data` — assert `validate_image_result(ImageResult(image_bytes=None, image_url=None, raw={}), adapter_name="a1111")` returns non-empty diagnostics

- [ ] Task 9: Write tests for image `PromptRuntime` routing
  - File: `tests/core/test_image_prompt_type.py` (new file)
  - Action: Write five tests following `test_prompt_fallbacks.py` stub-adapter pattern:
    1. `test_image_prompt_path_mode_writes_file_path` — stub `ImageEchoAdapter` returning `ImageResult(image_bytes=b"\x89PNG", image_url=None, raw={})`, execute orchestration with `prompt_type: image, image_output: path`, assert `store.get("prime.img.value")` is a string ending in `.png`, assert file exists on disk, clean up
    2. `test_image_prompt_base64_mode_writes_encoded_string` — same stub, `image_output: base64`, assert value is valid base64 string decoding to `b"\x89PNG"`
    3. `test_image_prompt_url_mode_writes_url` — stub returning `ImageResult(image_bytes=None, image_url="http://host/img.png", raw={})`, `image_output: url`, assert value is `"http://host/img.png"`
    4. `test_image_prompt_raises_when_adapter_lacks_generate_image` — use standard `EchoAdapter` (text only), `prompt_type: image`, assert `pytest.raises(RuntimeError)` with `"does not support image generation"` in message
    5. `test_image_prompt_meta_records_prompt_type` — assert `store.get("prime.img.meta.prompt_type") == "image"`

### Acceptance Criteria

- [ ] AC1: Given `base.py` is imported, when `ImageResult(image_bytes=b"x", image_url=None, raw={})` is constructed, then it is a frozen dataclass and `ImageResult.image_bytes == b"x"`
- [ ] AC2: Given `subprocess.run` is monkeypatched to return a valid A1111 response with one base64 image, when `Automatic1111Adapter().generate_image(model="sd-v1-5", prompt="a sunset", params=None, timeout_seconds=30)` is called, then it returns `ImageResult` with non-None `image_bytes` and `validate_image_result()` returns `[]`
- [ ] AC3: Given `subprocess.run` returns `returncode=28`, when `generate_image()` is called, then `RuntimeError` is raised with `"curl failed"` in the message
- [ ] AC4: Given `ImageResult(image_bytes=None, image_url=None, raw={})`, when `validate_image_result(result, adapter_name="automatic1111")` is called, then it returns a non-empty list with a diagnostic mentioning missing image data
- [ ] AC5: Given `runtime = {"adapters": {"automatic1111": {"base_url": "http://1.2.3.4:7860", "default_image_output": "base64"}}}`, when `build_adapter(adapter_name="automatic1111", runtime=runtime)` is called, then it returns `Automatic1111Adapter` with `base_url="http://1.2.3.4:7860"` and `default_image_output="base64"`
- [ ] AC6: Given an effect dict `{"type": "prompt", "name": "img", "template": "x", "prompt_type": "image", "image_output": "path", "image_dir": "./out"}`, when `_compile_prompt(effect)` is called, then it returns `PromptDefinition` with `prompt_type="image"`, `image_output="path"`, `image_dir="./out"`
- [ ] AC7: Given a stub `ImageEchoAdapter` with `generate_image()` returning a fixed `ImageResult(image_bytes=b"\x89PNG", ...)`, when `PromptRuntime` executes a `PromptDefinition(prompt_type="image", image_output="path")`, then `store.get("prime.img.value")` is a string path ending in `.png` and `meta["prompt_type"] == "image"`
- [ ] AC8: Given a standard text `EchoAdapter` (no `generate_image` method), when `PromptRuntime` executes a `prompt_type: image` definition, then `RuntimeError` is raised with `"does not support image generation"` in the message
- [ ] AC9: Given an orchestration YAML with `prompt_type: image` and `image_output: "invalid_value"`, when `validate()` (schema validation) is called, then it fails with an error referencing `image_output`
- [ ] AC10: Given the full test suite at `tests/`, when `pytest tests/ -v` is run after all tasks are complete, then 0 tests regress

## Additional Context

### Dependencies

- `Automatic1111` must be running with `--api --listen` flags to expose the REST API on your LAN (see Notes)
- No new Python packages required — `base64`, `os`, `json`, `subprocess` are all stdlib; curl is already required by existing adapters

### Testing Strategy

- **`tests/adapters/test_automatic1111.py`** — 4 tests: happy path conformance, params passthrough, curl error, `validate_image_result` contract check. Monkeypatch `subprocess.run` throughout.
- **`tests/core/test_image_prompt_type.py`** — 5 tests: all 3 output modes (path/base64/url), non-image-adapter error, meta field correctness. Frozen dataclass stub `ImageEchoAdapter`.
- **Regression gate**: run `pytest tests/ -v` after every task; zero regressions required before moving to next task.
- **Manual smoke test** (after A1111 is running): add `automatic1111` to `config.json`, write a one-effect orchestration with `prompt_type: image`, run `scripts/circuitry run` and verify the image appears on disk.

### Notes

**Risk: `_execute_image()` method complexity** — the image branch touches adapter resolution, file I/O, and verbose output. Keep it focused; resist adding features like retry logic in the first pass (retries can be added later the same way the text path has them).

**Risk: `"url"` mode for A1111** — Automatic1111 does not natively return image URLs in its default configuration; `image_url` will be `None` from `Automatic1111Adapter`. If a user specifies `image_output: url` with this adapter, `PromptRuntime` should raise `RuntimeError("image_output: url requested but adapter returned no image_url")` rather than storing `None`.

**Future: `image magic` adapter** — noted as out of scope. When implemented it should also use `ImageAdapter` protocol (or a richer subtype), so nothing done here needs to be undone.

**Automatic1111 Local Setup:**

```bash
# 1. Clone and install
git clone https://github.com/AUTOMATIC1111/stable-diffusion-webui
cd stable-diffusion-webui

# 2. Launch with API + LAN access (mirrors how Ollama is run)
./webui.sh --api --listen

# 3. Verify API is up
curl http://192.168.1.X:7860/sdapi/v1/sd-models
```

**`config.json` after setup:**

```json
{
  "default_model": "gpt-oss:20b",
  "default_adapter": "ollama",
  "runtime": {
    "adapters": {
      "ollama": {
        "base_url": "http://192.168.1.173:11434"
      },
      "automatic1111": {
        "base_url": "http://192.168.1.X:7860",
        "default_image_output": "path",
        "image_dir": "./output/images"
      }
    }
  }
}
```

**Example orchestration effect:**

```yaml
- type: prompt
  name: hero_image
  prompt_type: image
  template: "a majestic mountain at sunset, photorealistic, 8k"
  provider: automatic1111:stable-diffusion-v1-5
  image_output: path          # optional — overrides adapter config default
  image_dir: ./output/images  # optional — overrides adapter config default
```

State after execution: `prime.hero_image.value` = `"./output/images/hero_image_20260301T120000.png"`
