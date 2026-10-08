//! A line-for-line port of chevron's tokenizer
//! (`chevron/tokenizer.py`, vendored at `.venv/lib/.../chevron/tokenizer.py`
//! in the Circuitry venv this crate was ported from).
//!
//! Chevron's tokenizer is a plain Mustache lexer; it knows nothing about
//! Circuitry's partial-rejection policy (that's layered on top in `lib.rs`,
//! exactly like `core/templates.py`'s `_reject_partials` wraps chevron's
//! `tokenize()` rather than modifying it). This module stays a faithful
//! port of the upstream behavior, bugs included: an empty tag (`{{}}`)
//! raises the same "index out of range"-shaped error chevron's own
//! `tag[0]` indexing raises, kept in its own [`TokenizeFailure::Index`]
//! variant (as opposed to [`TokenizeFailure::Syntax`], the
//! `ChevronError`-equivalent) because `core/templates.py`'s
//! `render_template` sorts the two into different error messages
//! (`"malformed Mustache template"` vs. `"could not render"`) based on
//! exception *type*, not on how early the failure happened.

/// A tokenized Mustache tag, mirroring chevron's `(tag_type, tag_key)`
/// pairs. `Literal` carries raw template text; every other variant carries
/// the tag's (already-stripped) key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tag {
    Literal(String),
    Variable(String),
    NoEscape(String),
    Section(String),
    InvertedSection(String),
    End(String),
    Partial(String),
    SetDelimiter(String),
}

/// Why tokenizing failed. `Syntax` is chevron's own `ChevronError`
/// (a `SyntaxError` subclass in Python); `Index` is any other exception
/// chevron's tokenizer happens to raise (just `tag[0]` on an empty tag, in
/// practice) -- kept separate because Circuitry's `render_template` wraps
/// the two differently (`lib.rs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TokenizeFailure {
    Syntax(String),
    Index(String),
    /// `{{#.../{{^...` nesting deeper than [`crate::MAX_SECTION_DEPTH`],
    /// carrying the depth reached -- checked here, as each one opens,
    /// rather than once the whole token stream is built, so an
    /// over-nested template is rejected before `render::build_tree`/
    /// `render::render_nodes` would ever see it.
    Depth(usize),
}

/// A tokenize failure, plus every token already pushed before it
/// happened. Chevron's own tokenizer is a *generator*: `_reject_partials`
/// (`lib.rs`) consumes it one token at a time and raises on the first
/// `"partial"` token it sees, before the generator is ever asked to
/// produce another one -- so a partial earlier in the template wins over
/// a syntax error that would only surface later. Carrying the prefix lets
/// `lib.rs` replay that same ordering against this (eagerly-collecting)
/// port without restructuring it into a real generator.
#[derive(Debug)]
pub(crate) struct TokenizeError {
    pub(crate) failure: TokenizeFailure,
    pub(crate) tokens_before_failure: Vec<Tag>,
}

impl TokenizeFailure {
    /// The joined, single-line description every consumer actually prints
    /// (`core/templates.py`'s `_describe`: chevron's own messages span
    /// several lines; this collapses whitespace runs to single spaces,
    /// matching `" ".join(str(exc).split())` exactly).
    pub(crate) fn describe(&self) -> String {
        let raw = match self {
            TokenizeFailure::Syntax(msg) | TokenizeFailure::Index(msg) => msg,
            TokenizeFailure::Depth(depth) => {
                return format!(
                    "template nesting too deep ({depth} levels, max {})",
                    crate::MAX_SECTION_DEPTH
                );
            }
        };
        raw.split_whitespace().collect::<Vec<_>>().join(" ")
    }
}

struct Tokenizer<'a> {
    /// The remaining, not-yet-consumed template text.
    rest: &'a str,
    l_del: String,
    r_del: String,
    current_line: usize,
    last_tag_line: usize,
    open_sections: Vec<String>,
    is_standalone: bool,
    tokens: Vec<Tag>,
}

/// Tokenize a Mustache template with chevron's default behavior: `{{`/`}}`
/// delimiters (overridable in-template via `{{=left right=}}`), standalone
/// whitespace trimming, and section/end-tag balance checking. Does not
/// reject partials -- that's Circuitry's own policy, layered on top (see
/// the module docs).
pub(crate) fn tokenize(template: &str) -> Result<Vec<Tag>, TokenizeError> {
    let mut t = Tokenizer {
        rest: template,
        l_del: "{{".to_string(),
        r_del: "}}".to_string(),
        current_line: 1,
        last_tag_line: 0,
        open_sections: Vec::new(),
        is_standalone: true,
        tokens: Vec::new(),
    };
    match t.run() {
        Ok(()) => Ok(t.tokens),
        Err(failure) => Err(TokenizeError {
            failure,
            tokens_before_failure: t.tokens,
        }),
    }
}

impl<'a> Tokenizer<'a> {
    fn run(&mut self) -> Result<(), TokenizeFailure> {
        while !self.rest.is_empty() {
            let (literal, after_literal) = self.grab_literal();

            if after_literal.is_empty() {
                // The rest of the template is a literal; nothing follows.
                self.tokens.push(Tag::Literal(literal));
                self.rest = "";
                break;
            }
            self.rest = after_literal;

            self.is_standalone = self.l_sa_check(&literal);

            let (tag_type, tag_key) = self.parse_tag()?;

            match tag_type.as_str() {
                "set delimiter" => {
                    let parts: Vec<&str> = tag_key.trim().split(' ').collect();
                    self.l_del = parts[0].to_string();
                    self.r_del = parts[parts.len() - 1].to_string();
                }
                "section" | "inverted section" => {
                    self.open_sections.push(tag_key.clone());
                    self.last_tag_line = self.current_line;
                    if self.open_sections.len() > crate::MAX_SECTION_DEPTH {
                        return Err(TokenizeFailure::Depth(self.open_sections.len()));
                    }
                }
                "end" => {
                    let last_section = self.open_sections.pop().ok_or_else(|| {
                        TokenizeFailure::Syntax(format!(
                            "Trying to close tag \"{tag_key}\"\nLooks like it was not opened.\nline {}",
                            self.current_line + 1
                        ))
                    })?;
                    if tag_key != last_section {
                        return Err(TokenizeFailure::Syntax(format!(
                            "Trying to close tag \"{tag_key}\"\nlast open tag is \"{last_section}\"\nline {}",
                            self.current_line + 1
                        )));
                    }
                }
                _ => {}
            }

            self.is_standalone = self.r_sa_check(&tag_type);

            let mut literal = literal;
            if self.is_standalone {
                // Remove the stuff before the newline, on the right.
                // Chevron: `template.split('\n', 1)[-1]` -- when there is
                // no newline left, that's the *whole* remainder, not ''.
                self.rest = match self.rest.split_once('\n') {
                    Some((_, after)) => after,
                    None => self.rest,
                };
                if tag_type != "partial" {
                    literal = literal.trim_end_matches(' ').to_string();
                }
            }

            if !literal.is_empty() {
                self.tokens.push(Tag::Literal(literal));
            }

            if tag_type != "comment" {
                self.tokens.push(match tag_type.as_str() {
                    "variable" => Tag::Variable(tag_key),
                    "no escape" => Tag::NoEscape(tag_key),
                    "section" => Tag::Section(tag_key),
                    "inverted section" => Tag::InvertedSection(tag_key),
                    "end" => Tag::End(tag_key),
                    "partial" => Tag::Partial(tag_key),
                    "set delimiter" => Tag::SetDelimiter(tag_key),
                    // `no escape?` can survive unconverted only with custom
                    // delimiters whose tag text happens to start with `{`
                    // without actually being a triple-mustache -- chevron
                    // itself then renders it as an inert, unknown tag type
                    // (never a `variable`/`no escape` match in `renderer.py`'s
                    // `elif` chain). Kept as a zero-output literal-ish
                    // no-op here for the same reason.
                    _ => continue,
                });
            }
        }

        if let Some(unclosed) = self.open_sections.last() {
            return Err(TokenizeFailure::Syntax(format!(
                "Unexpected EOF\nthe tag \"{unclosed}\" was never closed\nwas opened at line {}",
                self.last_tag_line
            )));
        }

        Ok(())
    }

    /// `grab_literal`: split on the left delimiter, advancing the line
    /// counter by the literal's own newlines (mirrors chevron's
    /// `_CURRENT_LINE` bookkeeping, including its exact point in the loop).
    ///
    /// An empty left delimiter (reachable only via `{{= =}}`, an empty
    /// set-delimiter tag) is its own case: Python's `str.split('', 1)`
    /// raises `ValueError`, which chevron's `grab_literal` catches as "no
    /// more tags in the template" -- the *entire* rest becomes one
    /// literal, same as running off the end. `str::split_once("")` has no
    /// such failure mode (it matches at position 0), so that case is
    /// special-cased here rather than left to fall out of `split_once`'s
    /// own behavior.
    fn grab_literal(&mut self) -> (String, &'a str) {
        if self.l_del.is_empty() {
            return (self.rest.to_string(), "");
        }
        match self.rest.split_once(self.l_del.as_str()) {
            Some((literal, after)) => {
                self.current_line += literal.matches('\n').count();
                (literal.to_string(), after)
            }
            None => (self.rest.to_string(), ""),
        }
    }

    /// `l_sa_check`: a tag *could* be standalone if the literal before it
    /// ends the line in nothing but whitespace (or the previous tag was
    /// itself standalone and this literal is empty/whitespace-only).
    fn l_sa_check(&self, literal: &str) -> bool {
        if literal.contains('\n') || self.is_standalone {
            let padding = literal.rsplit('\n').next().unwrap_or("");
            is_whitespace_or_empty(padding)
        } else {
            false
        }
    }

    /// `r_sa_check`: final standalone check, looking at what follows the
    /// tag. Variable and no-escape tags can never be standalone.
    fn r_sa_check(&self, tag_type: &str) -> bool {
        if self.is_standalone && tag_type != "variable" && tag_type != "no escape" {
            let first_line = self.rest.split('\n').next().unwrap_or("");
            is_whitespace_or_empty(first_line)
        } else {
            false
        }
    }

    /// `parse_tag`: consume up to the right delimiter and classify the tag.
    fn parse_tag(&mut self) -> Result<(String, String), TokenizeFailure> {
        let (tag_text, after) = match self.rest.split_once(self.r_del.as_str()) {
            Some((tag, after)) => (tag, after),
            None => {
                return Err(TokenizeFailure::Syntax(format!(
                    "unclosed tag at line {}",
                    self.current_line
                )));
            }
        };
        self.rest = after;

        let first = tag_text
            .chars()
            .next()
            .ok_or_else(|| TokenizeFailure::Index("string index out of range".to_string()))?;

        let (mut tag_type, rest_of_tag) = match first {
            '!' => ("comment", &tag_text[first.len_utf8()..]),
            '#' => ("section", &tag_text[first.len_utf8()..]),
            '^' => ("inverted section", &tag_text[first.len_utf8()..]),
            '/' => ("end", &tag_text[first.len_utf8()..]),
            '>' => ("partial", &tag_text[first.len_utf8()..]),
            '=' => ("set delimiter?", &tag_text[first.len_utf8()..]),
            '{' => ("no escape?", &tag_text[first.len_utf8()..]),
            '&' => ("no escape", &tag_text[first.len_utf8()..]),
            _ => ("variable", tag_text),
        };

        let mut owned_tag = rest_of_tag.to_string();

        if tag_type == "set delimiter?" {
            if owned_tag.ends_with('=') {
                tag_type = "set delimiter";
                owned_tag.pop();
            } else {
                return Err(TokenizeFailure::Syntax(format!(
                    "unclosed set delimiter tag\nat line {}",
                    self.current_line
                )));
            }
        } else if tag_type == "no escape?"
            && self.l_del == "{{"
            && self.r_del == "}}"
            && self.rest.starts_with('}')
        {
            self.rest = &self.rest[1..];
            tag_type = "no escape";
        }

        Ok((tag_type.to_string(), owned_tag.trim().to_string()))
    }
}

/// Python's `s.isspace() or s == ''`: `str.isspace()` alone is `False`
/// for an empty string, so chevron's checks always `or` it with an
/// explicit emptiness test. Equivalent to "every character is
/// whitespace", vacuously true for an empty string.
fn is_whitespace_or_empty(s: &str) -> bool {
    s.chars().all(|c| c.is_whitespace())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_variable() {
        let tokens = tokenize("hello {{name}}!").unwrap();
        assert_eq!(
            tokens,
            vec![
                Tag::Literal("hello ".to_string()),
                Tag::Variable("name".to_string()),
                Tag::Literal("!".to_string()),
            ]
        );
    }

    #[test]
    fn unclosed_tag() {
        let err = tokenize("{{a").unwrap_err();
        assert_eq!(err.failure.describe(), "unclosed tag at line 1");
    }

    #[test]
    fn mismatched_close() {
        let err = tokenize("{{#a}}{{/b}}").unwrap_err();
        assert_eq!(
            err.failure.describe(),
            "Trying to close tag \"b\" last open tag is \"a\" line 2"
        );
    }

    #[test]
    fn unopened_close() {
        let err = tokenize("{{/a}}").unwrap_err();
        assert_eq!(
            err.failure.describe(),
            "Trying to close tag \"a\" Looks like it was not opened. line 2"
        );
    }

    #[test]
    fn unclosed_section() {
        let err = tokenize("{{#a}}x").unwrap_err();
        assert_eq!(
            err.failure.describe(),
            "Unexpected EOF the tag \"a\" was never closed was opened at line 1"
        );
    }

    #[test]
    fn empty_tag_is_index_error() {
        let err = tokenize("{{}}").unwrap_err();
        assert!(matches!(err.failure, TokenizeFailure::Index(_)));
        assert_eq!(err.failure.describe(), "string index out of range");
    }

    #[test]
    fn standalone_flag_carries_to_next_tag() {
        // Regression for the standalone flag not being written back to
        // `self.is_standalone`: `{{#items}}` is not itself standalone (it
        // doesn't start its own line), so `{{/items}}` right after it is
        // not standalone either, and the trailing newline survives.
        let tokens = tokenize("{{#items}}{{.}}{{/items}}\nnext").unwrap();
        assert_eq!(
            tokens,
            vec![
                Tag::Section("items".to_string()),
                Tag::Variable(".".to_string()),
                Tag::End("items".to_string()),
                Tag::Literal("\nnext".to_string()),
            ]
        );
    }

    #[test]
    fn standalone_comment_at_eof_keeps_trailing_whitespace() {
        // Regression: a standalone tag with no newline after it keeps
        // whatever trailing text follows instead of dropping it.
        let tokens = tokenize("x\n{{! c }}  ").unwrap();
        assert_eq!(
            tokens,
            vec![
                Tag::Literal("x\n".to_string()),
                Tag::Literal("  ".to_string())
            ]
        );
    }

    #[test]
    fn partial_tag_type() {
        let tokens = tokenize("{{> name}}").unwrap();
        assert_eq!(tokens, vec![Tag::Partial("name".to_string())]);
    }

    #[test]
    fn custom_delimiters() {
        let tokens = tokenize("{{=<% %>=}}<%a%>").unwrap();
        assert_eq!(
            tokens,
            vec![
                Tag::SetDelimiter("<% %>".to_string()),
                Tag::Variable("a".to_string()),
            ]
        );
    }
}
