---
title: 'Tool Effect System and Plugin Architecture'
slug: 'tool-effect-plugin-architecture'
created: '2026-03-01'
status: 'ready-for-dev'
stepsCompleted: [1, 2, 3, 4]
tech_stack:
  - Python 3.13
  - subprocess (no requests — all HTTP via curl pattern)
  - pytest + monkeypatch (subprocess.run mocked in tests)
  - frozen dataclasses
  - Protocol structural typing (no ABC)
files_to_modify:
  - src/circuitry/adapters/base.py
  - src/circuitry/adapters/conformance.py
  - src/circuitry/adapters/factory.py
  - src/circuitry/adapters/__init__.py
  - src/circuitry/core/prompt.py
  - src/circuitry/core/compiler.py
  - src/circuitry/core/dynamic.py
  - src/circuitry/cli/runtime_shim.py
  - src/circuitry/schema/orchestration.schema.json
  - docs/orchestration-reference.md
  - orchestrations/_image.yml
  - orchestrations/comic_strip.yml
files_to_create:
  - src/circuitry/core/tool.py
  - src/circuitry/plugins/__init__.py
  - src/circuitry/plugins/base.py
  - src/circuitry/plugins/factory.py
  - src/circuitry/plugins/comfyui.py
  - src/circuitry/plugins/ffmpeg.py
  - tests/plugins/test_ffmpeg.py
  - tests/plugins/test_comfyui_plugin.py
  - tests/core/test_tool_effect.py
files_to_delete:
  - src/circuitry/adapters/comfyui.py
code_patterns:
  - Frozen dataclass definitions; Runtime classes hold execution logic
  - DynamicRuntime._execute_effect() is the central dispatch — add ToolDefinition branch here
  - EffectDef union type defined in both compiler.py and dynamic.py — both need updating
  - All string params values recursively Mustache-rendered before plugin execution
  - Plugins live in src/circuitry/plugins/ (distinct from adapters/ and core/plugins.py lifecycle hooks)
  - core/plugins.py is lifecycle hooks (on_run_start etc.) — unrelated to this feature
test_patterns:
  - monkeypatch.setattr("subprocess.run", fake_run) for subprocess-based plugins
  - Tests in tests/plugins/ directory (new)
  - tests/core/test_tool_effect.py for ToolRuntime integration
---

# Tech-Spec: Tool Effect System and Plugin Architecture

**Created:** 2026-03-01

## Overview

### Problem Statement

Side-effect integrations (image generation, ffmpeg, and future external tools) have no first-class home in the effect system. Image generation is currently bolted onto the `prompt` effect via `prompt_type: image`, which is a semantic hack — a prompt effect should represent an LLM call, not a tool execution. This makes it impossible to add further tool integrations cleanly without continuing to overload the prompt system.

### Solution

Introduce a `tool` effect type with a `provider` field that routes execution to registered plugins. Each plugin implements a `ToolPlugin` Protocol, is registered in a plugin factory, and returns a `ToolResult`. Template rendering applies to all string `params` values (recursively). ComfyUI migrates from `prompt_type: image` to `type: tool, provider: comfyui`. `ffmpeg` is implemented as the first new plugin under this system.

### Scope

**In Scope:**
- `ToolDefinition` dataclass + `ToolRuntime` executor (`src/circuitry/core/tool.py`)
- `ToolResult` + `ToolPlugin` Protocol in `src/circuitry/plugins/base.py`
- Plugin factory (`src/circuitry/plugins/factory.py`)
- `ffmpeg` plugin (`src/circuitry/plugins/ffmpeg.py`) — ffmpeg-only binary, shell operator stripping, `-y` always injected, configurable timeout
- `comfyui` migrated to `src/circuitry/plugins/comfyui.py`, old `src/circuitry/adapters/comfyui.py` deleted
- Compiler: recognize `type: tool`, compile to `ToolDefinition`
- Schema: add `tool` effect type with `provider` and `params`
- `value` = primary output (e.g. output file path); `stdout`/`stderr`/`exit_code` in meta
- Remove `prompt_type: image` execution path from `PromptRuntime`
- Remove `ImageResult` and `ImageAdapter` from `adapters/base.py`
- Remove `validate_image_result` from `adapters/conformance.py`
- Migrate `orchestrations/_image.yml` and `orchestrations/comic_strip.yml` to new syntax
- `_require_resolved_settings` made conditional on presence of `PromptDefinition` in compiled tree

**Out of Scope:**
- Generic shell execution plugin
- Multiple inputs/outputs for ffmpeg
- File management plugin
- Streaming ffmpeg output
- `prompt_type: image` backwards-compatibility shim

## Context for Development

### Codebase Patterns

- Frozen dataclass definitions hold configuration; Runtime classes hold execution logic (e.g. `PromptDefinition` + `PromptRuntime`)
- `DynamicRuntime._execute_effect()` in `core/dynamic.py` is the central dispatch point for all effect types — this is where a new `isinstance(effect, ToolDefinition)` branch must be added
- `EffectDef` union type is defined independently in both `compiler.py` and `dynamic.py` — both must be updated
- All `params` string values should be recursively Mustache-rendered via `chevron.render()` (same renderer used in `PromptRuntime._render()`) before passing to the plugin
- `core/plugins.py` is a lifecycle hook system (on_run_start, on_run_success, etc.) — completely separate from this feature; avoid naming conflicts
- Schema uses nested `if/then/else` JSON Schema draft-07 discriminated union on `type` field — extend the chain for `"tool"`
- Tests mock `subprocess.run` via `monkeypatch.setattr`; the ffmpeg plugin uses the same pattern as existing adapter transport

### Files to Reference

| File | Purpose |
| ---- | ------- |
| `src/circuitry/adapters/base.py` | Remove `ImageResult`, `ImageAdapter` only — no tool types added here |
| `src/circuitry/plugins/base.py` | NEW — `ToolResult`, `ToolPlugin` Protocol, `validate_tool_result` |
| `src/circuitry/adapters/comfyui.py` | Source for migration to `src/circuitry/plugins/comfyui.py` |
| `src/circuitry/adapters/conformance.py` | Remove `validate_image_result` only |
| `src/circuitry/adapters/factory.py` | Remove comfyui; update `SUPPORTED_ADAPTERS` |
| `src/circuitry/adapters/__init__.py` | Remove image/comfyui exports; no tool types added here |
| `src/circuitry/core/compiler.py` | Add `"tool"` branch in `_compile_effect()`; add `_compile_tool()`; update `EffectDef` |
| `src/circuitry/core/dynamic.py` | Add `ToolDefinition` to `EffectDef`; dispatch in `_execute_effect()`; add to `_EFFECT_STYLE` |
| `src/circuitry/core/prompt.py` | Remove `_execute_image`, image fields from `PromptDefinition`, `"image"` from `PromptType` |
| `src/circuitry/schema/orchestration.schema.json` | Add `ToolEffect` def; extend `EffectDef` if/then/else chain |
| `orchestrations/_image.yml` | Migrate: `type: prompt, prompt_type: image` → `type: tool, provider: comfyui` |
| `orchestrations/comic_strip.yml` | Migrate: `prompt_type: image, provider: comfyui:model` → `type: tool, provider: comfyui, params.model` |
| `docs/orchestration-reference.md` | Add `### tool` effect section; update prompt table (remove `image`); update LLM Authoring Rules |
| `tests/adapters/test_conformance.py` | Remove image conformance tests; pattern for new tool conformance tests |
| `tests/cli/test_validate.py` | Pattern for schema validation tests; add `tool` effect validation test |

### Technical Decisions

- Plugin-specific fields go under `params` (freeform dict) — schema stays stable as plugins are added
- All `params` string values are recursively Mustache-rendered against current context before execution; integers/floats/booleans pass through untouched; nested dicts recurse
- ffmpeg safety: only the `ffmpeg` binary is allowed; presence of shell metacharacters (`&&`, `||`, `|`, `;`, `>`, `<`, `` ` ``, `$(`, `\n`) raises `ValueError` before execution
- `-y` (auto-overwrite output) is always injected immediately after `ffmpeg` in the command
- ffmpeg command construction: `ffmpeg -y -i <params.input> <params.flags> <params.output>`
- `ToolResult.value` = primary output (output path for ffmpeg, image path/base64/url for comfyui); `stdout`, `stderr`, `exit_code` go in `meta`
- **`ToolPlugin`, `ToolResult`, and `validate_tool_result` live in `src/circuitry/plugins/base.py`** — not in `adapters/base.py` or `adapters/conformance.py`; `adapters/` stays LLM-adapter-only
- `adapters/conformance.py` — only remove `validate_image_result`; no new additions
- `adapters/__init__.py` — remove `ImageAdapter`, `ImageResult`, `ComfyUIAdapter` exports; no tool types added here
- `build_plugin(*, plugin_name: str, runtime: dict[str, Any]) -> ToolPlugin` in `plugins/factory.py` mirrors `build_adapter()` pattern; reads `runtime.plugins.<plugin_name>` for config
- `ToolRuntime` behaves like `PromptRuntime` for verbose output — manages its own spinner + done/error lines; `DynamicRuntime` must treat `ToolDefinition` like `PromptDefinition` (i.e. `is_tool = isinstance(effect, ToolDefinition)` suppresses outer start/done printing); passes `cb_start`, `cb_done`, `cb_error` to `ToolRuntime`; icon `"⚙"`, color `"white"` in `_EFFECT_STYLE`
- **Adapter/model resolution made conditional**: add `_has_prompt_effects(root_def: DynamicDefinition) -> bool` to `runtime_shim.py` — recursively walks compiled tree; if `False`, skip `_require_resolved_settings` and `build_adapter`; pass a `_NoOpAdapter` (inline frozen dataclass in runtime_shim) to `DynamicRuntime`; `_NoOpAdapter.generate()` raises `RuntimeError("No LLM adapter configured")` if ever called
- ComfyUI `provider: comfyui:flux1-dev-fp8.safetensors` (old colon-model syntax) → `provider: comfyui` + `params.model: flux1-dev-fp8.safetensors` (new)
- Image output mode logic (path/base64/url) that previously lived in `PromptRuntime._execute_image` moves entirely into `ComfyUIPlugin.execute()`

## Implementation Plan

### Tasks

- [ ] Task 1: Create `src/circuitry/plugins/base.py`
  - File: `src/circuitry/plugins/base.py` (new)
  - Action: Define the plugin contract types:
    ```python
    @dataclass(frozen=True)
    class ToolResult:
        value: Any
        raw: dict[str, Any]
        stdout: str | None = None
        stderr: str | None = None
        exit_code: int | None = None

    class ToolPlugin(Protocol):
        @property
        def name(self) -> str: ...
        def execute(self, *, params: dict[str, Any], timeout_seconds: int = 300) -> ToolResult: ...

    def validate_tool_result(result: ToolResult, *, plugin_name: str) -> list[str]: ...
    ```
  - Notes: `validate_tool_result` checks `value` is not None, `raw` is dict, `exit_code` is int|None >= 0. Follow exact same pattern as `validate_generate_result` in `conformance.py`.

- [ ] Task 2: Create `src/circuitry/plugins/__init__.py`
  - File: `src/circuitry/plugins/__init__.py` (new)
  - Action: Export `ToolPlugin`, `ToolResult`, `validate_tool_result` from `.base`; export `build_plugin` from `.factory`. Set `__all__` explicitly.

- [ ] Task 3: Create `src/circuitry/plugins/comfyui.py`
  - File: `src/circuitry/plugins/comfyui.py` (new)
  - Action: Copy `ComfyUIAdapter` from `adapters/comfyui.py` as the base. Rename class to `ComfyUIPlugin`. Replace `generate_image(*, model, prompt, params, timeout_seconds) -> ImageResult` with `execute(*, params: dict[str, Any], timeout_seconds: int) -> ToolResult`. All params come from `params` dict: `params["prompt"]` (required), `params.get("model") or self.default_model` (required, raises if empty), `params.get("width", 512)`, `params.get("height", 512)`, `params.get("steps", 20)`, etc. Move image-output logic (path/base64/url) from `PromptRuntime._execute_image` into this method. Return `ToolResult(value=<path_or_base64_or_url>, raw=history[prompt_id], stdout=None, stderr=None, exit_code=None)`. Keep all `_curl_json`, `_curl_bytes`, `_build_workflow` helpers unchanged.
  - Notes: Dataclass fields stay the same: `name`, `base_url`, `default_model`, `default_image_output`, `image_dir`, `poll_interval`.

- [ ] Task 4: Create `src/circuitry/plugins/ffmpeg.py`
  - File: `src/circuitry/plugins/ffmpeg.py` (new)
  - Action: Implement `FfmpegPlugin` as a frozen dataclass with `name: str = "ffmpeg"`. `execute(*, params, timeout_seconds)`:
    1. Validate `params["input"]` and `params["output"]` are present strings; raise `ValueError` if missing.
    2. `flags = str(params.get("flags", "")).strip()`
    3. Safety check: scan `input + flags + output` for shell metacharacters `&&`, `||`, `|`, `;`, `>`, `<`, `` ` ``, `$(`, `\n`; raise `ValueError(f"ffmpeg params contain unsafe shell characters: ...")` if found.
    4. Validate binary: `cmd[0]` must be `"ffmpeg"` (enforced by construction, not configurable).
    5. Build: `cmd = ["ffmpeg", "-y", "-i", params["input"]] + shlex.split(flags) + [params["output"]]`
    6. Run: `proc = subprocess.run(cmd, capture_output=True, text=True, check=False)`
    7. If `proc.returncode != 0`: raise `RuntimeError(f"ffmpeg failed (exit {proc.returncode}): {proc.stderr.strip()}")`
    8. Return `ToolResult(value=params["output"], raw={}, stdout=proc.stdout, stderr=proc.stderr, exit_code=proc.returncode)`
  - Notes: Import `shlex`, `subprocess` from stdlib only. No curl — ffmpeg is local binary.

- [ ] Task 5: Create `src/circuitry/plugins/factory.py`
  - File: `src/circuitry/plugins/factory.py` (new)
  - Action: Implement `build_plugin(*, plugin_name: str, runtime: dict[str, Any]) -> ToolPlugin`. Read plugin config from `(runtime or {}).get("plugins", {}).get(plugin_name, {})`. Cases:
    - `"ffmpeg"`: return `FfmpegPlugin()`
    - `"comfyui"`: return `ComfyUIPlugin(base_url=cfg.get("base_url") or "http://localhost:8188", default_model=cfg.get("default_model") or "", default_image_output=cfg.get("default_image_output") or "path", image_dir=cfg.get("image_dir") or "./output/images", poll_interval=float(cfg.get("poll_interval") or 2.0))`
    - else: raise `ValueError(f"Unknown plugin: {plugin_name!r}. Supported: ffmpeg, comfyui.")`
  - Notes: Define `SUPPORTED_PLUGINS = ("ffmpeg", "comfyui")` constant.

- [ ] Task 6: Create `src/circuitry/core/tool.py`
  - File: `src/circuitry/core/tool.py` (new)
  - Action: Define `ToolDefinition` and `ToolRuntime`:
    ```python
    @dataclass(frozen=True)
    class ToolDefinition:
        name: str
        provider: str
        params: dict[str, Any]
        timeout_ms: int | None = None
        on_error: Literal["fail", "skip", "continue"] = "fail"
        description: str | None = None
    ```
    `_render_params(params: dict, ctx: dict) -> dict` — recursively render all string values via `chevron.render(v, ctx)`; non-strings pass through as-is.

    `ToolRuntime.__init__(definition, *, runtime_config, dry_run, timeout_seconds, verbose, depth, cb_start, cb_done, cb_error)`.

    `ToolRuntime.execute(*, store, ctx)`:
    1. `node = store.ensure_dict(self.defn.name)`; set up `meta` dict.
    2. Record `meta["created_at"]`, `meta["provider"]`, `meta["params_rendered"]` (the rendered params).
    3. If `dry_run`: write `node["value"] = None`, print done line, return.
    4. Render params: `rendered = _render_params(self.defn.params, ctx)`.
    5. Resolve timeout: `timeout_seconds = self.defn.timeout_ms // 1000 if self.defn.timeout_ms else self.timeout_seconds`.
    6. If verbose and `cb_start`: call `cb_start()`; show spinner via `rich.live.Live` (same pattern as `PromptRuntime` when `cb_start is None`).
    7. `plugin = build_plugin(plugin_name=self.defn.provider, runtime=self.runtime_config)`.
    8. `result = plugin.execute(params=rendered, timeout_seconds=timeout_seconds)`.
    9. `node["value"] = result.value`; update meta with `stdout`, `stderr`, `exit_code`, `completed_at`.
    10. Print done/error line via `cb_done`/`cb_error` or `_console.print` if no callback.
    11. On exception: write `meta["error"]`, honour `on_error` (`fail`/`skip`/`continue`).
  - Notes: Import `build_plugin` from `..plugins.factory`; import `_render` from `.prompt` (or duplicate the chevron call — check for circular imports first; if circular, inline `chevron.render`).

- [ ] Task 7: Update `src/circuitry/core/compiler.py`
  - File: `src/circuitry/core/compiler.py`
  - Action:
    1. Add `from .tool import ToolDefinition` import.
    2. Update `EffectDef` union: add `ToolDefinition` to the `Union[...]`.
    3. In `_compile_effect()`: add `if effect_type == "tool": return _compile_tool(effect, scope_path=scope_path, effect_path=effect_path)` before the final `raise ValueError`.
    4. Add `_compile_tool(effect, *, scope_path, effect_path) -> ToolDefinition`:
       - Validate `name` is present and valid via `_validate_name(...)`.
       - Validate `provider = effect.get("provider")` is a non-empty string; raise `ValueError` if missing.
       - `params = effect.get("params") or {}`; ensure it's a dict.
       - Read `timeout_ms`, `on_error` (default `"fail"`), `description`.
       - Return `ToolDefinition(name=name, provider=provider, params=params, timeout_ms=timeout_ms, on_error=on_error, description=description)`.

- [ ] Task 8: Update `src/circuitry/core/dynamic.py`
  - File: `src/circuitry/core/dynamic.py`
  - Action:
    1. Add `"ToolDefinition"` to the `EffectDef` Union (as a string TYPE_CHECKING import to avoid circular; or deferred import in `_execute_effect`).
    2. In `_execute_effect()`:
       - Add `is_tool = isinstance(effect, ToolDefinition)` alongside `is_prompt`.
       - Change `if self.verbose and not is_prompt:` → `if self.verbose and not is_prompt and not is_tool:` (suppress outer start/done for tool effects).
       - Add `elif isinstance(effect, ToolDefinition):` branch — defer-import `from .tool import ToolDefinition, ToolRuntime`; call `ToolRuntime(effect, runtime_config=self.runtime_config, dry_run=self.dry_run, timeout_seconds=self.timeout_seconds, verbose=self.verbose, depth=self.depth, cb_start=cb_start, cb_done=cb_done, cb_error=cb_error).execute(store=store, ctx=ctx)`.
       - Change verbose done/error block: `if self.verbose and not is_prompt and not is_tool:`.
    3. Update `_effect_type_label()`: add `from .tool import ToolDefinition` (deferred); add `if isinstance(effect, ToolDefinition): return "tool"`.
    4. Add `"tool": ("⚙", "white")` to `_EFFECT_STYLE`.
    5. Update tracker item construction in tree mode: `_effect_type_label` already handles it via the dict lookup.

- [ ] Task 9: Update `src/circuitry/core/prompt.py`
  - File: `src/circuitry/core/prompt.py`
  - Action:
    1. Remove `"image"` from the `PromptType = Literal[...]` union.
    2. Remove `image_output` and `image_dir` fields from `PromptDefinition`.
    3. Remove `_execute_image()` method from `PromptRuntime`.
    4. In `execute()`: remove the `if self.defn.prompt_type == "image": self._execute_image(...); return` block.
    5. In `_compile_prompt` equivalent (note: `_compile_prompt` is actually in `compiler.py`): see Task 7.
    6. Remove `from ..adapters.base import GenerateResult, ImageResult` — change to `from ..adapters.base import GenerateResult` only.
    7. Remove `import base64` and `import os` if only used by `_execute_image` (check usages — `os` is used for `os.makedirs` and `os.path.join` in `_execute_image`; `base64` only in `_execute_image`).
  - Notes: Also update `_decode_output()` — ensure `"image"` is not in the handled types (it currently isn't; `image` returned early before `_decode_output` was called, so no change needed there).

- [ ] Task 10: Update `src/circuitry/adapters/base.py`
  - File: `src/circuitry/adapters/base.py`
  - Action: Remove `ImageResult` dataclass and `ImageAdapter` Protocol entirely. Keep only `GenerateResult` and `Adapter`.

- [ ] Task 11: Update `src/circuitry/adapters/conformance.py`
  - File: `src/circuitry/adapters/conformance.py`
  - Action: Remove `validate_image_result` function. Remove `ImageResult` from the import line. Keep `validate_generate_result` unchanged.

- [ ] Task 12: Update `src/circuitry/adapters/factory.py`
  - File: `src/circuitry/adapters/factory.py`
  - Action: Remove `from .comfyui import ComfyUIAdapter`. Remove the `if adapter_name == "comfyui":` block. Remove `"comfyui"` from `SUPPORTED_ADAPTERS`.

- [ ] Task 13: Update `src/circuitry/adapters/__init__.py`
  - File: `src/circuitry/adapters/__init__.py`
  - Action: Remove imports of `ComfyUIAdapter`, `ImageAdapter`, `ImageResult`. Remove `validate_image_result` import from `conformance`. Remove all four from `__all__`.

- [ ] Task 14: Delete `src/circuitry/adapters/comfyui.py`
  - File: `src/circuitry/adapters/comfyui.py`
  - Action: Delete the file. Functionality has moved to `src/circuitry/plugins/comfyui.py`.

- [ ] Task 15: Update `src/circuitry/cli/runtime_shim.py`
  - File: `src/circuitry/cli/runtime_shim.py`
  - Action:
    1. Add `_has_prompt_effects(node: Any) -> bool` helper after the imports. Recursively walks any `DynamicDefinition`, `LoopDefinition`, `ConditionalDefinition`, `ReflectorDefinition` and returns `True` if any `PromptDefinition` is found. Use `isinstance` checks with deferred imports at the top of the function.
    2. Add inline `@dataclass(frozen=True) class _NoOpAdapter` with `name: str = "none"` and `generate()` raising `RuntimeError("No LLM adapter configured for this orchestration — add 'adapter' and 'model' fields if you need prompt effects.")`.
    3. In `run()`, after `root_def = compile_orchestration(...)`:
       - `if _has_prompt_effects(root_def): resolved_adapter, resolved_model = _require_resolved_settings(...); adapter = build_adapter(...)`
       - `else: adapter = _NoOpAdapter(); resolved_model = ""`
    4. Pass `adapter` and `resolved_model` to `DynamicRuntime` as before.

- [ ] Task 16: Update `src/circuitry/schema/orchestration.schema.json`
  - File: `src/circuitry/schema/orchestration.schema.json`
  - Action:
    1. Add `"ToolEffect"` to `$defs`:
       ```json
       "ToolEffect": {
         "type": "object",
         "required": ["type", "name", "provider"],
         "description": "Executes an external tool plugin. Does not invoke an LLM.",
         "properties": {
           "type": { "const": "tool" },
           "name": { "$ref": "#/$defs/NamePattern" },
           "provider": { "type": "string", "description": "Plugin name: ffmpeg, comfyui, etc." },
           "description": { "type": "string" },
           "params": { "type": "object", "description": "Plugin params. String values are Mustache-rendered." },
           "timeout_ms": { "type": "integer", "minimum": 0 },
           "on_error": { "$ref": "#/$defs/OnErrorDefault" }
         }
       }
       ```
    2. Extend `EffectDef` discriminated union: after the `"reflector"` branch, add a `"tool"` branch before the final `else` fallthrough. Also add `"tool"` to the final `else` enum list.
    3. In `PromptEffect`: remove `"image"` from `prompt_type` enum. Remove `image_output`, `image_dir`, and `prompt` properties. Remove the `oneOf` condition requiring `prompt` for `prompt_type: image`.

- [ ] Task 17: Migrate `orchestrations/_image.yml`
  - File: `orchestrations/_image.yml`
  - Action: Rewrite to:
    ```yaml
    name: _image
    description: Smoke test for image generation via ComfyUI

    effects:
      - name: generate_image
        type: tool
        provider: comfyui
        params:
          prompt: "a red apple on a wooden table, photorealistic"
          model: flux1-dev-fp8.safetensors
          image_output: path
          image_dir: ./output/images
          steps: 20
          cfg: 3.5
          sampler_name: euler
          scheduler: beta
          width: 512
          height: 512
    ```
  - Notes: Remove `adapter:` and `model:` top-level fields — no LLM calls, no adapter needed.

- [ ] Task 18: Migrate `orchestrations/comic_strip.yml`
  - File: `orchestrations/comic_strip.yml`
  - Action: Keep top-level `adapter: ollama` and `model: llama3:latest` (still needed for `generate_script` prompt). Replace each `type: prompt, prompt_type: image, provider: comfyui:flux1-dev-fp8.safetensors` block with `type: tool, provider: comfyui`. Move all image params (`steps`, `cfg`, `sampler_name`, `scheduler`, `width`, `height`) under `params:`. Add `model: flux1-dev-fp8.safetensors` to `params:`. Move the `prompt:` field into `params.prompt:`. Remove `image_output` and `image_dir` top-level fields on the effect; move them under `params:`.
  - Notes: The `prompt:` value references `{{prime.generate_script.value.panel_N_scene}}` — keep as-is under `params.prompt`.

- [ ] Task 19: Update `docs/orchestration-reference.md`
  - File: `docs/orchestration-reference.md`
  - Action:
    1. In `### prompt` field table: remove `image` from `prompt_type` enum values; remove `image_output`, `image_dir`, `prompt` rows.
    2. Add `### tool` section after `### reflector`. Include field table (`type`, `name`, `provider`, `params`, `timeout_ms`, `on_error`, `description`) and two examples: ffmpeg (two-step: prompt generates flags → tool executes) and comfyui (tool-only image generation).
    3. In **File Structure** table: add note to `adapter` row — "Optional when orchestration has no `prompt` effects."
    4. In **LLM Authoring Rules**:
       - Rule 4: add `tool` to valid `type` values.
       - Rule 5: remove `image` from valid `prompt_type` values.
       - Add Rule 16: "`tool`: requires `name` and `provider` (string). All string values in `params` are Mustache-rendered against state. Use `{{prime.<name>.value}}` in params strings to reference prior effect outputs."

- [ ] Task 20: Write tests
  - Files: `tests/plugins/test_ffmpeg.py`, `tests/plugins/test_comfyui_plugin.py`, `tests/core/test_tool_effect.py`
  - Action: See Testing Strategy section for per-file test cases.
  - Also update `tests/adapters/test_conformance.py`: remove any `validate_image_result` or `ImageResult` references.
  - Also update `tests/cli/test_validate.py`: add tests for `type: tool` schema acceptance and rejection of missing `provider`.

### Acceptance Criteria

- [ ] AC 1: Given a valid `type: tool, provider: ffmpeg` effect with `params.input`, `params.output`, and `params.flags`, when the orchestration runs, then ffmpeg is called as `ffmpeg -y -i <input> <flags> <output>`, `prime.<name>.value` is the output path, and `prime.<name>.meta` contains `stdout`, `stderr`, and `exit_code: 0`.

- [ ] AC 2: Given `params.flags` contains shell metacharacters (e.g. `; rm -rf /`), when `ToolRuntime.execute()` is called, then a `ValueError` is raised before any subprocess is spawned and `prime.<name>.meta.error` records the message.

- [ ] AC 3: Given a `type: tool, provider: comfyui` effect with `params.prompt` and `params.model`, when the orchestration runs, then ComfyUI is queued via its REST API, the generated image is saved to `params.image_dir`, `prime.<name>.value` is the file path, and `prime.<name>.meta.exit_code` is `None` (HTTP-based, not subprocess).

- [ ] AC 4: Given `type: prompt` with `prompt_type: image` in a YAML file, when `circuitry validate` is run, then it returns an error (schema rejects `image` as a valid `prompt_type`).

- [ ] AC 5: Given a `type: tool` effect is compiled, when `_compile_effect` processes it, then it returns a `ToolDefinition` instance (not `PromptDefinition`), and the `DynamicRuntime` dispatches it to `ToolRuntime`.

- [ ] AC 6: Given an orchestration with only `type: tool` effects and no `adapter:` field, when `run()` is called, then it succeeds without raising a missing-adapter error and the tool effects execute normally.

- [ ] AC 7: Given an orchestration with both `type: prompt` and `type: tool` effects and no `adapter:` field, when `run()` is called, then it raises a clear error about the missing adapter (prompt effect requires it).

- [ ] AC 8: Given `params` contains `"{{prime.generate_flags.value}}"` as a string value, when `ToolRuntime` renders the params, then the Mustache reference is resolved against the current store state before being passed to the plugin.

- [ ] AC 9: Given `type: tool, on_error: skip` and the plugin raises an exception, when `ToolRuntime.execute()` completes, then `prime.<name>.value` is `None`, `prime.<name>.meta.error` contains the error message, and execution continues to the next effect.

- [ ] AC 10: Given `circuitry validate` is run against the migrated `orchestrations/_image.yml`, then it returns `ok: true` with no errors.

## Additional Context

### Dependencies

No new package dependencies:
- `ffmpeg` is a system binary invoked via `subprocess`
- `chevron` (already installed) handles Mustache rendering
- `shlex` is stdlib
- `subprocess` is stdlib

### Testing Strategy

**`tests/plugins/test_ffmpeg.py`:**
- `test_ffmpeg_happy_path` — monkeypatch `subprocess.run` to return exit 0; assert `ToolResult.value` == output path, `exit_code == 0`
- `test_ffmpeg_nonzero_exit_raises` — monkeypatch to return exit 1; assert `RuntimeError` raised with exit code in message
- `test_ffmpeg_shell_injection_rejected` — pass `flags: "| rm -rf /"`, assert `ValueError` before subprocess is called
- `test_ffmpeg_missing_input_raises` — omit `params["input"]`, assert `ValueError`
- `test_ffmpeg_missing_output_raises` — omit `params["output"]`, assert `ValueError`
- `test_ffmpeg_y_flag_always_injected` — capture cmd passed to subprocess.run; assert `"-y"` is second element

**`tests/plugins/test_comfyui_plugin.py`:**
- `test_comfyui_execute_path_output` — monkeypatch `subprocess.run` for curl calls (queue + history + view); assert `ToolResult.value` is a path string ending in `.png`
- `test_comfyui_execute_base64_output` — same with `params["image_output"] = "base64"`; assert value is a base64 string
- `test_comfyui_missing_prompt_raises` — omit `params["prompt"]`; assert `ValueError` or `RuntimeError`
- `test_comfyui_missing_model_raises` — omit `params["model"]` and leave `default_model` empty; assert `RuntimeError`

**`tests/core/test_tool_effect.py`:**
- `test_tool_runtime_renders_params` — provide `params: {flags: "{{my_flag}}"}`, `ctx: {my_flag: "-vf scale=1280:720"}`; monkeypatch plugin factory; assert rendered param passed to plugin
- `test_tool_runtime_writes_value_and_meta` — run ToolRuntime with fake plugin; assert `store["encode"]["value"] == "out.mp4"` and `store["encode"]["meta"]["exit_code"] == 0`
- `test_tool_runtime_on_error_skip` — fake plugin raises; `on_error="skip"`; assert value is None, no exception propagated
- `test_tool_runtime_dry_run` — `dry_run=True`; assert plugin is never called and value is None

**`tests/cli/test_validate.py` additions:**
- `test_validate_accepts_valid_tool_effect` — YAML with `type: tool, name: x, provider: ffmpeg, params: {}` → `ok: True`
- `test_validate_rejects_tool_missing_provider` — YAML with `type: tool, name: x` (no provider) → `ok: False`
- `test_validate_rejects_prompt_type_image` — YAML with `type: prompt, name: x, template: hi, prompt_type: image` → `ok: False`

### Notes

- `orchestrations/_image.yml` can drop the `adapter:` field entirely after migration — `_require_resolved_settings` will be conditional on `_has_prompt_effects(root_def)`
- `comic_strip.yml` keeps `adapter: ollama` because `generate_script` is a prompt effect
- `prompt_type: image` in `_compile_prompt` should raise `ValueError("prompt_type: image is no longer supported. Use type: tool, provider: comfyui instead.")` after removal
- `_effect_type_label()` in `dynamic.py` needs updating to return `"tool"` for `ToolDefinition`
- `docs/orchestration-reference.md` changes required:
  - Remove `image` from `prompt_type` enum in prompt field table
  - Add new `### tool` effect section (fields: `type`, `name`, `provider`, `params`, `timeout_ms`, `on_error`, `description`; example with ffmpeg and comfyui)
  - Update File Structure table: note `adapter` is optional when orchestration has no `prompt` effects
  - Update LLM Authoring Rules: Rule 4 add `tool`; Rule 5 remove `image` from prompt_type; add Rule 16: `tool` requires `name` and `provider`; `params` values are Mustache-rendered against state
  - Add `tool` pattern example to Patterns section showing two-step: prompt generates flags → tool executes ffmpeg
- Task ordering matters: Tasks 1–5 (plugin foundation) must complete before Task 6 (ToolRuntime imports from plugins). Tasks 6–8 (core) must complete before Task 15 (runtime_shim imports from core). Tasks 10–14 (adapter cleanup) can run in parallel with Tasks 1–8.
