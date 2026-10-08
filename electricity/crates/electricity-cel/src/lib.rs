//! CEL evaluation for electricity, matching Circuitry's own
//! `core/cel_eval.py` (`evaluate_cel`/`evaluate_cel_expect`) rather than
//! generic CEL behavior (DESIGN.md §7.2, runtime-semantics.md §4).
//!
//! This crate wraps the [`cel`] crate (chosen over `cel-core`, see
//! `DESIGN.md` §7.2) behind Circuitry's own strict/non-strict layer:
//!
//! - [`evaluate_condition`] — `if`/`while` CEL mode. Binds one root,
//!   `state`. An unset `state.` path the expression reads makes the
//!   whole expression `false` (logged, never silently swallowed),
//!   decided *structurally* by walking the parse tree before evaluating
//!   — not by catching a failure during evaluation. Two exemptions:
//!   `has(...)` guards the one path it names, and `strict: true` turns
//!   the same case back into an error.
//! - [`evaluate_expect`] — tool/use `expect:` CEL. Binds three roots,
//!   `value`, `meta` (this effect's own outcome, unprefixed) and `state`.
//!   Gets none of the absent-path exemption above: an unresolved path
//!   raises like any other evaluation failure, which the caller treats
//!   as a failed expectation (Quirk Q6, runtime-semantics.md §4.4/§9).
//!
//! Both entry points enforce the 4096-character expression cap and
//! reject an empty/whitespace-only expression, with Circuitry's own
//! wording (word-for-word with `core/cel_eval.py`, not `cel`'s).

mod convert;
mod paths;

use std::fmt;
use std::sync::{Arc, OnceLock};

use cel::{Context, Env};
use electricity_value::Value;

/// `core/cel_eval.py`'s `_MAX_EXPR_LENGTH`.
pub const MAX_EXPR_LENGTH: usize = 4096;

/// A CEL expression failed to parse or evaluate — or was rejected before
/// either, for being empty or over the length cap.
///
/// Carries the offending `expression`, like `core/cel_eval.py`'s own
/// `CelError` base class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CelError {
    message: String,
    expression: String,
}

impl CelError {
    fn new(expression: &str, message: String) -> Self {
        CelError {
            message,
            expression: expression.to_string(),
        }
    }

    /// The CEL expression that failed.
    pub fn expression(&self) -> &str {
        &self.expression
    }
}

impl fmt::Display for CelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for CelError {}

fn stdlib_env() -> Arc<Env> {
    static ENV: OnceLock<Arc<Env>> = OnceLock::new();
    Arc::clone(ENV.get_or_init(|| Arc::new(Env::stdlib())))
}

/// Rejects an empty/whitespace-only or over-long expression with
/// Circuitry's own wording, matching `evaluate_cel`/`evaluate_cel_expect`
/// in `core/cel_eval.py` exactly (not text from `cel` itself).
fn check_length(expr: &str) -> Result<(), CelError> {
    if expr.trim().is_empty() {
        return Err(CelError::new(
            expr,
            "CEL expression is empty; nothing to evaluate.".to_string(),
        ));
    }
    let len = expr.chars().count();
    if len > MAX_EXPR_LENGTH {
        return Err(CelError::new(
            expr,
            format!("CEL expression too long ({len} chars, max {MAX_EXPR_LENGTH})."),
        ));
    }
    Ok(())
}

fn compile(env: &Env, expr: &str) -> Result<cel::Program, CelError> {
    env.compile(expr)
        .map_err(|e| CelError::new(expr, format!("CEL evaluation failed for '{expr}': {e}")))
}

/// Python `bool(result)` applied to whatever a CEL expression evaluated
/// to: `if`/`while` always expect a CEL `bool`, but `evaluate_cel` coerces
/// generically rather than requiring one, so this does too.
fn truthy(value: &cel::Value) -> bool {
    match value {
        cel::Value::Bool(b) => *b,
        cel::Value::Int(n) => *n != 0,
        cel::Value::UInt(n) => *n != 0,
        cel::Value::Float(f) => *f != 0.0,
        cel::Value::String(s) => !s.is_empty(),
        cel::Value::Bytes(b) => !b.is_empty(),
        cel::Value::List(l) => !l.is_empty(),
        cel::Value::Map(m) => !m.map.is_empty(),
        cel::Value::Null => false,
        cel::Value::Timestamp(_)
        | cel::Value::Duration(_)
        | cel::Value::Struct(_)
        | cel::Value::Opaque(_)
        | cel::Value::Function(..) => true,
    }
}

/// Evaluates an `if`/`while` CEL expression against `state`, and returns
/// a bool.
///
/// An unset `state.` path the expression reads unguarded makes the whole
/// expression `false` (a warning is logged naming the path, through the
/// `log` facade — nothing swallows it), unless `strict` is `true`, which
/// turns the same case into `Err`. `has(...)` always exempts the path it
/// names, strict or not.
///
/// # Errors
///
/// An empty/over-long/unparseable expression, an unknown function, or a
/// genuine evaluation failure (a real type error, calling a method on the
/// wrong type) all return `Err`. A malformed expression never silently
/// evaluates to `false` — that would be indistinguishable from a
/// legitimately false condition.
pub fn evaluate_condition(expr: &str, state: &Value, strict: bool) -> Result<bool, CelError> {
    check_length(expr)?;
    let env = stdlib_env();
    let program = compile(&env, expr)?;

    let collected = paths::collect_state_paths(program.expression());
    if let Some(unresolved) = paths::first_unresolved(&collected.paths, state) {
        if strict {
            return Err(CelError::new(
                expr,
                format!(
                    "CEL expression '{expr}' reads unset state path '{unresolved}' and is marked strict."
                ),
            ));
        }
        log::warn!(
            "CEL expression {expr:?} reads unset state path {unresolved:?}; condition is false"
        );
        return Ok(false);
    }

    let mut patched_state = state.clone();
    for chain in paths::guarded_chains(program.expression()) {
        if chain[0] == "state" {
            paths::ensure_has_target_parents(&mut patched_state, &chain);
        }
    }

    let mut ctx = Context::with_env(Arc::clone(&env));
    ctx.add_variable_from_value("state", convert::to_cel(&patched_state));
    let result = program
        .execute(&ctx)
        .map_err(|e| CelError::new(expr, format!("CEL evaluation failed for '{expr}': {e}")))?;
    Ok(truthy(&result))
}

/// Evaluates a tool/use `expect:` CEL expression and returns a bool.
///
/// Three root bindings, not one: `value` and `meta` are this effect's own
/// result, and `state` is the full run state, exactly as
/// [`evaluate_condition`] binds it. Unlike `evaluate_condition`, an
/// unresolved path here — `value.prompt_id` on a value with no such key —
/// raises the same as any other evaluation failure; there is no
/// absent-path-is-false convention for `expect:` (runtime-semantics.md
/// §4.4/§9, Quirk Q6).
///
/// # Errors
///
/// Same as [`evaluate_condition`], plus an unresolved `value`/`meta`/
/// `state` path.
pub fn evaluate_expect(
    expr: &str,
    value: &Value,
    meta: &Value,
    state: &Value,
) -> Result<bool, CelError> {
    check_length(expr)?;
    let env = stdlib_env();
    let program = compile(&env, expr)?;

    let mut patched_value = value.clone();
    let mut patched_meta = meta.clone();
    let mut patched_state = state.clone();
    for chain in paths::guarded_chains(program.expression()) {
        match chain[0].as_str() {
            "value" => paths::ensure_has_target_parents(&mut patched_value, &chain),
            "meta" => paths::ensure_has_target_parents(&mut patched_meta, &chain),
            "state" => paths::ensure_has_target_parents(&mut patched_state, &chain),
            _ => {}
        }
    }

    let mut ctx = Context::with_env(Arc::clone(&env));
    ctx.add_variable_from_value("value", convert::to_cel(&patched_value));
    ctx.add_variable_from_value("meta", convert::to_cel(&patched_meta));
    ctx.add_variable_from_value("state", convert::to_cel(&patched_state));
    let result = program
        .execute(&ctx)
        .map_err(|e| CelError::new(expr, format!("CEL evaluation failed for '{expr}': {e}")))?;
    Ok(truthy(&result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::Dict;

    fn dict(pairs: Vec<(&str, Value)>) -> Value {
        let mut d = Dict::new();
        for (k, v) in pairs {
            d.insert(Value::Str(k.to_string()), v);
        }
        Value::Dict(d)
    }

    #[test]
    fn simple_equality() {
        let state = dict(vec![("input", dict(vec![("ok", Value::Bool(true))]))]);
        assert_eq!(
            evaluate_condition("state.input.ok == true", &state, false),
            Ok(true)
        );
    }

    #[test]
    fn empty_expression_rejected() {
        let err = evaluate_condition("", &Value::Dict(Dict::new()), false).unwrap_err();
        assert_eq!(
            err.to_string(),
            "CEL expression is empty; nothing to evaluate."
        );
    }

    #[test]
    fn whitespace_expression_rejected() {
        let err = evaluate_condition("   ", &Value::Dict(Dict::new()), false).unwrap_err();
        assert_eq!(
            err.to_string(),
            "CEL expression is empty; nothing to evaluate."
        );
    }

    #[test]
    fn too_long_expression_rejected() {
        let expr = "state.x == 'a'".to_string() + &" && state.x == 'a'".repeat(500);
        assert!(expr.chars().count() > MAX_EXPR_LENGTH);
        let err = evaluate_condition(&expr, &Value::Dict(Dict::new()), false).unwrap_err();
        assert!(err.to_string().contains("too long"));
    }

    #[test]
    fn absent_path_is_false_by_default() {
        let state = Value::Dict(Dict::new());
        assert_eq!(
            evaluate_condition("state.prime.typo.value == 1", &state, false),
            Ok(false)
        );
    }

    #[test]
    fn absent_path_raises_under_strict() {
        let state = Value::Dict(Dict::new());
        let err = evaluate_condition("state.prime.tick.value <= 10", &state, true).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("state.prime.tick.value"));
        assert!(message.contains("strict"));
    }

    #[test]
    fn has_guard_is_exempt() {
        let empty = Value::Dict(Dict::new());
        let present = dict(vec![("input", dict(vec![("n", Value::from(5_i64))]))]);
        let expr = "has(state.input.n) && state.input.n > 1";
        assert_eq!(evaluate_condition(expr, &present, false), Ok(true));
        assert_eq!(evaluate_condition(expr, &empty, false), Ok(false));
    }

    #[test]
    fn has_guard_tolerates_a_doubly_missing_path() {
        // `state` has no "input" key at all, not just a missing "n" --
        // `cel`'s own has() only guards its *last* segment, so without
        // `paths::ensure_has_target_parents` this raises `NoSuchKey`
        // instead of letting `has()` report `false` the way cel-python's
        // own (whole-chain) `has()` does.
        let empty = Value::Dict(Dict::new());
        let expr = "!has(state.input.n) || state.input.n > 1";
        assert_eq!(evaluate_condition(expr, &empty, false), Ok(true));
    }

    #[test]
    fn heterogeneous_equality() {
        let state = Value::Dict(Dict::new());
        assert_eq!(evaluate_condition("1 == true", &state, false), Ok(false));
        assert_eq!(evaluate_condition("1 == 1.0", &state, false), Ok(true));
        assert_eq!(evaluate_condition("'a' == 1", &state, false), Ok(false));
    }

    #[test]
    fn expect_raises_on_unresolved_path() {
        let value = Value::Dict(Dict::new());
        let meta = Value::Dict(Dict::new());
        let state = Value::Dict(Dict::new());
        assert!(evaluate_expect("has(value.prompt_id)", &value, &meta, &state).is_ok());
        assert!(evaluate_expect("value.prompt_id == 1", &value, &meta, &state).is_err());
    }

    #[test]
    fn expect_binds_value_meta_and_state() {
        let value = dict(vec![("prompt_id", Value::from(1_i64))]);
        let meta = dict(vec![("ok", Value::Bool(true))]);
        let state = dict(vec![("input", dict(vec![("n", Value::from(2_i64))]))]);
        let expr = "value.prompt_id == 1 && meta.ok && state.input.n == 2";
        assert_eq!(evaluate_expect(expr, &value, &meta, &state), Ok(true));
    }

    #[test]
    fn unknown_function_raises() {
        let state = dict(vec![("a", Value::from(1_i64))]);
        assert!(evaluate_condition("nope(state.a)", &state, false).is_err());
    }

    #[test]
    fn matches_uses_the_regex_crate() {
        let state = dict(vec![(
            "input",
            dict(vec![("s", Value::Str("hello world".into()))]),
        )]);
        assert_eq!(
            evaluate_condition("state.input.s.matches('^h.*d$')", &state, false),
            Ok(true)
        );
    }
}
