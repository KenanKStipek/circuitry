//! One explicit limit on a CEL expression's nesting depth
//! ([`MAX_CEL_NESTING_DEPTH`]), checked once right after the expression
//! parses, before anything evaluates it -- the same number, the same
//! depth count and the same message `core/cel_eval.py` enforces, so
//! the two engines reject exactly the same expressions rather than
//! each failing wherever its own recursion happens to give out (which
//! used to differ by shape, and for cof, by Python version too).
//!
//! **Depth** counts one level per parenthesis, call, list/map/struct
//! literal, member-access/index/method-call link, binary arithmetic/
//! relational/equality/`in` link, logical `&&`/`||` link, prefix `!`/`-`
//! occurrence, ternary and macro -- every other grammar production is a
//! pure wrapper and adds nothing (`core/cel_eval.py`'s own
//! `_CHAIN_RULES`/`_ALWAYS_NESTS` docstrings enumerate the same list).
//!
//! [`ast_nesting_depth`] walks `cel`'s own parsed
//! [`cel::common::ast::IdedExpr`] tree, which already matches cof's own
//! left-linear structure exactly for every binary arithmetic/
//! relational/equality/`in` chain (`cel`'s own `visit_calc`/
//! `visit_relation` build a plain left-recursive [`Expr::Call`] the
//! same way cof's grammar does) and for every member-access/index/
//! method-call/list/map/struct/ternary construct ([`Expr::Select`],
//! [`Expr::List`], [`Expr::Map`]/[`Expr::Struct`], and the ternary's own
//! [`Expr::Call`] of three args) -- a plain recursive `max(children) +
//! 1` walk reproduces cof's count for every one of these with no
//! further help.
//!
//! Three shapes don't: `cel`'s own parser flattens a `&&`/`||` *run*
//! into a balanced binary tree rather than cof's own left-linear one
//! ([`chain_depth`] undoes that, weighting each flattened operand the
//! way cof's left fold would -- its own docs), a grouping parenthesis
//! has no AST node of its own at all (`(((1)))` parses to exactly the
//! same tree as `1`), and a run of prefix `!`/`-` collapses to zero or
//! one AST node by parity regardless of the run's own length. The
//! latter two leave no trace in the AST to recover from source text
//! either, in general (a grouping paren wrapping a `&&`/`||` operand
//! inside a larger run of the same operator is genuinely
//! indistinguishable from not having been there at all, since
//! flattening elides it the same way), so `cel`'s own parser visitor
//! records both directly, by id, while parsing, in
//! [`cel::common::ast::SourceInfo::nesting_override`] -- the exact
//! extra level(s) a consumer walking the AST alone would otherwise
//! miss, attached to whatever node the construct resolves to either
//! way (`vendor/cel/src/parser/parser.rs`'s `visit_Nested`/
//! `visit_LogicalNot`/`visit_Negate`, each the module this crate
//! vendors rather than something this crate owns). [`ast_nesting_depth`]
//! adds it in at every node, uniformly.
//!
//! A macro -- `cel`'s own `.map`/`.filter`/`.all`/`.exists`/
//! `.exists_one`, each an [`Expr::Comprehension`], and this crate's own
//! `has()` (an ordinary [`Expr::Call`], replaced wholesale, DESIGN.md
//! §7.2) alike -- desugars to synthesized boilerplate (a fixed shape
//! for any call to the same macro, never something the expression's
//! own author wrote) wrapping the user's own loop variable and
//! predicate/transform/tested argument -- walking that boilerplate
//! directly would count its own incidental depth instead of the
//! argument's own. [`cel::common::ast::SourceInfo::macro_body_ids`]
//! records each macro's own original argument ids, by the expansion's
//! own result id, in `cel::parser::Macro::expand` itself (every macro's
//! single call site, this crate's own `has()` included); [`find_by_id`]
//! locates each one inside whatever boilerplate the expansion wove it
//! into.
//!
//! **Why a pre-parse guard is still needed at all**, before any of the
//! above ever runs: `cel`'s own `Env::compile` always uses its
//! ANTLR-generated parser, whose visitor recurses once per level of
//! bracket nesting *and* once per link of an operator or member/index/
//! call chain while *building* that tree -- a chain shape genuinely
//! overflows a 2 MiB stack between 1,620 and 1,640 terms (measured
//! directly, debug and `--release` alike), well within the
//! 4096-character expression cap (roughly 2,048 terms for the shortest
//! two-character link). [`MAX_SAFETY_CHAIN_DEPTH`] (1,000) is a
//! generous margin under that floor -- [`safety_upper_bound`], checked
//! on the raw source text before `compile()` is ever called, keeps an
//! expression that deep from reaching the real parser at all; the same
//! floor every node [`ast_nesting_depth`] itself ever recurses into has
//! already passed, so its own recursion is bounded by it too. Bracket
//! nesting (parens, calls, list/map literals) cannot threaten the
//! stack this way regardless: `cel` 0.15.0's own parser has a
//! hard-coded internal recursion guard (`max_recursion_depth`, 96) over
//! every such grammar production on one path at once, which rejects
//! *gracefully* from 96 through at least 50,000 levels, never a crash
//! (confirmed directly). [`crate::is_parser_recursion_limit`] recognizes
//! that guard's own message and reports it the same uniform way as
//! every other over-limit rejection, never as a parse error.
//!
//! Since [`MAX_CEL_NESTING_DEPTH`] (32) is far under both the parser's
//! own 96-bracket guard and the 1,620-term real-overflow floor, the
//! only expressions that ever reach either of those two mechanisms are
//! ones already well past this module's own limit -- both are
//! insurance, never the boundary an ordinary document can actually
//! reach, and [`safety_upper_bound`]'s own count never undercounts
//! [`ast_nesting_depth`]'s, so anything it rejects is also over
//! [`MAX_CEL_NESTING_DEPTH`] -- reported the same uniform way.

use cel::common::ast::{EntryExpr, Expr, IdedEntryExpr, IdedExpr, SourceInfo, operators};

/// One explicit limit on a CEL expression's nesting depth, the same on
/// both engines (`core/cel_eval.py::MAX_CEL_NESTING_DEPTH`'s own module
/// docs have the full measurement this number is chosen against:
/// cof's own evaluator, run on CPython 3.10-3.13, first overflows at 49
/// levels for its tightest shape, a nested list or map literal; every
/// other shape's own floor is higher, up to 822 for an else-if ternary
/// ladder). 32 sits comfortably under that floor, with margin for a
/// Python version this hasn't been measured against yet and for
/// whatever stack an expression's own calling context has already
/// spent by the time it is checked.
pub const MAX_CEL_NESTING_DEPTH: usize = 32;

/// A generous, safety-only ceiling on [`safety_upper_bound`]'s count,
/// checked *before* ever calling `cel::Env::compile` -- not a parity
/// boundary with cof at all (module docs: this crate's own real
/// stack-overflow floor for a chain shape is 1,620-1,640 terms, so this
/// is margin under that, not under [`MAX_CEL_NESTING_DEPTH`]).
pub const MAX_SAFETY_CHAIN_DEPTH: usize = 1_000;

/// Whether *expr* (parsed as *ast*, with *info* the same parse's own
/// [`SourceInfo`]) is nested past [`MAX_CEL_NESTING_DEPTH`] -- the one
/// precision check every entry point in this crate runs right after
/// parsing.
pub fn exceeds_max_nesting_depth(ast: &IdedExpr, info: &SourceInfo) -> bool {
    ast_nesting_depth(ast, info) > MAX_CEL_NESTING_DEPTH
}

/// The deepest nesting level under *expr*, matching
/// `core/cel_eval.py::_nesting_depth` exactly (module docs): the AST's
/// own structure for every construct it represents left-linearly
/// already, [`chain_depth`] for the two it flattens into a balanced
/// tree instead, and *info*'s own per-id
/// [`SourceInfo::nesting_override`] added in uniformly for the two that
/// leave no AST trace of their own length at all.
pub fn ast_nesting_depth(expr: &IdedExpr, info: &SourceInfo) -> usize {
    intrinsic_depth(expr, info) + info.nesting_override(expr.id)
}

/// *expr*'s own depth contribution from its [`Expr`] variant alone,
/// before [`SourceInfo::nesting_override`] is added in -- the split
/// exists only so [`chain_depth`] can look up a child's *full*
/// depth (intrinsic plus override) while deciding whether to flatten
/// through it (its own docs).
fn intrinsic_depth(expr: &IdedExpr, info: &SourceInfo) -> usize {
    let body_ids = info.macro_body_ids(expr.id);
    if !body_ids.is_empty() {
        // A macro's own call, whatever expression it expanded to --
        // `cel`'s own `exists`/`all`/`exists_one`/`map`/`filter`, each
        // an `Expr::Comprehension`, and this crate's own `has()`
        // (`!@not_strictly_false(__electricity_false(X))`, an
        // `Expr::Call`) alike. One level for the call every one of
        // these is in cof's own grammar, over whichever of its own
        // original arguments -- the loop variable included, though it
        // never has depth of its own -- nests deepest; never the
        // synthesized wrapper around them, which [`find_by_id`] skips
        // straight past.
        let mut depth = 0;
        for &body_id in body_ids {
            let mut budget = 256u32;
            if let Some(body) = find_by_id(expr, body_id, &mut budget) {
                depth = depth.max(ast_nesting_depth(body, info));
            }
        }
        return depth + 1;
    }
    match &expr.expr {
        Expr::Call(call)
            if is_unary_prefix(&call.func_name)
                && call.target.is_none()
                && call.args.len() == 1 =>
        {
            // No level of its own: the run's full, correct depth
            // (module docs -- cof's grammar nests one level per `!`/`-`
            // with no cancellation, unlike this node, which exists only
            // if the run's length was odd) is already in the single
            // target's own `nesting_override`, added in when
            // `ast_nesting_depth` recurses into it below.
            ast_nesting_depth(&call.args[0], info)
        }
        Expr::Call(call)
            if is_logical_chain(&call.func_name)
                && call.target.is_none()
                && call.args.len() == 2 =>
        {
            chain_depth(call, info)
        }
        Expr::Call(call) => {
            let mut depth = call
                .target
                .as_deref()
                .map_or(0, |t| ast_nesting_depth(t, info));
            for arg in &call.args {
                depth = depth.max(ast_nesting_depth(arg, info));
            }
            depth + 1
        }
        // `select.test` is `has(...)`'s own marker on the outermost
        // field of the path it tests -- cof counts `has(a.b.c)` as the
        // member chain's own depth (one per `.`) *plus* one more for
        // the call `has(...)` itself always is in its own grammar,
        // where this AST has no separate call node for it at all.
        Expr::Select(select) => {
            let base = ast_nesting_depth(&select.operand, info) + 1;
            if select.test { base + 1 } else { base }
        }
        Expr::List(list) => {
            list.elements
                .iter()
                .map(|e| ast_nesting_depth(e, info))
                .max()
                .unwrap_or(0)
                + 1
        }
        Expr::Map(map) => entries_depth(&map.entries, info) + 1,
        Expr::Struct(s) => entries_depth(&s.entries, info) + 1,
        // Only reachable for a comprehension with no macro args
        // registered at all (the generic case above handles every
        // real macro's own comprehension, loop variable and body
        // included) -- a defensive fallback, not a path any of this
        // crate's own macros take.
        Expr::Comprehension(c) => ast_nesting_depth(&c.iter_range, info) + 1,
        Expr::Ident(_) | Expr::Literal(_) | Expr::Unspecified => 0,
    }
}

fn is_unary_prefix(func_name: &str) -> bool {
    func_name == operators::LOGICAL_NOT || func_name == operators::NEGATE
}

fn is_logical_chain(func_name: &str) -> bool {
    func_name == operators::LOGICAL_OR || func_name == operators::LOGICAL_AND
}

fn entries_depth(entries: &[IdedEntryExpr], info: &SourceInfo) -> usize {
    entries
        .iter()
        .map(|e| match &e.expr {
            EntryExpr::StructField(f) => ast_nesting_depth(&f.value, info),
            EntryExpr::MapEntry(m) => {
                ast_nesting_depth(&m.key, info).max(ast_nesting_depth(&m.value, info))
            }
        })
        .max()
        .unwrap_or(0)
}

/// cof's own left fold over a run of *k* operands joined by the same
/// chain operator (`&&`/`||`, here; `core/cel_eval.py::_nesting_depth`
/// applies the identical weighting to every other chain rule too, but
/// the AST already matches it there without help -- module docs)
/// nests operand *j* (0-indexed from the left) under exactly *k - 1*
/// levels if *j* is 0, or *k - j* otherwise: the two leftmost operands
/// tie at the deepest, each one step further right one level
/// shallower, because cof's grammar wraps a new node around the
/// *growing left chain* at every step, so whichever operand is on that
/// chain's own growing edge picks up one more wrapping level than the
/// one before it, except the very first step, which has no earlier
/// node to wrap.
///
/// `cel`'s own parser builds a *balanced* tree for the run instead
/// (module docs), so [`flatten_chain`] undoes that first, back into the
/// flat, ordered operand list this weighting needs.
fn chain_depth(call: &cel::common::ast::CallExpr, info: &SourceInfo) -> usize {
    let mut leaves = Vec::new();
    for arg in &call.args {
        flatten_chain(arg, &call.func_name, info, &mut leaves);
    }
    let k = leaves.len();
    let mut best = 0;
    for (j, leaf) in leaves.iter().enumerate() {
        let weight = if j == 0 { k.saturating_sub(1) } else { k - j };
        best = best.max(weight + ast_nesting_depth(leaf, info));
    }
    best
}

/// Collects *node*'s own flattened operands for *op* into *out*, left
/// to right: *node* itself if it is a plain `Call(op, [l, r])` with no
/// [`SourceInfo::nesting_override`] of its own (an internal node
/// `cel`'s own balanced-tree split introduced, not a boundary the
/// expression's own author wrote -- continue flattening through it),
/// recursing into both its args the same way; *node* as a single,
/// opaque leaf otherwise, override included -- a different operator, a
/// non-chain expression, or *this* same operator but wrapped in a
/// grouping parenthesis of its own (`nesting_override(node.id) != 0`),
/// which makes it one flattened operand of the outer run rather than a
/// continuation of it, the same way cof's own grammar would nest a
/// parenthesized `(a || b)` as one `conditionaland` term rather than
/// splicing its own operands into the surrounding run.
fn flatten_chain<'a>(node: &'a IdedExpr, op: &str, info: &SourceInfo, out: &mut Vec<&'a IdedExpr>) {
    if info.nesting_override(node.id) == 0 {
        if let Expr::Call(c) = &node.expr {
            if c.func_name == op && c.target.is_none() && c.args.len() == 2 {
                flatten_chain(&c.args[0], op, info, out);
                flatten_chain(&c.args[1], op, info, out);
                return;
            }
        }
    }
    out.push(node);
}

/// *node* or one of its descendants with id *target*, after *budget*
/// visits (searching only ever a macro's own small, fixed synthesized
/// shape -- module docs -- so running out without a match means
/// *target* is simply in a different field, not a sign of anything
/// pathological, not a reason to keep spending the budget). Descends
/// into a nested [`Expr::Comprehension`]'s own fields too -- every id
/// is unique to the one node the parser built it for, so a search for
/// one macro's own argument id can never resolve to a different
/// macro's own, nested or not.
fn find_by_id<'a>(node: &'a IdedExpr, target: u64, budget: &mut u32) -> Option<&'a IdedExpr> {
    if *budget == 0 {
        return None;
    }
    *budget -= 1;
    if node.id == target {
        return Some(node);
    }
    match &node.expr {
        Expr::Call(call) => call
            .target
            .as_deref()
            .and_then(|t| find_by_id(t, target, budget))
            .or_else(|| call.args.iter().find_map(|a| find_by_id(a, target, budget))),
        Expr::Select(select) => find_by_id(&select.operand, target, budget),
        Expr::List(list) => list
            .elements
            .iter()
            .find_map(|e| find_by_id(e, target, budget)),
        Expr::Map(map) => map.entries.iter().find_map(|e| match &e.expr {
            EntryExpr::MapEntry(m) => {
                find_by_id(&m.key, target, budget).or_else(|| find_by_id(&m.value, target, budget))
            }
            EntryExpr::StructField(f) => find_by_id(&f.value, target, budget),
        }),
        Expr::Struct(s) => s.entries.iter().find_map(|e| match &e.expr {
            EntryExpr::MapEntry(m) => {
                find_by_id(&m.key, target, budget).or_else(|| find_by_id(&m.value, target, budget))
            }
            EntryExpr::StructField(f) => find_by_id(&f.value, target, budget),
        }),
        Expr::Comprehension(c) => find_by_id(&c.iter_range, target, budget)
            .or_else(|| find_by_id(&c.loop_cond, target, budget))
            .or_else(|| find_by_id(&c.loop_step, target, budget))
            .or_else(|| find_by_id(&c.result, target, budget)),
        Expr::Ident(_) | Expr::Literal(_) | Expr::Unspecified => None,
    }
}

/// A conservative, nesting-aware (not a flat token count -- a long run
/// of independent, shallow terms is not penalized for its length)
/// upper bound on every bracket/chain shape at once, from one
/// left-to-right scan of *expr*'s raw source text, skipping string/
/// bytes literals and `//` comments -- [`MAX_SAFETY_CHAIN_DEPTH`]'s own
/// input, checked before `cel::Env::compile` is ever called at all, so
/// this can only ever over-count relative to [`ast_nesting_depth`] (by
/// design: every call's own `(` is counted here too, not skipped the
/// way the real check does) -- never under, which is what makes
/// rejecting anything over [`MAX_SAFETY_CHAIN_DEPTH`] here safe to
/// report the same uniform way the real check's own rejection is
/// (module docs).
pub fn safety_upper_bound(expr: &str) -> usize {
    let mut bracket_depth: usize = 0;
    let mut chain_run: usize = 0;
    let mut max_combined: usize = 0;
    // One combined bracket-depth-plus-chain-run entry per open bracket,
    // so a chain that continues after a closing bracket (`a + f(b) + c`)
    // picks back up where it left off, rather than losing what it had
    // counted before the bracket opened.
    let mut stack: Vec<usize> = Vec::new();
    let chars: Vec<char> = expr.chars().collect();
    let n = chars.len();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        match c {
            '\'' | '"' => {
                i = skip_string_from(&chars, i);
                continue;
            }
            '/' if i + 1 < n && chars[i + 1] == '/' => {
                while i < n && chars[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            '(' | '[' | '{' => {
                bracket_depth += 1;
                bump(bracket_depth + chain_run, &mut max_combined);
                stack.push(chain_run);
            }
            ')' | ']' | '}' => {
                bracket_depth = bracket_depth.saturating_sub(1);
                chain_run = stack.pop().unwrap_or(0);
            }
            '+' | '-' | '*' | '/' | '%' | '<' | '>' | '=' | '!' | '&' | '|' => {
                chain_run += 1;
                bump(bracket_depth + chain_run, &mut max_combined);
            }
            ',' | '?' | ':' => {
                // A list/call's own argument separator, or a ternary's
                // own punctuation, starts a new, independent
                // sub-expression, never a continuation of whatever
                // chain came before it.
                chain_run = 0;
            }
            _ => {}
        }
        i += 1;
    }
    max_combined
}

fn bump(value: usize, max: &mut usize) {
    if value > *max {
        *max = value;
    }
}

fn skip_string_from(chars: &[char], i: usize) -> usize {
    let quote = chars[i];
    let n = chars.len();
    let triple = i + 2 < n && chars[i + 1] == quote && chars[i + 2] == quote;
    let mut j = i + if triple { 3 } else { 1 };
    while j < n {
        if chars[j] == '\\' {
            j += 2;
            continue;
        }
        if triple {
            if j + 2 < n && chars[j] == quote && chars[j + 1] == quote && chars[j + 2] == quote {
                return j + 3;
            }
        } else if chars[j] == quote {
            return j + 1;
        }
        j += 1;
    }
    n
}
