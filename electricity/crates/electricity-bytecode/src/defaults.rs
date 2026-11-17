//! Default values applied as Circuitry applies them (issue
//! #408's Scope section). The compiler reads these when a document
//! omits the corresponding field; this module only names them so every
//! consumer applies the same number.

/// `ConditionalDefinition.threshold`.
pub const IF_THRESHOLD: f64 = 0.5;
/// `ReflectorDefinition.plan_from_step`.
pub const PLAN_FROM_STEP: &str = "propose_steps";
/// `ReflectorDefinition.max_effects`.
pub const MAX_EFFECTS: u32 = 8;
/// `ReflectorDefinition.generated_key`.
pub const GENERATED_KEY: &str = "generated";
/// `ReflectorDefinition.max_iterations`.
pub const REFLECTOR_MAX_ITERATIONS: u32 = 1;
/// `LoopEachDef.as_name`.
pub const EACH_AS: &str = "item";
/// `RetryPolicyDef.backoff_ms`.
pub const RETRY_BACKOFF_MS: u32 = 1000;
/// `RetryPolicyDef.max_attempts`.
pub const RETRY_MAX_ATTEMPTS: u32 = 1;
