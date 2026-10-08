//! The structural absent-path walk behind `evaluate_condition`
//! (runtime-semantics.md §4.4, ports `_collect_state_paths`/
//! `_first_unresolved`, `core/cel_eval.py`), and the AST rewrite behind
//! `has()` and strict-type operators (DESIGN.md §7.2).
//!
//! Reading an unset `state.` path is decided *before* evaluation, by
//! walking the parsed expression for every dotted `state.` read, not by
//! catching a failure during evaluation — `has(...)` guards one path at a
//! time, and a genuinely malformed expression must still raise rather
//! than silently resolve to "absent".

use cel::common::ast::{CallExpr, EntryExpr, Expr, IdedExpr, LiteralValue};
use cel::common::types::CelString;
use electricity_value::{Dict, Value};

use crate::{equality, ordering};

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
                    guarded: guarded || select.test,
                });
            }
            Some(_) => {}
            None => walk(&select.operand.expr, out, guarded || select.test),
        },
        Expr::Call(call) => {
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

/// Rewrites *node*'s parse tree in place so `cel` never has to evaluate
/// a `has(...)` call or a strict-type operator the way it natively would
/// (DESIGN.md §7.2):
///
/// - Every `has(...)` call becomes `<safe-navigation chain>.hasValue()`
///   when its argument is a chain of plain field selections and `[...]`
///   indices (see [`to_optional`]): `has(a.b[c].d)` becomes
///   `(a.?b[?c].?d).hasValue()`, built from `cel`'s own optional-value
///   primitives (`_?._`/`_[?_]`, on by default — `Env::with_optional_support`),
///   not a custom function. `cel`'s `_?._`/`_[?_]` already turn *any*
///   evaluation failure along the chain — a missing key, an out-of-range
///   index, a selection through the wrong type — into `optional.none()`
///   rather than raising (`objects.rs`'s `unwrap_optional`/`index_into`),
///   which is exactly cel-python's own rule for `has()`: "the argument
///   evaluated without error" (`evaluation.py`'s `ident_arg` `has`), not
///   "the last segment alone resolves gracefully" the way `cel`'s own,
///   native `has()` treats it. Because every step becomes ordinary
///   evaluation under `?.`/`[?]` — not a Rust-side walk against a
///   snapshot of `state`/`value`/`meta` taken before evaluation — this
///   handles a chain rooted at *any* identifier, including a
///   comprehension's own loop variable (`value.items.all(value, has(value.x))`
///   correctly tests the inner, loop-bound `value`, not the outer root a
///   prior, name-matching version of this rewrite confused it for), and
///   an index whose key is itself an arbitrary sub-expression
///   (`has(state.m[state.k].x)`), with no data to patch and nothing for
///   a patch to leak into.
/// - Every `_<_`/`_<=_`/`_>_`/`_>=_`/`_==_`/`_!=_`/`@in` call becomes a
///   call to [`ordering`]'s or [`equality`]'s own functions, which
///   reproduce celpy's exact rules (comment on each module) rather than
///   `cel`'s own, more permissive ones.
///
/// A `has(...)` argument [`to_optional`] can't rewrite (anything other
/// than a chain of field selections and indices — a macro, a plain
/// function call) is left for `cel` to evaluate as-is, matching this
/// function's own restriction before the rewrite existed.
pub fn rewrite(node: &mut IdedExpr) {
    if let Expr::Select(select) = &node.expr {
        if select.test {
            if let Some(operand) = to_optional(&select.operand.expr) {
                let chain = opt_select(operand, &select.field);
                node.expr = Expr::Call(CallExpr {
                    func_name: "hasValue".to_string(),
                    target: Some(Box::new(IdedExpr { id: 0, expr: chain })),
                    args: Vec::new(),
                });
                return;
            }
        }
    }
    if let Expr::Call(call) = &mut node.expr {
        if call.target.is_none() && call.args.len() == 2 {
            if let Some(name) = ordering::strict_function_name(&call.func_name)
                .or_else(|| equality::strict_function_name(&call.func_name))
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

/// *expr* rewritten into CEL's safe-navigation form (`_?._` for a field
/// selection, `_[?_]` for an index), or `None` if *expr* isn't a chain of
/// those over a leaf (an identifier or a literal) — a macro, a plain
/// function call, anything [`rewrite`]'s own restriction already left
/// alone before `has()` could rewrite at all. Every step this *can*
/// convert never raises: a missing key, a missing index or a selection
/// through the wrong type all become `optional.none()`
/// (`objects.rs`'s `unwrap_optional`/`index_into`, `Err(_) =>
/// CelOptional::none()`), and a later step chains off a `None` by simply
/// propagating it, never re-evaluating.
fn to_optional(expr: &Expr) -> Option<Expr> {
    match expr {
        Expr::Ident(_) | Expr::Literal(_) => Some(expr.clone()),
        Expr::Select(select) if !select.test => {
            let operand = to_optional(&select.operand.expr)?;
            Some(opt_select(operand, &select.field))
        }
        Expr::Call(call)
            if call.target.is_none()
                && call.args.len() == 2
                && call.func_name == cel::common::ast::operators::INDEX =>
        {
            let operand = to_optional(&call.args[0].expr)?;
            Some(Expr::Call(CallExpr {
                func_name: cel::common::ast::operators::OPT_INDEX.to_string(),
                target: None,
                args: vec![
                    IdedExpr {
                        id: 0,
                        expr: operand,
                    },
                    call.args[1].clone(),
                ],
            }))
        }
        _ => None,
    }
}

/// `*operand*.?*field*` (`_?._`), the optional-select `cel` already
/// implements — a missing field, or a selection through a type that
/// doesn't support one, both become `optional.none()`.
fn opt_select(operand: Expr, field: &str) -> Expr {
    Expr::Call(CallExpr {
        func_name: cel::common::ast::operators::OPT_SELECT.to_string(),
        target: None,
        args: vec![
            IdedExpr {
                id: 0,
                expr: operand,
            },
            IdedExpr {
                id: 0,
                expr: Expr::Literal(LiteralValue::String(CelString::from(field.to_string()))),
            },
        ],
    })
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
/// already placed is reused, not re-entered — mirroring Python's own
/// `if placed is value: break` by comparing *values*, not identities
/// (`Value` has no cheap notion of the latter): when the two are equal,
/// nothing a longer path could add is missing, and — the bug a previous
/// version of this had (issue #379 review finding 1) — overwriting
/// `target[part]` with a placeholder `{}` just because *value* itself
/// isn't a dict would otherwise corrupt what the shorter path already
/// placed there.
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
            if target.get(&key) == Some(value) {
                break;
            }
            if index == segments.len() - 1 {
                target.insert(key, value.clone());
                break;
            }
            if !matches!(target.get(&key), Some(Value::Dict(_))) {
                target.insert(key.clone(), Value::Dict(Dict::new()));
            }
            let Some(Value::Dict(next_target)) = target.get_mut(&key) else {
                unreachable!("just inserted or already a dict")
            };
            target = next_target;
            source = value;
        }
    }
    Value::Dict(root)
}

/// The paths in *paths* subject to the absent-state rule: a path guarded
/// by `has(...)` *anywhere* it occurs in the expression is exempt
/// everywhere it occurs, even where that same dotted chain is read again
/// unguarded (`has(state.input.n) && state.input.n > 1`) — guardedness is
/// a property of the path string, not of one occurrence of it, matching
/// `core/cel_eval.py`'s `_Compiled.read_paths`.
fn read_paths(paths: &[StatePath]) -> impl Iterator<Item = &str> {
    let guarded: std::collections::HashSet<&str> = paths
        .iter()
        .filter(|p| p.guarded)
        .map(|p| p.path.as_str())
        .collect();
    paths
        .iter()
        .filter(move |p| !p.guarded && !guarded.contains(p.path.as_str()))
        .map(|p| p.path.as_str())
}

/// The first path `read_paths` yields that *state* does not supply: a
/// missing segment, a segment that isn't a `Value::Dict`, or a path that
/// resolves to `Value::None` (a disabled node writes `{"value": None}`;
/// reading through it is the same "nothing there" as a key never
/// written).
pub fn first_unresolved<'a>(paths: &'a [StatePath], state: &Value) -> Option<&'a str> {
    for path in read_paths(paths) {
        let mut current = state;
        let mut resolved = true;
        for part in path.split('.').skip(1) {
            let Value::Dict(dict) = current else {
                resolved = false;
                break;
            };
            let Some(next) = dict.get(&Value::Str(part.to_string())) else {
                resolved = false;
                break;
            };
            current = next;
        }
        if !resolved || current.is_none() {
            return Some(path);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use cel::{Context, Env};
    use electricity_value::Dict;
    use std::sync::Arc;

    fn compile(expr: &str) -> IdedExpr {
        Env::stdlib().compile(expr).unwrap().expression().clone()
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
        // from the enclosing `has()` (finding 6), not be treated as
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
        let env = Arc::new(Env::stdlib());
        let program = env.compile(expr).unwrap();
        let mut tree = program.expression().clone();
        rewrite(&mut tree);
        let mut ctx = Context::with_env(Arc::clone(&env));
        ordering::register(&mut ctx);
        equality::register(&mut ctx);
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
        // where cel-python returns `false` (finding 1).
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
        // finding 3: `has()`'s argument contains an index, not just a
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
    fn has_inside_all_respects_the_loop_variable_not_the_outer_root() {
        // finding 4: a loop variable named like one of the usual roots
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
        // finding 1: `state.p.v == 'done' || has(state.p.v.price)` on
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
}
