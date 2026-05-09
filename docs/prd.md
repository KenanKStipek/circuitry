# Product Requirements Document — Circuitry

> **Historical artifact.** This is the v0.1.0 product brief that scoped the
> MVP. It is preserved here for context and credibility; the live source of
> truth for current capabilities is the [README](../README.md), the
> [stability commitments](stability.md), and the [`CHANGELOG`](../CHANGELOG.md).

**Author:** Kenan Stipek
**Date:** 2026-02-18
**Status:** MVP delivered 2026-03-02 — see "MVP Completion Status" below.

---

## Executive Summary

Circuitry is a brownfield **Cybernetic Orchestration Framework** for AI systems, implemented as a developer-focused shared library with CLI/runtime surfaces. It provides a simple DOL for defining orchestrations that can scale into complex dynamic execution patterns while remaining deterministic in structure and state access.

The core product objective is two-phase:
1. stabilize the current runtime and architecture into a reliable foundation, and
2. extend the framework with broader adapters, plugin integrations, and reusable shared-library orchestrations for production service use.

Circuitry is designed for teams building AI-enabled services that need explicit control flow, persistent state-aware execution, and predictable composability across prompts, dynamics, conditionals, loops, and reflector-driven planning.

### What Makes This Special

Circuitry's differentiator is the combination of:
- **cybernetic primitives** (especially loops and conditionals) for real-time dynamic control flow
- **functional/monadic core abstractions** (Prompt and Dynamic treated as monadic execution units)
- **deterministic orchestration-to-state-schema mapping**, where orchestration structure maps to known state keys/paths for reliable downstream retrieval and reuse
- **dynamic orchestration authoring and shared-library retrieval**
- **multi-adapter and tool/plugin connectivity** to heterogeneous upstream LLM/AI ecosystems

This creates a higher-leverage orchestration model than static workflow tools: orchestrations are not only executable artifacts but reusable, composable, and persistable operational assets.

## MVP Completion Status

**Completed:** 2026-03-02
**Sprint:** 6 epics, 24 stories — all done
**Test Suite:** 151 tests passing, CI green (pytest, ruff, mypy)

All MVP functional requirements (FR1–FR40) are implemented across Epics 1–6. The codebase has passed adversarial code review with all findings resolved.

### Code Quality Actions Completed During Review

- Relocated test plugin fixtures from `src/` to `tests/` (proper test isolation)
- Renamed `core/plugins.py` → `core/runtime_plugins.py` (disambiguate from tool plugin system)
- Verified CI workflow (`.github/workflows/quality.yml`) covers pytest, ruff, and mypy

### Post-MVP Backlog

The following items were identified during MVP review and deferred for post-MVP work:

1. **Postgres integration test with real DB** — Story 4-3 persistence layer is implemented with SQLite integration tests gated behind `CIRCUITRY_RUN_INTEGRATION=1`. A Postgres-specific integration test against a real database should be added when a CI-accessible Postgres instance is available.
2. **Shared library CI enforcement** — Story 5-4 defines the contribution workflow for the shared orchestration library. Once the external library repository exists, add CI validation (linting, schema checks) to that repo.
3. **Perceptron real-time state GUI** — FR40 preserves the roadmap boundary. Implementation is Phase 2 scope.

## Project Classification

- **Project Type:** developer_tool
- **Positioning:** Cybernetic Orchestration Framework
- **Domain:** general
- **Complexity:** medium
- **Project Context:** brownfield


## Success Criteria

### User Success

- **Primary user type 1:** Developers building products that use orchestrations and run them in Circuitry runtime.
- **Primary user type 2:** Operators/users running consistent periodic orchestrations (scheduled automation).
- **Success moments:**
  - A developer can trigger an orchestration from a REST API endpoint.
  - A user can schedule a daily orchestration (for example, document summarization) via cron-based execution.
- **Time-to-value targets:**
  - New user can run a built-in example immediately via CLI.
  - Developer can import Circuitry as a library and define orchestration logic directly with Prompt and Dynamic objects without custom scaffolding.

### Business Success

- **3-month targets (launch validation):**
  - Project is public.
  - More than 2 external users have executed orchestrations successfully.
- **12-month targets (market validation):**
  - 1000+ GitHub stars.
  - "Thousands" of users running orchestrations (operationalized as strong adoption at scale).
  - Multiple continuously running orchestrations in your own production usage.
- **Primary business priority:** production usage depth over shallow adoption.

### Technical Success

- Orchestration execution supports all defined execution patterns reliably.
- Test coverage explicitly validates each execution pattern and cybernetic primitive path.
- Runtime supports:
  - plugins
  - multiple adapters
  - shared-library orchestration fetch/use
  - shared-library orchestration creation/extension
- MVP integration set includes:
  - at least a couple upstream LLM providers
  - LiteLLM integration
  - Postgres persistence tool integration
- Perceptron real-time state GUI is planned as a post-MVP growth capability.

### Measurable Outcomes

- Built-in CLI examples execute successfully end-to-end for new users.
- REST-triggered orchestration flow is demonstrably working.
- Scheduled daily orchestration flow is demonstrably working.
- Cross-pattern orchestration test suite exists and passes.
- Public launch and minimum early-user adoption milestone achieved (3-month target).
- 12-month adoption/community targets tracked against GitHub stars and active orchestration usage.

## Product Scope

### MVP - Minimum Viable Product (DELIVERED 2026-03-02)

All MVP capabilities below are implemented and tested:

- Stable runtime baseline for all defined orchestration execution modes.
- Comprehensive tests covering all orchestration primitives and execution paths (151 tests).
- CLI-first quick start where examples run immediately.
- Library usage path where developers can author orchestrations directly with Prompt and Dynamic abstractions.
- REST endpoint trigger capability for orchestrations.
- Cron-based periodic orchestration execution.
- Initial adapter/tooling stack:
  - multiple upstream LLMs (Ollama, OpenAI-compatible)
  - LiteLLM integration
  - Postgres persistence tooling
- Tool effect system with plugin architecture (ffmpeg, ComfyUI providers).
- Runtime plugin lifecycle hooks (on_run_start, on_run_success, on_run_failure).
- JSON Schema validation for orchestration YAML (Draft-07).
- CI pipeline: pytest, ruff, mypy via GitHub Actions.

**Deferred to post-MVP:** Perceptron real-time state GUI (see Post-MVP Features).

### Growth Features (Post-MVP)

**Phase 2 — Near-term:**
- Perceptron real-time state GUI for orchestration execution visualization.
- Postgres integration testing against a real database in CI.
- Shared library external repository with CI enforcement (linting, schema checks).
- Community adds orchestrations to shared library.
- GitHub submission pipeline for orchestration contribution.

**Phase 2 — Mid-term:**
- System begins "building itself" by running orchestrations that process and operationalize submitted orchestrations.
- Additional tool effect plugins beyond ffmpeg and ComfyUI.
- Reliability automation and stronger operator controls.

### Vision (Future)

- Broad expansion of adapters, tools, and plugins.
- Ongoing iterations to improve orchestration tooling and developer/operator UX.
- Larger ecosystem where orchestration assets are reusable, composable, and continuously evolving.


## User Journeys

### Journey 1: Primary User (Developer) — Success Path
**Persona:** Maya, backend engineer building an AI-enabled feature.

**Opening scene:** Maya owns a product feature that needs structured AI orchestration, not one-off prompts. She needs deterministic behavior, persistence, and a path from prototype to production.

**Rising action:**
- She installs Circuitry and runs a built-in CLI example immediately.
- She imports Circuitry into her service and defines orchestration logic directly using Prompt and Dynamic abstractions.
- She wires a REST endpoint that triggers orchestration execution.
- She configures persistence so downstream steps can reuse prior state with predictable keys.

**Climax:** The first production request hits her endpoint and the orchestration completes with expected control flow and state output, without hidden behavior.

**Resolution:** Maya now treats orchestrations as reusable product assets. Feature delivery accelerates because orchestration logic is explicit, testable, and portable across environments.

### Journey 2: Primary User (Developer) — Edge Case (Adapter Outage)
**Persona:** Maya again, now in incident mode.

**Opening scene:** A production orchestration path fails because the upstream adapter provider is unavailable.

**Rising action:**
- Triggered runs fail at model-invocation steps.
- Maya inspects runtime/state records to identify where execution halted.
- She switches adapter/provider strategy (or fallback path) and re-runs affected orchestration with preserved state context.

**Climax:** She recovers execution with minimal product downtime, proving outage handling is operationally manageable.

**Resolution:** Adapter outage playbooks become part of standard engineering practice: detection, fallback, replay, and validation.

### Journey 3: Operations User — Scheduled Automation
**Persona:** Noah, engineer/operator running periodic orchestrations for business workflows.

**Opening scene:** Noah needs daily automation (for example, summarizing incoming documents) with consistent outcomes.

**Rising action:**
- He defines a scheduled orchestration run (cron-triggered).
- Runtime executes daily, writing deterministic state and outputs to persistence.
- He monitors run history and outcome consistency over time.

**Climax:** Daily summaries are produced reliably with no manual intervention.

**Resolution:** Periodic orchestration becomes a trusted operational primitive, replacing ad hoc scripts and inconsistent manual routines.

### Journey 4: Admin/Ops User — Runtime Governance and Scaling
**Persona:** Priya, platform engineer managing orchestration infrastructure for teams.

**Opening scene:** Multiple teams begin adopting Circuitry, increasing adapter usage, plugin needs, and scheduling load.

**Rising action:**
- Priya governs runtime config, adapter availability, plugin lifecycle, and persistence integrations.
- She standardizes environment policies for safe orchestration execution and observability.
- She enables shared-library orchestration usage patterns that keep behavior consistent across services.

**Climax:** Teams can ship orchestration-backed features without each team reinventing execution infrastructure.

**Resolution:** Circuitry becomes managed platform capability rather than isolated team tooling.

### Journey 5: Support/Troubleshooting User — Incident Investigation
**Persona:** Kenan (current owner), acting as support and quality triage.

**Opening scene:** A user reports unexpected output from a long-running orchestration chain.

**Rising action:**
- Kenan inspects state progression and execution metadata at known keys/paths.
- He traces the specific step where behavior diverged and validates whether issue is prompt logic, control-flow condition, adapter response, or persistence artifact.
- He reproduces in CLI/example mode and validates fix via tests across affected execution patterns.

**Climax:** Root cause is isolated quickly because orchestration-to-state mapping is predictable and auditable.

**Resolution:** Fix is shipped with stronger tests and clearer troubleshooting guidance for future incidents.

### Journey 6: API/Integration User — Embedded Service Integration
**Persona:** Elena, integration engineer connecting multiple systems through orchestration.

**Opening scene:** Elena needs both direct library embedding and API-triggered orchestration across internal services.

**Rising action:**
- She embeds Circuitry in service code where orchestration is tightly coupled to product logic.
- She also integrates with REST-triggered orchestration paths for cross-service workflows.
- She leverages shared-library orchestrations to avoid duplicating orchestration definitions per service.

**Climax:** One orchestration model works across embedded and service-triggered integration styles.

**Resolution:** Integration complexity drops, and orchestration behavior remains uniform across systems.

### Journey Requirements Summary

These journeys reveal required capability areas:

- Immediate CLI onboarding and example execution.
- First-class developer embedding via Prompt/Dynamic abstractions.
- Composable Prompt and Dynamic units for building larger orchestration structures.
- Support for both chain-of-thought and tree-of-thought style step effects within declared control-flow semantics.
- Direct package embedding in other Python software (not limited to REST/service-triggered execution).
- REST-trigger orchestration interface (when service integration is needed).
- Cron/periodic orchestration scheduling.
- Adapter outage resilience and recovery patterns.
- Deterministic state-path observability for incident triage.
- Admin/platform governance for adapters/plugins/persistence at scale.
- Shared-library orchestration retrieval and reuse.
- Cross-pattern test coverage for runtime confidence.


## Innovation & Novel Patterns

### Detected Innovation Areas

- **Cybernetic orchestration paradigm:** Circuitry positions orchestration as a real-time cybernetic control system rather than a static prompt/workflow runner.
- **Composable monadic core:** Prompt and Dynamic abstractions operate as composable monadic units, enabling structured composition of increasingly complex orchestration behavior.
- **DSL-first execution model:** A simple DOL can express advanced orchestration structures, including chain-style and tree-style effect topologies.
- **Deterministic state-path architecture:** Orchestration structure maps predictably to state schema keys, making persisted outputs consistently addressable for downstream use.
- **Dual-mode operational model:** Circuitry is usable both as a Python-embedded package and as externally triggered runtime infrastructure (CLI/service/API/scheduled execution).
- **Shared-library orchestration evolution:** Orchestrations are reusable assets that can be authored/retrieved dynamically and expanded over time through community/system contribution loops.
- **Real-time observability innovation:** Perceptron concept adds engaging, real-time visibility into state transitions during orchestration execution.

### Market Context & Competitive Landscape

- Most current orchestration tooling emphasizes static pipelines, ad hoc chains, or vendor-specific flow builders.
- Circuitry's differentiation is the combined focus on:
  - explicit cybernetic control primitives,
  - deterministic state mapping for persistent reuse,
  - and orchestration-as-library assets rather than one-off workflow artifacts.
- Positioning opportunity: lead in "cybernetic orchestration framework" category for developers who need auditable, composable, production-grade orchestration control.

### Validation Approach

- Validate paradigm value through MVP proofs:
  - successful REST-triggered orchestration execution,
  - successful periodic scheduled orchestration execution,
  - successful embedded-package orchestration execution in Python services.
- Validate DSL/composability value through:
  - examples demonstrating Prompt/Dynamic composition in both simple and complex flows,
  - tests covering chain and tree execution semantics and state predictability.
- Validate ecosystem value through:
  - initial shared-library orchestration publish/retrieve workflows,
  - multi-adapter interoperability (including LiteLLM and multiple upstream providers),
  - Postgres persistence usage in real runs.
- Validate observability value via Perceptron prototype demonstrating meaningful real-time state transition visibility.

### Risk Mitigation

- **Risk: innovation complexity outruns stability**
  Mitigation: enforce "stabilize core first" sequencing before major expansion.
- **Risk: cybernetic positioning is unclear to market**
  Mitigation: explicit messaging and examples contrasting Circuitry vs static orchestration tools.
- **Risk: state-schema determinism breaks under feature growth**
  Mitigation: strict compatibility contracts and tests around state-path guarantees.
- **Risk: adapter/tool/plugin ecosystem becomes brittle**
  Mitigation: interface contracts, conformance tests, and outage/fallback playbooks.
- **Risk: shared-library growth lowers quality**
  Mitigation: submission standards, automated validation, and governance for orchestration contributions.


## Developer Tool Specific Requirements

### Project-Type Overview

Circuitry is a Python-first developer tool (Cybernetic Orchestration Framework) focused on composable orchestration authoring/execution with deterministic state mapping and runtime control primitives. The near-term product shape prioritizes Python ecosystem fit, production-grade runtime stability, and high-quality examples/documentation over broad language expansion.

### Technical Architecture Considerations

- **Language scope (MVP):** Python only.
- **Distribution model:** Python package distribution and install flow aligned with standard Python packaging workflows.
- **Runtime consumption modes:**
  - Embedded package usage in Python software.
  - CLI-driven orchestration execution for rapid onboarding and operational usage.
  - Service-triggered patterns (e.g., REST) as integration layer, not sole usage model.
- **Control-flow semantics:** support explicitly composed orchestration effects, including chain-style and tree-style structures, with predictable execution/state behavior.
- **State and persistence:** deterministic orchestration-to-state-path mapping remains a core architectural constraint to preserve downstream reusability and observability.

### Language Matrix

- **MVP Supported Languages:**
  - Python (official, first-class)
- **Non-MVP:**
  - No official SDK support for additional languages in this phase.

### Installation Methods

- Primary distribution via Python package installation.
- Support standard Python installation/development workflows for both:
  - end users running orchestrations
  - developers embedding Circuitry into existing Python software

### API Surface

- **Library API:** importable Python package exposing orchestration primitives and runtime interfaces.
- **CLI API:** built-in command-line entry points for running/validating/inspecting orchestrations.
- **Integration API target:** orchestration execution should be triggerable from application/service endpoints where needed.

### Code Examples

MVP examples should cover:
- Immediate CLI run path (time-to-first-success)
- Embedded Python package usage path
- Core orchestration primitives in realistic compositions
- Dynamic control-flow examples that demonstrate predictable state usage
- Practical orchestration scenarios useful for developer adoption

### Migration Guide

- Not required for MVP because there is no existing external installed-user base yet.
- Migration guidance can be introduced later once versioned compatibility and user upgrade paths become relevant.

### Implementation Considerations

- Build and ship Python package ergonomics first (clarity of install/use over broad platform expansion).
- Add editor support via an extension focused on orchestration highlighting to improve authoring experience.
- Keep documentation lean and high-value for MVP:
  - API reference
  - README
  - architecture guide
- Avoid spending effort on irrelevant sections for this type in MVP:
  - visual design systems
  - app-store/compliance flows


## Project Scoping & Phased Development

### MVP Strategy & Philosophy

**MVP Approach:** Platform MVP focused on proving that Circuitry can deliver reliable, composable cybernetic orchestration execution with deterministic state behavior and practical developer/operator usability.

**Strategic intent:**
- De-risk core runtime correctness and reliability first.
- Establish real production utility through embedded use, API-triggered use, and scheduled use.
- Delay high-polish observability UX (Perceptron) until after runtime stability is proven.

**Resource Requirements:**
- Core build path assumes 1 primary builder (you), with optional contributors.
- Scope is intentionally sequenced to avoid overloading Phase 1.

### MVP Feature Set (Phase 1)

**Core User Journeys Supported:**
- Developer can run examples immediately via CLI.
- Developer can embed Circuitry in Python software and author orchestration logic with Prompt/Dynamic abstractions.
- Service integration can trigger orchestration execution via REST.
- Operator/user can run periodic scheduled orchestrations (cron-style automation).
- Support/troubleshooting can inspect deterministic state paths for diagnosis and recovery.

**Must-Have Capabilities:**
- Stable runtime execution across all defined orchestration execution modes.
- Cross-pattern tests covering execution modes and cybernetic primitives.
- Python package distribution and CLI-first onboarding.
- Embedded Python integration path.
- REST trigger capability.
- Cron/scheduler execution capability.
- Initial adapter stack:
  - at least 2 upstream LLM providers
  - LiteLLM support
- Postgres persistence integration.
- Core documentation for MVP:
  - README
  - architecture guide
  - API reference
- Editor extension for orchestration highlighting (initial practical version).

### Post-MVP Features

**Phase 2 (Growth):**
- Shared-library orchestration contribution workflow.
- GitHub submission pipeline for orchestration contributions.
- Reliability automation and stronger operator controls.
- Perceptron real-time state GUI.

**Phase 3 (Expansion):**
- Broader adapter/tool/plugin ecosystem expansion.
- System self-evolution loops based on orchestration submissions and automation.
- Continued tooling/UX iteration for authoring, runtime operations, and ecosystem governance.

### Risk Mitigation Strategy

**Technical Risks:**
- **Risk:** Runtime instability across complex execution paths.
  **Mitigation:** Phase-1-first reliability gating with comprehensive cross-pattern tests.
- **Risk:** Adapter outages and integration brittleness.
  **Mitigation:** adapter fallback/playbook patterns, conformance tests, and resilient execution handling.
- **Risk:** State-path determinism regressions as features expand.
  **Mitigation:** explicit compatibility contracts and regression tests for state schema/path behavior.

**Market Risks:**
- **Risk:** "Cybernetic orchestration" value proposition is misunderstood.
  **Mitigation:** examples and docs that clearly show differentiator vs static orchestration tooling.
- **Risk:** Adoption without depth.
  **Mitigation:** prioritize production usage depth metrics and practical integration outcomes.

**Resource Risks:**
- **Risk:** Solo-builder bandwidth constrained by broad roadmap.
  **Mitigation:** strict phase boundaries, perceptron deferral to Phase 2, and incremental delivery with narrow MVP acceptance criteria.


## Functional Requirements

### Orchestration Definition & Composition

- FR1: Developers can define orchestrations using Circuitry's DOL.
- FR2: Developers can compose Prompt and Dynamic objects as reusable orchestration units.
- FR3: Developers can define orchestrations with explicit chain-style effect flow.
- FR4: Developers can define orchestrations with explicit tree-style effect flow.
- FR5: Developers can include conditional control-flow effects in orchestrations.
- FR6: Developers can include loop control-flow effects in orchestrations.
- FR7: Developers can define nested orchestration structures composed of multiple effect types.
- FR8: Developers can reference orchestration outputs through predictable state keys/paths.

### Runtime Execution & Control

- FR9: Users can execute orchestrations through CLI commands.
- FR10: Systems can execute orchestrations through programmatic Python API usage.
- FR11: Services can trigger orchestration execution via REST-invoked workflows.
- FR12: Users can run orchestrations in scheduled recurring patterns (for example, daily cron-style runs).
- FR13: Runtime execution records orchestration outputs and execution metadata in state.
- FR14: Runtime preserves deterministic mapping between orchestration structure and state paths.
- FR15: Runtime supports execution across all defined orchestration modes and primitives.
- FR16: Runtime can continue to support orchestration execution under adapter fallback/recovery strategies when configured.

### Adapters, Providers, and Tool Integrations

- FR17: Developers can configure and use multiple upstream LLM adapters.
- FR18: Runtime supports LiteLLM as a provider integration path.
- FR19: Runtime supports at least two direct upstream provider integrations.
- FR20: Developers can configure provider selection and runtime adapter settings per environment.
- FR21: Runtime supports persistence integration through a Postgres-backed tool path.
- FR22: Developers can extend runtime capabilities through plugin/tool integration points.

### Shared Library & Orchestration Reuse

- FR23: Users can retrieve orchestrations from a shared library for execution.
- FR24: Users can add new orchestrations to the shared library.
- FR25: Teams can reuse shared-library orchestrations across multiple services/use cases.
- FR26: Shared-library orchestration assets can be executed without redefining orchestration logic each time.
- FR27: The product can support growth toward contribution workflows for library expansions.

### Developer Experience & Documentation

- FR28: New users can run built-in examples immediately via CLI.
- FR29: Developers can embed Circuitry as a package in other Python software.
- FR30: Developers can discover and use documented API surfaces for orchestration authoring and runtime execution.
- FR31: Product documentation includes a README sufficient for onboarding.
- FR32: Product documentation includes an architecture guide for system understanding.
- FR33: Product documentation includes an API reference for developer integration.
- FR34: Users can access example orchestrations demonstrating practical use patterns.
- FR35: Developers can use editor extension support for orchestration highlighting.

### Operations, Observability, and Troubleshooting

- FR36: Operators can monitor outcomes of recurring orchestration runs.
- FR37: Support users can inspect state and execution metadata to troubleshoot failed or unexpected runs.
- FR38: Support users can identify orchestration step divergence through deterministic state-path inspection.
- FR39: Teams can validate orchestration behavior through test coverage spanning all defined execution patterns.
- FR40: The product supports roadmap progression to a real-time state-visualization GUI (Perceptron) as a post-MVP capability.


## Non-Functional Requirements

### Performance

- **NFR-P1:** Built-in CLI example execution starts within **<= 5 seconds** on a baseline developer machine.
- **NFR-P2:** REST-trigger endpoint acknowledges orchestration requests within **<= 1 second** (excluding upstream model completion time).
- **NFR-P3:** Scheduled orchestration dispatch latency is **<= 30 seconds** from scheduled trigger time.

### Reliability

- **NFR-R1:** MVP target orchestration execution success rate is **>= 99%** for valid orchestrations under normal operating conditions.
- **NFR-R2:** Adapter outages and upstream invocation failures must produce structured, diagnosable error records in orchestration state/metadata.
- **NFR-R3:** No silent execution failures are permitted; all failed runs must emit traceable failure metadata.

### Security

- **NFR-S1:** API keys and secrets must never be hardcoded and must be provided through secure configuration or environment injection.
- **NFR-S2:** Persistence communications to Postgres must use encrypted transport in deployment environments.
- **NFR-S3:** Runtime-trigger interfaces used in deployed REST integrations must enforce at least basic token-based access control in MVP.

### Scalability

- **NFR-SC1:** MVP deployment profile supports at least **100 scheduled orchestrations per day**.
- **NFR-SC2:** A documented horizontal scaling approach is required for post-MVP growth planning.

### Integration

- **NFR-I1:** LiteLLM and at least two upstream providers must pass defined adapter conformance scenarios.
- **NFR-I2:** Postgres persistence integration must support deterministic orchestration state write/read behavior.
- **NFR-I3:** State-path behavior must remain backward-compatible across patch/minor releases within the MVP cycle.

### Maintainability & Testability

- **NFR-M1:** Release readiness requires passing tests, linting, and type-check gates.
- **NFR-M2:** Automated tests must cover all defined orchestration execution patterns and cybernetic primitive paths.
- **NFR-M3:** Example orchestrations must be versioned and validated against current runtime behavior.
