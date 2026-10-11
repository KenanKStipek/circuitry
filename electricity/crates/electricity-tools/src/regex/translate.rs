//! A Python-`re` pattern -> `fancy-regex` translation layer (#448's
//! decision 7). A construct the layer can't translate fails with the
//! same `regex: invalid pattern: ` prefix as Python's own invalid-
//! pattern error (the caller, [`crate::regex`], adds that prefix;
//! every error text here is compared by *location*, never byte for
//! byte, per issue #479's ruling 3 -- `fancy-regex`'s own parser
//! raises most genuinely malformed syntax itself once the translated
//! text reaches it).
//!
//! `fancy-regex` already parses most of Python's own `re` syntax
//! natively (named groups `(?P<name>...)`, `(?P=name)` backreferences,
//! comments `(?#...)`, atomic groups `(?>...)`, possessive quantifiers
//! `a++`/`a*+`/`a?+`, lookaround, and numbered backreferences), so this
//! translator mostly *passes through* pattern text unchanged. It does
//! its own work only where Python and `fancy-regex` genuinely diverge
//! (DESIGN.md §7.3; issue #479's own "translation layer" list):
//!
//! - `$`/`^` (MULTILINE-dependent; non-MULTILINE `$` also matches
//!   before a final trailing `\n`, which `fancy-regex`'s own `$` does
//!   not);
//! - `\w`/`\s`/`\d`/`\b`/`\B` (Python's own Unicode membership, from
//!   [`crate::generated`]'s generated tables, not Rust's different
//!   ones);
//! - VERBOSE whitespace/comment stripping (Python's own tokenizer
//!   rule: a quantifier brace containing whitespace falls back to
//!   literal characters rather than becoming a working quantifier,
//!   where `fancy-regex`'s own `x` mode would strip the whitespace and
//!   still parse it as one -- so this translator never forwards the
//!   engine's own `x` flag, stripping itself instead);
//! - literal syntax Rust treats specially inside a character class
//!   (`[`, `&&`, `--`, `~~`) or outside one (a `{` that isn't a valid
//!   quantifier, and Python's own `{,n}` shorthand, which `fancy-regex`
//!   doesn't accept and so is normalized to `{0,n}`);
//! - `\0`/octal vs. `\1`-`\99` backreference disambiguation, `\x`/`\u`/
//!   `\U` escapes, and the ASCII-letter-escape validity check;
//! - lookbehind fixed-width (`fancy-regex` is looser than Python here);
//! - `nothing to repeat`/`multiple repeat`/"global flags not at the
//!   start" -- constructs `fancy-regex` would accept but Python
//!   rejects.
//!
//! `\N{NAME}` (a full Unicode character-name database) is a construct
//! this translator can't support yet -- it fails with the same
//! invalid-pattern prefix, a documented divergence (DESIGN.md §7.3).

use crate::generated::regex_unicode;
use std::collections::HashMap;
use std::sync::OnceLock;

/// The five `_FLAG_MAP` flags, already resolved to booleans by the
/// caller (`RegexPlugin::execute`'s own `_resolve_flags`) -- `translate`
/// needs them because `$`, VERBOSE, ASCII and IGNORECASE all change the
/// translation (issue #479's own stub-signature note).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResolvedFlags {
    pub ignorecase: bool,
    pub multiline: bool,
    pub dotall: bool,
    pub verbose: bool,
    pub ascii: bool,
}

/// The translated pattern, plus the group bookkeeping `replace.rs`'s
/// own template parser needs (`\g<name>`/`\g<n>` resolution, and the
/// highest valid numbered backreference).
#[derive(Debug, Clone, Default)]
pub struct Translated {
    pub pattern: String,
    pub group_count: usize,
    pub group_names: HashMap<String, usize>,
}

/// A sub-expression's possible matched length, for the lookbehind
/// fixed-width check (`look-behind requires fixed-width pattern`) --
/// `max: None` means unbounded (e.g. a trailing `*`/`+`/open `{m,}`).
#[derive(Debug, Clone, Copy)]
struct Width {
    min: usize,
    max: Option<usize>,
}

impl Width {
    fn fixed(n: usize) -> Self {
        Width {
            min: n,
            max: Some(n),
        }
    }

    fn unknown() -> Self {
        Width { min: 0, max: None }
    }

    fn is_fixed(&self) -> bool {
        self.max == Some(self.min)
    }

    fn seq(a: Width, b: Width) -> Width {
        Width {
            min: a.min + b.min,
            max: match (a.max, b.max) {
                (Some(x), Some(y)) => Some(x + y),
                _ => None,
            },
        }
    }

    fn alt(a: Width, b: Width) -> Width {
        Width {
            min: a.min.min(b.min),
            max: match (a.max, b.max) {
                (Some(x), Some(y)) => Some(x.max(y)),
                _ => None,
            },
        }
    }

    fn repeat(self, lo: usize, hi: Option<usize>) -> Width {
        let min = self.min.saturating_mul(lo);
        let max = match (self.max, hi) {
            (Some(m), Some(h)) => Some(m.saturating_mul(h)),
            _ => None,
        };
        Width { min, max }
    }
}

/// Whether a following `*`/`+`/`?`/`{m,n}` can apply to this
/// [`Piece`] -- Python's own `nothing to repeat`
/// ([`Repeatable::Never`]: an anchor, comment or flag group, none of
/// which leaves a real node behind even though each is zero-width the
/// same way a lookaround assertion ([`Repeatable::Yes`]) is) vs.
/// `multiple repeat` ([`Repeatable::Already`]: the piece *is* itself
/// an already-applied quantifier).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Repeatable {
    Yes,
    Never,
    Already,
}

/// One translated atom (possibly already quantified) plus its own
/// [`Width`] -- `parse_concat`'s own unit of work.
struct Piece {
    text: String,
    width: Width,
    repeatable: Repeatable,
}

fn atom(text: String, width: Width) -> Piece {
    Piece {
        text,
        width,
        repeatable: Repeatable::Yes,
    }
}

fn non_quantifiable(text: String, width: Width) -> Piece {
    Piece {
        text,
        width,
        repeatable: Repeatable::Never,
    }
}

/// The MULTILINE/VERBOSE/ASCII state in effect at one point in the
/// pattern -- pushed/popped around a scoped flag group (`(?m:...)`,
/// `(?-x:...)`); IGNORECASE is tracked too, kept for symmetry, though
/// nothing here currently reads it from the scope (every ignorecase
/// decision is made once, from the resolved/leading flags, not
/// re-read per scope).
#[derive(Clone, Copy)]
struct Scope {
    multiline: bool,
    verbose: bool,
    ascii: bool,
    ignorecase: bool,
}

struct Translator {
    chars: Vec<char>,
    pos: usize,
    group_count: usize,
    group_names: HashMap<String, usize>,
    /// Each capturing group's own [`Width`], recorded as soon as the
    /// group closes -- a backreference inside a lookbehind
    /// ([`Translator::parse_numeric_escape`]) looks a referenced
    /// group's width up here instead of always widening to
    /// [`Width::unknown`], so `(ab)(?<=\1)` (CPython 3.11 accepts it:
    /// group 1 is fixed-width 2) passes the fixed-width check too.
    group_widths: HashMap<usize, Width>,
    scopes: Vec<Scope>,
}

type PResult<T> = Result<T, String>;

pub fn translate(pattern: &str, flags: ResolvedFlags) -> PResult<Translated> {
    let chars: Vec<char> = pattern.chars().collect();
    let mut t = Translator {
        chars,
        pos: 0,
        group_count: 0,
        group_names: HashMap::new(),
        group_widths: HashMap::new(),
        scopes: vec![Scope {
            multiline: flags.multiline,
            verbose: flags.verbose,
            ascii: flags.ascii,
            ignorecase: flags.ignorecase,
        }],
    };

    let leading = t.consume_leading_global_flags(flags)?;
    t.scopes[0] = Scope {
        multiline: leading.multiline,
        verbose: leading.verbose,
        ascii: leading.ascii,
        ignorecase: leading.ignorecase,
    };

    let mut out = String::new();
    let letters = engine_flag_letters(leading);
    if !letters.is_empty() {
        out.push_str("(?");
        out.push_str(&letters);
        out.push(')');
    }

    let body = t.parse_alternation()?;
    out.push_str(&body.text);

    if t.pos != t.chars.len() {
        return Err(format!("unbalanced parenthesis at position {}", t.pos));
    }

    Ok(Translated {
        pattern: out,
        group_count: t.group_count,
        group_names: t.group_names,
    })
}

/// `m`/`s`/`i` (never `x`/`a`/`u`/`L`, each handled entirely by this
/// translator -- see the module doc comment) for the bare prefix group
/// this module emits once, covering *flags*' own resolved state.
fn engine_flag_letters(flags: ResolvedFlags) -> String {
    let mut letters = String::new();
    if flags.multiline {
        letters.push('m');
    }
    if flags.dotall {
        letters.push('s');
    }
    if flags.ignorecase && !flags.ascii {
        letters.push('i');
    }
    letters
}

/// Python's own VERBOSE whitespace set (F8): `_parser.py`'s own
/// `WHITESPACE` frozenset, `" \t\n\r\v\f"` -- not Rust's broader
/// Unicode `char::is_whitespace()` (confirmed directly: U+00A0 NO-BREAK
/// SPACE stays a literal match target under `(?x)` in CPython).
fn is_py_verbose_whitespace(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{0b}' | '\u{0c}')
}

/// Escapes a literal codepoint for `fancy-regex` output outside a
/// character class -- only the characters that are themselves special
/// there need it; everything else passes through as the bare
/// character (a non-ASCII literal needs no escaping at all).
fn escape_literal(c: char) -> String {
    if "\\^$.|?*+()[]{}".contains(c) {
        format!("\\{c}")
    } else {
        c.to_string()
    }
}

/// Same, inside a `[...]` class -- `fancy-regex` (like Python) only
/// needs `]`, `\\` and a leading `^`/`-` escaped there, but this
/// translator always escapes `-` so it's never misread as a range by
/// accident, and always escapes `[`/`&`/`~` too (literal syntax Rust
/// treats specially -- POSIX classes, `&&`/`~~` set operations).
fn escape_in_class(c: char) -> String {
    if "\\]^[&~-]".contains(c) {
        format!("\\{c}")
    } else {
        c.to_string()
    }
}

/// One token of a parsed character class body, before IGNORECASE
/// expansion ([`expand_case_in_class`]) and rendering -- kept apart
/// from raw text so a `lo-hi` range ([`Translator::parse_class`]'s own
/// `-` detection, F1) and an escape-shortcut's own class text (`\w`,
/// or `[^...]` for a negated `\W`/`\S`/`\D`, F2) can each be told
/// apart from a single literal character that might still need an
/// ASCII or Turkish-I case variant added (F3/F6).
enum ClassToken {
    Char(char),
    Range(char, char),
    Verbatim(String),
}

/// Rejects `hi < lo` the way CPython's own `_parser.py::_class_escape`
/// does (`bad character range`, F1) before recording *lo*-*hi*.
fn push_range(tokens: &mut Vec<ClassToken>, lo: char, hi: char, pos: usize) -> PResult<()> {
    if (hi as u32) < (lo as u32) {
        return Err(format!("bad character range {lo}-{hi} at position {pos}"));
    }
    tokens.push(ClassToken::Range(lo, hi));
    Ok(())
}

/// `c`'s ASCII-only ignorecase counterpart (`a`<->`A`, else `None`) --
/// the ASCII half of F3/F6's shared expansion mechanism: under the
/// `ASCII` flag, a literal or class member gets this variant added
/// explicitly instead of relying on `fancy-regex`'s own `i` flag
/// (which folds every Unicode case pair, not just ASCII ones).
fn ascii_swap_case(c: char) -> Option<char> {
    if c.is_ascii_lowercase() {
        Some(c.to_ascii_uppercase())
    } else if c.is_ascii_uppercase() {
        Some(c.to_ascii_lowercase())
    } else {
        None
    }
}

/// The swapped-case sub-range(s) of *lo*-*hi* that overlap `a-z`/`A-Z`
/// -- e.g. `[a-z]` under ASCII IGNORECASE also needs `A-Z` added so the
/// range matches both cases ([`expand_case_in_class`]'s own range arm).
fn ascii_swap_range(lo: char, hi: char) -> Vec<(char, char)> {
    let (lo_u, hi_u) = (lo as u32, hi as u32);
    let delta = b'a' as u32 - b'A' as u32; // 32
    let mut out = Vec::new();
    let (a, z) = (b'a' as u32, b'z' as u32);
    let (ov_lo, ov_hi) = (lo_u.max(a), hi_u.min(z));
    if ov_lo <= ov_hi {
        out.push((
            char::from_u32(ov_lo - delta).unwrap(),
            char::from_u32(ov_hi - delta).unwrap(),
        ));
    }
    let (ca, cz) = (b'A' as u32, b'Z' as u32);
    let (ov_lo2, ov_hi2) = (lo_u.max(ca), hi_u.min(cz));
    if ov_lo2 <= ov_hi2 {
        out.push((
            char::from_u32(ov_lo2 + delta).unwrap(),
            char::from_u32(ov_hi2 + delta).unwrap(),
        ));
    }
    out
}

/// The *other* members of CPython's own Turkish-I ignorecase special
/// case (`_sre`'s own extra fold, confirmed against 3.11 directly) --
/// `i`/`I`/`ı`(U+0131)/`İ`(U+0130) all fold together under CPython,
/// where `fancy-regex`'s own Unicode folding keeps the Turkish pair
/// apart from plain `i`/`I` (the one documented divergence this
/// module's own doc comment used to list -- F6 turns it into a parity
/// case by expanding explicitly here instead).
fn turkish_i_variants(c: char) -> Option<[char; 3]> {
    match c {
        'i' => Some(['I', 'ı', 'İ']),
        'I' => Some(['i', 'ı', 'İ']),
        'ı' => Some(['i', 'I', 'İ']),
        'İ' => Some(['i', 'I', 'ı']),
        _ => None,
    }
}

/// A literal character's own translated text *outside* a class, under
/// the current scope's IGNORECASE/ASCII state (F3/F6) -- ASCII mode
/// expands to an explicit `[cC]` class (since `fancy-regex`'s engine
/// `i` flag is never emitted there, [`engine_flag_letters`]'s own doc
/// comment); non-ASCII mode leaves everything to the engine's own `i`
/// flag except the four Turkish-I code points, expanded the same way.
fn literal_outside_class(c: char, scope: Scope) -> String {
    if scope.ignorecase {
        if scope.ascii {
            if let Some(sw) = ascii_swap_case(c) {
                let mut pair = [c, sw];
                pair.sort_unstable();
                return format!("[{}{}]", escape_in_class(pair[0]), escape_in_class(pair[1]));
            }
        } else if let Some(variants) = turkish_i_variants(c) {
            let mut all = vec![c];
            all.extend(variants);
            all.sort_unstable();
            let body: String = all.iter().map(|&x| escape_in_class(x)).collect();
            return format!("[{body}]");
        }
    }
    escape_literal(c)
}

/// `\w`/`\s`/`\d` (or their negated forms) *outside* a class, as
/// `fancy-regex` text -- guarded with a local `(?-i:...)` (F8) when
/// IGNORECASE is on and ASCII isn't, so the engine's own global `i`
/// flag can't additionally case-fold this already-exhaustive Unicode
/// range (confirmed directly: without the guard, `\w` under
/// IGNORECASE alone starts matching U+0345 COMBINING GREEK
/// YPOGEGRAMMENI, which CPython's `\w` never does). Not available for
/// the same shortcut nested *inside* a user's own class
/// ([`Translator::read_class_escape`]'s own `Verbatim` tokens) --
/// `(?-i:...)` is a group, which can't appear inside `[...]`; that
/// case stays a documented residual gap (DESIGN.md §7.3).
fn unicode_class_text(body: &str, negate: bool, scope: Scope) -> String {
    let class = if negate {
        format!("[^{body}]")
    } else {
        format!("[{body}]")
    };
    if scope.ignorecase && !scope.ascii {
        format!("(?-i:{class})")
    } else {
        class
    }
}

/// Adds each token's own ASCII or Turkish-I case variant(s) in place
/// (F3/F6) -- called once per class, after the whole body is parsed,
/// so a variant added for one token never gets *re*-expanded as if it
/// were itself part of the original class text.
fn expand_case_in_class(tokens: &mut Vec<ClassToken>, ascii: bool) {
    let mut extra = Vec::new();
    for tok in tokens.iter() {
        match tok {
            ClassToken::Char(c) => {
                if ascii {
                    if let Some(sw) = ascii_swap_case(*c) {
                        extra.push(ClassToken::Char(sw));
                    }
                } else if let Some(variants) = turkish_i_variants(*c) {
                    extra.extend(variants.into_iter().map(ClassToken::Char));
                }
            }
            ClassToken::Range(lo, hi) => {
                if ascii {
                    for (swlo, swhi) in ascii_swap_range(*lo, *hi) {
                        extra.push(ClassToken::Range(swlo, swhi));
                    }
                }
            }
            ClassToken::Verbatim(_) => {}
        }
    }
    tokens.extend(extra);
}

impl Translator {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<char> {
        self.chars.get(self.pos + offset).copied()
    }

    fn advance(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn scope(&self) -> Scope {
        *self.scopes.last().expect("scope stack is never empty")
    }

    /// Consumes every leading bare `(?[aiLmsux]+)` flag group
    /// (contiguous from position 0 -- Python 3.11's own "global flags
    /// not at the start of the expression" rule means a *later* one is
    /// always an error, checked in [`Translator::parse_flag_group`],
    /// never here), folding each into *flags*.
    fn consume_leading_global_flags(&mut self, mut flags: ResolvedFlags) -> PResult<ResolvedFlags> {
        loop {
            let start = self.pos;
            if self.peek() != Some('(') || self.peek_at(1) != Some('?') {
                return Ok(flags);
            }
            let mut i = self.pos + 2;
            let mut letters = String::new();
            while let Some(c) = self.chars.get(i) {
                if "aiLmsux".contains(*c) {
                    letters.push(*c);
                    i += 1;
                } else {
                    break;
                }
            }
            if letters.is_empty() || self.chars.get(i) != Some(&')') {
                return Ok(flags);
            }
            self.pos = i + 1;
            apply_global_letters(&letters, &mut flags, start)?;
        }
    }

    /// Strips VERBOSE whitespace and `#...`-to-end-of-line comments,
    /// outside a character class, between top-level tokens -- never
    /// called from inside `[...]` parsing, and never applied to the
    /// digits inside a `{m,n}` quantifier brace, matching Python's own
    /// tokenizer exactly (`re/_parser.py::_parse`): a brace containing
    /// whitespace falls back to literal characters, each separately
    /// eligible for stripping by the *next* pass through this
    /// function, rather than becoming a working quantifier the way
    /// `fancy-regex`'s own `x` mode would.
    fn skip_verbose_ws(&mut self) {
        if !self.scope().verbose {
            return;
        }
        loop {
            match self.peek() {
                Some(c) if is_py_verbose_whitespace(c) => {
                    self.advance();
                }
                Some('#') => {
                    while let Some(c) = self.peek() {
                        self.advance();
                        if c == '\n' {
                            break;
                        }
                    }
                }
                _ => break,
            }
        }
    }

    fn parse_alternation(&mut self) -> PResult<Piece> {
        let mut text = String::new();
        let mut width: Option<Width> = None;
        loop {
            let branch = self.parse_concat()?;
            width = Some(match width {
                None => branch.width,
                Some(w) => Width::alt(w, branch.width),
            });
            text.push_str(&branch.text);
            self.skip_verbose_ws();
            if self.peek() == Some('|') {
                self.advance();
                text.push('|');
                continue;
            }
            break;
        }
        Ok(atom(text, width.unwrap_or_else(|| Width::fixed(0))))
    }

    fn parse_concat(&mut self) -> PResult<Piece> {
        let mut pieces: Vec<Piece> = Vec::new();
        loop {
            self.skip_verbose_ws();
            match self.peek() {
                None | Some('|') | Some(')') => break,
                Some('^') => {
                    self.advance();
                    pieces.push(non_quantifiable("^".to_string(), Width::fixed(0)));
                }
                Some('$') => {
                    self.advance();
                    let text = if self.scope().multiline {
                        "$".to_string()
                    } else {
                        "(?=\\n?\\z)".to_string()
                    };
                    pieces.push(non_quantifiable(text, Width::fixed(0)));
                }
                Some('.') => {
                    self.advance();
                    pieces.push(atom(".".to_string(), Width::fixed(1)));
                }
                Some('(') => {
                    // `(?#...)` (F8) is fully transparent to the
                    // surrounding pattern -- Python's own parser never
                    // leaves a node behind for it, so `a(?#c)*` means
                    // `a*`, not "nothing to repeat" against the
                    // comment. `parse_group` reports one with empty
                    // text and a `Never` [`Repeatable`] (nothing else
                    // does both); skip pushing it at all rather than
                    // leaving it on `pieces` for the next quantifier to
                    // trip over.
                    let piece = self.parse_group()?;
                    if !(piece.text.is_empty() && piece.repeatable == Repeatable::Never) {
                        pieces.push(piece);
                    }
                }
                Some('[') => {
                    let piece = self.parse_class()?;
                    pieces.push(piece);
                }
                Some('\\') => {
                    let piece = self.parse_escape()?;
                    pieces.push(piece);
                }
                Some(ch @ ('*' | '+' | '?')) => {
                    let start = self.pos;
                    self.advance();
                    self.apply_quantifier(&mut pieces, ch, start)?;
                }
                Some('{') => {
                    if let Some((lo, hi, brace_text, consumed)) =
                        self.try_parse_quantifier_braces()?
                    {
                        let start = self.pos;
                        self.pos += consumed;
                        self.apply_brace_quantifier(&mut pieces, lo, hi, &brace_text, start)?;
                    } else {
                        self.advance();
                        pieces.push(atom(
                            literal_outside_class('{', self.scope()),
                            Width::fixed(1),
                        ));
                    }
                }
                Some(other) => {
                    self.advance();
                    pieces.push(atom(
                        literal_outside_class(other, self.scope()),
                        Width::fixed(1),
                    ));
                }
            }
        }
        let mut width = Width::fixed(0);
        let mut text = String::new();
        for p in &pieces {
            width = Width::seq(width, p.width);
            text.push_str(&p.text);
        }
        Ok(atom(text, width))
    }

    fn check_repeatable(last: &Piece, pos: usize) -> PResult<()> {
        match last.repeatable {
            Repeatable::Yes => Ok(()),
            Repeatable::Never => Err(format!("nothing to repeat at position {pos}")),
            Repeatable::Already => Err(format!("multiple repeat at position {pos}")),
        }
    }

    fn apply_quantifier(&mut self, pieces: &mut Vec<Piece>, ch: char, pos: usize) -> PResult<()> {
        let last = pieces
            .pop()
            .ok_or_else(|| format!("nothing to repeat at position {pos}"))?;
        Self::check_repeatable(&last, pos)?;
        let mut text = last.text;
        text.push(ch);
        let suffix = self.consume_quantifier_suffix();
        text.push_str(suffix);
        let (lo, hi) = match ch {
            '*' => (0, None),
            '+' => (1, None),
            '?' => (0, Some(1)),
            _ => unreachable!(),
        };
        pieces.push(Piece {
            text,
            width: last.width.repeat(lo, hi),
            repeatable: Repeatable::Already,
        });
        Ok(())
    }

    fn apply_brace_quantifier(
        &mut self,
        pieces: &mut Vec<Piece>,
        lo: usize,
        hi: Option<usize>,
        brace_text: &str,
        pos: usize,
    ) -> PResult<()> {
        let last = pieces
            .pop()
            .ok_or_else(|| format!("nothing to repeat at position {pos}"))?;
        Self::check_repeatable(&last, pos)?;
        let mut text = last.text;
        text.push_str(brace_text);
        let suffix = self.consume_quantifier_suffix();
        text.push_str(suffix);
        pieces.push(Piece {
            text,
            width: last.width.repeat(lo, hi),
            repeatable: Repeatable::Already,
        });
        Ok(())
    }

    /// `?` (lazy) or `+` (possessive, `fancy-regex`'s own native
    /// syntax -- see the module doc comment) directly following a base
    /// quantifier.
    fn consume_quantifier_suffix(&mut self) -> &'static str {
        match self.peek() {
            Some('?') => {
                self.advance();
                "?"
            }
            Some('+') => {
                self.advance();
                "+"
            }
            _ => "",
        }
    }

    /// `{m}`, `{m,}`, `{,n}` or `{m,n}` at the current position,
    /// starting exactly at `{` -- Python's own tokenizer rule: no
    /// whitespace tolerated anywhere inside, even under VERBOSE (the
    /// module doc comment). Returns the *normalized* brace text
    /// (`{,n}` becomes `{0,n}`, since `fancy-regex` doesn't accept
    /// Python's own shorthand); a non-match leaves `self.pos` untouched
    /// so the caller treats `{` as a literal instead.
    #[allow(clippy::type_complexity)]
    fn try_parse_quantifier_braces(
        &self,
    ) -> PResult<Option<(usize, Option<usize>, String, usize)>> {
        let mut i = self.pos + 1;
        let lo_start = i;
        while self.chars.get(i).is_some_and(|c| c.is_ascii_digit()) {
            i += 1;
        }
        let lo_text: String = self.chars[lo_start..i].iter().collect();
        let has_comma = self.chars.get(i) == Some(&',');
        if has_comma {
            i += 1;
        }
        let hi_start = i;
        while self.chars.get(i).is_some_and(|c| c.is_ascii_digit()) {
            i += 1;
        }
        let hi_text: String = self.chars[hi_start..i].iter().collect();
        if self.chars.get(i) != Some(&'}') {
            return Ok(None);
        }
        // `{}` (no comma, no digits at all) isn't a quantifier in
        // Python either; `{,}` (a bare comma, F8) is -- CPython's own
        // tokenizer accepts it as `{0,}` (`a*`).
        if !has_comma && lo_text.is_empty() {
            return Ok(None);
        }
        let Some(lo) = (if lo_text.is_empty() {
            Some(0)
        } else {
            lo_text.parse().ok()
        }) else {
            return Ok(None);
        };
        let hi: Option<usize> = if hi_text.is_empty() {
            if has_comma { None } else { Some(lo) }
        } else {
            match hi_text.parse().ok() {
                Some(h) => Some(h),
                None => return Ok(None),
            }
        };
        // `a{3,2}` (F8): CPython rejects a backwards range outright,
        // rather than falling back to literal `{3,2}` text.
        if let Some(h) = hi {
            if h < lo {
                return Err(format!(
                    "min repeat greater than max repeat at position {lo_start}"
                ));
            }
        }
        let normalized = match hi {
            None => format!("{{{lo},}}"),
            Some(h) => {
                if has_comma {
                    format!("{{{lo},{h}}}")
                } else {
                    format!("{{{lo}}}")
                }
            }
        };
        let consumed = i + 1 - self.pos;
        Ok(Some((lo, hi, normalized, consumed)))
    }

    fn parse_class(&mut self) -> PResult<Piece> {
        let start = self.pos;
        self.advance(); // '['
        let negate = self.peek() == Some('^');
        if negate {
            self.advance();
        }
        let mut tokens: Vec<ClassToken> = Vec::new();
        let mut first = true;
        loop {
            match self.peek() {
                None => {
                    return Err(format!("unterminated character set at position {start}"));
                }
                Some(']') if !first => {
                    self.advance();
                    break;
                }
                Some('\\') => {
                    let lo_pos = self.pos;
                    self.advance();
                    let tok = self.read_class_escape()?;
                    match tok {
                        ClassToken::Char(lo) if self.at_range_dash() => {
                            self.advance(); // '-'
                            let hi = self.read_class_range_endpoint(lo_pos)?;
                            push_range(&mut tokens, lo, hi, lo_pos)?;
                        }
                        other => tokens.push(other),
                    }
                }
                Some(c) => {
                    self.advance();
                    if c != '-' && self.at_range_dash() {
                        let lo_pos = self.pos - 1;
                        self.advance(); // '-'
                        let hi = self.read_class_range_endpoint(lo_pos)?;
                        push_range(&mut tokens, c, hi, lo_pos)?;
                    } else {
                        tokens.push(ClassToken::Char(c));
                    }
                }
            }
            first = false;
        }
        if self.scope().ignorecase {
            expand_case_in_class(&mut tokens, self.scope().ascii);
        }
        let mut body = String::new();
        if negate {
            body.push('^');
        }
        for tok in &tokens {
            match tok {
                ClassToken::Char(c) => body.push_str(&escape_in_class(*c)),
                ClassToken::Range(lo, hi) => {
                    body.push_str(&escape_in_class(*lo));
                    body.push('-');
                    body.push_str(&escape_in_class(*hi));
                }
                ClassToken::Verbatim(s) => body.push_str(s),
            }
        }
        Ok(atom(format!("[{body}]"), Width::fixed(1)))
    }

    /// True when the char immediately ahead is a range dash: a `-`
    /// that is neither the class's own closing `]` nor past the end
    /// of the pattern (F1) -- Python treats a `-` right before `]`,
    /// or at the very end of input, as a literal hyphen instead.
    fn at_range_dash(&self) -> bool {
        self.peek() == Some('-') && self.peek_at(1).is_some() && self.peek_at(1) != Some(']')
    }

    /// The upper bound of a `lo-hi` class range (F1), right after the
    /// `-` has been consumed -- a shortcut escape (`\w` and friends)
    /// can't be a range endpoint in Python either (`bad character
    /// range`), so only [`ClassToken::Char`] is accepted here.
    fn read_class_range_endpoint(&mut self, start: usize) -> PResult<char> {
        match self.peek() {
            Some('\\') => {
                self.advance();
                match self.read_class_escape()? {
                    ClassToken::Char(c) => Ok(c),
                    _ => Err(format!("bad character range at position {start}")),
                }
            }
            Some(c) => {
                self.advance();
                Ok(c)
            }
            None => Err(format!("unterminated character set at position {start}")),
        }
    }

    /// `\w`/`\s`/`\d`/`\W`/`\S`/`\D`, `\b` (a literal backspace
    /// *inside* a class, unlike the word-boundary assertion outside
    /// one, F8), an octal escape (F8: any digit 0-7 starts one inside
    /// a class -- there's no backreference reading here, unlike
    /// outside one) and every other literal escape *inside* a class --
    /// the shortcut classes expand to the same explicit Unicode ranges
    /// [`Translator::parse_escape`] uses outside one; a negated
    /// shortcut (`\W`/`\S`/`\D`, F2) is wrapped as its own nested
    /// `[^...]` class via `fancy-regex`'s class-union support, rather
    /// than the un-negated `[...]` this translator used to emit.
    fn read_class_escape(&mut self) -> PResult<ClassToken> {
        let backslash_pos = self.pos - 1;
        let c = self
            .advance()
            .ok_or_else(|| format!("bad escape (end of pattern) at position {backslash_pos}"))?;
        match c {
            'w' => Ok(ClassToken::Verbatim(class_body_word(self.scope().ascii))),
            'W' => Ok(ClassToken::Verbatim(format!(
                "[^{}]",
                class_body_word(self.scope().ascii)
            ))),
            's' => Ok(ClassToken::Verbatim(class_body_space(self.scope().ascii))),
            'S' => Ok(ClassToken::Verbatim(format!(
                "[^{}]",
                class_body_space(self.scope().ascii)
            ))),
            'd' => Ok(ClassToken::Verbatim(class_body_digit(self.scope().ascii))),
            'D' => Ok(ClassToken::Verbatim(format!(
                "[^{}]",
                class_body_digit(self.scope().ascii)
            ))),
            'b' => Ok(ClassToken::Char('\u{08}')),
            '0'..='7' => {
                let ch = self.read_octal_from(c, backslash_pos)?;
                Ok(ClassToken::Char(ch))
            }
            '8' | '9' => Err(format!("bad escape \\{c} at position {backslash_pos}")),
            other => {
                let (ch, _) = self.decode_escape_char(other)?;
                Ok(ClassToken::Char(ch))
            }
        }
    }

    /// `\0`-`\7` followed by up to two more octal digits (F8) --
    /// shared by the outside-class `\0` reading
    /// ([`Translator::parse_escape`]) and every class digit escape
    /// ([`Translator::read_class_escape`]), since both follow the same
    /// CPython rule once the leading digit's own backreference-vs-
    /// octal disambiguation (only relevant outside a class) is past.
    fn read_octal_from(&mut self, first: char, backslash_pos: usize) -> PResult<char> {
        let mut digits = String::new();
        digits.push(first);
        for _ in 0..2 {
            if self.peek().is_some_and(|c| ('0'..='7').contains(&c)) {
                digits.push(self.advance().unwrap());
            } else {
                break;
            }
        }
        let value = u32::from_str_radix(&digits, 8).unwrap_or(0);
        if value > 0o377 {
            return Err(format!(
                "octal escape value \\{digits} outside of range 0-0o377 at position {backslash_pos}"
            ));
        }
        Ok(char::from_u32(value).unwrap_or('\0'))
    }

    /// One `(...)` group, including every extension form -- `(?:...)`,
    /// `(?P<name>...)`, `(?P=name)`, `(?#...)`, `(?=...)`/`(?!...)`/
    /// `(?<=...)`/`(?<!...)`, `(?>...)`, `(?(id)yes|no)`, and a bare or
    /// scoped inline-flag group. A plain capturing group gets this
    /// translator's own number, matching Python's left-to-right
    /// assignment.
    fn parse_group(&mut self) -> PResult<Piece> {
        let open_pos = self.pos;
        self.advance(); // '('
        if self.peek() != Some('?') {
            self.group_count += 1;
            let number = self.group_count;
            let inner = self.parse_alternation()?;
            self.expect_close(open_pos)?;
            self.group_widths.insert(number, inner.width);
            return Ok(atom(format!("({})", inner.text), inner.width));
        }
        // '?'
        let after_q = self.pos + 1;
        match self.chars.get(after_q) {
            Some(':') => {
                self.pos = after_q + 1;
                let inner = self.parse_alternation()?;
                self.expect_close(open_pos)?;
                Ok(atom(format!("(?:{})", inner.text), inner.width))
            }
            Some('#') => {
                self.pos = after_q + 1;
                loop {
                    match self.advance() {
                        None => {
                            return Err(format!(
                                "missing ), unterminated comment at position {open_pos}"
                            ));
                        }
                        Some(')') => break,
                        Some(_) => {}
                    }
                }
                Ok(non_quantifiable(String::new(), Width::fixed(0)))
            }
            Some('=') => {
                self.pos = after_q + 1;
                let inner = self.parse_alternation()?;
                self.expect_close(open_pos)?;
                Ok(atom(format!("(?={})", inner.text), Width::fixed(0)))
            }
            Some('!') => {
                self.pos = after_q + 1;
                let inner = self.parse_alternation()?;
                self.expect_close(open_pos)?;
                Ok(atom(format!("(?!{})", inner.text), Width::fixed(0)))
            }
            Some('>') => {
                self.pos = after_q + 1;
                let inner = self.parse_alternation()?;
                self.expect_close(open_pos)?;
                Ok(atom(format!("(?>{})", inner.text), inner.width))
            }
            Some('P') => self.parse_group_p(open_pos, after_q),
            Some('<') => self.parse_lookbehind(open_pos, after_q),
            Some('(') => self.parse_conditional(open_pos, after_q),
            _ => self.parse_flag_group(open_pos, after_q),
        }
    }

    fn expect_close(&mut self, open_pos: usize) -> PResult<()> {
        if self.peek() == Some(')') {
            self.advance();
            Ok(())
        } else {
            Err(format!(
                "missing ), unterminated subpattern at position {open_pos}"
            ))
        }
    }

    fn parse_group_p(&mut self, open_pos: usize, after_q: usize) -> PResult<Piece> {
        if self.chars.get(after_q + 1) == Some(&'<') {
            let name_start = after_q + 2;
            let mut i = name_start;
            while self
                .chars
                .get(i)
                .is_some_and(|c| c.is_alphanumeric() || *c == '_')
            {
                i += 1;
            }
            let name: String = self.chars[name_start..i].iter().collect();
            let starts_like_identifier = name
                .chars()
                .next()
                .is_some_and(|c| c.is_alphabetic() || c == '_');
            if i == name_start || !starts_like_identifier || self.chars.get(i) != Some(&'>') {
                return Err(format!(
                    "bad character in group name at position {name_start}"
                ));
            }
            // F8: Python rejects a repeated group name outright,
            // rather than silently overwriting the earlier mapping.
            if let Some(&existing) = self.group_names.get(&name) {
                return Err(format!(
                    "redefinition of group name '{name}' as group {}; was group {existing} at position {name_start}",
                    self.group_count + 1
                ));
            }
            self.group_count += 1;
            let number = self.group_count;
            self.group_names.insert(name, number);
            self.pos = i + 1;
            let inner = self.parse_alternation()?;
            self.expect_close(open_pos)?;
            self.group_widths.insert(number, inner.width);
            return Ok(atom(format!("({})", inner.text), inner.width));
        }
        if self.chars.get(after_q + 1) == Some(&'=') {
            let name_start = after_q + 2;
            let mut i = name_start;
            while self
                .chars
                .get(i)
                .is_some_and(|c| c.is_alphanumeric() || *c == '_')
            {
                i += 1;
            }
            if i == name_start || self.chars.get(i) != Some(&')') {
                return Err(format!(
                    "missing ), unterminated name at position {name_start}"
                ));
            }
            let name: String = self.chars[name_start..i].iter().collect();
            let number = *self
                .group_names
                .get(&name)
                .ok_or_else(|| format!("unknown group name {name:?} at position {name_start}"))?;
            self.pos = i + 1;
            return Ok(atom(format!("\\{number}"), Width::unknown()));
        }
        Err(format!("unknown extension ?P at position {open_pos}"))
    }

    fn parse_lookbehind(&mut self, open_pos: usize, after_q: usize) -> PResult<Piece> {
        let (negate, body_start) = match self.chars.get(after_q + 1) {
            Some('=') => (false, after_q + 2),
            Some('!') => (true, after_q + 2),
            _ => return Err(format!("unknown extension ?< at position {open_pos}")),
        };
        self.pos = body_start;
        let inner = self.parse_alternation()?;
        self.expect_close(open_pos)?;
        if !inner.width.is_fixed() {
            return Err(format!(
                "look-behind requires fixed-width pattern at position {open_pos}"
            ));
        }
        let marker = if negate { "!" } else { "=" };
        Ok(atom(format!("(?<{marker}{})", inner.text), Width::fixed(0)))
    }

    /// `(?(id)yes|no)` / `(?(name)yes|no)` -- passed through to
    /// `fancy-regex`'s own native conditional support, with a bare
    /// group name resolved to its number first (`fancy-regex` only
    /// recognizes a numbered backreference as a condition).
    fn parse_conditional(&mut self, open_pos: usize, after_q: usize) -> PResult<Piece> {
        let id_start = after_q + 1;
        let mut i = id_start;
        while self.chars.get(i).is_some_and(|c| *c != ')') {
            i += 1;
        }
        if self.chars.get(i) != Some(&')') {
            return Err(format!(
                "missing ), unterminated name at position {id_start}"
            ));
        }
        let id_text: String = self.chars[id_start..i].iter().collect();
        let number = if let Ok(n) = id_text.parse::<usize>() {
            n
        } else {
            *self
                .group_names
                .get(&id_text)
                .ok_or_else(|| format!("unknown group name {id_text:?} at position {id_start}"))?
        };
        self.pos = i + 1;
        let yes = self.parse_concat()?;
        let (no_text, no_width) = if self.peek() == Some('|') {
            self.advance();
            let no = self.parse_concat()?;
            (format!("|{}", no.text), Some(no.width))
        } else {
            (String::new(), None)
        };
        self.expect_close(open_pos)?;
        let width = match no_width {
            Some(nw) => Width::alt(yes.width, nw),
            None => Width::alt(yes.width, Width::fixed(0)),
        };
        Ok(atom(format!("(?({number}){}{})", yes.text, no_text), width))
    }

    /// A bare `(?flags)` or scoped `(?flags-flags:...)`/`(?flags:...)`
    /// group that isn't one of the other extension forms -- only valid
    /// mid-pattern in its *scoped* form (a bare one here means the
    /// leading-prefix scan in [`Translator::consume_leading_global_flags`]
    /// already ran, so anything reaching here is past the start).
    fn parse_flag_group(&mut self, open_pos: usize, after_q: usize) -> PResult<Piece> {
        let mut i = after_q;
        let mut add = String::new();
        while self.chars.get(i).is_some_and(|c| "aiLmsux".contains(*c)) {
            add.push(self.chars[i]);
            i += 1;
        }
        let mut remove = String::new();
        if self.chars.get(i) == Some(&'-') {
            i += 1;
            while self.chars.get(i).is_some_and(|c| "imsx".contains(*c)) {
                remove.push(self.chars[i]);
                i += 1;
            }
            if remove.is_empty() {
                return Err(format!("missing flag at position {i}"));
            }
        }
        match self.chars.get(i) {
            Some(')') if remove.is_empty() => {
                if add.contains('L') {
                    return Err(format!(
                        "bad inline flags: cannot use 'L' flag with a str pattern at position {i}"
                    ));
                }
                Err(format!(
                    "global flags not at the start of the expression at position {open_pos}"
                ))
            }
            Some(':') => {
                i += 1;
                if add.contains('L') {
                    return Err(format!(
                        "bad inline flags: cannot use 'L' flag with a str pattern at position {i}"
                    ));
                }
                let mut scope = self.scope();
                if add.contains('x') {
                    scope.verbose = true;
                }
                if remove.contains('x') {
                    scope.verbose = false;
                }
                if add.contains('a') {
                    scope.ascii = true;
                }
                if add.contains('m') {
                    scope.multiline = true;
                }
                if remove.contains('m') {
                    scope.multiline = false;
                }
                if add.contains('i') {
                    scope.ignorecase = true;
                }
                if remove.contains('i') {
                    scope.ignorecase = false;
                }
                // F3's own "Related" bug: forwarding a scoped `i`
                // to the engine under ASCII brings back full-Unicode
                // folding for this sub-expression; ASCII-mode case
                // handling is this translator's own job
                // ([`literal_outside_class`]/[`expand_case_in_class`]),
                // never the engine's `i` flag.
                let engine_add: String = add
                    .chars()
                    .filter(|c| "ims".contains(*c) && !(*c == 'i' && scope.ascii))
                    .collect();
                let engine_remove: String = remove.chars().filter(|c| "ims".contains(*c)).collect();
                self.pos = i;
                self.scopes.push(scope);
                let inner = self.parse_alternation()?;
                self.scopes.pop();
                self.expect_close(open_pos)?;
                let mut flag_text = String::from("?");
                flag_text.push_str(&engine_add);
                if !engine_remove.is_empty() {
                    flag_text.push('-');
                    flag_text.push_str(&engine_remove);
                }
                Ok(atom(format!("({flag_text}:{})", inner.text), inner.width))
            }
            Some(')') => Err(format!("missing : at position {i}")),
            _ => Err(format!("unknown extension at position {open_pos}")),
        }
    }

    /// One `\...` escape outside a character class.
    fn parse_escape(&mut self) -> PResult<Piece> {
        let backslash_pos = self.pos;
        self.advance(); // consume '\'
        let c = self
            .advance()
            .ok_or_else(|| format!("bad escape (end of pattern) at position {backslash_pos}"))?;
        match c {
            'A' => Ok(non_quantifiable("\\A".to_string(), Width::fixed(0))),
            'Z' => Ok(non_quantifiable("\\z".to_string(), Width::fixed(0))),
            'b' => Ok(non_quantifiable(
                self.word_boundary_text(false),
                Width::fixed(0),
            )),
            'B' => Ok(non_quantifiable(
                self.word_boundary_text(true),
                Width::fixed(0),
            )),
            'w' => Ok(atom(
                unicode_class_text(&class_body_word(self.scope().ascii), false, self.scope()),
                Width::fixed(1),
            )),
            'W' => Ok(atom(
                unicode_class_text(&class_body_word(self.scope().ascii), true, self.scope()),
                Width::fixed(1),
            )),
            's' => Ok(atom(
                unicode_class_text(&class_body_space(self.scope().ascii), false, self.scope()),
                Width::fixed(1),
            )),
            'S' => Ok(atom(
                unicode_class_text(&class_body_space(self.scope().ascii), true, self.scope()),
                Width::fixed(1),
            )),
            'd' => Ok(atom(
                unicode_class_text(&class_body_digit(self.scope().ascii), false, self.scope()),
                Width::fixed(1),
            )),
            'D' => Ok(atom(
                unicode_class_text(&class_body_digit(self.scope().ascii), true, self.scope()),
                Width::fixed(1),
            )),
            '1'..='9' => self.parse_numeric_escape(c, backslash_pos),
            '0' => {
                let ch = self.read_octal_from('0', backslash_pos)?;
                Ok(atom(
                    literal_outside_class(ch, self.scope()),
                    Width::fixed(1),
                ))
            }
            other => {
                let (ch, _) = self.decode_escape_char(other)?;
                Ok(atom(
                    literal_outside_class(ch, self.scope()),
                    Width::fixed(1),
                ))
            }
        }
    }

    /// `\b`/`\B` (F8: `\B` never matches an empty *input*, confirmed
    /// directly against CPython 3.11 -- this translator used to add
    /// an `\A\z` alternative specifically to make it match there,
    /// which was backwards).
    fn word_boundary_text(&self, negate: bool) -> String {
        let word = class_body_word(self.scope().ascii);
        if negate {
            format!(
                "(?:(?<=[{word}])(?=[{word}])|(?<=[^{word}])(?=[^{word}])|\\A(?=[^{word}])|(?<=[^{word}])\\z)"
            )
        } else {
            format!(
                "(?:(?<=[{word}])(?=[^{word}])|(?<=[^{word}])(?=[{word}])|\\A(?=[{word}])|(?<=[{word}])\\z)"
            )
        }
    }

    /// `\1`-`\99`, disambiguated against a three-octal-digit escape
    /// exactly the way CPython's own `re/_parser.py::_escape` does
    /// (the module doc comment).
    fn parse_numeric_escape(&mut self, first: char, backslash_pos: usize) -> PResult<Piece> {
        let mut digits = String::new();
        digits.push(first);
        if let Some(next) = self.peek() {
            if next.is_ascii_digit() {
                let second = next;
                let is_octal_prefix = ('0'..='7').contains(&first) && ('0'..='7').contains(&second);
                let third = self.peek_at(1);
                if is_octal_prefix && third.is_some_and(|c| ('0'..='7').contains(&c)) {
                    self.advance(); // second digit
                    let third = self.advance().unwrap(); // third digit
                    let octal = format!("{first}{second}{third}");
                    let value = u32::from_str_radix(&octal, 8).unwrap_or(0);
                    if value > 0o377 {
                        return Err(format!(
                            "octal escape value \\{octal} outside of range 0-0o377 at position {backslash_pos}"
                        ));
                    }
                    let ch = char::from_u32(value).unwrap_or('\0');
                    return Ok(atom(
                        literal_outside_class(ch, self.scope()),
                        Width::fixed(1),
                    ));
                }
                self.advance(); // second digit, decimal group ref
                digits.push(second);
            }
        }
        let group: usize = digits.parse().unwrap_or(usize::MAX);
        if group >= 1 && group <= self.group_count {
            let width = self
                .group_widths
                .get(&group)
                .copied()
                .unwrap_or_else(Width::unknown);
            return Ok(atom(format!("\\{group}"), width));
        }
        Err(format!(
            "invalid group reference {group} at position {backslash_pos}"
        ))
    }

    /// `\x`, `\u`, `\U`, `\N{...}` and a bare letter/non-letter escape
    /// -- shared by [`Translator::parse_escape`] and
    /// [`Translator::read_class_escape`], each applying its own
    /// `[...]`-safe escaping (or IGNORECASE expansion,
    /// [`literal_outside_class`]) to the raw `char` this returns.
    fn decode_escape_char(&mut self, c: char) -> PResult<(char, Width)> {
        let backslash_pos = self.pos.saturating_sub(2);
        match c {
            'n' => Ok(('\n', Width::fixed(1))),
            't' => Ok(('\t', Width::fixed(1))),
            'r' => Ok(('\r', Width::fixed(1))),
            'f' => Ok(('\u{0c}', Width::fixed(1))),
            'v' => Ok(('\u{0b}', Width::fixed(1))),
            'a' => Ok(('\u{07}', Width::fixed(1))),
            'x' => {
                let hex = self.take_hex_digits(2);
                if hex.len() != 2 {
                    return Err(format!(
                        "incomplete escape \\x{hex} at position {backslash_pos}"
                    ));
                }
                let value = u32::from_str_radix(&hex, 16).unwrap();
                Ok((char::from_u32(value).unwrap_or('\0'), Width::fixed(1)))
            }
            'u' => {
                let hex = self.take_hex_digits(4);
                if hex.len() != 4 {
                    return Err(format!(
                        "incomplete escape \\u{hex} at position {backslash_pos}"
                    ));
                }
                let value = u32::from_str_radix(&hex, 16).unwrap();
                Ok((char::from_u32(value).unwrap_or('\0'), Width::fixed(1)))
            }
            'U' => {
                let hex = self.take_hex_digits(8);
                if hex.len() != 8 {
                    return Err(format!(
                        "incomplete escape \\U{hex} at position {backslash_pos}"
                    ));
                }
                let value = u32::from_str_radix(&hex, 16).unwrap();
                let ch = char::from_u32(value)
                    .ok_or_else(|| format!("bad escape \\U{hex} at position {backslash_pos}"))?;
                Ok((ch, Width::fixed(1)))
            }
            'N' => Err(format!(
                "\\N{{...}} named escapes are not supported at position {backslash_pos}"
            )),
            other if other.is_ascii_alphabetic() => {
                Err(format!("bad escape \\{other} at position {backslash_pos}"))
            }
            other => Ok((other, Width::fixed(1))),
        }
    }

    fn take_hex_digits(&mut self, max: usize) -> String {
        let mut hex = String::new();
        for _ in 0..max {
            match self.peek() {
                Some(c) if c.is_ascii_hexdigit() => {
                    hex.push(c);
                    self.advance();
                }
                _ => break,
            }
        }
        hex
    }
}

/// Folds a single contiguous leading run of bare global-flag letters
/// (the module-level doc comment's own "`m`/`s`/`i` only" rule --
/// `a`/`u`/`L` are consumed here, never forwarded) into *flags*.
fn apply_global_letters(letters: &str, flags: &mut ResolvedFlags, pos: usize) -> PResult<()> {
    if letters.contains('L') {
        return Err(format!(
            "bad inline flags: cannot use 'L' flag with a str pattern at position {pos}"
        ));
    }
    if letters.contains('i') {
        flags.ignorecase = true;
    }
    if letters.contains('m') {
        flags.multiline = true;
    }
    if letters.contains('s') {
        flags.dotall = true;
    }
    if letters.contains('x') {
        flags.verbose = true;
    }
    if letters.contains('a') {
        flags.ascii = true;
    }
    Ok(())
}

/// `\w`'s class body (without the enclosing `[...]`) -- ASCII mode is
/// the literal `a-zA-Z0-9_`; the default mode is Python's own Unicode
/// membership, read from [`regex_unicode`]'s own generated
/// (CPython-3.11-sourced) range tables rather than scanning all 1.1
/// million code points per translation (F9) -- rendered once into a
/// cached class-body string behind a [`OnceLock`], since `\w`/`\s`/
/// `\d` already come as `&'static` range tables and a compile has no
/// reason to re-render the same text on every call.
fn class_body_word(ascii: bool) -> String {
    if ascii {
        return "a-zA-Z0-9_".to_string();
    }
    static BODY: OnceLock<String> = OnceLock::new();
    BODY.get_or_init(|| {
        // `_` is not itself alphanumeric, so it's not part of
        // `WORD_RANGES` (that table's own doc comment) -- added here,
        // the one place this translator needs the full `\w` set.
        let mut ranges = regex_unicode::WORD_RANGES.to_vec();
        ranges.push((0x5F, 0x5F));
        ranges.sort_unstable_by_key(|r| r.0);
        ranges_to_class(&ranges)
    })
    .clone()
}

fn class_body_space(ascii: bool) -> String {
    if ascii {
        return " \\t\\n\\r\\f\\x0b".to_string();
    }
    static BODY: OnceLock<String> = OnceLock::new();
    BODY.get_or_init(|| ranges_to_class(regex_unicode::SPACE_RANGES))
        .clone()
}

fn class_body_digit(ascii: bool) -> String {
    if ascii {
        return "0-9".to_string();
    }
    static BODY: OnceLock<String> = OnceLock::new();
    BODY.get_or_init(|| ranges_to_class(regex_unicode::DIGIT_RANGES))
        .clone()
}

fn ranges_to_class(ranges: &[(u32, u32)]) -> String {
    let mut body = String::new();
    for &(lo, hi) in ranges {
        let lo_c = char::from_u32(lo).unwrap();
        body.push_str(&escape_in_class(lo_c));
        if hi > lo {
            body.push('-');
            let hi_c = char::from_u32(hi).unwrap();
            body.push_str(&escape_in_class(hi_c));
        }
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flags() -> ResolvedFlags {
        ResolvedFlags::default()
    }

    fn translate_ok(pattern: &str, f: ResolvedFlags) -> Translated {
        translate(pattern, f).unwrap_or_else(|e| panic!("translate({pattern:?}) failed: {e}"))
    }

    #[test]
    fn plain_literal_passes_through() {
        let t = translate_ok("abc", flags());
        assert_eq!(t.pattern, "abc");
    }

    #[test]
    fn dollar_without_multiline_matches_before_a_trailing_newline() {
        let t = translate_ok("a$", flags());
        let re = fancy_regex::Regex::new(&t.pattern).unwrap();
        assert!(re.is_match("a").unwrap());
        assert!(re.is_match("a\n").unwrap());
        assert!(!re.is_match("a\nb").unwrap());
    }

    #[test]
    fn dollar_with_multiline_matches_before_every_newline() {
        let mut f = flags();
        f.multiline = true;
        let t = translate_ok("a$", f);
        let re = fancy_regex::Regex::new(&t.pattern).unwrap();
        assert!(re.is_match("a\nb").unwrap());
    }

    #[test]
    fn word_class_excludes_combining_marks() {
        let t = translate_ok(r"\w", flags());
        let re = fancy_regex::Regex::new(&t.pattern).unwrap();
        assert!(!re.is_match("\u{345}").unwrap());
        assert!(re.is_match("_").unwrap());
        assert!(re.is_match("5").unwrap());
    }

    #[test]
    fn ascii_flag_restricts_word_class() {
        let mut f = flags();
        f.ascii = true;
        let t = translate_ok(r"\w", f);
        let re = fancy_regex::Regex::new(&t.pattern).unwrap();
        assert!(re.is_match("a").unwrap());
        assert!(!re.is_match("\u{e9}").unwrap()); // é
    }

    #[test]
    fn nothing_to_repeat_on_anchor() {
        let err = translate("^*", flags()).unwrap_err();
        assert!(err.contains("nothing to repeat"));
    }

    #[test]
    fn multiple_repeat_is_rejected() {
        let err = translate("a**", flags()).unwrap_err();
        assert!(err.contains("multiple repeat"));
    }

    #[test]
    fn possessive_and_atomic_pass_through_natively() {
        let t = translate_ok("a++", flags());
        assert_eq!(t.pattern, "a++");
        let t2 = translate_ok("(?>a+)", flags());
        assert_eq!(t2.pattern, "(?>a+)");
    }

    #[test]
    fn backreference_within_group_count_is_accepted() {
        let t = translate_ok(r"(a)\1", flags());
        assert_eq!(t.pattern, "(a)\\1");
    }

    #[test]
    fn forward_backreference_is_rejected() {
        let err = translate(r"\1(a)", flags()).unwrap_err();
        assert!(err.contains("invalid group reference 1"));
    }

    #[test]
    fn variable_width_lookbehind_is_rejected() {
        let err = translate(r"(?<=a*)b", flags()).unwrap_err();
        assert!(err.contains("fixed-width"));
    }

    #[test]
    fn fixed_width_lookbehind_is_accepted() {
        let t = translate_ok(r"(?<=foo)bar", flags());
        assert_eq!(t.pattern, "(?<=foo)bar");
    }

    #[test]
    fn global_flag_not_at_start_is_rejected() {
        let err = translate("a(?i)b", flags()).unwrap_err();
        assert!(err.contains("global flags not at the start"));
    }

    #[test]
    fn leading_global_flags_are_consumed_without_output() {
        let t = translate_ok("(?i)abc", flags());
        assert_eq!(t.pattern, "(?i)abc");
    }

    #[test]
    fn verbose_strips_whitespace_and_comments() {
        let mut f = flags();
        f.verbose = true;
        let t = translate_ok("a b # comment\n c", f);
        assert_eq!(t.pattern, "abc");
    }

    #[test]
    fn verbose_keeps_whitespace_inside_a_class() {
        let mut f = flags();
        f.verbose = true;
        let t = translate_ok("[a b]", f);
        assert!(t.pattern.contains(' '));
    }

    #[test]
    fn named_group_and_backreference() {
        let t = translate_ok(r"(?P<x>a)(?P=x)", flags());
        assert_eq!(t.group_names.get("x"), Some(&1));
        let re = fancy_regex::Regex::new(&t.pattern).unwrap();
        assert!(re.is_match("aa").unwrap());
        assert!(!re.is_match("ab").unwrap());
    }

    #[test]
    fn pythons_comma_n_shorthand_is_normalized_to_an_explicit_lower_bound() {
        let t = translate_ok("a{,3}", flags());
        let re = fancy_regex::Regex::new(&t.pattern).unwrap();
        assert!(re.is_match("aaa").unwrap());
    }

    #[test]
    fn octal_escape_decodes_to_a_literal_char() {
        let t = translate_ok(r"\012", flags());
        let re = fancy_regex::Regex::new(&t.pattern).unwrap();
        assert!(re.is_match("\n").unwrap());
    }

    #[test]
    fn character_class_range_matches_the_whole_range_not_the_endpoints_and_dash() {
        let t = translate_ok("[a-z]+", flags());
        let re = fancy_regex::Regex::new(&t.pattern).unwrap();
        assert!(re.is_match("m").unwrap());
        assert!(!re.is_match("-").unwrap());
    }

    #[test]
    fn negated_digit_class_range_excludes_the_whole_range() {
        let t = translate_ok("[^0-9]", flags());
        let re = fancy_regex::Regex::new(&t.pattern).unwrap();
        assert!(!re.is_match("5").unwrap());
        assert!(re.is_match("x").unwrap());
    }

    #[test]
    fn trailing_dash_in_class_is_a_literal() {
        let t = translate_ok("[a-]", flags());
        let re = fancy_regex::Regex::new(&t.pattern).unwrap();
        assert!(re.is_match("-").unwrap());
        assert!(re.is_match("a").unwrap());
    }

    #[test]
    fn backwards_class_range_is_a_bad_character_range_error() {
        let err = translate("[z-a]", flags()).unwrap_err();
        assert!(err.contains("bad character range"));
    }

    #[test]
    fn negated_word_shortcut_in_class_is_negated() {
        let t = translate_ok(r"[\W]", flags());
        let re = fancy_regex::Regex::new(&t.pattern).unwrap();
        assert!(re.is_match(" ").unwrap());
        assert!(!re.is_match("a").unwrap());
    }

    #[test]
    fn whitespace_or_non_whitespace_class_matches_everything() {
        let t = translate_ok(r"[\s\S]", flags());
        let re = fancy_regex::Regex::new(&t.pattern).unwrap();
        assert!(re.is_match("a").unwrap());
        assert!(re.is_match(" ").unwrap());
    }

    #[test]
    fn ignorecase_ascii_folds_a_plain_literal() {
        let mut f = flags();
        f.ignorecase = true;
        f.ascii = true;
        let t = translate_ok("abc", f);
        let re = fancy_regex::Regex::new(&t.pattern).unwrap();
        assert!(re.is_match("ABC").unwrap());
    }

    #[test]
    fn ignorecase_ascii_folds_a_class_range() {
        let mut f = flags();
        f.ignorecase = true;
        f.ascii = true;
        let t = translate_ok("[a-z]+", f);
        let re = fancy_regex::Regex::new(&t.pattern).unwrap();
        assert!(re.is_match("ABC").unwrap());
    }

    #[test]
    fn ignorecase_ascii_still_excludes_kelvin_sign() {
        let mut f = flags();
        f.ignorecase = true;
        f.ascii = true;
        let t = translate_ok("k", f);
        let re = fancy_regex::Regex::new(&t.pattern).unwrap();
        assert!(!re.is_match("\u{212A}").unwrap());
    }

    #[test]
    fn ignorecase_non_ascii_expands_turkish_i() {
        let mut f = flags();
        f.ignorecase = true;
        let t = translate_ok("i", f);
        let re = fancy_regex::Regex::new(&t.pattern).unwrap();
        assert!(re.is_match("\u{131}").unwrap());
        assert!(re.is_match("\u{130}").unwrap());
        assert!(re.is_match("I").unwrap());
    }

    #[test]
    fn ignorecase_word_class_does_not_match_a_combining_mark() {
        let mut f = flags();
        f.ignorecase = true;
        let t = translate_ok(r"\w", f);
        let re = fancy_regex::Regex::new(&t.pattern).unwrap();
        assert!(!re.is_match("\u{345}").unwrap());
    }

    #[test]
    fn backspace_inside_a_class_is_a_literal_backspace() {
        let t = translate_ok(r"[\b]", flags());
        let re = fancy_regex::Regex::new(&t.pattern).unwrap();
        assert!(re.is_match("\u{08}").unwrap());
    }

    #[test]
    fn octal_escape_inside_a_class() {
        let t = translate_ok(r"[\101]", flags());
        let re = fancy_regex::Regex::new(&t.pattern).unwrap();
        assert!(re.is_match("A").unwrap());
    }

    #[test]
    fn bare_digit_8_inside_a_class_is_a_bad_escape() {
        let err = translate(r"[\8]", flags()).unwrap_err();
        assert!(err.contains("bad escape"));
    }

    #[test]
    fn verbose_does_not_strip_a_non_breaking_space() {
        let mut f = flags();
        f.verbose = true;
        let t = translate_ok("a\u{a0}b", f);
        let re = fancy_regex::Regex::new(&t.pattern).unwrap();
        assert!(re.is_match("a\u{a0}b").unwrap());
        assert!(!re.is_match("ab").unwrap());
    }

    #[test]
    fn bare_comma_braces_means_zero_or_more() {
        let t = translate_ok("a{,}", flags());
        let re = fancy_regex::Regex::new(&t.pattern).unwrap();
        assert!(re.is_match("aaa").unwrap());
        assert!(re.is_match("").unwrap());
    }

    #[test]
    fn backwards_brace_quantifier_is_an_error() {
        let err = translate("a{3,2}", flags()).unwrap_err();
        assert!(err.contains("min repeat greater than max repeat"));
    }

    #[test]
    fn a_comment_is_transparent_to_a_following_quantifier() {
        let t = translate_ok("a(?#c)*", flags());
        let re = fancy_regex::Regex::new(&t.pattern).unwrap();
        assert!(re.is_match("aaa").unwrap());
    }

    #[test]
    fn duplicate_group_names_are_rejected() {
        let err = translate("(?P<a>x)(?P<a>y)", flags()).unwrap_err();
        assert!(err.contains("redefinition of group name"));
    }

    #[test]
    fn non_word_boundary_does_not_match_an_empty_input() {
        let t = translate_ok(r"\B", flags());
        let re = fancy_regex::Regex::new(&t.pattern).unwrap();
        assert!(!re.is_match("").unwrap());
    }

    #[test]
    fn lookbehind_backreference_width_comes_from_the_referenced_group() {
        // CPython 3.11 accepts `(ab)(?<=\1)` (group 1 is fixed-width
        // 2) -- this translator's own [`Width`] check agrees (reads
        // `\1`'s width from `group_widths` rather than always
        // widening to `Width::unknown`, F8), so `translate` itself no
        // longer rejects it as variable-width. `fancy-regex` still
        // does, downstream: `Expr::Backref` never reports
        // `const_size: true` regardless of the referenced group's own
        // width (confirmed in its own `analyze.rs`), so this stays a
        // documented divergence (DESIGN.md §7.3) pinned by this test
        // and a corpus case, not a bug this translator can route
        // around without forking the engine.
        let t = translate_ok(r"(ab)(?<=\1)", flags());
        let err = fancy_regex::Regex::new(&t.pattern).unwrap_err();
        assert!(matches!(
            err,
            fancy_regex::Error::CompileError(fancy_regex::CompileError::LookBehindNotConst)
        ));
    }

    #[test]
    fn lookbehind_containing_only_a_fixed_text_backreference_is_still_rejected_at_translate_time() {
        // A backreference to a group whose width genuinely isn't
        // fixed (`(a|bb)`) still fails the *translator's* own check,
        // same as before this lane.
        let err = translate(r"(a|bb)(?<=\1)", flags()).unwrap_err();
        assert!(err.contains("fixed-width"));
    }
}
