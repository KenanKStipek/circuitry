# Perceptron Scope Boundary

## MVP Boundary

Perceptron is explicitly out of MVP implementation scope in this repository.

MVP includes:
- Deterministic orchestration runtime
- CLI, embedded API, adapter, persistence, plugin, and shared-library consumption flows
- Editor syntax highlighting support for authoring quality

MVP excludes:
- Perceptron real-time state GUI implementation
- Perceptron runtime transport/event streaming
- Perceptron frontend UX and deployment lifecycle

## Guardrails

- Do not add runtime coupling that assumes a Perceptron UI exists.
- Keep runtime metadata and deterministic state paths stable so post-MVP UI can consume them.
- Treat Perceptron requirements as documentation/roadmap unless a separate story explicitly promotes them into scope.

## Post-MVP Entry Criteria

Perceptron work can enter delivery scope when:
1. Runtime metadata contracts are frozen/versioned.
2. Observability and state-persistence interfaces are stable across target deployment modes.
3. Product planning creates explicit Perceptron delivery stories and acceptance criteria.
