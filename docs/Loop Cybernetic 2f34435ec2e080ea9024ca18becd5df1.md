# Loop Cybernetic

A Loop repeatedly executes a collection of effects while a continuation condition remains satisfied, or while iterating over a collection. Each iteration executes within the current Dynamic and observes the updated state produced by prior iterations.

Loops do not plan, reason globally, or emit new execution plans. They operate entirely within the scope of the current Dynamic.

---

## **Conceptual Definition**

A Loop is responsible for:

- repeatedly executing a defined body of effects
- determining whether additional iterations should occur
- enforcing iteration bounds and termination rules
- optionally recording iteration metadata

A Loop defines **how long effects repeat**, not **what effects exist**.

---

## **Loop Structure**

A Loop is composed of three conceptual parts:

1. **Body** — the effects executed each iteration
2. **Continuation strategy** — determines whether another iteration occurs
3. **Iteration control** — bounds, limits, and error behavior

Each iteration executes sequentially and observes the state produced by the previous iteration.

---

## **Continuation Strategies**

Loops support three continuation strategies, selected by mode:

1. **Cybernetic continuation** — model-executed, adaptive
2. **Deterministic continuation** — logical, CEL-based
3. **Collection iteration** — structural, path-based (each)

Each strategy determines *when* iteration occurs, not *how* execution is structured.

---

## **Cybernetic Continuation (mode: model)**

Cybernetic continuation executes a **model against a rendered template** to determine whether another iteration should occur.

This mode is used when continuation depends on:

- qualitative assessment
- heuristic judgment
- convergence detection
- natural-language interpretation

### **Characteristics**

- Template is rendered against updated state
- A model is invoked at the end of each iteration
- Output is normalized to a boolean
- Evaluation adapts as state evolves

Cybernetic continuation is the **only loop mode that invokes a model**.

---

## **Deterministic Continuation (mode: cel)**

Deterministic continuation evaluates a **CEL expression** directly against state.

This mode is used when continuation depends on:

- counters or thresholds
- convergence metrics
- strict invariants
- explicit boolean logic

### **Characteristics**

- No model invocation occurs
- Expression is evaluated locally
- Result is strictly boolean
- Evaluation is fast and reproducible

---

## **Collection Iteration (mode: each)**

Collection iteration executes the loop body **once per element** in a resolved collection.

This mode is used when effects must be applied uniformly across a known set of items.

### **Characteristics**

- No model invocation occurs
- No condition is re-evaluated per iteration
- Iteration count is determined by collection size
- Execution order follows collection order
- Behavior is fully deterministic

---

## **Named Loop vs Transparent Control**

Loops support two recording modes:

### **Named Loop**

A Loop with a name is a **named iteration construct**.

In this mode, iteration metadata such as count and termination reason is recorded under a deterministic state path derived from name.

### **Transparent Control**

A Loop without a name is **transparent control**.

In this mode:

- no loop wrapper record is written
- only the Effects produced by iterations are recorded
- iteration boundaries are not preserved in state

Transparent control preserves execution semantics while minimizing state noise.

---

## **Loop as a Macro Construct**

A Loop is **not** a standalone runtime primitive.

It is a **built-in DOL construct** expanded by the interpreter into a deterministic execution pattern composed of:

- a continuation check or collection resolution
- repeated execution of the loop body
- explicit termination

The runtime executes only Prompts, Dynamics, Conditionals, and Loops.

---

## **Loop Definition (DOL)**

### **Required Fields**

```
type: loop
body: array
```

### **Continuation Configuration**

Exactly one of the following must be specified:

### **Cybernetic or Deterministic Continuation**

```
while: object
```

### **Collection Iteration**

```
each: object
```

---

### **Optional Fields**

```
name?: string
description?: string

mode?: string              # model | cel | each (inferred if omitted)

# Iteration control
max_iterations?: number
min_iterations?: number

# Error handling
on_error?: string          # fail | break | continue

# Metadata
labels?: object
```

---

## **Continuation Definitions**

### **Cybernetic Continuation**

```
while:
  mode: model
  template: |
    Has the task converged?
    {{summary.latest}}
```

- Template is rendered against current state
- Model is invoked
- Output is normalized to a boolean

---

### **Deterministic Continuation (CEL)**

```
while:
  mode: cel
  expr: "state.iteration.count < 5 && state.score.value < 0.95"
```

- Expression is evaluated using a CEL engine
- No templating occurs inside expr
- Expression must evaluate to a boolean

---

### **Collection Iteration**

```
each:
  in: state.documents
  as: doc
```

### **Fields**

- **each.in**
    
    A dot-delimited path reference into the effective context.
    
    Must resolve to an array.
    
- **each.as** (optional)
    
    Identifier bound to the current element for each iteration.
    
    Defaults to item if omitted.
    

### **Resolution Rules**

- Missing or null → zero iterations
- Empty array → zero iterations
- Non-array value → error (or on_error policy)

---

## **Execution Lifecycle**

When executed, a Loop:

1. Builds an effective execution context from current state and inherited values.
2. Determines the continuation strategy.
3. For each mode:
    - resolves the collection once
    - iterates sequentially over elements
4. For model or cel modes:
    - evaluates the continuation condition
    - executes the body if true
    - re-evaluates after each iteration
5. Updates state with Effects produced by each iteration.
6. Terminates when continuation fails or a termination rule is met.

---

## **State Writes**

### **Named Loop**

Records:

- iteration count
- termination reason
- aggregated metadata
- Effects produced across iterations

### **Transparent Control**

- No loop wrapper record is written
- Only body Effects are recorded
- Iteration count is not preserved in state

---

## **Determinism**

Loop execution is structurally deterministic.

- Iteration order is fixed
- State evolves monotonically across iterations
- Termination behavior is explicitly defined

Model variability affects only continuation decisions, not execution structure.

---

## **Loop Usage Examples**

### **Example 1: Deterministic Named Loop**

```
type: loop
name: retry_until_success
while:
  mode: cel
  expr: "state.attempts < 3 && state.success != true"
body:
  - type: prompt
    name: attempt
    template: "Try again."
```

---

### **Example 2: Collection Iteration (Transparent Control)**

```
type: loop
mode: each
each:
  in: state.users
body:
  - type: prompt
    name: notify
    template: "Notify {{item.email}}"
```

---

## **Conceptual Role**

A Loop provides controlled repetition.

Cybernetic continuation enables adaptive stopping.

Deterministic continuation enforces strict bounds.

Collection iteration applies effects structurally.

Dynamics define execution topology.

Circuitry executes effects deterministically.

Together, they enable explicit, safe iteration without embedding planning or reasoning into the runtime.