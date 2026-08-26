# Documentation map

Start with the [README](../README.md) for install, first run, and the mental model. Then:

## The language

- [**The Guidebook**](./guidebook/README.md) — the long-form tour, fourteen chapters in three acts, one running example. Also built as [PDF](./guidebook/circuitry-guidebook.pdf) and [EPUB](./guidebook/circuitry-guidebook.epub).
  - Part I — Basics: [Prompt](./guidebook/01-prompt.md) · [Dynamic](./guidebook/02-dynamic.md) · [State](./guidebook/03-state.md) · [Configuration](./guidebook/04-configuration.md) · [Errors](./guidebook/05-errors.md)
  - Part II — Cybernetics: [If](./guidebook/06-if.md) · [Loop](./guidebook/07-loop.md) · [Reflector](./guidebook/08-reflector.md)
  - Part III — The machine in the world: [Composition](./guidebook/09-composition.md) · [Complexity](./guidebook/10-complexity.md) · [Decomposition](./guidebook/11-decomposition.md) · [Surfaces](./guidebook/12-surfaces.md) · [Tools and persistence](./guidebook/13-tools-and-persistence.md) · [The whole meal](./guidebook/14-the-whole-meal.md)
- [Orchestration Reference](./orchestration-reference.md) — every field of every effect, state path addressing, patterns and antipatterns, and the LLM authoring rules.
- [Grammar](./guidebook/grammar.md) — the formal grammar, derived from `orchestration.schema.json`.
- [Troubleshooting State Paths](./troubleshooting-state-paths.md) — the workflow for a run that diverged, and the symptom table for loop paths.

## Running orchestrations

- [Named Profiles](./profiles.md) — per-run overlays: defaults, inputs, per-effect model/provider/enabled/routing, persistence.
- [Complexity Configuration](./complexity-config.md) — `runtime.complexity`: the scoring, routing, and decomposition switches, their defaults, precedence, and errors.
- [Routing](./routing.md) — signals, band tables, the full model precedence ladder, a worked example, and what the scorer cannot see.
- [`cof score`](./score-command.md) — the static per-effect complexity preview.
- [Terminal UI](./tui.md) — `cof tui`: keymap and every view.
- [The Wizard](./wizard.md) — building orchestrations by talking; the turn contract and headless driving.
- [Library Sources](./library-sources.md) — `runtime.library.sources`: curation, folder, and GitHub sources behind `cof list/info/run/eject`.
- [Shared Library](./shared-library.md) · [Contributions](./shared-library-contributions.md) · [Growth](./shared-library-growth.md) — the publish-by-PR shared library and `cof fetch` / `cof run-library`.
- [CyberDiner Demo Runbook](./cyberdiner-demo-runbook.md) — a job-queue broker adapter, end to end.

## Extending

- [API Reference](./api-reference.md) — the public Python surface.
- [Plugin Extension Guide](./plugins.md) — runtime plugin hooks and registration; [ffmpeg](./plugins/ffmpeg.md) and [ComfyUI](./plugins/comfyui.md) tool plugin parameters.
- [Adapter Conformance](./adapter-conformance.md) — the adapter contract and how to validate a new provider.
- [Postgres Persistence](./postgres-persistence.md) — the persistence backends in production.
- [Editor Highlighting](./editor-highlighting.md) — the VS Code grammar under `editor/`.

## Project

- [Architecture](./architecture.md) — runtime flow and the code paths behind it.
- [Stability & Versioning Policy](./stability.md) — what counts as public API and the semver rules for `0.x`.
- [Threat Model](./threat-model.md) — attack surfaces, mitigations, allowlists, and known limitations.
- [Testing Policy](./testing-policy.md) · [Test Matrix](./test-matrix.md) — what every change must cover.
- [Contributing](../CONTRIBUTING.md) · [Releasing](../RELEASING.md) · [Security](../SECURITY.md) · [Changelog](../CHANGELOG.md)

When adding a link here, append it at the end of its section rather than mid-list — parallel PRs then merge without conflicts (see [Contributing](../CONTRIBUTING.md#changelog-fragments)).
