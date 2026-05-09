# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html) — with the alpha caveat documented in [`docs/stability.md`](docs/stability.md) (`0.x` minor bumps may include breaking changes).

## [Unreleased]

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
