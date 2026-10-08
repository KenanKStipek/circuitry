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
//!
//! A `Value::Int` too large for CEL's 64-bit `int`, if the expression
//! actually reads it, is a `CelError` from both entry points
//! (`convert::Overflow`, [`CelError::is_overflow`]). For
//! [`evaluate_condition`] this matches Python exactly: `_project`
//! narrows what `_to_cel` ever sees to what the expression reads, so an
//! unread big int elsewhere in `state` never raises, and one the
//! expression *does* read is `ValueError("overflow")`, wrapped into
//! `CelEvaluationError` the same as any other evaluation failure.
//!
//! For [`evaluate_expect`] this is a deliberate, evidence-backed
//! deviation, not parity: `evaluate_cel_expect` in `core/cel_eval.py`
//! converts `value`/`meta`/`state` *before* its own `try`/`except`, so
//! the matching `ValueError` escapes **uncaught** — it is not a
//! `CelEvaluationError`, and the two call sites that invoke
//! `core.expect` handle that raw `ValueError` differently from each
//! other and from a failed expectation:
//!
//! - `core.tool`'s attempt loop (`tool.py`) catches every exception from
//!   the attempt, `ValueError` included: the attempt fails and is
//!   retried up to the step's `retry:` policy, `meta.error` becomes
//!   `"overflow"`, and `meta.expect` is never set (a failed expectation
//!   sets it). `on_error: fail` then re-raises the bare `ValueError`.
//! - `core.use` (`use.py`) catches it too, but re-raises a `ValueError`
//!   **immediately, with no retry** — `meta.error` is `"overflow"`, and
//!   `on_error: fail`'s message is `"use '<name>' -> <label>: overflow"`.
//!
//! Both differ from a genuinely failed expectation (`meta.expect.error`
//! set, `meta.error` becoming `"expect failed: <summary>"`, retried up
//! to `max_attempts` for `use`). This crate raises a `CelError` here —
//! the same shape [`evaluate_condition`] already uses for its own
//! overflow — rather than reproducing either Python outcome exactly;
//! [`CelError::is_overflow`] lets a future caller distinguish this case
//! and reproduce whichever of the two Python behaviours it is wiring up
//! to (tool vs. use) if that parity ever matters, rather than baking in
//! a guess now.
//!
//! Known divergence: a `Value::Dict` key with no CEL
//! [`cel::objects::Key`] counterpart (anything other than
//! `bool`/`int`/`string` — a `float`, a `None`, a bare `date`, a nested
//! `list`/`dict`) is **dropped** by `convert::to_cel` rather than mapped
//! to some placeholder. This is reachable from an ordinary
//! orchestration, not just a key buried deep in `value`/`meta`: a YAML
//! input file's `2026-01-01: x` is a date key, and PyYAML resolves an
//! unquoted float- or null-looking key the same way, so `state.input`
//! can hold any of them directly. Circuitry's own `_to_cel`
//! (`core/cel_eval.py`) instead converts such a key to `None` (or keeps
//! a float/datetime key as its own CEL-less type) —
//! `celtypes.MapType.__setitem__` never validates keys — so several
//! date keys collapse into a single `null` key holding the last value,
//! and `size()` counts it. `cel::objects::Key` cannot represent `null`
//! (or a float, or a nested container) as a map key at all, so
//! reproducing Python's collapse-to-one-null-key behavior isn't an
//! option here; dropping the entry is the least-wrong of the choices
//! actually available. A `Value::Int` too large for `i64` as a dict key
//! raises `convert::Overflow` instead of being dropped, matching every
//! other big-int read.
//!
//! `has(X)` is rewritten, at parse time, into
//! `!@not_strictly_false(__electricity_false(X))` (`macros` module
//! docs) — exactly cel-python's own rule, "the argument evaluated
//! without error", for *any* `X`: a missing key, an out-of-range or
//! wrongly-typed list index, a selection through the wrong type, a
//! failing index *expression*, a chain rooted at any identifier
//! (including one not bound in the current mode, or a comprehension's
//! own loop variable) or at a macro/function-call result, and an
//! argument that was never a field selection to begin with
//! (`has(state.items[0])`, which `cel`'s own `has()` macro fails to
//! even *parse*). Nothing here is a narrower, undocumented divergence
//! the way an earlier safe-navigation-based rewrite's two gaps were —
//! this rewrite doesn't walk `X` step by step at all, so there's
//! nothing for it to leave a gap in.
//!
//! Indexing (`state.l[i]`, a string or bytes, inside `has()` or not) is
//! celpy's own `__getitem__`: for a `list`/`string`/`bytes`, a negative
//! *i* wraps from the end, a `bool` is `0`/`1`, and a `double` is
//! rejected outright, even a whole-number one — all different from
//! `cel`'s own, native indexing, which rejects negative and accepts a
//! whole-number `double`, and has no indexer at all for a bare string
//! or byte string (`indexing` module docs). Map indexing uses a strict
//! (non-cross-converting) key match, unlike `cel`'s own
//! [`cel::objects::Map::get`]: celpy's own `MapType.__getitem__` raises
//! on an int/uint key mismatch rather than finding the entry
//! (`indexing` module docs). One narrower case stays a known,
//! documented divergence: a `bool` key against an `int`/`uint` lookup
//! (or the reverse) doesn't reproduce celpy's own asymmetry — found
//! only when an `int`/`uint` probes a `bool`-keyed entry, not the
//! reverse (`indexing` module docs, pinned by a Rust unit test rather
//! than the differential corpus, which can't express it either).
//!
//! `in` over a bare `string`/`bytes` operand iterates its characters or
//! bytes, matching celpy's own `for c in container`; `cel`'s own `@in`
//! has no indexer for either at all (`equality` module docs). A nested
//! `list`/`map` comparison (reachable only through `in`) compares every
//! element before deciding, rather than stopping at the first one that
//! raises — celpy's own container equality folds its elements with
//! CEL's error-absorbing `&&`, where a `false` anywhere wins over an
//! error anywhere else, and a `Map` comparison checks both sides hold
//! the same key set before comparing any value, so neither result can
//! depend on a `HashMap`'s own unspecified iteration order
//! (`equality` module docs).
//!
//! The `map` macro only accepts the two-argument form
//! (`target.map(x, transform)`): the `cel` crate's own copied expander
//! (`macros` module docs) also accepts a third, filter argument
//! (`target.map(x, predicate, transform)`), but cel-python's own `map`
//! raises on three arguments, so this crate doesn't register that
//! shape. The call still *parses* — `cel`'s own `env.compile` leaves
//! it as an unexpanded, undeclared `map` call rather than rejecting the
//! arity outright — but resolving it then fails, because the loop
//! variable it names was never bound by any comprehension: an error
//! raised when the expression is evaluated, matching cel-python's own
//! outcome if not its exact wording or timing.

mod convert;
mod equality;
mod indexing;
mod macros;
mod ordering;
mod paths;

use std::fmt;
use std::sync::{Arc, OnceLock};

use cel::{Context, Env};
use electricity_value::Value;

/// Python's `repr()` of a `str`, used to match `core/cel_eval.py`'s own
/// `{expr!r}`/`{unresolved!r}` messages word for word: a message
/// containing a `'` (`state.x == 'open'`) gets double quotes, and
/// backslashes/newlines are escaped, neither of which a bare `'{expr}'`
/// does.
fn py_repr(s: &str) -> String {
    Value::Str(s.to_string()).py_repr()
}

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
    overflow: bool,
}

impl CelError {
    fn new(expression: &str, message: String) -> Self {
        CelError {
            message,
            expression: expression.to_string(),
            overflow: false,
        }
    }

    /// The CEL expression that failed.
    pub fn expression(&self) -> &str {
        &self.expression
    }

    /// Whether this is a big-int-overflow failure (`convert::Overflow`)
    /// rather than a parse/compile/evaluation failure — see the module
    /// docs' note on [`evaluate_expect`]'s big-int deviation: a future
    /// caller that wants to reproduce Circuitry's own `core.tool`/
    /// `core.use` handling of that specific case, rather than treating
    /// it like any other `CelError`, can branch on this.
    pub fn is_overflow(&self) -> bool {
        self.overflow
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
    Arc::clone(ENV.get_or_init(|| Arc::new(macros::env())))
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
    env.compile(expr).map_err(|e| {
        CelError::new(
            expr,
            format!("CEL evaluation failed for {}: {e}", py_repr(expr)),
        )
    })
}

/// `core/cel_eval.py`'s `ValueError("overflow")` for a `Value::Int` too
/// large for CEL's 64-bit `int`, wrapped the same way a genuine
/// evaluation failure is.
fn overflow_err(expr: &str) -> CelError {
    CelError {
        overflow: true,
        ..CelError::new(
            expr,
            format!("CEL evaluation failed for {}: overflow", py_repr(expr)),
        )
    }
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
        // `celtypes.DurationType` subclasses `datetime.timedelta`, whose
        // `__bool__` is "nonzero", not "always true" — a duration this
        // falsy is reachable (subtracting two equal timestamps).
        cel::Value::Duration(d) => !d.is_zero(),
        cel::Value::Timestamp(_)
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
                    "CEL expression {} reads unset state path {} and is marked strict.",
                    py_repr(expr),
                    py_repr(unresolved)
                ),
            ));
        }
        log::warn!(
            "CEL expression {} reads unset state path {}; condition is false",
            py_repr(expr),
            py_repr(unresolved)
        );
        return Ok(false);
    }

    // Narrowed to what the expression actually reads (`_project` in
    // `core/cel_eval.py`): a big int elsewhere in `state`, unread, must
    // not make the conversion below fail (finding 3), and converting is
    // cheaper for a large `state` than a handful of paths warrant.
    let projected_state = if collected.reads_whole_state {
        state.clone()
    } else {
        paths::project(state, &collected.paths)
    };

    let mut tree = program.expression().clone();
    paths::rewrite(&mut tree);

    let mut ctx = Context::with_env(Arc::clone(&env));
    ordering::register(&mut ctx);
    equality::register(&mut ctx);
    indexing::register(&mut ctx);
    macros::register(&mut ctx);
    ctx.add_variable_from_value(
        "state",
        convert::to_cel(&projected_state).map_err(|_| overflow_err(expr))?,
    );
    let result = cel::Value::resolve(&tree, &ctx).map_err(|e| {
        CelError::new(
            expr,
            format!("CEL evaluation failed for {}: {e}", py_repr(expr)),
        )
    })?;
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

    let mut tree = program.expression().clone();
    paths::rewrite(&mut tree);

    let mut ctx = Context::with_env(Arc::clone(&env));
    ordering::register(&mut ctx);
    equality::register(&mut ctx);
    indexing::register(&mut ctx);
    macros::register(&mut ctx);
    ctx.add_variable_from_value(
        "value",
        convert::to_cel(value).map_err(|_| overflow_err(expr))?,
    );
    ctx.add_variable_from_value(
        "meta",
        convert::to_cel(meta).map_err(|_| overflow_err(expr))?,
    );
    ctx.add_variable_from_value(
        "state",
        convert::to_cel(state).map_err(|_| overflow_err(expr))?,
    );
    let result = cel::Value::resolve(&tree, &ctx).map_err(|e| {
        CelError::new(
            expr,
            format!("CEL evaluation failed for {}: {e}", py_repr(expr)),
        )
    })?;
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
        // rewriting `has(...)` into a literal (`paths::rewrite`) this
        // raises `NoSuchKey` instead of letting `has()` report `false`
        // the way cel-python's own (whole-chain) `has()` does.
        let empty = Value::Dict(Dict::new());
        let expr = "!has(state.input.n) || state.input.n > 1";
        assert_eq!(evaluate_condition(expr, &empty, false), Ok(true));
    }

    #[test]
    fn has_does_not_leak_a_fabricated_parent_into_a_later_read() {
        // A prior version of this fix made `has()` work by inserting
        // empty dicts into a clone of `state` so `cel`'s own, less
        // graceful `has()` wouldn't hit a missing key — and that
        // fabricated `{}` leaked into every other read of the same path,
        // including a second `has()` on a longer chain through it.
        let empty = Value::Dict(Dict::new());
        let expr = "!has(state.a.b) || has(state.a.b.c)";
        assert_eq!(evaluate_condition(expr, &empty, false), Ok(true));
    }

    #[test]
    fn has_on_a_disabled_node_is_false_not_an_error() {
        // A disabled node writes `{"value": None}`; has() selecting a
        // further field through that `None` is exactly the case `cel`'s
        // own has() raises `NoSuchKey`/an overload error on instead of
        // returning `false` the way cel-python's does.
        let state = dict(vec![(
            "prime",
            dict(vec![("x", dict(vec![("value", Value::None)]))]),
        )]);
        assert_eq!(
            evaluate_condition("has(state.prime.x.value.price)", &state, false),
            Ok(false)
        );
    }

    #[test]
    fn big_int_read_by_the_expression_raises() {
        let huge: num_bigint::BigInt = "100000000000000000000".parse().unwrap();
        let state = dict(vec![("n", Value::from(huge))]);
        assert!(evaluate_condition("state.n == null", &state, false).is_err());
    }

    #[test]
    fn big_int_not_read_by_the_expression_does_not_raise() {
        // The conversion is narrowed to what the expression reads
        // (`paths::project`): an unrelated big int elsewhere in `state`
        // must not fail a condition that never looks at it.
        let huge: num_bigint::BigInt = "100000000000000000000".parse().unwrap();
        let state = dict(vec![
            ("n", Value::from(huge)),
            ("input", dict(vec![("ok", Value::Bool(true))])),
        ]);
        assert_eq!(
            evaluate_condition("state.input.ok == true", &state, false),
            Ok(true)
        );
    }

    #[test]
    fn cross_numeric_type_ordering_raises() {
        let state = dict(vec![("n", Value::from(1_i64))]);
        assert!(evaluate_condition("state.n < 1.5", &state, false).is_err());
    }

    #[test]
    fn same_numeric_type_ordering_still_works() {
        let state = dict(vec![("n", Value::from(1_i64))]);
        assert_eq!(evaluate_condition("state.n < 2", &state, false), Ok(true));
    }

    #[test]
    fn strict_message_matches_python_repr_for_a_quoted_expression() {
        let state = Value::Dict(Dict::new());
        let err = evaluate_condition("state.prime.tick.value == 'open'", &state, true).unwrap_err();
        assert_eq!(
            err.to_string(),
            "CEL expression \"state.prime.tick.value == 'open'\" reads unset state path \
             'state.prime.tick.value' and is marked strict."
        );
    }

    #[test]
    fn heterogeneous_equality() {
        let state = Value::Dict(Dict::new());
        assert_eq!(evaluate_condition("1 == true", &state, false), Ok(false));
        assert_eq!(evaluate_condition("1 == 1.0", &state, false), Ok(true));
        assert_eq!(evaluate_condition("'a' == 1", &state, false), Ok(false));
    }

    #[test]
    fn zero_duration_is_falsy() {
        // `celtypes.DurationType` subclasses `datetime.timedelta`, whose
        // `__bool__` is "nonzero" — `bool(timedelta(0))` is `False`, not
        // always `True` the way every other non-numeric CEL type is here.
        use chrono::{NaiveDate, NaiveDateTime};
        let t = |hour: u32| -> Value {
            let naive: NaiveDateTime = NaiveDate::from_ymd_opt(2020, 1, 1)
                .unwrap()
                .and_hms_opt(hour, 0, 0)
                .unwrap();
            Value::DateTime(naive, None)
        };
        let state = dict(vec![("a", t(10)), ("b", t(10))]);
        assert_eq!(
            evaluate_condition("state.a - state.b", &state, false),
            Ok(false)
        );
        let state = dict(vec![("a", t(11)), ("b", t(10))]);
        assert_eq!(
            evaluate_condition("state.a - state.b", &state, false),
            Ok(true)
        );
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
    fn negative_list_index_wraps_from_the_end() {
        let state = dict(vec![(
            "h",
            Value::List(vec![
                dict(vec![("s", Value::Str("x".into()))]),
                dict(vec![("s", Value::Str("done".into()))]),
            ]),
        )]);
        assert_eq!(
            evaluate_condition("state.h[-1].s == 'done'", &state, false),
            Ok(true)
        );
    }

    #[test]
    fn double_list_index_raises() {
        let state = dict(vec![(
            "l",
            Value::List(vec![Value::from(1_i64), Value::from(2_i64)]),
        )]);
        assert!(evaluate_condition("state.l[0.0] == 1", &state, false).is_err());
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
