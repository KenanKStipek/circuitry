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

- **Tech Stack:** Python, Typer, Rich, PyYAML, Chevron, Ollama/OpenAI/Anthropic/LiteLLM adapters
- **Entry Point:** `scripts/circuitry` and `python -m circuitry.cli.app`
- **Architecture Pattern:** deterministic orchestration runtime with explicit effect control flow

## Generated Documentation

- [Project Overview](./project-overview.md)
- [Architecture](./architecture.md)
- [API Reference](./api-reference.md)
- [Source Tree Analysis](./source-tree-analysis.md)
- [Development Guide](./development-guide.md)
- [Testing Policy](./testing-policy.md)
- [Test Matrix](./test-matrix.md)
- [Adapter Conformance](./adapter-conformance.md)
- [CyberDiner Demo Runbook](./cyberdiner-demo-runbook.md)
- [Postgres Persistence](./postgres-persistence.md)
- [Plugin Extensions](./plugins.md)
- [Shared Library Retrieval](./shared-library.md)
- [Shared Library Contributions](./shared-library-contributions.md)
- [Shared Library Growth](./shared-library-growth.md)
- [Editor Highlighting](./editor-highlighting.md)
- [Perceptron Boundary](./perceptron-boundary.md)
- [Troubleshooting State Paths](./troubleshooting-state-paths.md)
- [Project Scan Sections](./project-scan-sections.md)

## Existing Domain Documentation

- [Circuitry](./Circuitry 2c34435ec2e080c89fc0f253880c2612.md)
- [Circuitry Terminology](./Circuitry Terminology 2c94435ec2e0808daeeff76f7ed1ed25.md)
- [Circuitry Type System](./Circuitry Type System 2f34435ec2e0808394e7ddbb86d14a89.md)
- [Conditional Cybernetic](./Conditional Cybernetic 2f34435ec2e08022864fd77c7f2d5b20.md)
- [Dynamic](./Dynamic 2c94435ec2e0809d87eff08463c9ca97.md)
- [Loop Cybernetic](./Loop Cybernetic 2f34435ec2e080ea9024ca18becd5df1.md)
- [Prompt](./Prompt 2c94435ec2e080feb508d739b3272408.md)
- [Reflector](./Reflector 2c94435ec2e080468aa4ebb005c4c635.md)
- [Example Orchestration](./Example Orchestration 2f34435ec2e080ba995dcc11dd09047c.md)

## Getting Started

1. Read `project-overview.md` for scope and boundaries.
2. Read `architecture.md` for runtime/component flow.
3. Use `source-tree-analysis.md` to navigate implementation paths.
4. Use `development-guide.md` for setup and local verification commands.
5. For planning or brownfield PRD work, use this index as the primary input document.
