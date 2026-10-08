//! The environment this crate evaluates against, and the one macro in
//! it that isn't `cel`'s own: a `has()` replaced wholesale to accept any
//! expression, not just a field selection, and to turn *any* failure to
//! evaluate it into `false` rather than only the narrower set `cel`'s
//! own safe-navigation primitives (`.?`/`[?]`) can be built from
//! (DESIGN.md §7.2, issue #379 review findings 2 and 3 — the fourth
//! review).
//!
//! `cel`'s own `has_macro_expander` (`parser/macros.rs`) only accepts a
//! field selection as `has()`'s argument — `has(state.items[0])` fails
//! to *parse* — and even a prior version of this crate's own rewrite,
//! built from `.?`/`[?]`, still left two cases to `cel`'s/celpy's own
//! non-graceful evaluation: a failing *index expression*, and a chain
//! rooted at an identifier not bound in the current mode. celpy's own
//! rule has neither restriction: "the argument evaluated without
//! error" (`evaluation.py`'s `ident_arg` `has`), for *any* expression.
//!
//! [`env`] builds an environment whose `has` macro expands `has(X)`
//! into `!@not_strictly_false(__electricity_false(X))`, reproducing
//! that rule exactly, for any `X`:
//! - `@not_strictly_false` is one of `cel`'s own built-in operators,
//!   special-cased inside its evaluator (`objects.rs`) rather than a
//!   registered function: it evaluates its one argument and never
//!   raises itself — `true` unless that argument evaluates, without
//!   error, to the literal `false`.
//! - A *registered* function's own arguments, by contrast, are
//!   evaluated eagerly, one by one, strictly before the function body
//!   ever runs (`objects.rs`) — so [`electricity_false_fn`], which
//!   ignores its argument's value and always returns `false`, is only
//!   ever reached once `X` has evaluated without raising.
//!
//! Put together: if `X` evaluates without error, `__electricity_false`
//! returns `false`, `@not_strictly_false` passes that through unchanged
//! (`false` is exactly what it doesn't paper over), and the `!` makes
//! the whole thing `true`. If `X` raises, evaluating
//! `__electricity_false(X)`'s own argument fails before the function is
//! called, `@not_strictly_false` turns that `Err` into `true` (that's
//! what "not *strictly* false" means — anything but a bare `false`
//! counts, an error included), and the `!` makes the whole thing
//! `false`. Exactly celpy's rule, for a failing root, a failing index
//! expression, an unbound root, or an argument that was never a field
//! selection to begin with — none of them special-cased, because none
//! of them needs to be.
//!
//! This is why the rewrite it replaces
//! ([`crate::paths::rewrite`]'s safe-navigation chain, `.?`/`[?]`) is
//! gone along with it: nothing here needs to walk `X` step by step
//! deciding which steps to make optional, because a plain, ordinary
//! evaluation of `X` already raises in exactly the cases `has()` must
//! turn into `false`.
//!
//! One wrinkle: `cel`'s own `Env::stdlib()` bundles `has` together with
//! the five other standard macros (`all`, `exists`, `exists_one`/
//! `existsOne`, `map`, `filter`) as one shared, immutable set, and
//! [`Env::add_macro`] rejects a second macro for the same call shape
//! outright — so replacing `has` alone means not calling
//! `Env::stdlib()` at all. [`env`] instead builds on
//! `Env::default().with_stdlib()` (registers every standard *type* and
//! *function*, no macros: see that method's own docs) and adds all six
//! macros back one at a time — this module's own [`has_macro_expander`]
//! for `has`, and, for the other five, unchanged copies of `cel`'s own
//! expanders (`parser/macros.rs`, MIT-licensed; not exported for reuse,
//! since they're `pub(crate)` there) rather than a reimplementation:
//! [`all_macro_expander`], [`exists_macro_expander`],
//! [`exists_one_macro_expander`], [`map_macro_expander`] and
//! [`filter_macro_expander`] below build the exact same comprehension
//! `cel`'s own would, so `all`/`exists`/`exists_one`/`map`/`filter`
//! keep behaving exactly as `cel`'s own `Env::stdlib()` already made
//! them.

use cel::common::ast::{CallExpr, ComprehensionExpr, Expr, IdedExpr, ListExpr, operators};
use cel::extractors::Arguments;
use cel::parser::{Macro, MacroExprHelper, ParseError};
use cel::{Context, Env, ExecutionError, Value};

/// The function `has(X)` expands into a call to, wrapping `X` so a
/// registered function's eager argument evaluation is what decides
/// whether `X` itself raised (see the module docs).
const ELECTRICITY_FALSE: &str = "__electricity_false";

/// Whether *func_name* is the marker [`ELECTRICITY_FALSE`] expands
/// `has()` into — what
/// [`crate::paths::collect_state_paths`](crate::paths::collect_state_paths)
/// looks for to treat a call's one argument as `has()`-guarded, now that
/// expansion no longer leaves a field selection's `test` flag set for it
/// to look for instead.
pub fn is_has_guard(func_name: &str) -> bool {
    func_name == ELECTRICITY_FALSE
}

/// The environment this crate compiles and evaluates every expression
/// against: `cel`'s own standard types and functions, plus all six
/// standard macros — five of them `cel`'s own, `has` replaced (module
/// docs).
pub fn env() -> Env {
    let mut env = Env::default().with_stdlib();
    env.add_macro(Macro::receiver(operators::ALL, 2, all_macro_expander))
        .expect("name not already declared");
    env.add_macro(Macro::receiver(operators::EXISTS, 2, exists_macro_expander))
        .expect("name not already declared");
    env.add_macro(Macro::receiver(
        operators::EXISTS_ONE,
        2,
        exists_one_macro_expander,
    ))
    .expect("name not already declared");
    env.add_macro(Macro::receiver("existsOne", 2, exists_one_macro_expander))
        .expect("name not already declared");
    // Only the two-argument form of `map` is registered here: `cel`'s
    // own `map_macro_expander` below (unchanged) accepts a third
    // argument too (`target.map(x, predicate, transform)`), but
    // celpy's own `map` macro raises on three arguments, and Circuitry
    // evaluates through celpy. Not registering
    // `Macro::receiver(operators::MAP, 3, ...)` leaves no macro or
    // function named `map` with three arguments, so `map_macro_expander`
    // is never reached for this shape — `cel`'s own `env.compile` still
    // accepts the call (it leaves an unexpanded, undeclared `map` call
    // in the tree rather than rejecting the arity there), but resolving
    // it then fails, since the loop variable it names (`x`) was never
    // bound by any comprehension. An error either way, raised when the
    // expression is evaluated rather than when it is parsed (issue
    // #379).
    env.add_macro(Macro::receiver(operators::MAP, 2, map_macro_expander))
        .expect("name not already declared");
    env.add_macro(Macro::receiver(operators::FILTER, 2, filter_macro_expander))
        .expect("name not already declared");
    env.add_macro(Macro::global(operators::HAS, 1, has_macro_expander))
        .expect("name not already declared");
    env
}

/// Registers [`ELECTRICITY_FALSE`] on *ctx*, the same way
/// [`crate::equality::register`], [`crate::ordering::register`] and
/// [`crate::indexing::register`] register their own functions.
pub fn register(ctx: &mut Context) {
    ctx.add_function(ELECTRICITY_FALSE, electricity_false_fn)
        .expect("name not already declared");
}

/// Always `false`, once called — which, since a registered function's
/// arguments are evaluated eagerly before it runs, only happens once
/// its one argument has evaluated without raising (module docs).
fn electricity_false_fn(Arguments(args): Arguments) -> Result<Value, ExecutionError> {
    debug_assert_eq!(args.len(), 1);
    Ok(Value::Bool(false))
}

/// `has(X)` for any `X`, expanded at parse time into
/// `!@not_strictly_false(__electricity_false(X))` (module docs) — never
/// declines, so this macro's `Ok` is always `Some`.
fn has_macro_expander(
    helper: &mut MacroExprHelper,
    target: &mut Option<IdedExpr>,
    args: &mut Vec<IdedExpr>,
) -> Result<Option<IdedExpr>, ParseError> {
    debug_assert!(target.is_none(), "has() is a global, not a receiver, call");
    debug_assert_eq!(args.len(), 1, "has() takes exactly one argument");
    let x = args.remove(0);
    let wrapped = helper.next_expr(Expr::Call(CallExpr {
        func_name: ELECTRICITY_FALSE.to_string(),
        target: None,
        args: vec![x],
    }));
    let not_strictly_false = helper.next_expr(Expr::Call(CallExpr {
        func_name: operators::NOT_STRICTLY_FALSE.to_string(),
        target: None,
        args: vec![wrapped],
    }));
    Ok(Some(helper.next_expr(Expr::Call(CallExpr {
        func_name: operators::LOGICAL_NOT.to_string(),
        target: None,
        args: vec![not_strictly_false],
    }))))
}

// Copied from the `cel` crate (cel-rust) version 0.15.0, the version
// pinned in electricity/Cargo.lock, `parser/macros.rs`:
//
// Copyright (c) 2022 Tom Forbes and Contributors
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.
//
// --- Everything below is `cel`'s own `parser::macros` expanders
// (MIT-licensed), copied unchanged but for their `Result` type (that
// module's own `BuiltIn` expanders never decline, wrapped in `Some` by
// a private helper this crate has no access to) — not reimplemented,
// so `all`/`exists`/`exists_one`/`map`/`filter` keep matching `cel`'s
// own exactly. ---

fn exists_macro_expander(
    helper: &mut MacroExprHelper,
    target: &mut Option<IdedExpr>,
    args: &mut Vec<IdedExpr>,
) -> Result<Option<IdedExpr>, ParseError> {
    if target.is_none() {
        unreachable!("Expected a target, but got `None`!")
    }
    if args.len() != 2 {
        unreachable!("Expected two args!")
    }

    let mut arguments = vec![args.remove(1)];
    let v = extract_ident(args.remove(0), helper)?;

    let init = helper.next_expr(Expr::Literal(cel::common::ast::LiteralValue::Boolean(
        false.into(),
    )));
    let result_binding = "@result".to_string();
    let accu_ident = helper.next_expr(Expr::Ident(result_binding.clone()));
    let arg = helper.next_expr(Expr::Call(CallExpr {
        func_name: operators::LOGICAL_NOT.to_string(),
        target: None,
        args: vec![accu_ident],
    }));
    let condition = helper.next_expr(Expr::Call(CallExpr {
        func_name: operators::NOT_STRICTLY_FALSE.to_string(),
        target: None,
        args: vec![arg],
    }));

    arguments.insert(0, helper.next_expr(Expr::Ident(result_binding.clone())));
    let step = helper.next_expr(Expr::Call(CallExpr {
        func_name: operators::LOGICAL_OR.to_string(),
        target: None,
        args: arguments,
    }));

    let result = helper.next_expr(Expr::Ident(result_binding.clone()));

    Ok(Some(helper.next_expr(Expr::Comprehension(Box::new(
        ComprehensionExpr {
            iter_range: target.take().unwrap(),
            iter_var: v,
            iter_var2: None,
            accu_var: result_binding,
            accu_init: init,
            loop_cond: condition,
            loop_step: step,
            result,
        },
    )))))
}

fn all_macro_expander(
    helper: &mut MacroExprHelper,
    target: &mut Option<IdedExpr>,
    args: &mut Vec<IdedExpr>,
) -> Result<Option<IdedExpr>, ParseError> {
    if target.is_none() {
        unreachable!("Expected a target, but got `None`!")
    }
    if args.len() != 2 {
        unreachable!("Expected two args!")
    }

    let mut arguments = vec![args.remove(1)];
    let v = extract_ident(args.remove(0), helper)?;

    let init = helper.next_expr(Expr::Literal(cel::common::ast::LiteralValue::Boolean(
        true.into(),
    )));
    let result_binding = "@result".to_string();
    let accu_ident = helper.next_expr(Expr::Ident(result_binding.clone()));
    let condition = helper.next_expr(Expr::Call(CallExpr {
        func_name: operators::NOT_STRICTLY_FALSE.to_string(),
        target: None,
        args: vec![accu_ident],
    }));

    arguments.insert(0, helper.next_expr(Expr::Ident(result_binding.clone())));
    let step = helper.next_expr(Expr::Call(CallExpr {
        func_name: operators::LOGICAL_AND.to_string(),
        target: None,
        args: arguments,
    }));

    let result = helper.next_expr(Expr::Ident(result_binding.clone()));

    Ok(Some(helper.next_expr(Expr::Comprehension(Box::new(
        ComprehensionExpr {
            iter_range: target.take().unwrap(),
            iter_var: v,
            iter_var2: None,
            accu_var: result_binding,
            accu_init: init,
            loop_cond: condition,
            loop_step: step,
            result,
        },
    )))))
}

fn exists_one_macro_expander(
    helper: &mut MacroExprHelper,
    target: &mut Option<IdedExpr>,
    args: &mut Vec<IdedExpr>,
) -> Result<Option<IdedExpr>, ParseError> {
    if target.is_none() {
        unreachable!("Expected a target, but got `None`!")
    }
    if args.len() != 2 {
        unreachable!("Expected two args!")
    }

    let mut arguments = vec![args.remove(1)];
    let v = extract_ident(args.remove(0), helper)?;

    let init = helper.next_expr(Expr::Literal(cel::common::ast::LiteralValue::Int(0.into())));
    let result_binding = "@result".to_string();
    let condition = helper.next_expr(Expr::Literal(cel::common::ast::LiteralValue::Boolean(
        true.into(),
    )));

    let args = vec![
        helper.next_expr(Expr::Ident(result_binding.clone())),
        helper.next_expr(Expr::Literal(cel::common::ast::LiteralValue::Int(1.into()))),
    ];
    arguments.push(helper.next_expr(Expr::Call(CallExpr {
        func_name: operators::ADD.to_string(),
        target: None,
        args,
    })));
    arguments.push(helper.next_expr(Expr::Ident(result_binding.clone())));

    let step = helper.next_expr(Expr::Call(CallExpr {
        func_name: operators::CONDITIONAL.to_string(),
        target: None,
        args: arguments,
    }));

    let accu = helper.next_expr(Expr::Ident(result_binding.clone()));
    let one = helper.next_expr(Expr::Literal(cel::common::ast::LiteralValue::Int(1.into())));
    let result = helper.next_expr(Expr::Call(CallExpr {
        func_name: operators::EQUALS.to_string(),
        target: None,
        args: vec![accu, one],
    }));

    Ok(Some(helper.next_expr(Expr::Comprehension(Box::new(
        ComprehensionExpr {
            iter_range: target.take().unwrap(),
            iter_var: v,
            iter_var2: None,
            accu_var: result_binding,
            accu_init: init,
            loop_cond: condition,
            loop_step: step,
            result,
        },
    )))))
}

/// Expands `target.map(v, f)`, or `target.map(v, p, f)`.
fn map_macro_expander(
    helper: &mut MacroExprHelper,
    target: &mut Option<IdedExpr>,
    args: &mut Vec<IdedExpr>,
) -> Result<Option<IdedExpr>, ParseError> {
    if target.is_none() {
        unreachable!("Expected a target, but got `None`!")
    }
    if args.len() != 2 && args.len() != 3 {
        unreachable!("Expected two or three args!")
    }

    let func = args.pop().unwrap();
    let v = extract_ident(args.remove(0), helper)?;

    let init = helper.next_expr(Expr::List(ListExpr::new(Vec::default())));
    let result_binding = "@result".to_string();
    let condition = helper.next_expr(Expr::Literal(cel::common::ast::LiteralValue::Boolean(
        true.into(),
    )));

    let filter = args.pop();

    let args = vec![
        helper.next_expr(Expr::Ident(result_binding.clone())),
        helper.next_expr(Expr::List(ListExpr::new(vec![func]))),
    ];
    let step = helper.next_expr(Expr::Call(CallExpr {
        func_name: operators::ADD.to_string(),
        target: None,
        args,
    }));

    let step = match filter {
        Some(filter) => {
            let accu = helper.next_expr(Expr::Ident(result_binding.clone()));
            helper.next_expr(Expr::Call(CallExpr {
                func_name: operators::CONDITIONAL.to_string(),
                target: None,
                args: vec![filter, step, accu],
            }))
        }
        None => step,
    };

    let result = helper.next_expr(Expr::Ident(result_binding.clone()));

    Ok(Some(helper.next_expr(Expr::Comprehension(Box::new(
        ComprehensionExpr {
            iter_range: target.take().unwrap(),
            iter_var: v,
            iter_var2: None,
            accu_var: result_binding,
            accu_init: init,
            loop_cond: condition,
            loop_step: step,
            result,
        },
    )))))
}

fn filter_macro_expander(
    helper: &mut MacroExprHelper,
    target: &mut Option<IdedExpr>,
    args: &mut Vec<IdedExpr>,
) -> Result<Option<IdedExpr>, ParseError> {
    if target.is_none() {
        unreachable!("Expected a target, but got `None`!")
    }
    if args.len() != 2 {
        unreachable!("Expected two args!")
    }

    let var = args.remove(0);
    let v = extract_ident(var.clone(), helper)?;
    let filter = args.pop().unwrap();

    let init = helper.next_expr(Expr::List(ListExpr::new(Vec::default())));
    let result_binding = "@result".to_string();
    let condition = helper.next_expr(Expr::Literal(cel::common::ast::LiteralValue::Boolean(
        true.into(),
    )));

    let args = vec![
        helper.next_expr(Expr::Ident(result_binding.clone())),
        helper.next_expr(Expr::List(ListExpr::new(vec![var]))),
    ];
    let step = helper.next_expr(Expr::Call(CallExpr {
        func_name: operators::ADD.to_string(),
        target: None,
        args,
    }));

    let accu = helper.next_expr(Expr::Ident(result_binding.clone()));
    let step = helper.next_expr(Expr::Call(CallExpr {
        func_name: operators::CONDITIONAL.to_string(),
        target: None,
        args: vec![filter, step, accu],
    }));

    let result = helper.next_expr(Expr::Ident(result_binding.clone()));

    Ok(Some(helper.next_expr(Expr::Comprehension(Box::new(
        ComprehensionExpr {
            iter_range: target.take().unwrap(),
            iter_var: v,
            iter_var2: None,
            accu_var: result_binding,
            accu_init: init,
            loop_cond: condition,
            loop_step: step,
            result,
        },
    )))))
}

fn extract_ident(expr: IdedExpr, helper: &mut MacroExprHelper) -> Result<String, ParseError> {
    match expr.expr {
        Expr::Ident(ident) => Ok(ident),
        _ => Err(helper.new_error(expr.id, "argument must be a simple name")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{equality, indexing, ordering, paths};
    use cel::Context;
    use std::sync::Arc;

    fn eval(expr: &str) -> Result<Value, String> {
        let compiled = env().compile(expr).unwrap();
        let mut tree = compiled.expression().clone();
        paths::rewrite(&mut tree);
        let mut ctx = Context::with_env(Arc::new(env()));
        ordering::register(&mut ctx);
        equality::register(&mut ctx);
        indexing::register(&mut ctx);
        register(&mut ctx);
        Value::resolve(&tree, &ctx).map_err(|e| e.to_string())
    }

    #[test]
    fn standard_macros_still_work() {
        assert_eq!(eval("[1, 2, 3].all(x, x > 0)"), Ok(Value::Bool(true)));
        assert_eq!(eval("[1, 2, 3].exists(x, x > 2)"), Ok(Value::Bool(true)));
        assert_eq!(
            eval("[1, 2, 3].exists_one(x, x > 2)"),
            Ok(Value::Bool(true))
        );
        assert_eq!(eval("[1, 2, 3].existsOne(x, x > 2)"), Ok(Value::Bool(true)));
        assert_eq!(
            eval("[1, 2, 3].map(x, x * 2)"),
            Ok(Value::List(Arc::new(vec![
                Value::Int(2),
                Value::Int(4),
                Value::Int(6)
            ])))
        );
        assert_eq!(
            eval("[1, 2, 3].filter(x, x > 1)"),
            Ok(Value::List(Arc::new(vec![Value::Int(2), Value::Int(3)])))
        );
    }

    #[test]
    fn has_over_a_non_select_argument_parses_and_evaluates() {
        // issue #379 review finding 3: `cel`'s own `has` macro only
        // accepts a field selection; celpy evaluates `has()` over any
        // expression.
        assert_eq!(eval("has([1][0])"), Ok(Value::Bool(true)));
        assert_eq!(eval("has([][0])"), Ok(Value::Bool(false)));
    }

    #[test]
    fn has_over_a_failing_root_is_false_not_an_error() {
        // issue #379 review finding 2: a root that itself fails to
        // evaluate (an undeclared identifier) must resolve to `false`,
        // not propagate the failure.
        assert_eq!(eval("has(nope.x)"), Ok(Value::Bool(false)));
    }

    #[test]
    fn three_argument_map_is_rejected() {
        // issue #379: cel-python's own `map` macro raises on three
        // arguments, so this crate only registers the two-argument
        // form. The call still *parses* (it is left as an unexpanded,
        // undeclared `map` call), but resolving it fails: the loop
        // variable it names was never bound by any comprehension.
        assert!(eval("[1, 2].map(x, x > 1, x * 10)").is_err());
    }
}
