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
//! (`convert::ConvertError::Overflow`, [`CelError::is_overflow`]). For
//! [`evaluate_condition`] this matches Python exactly: `_project`
//! narrows what `_to_cel` ever sees to what the expression reads, so an
//! unread big int elsewhere in `state` never raises, and one the
//! expression *does* read is `ValueError("overflow")`, wrapped into
//! `CelEvaluationError` the same as any other evaluation failure.
//!
//! For [`evaluate_expect`] this crate still raises a `CelError` — the
//! same shape [`evaluate_condition`] already uses for its own overflow
//! — but `evaluate_cel_expect` in `core/cel_eval.py` converts
//! `value`/`meta`/`state` *before* its own `try`/`except`, so in Python
//! the matching `ValueError` escapes **uncaught**: it is not a
//! `CelEvaluationError`, and the two call sites that invoke
//! `core.expect` handle that raw `ValueError` differently from each
//! other and from a failed expectation:
//!
//! - `core.tool`'s attempt loop (`tool.py`) catches every exception from
//!   the attempt, `ValueError` included: the attempt fails and is
//!   retried up to the step's `retries:` policy, `meta.error` becomes
//!   `"overflow"`, and `meta.expect` is never set (a failed expectation
//!   sets it). `on_error: fail` then re-raises the bare `ValueError`.
//! - `core.use` (`use.py`) catches it too, but re-raises a `ValueError`
//!   **immediately, with no retry** — `meta.error` is `"overflow"`, and
//!   `on_error: fail`'s message is `"use '<name>' -> <label>: overflow"`.
//!
//! Both differ from a genuinely failed expectation (`meta.expect.error`
//! set, `meta.error` becoming `"expect failed: <summary>"`, retried up
//! to `max_attempts` for `use`). [`CelError::is_overflow`] is exactly
//! what lets `electricity-vm`'s own `tool`/`use` dispatch
//! (`exec/tool.rs`, `exec/use_.rs`) reproduce both outcomes exactly,
//! rather than treating this `CelError` as an ordinary evaluation
//! failure the way `if`/`while` CEL (below) already can: it branches on
//! `is_overflow()` before deciding how to report the failure, instead
//! of always wrapping it the same way every other `CelError` gets
//! wrapped.
//!
//! A `Value::Dict` key that converts, in Circuitry's own `_to_cel`
//! (`core/cel_eval.py`), to Python's `None` — a bare `Value::None` key,
//! or a bare `Value::Date` (never a `datetime.datetime`, the only
//! date/time type `_to_cel` special-cases) — collapses the same way in
//! this crate: `convert::to_cel` routes every such key to one reserved
//! sentinel [`cel::objects::Key::String`] (`convert::NULL_KEY_SENTINEL`),
//! so a plain `HashMap::insert` collapses them exactly like Python's own
//! dict assignment does (last value wins), `size()` counts the slot
//! once, and `null in state.m` finds it (`equality`'s own `in` arm
//! special-cases the sentinel back into a `null` match). This is
//! reachable from an ordinary orchestration, not just a key buried deep
//! in `value`/`meta`: a YAML input file's `2026-01-01: x` is a date key,
//! and PyYAML resolves an unquoted null-looking key (`~:`/`null:`) the
//! same way, so `state.input` can hold either directly. A literal
//! `state.m[null]` still raises either way: `cel::Value::Null` has no
//! `Key` conversion at all, matching `celtypes.MapType`'s own key-type
//! check, which rejects `None` too.
//!
//! Known divergence, not reproduced: a key that keeps its own, real CEL
//! value in Python instead of becoming `None` — a `float`
//! (`celtypes.DoubleType`), a `datetime.datetime` (`celtypes.
//! TimestampType`), `bytes`, or a nested `list`/`dict` — is still
//! **dropped** by `convert::to_cel` rather than mapped to some
//! placeholder, because `cel::objects::Key` has no variant for any of
//! them: `Key` is `Int`/`Uint`/`Bool`/`String` only, so there is no way
//! to keep such a key's own distinct identity the way the `null`
//! sentinel above keeps `None`'s. A `Value::Int` too large for `i64` as
//! a dict key raises `convert::ConvertError::Overflow` instead of being
//! dropped or collapsed, matching every other big-int read.
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
//! (`indexing` module docs). celpy's own asymmetry between a `bool`
//! key and an `int`/`uint` lookup is reproduced too: an `int`/`uint`
//! probe finds a `bool`-keyed entry (`BoolType` has no `__eq__`
//! override, so a plain dict lookup resolves through the plain `int`
//! it subclasses), but a `bool` probe against an `int`/`uint`-keyed
//! entry still raises, matching celpy exactly in both directions
//! (`indexing` module docs, pinned by a Rust unit test rather than
//! the differential corpus, which can't express a map literal with
//! both an `int`/`uint` *and* a `bool` key reaching the same slot).
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
mod nesting;
mod ordering;
mod paths;

use std::fmt;
use std::sync::{Arc, OnceLock};

use cel::{Context, Env};
use electricity_value::Value;

pub use nesting::{
    MAX_ACCESSOR_NESTING_DEPTH, MAX_CHAIN_NESTING_DEPTH, MAX_INDEX_NESTING_DEPTH,
    MAX_LIST_MAP_NESTING_DEPTH, MAX_LOGICAL_UNARY_NESTING_DEPTH, MAX_METHOD_CALL_NESTING_DEPTH,
    MAX_PAREN_CALL_NESTING_DEPTH, MAX_SAFETY_CHAIN_DEPTH,
};

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

/// Which of `nesting.rs`'s shape-specific caps (or its safety-only
/// backstop) a [`CelError::nesting_cap`] came from -- carried as
/// structured data rather than folded into the message text, since
/// the evaluation-time parity check's own message is deliberately the
/// same four words regardless of shape, matching Circuitry's own
/// uncaught `RecursionError` text exactly (`nesting.rs`'s module
/// docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NestingCapKind {
    ParenCall,
    ListMap,
    Chain,
    /// An index chain (`a[0][0]...`, [`MAX_INDEX_NESTING_DEPTH`]) --
    /// its own, tighter cap from `Chain` above, even though both
    /// recurse through the same grammar rule in `cel`'s own parser
    /// (`nesting.rs`'s module docs).
    Index,
    LogicalUnary,
    Accessor,
    /// A member-access chain that includes at least one method call
    /// ([`MAX_METHOD_CALL_NESTING_DEPTH`]) -- a different shape from
    /// plain `Accessor` in Circuitry's own terms, checked at
    /// evaluation time rather than compile time (`nesting.rs`'s module
    /// docs).
    MethodCall,
    /// [`nesting::MAX_SAFETY_CHAIN_DEPTH`]'s own backstop -- a Rust-only
    /// safety check with no Circuitry counterpart (`nesting.rs`'s
    /// module docs).
    Safety,
}

/// The shape, depth reached and cap exceeded behind a
/// [`CelError::is_too_deeply_nested`] failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NestingCapDetail {
    pub kind: NestingCapKind,
    pub depth: usize,
    pub max: usize,
}

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
    too_deeply_nested: bool,
    nesting_cap: Option<NestingCapDetail>,
}

impl CelError {
    fn new(expression: &str, message: String) -> Self {
        CelError {
            message,
            expression: expression.to_string(),
            overflow: false,
            too_deeply_nested: false,
            nesting_cap: None,
        }
    }

    /// The CEL expression that failed.
    pub fn expression(&self) -> &str {
        &self.expression
    }

    /// Whether this is a big-int-overflow failure (`convert::ConvertError::Overflow`)
    /// rather than a parse/compile/evaluation failure — see the module
    /// docs' note on [`evaluate_expect`]'s big-int deviation: this is
    /// what `electricity-vm`'s own `tool`/`use` dispatch branches on to
    /// reproduce Circuitry's own `core.tool`/`core.use` handling of
    /// that specific case, rather than treating it like any other
    /// `CelError`.
    pub fn is_overflow(&self) -> bool {
        self.overflow
    }

    /// Whether this is a nesting-too-deep failure: the expression itself
    /// nested past one of the five shape-specific caps `nesting.rs`
    /// tracks (rejected before parsing, that module's own docs), or a
    /// `value`/`meta`/`state` argument nested more than
    /// [`electricity_value::MAX_DEPTH`] levels deep (rejected while
    /// converting it, `convert.rs`'s module docs) — never a
    /// parse/compile/evaluation failure against well-formed, bounded
    /// input.
    pub fn is_too_deeply_nested(&self) -> bool {
        self.too_deeply_nested
    }

    /// Which cap, and at what depth, behind an
    /// [`CelError::is_too_deeply_nested`] failure that came from one of
    /// `nesting.rs`'s own checks -- `None` for a `value`/`meta`/`state`
    /// argument nested too deep while converting it (not an
    /// expression-shape cap) or any non-nesting failure.
    pub fn nesting_cap(&self) -> Option<NestingCapDetail> {
        self.nesting_cap
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

/// Rejects an expression before it is ever handed to `cel`'s own
/// parser, for either of the two reasons that must be caught *before*
/// `compile()` rather than after: [`MAX_ACCESSOR_NESTING_DEPTH`] -- the
/// one shape where Circuitry itself fails during compilation, not
/// evaluation (`nesting.rs`'s module docs) -- and
/// [`nesting::MAX_SAFETY_CHAIN_DEPTH`], the Rust-only backstop against
/// this crate's own `env.compile` call overflowing its stack for real,
/// with no Circuitry counterpart at all. Both are genuine stack-safety
/// checks, unlike [`check_parity_nesting`]'s four, which must run
/// *after* [`evaluate_condition`]'s own unset-path short-circuit to
/// match Circuitry's order: Circuitry's `_compile`
/// never fails for those four shapes, only evaluation does, and
/// evaluation never happens once the unset-path check already decided
/// the condition is `false`.
///
/// The accessor case is worded the bare way Circuitry's own uncaught
/// `RecursionError` is (`nesting.rs`'s module docs), matching
/// [`validate_syntax`]'s own [`CelSyntaxError::TooDeeplyNestedAccessor`]
/// text exactly, for a caller reaching [`evaluate_condition`]/
/// [`evaluate_expect`] directly rather than through a compiled
/// document (`evaluate_cel` hits the same bare exception that way in
/// Circuitry too).
fn check_stack_safety(expr: &str) -> Result<(), CelError> {
    let depths = nesting::max_nesting_depths(expr);
    if depths.accessor > MAX_ACCESSOR_NESTING_DEPTH {
        return Err(CelError {
            too_deeply_nested: true,
            nesting_cap: Some(NestingCapDetail {
                kind: NestingCapKind::Accessor,
                depth: depths.accessor,
                max: MAX_ACCESSOR_NESTING_DEPTH,
            }),
            ..CelError::new(expr, "maximum recursion depth exceeded".to_string())
        });
    }
    if depths.raw_combined > nesting::MAX_SAFETY_CHAIN_DEPTH {
        return Err(CelError {
            too_deeply_nested: true,
            nesting_cap: Some(NestingCapDetail {
                kind: NestingCapKind::Safety,
                depth: depths.raw_combined,
                max: nesting::MAX_SAFETY_CHAIN_DEPTH,
            }),
            ..CelError::new(
                expr,
                format!(
                    "CEL evaluation failed for {}: expression nesting too deep ({} links, max {} for compile-time safety).",
                    py_repr(expr),
                    depths.raw_combined,
                    nesting::MAX_SAFETY_CHAIN_DEPTH
                ),
            )
        });
    }
    Ok(())
}

/// Rejects an expression whose bracket nesting ([`MAX_PAREN_CALL_NESTING_DEPTH`],
/// [`MAX_LIST_MAP_NESTING_DEPTH`]) or chain shape ([`MAX_CHAIN_NESTING_DEPTH`],
/// [`MAX_LOGICAL_UNARY_NESTING_DEPTH`]) runs too deep -- the four
/// parity caps that, in Circuitry, only ever fail during *evaluation*,
/// never compilation (`nesting.rs`'s module docs): the document still
/// compiles and starts running, and this is a runtime failure on the
/// effect that evaluated it, which `on_error:` can catch. Worded with
/// Circuitry's own uncaught-`RecursionError` text
/// (`"maximum recursion depth exceeded"`), wrapped the same way every
/// other genuine evaluation failure is
/// (`"CEL evaluation failed for {expr!r}: ..."`) -- the *same* four
/// words regardless of which of the four shapes failed:
/// which cap and how deep is carried in [`CelError::nesting_cap`]
/// instead of the message, since Circuitry's own text never says
/// either.
///
/// Must run *after* [`evaluate_condition`]'s unset-path short-circuit,
/// not before: Circuitry only ever reaches the evaluation that can
/// raise this once that check has already passed.
fn check_parity_nesting(expr: &str) -> Result<(), CelError> {
    let depths = nesting::max_nesting_depths(expr);
    let detail = if depths.paren_call > MAX_PAREN_CALL_NESTING_DEPTH {
        Some(NestingCapDetail {
            kind: NestingCapKind::ParenCall,
            depth: depths.paren_call,
            max: MAX_PAREN_CALL_NESTING_DEPTH,
        })
    } else if depths.list_map > MAX_LIST_MAP_NESTING_DEPTH {
        Some(NestingCapDetail {
            kind: NestingCapKind::ListMap,
            depth: depths.list_map,
            max: MAX_LIST_MAP_NESTING_DEPTH,
        })
    } else if depths.chain > MAX_CHAIN_NESTING_DEPTH {
        Some(NestingCapDetail {
            kind: NestingCapKind::Chain,
            depth: depths.chain,
            max: MAX_CHAIN_NESTING_DEPTH,
        })
    } else if depths.index > MAX_INDEX_NESTING_DEPTH {
        Some(NestingCapDetail {
            kind: NestingCapKind::Index,
            depth: depths.index,
            max: MAX_INDEX_NESTING_DEPTH,
        })
    } else if depths.logical_unary > MAX_LOGICAL_UNARY_NESTING_DEPTH {
        Some(NestingCapDetail {
            kind: NestingCapKind::LogicalUnary,
            depth: depths.logical_unary,
            max: MAX_LOGICAL_UNARY_NESTING_DEPTH,
        })
    } else if depths.method_call > MAX_METHOD_CALL_NESTING_DEPTH {
        Some(NestingCapDetail {
            kind: NestingCapKind::MethodCall,
            depth: depths.method_call,
            max: MAX_METHOD_CALL_NESTING_DEPTH,
        })
    } else {
        None
    };
    match detail {
        Some(nesting_cap) => Err(CelError {
            too_deeply_nested: true,
            nesting_cap: Some(nesting_cap),
            ..CelError::new(
                expr,
                format!(
                    "CEL evaluation failed for {}: maximum recursion depth exceeded",
                    py_repr(expr)
                ),
            )
        }),
        None => Ok(()),
    }
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

/// A `value`/`meta`/`state` argument nested past
/// [`electricity_value::MAX_DEPTH`] (`convert.rs`'s own module docs) —
/// wrapped the same way a genuine evaluation failure is, same as
/// [`overflow_err`].
fn convert_depth_err(expr: &str) -> CelError {
    CelError {
        too_deeply_nested: true,
        ..CelError::new(
            expr,
            format!(
                "CEL evaluation failed for {}: nesting too deep",
                py_repr(expr)
            ),
        )
    }
}

fn convert_err(expr: &str, err: convert::ConvertError) -> CelError {
    match err {
        convert::ConvertError::Overflow => overflow_err(expr),
        convert::ConvertError::Depth => convert_depth_err(expr),
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
    check_stack_safety(expr)?;
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

    // The four parity caps, checked only now: Circuitry's own
    // evaluation (the thing that can raise this) never runs once the
    // unset-path check above already decided the condition is `false`
    // -- `check_stack_safety` above is a genuine
    // pre-compile concern instead, unaffected by this ordering. This
    // crate's own pre-check stands in for the real evaluator's own
    // recursion failure (the `cel` crate doesn't actually overflow at
    // these depths, which is exactly why a pre-check exists at all) --
    // logged the same way a genuine evaluation failure below is
    // (`core/cel_eval.py::evaluate_cel`'s own `logger.error`), since
    // Circuitry logs this one from inside the same try/except, not
    // from a separate check.
    check_parity_nesting(expr).inspect_err(|_| {
        log::error!(
            "CEL evaluation failed for expr {}: maximum recursion depth exceeded",
            py_repr(expr)
        );
    })?;

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
    let converted_state = convert::to_cel(&projected_state).map_err(|e| {
        // `core/cel_eval.py::evaluate_cel`'s own `compiled.context(ctx)`
        // (which calls `_to_cel`, raising on a big int the expression
        // actually reads) runs *inside* the same `try` block as the
        // evaluation below, under the same `logger.error("... %s",
        // expr, exc)` -- logged from the *raw* cause (`exc`), not the
        // wrapped exception, so this logs `e` itself, not the
        // `CelError` it becomes, to avoid doubling the `"CEL evaluation
        // failed for ..."` prefix.
        log::error!("CEL evaluation failed for expr {}: {e}", py_repr(expr));
        convert_err(expr, e)
    })?;
    ctx.add_variable_from_value("state", converted_state);
    let result = cel::Value::resolve(&tree, &ctx).map_err(|e| {
        // `core/cel_eval.py::evaluate_cel`'s own `logger.error("CEL
        // evaluation failed for expr %r: %s", expr, exc)` -- an `if`/
        // `while` condition's genuine evaluation failure, not the
        // absent-path-is-false case above (that one only ever warns).
        // `evaluate_expect`'s own identical-looking error below never
        // logs this: `core/cel_eval.py::evaluate_cel_expect` doesn't
        // either.
        log::error!("CEL evaluation failed for expr {}: {e}", py_repr(expr));
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
    check_stack_safety(expr)?;
    let env = stdlib_env();
    let program = compile(&env, expr)?;
    // No unset-path short-circuit to order around here (unlike
    // `evaluate_condition`): `expect:` always evaluates, so the four
    // parity caps can run right after `compile()`, matching that
    // Circuitry's own evaluation of `value`/`meta`/`state` always
    // happens too.
    check_parity_nesting(expr)?;

    let mut tree = program.expression().clone();
    paths::rewrite(&mut tree);

    let mut ctx = Context::with_env(Arc::clone(&env));
    ordering::register(&mut ctx);
    equality::register(&mut ctx);
    indexing::register(&mut ctx);
    macros::register(&mut ctx);
    ctx.add_variable_from_value(
        "value",
        convert::to_cel(value).map_err(|e| convert_err(expr, e))?,
    );
    ctx.add_variable_from_value(
        "meta",
        convert::to_cel(meta).map_err(|e| convert_err(expr, e))?,
    );
    ctx.add_variable_from_value(
        "state",
        convert::to_cel(state).map_err(|e| convert_err(expr, e))?,
    );
    let result = cel::Value::resolve(&tree, &ctx).map_err(|e| {
        CelError::new(
            expr,
            format!("CEL evaluation failed for {}: {e}", py_repr(expr)),
        )
    })?;
    Ok(truthy(&result))
}

/// Why a `mode: cel` expression failed electricity-compiler's
/// compile-time syntax check ([`validate_syntax`]) -- ported from
/// `core/cel_eval.py`'s `validate_cel_syntax`, minus the
/// `effect_path`/`label` context a caller (electricity-compiler's own
/// `state_ns` module) wraps around this error itself: that's
/// compiler-specific addressing context, not generic CEL, so it isn't
/// reproduced here -- see [`CelSyntaxError::is_parse_error`] for how a
/// caller reconstructs Python's own `"... . Expression: {expr!r}"` suffix,
/// present only on the parse-failure variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CelSyntaxError {
    Empty,
    TooLong(usize),
    /// A member-access chain ([`nesting::max_nesting_depths`]'s
    /// `accessor`) past [`MAX_ACCESSOR_NESTING_DEPTH`] -- the one shape
    /// where Circuitry itself fails before a document ever starts
    /// running (`nesting.rs`'s module docs), so this crate matches the
    /// phase as well as the boundary: checked only here, at compile
    /// time, never by [`check_parity_nesting`]. Its own `Display` is
    /// deliberately the bare text Circuitry's own uncaught exception
    /// shows (`nesting.rs`'s module docs), not the usual
    /// `"CEL evaluation failed for ..."` wrapper -- see
    /// [`CelSyntaxError::is_parse_error`], which this variant does *not*
    /// match, and `electricity-compiler`'s own `state_ns::validate_cel_syntax`,
    /// which skips its usual effect-path prefix for this one variant too.
    TooDeeplyNestedAccessor(usize),
    /// Not reachable from `core/cel_eval.py` parity at all -- a Rust-only
    /// safety backstop ([`nesting::MAX_SAFETY_CHAIN_DEPTH`]'s own module
    /// docs) so a compile-time syntax check can't itself stack-overflow
    /// on a bracket/chain/logical shape deep enough to threaten this
    /// crate's own 2 MiB stack, even though Circuitry has no comparable
    /// boundary at compile time for those shapes to match.
    TooDeeplyNestedForSafety(usize),
    Parse(String),
}

impl CelSyntaxError {
    /// Whether a caller should append Python's own `". Expression:
    /// {expr!r}"` suffix (`core/cel_eval.py`'s own `validate_cel_syntax`
    /// only appends it for an expression that fails to parse, not for
    /// the empty/too-long cases, which already show the whole problem
    /// without quoting the expression back). [`CelSyntaxError::TooDeeplyNestedAccessor`]
    /// is deliberately excluded too -- Circuitry's own uncaught
    /// exception for that shape has no such suffix either.
    pub fn is_parse_error(&self) -> bool {
        matches!(
            self,
            CelSyntaxError::Parse(_) | CelSyntaxError::TooDeeplyNestedForSafety(_)
        )
    }
}

impl fmt::Display for CelSyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CelSyntaxError::Empty => write!(f, "expression is empty."),
            CelSyntaxError::TooLong(len) => write!(
                f,
                "expression is too long ({len} chars, max {MAX_EXPR_LENGTH})."
            ),
            CelSyntaxError::TooDeeplyNestedAccessor(_depth) => {
                write!(f, "maximum recursion depth exceeded")
            }
            CelSyntaxError::TooDeeplyNestedForSafety(depth) => write!(
                f,
                "expression nesting too deep ({depth} links, max {} for compile-time safety).",
                nesting::MAX_SAFETY_CHAIN_DEPTH
            ),
            CelSyntaxError::Parse(msg) => write!(f, "expression does not parse ({msg})."),
        }
    }
}

impl std::error::Error for CelSyntaxError {}

/// Compile-time syntax check for a single `mode: cel` expression: the
/// 4096-character limit, an empty/whitespace-only expression, and
/// whether it parses -- `core/cel_eval.py`'s `validate_cel_syntax`,
/// minus the `effect_path`/`label`/`effect_name` context a caller wraps
/// around this error itself (see [`CelSyntaxError`]'s own docs for why
/// that context isn't reproduced in this crate).
///
/// # Errors
///
/// [`CelSyntaxError::Empty`]/[`CelSyntaxError::TooLong`]/
/// [`CelSyntaxError::Parse`] mirror Python's own three raises, in the
/// same order (empty is checked before length, matching
/// `core/cel_eval.py`'s own `if not expr ... ; if len(expr) > ...`).
/// [`CelSyntaxError::TooDeeplyNestedAccessor`] has a real, if uncaught
/// and unstably-worded, Python counterpart; [`CelSyntaxError::TooDeeplyNestedForSafety`]
/// has none at all (see their own docs).
pub fn validate_syntax(expr: &str) -> Result<(), CelSyntaxError> {
    if expr.trim().is_empty() {
        return Err(CelSyntaxError::Empty);
    }
    let len = expr.chars().count();
    if len > MAX_EXPR_LENGTH {
        return Err(CelSyntaxError::TooLong(len));
    }
    let depths = nesting::max_nesting_depths(expr);
    // The one shape Circuitry itself rejects before a document ever
    // runs (`nesting.rs`'s module docs) -- checked here, not in
    // `check_parity_nesting`, to match that phase.
    if depths.accessor > MAX_ACCESSOR_NESTING_DEPTH {
        return Err(CelSyntaxError::TooDeeplyNestedAccessor(depths.accessor));
    }
    // A safety-only backstop against this call's own `env.compile`
    // genuinely overflowing this crate's stack -- not a parity check;
    // every shape it could otherwise catch is already far tighter at
    // runtime (`check_parity_nesting`) or, for brackets, by `cel`'s own
    // parser guard (`nesting.rs`'s module docs).
    if depths.raw_combined > nesting::MAX_SAFETY_CHAIN_DEPTH {
        return Err(CelSyntaxError::TooDeeplyNestedForSafety(
            depths.raw_combined,
        ));
    }
    let env = stdlib_env();
    env.compile(expr)
        .map_err(|e| CelSyntaxError::Parse(e.to_string()))?;
    Ok(())
}

/// Every `state.` path *expr* reads, from the parse tree -- including
/// one that appears only as a `has(...)` argument (a guard is still a
/// reference) -- ported from `core/cel_eval.py`'s `state_paths`.
///
/// Deliberately has no length pre-check, matching Python's own
/// `state_paths`, which calls straight into `_compile` with none either
/// (only `evaluate_cel`/`evaluate_cel_expect`/`validate_cel_syntax`
/// check length) -- callers that need the length cap enforced too
/// (none do today: `core/state_ns.py`'s `validate_cel_expr` calls
/// `state_paths` to police the `state.<namespace>` grammar, and
/// separately calls `validate_cel_syntax`, which does check it) get it
/// from [`validate_syntax`] instead.
///
/// # Errors
///
/// An expression that doesn't parse returns `Err`, the same way
/// `core/cel_eval.py`'s own `state_paths` raises `CelValidationError` --
/// a caller checking only `state.<namespace>` usage, not syntax, is
/// expected to swallow this (`core/state_ns.py`'s own `validate_cel_expr`
/// does exactly that: `except CelError: return`); syntax is a separate
/// check, via [`validate_syntax`].
pub fn state_paths(expr: &str) -> Result<Vec<String>, CelError> {
    // Only the stack-safety checks, never `check_parity_nesting`:
    // Python's own `state_paths` calls straight into `_compile`, which
    // never fails for the four parity shapes (`nesting.rs`'s module
    // docs) -- this function only ever parses, it doesn't evaluate.
    check_stack_safety(expr)?;
    let env = stdlib_env();
    let program = compile(&env, expr)?;
    Ok(paths::collect_state_paths(program.expression())
        .paths
        .into_iter()
        .map(|p| p.path)
        .collect())
}

/// Every `state.`-rooted path *expr* reads (raw, `state.`-prefixed,
/// [`state_paths`]'s own shape, guarded or not -- [`evaluate_condition`]'s
/// own `project` call narrows by this same, unfiltered list, not just
/// the unguarded subset [`paths::first_unresolved`] checks), plus
/// whether *expr* also reads `state` as a whole value -- everything a
/// caller needs to narrow a `state` it hasn't built yet down to only
/// what this expression could ever use, the same decision
/// [`evaluate_condition`]'s own `project`/`reads_whole_state` branch
/// makes internally, but available *before* a caller ever materializes
/// a `state` to narrow (issue #463: a caller that already has to build
/// `state` from a live, mutable store rather than from an existing
/// `Value` can use this to build only the narrowed form directly,
/// never the whole thing). [`state_paths`] returns less (both the
/// guarded flag and `reads_whole_state` are dropped) and exists for a
/// different caller (`core/state_ns.py`'s own grammar check).
///
/// # Errors
///
/// Same as [`state_paths`].
pub fn condition_paths(expr: &str) -> Result<ConditionPaths, CelError> {
    // Same reasoning as `state_paths`: only parses, never evaluates.
    check_stack_safety(expr)?;
    let env = stdlib_env();
    let program = compile(&env, expr)?;
    let collected = paths::collect_state_paths(program.expression());
    Ok(ConditionPaths {
        paths: collected.paths.into_iter().map(|p| p.path).collect(),
        reads_whole_state: collected.reads_whole_state,
    })
}

/// [`condition_paths`]'s own result.
#[derive(Debug, Clone, Default)]
pub struct ConditionPaths {
    /// Every `state.`-rooted path *expr* reads, raw and unfiltered (see
    /// [`condition_paths`]'s own doc comment for why this keeps a
    /// `has()`-guarded path too).
    pub paths: Vec<String>,
    /// Whether *expr* also reads `state` as a whole value
    /// (`size(state)`, say) -- when `true`, no projection could narrow
    /// what the expression might read, so a caller must build the
    /// whole `state` instead of projecting by `paths`.
    pub reads_whole_state: bool,
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
    fn an_unset_path_past_a_parity_cap_is_still_false_not_a_nesting_error() {
        // The four parity caps must run *after* the
        // unset-path short-circuit, matching Circuitry's own order
        // (compile, then unset path, then evaluate) -- `_compile`
        // never fails for these shapes, only evaluation does, and
        // Circuitry never reaches evaluation once the unset path has
        // already decided the condition is `false`. `state.input.x` is
        // never set here, and the chain is one past
        // `MAX_CHAIN_NESTING_DEPTH`.
        let state = Value::Dict(Dict::new());
        let expr = format!("state.input.x{}", "+1".repeat(MAX_CHAIN_NESTING_DEPTH + 1));
        assert_eq!(evaluate_condition(&expr, &state, false), Ok(false));
    }

    #[test]
    fn an_unset_path_past_a_parity_cap_raises_circuitrys_own_strict_text_not_a_nesting_error() {
        let state = Value::Dict(Dict::new());
        let expr = format!("state.input.x{}", "+1".repeat(MAX_CHAIN_NESTING_DEPTH + 1));
        let err = evaluate_condition(&expr, &state, true).unwrap_err();
        assert!(!err.is_too_deeply_nested());
        assert!(err.to_string().contains("strict"));
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
    fn none_and_date_dict_keys_collapse_and_stay_reachable_via_in() {
        // Matches `_to_cel`'s own collapse (confirmed against the real
        // evaluator): a `None` key and a bare `date` key both convert
        // to Python's `None`, so a plain dict assignment overwrites
        // down to one entry -- `size()` counts it once, `null in m` is
        // `true`, and a literal `m[null]` still raises (`cel::Value::
        // Null` has no `Key` conversion, matching `celtypes.MapType`'s
        // own key-type check).
        use chrono::NaiveDate;
        let mut d = Dict::new();
        d.insert(Value::None, Value::from(1_i64));
        d.insert(
            Value::Date(NaiveDate::from_ymd_opt(2020, 1, 1).unwrap()),
            Value::from(2_i64),
        );
        let state = dict(vec![("m", Value::Dict(d))]);
        assert_eq!(
            evaluate_condition("size(state.m) == 1", &state, false),
            Ok(true)
        );
        assert_eq!(
            evaluate_condition("null in state.m", &state, false),
            Ok(true)
        );
        assert!(evaluate_condition("state.m[null] == 2", &state, false).is_err());
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
    fn validate_syntax_accepts_a_well_formed_expression() {
        assert_eq!(validate_syntax("state.input.ok == true"), Ok(()));
    }

    #[test]
    fn validate_syntax_rejects_empty() {
        assert_eq!(validate_syntax(""), Err(CelSyntaxError::Empty));
        assert_eq!(validate_syntax("   "), Err(CelSyntaxError::Empty));
        assert_eq!(
            validate_syntax("").unwrap_err().to_string(),
            "expression is empty."
        );
    }

    #[test]
    fn validate_syntax_rejects_too_long() {
        let expr = "state.x == 'a'".to_string() + &" && state.x == 'a'".repeat(500);
        let err = validate_syntax(&expr).unwrap_err();
        assert!(matches!(err, CelSyntaxError::TooLong(_)));
        assert!(err.to_string().contains("too long"));
        assert!(!err.is_parse_error());
    }

    #[test]
    fn validate_syntax_rejects_a_malformed_expression() {
        let err = validate_syntax("state.a ==").unwrap_err();
        assert!(matches!(err, CelSyntaxError::Parse(_)));
        assert!(err.to_string().starts_with("expression does not parse ("));
        assert!(err.is_parse_error());
    }

    #[test]
    fn validate_syntax_rejects_a_too_deep_member_chain_with_circuitrys_own_bare_text() {
        let expr = format!("state{}", ".a".repeat(MAX_ACCESSOR_NESTING_DEPTH + 1));
        let err = validate_syntax(&expr).unwrap_err();
        assert!(matches!(err, CelSyntaxError::TooDeeplyNestedAccessor(_)));
        assert_eq!(err.to_string(), "maximum recursion depth exceeded");
        assert!(!err.is_parse_error());
    }

    #[test]
    fn validate_syntax_accepts_a_member_chain_at_the_accessor_limit() {
        let expr = format!("state{}", ".a".repeat(MAX_ACCESSOR_NESTING_DEPTH));
        assert_eq!(validate_syntax(&expr), Ok(()));
    }

    #[test]
    fn validate_syntax_accepts_an_operator_chain_well_past_evaluations_own_cap() {
        // Circuitry's own compile-time budget for this shape is far
        // looser than its evaluation-time one (`nesting.rs`'s module
        // docs): a document using an arithmetic chain this deep still
        // compiles in Circuitry, only failing once it runs, so
        // `validate_syntax` must accept it too.
        let expr = format!("0{}", "+1".repeat(MAX_CHAIN_NESTING_DEPTH * 2));
        assert_eq!(validate_syntax(&expr), Ok(()));
    }

    #[test]
    fn validate_syntax_rejects_a_chain_past_the_compile_time_safety_backstop() {
        let expr = format!("0{}", "+1".repeat(nesting::MAX_SAFETY_CHAIN_DEPTH + 1));
        let err = validate_syntax(&expr).unwrap_err();
        assert!(matches!(err, CelSyntaxError::TooDeeplyNestedForSafety(_)));
        assert!(err.is_parse_error());
    }

    #[test]
    fn validate_syntax_accepts_a_long_flat_or_chain_the_safety_backstop_must_not_count_as_deep() {
        // Regression guard: the compile-time safety backstop used to
        // sum every link scanned (`raw_combined` as a running total)
        // rather than tracking live nesting depth, so a long but flat
        // expression could trip it even though nothing was ever deeply
        // nested. `||` never recurses in `cel`'s own grammar at all
        // (nesting.rs's module docs), so no number of them should.
        let expr = (0..500).map(|_| "true").collect::<Vec<_>>().join(" || ");
        assert!(expr.chars().count() > 3900 && expr.chars().count() <= MAX_EXPR_LENGTH);
        assert_eq!(validate_syntax(&expr), Ok(()));
    }

    #[test]
    fn validate_syntax_accepts_an_in_list_of_a_thousand_elements() {
        let elements = (0..1000).map(|_| "-1").collect::<Vec<_>>().join(",");
        let expr = format!("state.input.x in [{elements}]");
        assert!(expr.chars().count() <= MAX_EXPR_LENGTH);
        assert_eq!(validate_syntax(&expr), Ok(()));
    }

    #[test]
    fn validate_syntax_accepts_a_thousand_sibling_single_element_lists() {
        let elements = (0..1000).map(|_| "[0]").collect::<Vec<_>>().join(",");
        let expr = format!("[{elements}]");
        assert!(expr.chars().count() <= MAX_EXPR_LENGTH);
        assert_eq!(validate_syntax(&expr), Ok(()));
    }

    #[test]
    fn state_paths_collects_has_guarded_and_plain_reads() {
        let paths = state_paths("has(state.input.n) && state.prime.x.value == 1").unwrap();
        assert_eq!(paths, vec!["state.input.n", "state.prime.x.value"]);
    }

    #[test]
    fn state_paths_errors_on_a_malformed_expression() {
        assert!(state_paths("state.a ==").is_err());
    }

    #[test]
    fn condition_paths_collects_has_guarded_and_plain_reads_too() {
        let collected = condition_paths("has(state.input.n) && state.prime.x.value == 1").unwrap();
        assert_eq!(
            collected.paths,
            vec!["state.input.n", "state.prime.x.value"]
        );
        assert!(!collected.reads_whole_state);
    }

    #[test]
    fn condition_paths_reports_a_whole_state_read() {
        let collected = condition_paths("size(state) == 2").unwrap();
        assert!(collected.paths.is_empty());
        assert!(collected.reads_whole_state);
    }

    #[test]
    fn condition_paths_errors_on_a_malformed_expression() {
        assert!(condition_paths("state.a ==").is_err());
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
