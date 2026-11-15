//! One explicit limit on a CEL expression's nesting depth
//! ([`MAX_CEL_NESTING_DEPTH`]), checked once right after the expression
//! parses, before anything evaluates it -- the same number, the same
//! depth count and the same message `core/cel_eval.py` enforces, so the
//! two engines reject exactly the same expressions rather than each
//! failing wherever its own recursion happens to give out (which used
//! to differ by shape, and for cof, by Python version too).
//!
//! **Depth** counts one level per parenthesis, call, list/map literal,
//! member-access/index/method-call link, binary arithmetic/relational/
//! equality/`in` link, logical `&&`/`||` link and prefix `!`/`-`
//! occurrence -- every other grammar production is a pure wrapper and
//! adds nothing. [`ast_nesting_depth`] walks `cel`'s own parsed
//! [`cel::common::ast::IdedExpr`] tree to count most of these, but
//! three shapes leave no trace of their own *link count* in that tree
//! and need [`text_nesting_depth`]'s own separate scan of the source
//! text instead (confirmed directly, not assumed):
//!
//! - a grouping parenthesis has no AST node of its own at all
//!   (`(((1)))` parses to exactly the same tree as `1`);
//! - `cel`'s own parser folds a long `&&`/`||` *run* into a balanced
//!   binary tree rather than cof's own left-linear one, so its depth
//!   grows with the *logarithm* of the run's length, not the length
//!   itself (33 `||`-joined terms is AST depth 6, not 33);
//! - a run of prefix `!`/`-` collapses to exactly one AST node
//!   regardless of the run's own length (`cel`'s own grammar collects
//!   the whole run before building a node at all), so three `!`s and
//!   thirty `!`s are both AST depth 1.
//!
//! [`text_nesting_depth`]'s `group_paren`/`logical_run`/`unary_run`
//! fields are this module's own substitute count for exactly those
//! three, each tracking the true *link count* cof's own evaluator
//! would see; its `combined` field is unrelated, a pre-parse safety
//! margin (below), not a precision count.
//!
//! **Why a pre-parse guard is still needed at all**, now that the real
//! check happens on the parsed tree: `cel`'s own `Env::compile` always
//! uses its ANTLR-generated parser, whose visitor recurses once per
//! level of bracket nesting *and* once per link of an operator or
//! member/index/call chain while *building* that tree -- a chain shape
//! genuinely overflows a 2 MiB stack between 1,620 and 1,640 terms
//! (measured directly, debug and `--release` alike), well within the
//! 4096-character expression cap (roughly 2,048 terms for the shortest
//! two-character link). [`MAX_SAFETY_CHAIN_DEPTH`] (1,000) is a
//! generous margin under that floor -- [`text_nesting_depth`]'s own
//! combined count, checked before `compile()` is ever called, keeps an
//! expression that deep from reaching the real parser at all. Bracket
//! nesting (parens, calls, list/map literals) cannot threaten the
//! stack this way regardless: `cel` 0.15.0's own parser has a
//! hard-coded internal recursion guard (`max_recursion_depth`, 96) over
//! every such grammar production on one path at once, which rejects
//! *gracefully* from 96 through at least 50,000 levels, never a crash
//! (confirmed directly). [`crate::is_parser_recursion_limit`] recognizes
//! that guard's own message and reports it the same uniform way as
//! every other over-limit rejection.
//!
//! Since [`MAX_CEL_NESTING_DEPTH`] (32) is far under both the parser's
//! own 96-bracket guard and the 1,620-term real-overflow floor, the
//! only expressions that ever reach either of those two mechanisms are
//! ones already well past this module's own limit -- both are
//! insurance, never the boundary an ordinary document can actually
//! reach.
//!
//! **Known, accepted imprecision for a mixed shape.** A grouping paren
//! wrapping some other construct (`(state.a + state.b)`) is checked as
//! two independent maxima -- [`ast_nesting_depth`] for the addition,
//! [`text_nesting_depth`] for the parenthesis -- not as one combined
//! path, because a grouping paren leaves no trace in the parsed tree to
//! correlate the two against. A pathological expression mixing both
//! shapes right at the limit can therefore pass here while cof's own,
//! single combined count would reject it. This mirrors the same
//! mixed-shape divergence this module's own caps always had (now in one
//! direction instead of two): accepted, not required to close, because
//! no real orchestration's expression approaches this limit in even one
//! shape, let alone two at once (issue #778's own PR body records the
//! deepest expression found in this repository's own documents).

use cel::common::ast::{EntryExpr, Expr, IdedEntryExpr, IdedExpr};

/// One explicit limit on a CEL expression's nesting depth, the same on
/// both engines (`core/cel_eval.py::MAX_CEL_NESTING_DEPTH`'s own module
/// docs have the full measurement this number is chosen against:
/// cof's own evaluator, run on CPython 3.10-3.13, first overflows at 49
/// levels for its tightest shape, a nested list or map literal; every
/// other shape's own floor is higher). 32 sits comfortably under that
/// floor, with margin for a Python version this hasn't been measured
/// against yet and for whatever stack an expression's own calling
/// context has already spent by the time it is checked.
pub const MAX_CEL_NESTING_DEPTH: usize = 32;

/// A generous, safety-only ceiling on [`text_nesting_depth`]'s combined
/// count, checked *before* ever calling `cel::Env::compile` -- not a
/// parity boundary with cof at all (module docs: this crate's own real
/// stack-overflow floor for a chain shape is 1,620-1,640 terms, so this
/// is margin under that, not under [`MAX_CEL_NESTING_DEPTH`]).
pub const MAX_SAFETY_CHAIN_DEPTH: usize = 1_000;

/// The deepest nesting level under *expr*: one per call (global or
/// method, an operator included -- `cel`'s own parser desugars every
/// operator, and an index, to an ordinary call, module docs), member
/// access, list/map/struct literal, and comprehension (the macro
/// expansion `.map(...)`/`.filter(...)`/`.all(...)`/... becomes,
/// module docs) -- every other [`Expr`] variant (`Ident`, `Literal`,
/// `Unspecified`) is a leaf and adds nothing. Never a grouping
/// parenthesis, which leaves no trace here at all (module docs) --
/// see [`text_nesting_depth`] for that.
pub fn ast_nesting_depth(expr: &IdedExpr) -> usize {
    match &expr.expr {
        Expr::Call(call) => {
            let mut depth = call.target.as_deref().map_or(0, ast_nesting_depth);
            for arg in &call.args {
                depth = depth.max(ast_nesting_depth(arg));
            }
            depth + 1
        }
        Expr::Select(select) => ast_nesting_depth(&select.operand) + 1,
        Expr::List(list) => {
            list.elements
                .iter()
                .map(ast_nesting_depth)
                .max()
                .unwrap_or(0)
                + 1
        }
        Expr::Map(map) => entries_depth(&map.entries) + 1,
        Expr::Struct(s) => entries_depth(&s.entries) + 1,
        // Only `iter_range` -- what the macro is chained onto -- counts
        // the way a member/method-call link does; `accu_init`/
        // `loop_cond`/`loop_step`/`result` are this macro's own
        // synthesized accumulator-loop boilerplate (a fixed shape for
        // any `.map`/`.filter`/`.all`/`.exists`/`.exists_one` call,
        // never something the expression's own author wrote), and
        // walking them would count that boilerplate's own incidental
        // depth instead of the chain's -- confirmed directly: a single
        // `.map(x, x)` link's `loop_step` alone is already 2 levels
        // deep before any chaining at all, which `core/cel_eval.py`
        // does not reproduce (celpy never desugars a macro into a loop
        // at parse time, so `.map(x, x)` costs this count exactly 1
        // level there, matching every other link). A deeply nested
        // transform or predicate *inside* one macro call
        // (`.filter(x, state.a.b.c...)`) is consequently not counted by
        // this walk at all -- the same kind of accepted imprecision as
        // a grouping paren's own (module docs above), for the same
        // reason: no real orchestration nests inside one of these far
        // enough for it to matter.
        Expr::Comprehension(c) => ast_nesting_depth(&c.iter_range) + 1,
        Expr::Ident(_) | Expr::Literal(_) | Expr::Unspecified => 0,
    }
}

/// Whether *expr* (parsed as *ast*) is nested past [`MAX_CEL_NESTING_DEPTH`]
/// -- the one check every entry point in this crate runs right after
/// parsing, combining [`ast_nesting_depth`] and the grouping-paren half
/// of [`text_nesting_depth`] (module docs on why both are needed).
pub fn exceeds_max_nesting_depth(expr: &str, ast: &IdedExpr) -> bool {
    let text = text_nesting_depth(expr);
    let depth = ast_nesting_depth(ast)
        .max(text.group_paren)
        .max(text.logical_run)
        .max(text.unary_run);
    depth > MAX_CEL_NESTING_DEPTH
}

fn entries_depth(entries: &[IdedEntryExpr]) -> usize {
    entries
        .iter()
        .map(|e| match &e.expr {
            EntryExpr::StructField(f) => ast_nesting_depth(&f.value),
            EntryExpr::MapEntry(m) => ast_nesting_depth(&m.key).max(ast_nesting_depth(&m.value)),
        })
        .max()
        .unwrap_or(0)
}

/// Four numbers from one left-to-right scan of *expr*'s raw source
/// text, skipping string/bytes literals and `//` comments (mirrors
/// CEL's own `STRING`/`BYTES`/`COMMENT` grammar for that, not a
/// general-purpose tokenizer):
///
/// - `group_paren`: the deepest *grouping* parenthesis nesting reached
///   -- a `(` immediately preceded by an identifier or a closing
///   bracket is a call's own `(`, already counted once the expression
///   parses ([`ast_nesting_depth`]'s own docs), and is skipped here
///   entirely, never pushed onto the bracket stack this counts.
/// - `logical_run`: the deepest simultaneous `&&`/`||` link count
///   reached -- [`ast_nesting_depth`]'s own module docs on why this
///   substitutes for the AST, which folds a run into a balanced tree
///   instead of counting its true length.
/// - `unary_run`: the longest run of *adjacent* prefix `!`/`-`
///   reached (never a `-` immediately following a value, which is
///   subtraction, module docs above on why the AST needs a substitute
///   for this one too). Two non-adjacent prefix occurrences -- a `!`
///   that is not immediately followed by another `!`/`-`, such as
///   `!(!x)` -- are *not* a run in this sense and need no substitute:
///   `cel`'s own grammar only collapses a strictly adjacent run, so
///   each one already gets its own AST node, correctly counted by
///   [`ast_nesting_depth`].
/// - `combined`: a conservative, nesting-aware (not a flat token
///   count -- a long run of independent, shallow terms is not
///   penalized for its length) upper bound on every bracket/chain
///   shape at once, [`MAX_SAFETY_CHAIN_DEPTH`]'s own input -- excludes
///   `&&`/`||`/prefix `!`/`-` entirely (none of them recurses in
///   `cel`'s own grammar enough to threaten a real stack overflow,
///   module docs above), and over-counts everything else relative to
///   [`ast_nesting_depth`] by design (every call's own `(` is counted
///   here too, not skipped) -- both are fine for a safety-only ceiling
///   checked at a much higher threshold than [`MAX_CEL_NESTING_DEPTH`].
pub struct TextNestingDepths {
    pub group_paren: usize,
    pub logical_run: usize,
    pub unary_run: usize,
    pub combined: usize,
}

/// Which bracket a stack entry came from, so closing it decrements the
/// right counter.
#[derive(Clone, Copy, PartialEq, Eq)]
enum OpenBracket {
    GroupParen,
    Other,
}

pub fn text_nesting_depth(expr: &str) -> TextNestingDepths {
    let mut group_paren_depth: usize = 0;
    let mut other_depth: usize = 0;
    let mut chain_run: usize = 0;
    let mut logical_run_depth: usize = 0;
    let mut unary_run_depth: usize = 0;
    let mut max_group_paren: usize = 0;
    let mut max_logical_run: usize = 0;
    let mut max_unary_run: usize = 0;
    let mut max_combined: usize = 0;
    // Whether the token just scanned could be the left operand of a
    // binary operator, or the receiver of `.`/`[`/a call's `(` -- an
    // identifier, keyword literal (`true`/`false`/`null`), number,
    // string/bytes literal, or a closing `)`/`]`/`}`.
    let mut prev_value_end = false;
    // Saved (group_paren_depth, other_depth, chain_run, logical_run_depth)
    // for each open bracket, restored on close -- a chain or logical
    // run that continues after a closing bracket (`a + f(b) + c`,
    // `a && f(b) && c`) picks back up where it left off, rather than
    // losing what it had counted before the bracket opened. `unary_run`
    // needs no such entry: a prefix run always ends exactly at the
    // bracket boundary itself, on either side (module docs).
    let mut stack: Vec<(OpenBracket, usize, usize, usize, usize)> = Vec::new();

    let chars: Vec<char> = expr.chars().collect();
    let n = chars.len();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        match c {
            '\'' | '"' => {
                i = skip_string_from(&chars, i);
                prev_value_end = true;
                unary_run_depth = 0;
                continue;
            }
            '/' if i + 1 < n && chars[i + 1] == '/' => {
                while i < n && chars[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            '0'..='9' => {
                i += 1;
                while i < n && chars[i].is_ascii_digit() {
                    i += 1;
                }
                if i + 1 < n && chars[i] == '.' && chars[i + 1].is_ascii_digit() {
                    i += 1;
                    while i < n && chars[i].is_ascii_digit() {
                        i += 1;
                    }
                }
                if i < n && (chars[i] == 'e' || chars[i] == 'E') {
                    let mut j = i + 1;
                    if j < n && (chars[j] == '+' || chars[j] == '-') {
                        j += 1;
                    }
                    if j < n && chars[j].is_ascii_digit() {
                        i = j;
                        while i < n && chars[i].is_ascii_digit() {
                            i += 1;
                        }
                    }
                }
                prev_value_end = true;
                unary_run_depth = 0;
                continue;
            }
            c if c.is_alphabetic() || c == '_' => {
                let start = i;
                i += 1;
                while i < n && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let word: &str = &expr[byte_offset(&chars, start)..byte_offset(&chars, i)];
                unary_run_depth = 0;
                if word == "in" {
                    chain_run += 1;
                    bump(
                        group_paren_depth + other_depth + chain_run,
                        &mut max_combined,
                    );
                    prev_value_end = false;
                } else {
                    prev_value_end = true;
                }
                continue;
            }
            '(' if prev_value_end => {
                // A call's own `(` -- already counted via the AST once
                // parsing succeeds, so only `other_depth` (the
                // safety-only combined count) sees it, never
                // `group_paren_depth`.
                other_depth += 1;
                bump(
                    group_paren_depth + other_depth + chain_run,
                    &mut max_combined,
                );
                stack.push((
                    OpenBracket::Other,
                    group_paren_depth,
                    other_depth,
                    chain_run,
                    logical_run_depth,
                ));
                prev_value_end = false;
                unary_run_depth = 0;
            }
            '(' => {
                group_paren_depth += 1;
                bump(group_paren_depth, &mut max_group_paren);
                bump(
                    group_paren_depth + other_depth + chain_run,
                    &mut max_combined,
                );
                stack.push((
                    OpenBracket::GroupParen,
                    group_paren_depth,
                    other_depth,
                    chain_run,
                    logical_run_depth,
                ));
                prev_value_end = false;
                unary_run_depth = 0;
            }
            '[' | '{' => {
                other_depth += 1;
                bump(
                    group_paren_depth + other_depth + chain_run,
                    &mut max_combined,
                );
                stack.push((
                    OpenBracket::Other,
                    group_paren_depth,
                    other_depth,
                    chain_run,
                    logical_run_depth,
                ));
                prev_value_end = false;
                unary_run_depth = 0;
            }
            ')' | ']' | '}' => {
                if let Some((kind, saved_paren, saved_other, saved_chain, saved_logical)) =
                    stack.pop()
                {
                    match kind {
                        OpenBracket::GroupParen => {
                            group_paren_depth = saved_paren.saturating_sub(1)
                        }
                        OpenBracket::Other => other_depth = saved_other.saturating_sub(1),
                    }
                    chain_run = saved_chain;
                    logical_run_depth = saved_logical;
                }
                prev_value_end = true;
                unary_run_depth = 0;
            }
            '&' if i + 1 < n && chars[i + 1] == '&' => {
                // Excluded from `combined` entirely (its own docs on
                // why), but tracked for `logical_run` -- the AST's own
                // substitute for a run this long (module docs above).
                i += 2;
                logical_run_depth += 1;
                bump(logical_run_depth, &mut max_logical_run);
                prev_value_end = false;
                unary_run_depth = 0;
                continue;
            }
            '|' if i + 1 < n && chars[i + 1] == '|' => {
                i += 2;
                logical_run_depth += 1;
                bump(logical_run_depth, &mut max_logical_run);
                prev_value_end = false;
                unary_run_depth = 0;
                continue;
            }
            '+' | '-' | '*' | '/' | '%' | '<' | '>' | '=' | '!' => {
                // A *binary* arithmetic/relational/equality operator
                // (`prev_value_end`) bumps `chain_run`, `combined`'s
                // own input. A *prefix* `!`/`-` (never any of the
                // other characters this arm matches, which have no
                // valid prefix use in CEL at all) bumps `unary_run`
                // instead -- `logical_run`'s own sibling substitute for
                // a shape the AST also collapses (module docs above).
                // `<=`/`>=`/`==`/`!=` are two characters, but counting
                // each once each only ever makes `combined` larger,
                // never smaller, which is harmless for a safety-only
                // ceiling.
                i += 1;
                if prev_value_end {
                    chain_run += 1;
                    bump(
                        group_paren_depth + other_depth + chain_run,
                        &mut max_combined,
                    );
                    unary_run_depth = 0;
                } else if c == '!' || c == '-' {
                    unary_run_depth += 1;
                    bump(unary_run_depth, &mut max_unary_run);
                } else {
                    unary_run_depth = 0;
                }
                prev_value_end = false;
                continue;
            }
            ',' | '?' | ':' => {
                // Resets even inside an open bracket: a list/call's
                // own argument separator starts a new, independent
                // sub-expression, never a continuation of whatever
                // chain or logical run came before it -- unlike the
                // full precision `combined` doesn't need
                // (`nesting.rs`'s module docs), there is no case where
                // under-counting this way hides a genuine
                // arithmetic/relational/logical chain risk, since such
                // a chain never contains a comma of its own.
                chain_run = 0;
                logical_run_depth = 0;
                unary_run_depth = 0;
            }
            _ => {}
        }
        i += 1;
    }
    TextNestingDepths {
        group_paren: max_group_paren,
        logical_run: max_logical_run,
        unary_run: max_unary_run,
        combined: max_combined,
    }
}

fn bump(value: usize, max: &mut usize) {
    if value > *max {
        *max = value;
    }
}

fn byte_offset(chars: &[char], char_index: usize) -> usize {
    chars[..char_index].iter().map(|c| c.len_utf8()).sum()
}

/// Skips a string/bytes literal starting at *i* (the opening quote),
/// handling `'...'`/`"..."`/`'''...'''`/`"""..."""`, any optionally
/// `b`/`B`/`r`/`R`-prefixed, and returns the index just past its
/// closing quote (or `chars.len()` for an unterminated one). A
/// backslash always escapes the next character, matching every CEL
/// string form's own grammar closely enough for this pre-scan.
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
