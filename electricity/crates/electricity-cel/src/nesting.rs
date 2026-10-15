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
//! binary-operator chain. This module tracks seven numbers rather than
//! one, matching the seven boundaries measured (below), not the two or
//! three the grammar alone would suggest:
//!
//! - **`paren_call`** -- simultaneous grouping-`(`/global-call-`(`
//!   nesting (never a method call's own `(`, which `method_call`
//!   accounting handles instead -- see below).
//! - **`list_map`** -- simultaneous list-literal-`[`/map-literal-`{`
//!   nesting (never an index's own `[`, which `index` handles instead).
//! - **`chain`** -- a running count of binary arithmetic (`+ - * / %`),
//!   relational/equality (`< <= > >= == !=`) and `in` operators.
//! - **`index`** -- a running count of an index's own `[` (`a[0][0]...`,
//!   as opposed to a list literal's own opening `[`) -- tracked
//!   separately from `chain` even though both recurse through the same
//!   `member`/`calc`/`relation` grammar rules in `cel`'s own parser,
//!   because Circuitry's own measured floor for the two differs
//!   (238 for an index chain, 241 for the operator shapes `chain`
//!   covers -- sharing one counter at the tighter of the two, as an
//!   earlier version of this module did, rejected an operator chain
//!   Circuitry itself still accepted).
//! - **`logical_unary`** -- a running count of `&&`/`||` operators and
//!   individual prefix `!`/`-` occurrences (one per character, not one
//!   per run -- each costs Circuitry its own stack frame even though
//!   `cel`'s own grammar collapses a whole run into one visit).
//! - **`accessor`** -- a running count of member-access `.` links
//!   (never a float literal's decimal point, and never a `.` that is
//!   part of a method call -- see `method_call` below) in a chain with
//!   *no* method call anywhere in it. Counts dots (link boundaries), not
//!   the identifiers between them: `state.input` plus 610 further `.a`
//!   links is 611 dots total, 610 of them past the initial root, and
//!   [`MAX_ACCESSOR_NESTING_DEPTH`] (610) counts the latter -- the links
//!   added after the root, matching how every measurement below built
//!   its own probe expression (module docs' per-shape detail).
//! - **`method_call`** -- a running count of every link (`.field` *and*
//!   `.method(...)`) in a member-access chain that includes *at least
//!   one* method call anywhere in it -- the *whole* chain, not just the
//!   links from the call onward: Circuitry's own `_dotted_chain` gives
//!   up on a chain the moment it sees any call in it, at any position,
//!   so the whole thing loses the plain-select mechanism `accessor`
//!   models and falls back to the real evaluator's own recursion
//!   instead (mechanism section below).
//!
//! `chain`/`index`/`accessor`/`method_call` are only ever reset to zero
//! at a `&&`, `||`, `,`, `?` or `:` *at the top level* (never inside an
//! open bracket -- see below), and whenever one of the other three is
//! incremented (a link of one family breaks a run that was purely
//! another family's so far, and vice versa) -- so a long chain of many
//! independent, shallow terms (many `&&`-joined comparisons, many list
//! elements) is never penalized for its length, and a pure accessor
//! chain is judged against `accessor`'s own, much larger cap rather than
//! `chain`'s tighter one. `logical_unary` is reset only at a top-level
//! `,`/`?`/`:` (an independent subexpression boundary), never at
//! `&&`/`||` themselves -- those are exactly what it counts.
//!
//! The *top-level-only* qualifier matters for `method_call` specifically
//! (and, for the same reason, every other reset): `.map(x, x)`'s own `,`
//! separates that one link's *arguments*, not independent elements of
//! the outer chain the call is one link of -- a `.map(x, x).map(x, x)...`
//! chain must count every one of its links, not reset to 1 at every
//! single link's own internal comma (a regression this module once had,
//! not a hypothetical: `open_brackets` already tracks exactly this, so
//! every reset site below checks `open_brackets.is_empty()` first).
//! `chain`/`paren_call` already never reset across a bracket boundary at
//! all by construction (the group-wrapped-chain test below), so this
//! makes the other three consistent with that, for the same underlying
//! reason: whatever a bracket's own contents do to `cel`'s real parser
//! stack, the run that contains the bracket is still on that stack too.
//!
//! ## The seven measured boundaries
//!
//! | shape | Circuitry's own boundary (tightest of the three contexts) | kind |
//! |---|---|---|
//! | nested parentheses, nested global calls (`int(int(...))`) | 53 | clean `CelEvaluationError` |
//! | nested list literals, nested map literals | 48 | clean |
//! | `+`/`-`/`*`/`/`/`%`/`<`/`<=`/`>`/`>=`/`==`/`!=`/`in` chains | 241 | clean |
//! | an index chain (`state.input.a[0][0]...`, the path resolved to real data) | 238 | clean |
//! | `&&`/`||` chains, a run of prefix `!` or prefix `-` | 484 | clean |
//! | a member-access chain (`state.input.a.a...`), with or without real resolved data at every level | 610 | **bare**, uncaught `RecursionError` -- see below |
//! | a member-access chain that includes a method call anywhere in it (`[1].map(x, x).map(x, x)...`) | 295 | clean |
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
//! Re-measured with a nested-dict `state` resolving every level of the
//! chain (not just an absent/empty one): the same 610-613 range, because
//! the failing mechanism (`_collect_state_paths`'s own parse-tree walk,
//! next section) runs before any value is ever read, so whether the path
//! resolves to real data never affects it.
//!
//! The method-call shape is different again: a chain containing a call
//! is governed by the *real evaluator's* own recursion (next section),
//! not by `_collect_state_paths`'s structural walk, and that evaluator's
//! own per-link cost turned out considerably higher than a plain select
//! -- 295, not 610-613 -- confirmed directly via `cof run` in all three
//! contexts (this shape was entirely
//! unmeasured before, assumed to share `accessor`'s own number and
//! phase, which both turned out wrong once actually measured).
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
//! This crate's own index cap ([`MAX_INDEX_NESTING_DEPTH`], 238) already
//! rejects anything that deep long before either boundary, so the
//! unresolved case needs no separate handling.
//!
//! A method-call chain fails by a *third* mechanism, different from
//! both: Circuitry's own `_dotted_chain` (`core/cel_eval.py`) walks a
//! member-access chain to decide whether it is a plain `state.`-rooted
//! path at all, and gives up -- returns `None`, same as for a chain
//! rooted at anything else `_dotted_chain` doesn't recognize -- the
//! moment it finds a `Call` node anywhere in the chain, not just a
//! `Select`. A chain `_dotted_chain` gives up on is never added to
//! `_collect_state_paths`'s own result, so the structural, compile-time
//! walk that fails a plain select at 610 never even looks at it; the
//! document compiles cleanly regardless of depth. It still has to be
//! *evaluated*, though, and `cel-python`'s own expression evaluator
//! (a separate, ordinary recursive-descent walk over the same parse
//! tree, inside `evaluate_cel`'s normal `try`/`except`) recurses once
//! per link the same way -- just with enough of its own stack cost per
//! frame that it fails far sooner, at 295, as a clean, caught
//! `CelEvaluationError` indistinguishable in shape from the four parity
//! families below.
//!
//! This crate matches the *phase* as well as the boundary for this one
//! shape: [`validate_syntax`] (the compile-time check, run before a
//! document is ever accepted) and [`crate::check_stack_safety`] (the
//! same check, for a caller reaching [`crate::evaluate_condition`]/
//! [`crate::evaluate_expect`] directly, bypassing the compiler) both
//! reject a member-access chain past [`MAX_ACCESSOR_NESTING_DEPTH`] --
//! as [`CelSyntaxError::TooDeeplyNestedAccessor`] from the former,
//! bare `"maximum recursion depth exceeded"` text from the latter (no
//! `"CEL evaluation failed for"` wrapper, no effect path) -- the
//! simpler of the two forms Circuitry itself produces, picked because
//! it is the one seen from a top-level `if`, the shallowest and most
//! common real context. Every other shape's cap -- including
//! `method_call`'s, despite failing by the third mechanism above -- is
//! checked only at *evaluation* time ([`crate::check_parity_nesting`],
//! called from [`crate::evaluate_condition`]/[`crate::evaluate_expect`]
//! -- in `evaluate_condition`'s case, only *after* the unset-path
//! short-circuit below has already passed, never before: Circuitry's
//! own evaluation of one of these shapes, the only place any of them
//! can raise this, never runs once that check has already decided the
//! condition is `false`), worded with the usual, and now deliberately
//! *identical regardless of shape*, `"CEL evaluation failed for
//! '<expr>': maximum recursion depth exceeded"` text -- Circuitry's own
//! uncaught `RecursionError` never says which operator or how deep
//! either, so which of the five caps tripped and at what depth is
//! carried in [`crate::NestingCapDetail`] instead of the message. A
//! document using one of those shapes this deep compiles and starts
//! running in Circuitry too, only failing once the effect actually
//! evaluates, which `on_error:` can catch.
//!
//! [`validate_syntax`] and [`crate::check_stack_safety`] both apply
//! [`MAX_SAFETY_CHAIN_DEPTH`] too, a much larger, safety-only cap on
//! the *live* sum of the five quantities that actually recurse in
//! `cel`'s own ANTLR visitor at any one scan position --
//! `paren_call`/`list_map`/`chain`/`index`/`accessor` together,
//! deliberately excluding `logical_unary` (`cel`'s grammar never
//! recurses for `&&`/`||`/unary runs at all, so no length of one
//! threatens this crate's own stack, module docs above) and
//! `method_call` (a chain with a call in it never reaches
//! `env.compile`'s own ANTLR-visitor recursion concern this backstop
//! insures against at all -- it fails, if it fails, in the real
//! evaluator afterward, a separate call `validate_syntax` itself never
//! makes) -- not a parity boundary,
//! just insurance against this crate's own `env.compile` (which does
//! the real ANTLR-visitor recursion the module docs above describe)
//! ever being asked to compile something deep enough to overflow a 2
//! MiB stack for real before any of the tighter, shape-specific checks
//! would have caught it. Brackets need no such
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
//! ## Known imprecision: mixed shapes
//!
//! Tracking `paren_call`/`list_map`/`chain`/`index`/`logical_unary`/
//! `accessor`/`method_call` independently, rather than their sum against
//! one combined cap, is not a safety argument for a *mixed* shape (two
//! or more of the seven stacked at once) -- and, unlike the module's
//! earlier drafts claimed, the resulting divergence from Circuitry runs
//! in **both** directions, not just the lenient one:
//!
//! - **This crate can be more lenient.** A combined expression can fail
//!   Circuitry's own evaluator well under the sum of the relevant caps,
//!   because Python's recursion budget is one shared resource regardless
//!   of which grammar rule is consuming it -- this crate's own caps,
//!   tracked independently, don't model that shared cost at all. Example,
//!   confirmed directly: 48 nested `size()` calls wrapping a map literal
//!   fails Circuitry (well under any single cap: `size` is a plain
//!   global call, so its own nesting counts against `paren_call`, whose
//!   cap is 53) but this crate's own pre-scan accepts it.
//! - **This crate can also be *stricter*.** [`crate::MAX_EXPR_LENGTH`]
//!   aside, a mixed shape can run into a boundary of `cel`'s own parser
//!   this module's docs above don't otherwise discuss: its hard-coded
//!   `max_recursion_depth` of 96 counts *every* `expr` grammar
//!   production on the path to the deepest node, not brackets alone --
//!   so 50 grouping parens wrapped around a 48-deep nested list literal
//!   (98 total `expr` productions on that path, each individually within
//!   both this crate's own caps, 53 and 48) fails to *parse* at all in
//!   this crate (`cel`'s own guard, not a cap this module tracks), where
//!   Circuitry still compiles it and only fails once it evaluates.
//!
//! A mixed shape this far outside anything an ordinary orchestration
//! writes is a known, accepted residual divergence in both directions,
//! not a gap either side needs to close -- this crate's own stack stays
//! safe regardless (the lenient direction's own worked example is in the
//! tests below, `a_group_that_is_the_leftmost_operand_adds_to_the_outer_chain`,
//! and the margin to a real overflow, module docs above, is many times
//! either cap). A combined, cross-shape weighted budget that would close
//! both directions at once is not required (and not attempted here).
//!
//! Related, in `electricity-compiler` rather than this module:
//! `crate::state_paths`'s own [`crate::check_stack_safety`] call (not
//! [`crate::check_parity_nesting`], module docs above on why) can reject
//! a chain shape this module itself would accept were it checked in
//! isolation, once mixed with enough of another shape to cross
//! [`MAX_SAFETY_CHAIN_DEPTH`] -- `state_ns.rs`'s own `validate_cel_expr`
//! deliberately swallows that `Err` (any parse/safety failure there is
//! `validate_syntax`'s problem to report, not this check's), which is
//! correct for this module's own safety-only rejections but means a
//! `state.<namespace>` grammar violation inside such an expression goes
//! unchecked by that one pass -- `validate_syntax` itself still runs
//! separately and still rejects the expression, just not for the
//! `state.<namespace>` reason.
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

/// The deepest arithmetic/relational/equality/`in` operator chain
/// ([`max_nesting_depths`]'s `chain`) an expression may reach before
/// it is rejected -- the tightest of the three real contexts measured
/// (module docs): 241. An index chain (`a[0][0]...`) has its own,
/// tighter cap, [`MAX_INDEX_NESTING_DEPTH`] -- not this one, even
/// though both recurse through the same `member`/`calc`/`relation`
/// grammar rules in `cel`'s own parser (measured
/// independently once an operator chain's real floor turned out
/// looser than an index chain's).
pub const MAX_CHAIN_NESTING_DEPTH: usize = 241;

/// The deepest index chain (`a[0][0]...`, [`max_nesting_depths`]'s
/// `index`) an expression may reach before it is rejected -- the
/// tightest of the three real contexts measured (module docs): 238,
/// three terms tighter than [`MAX_CHAIN_NESTING_DEPTH`]'s own 241 for
/// the operator shapes it shares a grammar rule with.
pub const MAX_INDEX_NESTING_DEPTH: usize = 238;

/// The deepest run of `&&`/`||` operators or prefix `!`/`-` occurrences
/// ([`max_nesting_depths`]'s `logical_unary`) an expression may reach
/// before it is rejected -- the tightest of the three real contexts
/// measured (module docs): 484. Each character costs its own stack
/// frame in Circuitry's own evaluator even though `cel`'s own grammar
/// never recurses for these at all (module docs).
pub const MAX_LOGICAL_UNARY_NESTING_DEPTH: usize = 484;

/// The deepest member-access-only chain ([`max_nesting_depths`]'s
/// `accessor`) an expression may reach before it is rejected -- the
/// tightest of the three real contexts measured, confirmed unchanged
/// against a chain that resolves to real data (module docs): 610.
/// Checked only at *compile* time ([`crate::validate_syntax`]/
/// [`crate::check_stack_safety`]), not at evaluation time, because this
/// is the one shape where Circuitry itself fails before a document
/// ever starts running (module docs) -- but only for a chain with *no*
/// method call anywhere in it; one that has any is
/// [`MAX_METHOD_CALL_NESTING_DEPTH`]'s own concern instead, a
/// different shape in Circuitry's own terms (module docs).
pub const MAX_ACCESSOR_NESTING_DEPTH: usize = 610;

/// The deepest member-access chain that includes at least one method
/// call ([`max_nesting_depths`]'s `method_call`) an expression may
/// reach before it is rejected -- the tightest of the three real
/// contexts measured (module docs): 295. Checked only at *evaluation*
/// time ([`crate::check_parity_nesting`]), the same phase and uniform
/// text as [`MAX_PAREN_CALL_NESTING_DEPTH`]/[`MAX_LIST_MAP_NESTING_DEPTH`]/
/// [`MAX_CHAIN_NESTING_DEPTH`]/[`MAX_LOGICAL_UNARY_NESTING_DEPTH`] --
/// unlike a pure member-access chain, Circuitry's own `_dotted_chain`
/// gives up on a chain containing any call at all (module docs), so no
/// structural, compile-time check ever applies to it; only the real
/// evaluator's own recursion can fail it, caught normally.
pub const MAX_METHOD_CALL_NESTING_DEPTH: usize = 295;

/// A safety-only cap on the raw, combined scan (every link counted
/// together, regardless of which of the six buckets above it would
/// fall into) -- not a parity boundary, insurance for [`crate::validate_syntax`]'s
/// own call into `env.compile` against this crate's own real overflow
/// floor (module docs: 1,620-1,640 terms on a 2 MiB stack). Comfortably
/// under that floor; every shape-specific cap above is tighter than
/// this one for its own shape, so this one only ever matters for a
/// shape none of the six caps bounds as tightly -- there is no such
/// shape today, but a future one should still be caught here rather
/// than reaching `env.compile` at a dangerous depth.
pub const MAX_SAFETY_CHAIN_DEPTH: usize = 1_000;

/// [`max_nesting_depths`]'s own result -- seven numbers tracked
/// independently (module docs explain why, and why not eight or two).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NestingDepths {
    /// Deepest simultaneous grouping-`(`/global-call-`(` nesting reached.
    pub paren_call: usize,
    /// Deepest simultaneous list-literal-`[`/map-literal-`{` nesting reached.
    pub list_map: usize,
    /// Deepest arithmetic/relational/equality/`in` operator chain reached.
    pub chain: usize,
    /// Deepest index chain reached ([`MAX_INDEX_NESTING_DEPTH`]'s own
    /// input, a different, tighter cap from `chain` above -- module
    /// docs).
    pub index: usize,
    /// Deepest run of `&&`/`||`/prefix-`!`/prefix-`-` reached.
    pub logical_unary: usize,
    /// Deepest member-access chain reached that never includes a
    /// method call.
    pub accessor: usize,
    /// Deepest member-access chain reached that includes at least one
    /// method call ([`MAX_METHOD_CALL_NESTING_DEPTH`]'s own input, a
    /// different shape in Circuitry's own terms from `accessor` above
    /// -- module docs).
    pub method_call: usize,
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
    let mut index: usize = 0;
    let mut logical_unary: usize = 0;
    let mut accessor: usize = 0;
    // Whether the member-access run `accessor` is currently counting
    // has included a method call (`.f(...)`) anywhere in it yet --
    // decided only when the run *ends* (module docs on why a run
    // containing any call is judged, in full, against
    // `MAX_METHOD_CALL_NESTING_DEPTH` instead of `MAX_ACCESSOR_NESTING_DEPTH`,
    // never split at the call).
    let mut accessor_run_has_call = false;

    let mut max_paren_call: usize = 0;
    let mut max_list_map: usize = 0;
    let mut max_chain: usize = 0;
    let mut max_index: usize = 0;
    let mut max_logical_unary: usize = 0;
    let mut max_accessor: usize = 0;
    let mut max_method_call: usize = 0;
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

    // Recomputed (not accumulated) at every call site below, right
    // after whichever of `paren_call_depth`/`list_map_depth`/`chain`/
    // `accessor` just changed -- the live sum of the four quantities
    // that actually recurse in `cel`'s own ANTLR visitor (module docs),
    // so `max_raw_combined` tracks the deepest point the real parser
    // stack would reach, not the total number of links scanned so far.
    // `logical_unary` is deliberately excluded: `cel`'s grammar never
    // recurses for `&&`/`||`/unary runs at all (module docs), so they
    // contribute nothing to this safety backstop regardless of how
    // long the run is.
    macro_rules! bump_raw {
        () => {
            let live = paren_call_depth + list_map_depth + chain + index + accessor;
            if live > max_raw_combined {
                max_raw_combined = live;
            }
        };
    }
    // Commits the member-access run `accessor` has been counting to
    // whichever of `max_accessor`/`max_method_call` it belongs to --
    // decided now, not at each `.`, because whether a run belongs to
    // the tighter `method_call` bucket depends on whether *any* `(` the
    // run ever saw was a method call, which this single left-to-right
    // pass can only know once the run ends (module docs).
    macro_rules! commit_accessor_run {
        () => {
            if accessor_run_has_call {
                if accessor > max_method_call {
                    max_method_call = accessor;
                }
            } else if accessor > max_accessor {
                max_accessor = accessor;
            }
        };
    }
    macro_rules! end_accessor_run {
        () => {
            commit_accessor_run!();
            accessor = 0;
            accessor_run_has_call = false;
        };
    }
    // Only resets at the *top* level (`open_brackets` empty): inside
    // any open bracket -- most importantly a method call's own `(...)`
    // -- a `,`/`&&`/`||` is scoping an *argument* of that call, not
    // ending the member-access run the call itself is one link of.
    // `[1].map(x, x).map(x, x)` must count all 297 links of a 297-deep
    // chain, not just 1, even though every single link's own call has
    // an internal `,` between `x` and `x` -- resetting there would
    // otherwise throw away everything counted before it, every single
    // link (a genuine regression this scan once had, not hypothetical).
    // `chain`/`paren_call` already never reset across a bracket
    // boundary at all (module docs on `a_group_that_is_the_leftmost_
    // operand_adds_to_the_outer_chain`); this makes `accessor`/
    // `logical_unary` consistent with that, for the same reason.
    macro_rules! reset_chain_and_accessor {
        () => {
            if open_brackets.is_empty() {
                chain = 0;
                index = 0;
                end_accessor_run!();
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
                    bump_raw!();
                    if chain > max_chain {
                        max_chain = chain;
                    }
                    index = 0;
                    end_accessor_run!();
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
                // Index continuation (`x[i]`) -- its own counter, not
                // `chain` (module docs on why: measured independently
                // at a tighter floor, 238 against the operator chain's
                // own 241) -- not a list literal's own opening
                // bracket. A sequential `x[0][0][0]...` chain (brackets
                // never simultaneously open) still costs one level per
                // link.
                index += 1;
                bump_raw!();
                if index > max_index {
                    max_index = index;
                }
                chain = 0;
                end_accessor_run!();
                open_brackets.push(OpenBracket::NotCounted);
                prev_value_end = false;
                prev_was_identifier = false;
            }
            '(' if prev_was_identifier && prev_identifier_was_dotted => {
                // A method call's `(` (`x.f(...)`) -- its link was
                // already counted via `accessor` when `.` was scanned;
                // counting this `(` too would double-count every link
                // of a `.f().f().f()...` chain. Marks the whole
                // *current* accessor run as a method-call run
                // (`end_accessor_run!`'s own doc comment on why the
                // whole run, not just the links from here on, is
                // judged against the tighter cap).
                accessor_run_has_call = true;
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
                if open_brackets.is_empty() {
                    logical_unary = 0;
                }
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
                    index = 0;
                    end_accessor_run!();
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
                    index = 0;
                    end_accessor_run!();
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
                index = 0;
                end_accessor_run!();
                prev_value_end = false;
                prev_was_identifier = false;
            }
            '.' => {
                // Not a float literal's decimal point: the numeric
                // branch above already consumed any `.` directly
                // between two digit runs. A standalone `.` is always a
                // member-access dot (plain select or the lead-in to a
                // method call, counted the same way) -- which of
                // `max_accessor`/`max_method_call` this run's peak
                // belongs to isn't decided here, only once the run ends
                // (`end_accessor_run!`'s own doc comment).
                accessor += 1;
                bump_raw!();
                chain = 0;
                index = 0;
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
    // A run still open when the expression ends (`a.b.c`, with nothing
    // after it to trigger `end_accessor_run!`) still needs committing --
    // no need to also reset, since nothing reads `accessor` again.
    commit_accessor_run!();
    NestingDepths {
        paren_call: max_paren_call,
        list_map: max_list_map,
        chain: max_chain,
        index: max_index,
        logical_unary: max_logical_unary,
        accessor: max_accessor,
        method_call: max_method_call,
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
    fn method_call(expr: &str) -> usize {
        max_nesting_depths(expr).method_call
    }
    fn index(expr: &str) -> usize {
        max_nesting_depths(expr).index
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
        // `.f()` -- the link is counted once via `method_call` (the
        // `.`), not again via the `(`. The whole run belongs to
        // `method_call`, not `accessor`, because it includes a call
        // (`end_accessor_run!`'s own doc comment).
        assert_eq!(paren_call("state.f().f().f()"), 0);
        assert_eq!(accessor("state.f().f().f()"), 0);
        assert_eq!(method_call("state.f().f().f()"), 3);
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
                // The live sum of `paren_call`/`list_map`/`chain`/
                // `accessor` peaks once both the outer group and the
                // `+` inside it are open at once (`paren_call: 2` +
                // `chain: 1`, module docs on why `logical_unary` is
                // excluded) -- not the total number of links scanned.
                raw_combined: 3,
                ..NestingDepths::default()
            }
        );
    }

    #[test]
    fn raw_combined_is_the_deepest_live_sum_not_a_running_total_of_links_scanned() {
        // Regression guard for the bug this field used to have: a long
        // but flat `||` chain, each term resetting `chain`/`accessor`
        // to zero before the next one begins, must never grow
        // `raw_combined` past a handful regardless of how many terms
        // there are -- `||` never recurses in `cel`'s own grammar at
        // all (module docs), so no length of one threatens this
        // crate's own stack.
        let expr = (0..2000)
            .map(|i| format!("a.b == {i}"))
            .collect::<Vec<_>>()
            .join(" || ");
        assert!(max_nesting_depths(&expr).raw_combined < 10);
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
        let expr = format!("state{}", "[0]".repeat(MAX_INDEX_NESTING_DEPTH));
        assert_eq!(index(&expr), MAX_INDEX_NESTING_DEPTH);
        assert_eq!(chain(&expr), 0);
        let expr = format!("state{}", "[0]".repeat(MAX_INDEX_NESTING_DEPTH + 1));
        assert_eq!(index(&expr), MAX_INDEX_NESTING_DEPTH + 1);
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
    fn method_call_chain_counts_once_per_link_via_method_call_not_accessor() {
        let expr = format!("state{}", ".f()".repeat(MAX_METHOD_CALL_NESTING_DEPTH));
        assert_eq!(method_call(&expr), MAX_METHOD_CALL_NESTING_DEPTH);
        assert_eq!(accessor(&expr), 0);
    }

    #[test]
    fn exactly_at_the_limit_and_one_past_it_for_a_method_call_chain() {
        let expr = format!("state{}", ".f()".repeat(MAX_METHOD_CALL_NESTING_DEPTH));
        assert_eq!(method_call(&expr), MAX_METHOD_CALL_NESTING_DEPTH);
        let expr = format!("state{}", ".f()".repeat(MAX_METHOD_CALL_NESTING_DEPTH + 1));
        assert_eq!(method_call(&expr), MAX_METHOD_CALL_NESTING_DEPTH + 1);
    }

    #[test]
    fn a_calls_own_internal_comma_does_not_reset_the_chain_it_is_one_link_of() {
        // Regression guard: `.map(x, x)`'s own `,` must not reset the
        // outer method-call run it is a part of -- every one of the
        // call's own 297 links must still count, not just the first
        // one, even though each link has its own internal `,`.
        let expr = format!(
            "[1]{}",
            ".map(x, x)".repeat(MAX_METHOD_CALL_NESTING_DEPTH + 2)
        );
        assert_eq!(method_call(&expr), MAX_METHOD_CALL_NESTING_DEPTH + 2);
    }

    #[test]
    fn a_single_trailing_call_puts_the_whole_run_in_method_call_not_accessor() {
        // `_dotted_chain` gives up on the *whole* chain the moment any
        // link in it is a call, not just from that point on (module
        // docs) -- a long plain-select prefix followed by one call at
        // the end must count entirely against `method_call`, none of
        // it against `accessor`.
        let expr = format!(
            "state{}.f()",
            ".a".repeat(MAX_METHOD_CALL_NESTING_DEPTH - 1)
        );
        assert_eq!(accessor(&expr), 0);
        assert_eq!(method_call(&expr), MAX_METHOD_CALL_NESTING_DEPTH);
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

    #[test]
    fn mixed_shape_48_nested_size_calls_is_a_known_lenient_divergence() {
        // Known imprecision (module docs): confirmed directly that cof
        // fails this (every single `size(` is a plain global call,
        // `paren_call`'s own concern, individually within its 53 cap --
        // Circuitry's own shared recursion budget still exhausts well
        // under the sum). This crate's own independent per-shape caps
        // accept it; pinned here as a known, accepted lenient gap, not
        // a target to close.
        let expr = format!("{}{{}}{}", "size(".repeat(48), ")".repeat(48));
        assert_eq!(crate::validate_syntax(&expr), Ok(()));
    }

    #[test]
    fn mixed_shape_50_parens_around_48_list_brackets_is_a_known_stricter_divergence() {
        // Known imprecision (module docs): 98 total `expr` grammar
        // productions (50 parens + 48 list brackets) on one path trips
        // `cel`'s own hard-coded 96-production parser guard, even
        // though each half is individually within this crate's own
        // caps (53, 48) -- a compile-time *parse* failure here, where
        // Circuitry itself still compiles this and only fails once it
        // evaluates. Pinned as a known, accepted stricter gap.
        let expr = format!(
            "{}{}1{}{}",
            "(".repeat(50),
            "[".repeat(48),
            "]".repeat(48),
            ")".repeat(50)
        );
        let err = crate::validate_syntax(&expr).unwrap_err();
        assert!(err.is_parse_error());
    }
}
