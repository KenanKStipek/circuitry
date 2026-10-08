//! The structural absent-path walk behind `evaluate_condition`
//! (runtime-semantics.md §4.4, ports `_collect_state_paths`/
//! `_first_unresolved`, `core/cel_eval.py`).
//!
//! Reading an unset `state.` path is decided *before* evaluation, by
//! walking the parsed expression for every dotted `state.` read, not by
//! catching a failure during evaluation — `has(...)` guards one path at a
//! time, and a genuinely malformed expression must still raise rather
//! than silently resolve to "absent".

use cel::common::ast::{EntryExpr, Expr, IdedExpr};
use electricity_value::Value;

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
    walk(&expr.expr, &mut out);
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

fn walk(expr: &Expr, out: &mut Collected) {
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
                    guarded: select.test,
                });
            }
            Some(_) => {}
            None => walk(&select.operand.expr, out),
        },
        Expr::Call(call) => {
            if let Some(target) = &call.target {
                walk(&target.expr, out);
            }
            for arg in &call.args {
                walk(&arg.expr, out);
            }
        }
        Expr::Comprehension(c) => {
            walk(&c.iter_range.expr, out);
            walk(&c.accu_init.expr, out);
            walk(&c.loop_cond.expr, out);
            walk(&c.loop_step.expr, out);
            walk(&c.result.expr, out);
        }
        Expr::List(list) => {
            for item in &list.elements {
                walk(&item.expr, out);
            }
        }
        Expr::Map(map) => {
            for entry in &map.entries {
                if let EntryExpr::MapEntry(e) = &entry.expr {
                    walk(&e.key.expr, out);
                    walk(&e.value.expr, out);
                }
            }
        }
        Expr::Struct(s) => {
            for entry in &s.entries {
                if let EntryExpr::StructField(f) = &entry.expr {
                    walk(&f.value.expr, out);
                }
            }
        }
        Expr::Literal(_) | Expr::Unspecified => {}
    }
}

/// Every `has(...)` argument anywhere in *expr*'s parse tree, root first
/// and including the root (`["state", "input", "n"]` for
/// `has(state.input.n)`), regardless of which root it names — `state`,
/// or, for `expect:`, `value`/`meta` too.
///
/// `cel`'s own `has()` only guards its *last* segment: evaluating
/// `select_field(value, field, false)` (a plain, non-test select) for
/// every segment up to that one raises `NoSuchKey` the moment one of
/// *those* is missing too, instead of the whole chain gracefully
/// resolving to `false` the way `cel-python`'s (and the CEL spec's) own
/// `has()` does. [`ensure_has_target_parents`] closes that gap at the
/// data level — no intermediate segment of a `has()` argument is ever
/// truly missing by the time `cel` evaluates it — rather than reaching
/// for `cel-core` (DESIGN.md §7.2) for what is otherwise a one-function
/// difference.
pub fn guarded_chains(expr: &IdedExpr) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    collect_guarded(&expr.expr, &mut out);
    out
}

fn collect_guarded(expr: &Expr, out: &mut Vec<Vec<String>>) {
    match expr {
        Expr::Select(select) => match dotted_chain(expr) {
            Some(segments) => {
                if select.test {
                    out.push(segments.into_iter().map(String::from).collect());
                }
            }
            None => collect_guarded(&select.operand.expr, out),
        },
        Expr::Call(call) => {
            if let Some(target) = &call.target {
                collect_guarded(&target.expr, out);
            }
            for arg in &call.args {
                collect_guarded(&arg.expr, out);
            }
        }
        Expr::Comprehension(c) => {
            collect_guarded(&c.iter_range.expr, out);
            collect_guarded(&c.accu_init.expr, out);
            collect_guarded(&c.loop_cond.expr, out);
            collect_guarded(&c.loop_step.expr, out);
            collect_guarded(&c.result.expr, out);
        }
        Expr::List(list) => {
            for item in &list.elements {
                collect_guarded(&item.expr, out);
            }
        }
        Expr::Map(map) => {
            for entry in &map.entries {
                if let EntryExpr::MapEntry(e) = &entry.expr {
                    collect_guarded(&e.key.expr, out);
                    collect_guarded(&e.value.expr, out);
                }
            }
        }
        Expr::Struct(s) => {
            for entry in &s.entries {
                if let EntryExpr::StructField(f) = &entry.expr {
                    collect_guarded(&f.value.expr, out);
                }
            }
        }
        Expr::Ident(_) | Expr::Literal(_) | Expr::Unspecified => {}
    }
}

/// Ensures every segment of *chain* up to (but not including) the last —
/// the one `has()` actually tests — resolves to a `Value::Dict` within
/// *root*, inserting an empty `Dict` wherever one is missing so `cel`'s
/// own (only-the-last-segment-is-graceful) `has()` evaluation never hits
/// a genuinely missing intermediate key. *chain* includes the root name
/// itself at `chain[0]`; *root* is the bound value for that name (always
/// `state`/`value`/`meta`, never mutated in place by a caller — pass a
/// clone made for binding, not the caller's own data).
///
/// Stops early, changing nothing further, the moment a segment already
/// present isn't a `Dict` — `cel`'s own evaluation is left to decide what
/// a presence test through a non-container means.
pub fn ensure_has_target_parents(root: &mut Value, chain: &[String]) {
    if chain.len() < 3 {
        return; // no intermediate segment between the root and the tested field
    }
    let mut current = root;
    for part in &chain[1..chain.len() - 1] {
        let Value::Dict(dict) = current else {
            return;
        };
        if !dict.contains_key(&Value::Str(part.clone())) {
            dict.insert(
                Value::Str(part.clone()),
                Value::Dict(electricity_value::Dict::new()),
            );
        }
        current = dict
            .get_mut(&Value::Str(part.clone()))
            .expect("just inserted or already present");
    }
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
    fn guarded_chains_finds_has_arguments_by_root() {
        let chains = guarded_chains(&compile("has(state.input.n) && has(value.prompt_id)"));
        assert_eq!(
            chains,
            vec![
                vec!["state".to_string(), "input".to_string(), "n".to_string()],
                vec!["value".to_string(), "prompt_id".to_string()],
            ]
        );
    }

    #[test]
    fn ensure_has_target_parents_inserts_missing_intermediates() {
        let mut state = Value::Dict(Dict::new());
        let chain = vec!["state".to_string(), "input".to_string(), "n".to_string()];
        ensure_has_target_parents(&mut state, &chain);
        let Value::Dict(dict) = &state else {
            panic!("expected a dict")
        };
        assert!(matches!(
            dict.get(&Value::Str("input".into())),
            Some(Value::Dict(_))
        ));
    }

    #[test]
    fn ensure_has_target_parents_leaves_the_tested_field_alone() {
        let mut state = Value::Dict(Dict::new());
        let chain = vec!["state".to_string(), "a".to_string()];
        ensure_has_target_parents(&mut state, &chain);
        assert_eq!(state, Value::Dict(Dict::new()));
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
