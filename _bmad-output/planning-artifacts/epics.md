---
stepsCompleted: [step-01-validate-prerequisites, step-02-design-epics, step-03-create-stories, step-04-final-validation]
inputDocuments:
  - _bmad-output/planning-artifacts/prd.md
  - _bmad-output/planning-artifacts/architecture.md
---

# circuitry - Epic Breakdown

## Overview

This document provides the complete epic and story breakdown for circuitry, decomposing the requirements from the PRD, UX Design if it exists, and Architecture requirements into implementable stories.

## Requirements Inventory

### Functional Requirements

FR1: Developers can define orchestrations using Circuitry’s DOL.
FR2: Developers can compose Prompt and Dynamic objects as reusable orchestration units.
FR3: Developers can define orchestrations with explicit chain-style effect flow.
FR4: Developers can define orchestrations with explicit tree-style effect flow.
FR5: Developers can include conditional control-flow effects in orchestrations.
FR6: Developers can include loop control-flow effects in orchestrations.
FR7: Developers can define nested orchestration structures composed of multiple effect types.
FR8: Developers can reference orchestration outputs through predictable state keys/paths.
FR9: Users can execute orchestrations through CLI commands.
FR10: Systems can execute orchestrations through programmatic Python API usage.
FR11: Services can trigger orchestration execution via REST-invoked workflows.
FR12: Users can run orchestrations in scheduled recurring patterns (for example, daily cron-style runs).
FR13: Runtime execution records orchestration outputs and execution metadata in state.
FR14: Runtime preserves deterministic mapping between orchestration structure and state paths.
FR15: Runtime supports execution across all defined orchestration modes and primitives.
FR16: Runtime can continue to support orchestration execution under adapter fallback/recovery strategies when configured.
FR17: Developers can configure and use multiple upstream LLM adapters.
FR18: Runtime supports LiteLLM as a provider integration path.
FR19: Runtime supports at least two direct upstream provider integrations.
FR20: Developers can configure provider selection and runtime adapter settings per environment.
FR21: Runtime supports persistence integration through a Postgres-backed tool path.
FR22: Developers can extend runtime capabilities through plugin/tool integration points.
FR23: Users can retrieve orchestrations from a shared library for execution.
FR24: Users can add new orchestrations to the shared library.
FR25: Teams can reuse shared-library orchestrations across multiple services/use cases.
FR26: Shared-library orchestration assets can be executed without redefining orchestration logic each time.
FR27: The product can support growth toward contribution workflows for library expansions.
FR28: New users can run built-in examples immediately via CLI.
FR29: Developers can embed Circuitry as a package in other Python software.
FR30: Developers can discover and use documented API surfaces for orchestration authoring and runtime execution.
FR31: Product documentation includes a README sufficient for onboarding.
FR32: Product documentation includes an architecture guide for system understanding.
FR33: Product documentation includes an API reference for developer integration.
FR34: Users can access example orchestrations demonstrating practical use patterns.
FR35: Developers can use editor extension support for orchestration highlighting.
FR36: Operators can monitor outcomes of recurring orchestration runs.
FR37: Support users can inspect state and execution metadata to troubleshoot failed or unexpected runs.
FR38: Support users can identify orchestration step divergence through deterministic state-path inspection.
FR39: Teams can validate orchestration behavior through test coverage spanning all defined execution patterns.
FR40: The product supports roadmap progression to a real-time state-visualization GUI (Perceptron) as a post-MVP capability.

### NonFunctional Requirements

NFR1: [NFR-P1] Built-in CLI example execution starts within <= 5 seconds on a baseline developer machine.
NFR2: [NFR-P2] REST-trigger endpoint acknowledges orchestration requests within <= 1 second (excluding upstream model completion time).
NFR3: [NFR-P3] Scheduled orchestration dispatch latency is <= 30 seconds from scheduled trigger time.
NFR4: [NFR-R1] MVP target orchestration execution success rate is >= 99% for valid orchestrations under normal operating conditions.
NFR5: [NFR-R2] Adapter outages and upstream invocation failures must produce structured, diagnosable error records in orchestration state/metadata.
NFR6: [NFR-R3] No silent execution failures are permitted; all failed runs must emit traceable failure metadata.
NFR7: [NFR-S1] API keys and secrets must never be hardcoded and must be provided through secure configuration or environment injection.
NFR8: [NFR-S2] Persistence communications to Postgres must use encrypted transport in deployment environments.
NFR9: [NFR-S3] Runtime-trigger interfaces used in deployed REST integrations must enforce at least basic token-based access control in MVP.
NFR10: [NFR-SC1] MVP deployment profile supports at least 100 scheduled orchestrations per day.
NFR11: [NFR-SC2] A documented horizontal scaling approach is required for post-MVP growth planning.
NFR12: [NFR-I1] LiteLLM and at least two upstream providers must pass defined adapter conformance scenarios.
NFR13: [NFR-I2] Postgres persistence integration must support deterministic orchestration state write/read behavior.
NFR14: [NFR-I3] State-path behavior must remain backward-compatible across patch/minor releases within the MVP cycle.
NFR15: [NFR-M1] Release readiness requires passing tests, linting, and type-check gates.
NFR16: [NFR-M2] Automated tests must cover all defined orchestration execution patterns and cybernetic primitive paths.
NFR17: [NFR-M3] Example orchestrations must be versioned and validated against current runtime behavior.

### Additional Requirements

- Architecture document at _bmad-output/planning-artifacts/architecture.md is initialized but not yet populated with technical decisions; architecture-driven implementation constraints are currently incomplete.
- Until architecture decisions are completed, epics/stories should avoid locking irreversible implementation choices (especially deployment topology, API contract specifics, and data model partitioning).
- Preserve deterministic orchestration-to-state mapping as a non-negotiable implementation invariant when decomposing stories.
- Ensure multi-provider adapter conformance, Postgres persistence integration, and observability/error metadata requirements are represented in story acceptance criteria.
- Treat Perceptron GUI as post-MVP scope (do not include in MVP implementation stories).

### FR Coverage Map

FR1: Epic 1 - Define orchestrations using DOL
FR2: Epic 1 - Compose Prompt and Dynamic units
FR3: Epic 2 - Support explicit chain-style flow
FR4: Epic 2 - Support explicit tree-style flow
FR5: Epic 2 - Support conditional control-flow effects
FR6: Epic 2 - Support loop control-flow effects
FR7: Epic 2 - Support nested composed effects
FR8: Epic 1 - Expose predictable state key/path usage
FR9: Epic 1 - Execute orchestrations from CLI
FR10: Epic 1 - Execute orchestrations from Python API
FR11: Epic 3 - Trigger orchestrations via REST
FR12: Epic 3 - Run recurring scheduled orchestrations
FR13: Epic 1 - Persist outputs and execution metadata to state
FR14: Epic 1 - Preserve deterministic orchestration-state mapping
FR15: Epic 2 - Execute all defined orchestration modes/primitives
FR16: Epic 3 - Support configured adapter fallback/recovery behavior
FR17: Epic 4 - Configure and use multiple upstream adapters
FR18: Epic 4 - Support LiteLLM integration path
FR19: Epic 4 - Support at least two direct upstream providers
FR20: Epic 4 - Configure provider/runtime settings per environment
FR21: Epic 4 - Integrate Postgres-backed persistence tool path
FR22: Epic 4 - Provide plugin/tool extension integration points
FR23: Epic 5 - Retrieve orchestrations from shared library
FR24: Epic 5 - Add orchestrations to shared library
FR25: Epic 5 - Reuse shared library orchestrations across services
FR26: Epic 5 - Execute shared orchestration assets directly
FR27: Epic 5 - Support growth contribution workflows
FR28: Epic 1 - Run built-in examples immediately via CLI
FR29: Epic 1 - Embed Circuitry as a Python package
FR30: Epic 1 - Discover and use documented runtime APIs
FR31: Epic 6 - Provide README onboarding documentation
FR32: Epic 6 - Provide architecture documentation
FR33: Epic 6 - Provide API reference documentation
FR34: Epic 6 - Provide practical example orchestrations
FR35: Epic 6 - Provide editor orchestration highlighting support
FR36: Epic 3 - Monitor recurring orchestration outcomes
FR37: Epic 3 - Inspect execution state/metadata for troubleshooting
FR38: Epic 3 - Identify step divergence via deterministic state paths
FR39: Epic 2 - Validate behavior across all execution patterns
FR40: Epic 6 - Preserve roadmap path to post-MVP Perceptron GUI

## Epic List

### Epic 1: Author and Run Orchestrations End-to-End
Developers can define orchestrations and execute them immediately via CLI or embedded Python usage, with deterministic persisted outputs.
**FRs covered:** FR1, FR2, FR8, FR9, FR10, FR13, FR14, FR28, FR29, FR30

### Epic 2: Build Complex Cybernetic Control Flows
Developers can compose advanced chain/tree/nested orchestration logic with conditionals and loops that execute predictably.
**FRs covered:** FR3, FR4, FR5, FR6, FR7, FR15, FR39

### Epic 3: Operate Orchestrations in Production Triggers
Teams can run orchestrations via REST and schedules, recover from adapter disruptions, and troubleshoot using deterministic state and metadata.
**FRs covered:** FR11, FR12, FR16, FR36, FR37, FR38

### Epic 4: Integrate Providers, Persistence, and Runtime Extensions
Developers can configure multiple providers/adapters, persist state in Postgres, and extend runtime behavior through plugin/tool integrations.
**FRs covered:** FR17, FR18, FR19, FR20, FR21, FR22

### Epic 5: Reuse and Evolve Orchestrations Through a Shared Library
Teams can retrieve, contribute, and reuse orchestration assets across services with a growth path for contribution workflows.
**FRs covered:** FR23, FR24, FR25, FR26, FR27

### Epic 6: Ship Developer Experience and Growth-Ready Tooling
Users get complete onboarding docs/examples and editor highlighting, with roadmap-aligned preparation for post-MVP state visualization.
**FRs covered:** FR31, FR32, FR33, FR34, FR35, FR40

## Epic 1: Author and Run Orchestrations End-to-End

Enable developers to define orchestrations and execute them through CLI and embedded Python APIs with deterministic state writes and fast time-to-first-value.

### Story 1.1: Define and Validate Basic DOL Orchestrations

As a developer,
I want to define simple orchestrations using Prompt and Dynamic objects in DOL,
So that I can author valid orchestration plans quickly.

**Acceptance Criteria:**

**Given** an orchestration definition with Prompt and Dynamic nodes
**When** I run schema/structure validation
**Then** the runtime accepts valid constructs and rejects invalid definitions with actionable errors
**And** validation messages identify failing node paths and expected schema shape
**And** validation enforces unique sibling step names and state-path-safe naming conventions required by the State Path Contract.

### Story 1.2: Execute Orchestrations from CLI with State Output

As a user,
I want to run an orchestration from the CLI,
So that I can see execution outputs and metadata persisted in deterministic state paths.

**Acceptance Criteria:**

**Given** a valid orchestration file and CLI entrypoint
**When** I execute a run command
**Then** the orchestration completes and writes both value and metadata to deterministic state paths
**And** CLI output includes success/failure summary and artifact location for state inspection
**And** at least one documented CLI example shows exact state paths that map directly to orchestration composition (including named loop `iter_<n>` segments).

### Story 1.3: Execute Orchestrations via Embedded Python API

As a developer,
I want to execute orchestrations from Python package APIs,
So that I can embed Circuitry in other Python systems without shelling out to CLI.

**Acceptance Criteria:**

**Given** an installed Circuitry package in a Python project
**When** I invoke orchestration execution through public API methods
**Then** execution behavior matches CLI semantics for outputs and state writes
**And** API usage is documented with runnable examples.

### Story 1.4: Ship Time-to-Value CLI Examples

As a new user,
I want built-in examples that run immediately,
So that I can confirm the framework works before writing custom orchestration logic.

**Acceptance Criteria:**

**Given** a fresh local setup with documented prerequisites
**When** I run the quick-start example command
**Then** a successful orchestration run starts within the NFR target and produces expected output
**And** README quick-start instructions are sufficient without additional tribal knowledge.

## Epic 2: Build Complex Cybernetic Control Flows

Enable developers to model advanced orchestration behavior using chain/tree flow, conditionals, loops, and nested dynamics with predictable runtime semantics.

### Story 2.1: Implement Chain and Tree Effect Topologies

As a developer,
I want to express chain and tree orchestration flow patterns,
So that I can encode different execution structures explicitly in DOL.

**Acceptance Criteria:**

**Given** orchestration definitions using chain and tree patterns
**When** the compiler normalizes these definitions for execution
**Then** runtime executes effects in the expected deterministic structure
**And** resulting state paths reflect declared topology consistently.

### Story 2.2: Implement Conditional and Loop Cybernetic Primitives

As a developer,
I want to use conditionals and loops in orchestrations,
So that I can encode dynamic control flow for real-time decision and iteration behavior.

**Acceptance Criteria:**

**Given** orchestration definitions containing conditional and loop constructs
**When** runtime executes control-flow evaluation and loop progression
**Then** branching and iteration follow declared mode semantics with explicit termination behavior
**And** runtime records metadata needed to diagnose branch/loop outcomes
**And** conditional and loop writes follow deterministic wrapper/iteration path rules defined in the architecture State Path Contract.

### Story 2.3: Support Nested Dynamic Composition

As a developer,
I want to compose nested dynamics and mixed effect types,
So that I can model complex orchestration logic while preserving readability and predictability.

**Acceptance Criteria:**

**Given** a nested orchestration with prompts, dynamics, conditionals, and loops
**When** execution runs through nested levels
**Then** each node resolves context and state paths deterministically
**And** failures report hierarchical location for rapid troubleshooting
**And** path composition remains derivable from orchestration object names without hidden or non-deterministic segments.

### Story 2.4: Validate Execution Pattern Test Coverage

As an engineering owner,
I want automated tests covering all supported execution patterns,
So that runtime behavior remains stable as orchestration capabilities evolve.

**Acceptance Criteria:**

**Given** the supported orchestration modes and cybernetic primitives
**When** automated tests run in CI
**Then** each execution pattern has explicit test coverage for success and failure paths
**And** release gates fail if coverage or correctness regressions are introduced
**And** a conformance suite verifies deterministic state-path mapping across chain, tree, conditional, and loop execution patterns.

## Epic 3: Operate Orchestrations in Production Triggers

Enable production operation through REST and scheduler triggers, with resilience and traceability for troubleshooting and recovery.

### Story 3.1: Trigger Orchestrations Through REST Interface

As an integration engineer,
I want to invoke orchestrations through a REST endpoint,
So that external services can trigger runtime execution programmatically.

**Acceptance Criteria:**

**Given** a deployed trigger interface with authentication enabled
**When** a valid orchestration request is submitted
**Then** the service acknowledges the request within the defined NFR target
**And** execution request IDs and status are traceable for follow-up inspection.

### Story 3.2: Run Recurring Scheduled Orchestrations

As an operator,
I want orchestrations to run on recurring schedules,
So that periodic workflows execute consistently without manual intervention.

**Acceptance Criteria:**

**Given** a configured recurring schedule and orchestration target
**When** scheduled trigger time is reached
**Then** runtime dispatch occurs within target latency and run outcome is persisted
**And** missed or failed schedules emit explicit diagnostic records.

### Story 3.3: Handle Adapter Outages with Recovery Metadata

As an operator,
I want adapter failures to produce structured failure and fallback records,
So that I can recover runs and minimize downtime impact.

**Acceptance Criteria:**

**Given** an orchestration execution where the primary adapter fails
**When** runtime applies configured fallback/recovery behavior
**Then** failure context, fallback path, and final outcome are written to execution metadata
**And** no failure scenario ends silently without a traceable error record.

### Story 3.4: Troubleshoot Divergence via Deterministic State Paths

As a support user,
I want to inspect state and execution metadata deterministically,
So that I can isolate where orchestration behavior diverged from expectations.

**Acceptance Criteria:**

**Given** a completed or failed orchestration run
**When** support inspects stored state paths and metadata
**Then** divergence points can be identified to specific orchestration steps
**And** troubleshooting documentation explains how to perform this diagnosis.

## Epic 4: Integrate Providers, Persistence, and Runtime Extensions

Enable robust provider integration, durable persistence, and extension interfaces for plugins/tools across environments.

### Story 4.1: Configure Multi-Provider Adapter Runtime

As a developer,
I want to configure multiple upstream providers and adapter selection rules,
So that I can choose the best model path per environment and workload.

**Acceptance Criteria:**

**Given** environment-specific runtime configuration
**When** an orchestration run is executed
**Then** adapter/provider resolution follows configured selection rules
**And** runtime metadata records provider identity and configuration context used.

### Story 4.2: Integrate LiteLLM and Direct Provider Conformance

As a platform engineer,
I want LiteLLM and direct providers validated against conformance scenarios,
So that provider behavior is portable and predictable.

**Acceptance Criteria:**

**Given** at least LiteLLM and two direct provider adapters
**When** adapter conformance tests are executed
**Then** all required capabilities pass the defined scenarios
**And** incompatibilities are surfaced with actionable diagnostics.

### Story 4.3: Persist Runtime State Through Postgres Tooling

As a developer,
I want orchestration state persisted through a Postgres integration path,
So that run data is durable and queryable for downstream usage.

**Acceptance Criteria:**

**Given** Postgres persistence configuration and secure transport settings
**When** orchestration runs write and read state data
**Then** persisted state preserves deterministic path structure and integrity
**And** persistence failures are reported with non-silent error metadata.

### Story 4.4: Expose Plugin and Tool Extension Points

As an advanced user,
I want extension interfaces for plugins and tools,
So that I can add custom runtime capabilities without patching core execution engine internals.

**Acceptance Criteria:**

**Given** documented extension contracts
**When** a plugin/tool integration is registered and invoked
**Then** runtime executes extension behavior through supported interfaces
**And** extension failures are isolated and observable in run metadata.

## Epic 5: Reuse and Evolve Orchestrations Through a Shared Library

Enable teams to retrieve, execute, and contribute orchestration assets that can be reused across products and services.

### Story 5.1: Retrieve Shared Library Orchestrations

As a developer,
I want to fetch orchestrations from a shared library,
So that I can reuse proven orchestration assets quickly.

**Acceptance Criteria:**

**Given** access to the shared orchestration library
**When** I request an orchestration asset
**Then** I can retrieve it with required metadata/version details
**And** fetched assets are executable without manual restructuring.

### Story 5.2: Execute Shared Assets Across Services

As a team integrating multiple services,
I want shared orchestrations to run consistently across contexts,
So that behavior is reusable without redefining logic per service.

**Acceptance Criteria:**

**Given** a shared orchestration asset used by multiple services
**When** each service executes the orchestration through supported runtime interfaces
**Then** execution semantics and state outputs remain consistent
**And** service-specific configuration can be applied without altering core asset logic.

### Story 5.3: Publish New Orchestrations to Shared Library

As a contributor,
I want to add new orchestrations to the shared library,
So that other teams can discover and reuse them.

**Acceptance Criteria:**

**Given** a valid orchestration artifact and publication workflow
**When** I publish the asset
**Then** it is stored with searchable metadata and version information
**And** publication validation ensures minimum quality requirements are met.

### Story 5.4: Define Growth Workflow for Library Contributions

As a maintainer,
I want a defined growth path for contributions,
So that library expansion remains scalable and governed over time.

**Acceptance Criteria:**

**Given** roadmap requirements for shared library growth
**When** contribution workflow policies are applied
**Then** submissions can be reviewed and validated consistently
**And** workflow documentation defines how growth automation can evolve post-MVP.

## Epic 6: Ship Developer Experience and Growth-Ready Tooling

Enable practical onboarding and documentation quality for adoption, while preparing for post-MVP visualization features.

### Story 6.1: Deliver README and Quick-Start Documentation

As a new user,
I want clear README onboarding documentation,
So that I can install, configure, and run first orchestration quickly.

**Acceptance Criteria:**

**Given** a fresh user with baseline prerequisites
**When** they follow README onboarding instructions
**Then** they can run example orchestrations successfully without external guidance
**And** documentation clearly covers CLI and embedded Python usage.

### Story 6.2: Deliver Architecture and API Reference Documentation

As a developer integrator,
I want architecture and API references,
So that I can understand runtime boundaries and integrate correctly.

**Acceptance Criteria:**

**Given** official architecture and API docs
**When** a developer implements integration using those docs
**Then** they can discover core interfaces and expected behavior accurately
**And** documentation remains aligned with current runtime implementation.

### Story 6.3: Provide Curated Example Orchestrations

As a developer evaluating Circuitry,
I want example orchestrations for real usage patterns,
So that I can adapt working patterns instead of starting from scratch.

**Acceptance Criteria:**

**Given** example orchestration artifacts in repository docs or examples
**When** users run and inspect them
**Then** examples demonstrate core primitives and operational modes clearly
**And** examples are versioned and validated against current runtime behavior.

### Story 6.4: Provide Editor Highlighting and Post-MVP Perceptron Boundary

As a developer,
I want editor highlighting support now and clear post-MVP Perceptron boundaries,
So that authoring experience improves without destabilizing MVP scope.

**Acceptance Criteria:**

**Given** editor extension support scope and roadmap notes
**When** users author orchestration files
**Then** syntax and highlighting support improves authoring clarity in MVP
**And** Perceptron implementation remains explicitly out of MVP delivery scope.
