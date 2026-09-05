# Project Documentation Index

Last Updated: 2026-05-08

## Public Debut Docs

- [Product Requirements (v0.1.0)](./prd.md) — original product brief and MVP scope
- [Stability & Versioning Policy](./stability.md) — what counts as public API and the deprecation/semver rules
- [Threat Model](./threat-model.md) — attack surfaces, mitigations, and known limitations

## Project Overview

- **Type:** monolith (single-part)
- **Primary Language:** Python
- **Architecture:** layered runtime library (CLI + core runtime + adapter boundary)

## Quick Reference

- **Tech Stack:** Python, Typer, Rich, PyYAML, Chevron, Ollama/OpenAI/Anthropic/LiteLLM/CyberDiner adapters
- **Entry Point:** `cof` and `python -m circuitry.cli.app`
- **Architecture Pattern:** deterministic orchestration runtime with explicit effect control flow

## Generated Documentation

- [Architecture](./architecture.md)
- [API Reference](./api-reference.md)
- [Testing Policy](./testing-policy.md)
- [Test Matrix](./test-matrix.md)
- [Adapter Conformance](./adapter-conformance.md)
- [CyberDiner Demo Runbook](./cyberdiner-demo-runbook.md)
- [Postgres Persistence](./postgres-persistence.md)
- [Plugin Extensions](./plugins.md)
- [The Wizard](./wizard.md) — building orchestrations by talking; the turn contract and how to drive it headlessly
- [Library Sources](./library-sources.md) — `runtime.library.sources`: curation + folder sources behind `cof list/info/run/eject`
- [Named Profiles](./profiles.md)
- [Shared Library Retrieval](./shared-library.md)
- [Shared Library Contributions](./shared-library-contributions.md)
- [Shared Library Growth](./shared-library-growth.md)
- [Terminal UI](./tui.md) — app chrome (keymap, help overlay, resize breakpoints, TUI-mode logging) and the Library, Run, Runs, Doctor, Settings, Validate and Chat views
- [Editor Highlighting](./editor-highlighting.md)
- [Troubleshooting State Paths](./troubleshooting-state-paths.md)
- [Complexity Configuration](./complexity-config.md) — `runtime.complexity`: the scoring/routing/decomposition switches, their defaults, and precedence
- [`cof score`](./score-command.md) — the static per-effect complexity preview: dotted paths, unscoreable effects, and the `--json` payload
- [Routing](./routing.md) — signals, band tables, the full model precedence ladder, a worked example, and what the scorer honestly cannot see

## Getting Started

1. Read `architecture.md` for runtime/component flow.
2. For planning or brownfield PRD work, use this index as the primary input document.
