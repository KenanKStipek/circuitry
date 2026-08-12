# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html) — with the alpha caveat documented in [`docs/stability.md`](docs/stability.md) (`0.x` minor bumps may include breaking changes).

## [Unreleased]

### Added

- **`agents/wizard` — the orchestration that writes orchestrations.** A conversational agent that handles ONE turn of an interview (`interpret → ask-or-draft → validate → revise → gate`) and hands back a structured turn contract: `say` (what to show the human), `yaml` (the validated draft, `null` on a question turn), and `done`. The host drives the outer loop by re-running it with the accumulated `goal`/`conversation`/`draft` state — see [`docs/wizard.md`](docs/wizard.md) for the headless `api.run_orchestration` example and `scripts/wizard-chat` for a runnable driver. `done` is a deterministic gate (CEL + tool), not a model claim: it is true only when the model says it is finished *and* the draft passed validation, so the wizard cannot finish on invalid YAML.
- **`WIZARD_PRIME_V1`** in `core/primes.py` — a DSL cheat-sheet prime (seven primitives, naming rules, interface blocks, state paths, prompt types) with the same no-fences/no-markdown emission discipline as `REFLECTOR_PRIME_V1`.
- **`validate_yaml` tool provider — schema + compiler validation with no LLM.** Validates an orchestration document against `orchestration.schema.json` and then compiles it to catch what the schema cannot (duplicate sibling names, reserved `iter_N` names, unknown effect types). Returns `{ok, errors, yaml}`; an empty document is `ok: false` rather than an exception, errors are capped (`max_errors`, default 20) so they fit in a revision prompt, and `strip_fences` (default `true`) tolerates fenced model output. The echoed `yaml` is the cleaned text that was actually validated.

### Fixed

- **`tool` and `use` effects inside conditional branches, and `use` inside loop bodies, were silently skipped.** `ConditionalRuntime`'s dispatch chain handled `prompt`/`dynamic`/`if`/`loop`/`reflector` and then fell through: a `tool` effect in a branch wrote nothing to the store, was recorded as executed, and the run reported success. `LoopRuntime` had the same gap for `use`. Both now dispatch the full effect set and raise on an unhandled type, matching `DynamicRuntime`.

- **`mcp` tool provider — call tools on external MCP servers.** Orchestrations can now invoke tools on any Model Context Protocol server (the client-side complement to `circuitry-mcp`). Servers are declared under `runtime.plugins.mcp.servers` with stdio (`command`/`args`/`env`) or streamable-HTTP (`url`/`headers`) transports; tool effects reference them by name (`params: {server, tool, arguments}`). `operation: list_tools` returns a server's tool catalog for runtime capability discovery. Server-side tool errors surface via `stderr`/`exit_code` rather than raising, matching the `http` plugin's policy. Credential fields are covered by the existing serialization redaction; `check()` reports missing stdio binaries at preflight.
- **`use` effect — `ref:` and `path:` fields.** `ref:` is a curation library lookup (slash-delimited, e.g. `utilities/critique`); `path:` is a filesystem path (absolute, cwd-relative, or parent-orchestration-relative).
- **`use` effect — opt-in full-namespace mode.** When neither `outputs:` nor a child `interface:` declares outputs, the entire child `prime` subtree is exposed at `prime.<use_name>.<child_effect>.value` (matches `dynamic` namespacing). The legacy `value: True` sentinel is removed in this mode.
- **Cycle detection** for `use` references, both at runtime (call-stack tracking by resolved path) and statically inside `validate()` (DFS over the static `ref:`/`path:` graph). Cycles are reported with the full path: `Cycle: A → B → C → A`.
- **Curation library** at `src/circuitry/curation/`, organised by category: `learn/`, `utilities/`, `patterns/`, `recipes/`, `agents/`. Replaces the flat `bundled/orchestrations/` and top-level `orchestrations/` mirror.
- **Consolidated `curation/manifest.json`** — single source of truth for `cof list/run/info/eject` AND per-entry documentation (intent, when_to_use, inputs, outputs, primitives, tags, difficulty). Replaces the separate `index.yml` + `manifest.json`.
- **`curation-manifest.schema.json`** — Draft-07 JSON Schema for the manifest, enforced by `tests/orchestrations/test_curation_metadata.py`.

### Changed

- **`cof list / run / info / eject` use slash-delimited names.** `cof run hello` is now `cof run learn/hello`; `cof run article-summarizer` is now `cof run recipes/article_summarizer`. Bare last-segment names still resolve when unambiguous.
- **Test isolation in `use`** — child effects now land at `prime.<use_name>.<child_effect>.value` by default (full-namespace mode). Existing tests/orchestrations that declared `outputs:` or used a child `interface:` are unaffected.

### Deprecated

- **`use` effect — `orchestration:` field.** Compiles with a `DeprecationWarning`; will be removed in a future release. Migrate to `ref:` (for curation library entries) or `path:` (for filesystem paths).

### Removed

- `src/circuitry/bundled/orchestrations/` — content moved to `src/circuitry/curation/<category>/` and renamed (e.g. `_prompt.yml` → `learn/prompt.yml`, `orchestration_improver.yml` → `agents/improver.yml`).
- Top-level `orchestrations/` mirror — was kept in sync with `bundled/orchestrations/` by `scripts/sync-bundled`. Both removed.
- `scripts/sync-bundled` — obsolete now that `curation/` is the single source of truth (referenced directly via `pyproject.toml` package-data).
- `_image.yml` — the legacy `prompt_type: image` example. The compiler has rejected `prompt_type: image` since the tool plugin migration; use `tool` with `provider: comfyui`.

## [0.1.0] — 2026-05-08

First public release.

### Added

- **Cybernetic orchestration runtime.** Effects (`prompt`, `dynamic`, `loop`, `if`, `reflector`, `use`, `tool`) compose via deterministic state paths. State observed by interpolation enables genuine feedback chains.
- **`cof` CLI** with `run`, `check`, `validate`, `inspect`, `list`, `info`, `eject`, `gen`, `setup`, `doctor`, `init`, `version`, `fetch`, `run-library` subcommands.
- **Bundled orchestrations** — `hello`, `article-summarizer`, `comic-strip`, `meta-orchestrator`, `orchestration-improver`, `orchestration-improver-judge`, plus per-primitive templates.
- **Adapters** — `OllamaAdapter`, `OpenAIAdapter`, `AnthropicAdapter`, `LiteLLMAdapter`, behind a `Adapter` Protocol with `build_adapter` factory.
- **Tool plugins** — `ffmpeg` (drawtext, filter_complex, panel composition), `comfyui` (txt2img + img2img with reference image upload), behind a `ToolPlugin` Protocol with `build_plugin` factory.
- **JSON Schema validation** of all orchestration YAML at compile time, with explicit opt-out for `use(inline:)` (default is enforced validation).
- **Layered configuration** — CLI flags > env vars > project `circuitry.config.json` > global `~/.config/circuitry/config.json` > defaults.
- **`--last` replay** — `cof run --last` re-runs the most recent orchestration with the same arguments, sourced from `~/.config/circuitry/last-run.json`.
- **Live state streaming** — `--live-state` writes state atomically after each effect for external monitoring.
- **Programmatic API** — `run_orchestration`, `run_shared_orchestration`, `validate_orchestration`, `inspect_orchestration`, `inspect_divergence_paths`, `CircuitryExecutionError` exported from `circuitry.__init__`.
- **Stability commitments** — `docs/stability.md` enumerates the public API surface (Python re-exports, `cof` CLI flags, JSON Schema, state path conventions) and the deprecation policy.
- **Threat model** — `docs/threat-model.md` documents inline-orchestration validation, plugin sandboxing, credential handling, telemetry stance ("none"), and explicit non-goals.
- **Secret redaction** — credential-bearing fields in `runtime.effective_settings` and `~/.config/circuitry/last-run.json` are redacted via a centralized helper before serialization.
- **Community files** — `LICENSE` (MIT), `CODE_OF_CONDUCT.md`, `SECURITY.md`, `CONTRIBUTING.md`, GitHub issue and PR templates.
- **CI** — pytest with coverage, ruff, and mypy across Python 3.9–3.13 on every push and pull request, plus an offline smoke job that validates every bundled orchestration.

### Notes

- The PyPI distribution is published as `circuitry-cof` because the `circuitry` name was already taken. The Python import name remains `circuitry` (`from circuitry import run_orchestration`). Install with `pipx install circuitry-cof` once the first release is published, or via the install script for the time being.
- Circuitry collects no telemetry. See [SECURITY.md](SECURITY.md).

[Unreleased]: https://github.com/kenankstipek/circuitry/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/kenankstipek/circuitry/releases/tag/v0.1.0
