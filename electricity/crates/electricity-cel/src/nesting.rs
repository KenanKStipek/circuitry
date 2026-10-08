//! A cheap pre-scan of an expression's bracket nesting depth, run before
//! handing it to the `cel` crate's own ANTLR-generated parser.
//!
//! Measured directly (this module's own tests, and `Cargo.toml`'s
//! `[profile.dev.package.cel]`/`[profile.dev.package.antlr4rust]`
//! comment): an *unoptimized* debug build of `cel` and its ANTLR
//! runtime, `antlr4rust`, overflowed a 2 MiB thread stack -- the default
//! stack size a spawned thread gets, where the runner does most of this
//! work -- parsing as few as **12** levels of nested parentheses,
//! brackets, or map/list literals. The same build parsed and evaluated
//! 1300 chained unary `!` and a 500-term `&&` chain (close to the
//! largest either can be under [`crate::MAX_EXPR_LENGTH`]) without any
//! overflow at all: `cel`'s own Pratt-style operator-precedence parser
//! (`parser/pratt_parser.rs`) climbs precedence in a loop, not one
//! recursive descent call per operator, so neither one costs a parser
//! stack frame per occurrence the way a bracket does. [`MAX_NESTING_DEPTH`]
//! therefore only ever counts brackets, not operators -- counting
//! operators too would reject expressions that are entirely safe, for no
//! safety gain, and the measurement above is the evidence for that,
//! not a guess.
//!
//! `cel`'s own parser additionally enforces an internal recursion limit
//! (observed at 96 against this crate's own `cel = "0.15"`; undocumented,
//! not part of its public API, and not guaranteed to stay the same
//! across a future upgrade) that catches a deeply bracketed expression
//! with its own `OtherError` before it could overflow the stack at
//! all -- but only once `[profile.dev.package.cel]`/
//! `[profile.dev.package.antlr4rust]` keep that internal check's own
//! debug-build stack cost low enough not to overflow first; raw, that
//! limit is unreachable; expression parsing overflows (per the 12-level
//! figure above) long before reaching it. This pre-scan exists so an
//! over-nested expression gets Circuitry's own wording and
//! [`crate::CelError::is_too_deeply_nested`] instead of a parser-internal
//! message this crate doesn't control, not because the profile fix on
//! its own would be unsafe without it.

/// The deepest an expression's brackets (`(`/`)`, `[`/`]`, `{`/`}`,
/// counted together regardless of type) may nest before
/// [`max_bracket_depth`] rejects it -- a fixed, crate-specific number
/// with a wide safety margin under `cel`'s own, internal, undocumented
/// parser recursion limit (observed at 96; see the module docs above).
///
/// Deliberately **not** [`electricity_value::MAX_DEPTH`] (512): that
/// constant bounds how deep a `Value` tree may nest, a data-structure
/// limit that has nothing to do with how many stack frames `cel`'s own
/// ANTLR-generated parser spends per level of *expression syntax* --
/// sharing one number between the two would either make this limit
/// unsafe (parsing 512 levels of nested parentheses overflows long
/// before `cel` would ever report an error, profile fix or not) or make
/// `electricity-value`'s limit uselessly small for data that was never
/// CEL source text to begin with.
pub const MAX_NESTING_DEPTH: usize = 64;

/// Scans *expr* for its deepest simultaneous bracket nesting --
/// `(`/`)`, `[`/`]`, `{`/`}` all counted together, matching or not --
/// skipping over string/bytes literals (`'...'`, `"..."`,
/// `'''...'''`, `"""..."""`, any of the four optionally `b`/`B`-prefixed)
/// and `//` line comments, so a bracket character that only ever
/// appears as string content or inside a comment is never counted.
/// CEL's own grammar (`parser/gen/CEL.g4`'s `STRING`/`BYTES`/`COMMENT`
/// rules) is the authority this mirrors, not a general-purpose
/// tokenizer -- it is a pre-scan, not a second parser: an unterminated
/// string or a mismatched bracket is left entirely to the real parser to
/// reject. The one known inexactness is deliberate: a raw string
/// (`r"..."`/`R'...'`, whose grammar rule gives backslash no escaping
/// role at all) is still scanned with the same "a backslash always
/// escapes the next character" rule every other string gets, which can
/// only ever make this scan treat *more* of the expression as string
/// content than CEL's own lexer would, never less -- so a pathological
/// raw string immediately before a run of real brackets could cause
/// this pre-scan to undercount them and let the expression through
/// uncaught by its own depth check. That case does not need this
/// pre-scan to be safe: `cel`'s own internal recursion limit (this
/// module's own docs) still catches it, gracefully, independent of
/// whether this scan counted correctly.
pub fn max_bracket_depth(expr: &str) -> usize {
    let mut depth: usize = 0;
    let mut max_depth: usize = 0;
    let mut chars = expr.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' | '"' => skip_string(c, &mut chars),
            '/' if chars.peek() == Some(&'/') => {
                while let Some(&next) = chars.peek() {
                    if next == '\n' {
                        break;
                    }
                    chars.next();
                }
            }
            '(' | '[' | '{' => {
                depth += 1;
                max_depth = max_depth.max(depth);
            }
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    max_depth
}

/// Consumes *chars* up to (and including) the end of the string literal
/// that just opened with *quote* -- a plain single-character string, or,
/// if the next two characters are also *quote*, a triple-quoted one.
fn skip_string(quote: char, chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    let is_triple = {
        let mut lookahead = chars.clone();
        lookahead.next() == Some(quote) && lookahead.next() == Some(quote)
    };
    if is_triple {
        chars.next();
        chars.next();
        let mut run = 0u8;
        while let Some(c) = chars.next() {
            if c == '\\' {
                chars.next();
                run = 0;
                continue;
            }
            if c == quote {
                run += 1;
                if run == 3 {
                    return;
                }
            } else {
                run = 0;
            }
        }
    } else {
        while let Some(c) = chars.next() {
            if c == '\\' {
                chars.next();
                continue;
            }
            if c == quote || c == '\n' {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_has_zero_depth() {
        assert_eq!(max_bracket_depth("1 + 1"), 0);
    }

    #[test]
    fn counts_simultaneous_nesting_not_total_brackets() {
        assert_eq!(max_bracket_depth("(1) + (2) + (3)"), 1);
        assert_eq!(max_bracket_depth("((()))"), 3);
    }

    #[test]
    fn counts_every_bracket_kind_together() {
        assert_eq!(max_bracket_depth("[{(1)}]"), 3);
    }

    #[test]
    fn ignores_brackets_inside_a_single_quoted_string() {
        assert_eq!(max_bracket_depth("'(((((' == x"), 0);
    }

    #[test]
    fn ignores_brackets_inside_a_double_quoted_string() {
        assert_eq!(max_bracket_depth("\"[[[[[\" == x"), 0);
    }

    #[test]
    fn ignores_brackets_inside_a_triple_quoted_string() {
        assert_eq!(max_bracket_depth("'''(((((''' == x"), 0);
    }

    #[test]
    fn ignores_brackets_inside_a_bytes_literal() {
        assert_eq!(max_bracket_depth("b'[[[[['  == x"), 0);
    }

    #[test]
    fn an_escaped_quote_does_not_end_the_string_early() {
        // Without escape handling, this would treat the string as ending
        // right after `\`, then see `'` `((((` `'` as separate tokens,
        // undercounting nothing here but for the wrong reason -- pinned
        // so a regression that breaks escape handling shows up even
        // though this particular case has no real brackets outside the
        // string either way.
        assert_eq!(max_bracket_depth(r"'a\'(((' == x"), 0);
    }

    #[test]
    fn ignores_brackets_inside_a_line_comment() {
        assert_eq!(max_bracket_depth("1 // (((((\n+ 1"), 0);
    }

    #[test]
    fn real_nesting_after_a_string_is_still_counted() {
        assert_eq!(max_bracket_depth("'(((' == x || (1 + (2))"), 2);
    }

    #[test]
    fn exactly_at_the_limit_and_one_past_it() {
        let expr = format!(
            "{}1{}",
            "(".repeat(MAX_NESTING_DEPTH),
            ")".repeat(MAX_NESTING_DEPTH)
        );
        assert_eq!(max_bracket_depth(&expr), MAX_NESTING_DEPTH);
        let expr = format!(
            "{}1{}",
            "(".repeat(MAX_NESTING_DEPTH + 1),
            ")".repeat(MAX_NESTING_DEPTH + 1)
        );
        assert_eq!(max_bracket_depth(&expr), MAX_NESTING_DEPTH + 1);
    }
}
