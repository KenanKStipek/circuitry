# Conditional Cybernetic

A Conditional evaluates an **if** condition against the current state and, based on the result, selects **exactly one** branch to execute. The selected branch executes within the current Dynamic. Non-selected branches are not executed and do not produce effects.

Conditionals do not plan, reason globally, or emit new execution plans. They operate entirely within the scope of the current Dynamic.

---

## **Conceptual Definition**

A Conditional is responsible for:

- evaluating an **if** condition against current state
- optionally invoking a model as part of that evaluation
- deterministically selecting a single execution branch
- enforcing exclusive branch execution
- optionally recording the outcome of the decision

A Conditional defines **which effects execute**, not **how execution is structured**.

---

## **Evaluation Modes**

Conditionals support two evaluation modes:

1. **Cybernetic evaluation** — model-executed, adaptive, heuristic
2. **Deterministic evaluation** — non-model, CEL-based, strictly logical

Both modes produce a boolean decision signal consumed by a deterministic selector.

---

## **Cybernetic Evaluation (Model-Based)**

Cybernetic evaluation executes a **model against a rendered template**.

This mode is used when the decision depends on:

- natural language interpretation
- fuzzy or heuristic judgment
- learned behavior
- ambiguous or qualitative criteria

### **Characteristics**

- Template is rendered against current state
- A model is invoked
- Output is normalized to a boolean
- Evaluation adapts as state evolves

Cybernetic evaluation is the **only part of a Conditional that invokes a model**.

---

## **Deterministic Evaluation (CEL-Based)**

Deterministic evaluation executes a **CEL (Common Expression Language)** expression directly against state.

This mode is used when the decision depends on:

- explicit boolean logic
- numeric or string comparisons
- presence checks
- strict invariants

### **Characteristics**

- No model invocation occurs
- Expression is evaluated locally using a CEL engine
- Result is strictly boolean
- Evaluation is fast, reproducible, and deterministic

Deterministic evaluation is **not cybernetic**.

---

## **Named Decision vs Transparent Control**

Conditionals support two recording modes:

### **Named Decision**

A Conditional with a name is a **named decision point**.

In this mode, the evaluation result and selected branch are recorded under a deterministic state path derived from name. This mode is intended for auditability, debugging, and downstream references.

### **Transparent Control**

A Conditional without a name is **transparent control**.

In this mode:

- no conditional wrapper record is written
- only the Effects produced by the selected branch are recorded
- branch Effects are merged directly into the parent Dynamic

Transparent control preserves execution semantics while minimizing state noise.

---

## **Conditional as a Macro Construct**

A Conditional is **not** a standalone runtime primitive.

It is a **built-in DOL construct** expanded by the interpreter into a deterministic execution pattern composed of:

1. an **evaluation step** (cybernetic or deterministic)
2. a **deterministic selector**
3. a **single branch execution Dynamic**

The runtime executes only Prompts, Dynamics, and Loops.

---

## **Conditional Definition (DOL)**

A Conditional is declared declaratively in the orchestration domain object language.

### **Required Fields**

```
type: if
if: object
then: array
```

### **Optional Fields**

```
name?: string
description?: string

else?: array

# Evaluation behavior
mode?: string            # model | cel (default: model)
threshold?: number       # optional threshold for model-based decisions
on_error?: string        # fail | continue | skip

# Metadata and annotations
labels?: object
```

---

## **Branch Definitions**

- **then**
    
    A collection of effects executed when the **if** condition evaluates to true.
    
- **else** (optional)
    
    A collection of effects executed when the **if** condition evaluates to false.
    

Each branch is a collection of executable effects. Each effect must be one of:

- a Prompt
- a Dynamic
- another Conditional
- a Loop

---

## **Condition Evaluation**

The **if** field defines how the condition is evaluated.

### **Cybernetic (Model-Based) Evaluation**

```
if:
  mode: model
  template: |
    Is the following input sufficient?
    {{input.text}}
```

- Template is rendered against state
- Model is invoked
- Output is normalized to a boolean

---

### **Deterministic (CEL-Based) Evaluation**

```
if:
  mode: cel
  expr: "state.score.value >= 0.9 && state.attempts < 3"
```

- Expression is evaluated using a CEL engine
- No templating occurs inside expr
- Expression must evaluate to a boolean

---

## **CEL Evaluation Environment**

CEL expressions are evaluated against a **single root object**:

```
state
```

All state access must be explicit:

```
state.input.user_role == "admin"
state.score.value >= 0.9
```

### **CEL Constraints**

- No side effects
- No external calls
- Only approved CEL functions are available
- Custom functions (if any) must be deterministic and pure

Missing fields evaluate to null per CEL semantics and must be handled explicitly (e.g., via has() if supported).

---

## **Selector Behavior**

After evaluation, a deterministic selector:

- reads the boolean result
- selects exactly one branch (then or else)
- enforces exclusive execution
- performs no reasoning or model invocation

Selector behavior is deterministic and non-cybernetic.

---

## **Execution Lifecycle**

When executed, a Conditional:

1. Builds an effective execution context from current state and inherited values.
2. Evaluates the **if** condition using the selected evaluation mode.
3. Produces a boolean decision signal.
4. Deterministically selects one branch.
5. Executes the selected branch’s effects.
6. Records outcomes according to recording mode.

---

## **State Writes**

### **Named Decision**

Records:

- **result** — boolean outcome
- **branch** — "then" or "else"
- **effects** — Effects from the selected branch
- **meta** — evaluation metadata

### **Transparent Control**

- No wrapper record is written
- Only branch Effects are recorded and merged into the parent Dynamic

---

## **Determinism**

Conditional execution is structurally deterministic.

- Exactly one branch executes
- Non-selected branches produce no effects
- Execution topology is fixed by the orchestration definition

Model variability affects only the decision value, not execution structure.

---

## **Conditional Usage Examples**

### **Example 1: Cybernetic Named Decision**

```
type: if
name: is_clear
if:
  mode: model
  template: "Is this question clear?\n{{input.question}}"
then:
  - type: prompt
    name: answer
    template: "Answer:\n{{input.question}}"
else:
  - type: prompt
    name: clarify
    template: "Please clarify your question."
```

---

### **Example 2: Deterministic Transparent Control (CEL)**

```
type: if
if:
  mode: cel
  expr: "state.input.is_admin == true"
then:
  - type: prompt
    name: show_admin_panel
    template: "Render admin view."
else:
  - type: prompt
    name: show_user_panel
    template: "Render user view."
```

---

## **Conceptual Role**

A Conditional is a control construct implemented through composition.

Cybernetic evaluation adapts decisions to state.

CEL evaluation enforces strict logic.

Selectors gate execution.

Dynamics structure execution.

Together, they provide explicit, minimal, and auditable execution-time branching while keeping the runtime small and deterministic.