# Circuitry Type System

---

This document defines the canonical object shapes for Circuitry’s orchestration language and runtime records.

Circuitry is intentionally split into two worlds.

The orchestration world is declarative. It defines the executable plan in a data structure (the “domain object language”, DOL). DOL nodes define topology, control constructs, and model invocations, but they do not execute anything on their own.

The runtime world is deterministic and append-only. It executes the plan exactly as defined and records Effects and metadata into hierarchical State.

This type system is designed to make that separation explicit.

---

## **1. Shared Concepts**

### **1.1 Node identity and addressing**

Every executable unit is “named” within a parent scope. Names form a deterministic hierarchical address, used both for state writes and template interpolation.

A node’s fully qualified path is conceptually:

```
<primeDynamicName>.<childName>.<grandchildName>...
```

Paths are used for interpolation (Mustache-style rendering) and for state retrieval. Prompts and Dynamics write to runtime state at deterministic paths derived from their name and parent scope.

### **1.2 Effect**

An Effect is the runtime record of “what happened” when a node executed. Effects are immutable runtime outputs containing value(s) and meta.

Effects exist for Prompts, Dynamics, Conditionals, and Loops.

### **1.3 Meta**

Meta is the execution metadata for an Effect. Meta is append-only, auditable, and includes timestamps, error information, model/provider identity, and token usage (where applicable).

---

## **2. DOL Types (Definition-Time)**

These objects describe the orchestration plan.

### **2.1 Orchestration document**

A DOL “file” (or top-level object) typically declares a Prime Dynamic. The document may also include global defaults (model/provider/adapter policy), though those global fields are implementation details of the runtime integration layer.

```
export type OrchestrationDoc = {
  version?: number;
  prime: DynamicDef;
  defaults?: DefaultsDef;
};

export type DefaultsDef = {
  model?: string;
  provider?: string;
  provider_fallbacks?: string[];
  params?: Record<string, unknown>;
  timeout_ms?: number;
  deterministic?: boolean;
  stop_on_error?: boolean;
};
```

Prime is the entry point into the runtime: Prime receives State and a Prime Dynamic, executes it, and returns updated State.

### **2.2 Effect definitions**

A Dynamic’s “effects” are a collection of executable nodes: Prompt, Dynamic, Conditional, Loop.

```
export type EffectDef = PromptDef | DynamicDef | ConditionalDef | LoopDef;
```

---

## **3. Prompt Types**

A Prompt is the atomic execution unit. It performs exactly one model invocation, producing a typed result and execution metadata.

### **3.1 PromptDef**

The Prompt DOL definition includes required identity fields and exactly one primary input form: template or messages.

```
export type PromptDef = {
  type: "prompt";
  name: string;

  description?: string;

  // typing and decoding
  prompt_type?: PromptType;
  schema?: JsonSchema; // optional unless prompt_type implies structured output

  // model configuration
  model?: string;
  provider?: string;
  provider_fallbacks?: string[];

  // execution parameters
  params?: Record<string, unknown>;
  timeout_ms?: number;
  deterministic?: boolean;

  // exactly one of these must be provided
  template?: string;
  messages?: Array<MessageDef>;

  // prompt-local structured values available in effective context
  inputs?: Record<string, unknown>;

  // non-text inputs
  assets?: Array<AssetRefDef>;

  // reliability
  retries?: RetryPolicyDef;
  on_error?: OnErrorPolicy;
};
```

PromptDef validation rules include: type must be "prompt", name must be unique among siblings, and exactly one of template or messages must be present.

### **3.2 PromptType**

PromptType constrains the expected shape of the output and informs decoding/validation/persistence. Prompt Types exist specifically to make Prompt results predictable and safe to interpolate.

```
export type PromptType =
  | "text"
  | "json"
  | "boolean"
  | "tool"
  | "number"
  | "array"
  | "object";
```

The docs emphasize JSON schema as a validation mechanism for structured outputs.

### **3.3 Messages and assets**

Prompt inputs may be pure template text, role-based messages, and optionally assets.

```
export type MessageDef = {
  role: "system" | "user" | "assistant" | "tool";
  content: string;
};

export type AssetRefDef = {
  kind: string; // e.g. "image", "file", "audio"
  ref: string;  // a resolvable id/path/uri in your runtime
};
```

### **3.4 Retry policy and error policy**

Prompts may declare retries and error behavior without altering the meaning of the Prompt itself.

```
export type RetryPolicyDef = {
  max_attempts?: number;
  backoff_ms?: number | { base_ms: number; max_ms: number; factor?: number };
};

export type OnErrorPolicy = "fail" | "skip" | "continue" | "break";
```

---

## **4. Dynamic Types**

A Dynamic composes Prompts, Conditionals, Loops, and other Dynamics into a single executable unit, declares execution topology, aggregates metadata, and records a Dynamic execution record to state. Dynamics do not plan or decide what happens next.

### **4.1 DynamicDef**

```
export type DynamicDef = {
  type: "dynamic";
  name: string;

  description?: string;

  // execution topology
  flow: FlowModel;

  // collection of effects executed under this dynamic
  effects: EffectDef[];

  // bounded concurrency
  max_concurrency?: number;

  // error behavior
  stop_on_error?: boolean;
  on_error?: OnErrorPolicy;

  // annotations
  labels?: Record<string, unknown>;
};
```

Dynamic validation includes: effects must be non-empty, name unique within scope, flow must resolve to supported model.

### **4.2 FlowModel and normalization**

Dynamics support explicit control flow models: chain and tree, plus alias forms that normalize to canonical behaviors. Aliases are readability/documentation only; they do not change runtime behavior.

```
export type FlowModel =
  | "chain"
  | "chain_of_thought"
  | "cot"
  | "tree"
  | "tree_of_thought"
  | "tot";

export type CanonicalFlow = "chain" | "tree";

export function normalizeFlow(flow: FlowModel): CanonicalFlow {
  if (flow === "chain" || flow === "chain_of_thought" || flow === "cot") return "chain";
  return "tree";
}
```

---

## **5. Conditional Types**

A Conditional evaluates an if condition against state and selects exactly one branch to execute. Non-selected branches produce no effects. It operates entirely within the current Dynamic and does not emit new execution plans.

### **5.1 ConditionalDef**

Your doc indicates type can be conditional or if, with if containing evaluation configuration, and then/else arrays containing effects.

```
export type ConditionalDef = {
  type: "conditional" | "if";
  name?: string;

  description?: string;

  if: ConditionDef;

  then: EffectDef[];
  else?: EffectDef[];

  // evaluation behavior
  mode?: ConditionMode;       // default: model
  threshold?: number;         // for model-based decisions
  on_error?: OnErrorPolicy;

  labels?: Record<string, unknown>;
};
```

The “named vs transparent control” distinction matters for runtime state writes. A named conditional records its decision wrapper. A nameless conditional may merge branch effects directly into the parent Dynamic record.

### **5.2 ConditionDef**

Conditions support cybernetic (model-based) evaluation using a rendered template, or deterministic CEL evaluation against state.

```
export type ConditionMode = "model" | "cel";

export type ConditionDef =
  | { mode?: "model"; template: string }
  | { mode: "cel"; expr: string };
```

CEL expressions evaluate against a single root object named state, requiring explicit access like state.input.role == "admin".

---

## **6. Loop Types**

A Loop repeats a body while a continuation condition remains satisfied, or iterates over a collection. Loops do not plan globally and operate within the current Dynamic.

### **6.1 LoopDef**

```
export type LoopDef = {
  type: "loop";
  name?: string;

  description?: string;

  // continuation configuration: exactly one of these
  while?: LoopWhileDef;
  each?: LoopEachDef;

  // explicit mode can exist, but can also be inferred
  mode?: LoopMode; // "model" | "cel" | "each"

  body: EffectDef[];

  // iteration bounds
  max_iterations?: number;
  min_iterations?: number;

  // error behavior
  on_error?: OnErrorPolicy;

  labels?: Record<string, unknown>;
};
```

Your Loop doc describes “Named Loop vs Transparent Control”: named loops record wrapper metadata such as count and termination reason, while unnamed loops can be transparent control (iterations execute, but wrapper is not recorded).

### **6.2 Continuation types**

Loops support model-based continuation (while with rendered template), CEL continuation (while with expr), and collection iteration (each with a path reference).

```
export type LoopMode = "model" | "cel" | "each";

export type LoopWhileDef =
  | { mode?: "model"; template: string }
  | { mode: "cel"; expr: string };

export type LoopEachDef = {
  in: string;    // dot-delimited path into effective context (resolves to array)
  as?: string;   // binding name for current element, default "item"
};
```

---

## **7. Reflector Types (Planning-Time)**

A Reflector is a planner. It reads State, applies heuristics or directives, and produces a Prime Dynamic. Reflectors do not execute Prompts/Dynamics and do not write to state directly.

### **7.1 Reflector interface**

A Reflector produces either a complete Prime Dynamic or termination (no output).

```
export type ReflectorDirective = {
  // intentionally open; directives express intent, not execution
  kind: string;
  payload?: unknown;
};

export type ReflectorResult =
  | { kind: "prime"; prime: DynamicDef }
  | { kind: "terminate"; reason?: string };

export interface Reflector {
  plan(state: CircuitryState, directives?: ReflectorDirective[]): Promise<ReflectorResult>;
}
```

Iterations are planning cycles and are structural rather than DSL entities.

---

## **8. Runtime Types (Execution-Time)**

These are the materialized objects produced by executing DOL.

### **8.1 CircuitryState**

State is the single source of truth, hierarchical and serializable, containing domain state and runtime state. Circuitry writes only to runtime state; domain state is never mutated implicitly.

The simplest runtime model treats State as two top-level regions:

```
export type CircuitryState = {
  domain?: Record<string, unknown>;
  runtime: RuntimeState;
};
```

### **8.2 RuntimeState, EffectRecord, and deterministic paths**

Runtime state stores Effect records under deterministic paths.

A generic Effect record is:

```
export type EffectRecord<V = unknown, M = Record<string, unknown>> = {
  value?: V;
  meta: M;
};
```

The exact fields depend on effect type (Prompt/Dynamic/Conditional/Loop).

---

## **9. Prompt Effect Records**

A Prompt writes value and meta under its deterministic path. Metadata includes timestamps, model/provider identity, token usage, retry details, and errors.

```
export type PromptEffect<V = unknown> = EffectRecord<V, PromptMeta>;

export type PromptMeta = {
  created_at?: string;    // ISO
  invoked_at?: string;
  completed_at?: string;

  model?: string;
  provider?: string;

  // the fully materialized prompt input that was sent
  prompt_sent?: string | { messages: Array<{ role: string; content: string }> };

  tokens_sent?: number;
  tokens_received?: number;

  retries?: {
    attempts?: number;
    backoff_ms?: number | Record<string, unknown>;
  };

  error?: {
    message: string;
    kind?: string;
    retriable?: boolean;
  };
};
```

---

## **10. Dynamic Effect Records**

A Dynamic writes a single record containing aggregated meta and a collection of child effects. A Dynamic produces no direct scalar value; its “result” is the effects it recorded.

```
export type DynamicEffect = EffectRecord<DynamicValue, DynamicMeta>;

export type DynamicValue = {
  effects: Record<string, RuntimeNodeEffect>;
};

export type DynamicMeta = {
  created_at?: string;
  invoked_at?: string;
  completed_at?: string;

  flow?: "chain" | "tree";
  max_concurrency?: number;

  tokens_sent_total?: number;
  tokens_received_total?: number;

  error?: { message: string; kind?: string };
};
```

RuntimeNodeEffect is a union of all execution-time effect record shapes:

```
export type RuntimeNodeEffect =
  | PromptEffect
  | DynamicEffect
  | ConditionalEffect
  | LoopEffect;
```

---

## **11. Conditional Effect Records**

Named conditionals record decision results, branch selection, effects from the selected branch, and evaluation meta. Transparent control conditionals omit wrapper records and only persist branch effects.

```
export type ConditionalEffect = EffectRecord<ConditionalValue, ConditionalMeta>;

export type ConditionalValue = {
  result: boolean;
  branch: "then" | "else";
  effects: Record<string, RuntimeNodeEffect>;
};

export type ConditionalMeta = {
  created_at?: string;
  invoked_at?: string;
  completed_at?: string;

  mode?: "model" | "cel";
  threshold?: number;

  condition_materialized?: string; // rendered template or expr
  decision_raw?: unknown;          // raw model output, if any

  tokens_sent?: number;
  tokens_received?: number;

  error?: { message: string; kind?: string };
};
```

---

## **12. Loop Effect Records**

Named loops record iteration count and termination reason plus the effects produced across iterations. Transparent control loops omit the wrapper record.

```
export type LoopEffect = EffectRecord<LoopValue, LoopMeta>;

export type LoopValue = {
  iterations: number;
  termination: {
    reason: "condition_false" | "max_iterations" | "collection_exhausted" | "error";
    detail?: string;
  };

  // effects recorded per iteration, typically keyed by iteration index
  effects_by_iteration: Array<Record<string, RuntimeNodeEffect>>;
};

export type LoopMeta = {
  created_at?: string;
  invoked_at?: string;
  completed_at?: string;

  mode?: "model" | "cel" | "each";
  condition_materialized?: string; // rendered template or expr, if applicable
  each_in_path?: string;
  each_as?: string;

  tokens_sent_total?: number;
  tokens_received_total?: number;

  error?: { message: string; kind?: string };
};
```

---

## **13. Adapter Types**

Adapters translate Circuitry’s internal execution model into provider/tool-specific requests and normalize responses back for deterministic recording. Adapters do not define orchestration logic.

```
export type AdapterRequest = {
  model?: string;
  provider?: string;
  params?: Record<string, unknown>;
  timeout_ms?: number;

  input:
    | { kind: "template"; text: string }
    | { kind: "messages"; messages: Array<{ role: string; content: string }> };

  assets?: Array<{ kind: string; ref: string }>;
};

export type AdapterResponse = {
  output_raw: unknown;
  output_text?: string;

  tokens_sent?: number;
  tokens_received?: number;

  provider_meta?: Record<string, unknown>;
};
```

---