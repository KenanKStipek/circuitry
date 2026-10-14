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
//! before doing anything else, exactly the way a bracket does.
//!
//! This pre-scan tracks two independent numbers rather than one, because
//! they fail differently on both sides of the Rust/Python divide:
//!
//! **Bracket nesting** -- simultaneous `(`/`[`/`{` nesting (grouping
//! parentheses, a list/map literal's own brackets, a *global* function
//! call's `(`) -- is, on the `cel` crate's side, caught by the crate's
//! own hard-coded recursion guard before it can ever threaten a stack
//! overflow: `Parser`/`PrattParser`'s `max_recursion_depth` defaults to
//! **96** (`cel-0.15.0/src/parser/parser.rs`, `src/parser/pratt_parser.rs`),
//! confirmed directly (this module's own tests): a bare `cel::Env`
//! (default config, nothing from this crate involved) accepts up to 95
//! levels of nested parentheses and rejects, gracefully, every depth
//! from 96 up to at least 50,000 -- never a crash, in an unoptimized
//! debug build or `--release` alike. This crate's own bracket cap below
//! exists only to match Circuitry's own, *much* tighter boundary
//! (measured directly: see below), not because `cel`'s own guard needs
//! help to stay safe.
//!
//! **Chain nesting** -- a running count of binary arithmetic
//! (`+ - * / %`), relational/equality (`< <= > >= == !=`) and `in`
//! operators, member-access `.` (never a float literal's decimal
//! point), an index's `[` (as opposed to a list-literal's own opening
//! `[`), and a method call's leading `.` -- has no such guard: `cel`'s
//! own parser recurses once per link with nothing to stop it short of
//! the real call stack. Measured directly on a 2 MiB thread stack (the
//! default a spawned thread gets, where the runner does most of this
//! work), within [`crate::MAX_EXPR_LENGTH`] (4096 characters):
//!
//! - `0+1+1+...` (an arithmetic chain) and `state[0][0][0]...` (a
//!   sequential index chain, never two brackets open at once) both
//!   overflowed between 1,620 and 1,640 terms, in both an unoptimized
//!   debug build and `--release`;
//! - `state.a.a.a...` (a sequential field-select chain), `0<1<2<...`
//!   (a sequential relation chain) and `state.size().size()...` (a
//!   sequential method-call chain) did not overflow even at the
//!   deepest each can reach under the 4096-character cap (about 2045,
//!   1080 and 1020 terms respectively) -- a coincidence of today's
//!   stack-frame sizes against today's character cap, not a structural
//!   difference, so this crate's own chain cap below treats every one
//!   of them the same way `bracket_depth`/`chain` already did before
//!   this module split the two.
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
//! (`tests/nesting_limit.rs`), well past either cap below.
//!
//! ## Matching Circuitry's own boundary
//!
//! Circuitry's own evaluator (`cel-python`, through `evaluate_cel`) has
//! no pre-scan at all: a malformed or over-deep expression either parses
//! (`lark`) and evaluates, or fails somewhere inside Python's own
//! recursion machinery. For the bracket shapes (parentheses, list and
//! map literals), that failure is Python's `RecursionError`, wrapped as
//! `CelEvaluationError` -- measured directly, through `evaluate_cel`, in
//! three call contexts that differ in how much of Python's recursion
//! budget is already spent by the time `evaluate_cel` is reached from a
//! real run (a top-level `if`, a CEL `while` loop condition, and an `if`
//! inside a `use` child's own orchestration): **48 levels is the deepest
//! every context accepted** (parentheses alone reach 53 in all three;
//! list and map literals, the tighter of the shapes measured, reach only
//! 48) -- consistent across all three contexts at the granularity
//! measured, because the three real call-stack depths involved (roughly
//! 8-12 Python frames apart) are small next to Python's default
//! recursion limit (1000).
//!
//! For the chain shapes, the same three-context measurement gives
//! **242 terms** as the deepest every context accepted for an
//! arithmetic (`+`) or relational (`<`) chain (identical in all three
//! contexts at this granularity), and **615 terms** for a field-select
//! chain (`state.a.a...`) -- one term *tighter* in the deepest context
//! (inside a `use` child) than the other two, which both still accepted
//! 616. [`MAX_CHAIN_NESTING_DEPTH`] below is shared by every chain shape
//! (arithmetic, relational, select, index, method-call alike -- the
//! same single running `chain` count this module always used), so it is
//! set to the *tightest* of these: 242, safely under even the
//! select-chain's own narrower 615-term floor, not just the arithmetic/
//! relational one it was measured against directly.
//!
//! Beyond these accepted depths, Circuitry's own boundary is not a
//! stable target to begin with: a few terms further (barely outside the
//! range this crate's own corpus pins), the *same* expression evaluated
//! in two different call contexts was observed to differ by whether it
//! fails cleanly (`CelEvaluationError`, Python's `except Exception`
//! catching the `RecursionError` and formatting it successfully) or
//! escapes as a **bare** `RecursionError` (the formatting itself
//! re-triggering recursion past the limit, which escapes every
//! `except CelError`/`except Exception` it passes through uncaught --
//! `core/cel_eval.py`'s own `_compile`/`evaluate_cel` do not guard
//! against this). That transition point moves with how deep the
//! surrounding call already is, which this crate cannot know or
//! reproduce; this is why the caps below sit at the smallest depth
//! *every measured context still accepted cleanly*, not at some single,
//! precise "Circuitry's limit is exactly N" figure the deeper tiers
//! don't actually have. The corpus (`golden_corpus.rs`) accordingly only
//! pins depths clearly inside the accepted range and clearly past both
//! caps -- never a depth inside the range the three contexts disagreed
//! on even a single term apart.
//!
//! [`max_nesting_depths`] tracks two numbers at once, returned
//! separately (not combined the way an earlier version of this module
//! summed them into one `MAX_NESTING_DEPTH`): `bracket_depth`,
//! simultaneous `(`/`[`/`{` nesting exactly as before (one bracket
//! closing undoes exactly the one level it opened); and `chain`, a
//! running count of binary arithmetic (`+ - * / %`), relational/
//! equality (`< <= > >= == !=`) and `in` operators, member-access `.`
//! (never a float literal's decimal point), and an index's `[` (as
//! opposed to a list-literal's own opening `[`) -- which, unlike
//! `bracket_depth`, is *not* undone when a bracket it was raised inside
//! closes: a chain link, once counted, keeps costing a stack frame for
//! whatever comes after it even across a bracket boundary the *outer*
//! chain continues past, because the ANTLR visitor's own call stack
//! genuinely does not unwind between an outer chain link and an inner
//! one it encloses (worked example,
//! `a_group_that_is_the_leftmost_operand_adds_to_the_outer_chain`
//! below). `chain` is only ever reset to zero at a `&&`, `||`, `,`, `?`
//! or `:` -- points where `cel`'s own flat-list/independent-branch
//! grammar means nothing stacks across them, so a long chain of
//! independent, shallow terms (many `&&`-joined comparisons, many list
//! elements) is not penalized for its length.
//!
//! Checking the two numbers independently against their own caps,
//! rather than their sum against one combined cap, is deliberately
//! *not* a safety argument for a *mixed* shape (bracket nesting wrapped
//! around a long chain, or the reverse): a combined, bracket-and-chain
//! expression can fail Circuitry's own evaluator well under the sum of
//! the two caps below (confirmed directly: 48 nested parentheses
//! wrapped around a 242-term arithmetic chain inside already raises
//! `RecursionError` in Circuitry, where 48 parentheses alone and 242
//! chain terms alone both succeed) -- Python's recursion budget is one
//! shared resource regardless of which grammar rule is consuming it,
//! the same way this crate's own combined total used to model it.
//! Checking independently is safe for *this* crate's own stack only
//! because the margins are generous enough that it doesn't matter: a
//! bracket-cap-deep expression (at most 95, `cel`'s own guard) wrapped
//! around a chain-cap-deep one (242) is still only a few hundred stack
//! frames, nowhere near the lowest measured overflow floor above
//! (1,620). A mixed expression this crate's own pre-scan accepts but
//! Circuitry's evaluator would reject is therefore a known, accepted
//! residual divergence (this crate being *more* lenient, never less, on
//! a shape this far outside anything an ordinary orchestration writes)
//! rather than a gap either side needs to close.
//!
//! A method call's `(` (`x.f(...)`) is deliberately never counted on
//! its own -- the grammar's `MemberCall` already folds `.`/id/`(`/`)`
//! into the one recursive link the leading `.` accounts for, so
//! counting the `(` too would double-count every link of a
//! `.f().f().f()...` chain. A *global* call's `(` (`f(...)`, no `.` to
//! have already counted it) still bumps `bracket_depth` like any other
//! bracket, because nested global calls (`f(f(f(...)))`) are a genuine,
//! un-flattened nesting risk `chain` does not otherwise see, and because
//! `cel`'s own recursion guard treats it the same way it treats a
//! parenthesized group.
//!
//! This can both over- and undercount the true worst-case recursion
//! depth for a handful of tree shapes a single left-to-right pass
//! cannot represent exactly without becoming a real parser -- a
//! sibling chain that would already have returned before a later one
//! starts is overcounted (safe: rejects some expressions that were
//! actually fine); a chain-opening bracket's own primary-rule wrapper
//! is never billed its own frame, undercounting by exactly one in a
//! shape like `(1+1+1+1) + 1 + 1 + 1 + 1` (pinned by the same test).
//! An undercount of one is not a safety concern against either cap
//! below -- but it means this scan's output is a close, deliberately
//! conservative proxy for recursion cost, not an exact frame count, and
//! should not be relied on for more precision than that.

/// The deepest simultaneous `(`/`[`/`{` nesting ([`max_nesting_depths`]'s
/// `bracket`) an expression may reach before it is rejected -- at or
/// below the smallest depth Circuitry's own evaluator accepted in every
/// of the three call contexts measured (module docs): 48, the tighter of
/// list/map literals' own floor (parentheses alone reach 53). Far under
/// the `cel` crate's own hard-coded parser recursion guard (96, module
/// docs) -- that guard, not this cap, is what actually keeps bracket
/// nesting from ever threatening a stack overflow in this crate; this
/// cap exists purely to match Circuitry's own, much tighter boundary.
pub const MAX_BRACKET_NESTING_DEPTH: usize = 48;

/// The deepest operator/accessor chain ([`max_nesting_depths`]'s
/// `chain`) an expression may reach before it is rejected -- shared by
/// every chain shape (arithmetic, relational, member-select, index,
/// method-call), at or below the tightest of the per-shape floors
/// Circuitry's own evaluator accepted in every of the three call
/// contexts measured (module docs): 242, matching the arithmetic/
/// relational chains' own floor exactly and safely under the
/// select-chain's own, looser 615-term floor. A wide safety margin
/// under the lowest measured point this crate's own parser and
/// evaluator overflow a 2 MiB stack at all (~1,620 terms, module docs,
/// roughly 6.7x this cap).
pub const MAX_CHAIN_NESTING_DEPTH: usize = 242;

/// [`max_nesting_depths`]'s own result -- the two numbers tracked
/// independently rather than summed (module docs explain why summing
/// them was dropped).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NestingDepths {
    /// Deepest simultaneous `(`/`[`/`{` nesting reached.
    pub bracket: usize,
    /// Deepest operator/accessor chain reached.
    pub chain: usize,
}

/// Scans *expr* for the deepest point its bracket nesting and its
/// operator/accessor chain each reach -- see the module docs for
/// exactly what each counts, and why they're tracked separately.
/// Skips over string/bytes literals (`'...'`, `"..."`,
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
/// `cel`'s own internal recursion guard (bracket shapes) or the
/// measured overflow margin (chain shapes, module docs) still catches
/// it, independent of whether this scan counted correctly.
pub fn max_nesting_depths(expr: &str) -> NestingDepths {
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
    let mut max_bracket: usize = 0;
    let mut max_chain: usize = 0;
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

    macro_rules! update_bracket_max {
        () => {
            if bracket_depth > max_bracket {
                max_bracket = bracket_depth;
            }
        };
    }
    macro_rules! update_chain_max {
        () => {
            if chain > max_chain {
                max_chain = chain;
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
                    update_chain_max!();
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
                update_chain_max!();
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
                update_bracket_max!();
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
                    update_chain_max!();
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
                    update_chain_max!();
                }
                prev_value_end = false;
                prev_was_identifier = false;
            }
            '*' | '%' => {
                chain += 1;
                update_chain_max!();
                prev_value_end = false;
                prev_was_identifier = false;
            }
            '.' => {
                // Not a float literal's decimal point: the numeric
                // branch above already consumed any `.` directly
                // between two digit runs. A standalone `.` is always a
                // member-access dot.
                chain += 1;
                update_chain_max!();
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
    NestingDepths {
        bracket: max_bracket,
        chain: max_chain,
    }
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

    fn bracket(expr: &str) -> usize {
        max_nesting_depths(expr).bracket
    }
    fn chain(expr: &str) -> usize {
        max_nesting_depths(expr).chain
    }

    #[test]
    fn scalar_has_zero_depth() {
        let d = max_nesting_depths("1 + 1");
        assert_eq!(
            d,
            NestingDepths {
                bracket: 0,
                chain: 1
            }
        );
    }

    #[test]
    fn literal_has_zero_depth() {
        assert_eq!(max_nesting_depths("1"), NestingDepths::default());
    }

    #[test]
    fn counts_simultaneous_bracket_nesting_not_total_brackets() {
        assert_eq!(bracket("(1) + (2) + (3)"), 1);
        assert_eq!(bracket("((()))"), 3);
    }

    #[test]
    fn counts_every_bracket_kind_together() {
        assert_eq!(bracket("[{(1)}]"), 3);
    }

    #[test]
    fn ignores_brackets_and_operators_inside_a_single_quoted_string() {
        assert_eq!(chain("'((((( + + +' == x"), 1);
        assert_eq!(bracket("'((((( + + +' == x"), 0);
    }

    #[test]
    fn ignores_brackets_inside_a_double_quoted_string() {
        assert_eq!(chain("\"[[[[[\" == x"), 1);
    }

    #[test]
    fn ignores_brackets_inside_a_triple_quoted_string() {
        assert_eq!(chain("'''(((((''' == x"), 1);
    }

    #[test]
    fn ignores_brackets_inside_a_bytes_literal() {
        assert_eq!(chain("b'[[[[['  == x"), 1);
    }

    #[test]
    fn an_escaped_quote_does_not_end_the_string_early() {
        assert_eq!(chain(r"'a\'(((' == x"), 1);
    }

    #[test]
    fn ignores_brackets_inside_a_line_comment() {
        assert_eq!(chain("1 // (((((\n+ 1"), 1);
    }

    #[test]
    fn real_nesting_after_a_string_is_still_counted() {
        let d = max_nesting_depths("'(((' == x || (1 + (2))");
        assert_eq!(
            d,
            NestingDepths {
                bracket: 2,
                chain: 1
            }
        );
    }

    #[test]
    fn exactly_at_the_limit_and_one_past_it_for_brackets() {
        let expr = format!(
            "{}1{}",
            "(".repeat(MAX_BRACKET_NESTING_DEPTH),
            ")".repeat(MAX_BRACKET_NESTING_DEPTH)
        );
        assert_eq!(bracket(&expr), MAX_BRACKET_NESTING_DEPTH);
        let expr = format!(
            "{}1{}",
            "(".repeat(MAX_BRACKET_NESTING_DEPTH + 1),
            ")".repeat(MAX_BRACKET_NESTING_DEPTH + 1)
        );
        assert_eq!(bracket(&expr), MAX_BRACKET_NESTING_DEPTH + 1);
    }

    #[test]
    fn exactly_at_the_limit_and_one_past_it_for_an_arithmetic_chain() {
        let expr = format!("0{}", "+1".repeat(MAX_CHAIN_NESTING_DEPTH));
        assert_eq!(chain(&expr), MAX_CHAIN_NESTING_DEPTH);
        let expr = format!("0{}", "+1".repeat(MAX_CHAIN_NESTING_DEPTH + 1));
        assert_eq!(chain(&expr), MAX_CHAIN_NESTING_DEPTH + 1);
    }

    #[test]
    fn exactly_at_the_limit_and_one_past_it_for_a_relation_chain() {
        let expr = format!("0{}", "<1".repeat(MAX_CHAIN_NESTING_DEPTH));
        assert_eq!(chain(&expr), MAX_CHAIN_NESTING_DEPTH);
        let expr = format!("0{}", "<1".repeat(MAX_CHAIN_NESTING_DEPTH + 1));
        assert_eq!(chain(&expr), MAX_CHAIN_NESTING_DEPTH + 1);
    }

    #[test]
    fn exactly_at_the_limit_and_one_past_it_for_a_select_chain() {
        let expr = format!("state{}", ".a".repeat(MAX_CHAIN_NESTING_DEPTH));
        assert_eq!(chain(&expr), MAX_CHAIN_NESTING_DEPTH);
        let expr = format!("state{}", ".a".repeat(MAX_CHAIN_NESTING_DEPTH + 1));
        assert_eq!(chain(&expr), MAX_CHAIN_NESTING_DEPTH + 1);
    }

    #[test]
    fn exactly_at_the_limit_and_one_past_it_for_an_index_chain() {
        let expr = format!("state{}", "[0]".repeat(MAX_CHAIN_NESTING_DEPTH));
        assert_eq!(chain(&expr), MAX_CHAIN_NESTING_DEPTH);
        let expr = format!("state{}", "[0]".repeat(MAX_CHAIN_NESTING_DEPTH + 1));
        assert_eq!(chain(&expr), MAX_CHAIN_NESTING_DEPTH + 1);
    }

    #[test]
    fn a_long_chain_of_unary_not_is_not_bounded_by_nesting_depth() {
        let expr = format!("{}true", "!".repeat(1300));
        assert_eq!(max_nesting_depths(&expr), NestingDepths::default());
    }

    #[test]
    fn a_long_and_chain_is_not_bounded_by_nesting_depth() {
        let expr = (0..500).map(|_| "true").collect::<Vec<_>>().join(" && ");
        assert_eq!(max_nesting_depths(&expr), NestingDepths::default());
    }

    #[test]
    fn a_long_or_chain_is_not_bounded_by_nesting_depth() {
        let expr = (0..500).map(|_| "true").collect::<Vec<_>>().join(" || ");
        assert_eq!(max_nesting_depths(&expr), NestingDepths::default());
    }

    #[test]
    fn and_resets_the_chain_so_many_independent_comparisons_are_safe() {
        // A realistic wide condition -- many independent `a.b == N`
        // comparisons ANDed together -- must not be rejected just
        // because there happen to be more than `MAX_CHAIN_NESTING_DEPTH`
        // of them: each is only one `.` and one `==` deep, and `&&`
        // never stacks a parser frame (module docs).
        let expr = (0..100)
            .map(|i| format!("state.a == {i}"))
            .collect::<Vec<_>>()
            .join(" && ");
        assert_eq!(chain(&expr), 2);
    }

    #[test]
    fn comma_resets_the_chain_for_list_and_call_arguments() {
        let expr = (0..100)
            .map(|i| format!("{i}.5"))
            .collect::<Vec<_>>()
            .join(", ");
        assert_eq!(chain(&format!("[{expr}]")), 0);
        assert_eq!(bracket(&format!("[{expr}]")), 1);
    }

    #[test]
    fn float_literals_are_not_mistaken_for_member_access() {
        assert_eq!(chain("1.5 + 2.5 + 3.5"), 2);
    }

    #[test]
    fn method_call_chain_counts_once_per_link() {
        let expr = format!("state{}", ".f()".repeat(MAX_CHAIN_NESTING_DEPTH));
        assert_eq!(chain(&expr), MAX_CHAIN_NESTING_DEPTH);
        let expr = format!("state{}", ".f()".repeat(MAX_CHAIN_NESTING_DEPTH + 1));
        assert_eq!(chain(&expr), MAX_CHAIN_NESTING_DEPTH + 1);
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
        // many multiples of either cap, not because undercounting is
        // ever the safe direction in general. `bracket` peaks at 1
        // (only the one group is ever open at once).
        let expr = "(1+1+1+1) + 1 + 1 + 1 + 1";
        assert_eq!(
            max_nesting_depths(expr),
            NestingDepths {
                bracket: 1,
                chain: 3 + 4
            }
        );
    }
}
