//! The structural absent-path walk behind `evaluate_condition`
//! (runtime-semantics.md §4.4, ports `_collect_state_paths`/
//! `_first_unresolved`, `core/cel_eval.py`), and the AST rewrite behind
//! strict-type operators (DESIGN.md §7.2). `has()` itself is
//! [`crate::macros`]'s own rewrite, expanded at parse time rather than
//! here — this module's own [`walk`] recognizes its expansion
//! ([`crate::macros::is_has_guard`]) to decide what it guards, the same
//! way it used to recognize a field selection's `test` flag.
//!
//! Reading an unset `state.` path is decided *before* evaluation, by
//! walking the parsed expression for every dotted `state.` read, not by
//! catching a failure during evaluation — `has(...)` guards one path at a
//! time, and a genuinely malformed expression must still raise rather
//! than silently resolve to "absent".

use cel::common::ast::{CallExpr, EntryExpr, Expr, IdedExpr, LiteralValue, operators};
use cel::common::types::CelString;
use electricity_value::{Dict, Value};

use crate::{equality, indexing, macros, ordering};

/// The function a marked, unset `state.` occurrence is rewritten into by
/// [`mark_unset`] -- a reserved name ([`is_reserved_name`]) a user
/// expression can never call directly, carrying the path as its one
/// string-literal argument. [`crate::evaluate_condition`] recognizes the
/// `ExecutionError::FunctionError` evaluating it raises and applies the
/// absent-state rule with that path, only if the whole expression's own
/// result turns out to be it -- never checked structurally, the way the
/// pre-evaluation short-circuit this replaces used to be.
pub const UNSET_MARKER_FN: &str = "__electricity_unset";

/// Whether *func_name* names one of this crate's own internal
/// registered functions -- [`equality`], [`ordering`] and [`indexing`]'s
/// own `__electricity_strict_*`/`__electricity_python_index`
/// substitutions for `cel`'s native operators, and [`macros`]'s own
/// `__electricity_false` marker for `has()` -- never a name a user
/// expression could legitimately call directly (issue #400).
fn is_reserved_name(func_name: &str) -> bool {
    func_name.starts_with("__electricity_")
}

/// The first reserved-name call ([`is_reserved_name`]) a user wrote
/// directly in *expr*'s own parse tree, or `None` if it contains none.
///
/// `cel`'s own macro expansion already ran by the time this crate ever
/// sees a compiled [`IdedExpr`] -- `has(X)` has already become a call to
/// [`macros`]'s own marker function, indistinguishable in shape from a
/// user writing that same call out by hand -- so this recognizes
/// exactly the one shape [`macros::has_macro_expander`] produces
/// (`!@not_strictly_false(__electricity_false(X))`) and only descends
/// into *X*, the user-authored part, rather than flagging the marker
/// call itself. Every other occurrence of a reserved name anywhere in
/// the tree -- including `__electricity_false` called outside that
/// exact shape, and every `__electricity_strict_*`/
/// `__electricity_python_index` call, which [`rewrite`] (below) never
/// produces until *after* this runs -- is a direct user reference,
/// caught here before [`crate::compile`] ever hands the tree to
/// [`rewrite`] or to evaluation.
pub fn reserved_call_name(expr: &IdedExpr) -> Option<String> {
    find_reserved(&expr.expr)
}

fn find_reserved(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Call(call) => {
            if call.target.is_none()
                && call.func_name == operators::NOT_STRICTLY_FALSE
                && call.args.len() == 1
            {
                if let Expr::Call(inner) = &call.args[0].expr {
                    if inner.target.is_none()
                        && macros::is_has_guard(&inner.func_name)
                        && inner.args.len() == 1
                    {
                        return find_reserved(&inner.args[0].expr);
                    }
                }
            }
            if is_reserved_name(&call.func_name) {
                return Some(call.func_name.clone());
            }
            if let Some(target) = &call.target {
                if let Some(found) = find_reserved(&target.expr) {
                    return Some(found);
                }
            }
            call.args.iter().find_map(|arg| find_reserved(&arg.expr))
        }
        Expr::Select(select) => find_reserved(&select.operand.expr),
        Expr::Comprehension(c) => find_reserved(&c.iter_range.expr)
            .or_else(|| find_reserved(&c.accu_init.expr))
            .or_else(|| find_reserved(&c.loop_cond.expr))
            .or_else(|| find_reserved(&c.loop_step.expr))
            .or_else(|| find_reserved(&c.result.expr)),
        Expr::List(list) => list
            .elements
            .iter()
            .find_map(|item| find_reserved(&item.expr)),
        Expr::Map(map) => map.entries.iter().find_map(|entry| {
            let EntryExpr::MapEntry(e) = &entry.expr else {
                return None;
            };
            find_reserved(&e.key.expr).or_else(|| find_reserved(&e.value.expr))
        }),
        Expr::Struct(s) => s.entries.iter().find_map(|entry| {
            let EntryExpr::StructField(f) = &entry.expr else {
                return None;
            };
            find_reserved(&f.value.expr)
        }),
        Expr::Ident(_) | Expr::Literal(_) | Expr::Unspecified => None,
    }
}

/// A `state.`-rooted dotted read the parse tree contains, and whether it
/// is guarded by `has(...)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatePath {
    pub path: String,
    pub guarded: bool,
}

/// Every `state.` path *expr* reads, plus whether `state` is also named as
/// a value in its own right (`size(state)`) — in which case no projection
/// could narrow what the expression might read, the same distinction
/// `_Compiled.reads_whole_state` draws in `core/cel_eval.py`. Both fields
/// feed [`evaluate_condition`](crate::evaluate_condition): `paths` into
/// [`project`] (what to convert) and the absent-path check (what must
/// resolve), `reads_whole_state` into the choice between the two.
#[derive(Debug, Clone, Default)]
pub struct Collected {
    pub paths: Vec<StatePath>,
    pub reads_whole_state: bool,
}

/// Walks *expr*'s parse tree, collecting every `state.` path.
pub fn collect_state_paths(expr: &IdedExpr) -> Collected {
    let mut out = Collected::default();
    walk(&expr.expr, &mut out, false);
    out
}

/// The segments of *expr* if it is a pure `a.b.c` chain of identifiers and
/// field selections, root first. Anything else in the chain — an index, a
/// call, an arithmetic term — disqualifies it: the result would no longer
/// be a path resolvable structurally against a `Value` tree.
fn dotted_chain(expr: &Expr) -> Option<Vec<&str>> {
    match expr {
        Expr::Ident(name) => Some(vec![name.as_str()]),
        Expr::Select(select) => {
            let mut prefix = dotted_chain(&select.operand.expr)?;
            prefix.push(select.field.as_str());
            Some(prefix)
        }
        _ => None,
    }
}

/// *guarded* is true for everything syntactically under a `has(...)`
/// argument — not just the dotted chain `has()` itself tests, but
/// anything else that argument reads along the way (`state.k` in
/// `has(state.m[state.k].x)`), matching `_collect_state_paths`'s own
/// `guarded` parameter in `core/cel_eval.py`, threaded the same way.
/// `has(X)` has already been expanded, at parse time, into a call to
/// [`macros::is_has_guard`]'s marker function wrapping `X`
/// (`crate::macros`) by the time this walks the tree, so that marker
/// is what turns `guarded` on for everything under it, in place of a
/// field selection's `test` flag.
fn walk(expr: &Expr, out: &mut Collected, guarded: bool) {
    match expr {
        Expr::Ident(name) => {
            if name == "state" {
                out.reads_whole_state = true;
            }
        }
        Expr::Select(select) => match dotted_chain(expr) {
            Some(segments) if segments[0] == "state" => {
                out.paths.push(StatePath {
                    path: segments.join("."),
                    guarded,
                });
            }
            Some(_) => {}
            None => walk(&select.operand.expr, out, guarded),
        },
        Expr::Call(call) => {
            let guarded = guarded || macros::is_has_guard(&call.func_name);
            if let Some(target) = &call.target {
                walk(&target.expr, out, guarded);
            }
            for arg in &call.args {
                walk(&arg.expr, out, guarded);
            }
        }
        Expr::Comprehension(c) => {
            walk(&c.iter_range.expr, out, guarded);
            walk(&c.accu_init.expr, out, guarded);
            walk(&c.loop_cond.expr, out, guarded);
            walk(&c.loop_step.expr, out, guarded);
            walk(&c.result.expr, out, guarded);
        }
        Expr::List(list) => {
            for item in &list.elements {
                walk(&item.expr, out, guarded);
            }
        }
        Expr::Map(map) => {
            for entry in &map.entries {
                if let EntryExpr::MapEntry(e) = &entry.expr {
                    walk(&e.key.expr, out, guarded);
                    walk(&e.value.expr, out, guarded);
                }
            }
        }
        Expr::Struct(s) => {
            for entry in &s.entries {
                if let EntryExpr::StructField(f) = &entry.expr {
                    walk(&f.value.expr, out, guarded);
                }
            }
        }
        Expr::Literal(_) | Expr::Unspecified => {}
    }
}

/// Every free identifier *expr*'s macro-expanded parse tree reads, in
/// source order -- ported from `core/cel_eval.py`'s `free_identifiers`,
/// but over `cel`'s own tree shape rather than celpy's: `all`/`exists`/
/// `exists_one`/`map`/`filter` are already [`Expr::Comprehension`] nodes
/// by the time `cel`'s own parser hands back a compiled expression (its
/// macro expansion runs at parse time, unlike celpy's, which keeps the
/// unexpanded call shape for the evaluator itself to special-case) --
/// `iter_var`/`iter_var2`/`accu_var` are bound only in `loop_cond`/
/// `loop_step`/`result`, never in `iter_range`/`accu_init`, matching
/// cel-spec's own comprehension semantics. A property name after `.`
/// and a function or method name (`call.func_name`, never an `Ident`
/// node) are never identifiers here; a message-construction type name
/// (`Msg{f: 1}`, [`Expr::Struct`]'s own `type_name`) is, the same as
/// `free_identifiers` in Python counts it.
pub fn free_identifiers(expr: &IdedExpr) -> Vec<String> {
    let mut out = Vec::new();
    walk_free_identifiers(&expr.expr, &std::collections::HashSet::new(), &mut out);
    out
}

fn walk_free_identifiers(
    expr: &Expr,
    bound: &std::collections::HashSet<String>,
    out: &mut Vec<String>,
) {
    match expr {
        Expr::Ident(name) => {
            if !bound.contains(name.as_str()) {
                out.push(name.clone());
            }
        }
        Expr::Select(select) => walk_free_identifiers(&select.operand.expr, bound, out),
        Expr::Call(call) => {
            if let Some(target) = &call.target {
                walk_free_identifiers(&target.expr, bound, out);
            }
            for arg in &call.args {
                walk_free_identifiers(&arg.expr, bound, out);
            }
        }
        Expr::Comprehension(c) => {
            walk_free_identifiers(&c.iter_range.expr, bound, out);
            walk_free_identifiers(&c.accu_init.expr, bound, out);
            let mut inner = bound.clone();
            inner.insert(c.iter_var.clone());
            if let Some(var2) = &c.iter_var2 {
                inner.insert(var2.clone());
            }
            inner.insert(c.accu_var.clone());
            walk_free_identifiers(&c.loop_cond.expr, &inner, out);
            walk_free_identifiers(&c.loop_step.expr, &inner, out);
            walk_free_identifiers(&c.result.expr, &inner, out);
        }
        Expr::List(list) => {
            for item in &list.elements {
                walk_free_identifiers(&item.expr, bound, out);
            }
        }
        Expr::Map(map) => {
            for entry in &map.entries {
                if let EntryExpr::MapEntry(e) = &entry.expr {
                    walk_free_identifiers(&e.key.expr, bound, out);
                    walk_free_identifiers(&e.value.expr, bound, out);
                }
            }
        }
        Expr::Struct(s) => {
            if !bound.contains(s.type_name.as_str()) {
                out.push(s.type_name.clone());
            }
            for entry in &s.entries {
                if let EntryExpr::StructField(f) = &entry.expr {
                    walk_free_identifiers(&f.value.expr, bound, out);
                }
            }
        }
        Expr::Literal(_) | Expr::Unspecified => {}
    }
}

/// Rewrites *node*'s parse tree in place so `cel` never has to evaluate
/// a strict-type operator the way it natively would (DESIGN.md §7.2):
/// every `_<_`/`_<=_`/`_>_`/`_>=_`/`_==_`/`_!=_`/`@in`/`_[_]` call
/// becomes a call to [`ordering`]'s, [`equality`]'s or [`indexing`]'s
/// own functions, which reproduce celpy's exact rules (comment on each
/// module) rather than `cel`'s own, more permissive ones. `has(...)` is
/// already handled by the time this runs — [`crate::macros`]'s own
/// `has` macro expands it at parse time, into an ordinary call this
/// function recurses into like any other.
pub fn rewrite(node: &mut IdedExpr) {
    if let Expr::Call(call) = &mut node.expr {
        if call.target.is_none() && call.args.len() == 2 {
            if let Some(name) = ordering::strict_function_name(&call.func_name)
                .or_else(|| equality::strict_function_name(&call.func_name))
                .or_else(|| indexing::strict_function_name(&call.func_name))
            {
                call.func_name = name.to_string();
            }
        }
    }
    match &mut node.expr {
        Expr::Select(select) => rewrite(&mut select.operand),
        Expr::Call(call) => {
            if let Some(target) = &mut call.target {
                rewrite(target);
            }
            for arg in &mut call.args {
                rewrite(arg);
            }
        }
        Expr::Comprehension(c) => {
            rewrite(&mut c.iter_range);
            rewrite(&mut c.accu_init);
            rewrite(&mut c.loop_cond);
            rewrite(&mut c.loop_step);
            rewrite(&mut c.result);
        }
        Expr::List(list) => {
            for item in &mut list.elements {
                rewrite(item);
            }
        }
        Expr::Map(map) => {
            for entry in &mut map.entries {
                if let EntryExpr::MapEntry(e) = &mut entry.expr {
                    rewrite(&mut e.key);
                    rewrite(&mut e.value);
                }
            }
        }
        Expr::Struct(s) => {
            for entry in &mut s.entries {
                if let EntryExpr::StructField(f) = &mut entry.expr {
                    rewrite(&mut f.value);
                }
            }
        }
        Expr::Ident(_) | Expr::Literal(_) | Expr::Unspecified => {}
    }
}

/// A minimal `Value::Dict` carrying just the subtrees *paths* names out
/// of *state*, ported from `core/cel_eval.py`'s `_project`: converting a
/// whole run's state costs time proportional to the state, not to the
/// expression, and more importantly for correctness, a big int `state`
/// holds elsewhere (outside anything this expression reads) must not
/// make [`crate::convert::to_cel`] reject the conversion — `_to_cel` in
/// Python only ever sees the projected subset too.
///
/// *paths* sorted shortest-first, so a whole subtree a shorter path
/// already placed is reused, not re-entered. At an intermediate segment,
/// whatever is already placed under *part* decides what to do — keep
/// descending into it if it's a `Dict` (a shorter path already started
/// this same subtree), or stop if it's anything else (a shorter,
/// unguarded path already placed a *leaf* there, matching Python's own
/// `if placed is value: break`, which this reaches the same outcome as
/// without needing identity comparison — `Value` has none cheap). The
/// terminal segment always just (re-)inserts the freshly read *value*,
/// unconditionally: it's read straight from *state* every time, so it's
/// always the correct thing to place, whether or not something was there
/// before. Comparing the placed and freshly read *values* for equality
/// instead — a previous version of this did, to decide both of the above
/// in one check — was itself the P0 this ported from Python to fix
/// (overwriting a placed non-dict leaf with `{}` the moment a longer,
/// `has()`-guarded path tried to extend past it, issue #379)
/// and *re-broke* the same way for a `NaN` leaf
/// (`has(state.a.n.x) || state.a.n > 0.0`): `NaN != NaN`, so
/// the equality check wrongly decided a *different* value was now being
/// placed and overwrote the leaf with `{}` to keep descending.
pub fn project(state: &Value, paths: &[StatePath]) -> Value {
    let mut unique: Vec<&str> = paths.iter().map(|p| p.path.as_str()).collect();
    unique.sort_by_key(|p| (p.matches('.').count(), *p));
    unique.dedup();

    let mut root = Dict::new();
    for path in unique {
        let segments: Vec<&str> = path.split('.').skip(1).collect();
        let mut source = state;
        let mut target = &mut root;
        for (index, part) in segments.iter().enumerate() {
            let Value::Dict(source_dict) = source else {
                break;
            };
            let Some(value) = source_dict.get(&Value::Str(part.to_string())) else {
                break;
            };
            let key = Value::Str(part.to_string());
            if index == segments.len() - 1 {
                target.insert(key, value.clone());
                break;
            }
            match target.get(&key) {
                Some(Value::Dict(_)) => {}
                Some(_) => break,
                None => {
                    target.insert(key.clone(), Value::Dict(Dict::new()));
                }
            }
            let Some(Value::Dict(next_target)) = target.get_mut(&key) else {
                unreachable!("just inserted or confirmed to be a dict")
            };
            target = next_target;
            source = value;
        }
    }
    Value::Dict(root)
}

/// Whether *path* (a dotted `state.`-rooted chain) resolves against
/// *state*: every segment present, every intermediate a `Value::Dict`,
/// and the final value not `Value::None` (a disabled node writes
/// `{"value": None}`; reading through it is the same "nothing there" as
/// a key never written).
fn resolves(path: &str, state: &Value) -> bool {
    let mut current = state;
    for part in path.split('.').skip(1) {
        let Value::Dict(dict) = current else {
            return false;
        };
        let Some(next) = dict.get(&Value::Str(part.to_string())) else {
            return false;
        };
        current = next;
    }
    !current.is_none()
}

/// The first unguarded path in *paths* that *state* does not supply, in
/// the order *paths* lists them -- used only by this module's own tests
/// now that [`evaluate_condition`](crate::evaluate_condition) decides
/// per occurrence, via [`mark_unset`], rather than structurally over
/// every path at once.
#[cfg(test)]
fn first_unresolved<'a>(paths: &'a [StatePath], state: &Value) -> Option<&'a str> {
    paths
        .iter()
        .find(|p| !p.guarded && !resolves(&p.path, state))
        .map(|p| p.path.as_str())
}

/// Replaces every unguarded `state.` occurrence in *node* whose path does
/// not resolve against *state* with a call to [`UNSET_MARKER_FN`]
/// carrying that path -- one occurrence counts even if the same dotted
/// chain also occurs, elsewhere in the expression, as a `has()` argument
/// (unlike [`first_unresolved`]'s *paths*, which only ever sees whichever
/// occurrence [`collect_state_paths`] happened to keep): removing the
/// "guarded anywhere" exemption is exactly what lets a guard protect only
/// the read it actually guards (issue #657). Runs after
/// [`rewrite`], on the same tree, so this crate's own
/// `__electricity_strict_*`/`__electricity_python_index` substitutions
/// are already in place and are themselves walked like any other call.
pub fn mark_unset(node: &mut IdedExpr, state: &Value) {
    mark(&mut node.expr, state, false);
}

/// Registers [`UNSET_MARKER_FN`] on *ctx*, the same way
/// [`crate::equality::register`], [`crate::ordering::register`],
/// [`crate::indexing::register`] and [`macros::register`] register
/// their own functions. Only ever reached, once called, through a call
/// [`mark_unset`] itself inserted -- a user expression can never name a
/// reserved `__electricity_*` function directly ([`is_reserved_name`],
/// checked earlier) -- so its one argument (the path, as written) is
/// always exactly what the error needs to carry.
pub fn register(ctx: &mut cel::Context) {
    ctx.add_function(UNSET_MARKER_FN, unset_marker_fn)
        .expect("name not already declared");
}

fn unset_marker_fn(ftx: &cel::FunctionContext) -> Result<cel::Value, cel::ExecutionError> {
    let path = ftx
        .args
        .first()
        .and_then(|arg| arg.as_ref().downcast_ref::<CelString>())
        .map(|s| s.inner().to_string())
        .unwrap_or_default();
    Err(cel::ExecutionError::function_error(UNSET_MARKER_FN, path))
}

fn mark(expr: &mut Expr, state: &Value, guarded: bool) {
    if let Expr::Select(_) = expr {
        let chain = dotted_chain(expr).map(|segments| (segments[0] == "state", segments.join(".")));
        match chain {
            Some((true, path)) => {
                if !guarded && !resolves(&path, state) {
                    *expr = Expr::Call(CallExpr {
                        func_name: UNSET_MARKER_FN.to_string(),
                        target: None,
                        args: vec![IdedExpr {
                            id: 0,
                            expr: Expr::Literal(LiteralValue::String(CelString::from(
                                std::borrow::Cow::Owned(path),
                            ))),
                        }],
                    });
                }
            }
            Some((false, _)) => {}
            None => {
                if let Expr::Select(select) = expr {
                    mark(&mut select.operand.expr, state, guarded);
                }
            }
        }
        return;
    }
    match expr {
        Expr::Call(call) => {
            let guarded = guarded || macros::is_has_guard(&call.func_name);
            if let Some(target) = &mut call.target {
                mark(&mut target.expr, state, guarded);
            }
            for arg in &mut call.args {
                mark(&mut arg.expr, state, guarded);
            }
        }
        Expr::Comprehension(c) => {
            mark(&mut c.iter_range.expr, state, guarded);
            mark(&mut c.accu_init.expr, state, guarded);
            mark(&mut c.loop_cond.expr, state, guarded);
            mark(&mut c.loop_step.expr, state, guarded);
            mark(&mut c.result.expr, state, guarded);
        }
        Expr::List(list) => {
            for item in &mut list.elements {
                mark(&mut item.expr, state, guarded);
            }
        }
        Expr::Map(map) => {
            for entry in &mut map.entries {
                if let EntryExpr::MapEntry(e) = &mut entry.expr {
                    mark(&mut e.key.expr, state, guarded);
                    mark(&mut e.value.expr, state, guarded);
                }
            }
        }
        Expr::Struct(s) => {
            for entry in &mut s.entries {
                if let EntryExpr::StructField(f) = &mut entry.expr {
                    mark(&mut f.value.expr, state, guarded);
                }
            }
        }
        Expr::Select(_) => unreachable!("handled above"),
        Expr::Ident(_) | Expr::Literal(_) | Expr::Unspecified => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cel::Context;
    use electricity_value::Dict;
    use std::sync::Arc;

    fn compile(expr: &str) -> IdedExpr {
        macros::env().compile(expr).unwrap().expression().clone()
    }

    #[test]
    fn longest_chain_only() {
        let c = collect_state_paths(&compile("state.a.b.c == 1"));
        assert_eq!(c.paths.len(), 1);
        assert_eq!(c.paths[0].path, "state.a.b.c");
        assert!(!c.paths[0].guarded);
    }

    #[test]
    fn multiple_paths_in_order() {
        let c = collect_state_paths(&compile("state.a.b == 1 && state.c.d == 2"));
        let paths: Vec<&str> = c.paths.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(paths, vec!["state.a.b", "state.c.d"]);
    }

    #[test]
    fn indexed_read_reports_the_resolvable_prefix() {
        let c = collect_state_paths(&compile("state.input.items[0].n == 1"));
        let paths: Vec<&str> = c.paths.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(paths, vec!["state.input.items"]);
    }

    #[test]
    fn non_state_identifiers_are_ignored() {
        let c = collect_state_paths(&compile("item.name == 'x' && 1 == 1"));
        assert!(c.paths.is_empty());
        assert!(!c.reads_whole_state);
    }

    #[test]
    fn string_literal_is_not_a_path() {
        let c = collect_state_paths(&compile("state.a == 'state.b.c'"));
        let paths: Vec<&str> = c.paths.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(paths, vec!["state.a"]);
    }

    #[test]
    fn has_argument_is_guarded() {
        let c = collect_state_paths(&compile("has(state.input.n) && state.input.n > 1"));
        assert_eq!(c.paths.len(), 2);
        assert!(c.paths[0].guarded);
        assert!(!c.paths[1].guarded);
    }

    #[test]
    fn has_argument_inside_an_index_still_guards_its_own_reads() {
        // `has(state.m[state.k].x)`: the has() argument itself isn't a
        // pure dotted chain (it contains an index), so neither `state.m`
        // nor `state.k`, read along the way to decide it, is the chain
        // `has()` itself tests — but both must still inherit guardedness
        // from the enclosing `has()`, not be treated as
        // ordinary unguarded reads.
        let c = collect_state_paths(&compile("has(state.m[state.k].x)"));
        let paths: Vec<(&str, bool)> = c
            .paths
            .iter()
            .map(|p| (p.path.as_str(), p.guarded))
            .collect();
        assert_eq!(paths, vec![("state.m", true), ("state.k", true)]);
    }

    #[test]
    fn macro_variable_is_not_a_state_path() {
        let c = collect_state_paths(&compile("state.input.items.all(i, i > 0)"));
        let paths: Vec<&str> = c.paths.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(paths, vec!["state.input.items"]);
    }

    #[test]
    fn bare_state_is_whole_state_read() {
        let c = collect_state_paths(&compile("size(state) == 2"));
        assert!(c.paths.is_empty());
        assert!(c.reads_whole_state);
    }

    fn eval(expr: &str, state: &Value) -> cel::Value {
        let env = Arc::new(macros::env());
        let program = env.compile(expr).unwrap();
        let mut tree = program.expression().clone();
        rewrite(&mut tree);
        let mut ctx = Context::with_env(Arc::clone(&env));
        ordering::register(&mut ctx);
        equality::register(&mut ctx);
        indexing::register(&mut ctx);
        macros::register(&mut ctx);
        ctx.add_variable_from_value("state", crate::convert::to_cel(state).unwrap());
        cel::Value::resolve(&tree, &ctx).unwrap()
    }

    fn dict_value(pairs: Vec<(&str, Value)>) -> Value {
        let mut d = Dict::new();
        for (k, v) in pairs {
            d.insert(Value::Str(k.to_string()), v);
        }
        Value::Dict(d)
    }

    #[test]
    fn rewrite_has_does_not_leak_into_a_later_unguarded_read() {
        // `!has(state.a.b) || has(state.a.b.c)` on `state = {}`: a prior
        // version of this rewrite patched `state` with empty dicts to make
        // `cel`'s own `has()` gracious, and that patch leaked — the second
        // `has()` saw the first's fabricated `{}` and returned `true`
        // where cel-python returns `false`.
        let state = Value::Dict(Dict::new());
        assert_eq!(
            eval("!has(state.a.b) || has(state.a.b.c)", &state),
            cel::Value::Bool(true)
        );
    }

    #[test]
    fn has_on_a_disabled_node_is_false_not_an_error() {
        let state = dict_value(vec![(
            "prime",
            dict_value(vec![("x", dict_value(vec![("value", Value::None)]))]),
        )]);
        assert_eq!(
            eval("has(state.prime.x.value.price)", &state),
            cel::Value::Bool(false)
        );
    }

    #[test]
    fn has_on_a_list_index_is_false_not_an_error() {
        // `has()`'s argument contains an index, not just a
        // dotted chain — `cel`'s own, native `has()` only evaluates
        // gracefully for the *last* segment, so it raises here; the
        // safe-navigation rewrite handles every step.
        let state = dict_value(vec![("l", Value::List(vec![]))]);
        assert_eq!(eval("has(state.l[0].x)", &state), cel::Value::Bool(false));
    }

    #[test]
    fn has_on_a_map_index_is_false_not_an_error() {
        let state = dict_value(vec![
            ("m", Value::Dict(Dict::new())),
            ("k", Value::Str("a".to_string())),
        ]);
        assert_eq!(
            eval("has(state.m[state.k].x)", &state),
            cel::Value::Bool(false)
        );
    }

    #[test]
    fn has_over_a_filter_result_is_false_not_an_error() {
        // `has()`'s argument contains a macro call
        // (`filter`), not just a dotted/indexed chain over an identifier
        // — a previous version of `to_optional` left this to `cel`'s own,
        // native `has()`, which raises indexing into the empty result.
        let state = dict_value(vec![(
            "results",
            Value::List(vec![dict_value(vec![("ok", Value::Bool(false))])]),
        )]);
        assert_eq!(
            eval("has(state.results.filter(r, r.ok)[0].id)", &state),
            cel::Value::Bool(false)
        );
    }

    #[test]
    fn has_over_a_negative_list_index_matches_plain_indexing() {
        // `has()`'s own safe-navigation chain needs
        // the same Python list-indexing semantics as a plain `[...]`.
        let state = dict_value(vec![(
            "l",
            Value::List(vec![dict_value(vec![("x", Value::from(1_i64))])]),
        )]);
        assert_eq!(eval("has(state.l[-1].x)", &state), cel::Value::Bool(true));
    }

    #[test]
    fn has_over_a_double_list_index_is_false_not_an_error() {
        let state = dict_value(vec![(
            "l",
            Value::List(vec![dict_value(vec![("x", Value::from(1_i64))])]),
        )]);
        assert_eq!(eval("has(state.l[0.0].x)", &state), cel::Value::Bool(false));
    }

    #[test]
    fn has_in_an_index_key_gets_its_own_safe_navigation_too() {
        // A prior version left `has()`'s own index
        // *key* expression to `cel`'s native evaluation, unrewritten --
        // `has(state.sub.deep)` inside the key is a two-level-missing
        // chain that `cel`'s own, native `has()` raises on (it's only
        // graceful for the last segment); without rewriting the key too,
        // that raise would propagate out of the whole expression instead
        // of resolving to the `'b'` branch the way cel-python does.
        let state = dict_value(vec![(
            "m",
            dict_value(vec![("b", dict_value(vec![("x", Value::from(42_i64))]))]),
        )]);
        assert_eq!(
            eval("has(state.m[has(state.sub.deep) ? 'a' : 'b'].x)", &state),
            cel::Value::Bool(true)
        );
    }

    #[test]
    fn has_inside_all_respects_the_loop_variable_not_the_outer_root() {
        // A loop variable named like one of the usual roots
        // (`value`/`state`/`meta`) must shadow it inside the macro — the
        // old rewrite matched `has()`'s root by name alone and always
        // read the *outer* binding.
        let state = dict_value(vec![(
            "rows",
            Value::List(vec![dict_value(vec![("a", dict_value(vec![]))])]),
        )]);
        assert_eq!(
            eval("state.rows.all(r, !has(r.a.b) || r.a.b > 0)", &state),
            cel::Value::Bool(true)
        );
    }

    #[test]
    fn project_keeps_only_the_read_paths() {
        let state = dict_value(vec![
            ("a", dict_value(vec![("b", Value::from(1_i64))])),
            ("unread", Value::from(2_i64)),
        ]);
        let projected = project(
            &state,
            &[StatePath {
                path: "state.a.b".to_string(),
                guarded: false,
            }],
        );
        assert_eq!(
            projected,
            dict_value(vec![("a", dict_value(vec![("b", Value::from(1_i64))]))])
        );
    }

    #[test]
    fn project_merges_overlapping_paths_at_the_same_key() {
        let state = dict_value(vec![(
            "a",
            dict_value(vec![("b", Value::from(1_i64)), ("c", Value::from(2_i64))]),
        )]);
        let projected = project(
            &state,
            &[
                StatePath {
                    path: "state.a.b".to_string(),
                    guarded: false,
                },
                StatePath {
                    path: "state.a.c".to_string(),
                    guarded: false,
                },
            ],
        );
        assert_eq!(projected, state);
    }

    #[test]
    fn project_does_not_corrupt_a_scalar_a_longer_guarded_path_extends() {
        // `state.p.v == 'done' || has(state.p.v.price)` on
        // `{p: {v: 'done'}}`. `state.p.v` (unguarded) and
        // `state.p.v.price` (has()-guarded) are both collected; a
        // previous version of `project` inserted `{}` over the already-
        // placed string `'done'` once it found `v` wasn't a dict, which
        // made the *unguarded* `state.p.v == 'done'` read see `{}`
        // instead.
        let state = dict_value(vec![(
            "p",
            dict_value(vec![("v", Value::Str("done".into()))]),
        )]);
        let projected = project(
            &state,
            &[
                StatePath {
                    path: "state.p.v".to_string(),
                    guarded: false,
                },
                StatePath {
                    path: "state.p.v.price".to_string(),
                    guarded: true,
                },
            ],
        );
        assert_eq!(projected, state);
    }

    #[test]
    fn project_does_not_corrupt_a_nan_leaf_a_longer_path_extends() {
        // `has(state.a.n.x) || state.a.n > 0.0` on
        // `{a: {n: NaN}}`. `state.a.n` (unguarded) and `state.a.n.x`
        // (has()-guarded) are both collected; a value-equality check
        // between the freshly read `NaN` and the already-placed `NaN`
        // is itself `false` (`NaN != NaN`), so a version of `project`
        // relying on that equality to decide "nothing more to do" wrongly
        // kept descending and overwrote the leaf with `{}`.
        let state = dict_value(vec![("a", dict_value(vec![("n", Value::from(f64::NAN))]))]);
        let projected = project(
            &state,
            &[
                StatePath {
                    path: "state.a.n".to_string(),
                    guarded: false,
                },
                StatePath {
                    path: "state.a.n.x".to_string(),
                    guarded: true,
                },
            ],
        );
        let Value::Dict(root) = &projected else {
            panic!("expected a dict")
        };
        let Some(Value::Dict(a)) = root.get(&Value::Str("a".to_string())) else {
            panic!("expected state.a to stay a dict")
        };
        assert!(matches!(a.get(&Value::Str("n".to_string())), Some(Value::Float(f)) if f.is_nan()));
    }

    #[test]
    fn missing_key_is_unresolved() {
        let state = Value::Dict(Dict::new());
        let paths = vec![StatePath {
            path: "state.a.b".into(),
            guarded: false,
        }];
        assert_eq!(first_unresolved(&paths, &state), Some("state.a.b"));
    }

    #[test]
    fn present_value_resolves() {
        let mut inner = Dict::new();
        inner.insert(Value::Str("b".into()), Value::from(1_i64));
        let mut outer = Dict::new();
        outer.insert(Value::Str("a".into()), Value::Dict(inner));
        let state = Value::Dict(outer);
        let paths = vec![StatePath {
            path: "state.a.b".into(),
            guarded: false,
        }];
        assert_eq!(first_unresolved(&paths, &state), None);
    }

    #[test]
    fn none_value_is_unresolved() {
        let mut outer = Dict::new();
        outer.insert(Value::Str("a".into()), Value::None);
        let state = Value::Dict(outer);
        let paths = vec![StatePath {
            path: "state.a".into(),
            guarded: false,
        }];
        assert_eq!(first_unresolved(&paths, &state), Some("state.a"));
    }

    #[test]
    fn guarded_path_is_skipped() {
        let state = Value::Dict(Dict::new());
        let paths = vec![StatePath {
            path: "state.a".into(),
            guarded: true,
        }];
        assert_eq!(first_unresolved(&paths, &state), None);
    }

    fn eval_marked(expr: &str, state: &Value) -> Result<cel::Value, cel::ExecutionError> {
        let env = Arc::new(macros::env());
        let program = env.compile(expr).unwrap();
        let mut tree = program.expression().clone();
        rewrite(&mut tree);
        mark_unset(&mut tree, state);
        let mut ctx = Context::with_env(Arc::clone(&env));
        ordering::register(&mut ctx);
        equality::register(&mut ctx);
        indexing::register(&mut ctx);
        macros::register(&mut ctx);
        register(&mut ctx);
        ctx.add_variable_from_value("state", crate::convert::to_cel(state).unwrap());
        cel::Value::resolve(&tree, &ctx)
    }

    fn unset_marker_path(err: &cel::ExecutionError) -> Option<&str> {
        match err {
            cel::ExecutionError::FunctionError { function, message }
                if function == UNSET_MARKER_FN =>
            {
                Some(message.as_str())
            }
            _ => None,
        }
    }

    #[test]
    fn mark_unset_marks_an_unguarded_occurrence() {
        let state = Value::Dict(Dict::new());
        let err = eval_marked("state.a.b == 1", &state).unwrap_err();
        assert_eq!(unset_marker_path(&err), Some("state.a.b"));
    }

    #[test]
    fn mark_unset_exempts_only_the_has_guarded_occurrence() {
        let state = Value::Dict(Dict::new());
        assert_eq!(
            eval_marked("has(state.a.b)", &state).unwrap(),
            cel::Value::Bool(false)
        );
    }

    #[test]
    fn mark_unset_is_per_occurrence_not_per_path_string() {
        // The exact string "state.a.b" occurs twice: inside has(),
        // which structurally exempts only that one occurrence, and
        // again on the right of `||`, reached because neither side of
        // `&&` decided false -- that second occurrence must still be
        // marked. Removing the old "guarded anywhere" exemption (a
        // path string guarded once was exempt everywhere it recurred)
        // is exactly what lets a guard protect only the read it
        // actually guards (issue #657).
        let state = Value::Dict(Dict::new());
        let err = eval_marked("has(state.a.b) && true || state.a.b == 1", &state).unwrap_err();
        assert_eq!(unset_marker_path(&err), Some("state.a.b"));
    }

    #[test]
    fn mark_unset_leaves_a_resolved_path_untouched() {
        let mut inner = Dict::new();
        inner.insert(Value::Str("b".into()), Value::from(1_i64));
        let mut outer = Dict::new();
        outer.insert(Value::Str("a".into()), Value::Dict(inner));
        let state = Value::Dict(outer);
        assert_eq!(
            eval_marked("state.a.b == 1", &state).unwrap(),
            cel::Value::Bool(true)
        );
    }

    #[test]
    fn mark_unset_survives_a_rewritten_strict_operator() {
        // `rewrite` substitutes `_<=_` for one of this crate's own
        // `__electricity_strict_*` operators before `mark_unset` runs
        // on the same tree -- the unguarded occurrence underneath that
        // substitution must still be found and marked.
        let state = Value::Dict(Dict::new());
        let err = eval_marked("state.a.b <= 1", &state).unwrap_err();
        assert_eq!(unset_marker_path(&err), Some("state.a.b"));
    }
}
