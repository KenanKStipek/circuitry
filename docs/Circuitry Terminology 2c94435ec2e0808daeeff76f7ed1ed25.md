# Circuitry Terminology

---

## **State**

State is the single source of truth.

It is a hierarchical, serializable structure containing **domain state** and **runtime state**. State is predictable in shape, append-only for runtime execution, safe for template interpolation, and suitable for persistence to external data stores.

Circuitry writes only to runtime state. Domain state is never mutated implicitly.

State is read-only for planning components and append-only for execution components.

---

## **Prompt**

A Prompt is the atomic execution unit.

A Prompt has a name and targets a specific model capability, including language, vision, multimodal, or tool-capable models. Prompt input may include text, images, structured data, or model-specific payloads.

A Prompt renders its input against the current state, performs **exactly one** model invocation, produces a value, and writes both its value and execution metadata to state at a deterministic path.

A Prompt represents one and only one model call.

---

## **Dynamic**

A Dynamic is a named execution structure.

A Dynamic contains Prompts, Conditionals, Loops, and/or other Dynamics. It defines **control flow and execution topology**, aggregates execution metadata from its children, and records its effects in state.

A Dynamic defines **how execution proceeds**, not **why execution occurs**.

---

### **Control Flow Models**

Dynamics support multiple explicit control flow models. These models describe execution structure, not reasoning intent, and are enforced deterministically by the runtime.

Supported control flows include:

- **Chain (Sequential Execution)**
    
    Effects are executed one after another in a fixed order.
    
    Each step observes the same evolving state produced by prior steps.
    
- **Chain of Thought**
    
    A linear execution flow where intermediate reasoning steps are explicitly modeled as Prompts or nested Dynamics.
    
    Each step records its intermediate outputs to state, enabling inspection, auditing, and reuse.
    
- **Tree (Parallel Execution)**
    
    Multiple effects are executed concurrently using structured or bounded concurrency.
    
    All branches observe the same input state and produce independent effects.
    
- **Tree of Thought**
    
    A structured branching execution flow where multiple reasoning paths are explored in parallel.
    
    Each branch represents an independent line of reasoning and records its outputs separately in state for later evaluation, comparison, or selection.
    

Execution order, branching, and concurrency are determined solely by the Dynamic definition and runtime strategy. No implicit prioritization or pruning occurs.

---

### **Execution Semantics**

- Dynamics do not infer control flow.
- Dynamics do not decide which branches to execute.
- All control flow is explicitly declared.

Branch selection, repetition, termination, or convergence of execution paths is handled by Conditionals, Loops, or planning components.

---

## **Prime Dynamic**

A Prime Dynamic is a top-level Dynamic executed by Circuitry.

It has no parent Dynamic and represents a complete, self-contained execution plan. A Prime Dynamic may be authored directly, generated programmatically, or produced by a Reflector.

Prime Dynamics are executed exactly as defined. They are not modified, inferred, or expanded by the runtime.

---

## **Conditional**

A Conditional is an execution-time branching construct.

A Conditional evaluates a condition against the current state. The condition may be evaluated using a model invocation, structured logic, or deterministic state checks.

Based on the result, exactly one branch is selected and executed. Non-selected branches are not executed and do not produce effects.

Conditionals operate entirely within the current Dynamic and do not emit new execution plans.

---

## **Loop**

A Loop is an execution-time repetition construct.

A Loop repeatedly executes a contained Dynamic or Effect until a termination condition is met. Termination conditions may include deterministic state predicates, model-evaluated conditions, or explicit iteration limits.

Loops are bounded, fully auditable, and recorded in runtime state. Each iteration produces observable effects and metadata.

---

## **Directive**

A Directive is a planning instruction.

A Directive expresses intent rather than execution. It is consumed by a Reflector and is never executed by the runtime.

Directives are optional. Circuitry does not require Directives to execute.

---

## **Reflector**

A Reflector is an optional planning and control component.

A Reflector reads state, interprets Directives or heuristics, and produces one or more Prime Dynamics. After execution, it may inspect updated state and decide whether to emit additional Prime Dynamics or terminate.

Reflectors do not execute Prompts, Dynamics, Conditionals, or Loops, and do not write to state directly. Their sole responsibility is constructing execution plans.

---

## **Iteration**

An Iteration represents a single Reflector planning cycle.

Each Iteration consumes state, produces one or more Prime Dynamics, and records planning metadata. Iterations are indexed and structural but are not named DSL entities.

Iterations exist only when Reflectors are used.

---

## **Circuitry Runtime**

The Circuitry Runtime is the deterministic execution engine.

It executes Prime Dynamics, evaluates Dynamics, Prompts, Conditionals, and Loops exactly as defined, invokes models, and writes all values and metadata to state.

The runtime performs no planning, reflection, or intent inference.

---

## **Prime**

Prime is the entry point into the Circuitry Runtime.

Prime receives the current state and a Prime Dynamic, executes the Dynamic, and returns the updated state.

Prime does not require a Reflector.

---

## **Conceptual Flow**

Execution-only flow:

State and a Prime Dynamic are provided to Prime. The Prime Dynamic is executed by the Circuitry Runtime. The updated state is returned.

Planning-assisted flow:

State and a Directive are provided to a Reflector. The Reflector produces one or more Prime Dynamics. These Dynamics are executed by the Circuitry Runtime, and the resulting state may be fed back into the Reflector.

---

## **Effects**

An **Effect** is a recorded outcome of execution.

Effects represent what occurred as a result of executing a Prompt, Dynamic, Conditional, or Loop. Effects are immutable records containing values and metadata. They are stored in state for inspection, persistence, and downstream reasoning.

Effects do not define control flow and do not influence execution directly.

## **Adapter**

An Adapter is an integration layer between Circuitry and external systems.

Adapters translate Circuitry’s internal execution model into the concrete interfaces required by upstream AI models, tools, or services. They are responsible for adapting request formats, handling transport concerns, normalizing responses, and reporting execution metadata back to the runtime.

Adapters may support:

- Language, vision, multimodal, or tool-capable AI models
- Local or remote model execution
- Provider-specific authentication, retries, timeouts, and quotas

Adapters do not define orchestration logic, control flow, or state structure. They execute work on behalf of Prompts and return results in a form the runtime can record deterministically.

Adapters allow Circuitry to remain model-agnostic while supporting heterogeneous and evolving AI systems.

---

## **Plugin**

A Plugin is an extension point for integrating Circuitry with external infrastructure.

Plugins allow Circuitry to be connected to systems such as persistence layers, observability tools, message queues, or custom runtime services. Common examples include state persistence to databases (e.g., MongoDB, PostgreSQL), external logging, or execution hooks.

Plugins may:

- Persist or hydrate state
- Observe execution lifecycle events
- Augment runtime behavior without modifying core logic

Plugins do not execute Prompts, make planning decisions, or alter execution semantics. They operate alongside the runtime through well-defined interfaces.

Plugins allow Circuitry to be embedded into larger systems while preserving deterministic execution and a stable core.