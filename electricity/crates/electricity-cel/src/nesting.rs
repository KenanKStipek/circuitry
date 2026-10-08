//! A cheap pre-scan of an expression's nesting, run before handing it to
//! the `cel` crate's own ANTLR-generated parser.
//!
//! `cel`'s own `Env::compile` always uses that parser (`Env::parser`,
//! `cel-0.15.0/src/env.rs`), not the crate's separate, feature-gated
//! `PrattParser` (`parser_pratt`, off by default and not enabled by this
//! crate's `Cargo.toml`) -- an earlier version of this module's docs
//! claimed the opposite. The ANTLR visitor recurses once per level for
//! every binary operator chain (`calc`/`relation` in the grammar:
//! arithmetic, comparison, equality, `in`) and every postfix member/
//! index/call link (`member`'s own left-recursive alternatives: `.field`,
//! `[i]`, `.method(...)`) -- `cel-0.15.0/src/parser/parser.rs`'s
//! `visit_calc`/`visit_relation`/`visit_Select`/`visit_Index`/
//! `visit_MemberCall`, each calling `self.visit(lhs/member/target)`
//! before doing anything else, exactly the way a bracket does. Measured
//! directly (this module's own tests, `tests/nesting_limit.rs`, and
//! `Cargo.toml`'s `[profile.dev.package.cel]`/
//! `[profile.dev.package.antlr4rust]` comment), on a 2 MiB thread stack
//! (the default a spawned thread gets, where the runner does most of
//! this work), within [`crate::MAX_EXPR_LENGTH`] (4096 characters):
//!
//! - an unoptimized debug build overflowed parsing as few as **12**
//!   levels of nested parentheses, brackets, or map/list literals;
//! - `0+1+1+...` (an arithmetic chain) overflowed somewhere between
//!   1600 and 1700 terms, in *both* an unoptimized debug build and
//!   `--release`;
//! - `state[0][0][0]...` (a sequential index chain, never two brackets
//!   open at once) overflowed at 1300 terms, in both debug and release;
//! - `state.a.a.a...` (a sequential field-select chain), `0<1<2<...`
//!   (a sequential relation chain) and `state.f().f().f()...` (a
//!   sequential method-call chain) did **not** overflow even at the
//!   deepest each can reach under the 4096-character cap (about 2045,
//!   1080 and 1020 terms respectively) -- but that is a coincidence of
//!   today's stack-frame sizes against today's character cap, not a
//!   structural difference: `member`'s own grammar makes `.field`/
//!   `.method(...)` exactly as left-recursive as `[i]`, which *does*
//!   overflow at a similar depth. Raising [`crate::MAX_EXPR_LENGTH`], a
//!   platform with smaller stack frames, or a future `cel` upgrade could
//!   turn that margin negative without anything in this crate changing,
//!   so this pre-scan treats all of them as equally unsafe rather than
//!   trusting the measurement never to have been close.
//!
//! Chained unary `!`/`-` and a long `&&`/`||` chain are the two shapes
//! that genuinely never cost a stack frame per occurrence, not just
//! ones this measurement happened not to overflow: the grammar collects
//! every `!`/`-` in a run into one node's `ops: Vec` (`unary`'s
//! `LogicalNot`/`Negate` alternatives), visited once regardless of the
//! run's length, and `conditionalAnd`/`conditionalOr` are themselves
//! flat, ANTLR-list productions (`e+=relation (ops+='&&' e1+=relation)*`),
//! not self-referential the way `calc`/`relation`/`member` are. Both are
//! confirmed safe at 1300 and 500 terms respectively
//! (`tests/nesting_limit.rs`), well past anything
//! [`MAX_NESTING_DEPTH`] would ever allow through for the shapes that do
//! cost a frame per occurrence.
//!
//! [`max_nesting_depth`] tracks two numbers at once, added together for
//! the result: `bracket_depth`, simultaneous `(`/`[`/`{` nesting exactly
//! as before (one bracket closing undoes exactly the one level it
//! opened); and `chain`, a running count of binary arithmetic
//! (`+ - * / %`), relational/equality (`< <= > >= == !=`) and `in`
//! operators, member-access `.` (never a float literal's decimal
//! point), and an index's `[` (as opposed to a list-literal's own
//! opening `[`) -- which, unlike `bracket_depth`, is *not* undone when
//! a bracket it was raised inside closes: a chain link, once counted,
//! keeps costing a stack frame for whatever comes after it even across
//! a bracket boundary the *outer* chain continues past, because the
//! ANTLR visitor's own call stack genuinely does not unwind between an
//! outer chain link and an inner one it encloses (worked example,
//! `a_group_that_is_the_leftmost_operand_adds_to_the_outer_chain`
//! below). `chain` is only ever reset to zero at a `&&`, `||`, `,`, `?`
//! or `:` -- points where `cel`'s own flat-list/independent-branch
//! grammar means nothing stacks across them, so a long chain of
//! independent, shallow terms (many `&&`-joined comparisons, many list
//! elements) is not penalized for its length.
//!
//! A method call's `(` (`x.f(...)`) is deliberately never counted on
//! its own -- the grammar's `MemberCall` already folds `.`/id/`(`/`)`
//! into the one recursive link the leading `.` accounts for, so
//! counting the `(` too would double-count every link of a
//! `.f().f().f()...` chain. A *global* call's `(` (`f(...)`, no `.` to
//! have already counted it) still bumps `bracket_depth` like any other
//! bracket, because nested global calls (`f(f(f(...)))`) are a genuine,
//! un-flattened nesting risk `chain` does not otherwise see.
//!
//! This can both over- and undercount the true worst-case recursion
//! depth for a handful of tree shapes a single left-to-right pass
//! cannot represent exactly without becoming a real parser -- a
//! sibling chain that would already have returned before a later one
//! starts is overcounted (safe: rejects some expressions that were
//! actually fine); a chain-opening bracket's own primary-rule wrapper
//! is never billed its own frame, undercounting by exactly one in a
//! shape like `(1+1+1+1) + 1 + 1 + 1 + 1` (pinned by the same test).
//! Given [`MAX_NESTING_DEPTH`] (64) against the measured overflow
//! thresholds above (the lowest ~1300), an undercount of one is not a
//! safety concern -- but it means this scan's output is a close,
//! deliberately conservative proxy for recursion cost, not an exact
//! frame count, and should not be relied on for more precision than
//! that.
//!
//! Circuitry's own `evaluate_cel`/`evaluate_cel_expect`
//! (`core/cel_eval.py`) have no equivalent pre-scan: `celpy`'s parser
//! (`lark`) raises Python's own `RecursionError` well before a native
//! stack overflow, in principle a graceful, catchable failure for
//! exactly this input -- but `_compile`'s own `except Exception` handler
//! that would turn it into `CelValidationError` can itself re-trigger a
//! second `RecursionError` formatting the first one's message
//! (`str(exc)`, still deep in the same exhausted recursion budget),
//! which escapes **uncaught**, propagating past every call site in
//! `core/cel_eval.py` as a bare `RecursionError` rather than a
//! `CelEvaluationError` (confirmed directly: `evaluate_cel("0" + "+1" *
//! 2000, {})` raises `RecursionError`, not `CelEvaluationError`, both at
//! Python's default recursion limit and a raised one). `electricity-cel`
//! does not reproduce that failure mode -- there is no Rust exception to
//! catch a native stack overflow at all, so this pre-scan's job is to
//! never let the recursive parse happen, which, incidentally, also means
//! electricity rejects this input more cleanly than Circuitry's own
//! evaluator does today.

/// The deepest an expression's combined bracket-and-chain nesting
/// ([`max_nesting_depth`]) may go before it is rejected -- a fixed,
/// crate-specific number with a wide safety margin under both `cel`'s
/// own internal, undocumented parser recursion limit (observed at 96)
/// and the measured overflow thresholds above (the lowest of which,
/// ~1300, is still twenty times this limit).
///
/// Deliberately **not** [`electricity_value::MAX_DEPTH`] (512): that
/// constant bounds how deep a `Value` tree may nest, a data-structure
/// limit that has nothing to do with how many stack frames `cel`'s own
/// ANTLR-generated parser spends per level of *expression syntax* --
/// sharing one number between the two would either make this limit
/// unsafe (parsing 512 levels of nested parentheses, or a 512-term
/// arithmetic chain, overflows long before `cel` would ever report an
/// error) or make `electricity-value`'s limit uselessly small for data
/// that was never CEL source text to begin with.
pub const MAX_NESTING_DEPTH: usize = 64;

/// Scans *expr* for the deepest point its combined bracket-and-chain
/// nesting reaches -- see the module docs for exactly what counts, and
/// why. Skips over string/bytes literals (`'...'`, `"..."`,
/// `'''...'''`, `"""..."""`, any of the four optionally `b`/`B`-prefixed)
/// and `//` line comments, so a character that only ever appears as
/// string content or inside a comment is never counted; mirrors CEL's
/// own grammar (`parser/gen/CEL.g4`'s `STRING`/`BYTES`/`COMMENT` rules)
/// for that, not a general-purpose tokenizer -- this is a pre-scan, not
/// a second parser. The one known inexactness, inherited unchanged from
/// an earlier version of this scan: a raw string (`r"..."`/`R'...'`,
/// whose grammar rule gives backslash no escaping role at all) is still
/// scanned with the same "a backslash always escapes the next character"
/// rule every other string gets, which can only ever make this scan
/// treat *more* of the expression as string content than CEL's own
/// lexer would, never less -- so a pathological raw string immediately
/// before a run of real brackets/operators could cause this pre-scan to
/// undercount them and let the expression through uncaught by its own
/// depth check. That case does not need this pre-scan to be safe:
/// `cel`'s own internal recursion limit (the module docs above) still
/// catches it, gracefully, independent of whether this scan counted
/// correctly.
pub fn max_nesting_depth(expr: &str) -> usize {
    // Simultaneous bracket nesting -- `(`/`[`/`{` genuinely open at
    // once -- and the operator/accessor chain -- `+`/`<`/`.`/an index
    // `[` -- are tracked separately because they decay differently:
    // closing a bracket undoes exactly the one level it opened, but a
    // chain link, once counted, keeps costing a stack frame for
    // whatever comes after it even across a bracket boundary the outer
    // chain continues past (module docs' worked example) -- it is only
    // ever reset by `&&`/`||`/`,`/`?`/`:`, the tokens whose own grammar
    // (module docs) means nothing stacks across them.
    let mut bracket_depth: usize = 0;
    let mut chain: usize = 0;
    let mut max_depth: usize = 0;
    // Whether the token just scanned could be the left operand of a
    // binary operator, or the receiver of `.`/`[` -- an identifier,
    // keyword literal (`true`/`false`/`null`), number, string/bytes
    // literal, or a closing `)`/`]`/`}`.
    let mut prev_value_end = false;
    // Whether the token just scanned was an identifier (never a
    // literal, closing bracket, or keyword-operator like `in`) --
    // needed only to tell a method call's `.name(` (already counted via
    // `.`, so its `(` must not *also* bump `bracket_depth`) apart from a
    // global call's bare `name(` (which has no `.` to count it, and
    // nested global calls are a genuine, un-flattened nesting risk the
    // same way nested parens are, so its `(` must).
    let mut prev_was_identifier = false;
    // Whether the identifier just scanned was itself preceded by `.`.
    let mut prev_identifier_was_dotted = false;
    // Whether a `.` was just scanned, awaiting the identifier that
    // completes it.
    let mut after_dot = false;
    // Per opened bracket, whether closing it should decrement
    // `bracket_depth` -- false for an index's `[` and a method call's
    // `(`, both of which already went through `chain` instead (above).
    let mut bracket_kind: Vec<bool> = Vec::new();

    let chars: Vec<char> = expr.chars().collect();
    let n = chars.len();
    let mut i = 0;

    macro_rules! update_max {
        () => {
            let total = bracket_depth + chain;
            if total > max_depth {
                max_depth = total;
            }
        };
    }

    while i < n {
        let c = chars[i];
        match c {
            '\'' | '"' => {
                i = skip_string_from(&chars, i);
                prev_value_end = true;
                prev_was_identifier = false;
                after_dot = false;
                continue;
            }
            '/' if i + 1 < n && chars[i + 1] == '/' => {
                while i < n && chars[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            '0'..='9' => {
                // Consume the whole numeric literal (digits, at most one
                // embedded `.` immediately followed by a digit, and an
                // optional exponent) as one token, so a later standalone
                // `.` is unambiguously a member-access dot, never a
                // decimal point this loop already passed over.
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
                prev_was_identifier = false;
                after_dot = false;
                continue;
            }
            c if c.is_alphabetic() || c == '_' => {
                let start = i;
                i += 1;
                while i < n && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                let word: String = chars[start..i].iter().collect();
                if word == "in" {
                    chain += 1;
                    update_max!();
                    prev_value_end = false;
                    prev_was_identifier = false;
                } else {
                    // Any other identifier or keyword literal
                    // (`true`/`false`/`null`) is a value.
                    prev_value_end = true;
                    prev_identifier_was_dotted = after_dot;
                    prev_was_identifier = true;
                }
                after_dot = false;
                continue;
            }
            '[' if prev_value_end => {
                // Index continuation (`x[i]`), not a list literal's own
                // opening bracket: counted via `chain`, the same as a
                // `.`, so a sequential `x[0][0][0]...` chain (brackets
                // never simultaneously open) still costs one level per
                // link -- `bracket_depth` alone (simultaneous nesting
                // only) would never see it (module docs).
                chain += 1;
                update_max!();
                bracket_kind.push(false);
                prev_value_end = false;
                prev_was_identifier = false;
            }
            '(' if prev_was_identifier && prev_identifier_was_dotted => {
                // A method call's `(` (`x.f(...)`) -- its link was
                // already counted when `.` was scanned; counting this
                // `(` too would double-count every link of a
                // `.f().f().f()...` chain.
                bracket_kind.push(false);
                prev_value_end = false;
                prev_was_identifier = false;
            }
            '(' | '[' | '{' => {
                // Grouping `(...)`, a list/map literal's own brackets,
                // or a *global* call's `(` (`f(...)`, with no `.` to
                // have already counted it) -- `f(f(f(...)))` nests
                // these for real, so unlike a method call's `(`, this
                // one must bump `bracket_depth` to catch it.
                bracket_depth += 1;
                update_max!();
                bracket_kind.push(true);
                prev_value_end = false;
                prev_was_identifier = false;
            }
            ')' | ']' | '}' => {
                if bracket_kind.pop() == Some(true) {
                    bracket_depth = bracket_depth.saturating_sub(1);
                }
                prev_value_end = true;
                prev_was_identifier = false;
            }
            '&' if i + 1 < n && chars[i + 1] == '&' => {
                i += 2;
                chain = 0;
                prev_value_end = false;
                prev_was_identifier = false;
                continue;
            }
            '|' if i + 1 < n && chars[i + 1] == '|' => {
                i += 2;
                chain = 0;
                prev_value_end = false;
                prev_was_identifier = false;
                continue;
            }
            ',' | '?' | ':' => {
                chain = 0;
                prev_value_end = false;
                prev_was_identifier = false;
            }
            '<' | '>' | '=' | '!' => {
                // `<=`/`>=`/`==`/`!=` are two-char; a bare `=` or `!`
                // alone (`!` is unary-only in CEL, never counted) falls
                // through without a bump.
                let two_char = i + 1 < n && chars[i + 1] == '=';
                if c == '<' || c == '>' || (c == '=' && two_char) || (c == '!' && two_char) {
                    chain += 1;
                    update_max!();
                }
                if two_char {
                    i += 1;
                }
                prev_value_end = false;
                prev_was_identifier = false;
            }
            '+' | '-' => {
                // Binary only when it follows a value; a leading or
                // post-operator `+`/`-` is unary, collapsed by the
                // grammar's own `ops` run the same as `!` (module docs).
                if prev_value_end {
                    chain += 1;
                    update_max!();
                }
                prev_value_end = false;
                prev_was_identifier = false;
            }
            '*' | '%' => {
                chain += 1;
                update_max!();
                prev_value_end = false;
                prev_was_identifier = false;
            }
            '.' => {
                // Not a float literal's decimal point: the numeric
                // branch above already consumed any `.` directly
                // between two digit runs. A standalone `.` is always a
                // member-access dot.
                chain += 1;
                update_max!();
                prev_value_end = false;
                prev_was_identifier = false;
                after_dot = true;
                i += 1;
                continue;
            }
            _ => {
                prev_was_identifier = false;
            }
        }
        i += 1;
    }
    max_depth
}

/// Index of the character one past the end of the string/bytes literal
/// that starts at `chars[start]` (`chars[start]` is the opening quote) --
/// a plain single-character string, or, if the next two characters are
/// also the same quote, a triple-quoted one.
fn skip_string_from(chars: &[char], start: usize) -> usize {
    let quote = chars[start];
    let n = chars.len();
    let is_triple = start + 2 < n && chars[start + 1] == quote && chars[start + 2] == quote;
    if is_triple {
        let mut i = start + 3;
        let mut run = 0u8;
        while i < n {
            if chars[i] == '\\' {
                i += 2;
                run = 0;
                continue;
            }
            if chars[i] == quote {
                run += 1;
                i += 1;
                if run == 3 {
                    return i;
                }
            } else {
                run = 0;
                i += 1;
            }
        }
        i
    } else {
        let mut i = start + 1;
        while i < n {
            if chars[i] == '\\' {
                i += 2;
                continue;
            }
            if chars[i] == quote || chars[i] == '\n' {
                return i + 1;
            }
            i += 1;
        }
        i
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_has_zero_depth() {
        assert_eq!(max_nesting_depth("1 + 1"), 1);
    }

    #[test]
    fn literal_has_zero_depth() {
        assert_eq!(max_nesting_depth("1"), 0);
    }

    #[test]
    fn counts_simultaneous_bracket_nesting_not_total_brackets() {
        assert_eq!(max_nesting_depth("(1) + (2) + (3)"), 3);
        assert_eq!(max_nesting_depth("((()))"), 3);
    }

    #[test]
    fn counts_every_bracket_kind_together() {
        assert_eq!(max_nesting_depth("[{(1)}]"), 3);
    }

    #[test]
    fn ignores_brackets_and_operators_inside_a_single_quoted_string() {
        assert_eq!(max_nesting_depth("'((((( + + +' == x"), 1);
    }

    #[test]
    fn ignores_brackets_inside_a_double_quoted_string() {
        assert_eq!(max_nesting_depth("\"[[[[[\" == x"), 1);
    }

    #[test]
    fn ignores_brackets_inside_a_triple_quoted_string() {
        assert_eq!(max_nesting_depth("'''(((((''' == x"), 1);
    }

    #[test]
    fn ignores_brackets_inside_a_bytes_literal() {
        assert_eq!(max_nesting_depth("b'[[[[['  == x"), 1);
    }

    #[test]
    fn an_escaped_quote_does_not_end_the_string_early() {
        assert_eq!(max_nesting_depth(r"'a\'(((' == x"), 1);
    }

    #[test]
    fn ignores_brackets_inside_a_line_comment() {
        assert_eq!(max_nesting_depth("1 // (((((\n+ 1"), 1);
    }

    #[test]
    fn real_nesting_after_a_string_is_still_counted() {
        assert_eq!(max_nesting_depth("'(((' == x || (1 + (2))"), 3);
    }

    #[test]
    fn exactly_at_the_limit_and_one_past_it_for_brackets() {
        let expr = format!(
            "{}1{}",
            "(".repeat(MAX_NESTING_DEPTH),
            ")".repeat(MAX_NESTING_DEPTH)
        );
        assert_eq!(max_nesting_depth(&expr), MAX_NESTING_DEPTH);
        let expr = format!(
            "{}1{}",
            "(".repeat(MAX_NESTING_DEPTH + 1),
            ")".repeat(MAX_NESTING_DEPTH + 1)
        );
        assert_eq!(max_nesting_depth(&expr), MAX_NESTING_DEPTH + 1);
    }

    #[test]
    fn exactly_at_the_limit_and_one_past_it_for_an_arithmetic_chain() {
        let expr = format!("0{}", "+1".repeat(MAX_NESTING_DEPTH));
        assert_eq!(max_nesting_depth(&expr), MAX_NESTING_DEPTH);
        let expr = format!("0{}", "+1".repeat(MAX_NESTING_DEPTH + 1));
        assert_eq!(max_nesting_depth(&expr), MAX_NESTING_DEPTH + 1);
    }

    #[test]
    fn exactly_at_the_limit_and_one_past_it_for_a_relation_chain() {
        let expr = format!("0{}", "<1".repeat(MAX_NESTING_DEPTH));
        assert_eq!(max_nesting_depth(&expr), MAX_NESTING_DEPTH);
        let expr = format!("0{}", "<1".repeat(MAX_NESTING_DEPTH + 1));
        assert_eq!(max_nesting_depth(&expr), MAX_NESTING_DEPTH + 1);
    }

    #[test]
    fn exactly_at_the_limit_and_one_past_it_for_a_select_chain() {
        let expr = format!("state{}", ".a".repeat(MAX_NESTING_DEPTH));
        assert_eq!(max_nesting_depth(&expr), MAX_NESTING_DEPTH);
        let expr = format!("state{}", ".a".repeat(MAX_NESTING_DEPTH + 1));
        assert_eq!(max_nesting_depth(&expr), MAX_NESTING_DEPTH + 1);
    }

    #[test]
    fn exactly_at_the_limit_and_one_past_it_for_an_index_chain() {
        let expr = format!("state{}", "[0]".repeat(MAX_NESTING_DEPTH));
        assert_eq!(max_nesting_depth(&expr), MAX_NESTING_DEPTH);
        let expr = format!("state{}", "[0]".repeat(MAX_NESTING_DEPTH + 1));
        assert_eq!(max_nesting_depth(&expr), MAX_NESTING_DEPTH + 1);
    }

    #[test]
    fn a_long_chain_of_unary_not_is_not_bounded_by_nesting_depth() {
        let expr = format!("{}true", "!".repeat(1300));
        assert_eq!(max_nesting_depth(&expr), 0);
    }

    #[test]
    fn a_long_and_chain_is_not_bounded_by_nesting_depth() {
        let expr = (0..500).map(|_| "true").collect::<Vec<_>>().join(" && ");
        assert_eq!(max_nesting_depth(&expr), 0);
    }

    #[test]
    fn a_long_or_chain_is_not_bounded_by_nesting_depth() {
        let expr = (0..500).map(|_| "true").collect::<Vec<_>>().join(" || ");
        assert_eq!(max_nesting_depth(&expr), 0);
    }

    #[test]
    fn and_resets_the_chain_so_many_independent_comparisons_are_safe() {
        // A realistic wide condition -- many independent `a.b == N`
        // comparisons ANDed together -- must not be rejected just
        // because there happen to be more than `MAX_NESTING_DEPTH` of
        // them: each is only one `.` and one `==` deep, and `&&` never
        // stacks a parser frame (module docs).
        let expr = (0..100)
            .map(|i| format!("state.a == {i}"))
            .collect::<Vec<_>>()
            .join(" && ");
        assert_eq!(max_nesting_depth(&expr), 2);
    }

    #[test]
    fn comma_resets_the_chain_for_list_and_call_arguments() {
        let expr = (0..100)
            .map(|i| format!("{i}.5"))
            .collect::<Vec<_>>()
            .join(", ");
        assert_eq!(max_nesting_depth(&format!("[{expr}]")), 1);
    }

    #[test]
    fn float_literals_are_not_mistaken_for_member_access() {
        assert_eq!(max_nesting_depth("1.5 + 2.5 + 3.5"), 2);
    }

    #[test]
    fn method_call_chain_counts_once_per_link() {
        let expr = format!("state{}", ".f()".repeat(MAX_NESTING_DEPTH));
        assert_eq!(max_nesting_depth(&expr), MAX_NESTING_DEPTH);
        let expr = format!("state{}", ".f()".repeat(MAX_NESTING_DEPTH + 1));
        assert_eq!(max_nesting_depth(&expr), MAX_NESTING_DEPTH + 1);
    }

    #[test]
    fn a_group_that_is_the_leftmost_operand_adds_to_the_outer_chain() {
        // `(1+1+1+1) + 1 + 1 + 1 + 1`: the group's own internal chain
        // (3 links) and the outer chain that continues past it (4
        // links) are both genuinely on the call stack at once when the
        // ANTLR visitor's left-recursive descent reaches the group --
        // `chain` must carry the 3 forward past the closing `)` and add
        // the outer 4 on top (7), not reset to whichever is larger. The
        // true frame count is one more (8: `chain` doesn't bill the
        // group's own primary-rule wrapper as a separate frame) -- an
        // undercount, pinned here at exactly one so a future change
        // can't silently widen it: acceptable only because the margin
        // to a real overflow (~700 unbracketed terms, module docs) is
        // many multiples of [`MAX_NESTING_DEPTH`] (64), not because
        // undercounting is ever the safe direction in general.
        let expr = "(1+1+1+1) + 1 + 1 + 1 + 1";
        assert_eq!(max_nesting_depth(expr), 3 + 4);
    }
}
