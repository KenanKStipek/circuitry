//! A cheap pre-scan of an expression's nesting, run before handing it to
//! the `cel` crate's own ANTLR-generated parser.
//!
//! `cel`'s own `Env::compile` always uses that parser (`Env::parser`,
//! `cel-0.15.0/src/env.rs`), not the crate's separate, feature-gated
//! `PrattParser` (`parser_pratt`, off by default and not enabled by this
//! crate's `Cargo.toml`). The ANTLR visitor recurses once per level for
//! every binary operator chain (`calc`/`relation` in the grammar:
//! arithmetic, comparison, equality, `in`) and every postfix member/
//! index/call link (`member`'s own left-recursive alternatives: `.field`,
//! `[i]`, `.method(...)`) -- `cel-0.15.0/src/parser/parser.rs`'s
//! `visit_calc`/`visit_relation`/`visit_Select`/`visit_Index`/
//! `visit_MemberCall`, each calling `self.visit(lhs/member/target)`
//! before doing anything else, exactly the way a bracket does. Grouping
//! parentheses, a list/map literal's own brackets, and a global
//! function call's `(` recurse the same way for nested brackets.
//!
//! Chained unary `!`/`-` and a long `&&`/`||` chain cost nothing on this
//! side: the grammar collects every `!`/`-` in a run into one node's
//! `ops: Vec` (`unary`'s `LogicalNot`/`Negate` alternatives), visited
//! once regardless of the run's length, and `conditionalAnd`/
//! `conditionalOr` are themselves flat, ANTLR-list productions
//! (`e+=relation (ops+='&&' e1+=relation)*`), not self-referential the
//! way `calc`/`relation`/`member` are.
//!
//! Circuitry's own evaluator (`cel-python`, through `evaluate_cel`) has a
//! *different* recursion shape per operator family, measured directly by
//! running `cof run` (this worktree's own build, a temporary `HOME`) on a
//! minimal orchestration per shape, in three contexts that differ in how
//! much of Python's recursion budget is already spent by the time the
//! expression is reached: a top-level `if`, a `while` loop condition, and
//! an `if` inside a `use` child's own orchestration (chosen as the
//! deepest realistic nesting a document can put a CEL expression under).
//! Each context's `then`/`body` is empty, so nothing ever needs a real
//! adapter call and `--dry-run` is never used (a `use` effect skips its
//! child's own effects entirely under `--dry-run`, so that flag would
//! have silently measured nothing for the deepest context).
//!
//! Every shape measured this way turns out to recurse once per link
//! regardless of which of these three families it is in grammar terms,
//! unlike the Rust side above -- `&&`/`||`/unary `!`/`-` chains fail in
//! Circuitry too, just at a much larger N (below) than a bracket or
//! binary-operator chain. This module tracks five numbers rather than
//! one, matching the five boundaries measured (below), not the two or
//! three the grammar alone would suggest:
//!
//! - **`paren_call`** -- simultaneous grouping-`(`/global-call-`(`
//!   nesting (never a method call's own `(`, which [`MAX_CHAIN_NESTING_DEPTH`]`-family
//!   accounting handles instead -- see `accessor` below).
//! - **`list_map`** -- simultaneous list-literal-`[`/map-literal-`{`
//!   nesting (never an index's own `[`, which `chain` handles instead).
//! - **`chain`** -- a running count of binary arithmetic (`+ - * / %`),
//!   relational/equality (`< <= > >= == !=`) and `in` operators, and an
//!   index's `[` (as opposed to a list literal's own opening `[`).
//! - **`logical_unary`** -- a running count of `&&`/`||` operators and
//!   individual prefix `!`/`-` occurrences (one per character, not one
//!   per run -- each costs Circuitry its own stack frame even though
//!   `cel`'s own grammar collapses a whole run into one visit).
//! - **`accessor`** -- a running count of member-access `.` links
//!   (never a float literal's decimal point) and method-call links
//!   (`.method(...)`, whose own `(` is not counted separately -- see
//!   below).
//!
//! `chain` and `accessor` are only ever reset to zero at a `&&`, `||`,
//! `,`, `?` or `:` (points where nothing stacks across them, in both
//! `cel`'s own grammar and Circuitry's), and whenever the *other* one is
//! incremented (an operator or index link breaks a run that was purely
//! `accessor` so far, and vice versa) -- so a long chain of many
//! independent, shallow terms (many `&&`-joined comparisons, many list
//! elements) is never penalized for its length, and a pure accessor
//! chain is judged against `accessor`'s own, much larger cap rather than
//! `chain`'s tighter one. `logical_unary` is reset only at `,`/`?`/`:`
//! (an independent subexpression boundary), never at `&&`/`||`
//! themselves -- those are exactly what it counts.
//!
//! ## The five measured boundaries
//!
//! | shape | Circuitry's own boundary (tightest of the three contexts) | kind |
//! |---|---|---|
//! | nested parentheses, nested global calls (`int(int(...))`) | 53 | clean `CelEvaluationError` |
//! | nested list literals, nested map literals | 48 | clean |
//! | `+`/`-`/`*`/`/`/`%`/`<`/`<=`/`>`/`>=`/`==`/`!=` chains, an index chain (`state.input.a[0][0]...`, the path resolved) | 238-241 | clean |
//! | `&&`/`||` chains, a run of prefix `!` or prefix `-` | 484 | clean |
//! | a member-access chain (`state.input.a.a...`) | 610 | **bare**, uncaught `RecursionError` -- see below |
//!
//! Each cap below sits at or just under the tightest of the three
//! contexts for its shape (never rejecting something Circuitry accepted
//! in its most constrained real context), and every one of them is far
//! under the depth this crate's own parser and evaluator need to
//! actually overflow a 2 MiB stack (next section) -- so lowering any of
//! them to match Circuitry more exactly, or raising them because a
//! future measurement finds more headroom, is always safe on this
//! crate's own side; only Circuitry's side constrains the choice.
//!
//! The three contexts' own numbers differ by at most a term or two for
//! every bracket/chain/logical shape (for example, the `+` chain above:
//! 241 accepted in the `if`/`use` contexts, 242 in `while`) -- small
//! enough that Circuitry's limit for these shapes is, for this crate's
//! purposes, effectively one fixed number, not a genuinely unstable
//! range. The member-access chain is the one shape where the three
//! contexts disagree by more (610-613) *and* always fails as a bare,
//! uncaught exception rather than a caught one (next section) --
//! [`MAX_ACCESSOR_NESTING_DEPTH`] is set to the tightest of the three
//! (610) specifically because that range is real, not measurement noise.
//!
//! ## Why a member-access chain is different: it fails before the document ever runs
//!
//! Every other shape above fails Circuitry during *evaluation* -- the
//! document still compiles, the effect runs, and the failure is a
//! caught `CelEvaluationError` an `on_error:` can handle, worded
//! `"CEL evaluation failed for '<expr>': <reason>"` with the failing
//! effect's own path prefixed (`prime.<name>: ...`). A member-access
//! chain this deep fails Circuitry during *compilation* instead --
//! `core/cel_eval.py`'s `_collect_state_paths`/`_dotted_chain` walk the
//! whole parse tree once per `.` link to find every `state.` path the
//! expression reads, and the `RecursionError` that walk raises is never
//! caught by anything in `_compile`/`validate_cel_syntax` (both guard
//! only the parse call itself, not path collection) -- confirmed
//! directly: `cof check` on the exact same file fails identically, and
//! the document never starts running. The visible text has neither the
//! usual `"CEL evaluation failed for ..."` wording nor the failing
//! effect's own path -- just Python's own bare exception text, which
//! varies with which further recursion the exception's own formatting
//! triggers while escaping (`"maximum recursion depth exceeded"` in a
//! top-level `if`; `"...while getting the str of an object"` in the
//! other two contexts) -- confirmed directly via the CLI, not inferred.
//!
//! An index chain (`state.input.a[0][0]...`) behaves like every other
//! chain shape -- a clean, caught evaluation failure at 238 -- only when
//! the path it indexes into actually resolves to real data. When it
//! doesn't (for example, inside a `use` child whose parent never forwarded
//! `state.input` into it), Circuitry's own unresolved-path check walks
//! the same parse tree before ever reading a value, so it shares the
//! member-access chain's own compile-time mechanism and magnitude (measured
//! directly at 608 in that case) rather than the index chain's normal one.
//! This crate's own index cap (238, folded into `chain`) already rejects
//! anything that deep long before either boundary, so the unresolved case
//! needs no separate handling.
//!
//! This crate matches the *phase* as well as the boundary for this one
//! shape: [`validate_syntax`] (the compile-time check, run before a
//! document is ever accepted) rejects a member-access chain past
//! [`MAX_ACCESSOR_NESTING_DEPTH`] with [`CelSyntaxError::TooDeeplyNestedAccessor`],
//! whose own `Display` is the bare `"maximum recursion depth exceeded"`
//! text (no `"CEL evaluation failed for"` wrapper, no effect path) --
//! the simpler of the two forms Circuitry itself produces, picked because
//! it is the one seen from a top-level `if`, the shallowest and most
//! common real context. Every other shape's cap is checked only at
//! *evaluation* time ([`crate::check_nesting_depth`], called from
//! [`crate::evaluate_condition`]/[`crate::evaluate_expect`]), worded with
//! the usual `"CEL evaluation failed for '<expr>': ..."` form, matching
//! Circuitry's own runtime `CelEvaluationError` -- a document using one of
//! those shapes this deep compiles and starts running in Circuitry too,
//! only failing once the effect actually evaluates, which `on_error:` can
//! catch.
//!
//! [`validate_syntax`] additionally applies [`MAX_SAFETY_CHAIN_DEPTH`], a
//! much larger, safety-only cap on the raw combined scan (every link
//! counted together, ignoring which of the five buckets it fell into) --
//! not a parity boundary, just insurance against this crate's own
//! `env.compile` (which does the real ANTLR-visitor recursion the module
//! docs above describe) ever being asked to compile something deep
//! enough to overflow a 2 MiB stack for real before any of the tighter,
//! shape-specific checks would have caught it. Brackets need no such
//! backstop: `cel`'s own parser has a hard-coded `max_recursion_depth` of
//! **96** (`cel-0.15.0/src/parser/parser.rs`, confirmed directly: a bare
//! `cel::Env`, nothing from this crate involved, accepts up to 95 levels
//! of nested parentheses and rejects, gracefully, every depth from 96 up
//! to at least 50,000, never a crash). A chain-shaped expression has no
//! such guard and recurses for real in `env.compile`'s own ANTLR visitor;
//! measured directly on an explicit 2 MiB thread stack (this crate's own
//! deliberately conservative floor for the test that measured it, not a
//! platform default or a claim about how `electricity-cli` itself runs --
//! its own `main.rs` evaluates CEL on the process's main thread, whose
//! stack is the OS default, not a spawned one at all), within
//! [`crate::MAX_EXPR_LENGTH`]'s 4096 characters: an
//! arithmetic chain and a sequential index chain both overflowed between
//! 1,620 and 1,640 terms (debug and `--release` alike); a select chain, a
//! relation chain and a method-call chain did not overflow even at the
//! deepest each can reach under the character cap (roughly 2045, 1080 and
//! 1020 terms) -- a property of today's stack-frame sizes against
//! today's character cap, not a structural difference, so
//! [`MAX_SAFETY_CHAIN_DEPTH`] (1,000, comfortably under the lowest
//! measured floor) treats every chain shape the same way the scan itself
//! already does.
//!
//! ## Known imprecision
//!
//! Tracking `chain`/`accessor`/`logical_unary` independently, rather than
//! their sum against one combined cap, is not a safety argument for a
//! *mixed* shape (for example, parentheses wrapped around a long
//! arithmetic chain): a combined expression can fail Circuitry's own
//! evaluator well under the sum of the relevant caps, because Python's
//! recursion budget is one shared resource regardless of which grammar
//! rule is consuming it. Tracking independently is safe for *this*
//! crate's own stack only, because the margins are generous enough that
//! it doesn't matter (worked example in the tests below,
//! `a_group_that_is_the_leftmost_operand_adds_to_the_outer_chain`). A
//! mixed expression this crate's own pre-scan accepts but Circuitry would
//! reject is a known, accepted residual divergence -- this crate being
//! *more* lenient, never less, on a shape this far outside anything an
//! ordinary orchestration writes -- not a gap either side needs to close.
//!
//! This scan can both over- and undercount the true worst-case recursion
//! depth for a handful of tree shapes a single left-to-right pass cannot
//! represent exactly without becoming a real parser; it is a close,
//! deliberately conservative proxy for recursion cost, not an exact frame
//! count, and should not be relied on for more precision than that.

/// The deepest simultaneous grouping-`(`/global-call-`(` nesting
/// ([`max_nesting_depths`]'s `paren_call`) an expression may reach
/// before it is rejected -- the tightest of the three real contexts
/// Circuitry's own evaluator was measured in (module docs): 53. Far
/// under the `cel` crate's own hard-coded parser recursion guard (96,
/// module docs) -- that guard, not this cap, is what actually keeps
/// bracket nesting from ever threatening a stack overflow in this
/// crate; this cap exists purely to match Circuitry's own, much
/// tighter boundary.
pub const MAX_PAREN_CALL_NESTING_DEPTH: usize = 53;

/// The deepest simultaneous list-literal-`[`/map-literal-`{` nesting
/// ([`max_nesting_depths`]'s `list_map`) an expression may reach before
/// it is rejected -- the tightest of the three real contexts measured
/// (module docs): 48.
pub const MAX_LIST_MAP_NESTING_DEPTH: usize = 48;

/// The deepest operator/index chain ([`max_nesting_depths`]'s `chain`)
/// an expression may reach before it is rejected -- shared by every
/// arithmetic, relational, equality and index shape, at or below the
/// tightest of the per-shape floors Circuitry's own evaluator was
/// measured accepting in every of the three real contexts (module
/// docs): 238 (an index chain's own floor; the arithmetic/relational/
/// equality shapes reach 241 or 242).
pub const MAX_CHAIN_NESTING_DEPTH: usize = 238;

/// The deepest run of `&&`/`||` operators or prefix `!`/`-` occurrences
/// ([`max_nesting_depths`]'s `logical_unary`) an expression may reach
/// before it is rejected -- the tightest of the three real contexts
/// measured (module docs): 484. Each character costs its own stack
/// frame in Circuitry's own evaluator even though `cel`'s own grammar
/// never recurses for these at all (module docs).
pub const MAX_LOGICAL_UNARY_NESTING_DEPTH: usize = 484;

/// The deepest member-access chain ([`max_nesting_depths`]'s
/// `accessor`) an expression may reach before it is rejected -- the
/// tightest of the three real contexts measured (module docs): 610.
/// Checked only at *compile* time ([`crate::validate_syntax`]), not at
/// evaluation time, because this is the one shape where Circuitry
/// itself fails before a document ever starts running (module docs).
pub const MAX_ACCESSOR_NESTING_DEPTH: usize = 610;

/// A safety-only cap on the raw, combined scan (every link counted
/// together, regardless of which of the five buckets above it would
/// fall into) -- not a parity boundary, insurance for [`crate::validate_syntax`]'s
/// own call into `env.compile` against this crate's own real overflow
/// floor (module docs: 1,620-1,640 terms on a 2 MiB stack). Comfortably
/// under that floor; every shape-specific cap above is tighter than
/// this one for its own shape, so this one only ever matters for a
/// shape none of the five caps bounds as tightly -- there is no such
/// shape today, but a future one should still be caught here rather
/// than reaching `env.compile` at a dangerous depth.
pub const MAX_SAFETY_CHAIN_DEPTH: usize = 1_000;

/// [`max_nesting_depths`]'s own result -- five numbers tracked
/// independently (module docs explain why, and why not six or two).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NestingDepths {
    /// Deepest simultaneous grouping-`(`/global-call-`(` nesting reached.
    pub paren_call: usize,
    /// Deepest simultaneous list-literal-`[`/map-literal-`{` nesting reached.
    pub list_map: usize,
    /// Deepest operator/index chain reached.
    pub chain: usize,
    /// Deepest run of `&&`/`||`/prefix-`!`/prefix-`-` reached.
    pub logical_unary: usize,
    /// Deepest member-access chain reached.
    pub accessor: usize,
    /// Deepest point of the raw, combined scan (every link counted
    /// together) -- [`MAX_SAFETY_CHAIN_DEPTH`]'s own input, not a
    /// parity number.
    pub raw_combined: usize,
}

/// Which bracket a stack entry came from, so closing it decrements the
/// right counter (or none, for an index's `[` / a method call's `(`,
/// which were already billed to `chain`/`accessor` instead).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenBracket {
    ParenCall,
    ListMap,
    NotCounted,
}

/// Scans *expr* for the deepest point each of the five nesting shapes
/// reaches -- see the module docs for exactly what each counts, and why
/// they're tracked separately. Skips over string/bytes literals
/// (`'...'`, `"..."`, `'''...'''`, `"""..."""`, any of the four
/// optionally `b`/`B`-prefixed) and `//` line comments, so a character
/// that only ever appears as string content or inside a comment is
/// never counted; mirrors CEL's own grammar (`parser/gen/CEL.g4`'s
/// `STRING`/`BYTES`/`COMMENT` rules) for that, not a general-purpose
/// tokenizer -- this is a pre-scan, not a second parser. The one known
/// inexactness, inherited unchanged from an earlier version of this
/// scan: a raw string (`r"..."`/`R'...'`, whose grammar rule gives
/// backslash no escaping role at all) is still scanned with the same "a
/// backslash always escapes the next character" rule every other
/// string gets, which can only ever make this scan treat *more* of the
/// expression as string content than CEL's own lexer would, never less
/// -- so a pathological raw string immediately before a run of real
/// brackets/operators could cause this pre-scan to undercount them and
/// let the expression through uncaught by its own depth check. That
/// case does not need this pre-scan to be safe: `cel`'s own internal
/// recursion guard (bracket shapes) or the measured overflow margin
/// (chain shapes, module docs) still catches it, independent of whether
/// this scan counted correctly.
pub fn max_nesting_depths(expr: &str) -> NestingDepths {
    let mut paren_call_depth: usize = 0;
    let mut list_map_depth: usize = 0;
    let mut chain: usize = 0;
    let mut logical_unary: usize = 0;
    let mut accessor: usize = 0;
    let mut raw_combined: usize = 0;

    let mut max_paren_call: usize = 0;
    let mut max_list_map: usize = 0;
    let mut max_chain: usize = 0;
    let mut max_logical_unary: usize = 0;
    let mut max_accessor: usize = 0;
    let mut max_raw_combined: usize = 0;

    // Whether the token just scanned could be the left operand of a
    // binary operator, or the receiver of `.`/`[` -- an identifier,
    // keyword literal (`true`/`false`/`null`), number, string/bytes
    // literal, or a closing `)`/`]`/`}`.
    let mut prev_value_end = false;
    // Whether the token just scanned was an identifier (never a
    // literal, closing bracket, or keyword-operator like `in`) --
    // needed only to tell a method call's `.name(` (already counted via
    // `.`, so its `(` must not *also* bump `paren_call_depth`) apart
    // from a global call's bare `name(` (which has no `.` to count it,
    // and nested global calls are a genuine, un-flattened nesting risk
    // the same way nested parens are, so its `(` must).
    let mut prev_was_identifier = false;
    // Whether the identifier just scanned was itself preceded by `.`.
    let mut prev_identifier_was_dotted = false;
    // Whether a `.` was just scanned, awaiting the identifier that
    // completes it.
    let mut after_dot = false;
    // Per opened bracket, which counter closing it should decrement.
    let mut open_brackets: Vec<OpenBracket> = Vec::new();

    let chars: Vec<char> = expr.chars().collect();
    let n = chars.len();
    let mut i = 0;

    macro_rules! bump_raw {
        () => {
            raw_combined += 1;
            if raw_combined > max_raw_combined {
                max_raw_combined = raw_combined;
            }
        };
    }
    macro_rules! reset_chain_and_accessor {
        () => {
            chain = 0;
            accessor = 0;
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
                    bump_raw!();
                    if chain > max_chain {
                        max_chain = chain;
                    }
                    accessor = 0;
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
                // opening bracket: counted via `chain`, the same as an
                // operator link, so a sequential `x[0][0][0]...` chain
                // (brackets never simultaneously open) still costs one
                // level per link.
                chain += 1;
                bump_raw!();
                if chain > max_chain {
                    max_chain = chain;
                }
                accessor = 0;
                open_brackets.push(OpenBracket::NotCounted);
                prev_value_end = false;
                prev_was_identifier = false;
            }
            '(' if prev_was_identifier && prev_identifier_was_dotted => {
                // A method call's `(` (`x.f(...)`) -- its link was
                // already counted via `accessor` when `.` was scanned;
                // counting this `(` too would double-count every link
                // of a `.f().f().f()...` chain.
                open_brackets.push(OpenBracket::NotCounted);
                prev_value_end = false;
                prev_was_identifier = false;
            }
            '(' => {
                // Grouping `(...)` or a *global* call's `(` (`f(...)`,
                // no `.` to have already counted it) -- `f(f(f(...)))`
                // nests these for real, so unlike a method call's `(`,
                // this one must bump `paren_call_depth` to catch it.
                paren_call_depth += 1;
                bump_raw!();
                if paren_call_depth > max_paren_call {
                    max_paren_call = paren_call_depth;
                }
                open_brackets.push(OpenBracket::ParenCall);
                prev_value_end = false;
                prev_was_identifier = false;
            }
            '[' | '{' => {
                // A list or map literal's own brackets.
                list_map_depth += 1;
                bump_raw!();
                if list_map_depth > max_list_map {
                    max_list_map = list_map_depth;
                }
                open_brackets.push(OpenBracket::ListMap);
                prev_value_end = false;
                prev_was_identifier = false;
            }
            ')' | ']' | '}' => {
                match open_brackets.pop() {
                    Some(OpenBracket::ParenCall) => {
                        paren_call_depth = paren_call_depth.saturating_sub(1);
                    }
                    Some(OpenBracket::ListMap) => {
                        list_map_depth = list_map_depth.saturating_sub(1);
                    }
                    Some(OpenBracket::NotCounted) | None => {}
                }
                prev_value_end = true;
                prev_was_identifier = false;
            }
            '&' if i + 1 < n && chars[i + 1] == '&' => {
                i += 2;
                logical_unary += 1;
                bump_raw!();
                if logical_unary > max_logical_unary {
                    max_logical_unary = logical_unary;
                }
                reset_chain_and_accessor!();
                prev_value_end = false;
                prev_was_identifier = false;
                continue;
            }
            '|' if i + 1 < n && chars[i + 1] == '|' => {
                i += 2;
                logical_unary += 1;
                bump_raw!();
                if logical_unary > max_logical_unary {
                    max_logical_unary = logical_unary;
                }
                reset_chain_and_accessor!();
                prev_value_end = false;
                prev_was_identifier = false;
                continue;
            }
            ',' | '?' | ':' => {
                reset_chain_and_accessor!();
                logical_unary = 0;
                prev_value_end = false;
                prev_was_identifier = false;
            }
            '<' | '>' | '=' | '!' => {
                // `<=`/`>=`/`==`/`!=` are two-char comparisons; a bare
                // `!` with no `=` following is unary-not, handled below
                // instead. A bare `=` alone never appears in a valid
                // CEL expression.
                let two_char = i + 1 < n && chars[i + 1] == '=';
                if c == '<' || c == '>' || (c == '=' && two_char) || (c == '!' && two_char) {
                    chain += 1;
                    bump_raw!();
                    if chain > max_chain {
                        max_chain = chain;
                    }
                    accessor = 0;
                } else if c == '!' {
                    // Prefix unary-not -- costs Circuitry its own stack
                    // frame per character (module docs), unlike `cel`'s
                    // own grammar.
                    logical_unary += 1;
                    bump_raw!();
                    if logical_unary > max_logical_unary {
                        max_logical_unary = logical_unary;
                    }
                }
                if two_char {
                    i += 1;
                }
                prev_value_end = false;
                prev_was_identifier = false;
            }
            '+' | '-' => {
                // Binary only when it follows a value; a leading or
                // post-operator `+`/`-` is unary.
                if prev_value_end {
                    chain += 1;
                    bump_raw!();
                    if chain > max_chain {
                        max_chain = chain;
                    }
                    accessor = 0;
                } else if c == '-' {
                    // Prefix unary-minus -- same per-character cost as
                    // unary-not (module docs). A unary `+` does not
                    // exist in CEL's grammar, so only `-` needs this.
                    logical_unary += 1;
                    bump_raw!();
                    if logical_unary > max_logical_unary {
                        max_logical_unary = logical_unary;
                    }
                }
                prev_value_end = false;
                prev_was_identifier = false;
            }
            '*' | '%' | '/' => {
                chain += 1;
                bump_raw!();
                if chain > max_chain {
                    max_chain = chain;
                }
                accessor = 0;
                prev_value_end = false;
                prev_was_identifier = false;
            }
            '.' => {
                // Not a float literal's decimal point: the numeric
                // branch above already consumed any `.` directly
                // between two digit runs. A standalone `.` is always a
                // member-access dot (plain select or the lead-in to a
                // method call, counted the same way).
                accessor += 1;
                bump_raw!();
                if accessor > max_accessor {
                    max_accessor = accessor;
                }
                chain = 0;
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
        paren_call: max_paren_call,
        list_map: max_list_map,
        chain: max_chain,
        logical_unary: max_logical_unary,
        accessor: max_accessor,
        raw_combined: max_raw_combined,
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

    fn paren_call(expr: &str) -> usize {
        max_nesting_depths(expr).paren_call
    }
    fn list_map(expr: &str) -> usize {
        max_nesting_depths(expr).list_map
    }
    fn chain(expr: &str) -> usize {
        max_nesting_depths(expr).chain
    }
    fn logical_unary(expr: &str) -> usize {
        max_nesting_depths(expr).logical_unary
    }
    fn accessor(expr: &str) -> usize {
        max_nesting_depths(expr).accessor
    }

    #[test]
    fn scalar_has_zero_depth() {
        let d = max_nesting_depths("1 + 1");
        assert_eq!(
            d,
            NestingDepths {
                chain: 1,
                raw_combined: 1,
                ..NestingDepths::default()
            }
        );
    }

    #[test]
    fn literal_has_zero_depth() {
        assert_eq!(max_nesting_depths("1"), NestingDepths::default());
    }

    #[test]
    fn counts_simultaneous_paren_nesting_not_total_parens() {
        assert_eq!(paren_call("(1) + (2) + (3)"), 1);
        assert_eq!(paren_call("((()))"), 3);
    }

    #[test]
    fn parens_and_list_map_are_tracked_independently() {
        assert_eq!(paren_call("[{(1)}]"), 1);
        assert_eq!(list_map("[{(1)}]"), 2);
    }

    #[test]
    fn global_call_parens_count_with_grouping_parens_not_list_map() {
        assert_eq!(paren_call("int(int(1))"), 2);
        assert_eq!(list_map("int(int(1))"), 0);
    }

    #[test]
    fn method_call_parens_are_not_double_counted() {
        // `.f()` -- the link is counted once via `accessor` (the `.`),
        // not again via the `(`.
        assert_eq!(paren_call("state.f().f().f()"), 0);
        assert_eq!(accessor("state.f().f().f()"), 3);
    }

    #[test]
    fn ignores_brackets_and_operators_inside_a_single_quoted_string() {
        assert_eq!(chain("'((((( + + +' == x"), 1);
        assert_eq!(paren_call("'((((( + + +' == x"), 0);
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
                paren_call: 2,
                chain: 1,
                logical_unary: 1,
                raw_combined: 5,
                ..NestingDepths::default()
            }
        );
    }

    #[test]
    fn exactly_at_the_limit_and_one_past_it_for_parens() {
        let expr = format!(
            "{}1{}",
            "(".repeat(MAX_PAREN_CALL_NESTING_DEPTH),
            ")".repeat(MAX_PAREN_CALL_NESTING_DEPTH)
        );
        assert_eq!(paren_call(&expr), MAX_PAREN_CALL_NESTING_DEPTH);
        let expr = format!(
            "{}1{}",
            "(".repeat(MAX_PAREN_CALL_NESTING_DEPTH + 1),
            ")".repeat(MAX_PAREN_CALL_NESTING_DEPTH + 1)
        );
        assert_eq!(paren_call(&expr), MAX_PAREN_CALL_NESTING_DEPTH + 1);
    }

    #[test]
    fn exactly_at_the_limit_and_one_past_it_for_nested_global_calls() {
        let expr = format!(
            "{}1{}",
            "int(".repeat(MAX_PAREN_CALL_NESTING_DEPTH),
            ")".repeat(MAX_PAREN_CALL_NESTING_DEPTH)
        );
        assert_eq!(paren_call(&expr), MAX_PAREN_CALL_NESTING_DEPTH);
        let expr = format!(
            "{}1{}",
            "int(".repeat(MAX_PAREN_CALL_NESTING_DEPTH + 1),
            ")".repeat(MAX_PAREN_CALL_NESTING_DEPTH + 1)
        );
        assert_eq!(paren_call(&expr), MAX_PAREN_CALL_NESTING_DEPTH + 1);
    }

    #[test]
    fn exactly_at_the_limit_and_one_past_it_for_list_literals() {
        let expr = format!(
            "{}1{}",
            "[".repeat(MAX_LIST_MAP_NESTING_DEPTH),
            "]".repeat(MAX_LIST_MAP_NESTING_DEPTH)
        );
        assert_eq!(list_map(&expr), MAX_LIST_MAP_NESTING_DEPTH);
        let expr = format!(
            "{}1{}",
            "[".repeat(MAX_LIST_MAP_NESTING_DEPTH + 1),
            "]".repeat(MAX_LIST_MAP_NESTING_DEPTH + 1)
        );
        assert_eq!(list_map(&expr), MAX_LIST_MAP_NESTING_DEPTH + 1);
    }

    #[test]
    fn exactly_at_the_limit_and_one_past_it_for_map_literals() {
        let expr = format!(
            "{}1{}",
            "{'a':".repeat(MAX_LIST_MAP_NESTING_DEPTH),
            "}".repeat(MAX_LIST_MAP_NESTING_DEPTH)
        );
        assert_eq!(list_map(&expr), MAX_LIST_MAP_NESTING_DEPTH);
        let expr = format!(
            "{}1{}",
            "{'a':".repeat(MAX_LIST_MAP_NESTING_DEPTH + 1),
            "}".repeat(MAX_LIST_MAP_NESTING_DEPTH + 1)
        );
        assert_eq!(list_map(&expr), MAX_LIST_MAP_NESTING_DEPTH + 1);
    }

    #[test]
    fn exactly_at_the_limit_and_one_past_it_for_an_arithmetic_chain() {
        let expr = format!("0{}", "+1".repeat(MAX_CHAIN_NESTING_DEPTH));
        assert_eq!(chain(&expr), MAX_CHAIN_NESTING_DEPTH);
        let expr = format!("0{}", "+1".repeat(MAX_CHAIN_NESTING_DEPTH + 1));
        assert_eq!(chain(&expr), MAX_CHAIN_NESTING_DEPTH + 1);
    }

    #[test]
    fn exactly_at_the_limit_and_one_past_it_for_a_division_chain() {
        let expr = format!("1{}", "/1".repeat(MAX_CHAIN_NESTING_DEPTH));
        assert_eq!(chain(&expr), MAX_CHAIN_NESTING_DEPTH);
        let expr = format!("1{}", "/1".repeat(MAX_CHAIN_NESTING_DEPTH + 1));
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
    fn exactly_at_the_limit_and_one_past_it_for_an_equality_chain() {
        let expr = format!("0{}", "==1".repeat(MAX_CHAIN_NESTING_DEPTH));
        assert_eq!(chain(&expr), MAX_CHAIN_NESTING_DEPTH);
        let expr = format!("0{}", "==1".repeat(MAX_CHAIN_NESTING_DEPTH + 1));
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
    fn exactly_at_the_limit_and_one_past_it_for_an_and_chain() {
        let expr = (0..=MAX_LOGICAL_UNARY_NESTING_DEPTH)
            .map(|_| "true")
            .collect::<Vec<_>>()
            .join(" && ");
        assert_eq!(logical_unary(&expr), MAX_LOGICAL_UNARY_NESTING_DEPTH);
        let expr = (0..=MAX_LOGICAL_UNARY_NESTING_DEPTH + 1)
            .map(|_| "true")
            .collect::<Vec<_>>()
            .join(" && ");
        assert_eq!(logical_unary(&expr), MAX_LOGICAL_UNARY_NESTING_DEPTH + 1);
    }

    #[test]
    fn exactly_at_the_limit_and_one_past_it_for_an_or_chain() {
        let expr = (0..=MAX_LOGICAL_UNARY_NESTING_DEPTH)
            .map(|_| "true")
            .collect::<Vec<_>>()
            .join(" || ");
        assert_eq!(logical_unary(&expr), MAX_LOGICAL_UNARY_NESTING_DEPTH);
    }

    #[test]
    fn exactly_at_the_limit_and_one_past_it_for_a_unary_not_run() {
        let expr = format!("{}true", "!".repeat(MAX_LOGICAL_UNARY_NESTING_DEPTH));
        assert_eq!(logical_unary(&expr), MAX_LOGICAL_UNARY_NESTING_DEPTH);
        let expr = format!("{}true", "!".repeat(MAX_LOGICAL_UNARY_NESTING_DEPTH + 1));
        assert_eq!(logical_unary(&expr), MAX_LOGICAL_UNARY_NESTING_DEPTH + 1);
    }

    #[test]
    fn exactly_at_the_limit_and_one_past_it_for_a_unary_minus_run() {
        let expr = format!("{}1", "-".repeat(MAX_LOGICAL_UNARY_NESTING_DEPTH));
        assert_eq!(logical_unary(&expr), MAX_LOGICAL_UNARY_NESTING_DEPTH);
        let expr = format!("{}1", "-".repeat(MAX_LOGICAL_UNARY_NESTING_DEPTH + 1));
        assert_eq!(logical_unary(&expr), MAX_LOGICAL_UNARY_NESTING_DEPTH + 1);
    }

    #[test]
    fn exactly_at_the_limit_and_one_past_it_for_a_select_chain() {
        let expr = format!("state{}", ".a".repeat(MAX_ACCESSOR_NESTING_DEPTH));
        assert_eq!(accessor(&expr), MAX_ACCESSOR_NESTING_DEPTH);
        let expr = format!("state{}", ".a".repeat(MAX_ACCESSOR_NESTING_DEPTH + 1));
        assert_eq!(accessor(&expr), MAX_ACCESSOR_NESTING_DEPTH + 1);
    }

    #[test]
    fn method_call_chain_counts_once_per_link_via_accessor() {
        let expr = format!("state{}", ".f()".repeat(MAX_ACCESSOR_NESTING_DEPTH));
        assert_eq!(accessor(&expr), MAX_ACCESSOR_NESTING_DEPTH);
    }

    #[test]
    fn and_resets_chain_and_accessor_so_many_independent_terms_are_safe() {
        // A realistic wide condition -- many independent `a.b == N`
        // comparisons ANDed together -- must not be rejected just
        // because there happen to be more than `MAX_CHAIN_NESTING_DEPTH`
        // of them: each is only one `.` and one `==` deep, and `&&`
        // never stacks a parser frame on either side (module docs).
        let expr = (0..100)
            .map(|i| format!("state.a == {i}"))
            .collect::<Vec<_>>()
            .join(" && ");
        let d = max_nesting_depths(&expr);
        assert_eq!(d.chain, 1);
        assert_eq!(d.accessor, 1);
        assert_eq!(d.logical_unary, 99);
    }

    #[test]
    fn a_long_and_chain_of_shallow_terms_does_not_grow_chain_or_accessor() {
        let expr = (0..2000)
            .map(|i| format!("state.a == {i}"))
            .collect::<Vec<_>>()
            .join(" && ");
        let d = max_nesting_depths(&expr);
        assert_eq!(d.chain, 1);
        assert_eq!(d.accessor, 1);
    }

    #[test]
    fn comma_resets_chain_accessor_and_logical_for_list_and_call_arguments() {
        let expr = (0..100)
            .map(|i| format!("{i}.5"))
            .collect::<Vec<_>>()
            .join(", ");
        let d = max_nesting_depths(&format!("[{expr}]"));
        assert_eq!(d.chain, 0);
        assert_eq!(d.accessor, 0);
        assert_eq!(d.list_map, 1);
    }

    #[test]
    fn float_literals_are_not_mistaken_for_member_access() {
        assert_eq!(chain("1.5 + 2.5 + 3.5"), 2);
        assert_eq!(accessor("1.5 + 2.5 + 3.5"), 0);
    }

    #[test]
    fn an_operator_link_resets_accessor_and_an_accessor_link_resets_chain() {
        // A mixed shape (`state.a.a + 1 + 1`): the dotted prefix is
        // judged against `accessor`'s own cap, the arithmetic suffix
        // against `chain`'s -- each resets the other, so neither
        // silently inherits the other's count.
        let d = max_nesting_depths("state.a.a + 1 + 1");
        assert_eq!(d.accessor, 2);
        assert_eq!(d.chain, 2);
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
        // to a real overflow (~1,620 unbracketed terms, module docs) is
        // many multiples of either cap, not because undercounting is
        // ever the safe direction in general. `paren_call` peaks at 1
        // (only the one group is ever open at once).
        let expr = "(1+1+1+1) + 1 + 1 + 1 + 1";
        let d = max_nesting_depths(expr);
        assert_eq!(d.paren_call, 1);
        assert_eq!(d.chain, 3 + 4);
    }
}
