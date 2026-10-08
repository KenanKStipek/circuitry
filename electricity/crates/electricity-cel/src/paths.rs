//! The structural absent-path walk behind `evaluate_condition`
//! (runtime-semantics.md §4.4, ports `_collect_state_paths`/
//! `_first_unresolved`, `core/cel_eval.py`).
//!
//! Reading an unset `state.` path is decided *before* evaluation, by
//! walking the parsed expression for every dotted `state.` read, not by
//! catching a failure during evaluation — `has(...)` guards one path at a
//! time, and a genuinely malformed expression must still raise rather
//! than silently resolve to "absent".

use cel::common::ast::{EntryExpr, Expr, IdedExpr, LiteralValue};
use cel::common::types::CelBool;
use electricity_value::{Dict, Value};

use crate::ordering;

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
/// `_Compiled.reads_whole_state` draws in `core/cel_eval.py`. Unused by
/// `evaluate_condition` today (the whole `state` binding is always built),
/// kept because it is the direct, tested port of the Python structural
/// walk the absent-path convention depends on.
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
/// a `has(...)` call or a numeric ordering operator the way it natively
/// would (DESIGN.md §7.2):
///
/// - Every `has(...)` argument that is a pure dotted chain rooted at one
///   of *roots* (`state`/`value`/`meta`) becomes a `bool` literal,
///   computed by [`has_result`] walking the matching `Value` directly —
///   cel-python's own rule (`evaluation.py`'s `ident_arg` `has`: "the
///   argument evaluated without error"), not `cel`'s. `cel`'s own
///   `has()` only evaluates gracefully for the *last* segment
///   (`objects.rs`'s `select_field`); every segment before it is a
///   plain, non-test field selection that raises `NoSuchKey`/an overload
///   error the moment it hits a missing key or a non-container —
///   patching the bound data to paper over that (as a prior version of
///   this function did) leaks fabricated empty dicts into every other
///   read of the same path in the same expression. Replacing the whole
///   node with a literal avoids the problem instead of working around
///   it: there is no data to patch, and nothing for a patch to leak
///   into.
/// - Every `_<_`/`_<=_`/`_>_`/`_>=_` call becomes a call to this crate's
///   own [`ordering`] functions, which raise for an `int` on the *left*
///   compared against a different numeric type (`state.n < 1.5`) the
///   way `celtypes.IntType`'s own `@type_matched` does and `cel`'s own
///   `PartialOrd for Value` does not — asymmetrically, matching celpy:
///   `celtypes.UintType`/`DoubleType` never override ordering at all, so
///   `1.5 > state.n` doesn't raise (DESIGN.md §7.2, §3.2's naive/aware
///   split is unrelated).
///
/// A `has()` argument that isn't a pure dotted chain (an index, a call)
/// or whose root isn't one of *roots* (a comprehension's own loop
/// variable, say) is left for `cel` to evaluate as-is, matching this
/// function's own restriction before the rewrite existed.
pub fn rewrite(node: &mut IdedExpr, roots: &[(&str, &Value)]) {
    if let Expr::Select(select) = &node.expr {
        if select.test {
            let chain = dotted_chain(&node.expr)
                .map(|segments| segments.into_iter().map(String::from).collect::<Vec<_>>());
            if let Some(chain) = chain {
                if let Some(&(_, root)) = roots.iter().find(|&&(name, _)| name == chain[0]) {
                    let present = has_result(root, &chain[1..]);
                    node.expr = Expr::Literal(LiteralValue::Boolean(if present {
                        CelBool::TRUE
                    } else {
                        CelBool::FALSE
                    }));
                    return;
                }
            }
        }
    }
    if let Expr::Call(call) = &mut node.expr {
        if call.target.is_none() && call.args.len() == 2 {
            if let Some(name) = ordering::strict_function_name(&call.func_name) {
                call.func_name = name.to_string();
            }
        }
    }
    match &mut node.expr {
        Expr::Select(select) => rewrite(&mut select.operand, roots),
        Expr::Call(call) => {
            if let Some(target) = &mut call.target {
                rewrite(target, roots);
            }
            for arg in &mut call.args {
                rewrite(arg, roots);
            }
        }
        Expr::Comprehension(c) => {
            rewrite(&mut c.iter_range, roots);
            rewrite(&mut c.accu_init, roots);
            rewrite(&mut c.loop_cond, roots);
            rewrite(&mut c.loop_step, roots);
            rewrite(&mut c.result, roots);
        }
        Expr::List(list) => {
            for item in &mut list.elements {
                rewrite(item, roots);
            }
        }
        Expr::Map(map) => {
            for entry in &mut map.entries {
                if let EntryExpr::MapEntry(e) = &mut entry.expr {
                    rewrite(&mut e.key, roots);
                    rewrite(&mut e.value, roots);
                }
            }
        }
        Expr::Struct(s) => {
            for entry in &mut s.entries {
                if let EntryExpr::StructField(f) = &mut entry.expr {
                    rewrite(&mut f.value, roots);
                }
            }
        }
        Expr::Ident(_) | Expr::Literal(_) | Expr::Unspecified => {}
    }
}

/// Whether *chain* (the segments after the root; the last one is the
/// field `has()` tests) is present in *root*, matching cel-python's
/// `has()` exactly: every segment up to the last must resolve through a
/// `Value::Dict`, and the last only has to be a key that *exists* — its
/// value, even `Value::None`, doesn't matter (`evaluation.py`'s
/// `member_dot`, a plain dict lookup, not a presence-of-non-null check).
fn has_result(root: &Value, chain: &[String]) -> bool {
    let mut current = root;
    for part in &chain[..chain.len() - 1] {
        let Value::Dict(dict) = current else {
            return false;
        };
        let Some(next) = dict.get(&Value::Str(part.clone())) else {
            return false;
        };
        current = next;
    }
    let Value::Dict(dict) = current else {
        return false;
    };
    dict.contains_key(&Value::Str(chain[chain.len() - 1].clone()))
}

/// A minimal `Value::Dict` carrying just the subtrees *paths* names out
/// of *state*, ported from `core/cel_eval.py`'s `_project`: converting a
/// whole run's state costs time proportional to the state, not to the
/// expression, and more importantly for correctness, a big int `state`
/// holds elsewhere (outside anything this expression reads) must not
/// make [`crate::convert::to_cel`] reject the conversion — `_to_cel` in
/// Python only ever sees the projected subset too.
///
/// Unlike `_project`, this doesn't special-case an already-captured
/// subtree by object identity (Python's `placed is value` check): with
/// *paths* sorted shortest-first, a longer path's walk only ever reuses
/// or extends a dict a shorter path already placed, so skipping that
/// check changes nothing about the final shape, only how much redundant
/// (but harmless) re-insertion happens getting there.
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
            if index == segments.len() - 1 {
                target.insert(Value::Str(part.to_string()), value.clone());
                break;
            }
            if !matches!(
                target.get(&Value::Str(part.to_string())),
                Some(Value::Dict(_))
            ) {
                target.insert(Value::Str(part.to_string()), Value::Dict(Dict::new()));
            }
            let Some(Value::Dict(next_target)) = target.get_mut(&Value::Str(part.to_string()))
            else {
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
    use cel::Env;
    use electricity_value::Dict;

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

    fn dict_value(pairs: Vec<(&str, Value)>) -> Value {
        let mut d = Dict::new();
        for (k, v) in pairs {
            d.insert(Value::Str(k.to_string()), v);
        }
        Value::Dict(d)
    }

    #[test]
    fn has_result_requires_every_intermediate_segment_present() {
        let state = dict_value(vec![("input", dict_value(vec![("n", Value::from(5_i64))]))]);
        assert!(has_result(&state, &["input".to_string(), "n".to_string()]));
        assert!(!has_result(
            &state,
            &["input".to_string(), "missing".to_string()]
        ));
        assert!(!has_result(
            &state,
            &["missing".to_string(), "n".to_string()]
        ));
    }

    #[test]
    fn has_result_is_true_for_a_present_null_value() {
        // A key mapped to `Value::None` is still *present*: cel-python's
        // `has()` is "the dict lookup didn't raise", not "the value
        // isn't null" (evaluation.py's member_dot).
        let state = dict_value(vec![("a", Value::None)]);
        assert!(has_result(&state, &["a".to_string()]));
    }

    #[test]
    fn has_result_is_false_through_a_non_dict_intermediate() {
        // A disabled node writes `{"value": None}`; selecting a further
        // field through that `None` is the exact case `cel`'s own `has()`
        // raises on instead of returning false (finding 1).
        let state = dict_value(vec![("value", Value::None)]);
        assert!(!has_result(
            &state,
            &["value".to_string(), "price".to_string()]
        ));
    }

    #[test]
    fn rewrite_has_does_not_leak_into_a_later_unguarded_read() {
        // `!has(state.a.b) || has(state.a.b.c)` on `state = {}`: a prior
        // version of this rewrite patched `state` with empty dicts to make
        // `cel`'s own `has()` gracious, and that patch leaked — the second
        // `has()` saw the first's fabricated `{}` and returned `true`
        // where cel-python returns `false` (finding 1).
        let state = Value::Dict(Dict::new());
        let mut tree = compile("!has(state.a.b) || has(state.a.b.c)");
        rewrite(&mut tree, &[("state", &state)]);
        let env = cel::Env::stdlib();
        let mut ctx = cel::Context::with_env(std::sync::Arc::new(env));
        ctx.add_variable_from_value("state", crate::convert::to_cel(&state).unwrap());
        assert_eq!(cel::Value::resolve(&tree, &ctx), Ok(cel::Value::Bool(true)));
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
