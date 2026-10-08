//! The composer: walks `saphyr-parser`'s event stream into a `Value`,
//! in two phases that mirror Circuitry's own loader exactly
//! (runtime-semantics §1.1, DESIGN.md §3.2):
//!
//! - **Phase A, [`Composer`]** builds a structural [`Node`] tree --
//!   anchors/aliases resolved, tags resolved to a canonical string, but
//!   nothing validated or constructed yet (no scalar parsing, no
//!   duplicate-key check, no `<<:` expansion). This mirrors PyYAML's
//!   `Composer`: `get_single_node`'s single-document check runs once
//!   this tree is complete, *before* anything in it is constructed --
//!   matching `yaml.safe_load` checking for a second document before
//!   building the first one's value.
//! - **Phase B** mirrors PyYAML's `BaseConstructor`/`SafeConstructor`
//!   plus Circuitry's `_UniqueKeyLoader`: [`validate_order`] replays the
//!   exact *order* in which `construct_document` would raise an error
//!   (breadth-first by nesting level, since every container's own
//!   constructor is a generator that returns an empty placeholder
//!   immediately and defers its body one level at a time -- a mapping's
//!   own duplicate-key check runs, for *all* its own keys, before any of
//!   its *values* are constructed, nested or not); [`construct`] then
//!   does the actual, ordinary depth-first build, which -- since
//!   [`validate_order`] already found the whole tree error-free -- never
//!   itself raises in practice.

use crate::error::{Mark, YamlError};
use crate::scalar;
use electricity_value::{Dict, Value};
use saphyr_parser::{Event, Marker, Parser, ScalarStyle, Span, StrInput, Tag};
use std::collections::HashMap;
use std::rc::Rc;

type Input<'input> = StrInput<'input>;

/// `yaml.safe_load(text)`, minus silent duplicate keys: `core/yaml_load.py`'s
/// `load_yaml`.
pub fn load_yaml(text: &str) -> Result<Value, YamlError> {
    reject_non_printable(text)?;
    let text = strip_leading_bom(text);
    let mut parser = Parser::new_from_str(text);
    let mut last_event_end = Marker::new(0, 1, 0);

    let (first, span) = pull(&mut parser, &mut last_event_end)?;
    debug_assert!(matches!(first, Event::StreamStart));

    let (second, doc_span) = pull(&mut parser, &mut last_event_end)?;
    match second {
        // No document at all (empty input, or only comments/whitespace):
        // `yaml.safe_load` returns `None` for this (confirmed empirically).
        Event::StreamEnd => Ok(Value::None),
        Event::DocumentStart(_) => {
            // An *implicit* document start (no `---`) takes the span of
            // the first content token itself (saphyr-parser's
            // `Parser::document_start`), so leaving `last_event_end` at
            // its end -- what `pull` does for every other event -- would
            // already consume a root-level anchor/tag token before the
            // composer ever gets to scan for one (`node_prefix`'s gap
            // would start past it, missing it entirely). Rewinding to
            // the span's *start* instead costs nothing for an explicit
            // `---`, whose own span holds no `&`/`!` for
            // `find_prefix_start` to find anyway.
            last_event_end = doc_span.start;
            let mut composer = Composer::new(text);
            let root = composer.compose_node(&mut parser, &mut last_event_end, 1)?;

            let (end, _) = pull(&mut parser, &mut last_event_end)?;
            debug_assert!(matches!(end, Event::DocumentEnd));

            // `get_single_node`: checked before anything in `root` is
            // constructed, exactly like PyYAML -- not after, the way a
            // single depth-first compose-and-construct pass would.
            let (after, after_span) = pull(&mut parser, &mut last_event_end)?;
            match after {
                Event::StreamEnd => {}
                Event::DocumentStart(_) => {
                    return Err(YamlError::MultipleDocuments {
                        mark: after_span.start.into(),
                    });
                }
                other => unreachable_event(&other),
            }

            validate_order(&root)?;
            construct(&root)
        }
        other => {
            let _ = span;
            unreachable_event(&other)
        }
    }
}

/// [`load_yaml`], but a repeated mapping key keeps its last value
/// instead of erroring -- plain `yaml.safe_load`'s own behaviour (no
/// `_UniqueKeyLoader`), for a caller reading a child document the way
/// `core/cycle_check.py::load_orch` does rather than the way
/// `core/yaml_load.py::load_yaml` does. Everything else (document
/// structure, scalar resolution, non-printable-character rejection) is
/// identical to [`load_yaml`].
pub fn load_yaml_last_key_wins(text: &str) -> Result<Value, YamlError> {
    reject_non_printable(text)?;
    let text = strip_leading_bom(text);
    let mut parser = Parser::new_from_str(text);
    let mut last_event_end = Marker::new(0, 1, 0);

    let (first, span) = pull(&mut parser, &mut last_event_end)?;
    debug_assert!(matches!(first, Event::StreamStart));

    let (second, doc_span) = pull(&mut parser, &mut last_event_end)?;
    match second {
        Event::StreamEnd => Ok(Value::None),
        Event::DocumentStart(_) => {
            last_event_end = doc_span.start;
            let mut composer = Composer::new(text);
            let root = composer.compose_node(&mut parser, &mut last_event_end, 1)?;

            let (end, _) = pull(&mut parser, &mut last_event_end)?;
            debug_assert!(matches!(end, Event::DocumentEnd));

            let (after, after_span) = pull(&mut parser, &mut last_event_end)?;
            match after {
                Event::StreamEnd => {}
                Event::DocumentStart(_) => {
                    return Err(YamlError::MultipleDocuments {
                        mark: after_span.start.into(),
                    });
                }
                other => unreachable_event(&other),
            }

            construct_lenient(&root)
        }
        other => {
            let _ = span;
            unreachable_event(&other)
        }
    }
}

/// PyYAML's `Reader.check_printable` (`reader.py`): every `str` input is
/// scanned up front for a character outside PyYAML's own allowed set --
/// before any tokenizing, so a non-printable character anywhere in the
/// document (a raw ANSI escape, a stray control byte -- the kind of
/// thing tool or model output can contain) fails immediately with a
/// `ReaderError` that carries no mark at all (`reader.py`'s own
/// `ReaderError.__str__` reports a raw character *position*, not a
/// line/column `Mark`) -- PyYAML's own reader has no line/column tracking
/// at this point, since `check_printable` runs before `forward()` is
/// ever called. `saphyr-parser` 0.1.0 has no such check at all (loading
/// a document with a raw `\x1b`/`\x80` byte in it silently succeeds), so
/// this crate owns it, matching the character class exactly: allowed
/// are tab/LF/CR, printable ASCII (0x20-0x7E), NEL (0x85), and every
/// non-control, non-surrogate, non-noncharacter codepoint above that
/// (0xA0-0xD7FF, 0xE000-0xFFFD, 0x10000-0x10FFFF).
fn reject_non_printable(text: &str) -> Result<(), YamlError> {
    fn is_printable(c: char) -> bool {
        matches!(c, '\u{09}' | '\u{0A}' | '\u{0D}' | '\u{85}')
            || ('\u{20}'..='\u{7E}').contains(&c)
            || ('\u{A0}'..='\u{D7FF}').contains(&c)
            || ('\u{E000}'..='\u{FFFD}').contains(&c)
            || ('\u{10000}'..='\u{10FFFF}').contains(&c)
    }
    if let Some((byte, ch)) = text.char_indices().find(|(_, c)| !is_printable(*c)) {
        let mut line = 0;
        let mut column = 0;
        for c in text[..byte].chars() {
            if c == '\n' {
                line += 1;
                column = 0;
            } else {
                column += 1;
            }
        }
        return Err(YamlError::Scan {
            message: format!(
                "unacceptable character #x{:04x}: special characters are not allowed",
                ch as u32
            ),
            mark: Mark { line, column },
        });
    }
    Ok(())
}

/// PyYAML's `Scanner.scan_to_next_token` (`scanner.py`): "the byte order
/// mark is stripped if it's the first character in the stream", and
/// only then -- a BOM anywhere else in the document is left alone ("we
/// do not yet support BOM inside the stream", PyYAML's own comment).
/// `saphyr-parser` 0.1.0 has no such handling at all (a leading BOM
/// becomes part of the first token's text, e.g. a mapping key literally
/// named `"\ufeffa"` instead of `"a"`), so this crate owns it too.
/// Stripping before the composer ever sees the text (rather than, say,
/// treating it as a zero-width prefix during composition) also matches
/// PyYAML's own column accounting: the BOM consumes no column at all
/// (`reader.py`'s `forward`: `elif ch != '\uFEFF': self.column += 1`),
/// exactly as if it had never been there.
fn strip_leading_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

fn unreachable_event(ev: &Event<'_>) -> ! {
    unreachable!("unexpected top-level event from saphyr-parser: {ev:?}")
}

/// Every event pull in this module goes through here, so
/// `last_event_end` -- the left edge [`node_prefix_mark`] scans forward
/// from -- always reflects exactly what was last consumed, regardless of
/// nesting depth.
fn pull<'input>(
    parser: &mut Parser<'input, Input<'input>>,
    last_event_end: &mut Marker,
) -> Result<(Event<'input>, Span), YamlError> {
    let result = match parser.next() {
        Some(Ok(pair)) => Ok(pair),
        Some(Err(err)) => Err(YamlError::Scan {
            message: err.info().to_string(),
            mark: (*err.marker()).into(),
        }),
        None => unreachable!("saphyr-parser's stream ended without StreamEnd"),
    };
    if let Ok((_, span)) = &result {
        *last_event_end = span.end;
    }
    result
}

/// The full tag string for an explicit tag, matching PyYAML's own
/// convention: a core-schema shorthand (`!!int`) becomes
/// `tag:yaml.org,2002:int`; a local shorthand (`!custom`) stays
/// `!custom`; the bare non-specific tag (`!` alone) forces `str` without
/// going through implicit resolution (confirmed: `! on` stays the string
/// `"on"`).
fn canonical_tag(tag: &Tag) -> String {
    format!("{}{}", tag.handle, tag.suffix)
}

/// `true` for the bare, non-specific tag (`!` alone, with no suffix) --
/// distinct from an *absent* tag, even though both end up resolving the
/// same way below. PyYAML runs full implicit resolution on its text for
/// `!` regardless of scalar style (confirmed: `! on`, quoted or plain,
/// both resolve to the bool `True` -- only an absent tag restricts
/// implicit resolution to plain-style scalars).
fn is_non_specific(tag: &Tag) -> bool {
    tag.handle.is_empty() && tag.suffix == "!"
}

/// The tag a scalar resolves to: an explicit, specific tag always wins.
/// Otherwise (no tag, or the bare non-specific `!`) a plain scalar
/// resolves implicitly; quoted/literal/folded scalars are `str` -- except
/// that the bare `!` forces implicit resolution even then
/// (runtime-semantics §1.1; known, narrow divergence: unlike CPython's
/// `re`, this crate's regex `$` does not also match just before a
/// trailing `\n` -- `int`/`float`'s own regexes special-case it
/// ([`scalar::construct_int`]'s doc comment), but `bool`'s lookup table
/// doesn't, so `! |\n  on\n` -- a construct that already raises a bare
/// `KeyError` under Circuitry's real loader -- resolves to the string
/// `"on\n"` here instead of erroring).
fn scalar_tag(text: &str, style: ScalarStyle, tag: Option<&Tag>) -> String {
    match tag {
        Some(t) if !is_non_specific(t) => canonical_tag(t),
        Some(_) => scalar::implicit_tag(text).to_string(),
        None if style == ScalarStyle::Plain => scalar::implicit_tag(text).to_string(),
        None => scalar::TAG_STR.to_string(),
    }
}

/// The tag a sequence/mapping node resolves to: an explicit, specific
/// tag always wins; otherwise (no tag, or a bare `!`) it's always the
/// default (`seq`/`map`) -- containers have no implicit-resolution table
/// the way scalars do.
fn container_tag(tag: Option<&Tag>, default: &str) -> String {
    match tag {
        Some(t) if !is_non_specific(t) => canonical_tag(t),
        _ => default.to_string(),
    }
}

/// Finds the first anchor (`&`) or tag (`!`) token in `gap` -- text known
/// to hold nothing but whitespace, comments, YAML punctuation, and at
/// most one anchor and one tag (the gap between two consecutive events,
/// by construction never a scalar's own text) -- skipping comments
/// (`#` to end of line) along the way. `None` if the gap holds neither.
fn find_prefix_start(gap: &str) -> Option<usize> {
    let bytes = gap.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'#' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'&' | b'!' => return Some(i),
            _ => i += 1,
        }
    }
    None
}

/// Finds an anchor (`&name`) in `gap` specifically (as opposed to
/// [`find_prefix_start`], which stops at whichever of `&`/`!` comes
/// first) and returns its name -- needed to reject a reused anchor name
/// regardless of whether a tag happens to precede or follow it.
fn find_anchor_name(gap: &str) -> Option<String> {
    let bytes = gap.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'#' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'&' => {
                let rest = &gap[i + 1..];
                let end = rest
                    .find(|c: char| c.is_whitespace() || matches!(c, ',' | '[' | ']' | '{' | '}'))
                    .unwrap_or(rest.len());
                return Some(rest[..end].to_string());
            }
            _ => i += 1,
        }
    }
    None
}

/// Re-walks `gap` up to byte offset `upto`, advancing `start` by however
/// many characters (and lines) that covers -- `gap`'s own text is always
/// a handful of punctuation/whitespace characters, so this is cheap.
/// Counts a lone `\r` (one not immediately followed by `\n`) as its own
/// line break, same as `\n` and a `\r\n` pair -- matching both
/// `saphyr_parser::char_traits::is_break` (which already treats a bare
/// `\r` this way: `Scanner::skip_linebreak`/`skip_nl` increment its own
/// `Marker`'s line for exactly this case) and PyYAML's `scan_line_break`.
/// A `\r` that *is* followed by `\n` advances neither `line` nor `col`
/// here -- the `\n` right after it does both -- so a CRLF pair still
/// counts as one break, not two.
fn advance_marker(start: Marker, gap: &str, upto: usize) -> Marker {
    let mut index = start.index();
    let mut line = start.line();
    let mut col = start.col();
    let mut chars = gap[..upto].chars().peekable();
    while let Some(ch) = chars.next() {
        index += ch.len_utf8();
        match ch {
            '\n' => {
                line += 1;
                col = 0;
            }
            '\r' if chars.peek() != Some(&'\n') => {
                line += 1;
                col = 0;
            }
            '\r' => {}
            _ => col += 1,
        }
    }
    Marker::new(index, line, col)
}

/// A structural node (runtime-semantics §1.1): built by [`Composer`]
/// (Phase A) with no validation beyond what `saphyr-parser` itself
/// enforces, consumed by [`validate_order`]/[`construct`] (Phase B).
/// `mark` is the node's *true* start -- including a leading anchor
/// and/or tag, per [`node_prefix_mark`] -- matching PyYAML's own node
/// `start_mark` exactly (DESIGN.md §3.2).
///
/// A container's children are [`Rc`], not owned directly: an anchored
/// node is reached both from its parent's own `items`/`pairs` (as the
/// tree is built) and from [`Composer::anchors`] (for a later alias to
/// find) -- sharing one allocation between those two owners, rather
/// than deep-copying the whole subtree into `anchors` on top of the one
/// already built, is what keeps [`Composer::store_anchor`] O(1)
/// regardless of the anchored subtree's size (`crate::MAX_NODES`'s doc
/// comment: nested anchors previously multiplied memory by the clone's
/// size on every level). An [`Event::Alias`] is unaffected by this: it
/// still charges the anchored node's full expanded [`Node::size`]
/// against the budget, because [`construct`] -- unlike this tree --
/// builds a genuinely distinct `Value` for every occurrence.
enum Node {
    Scalar {
        text: String,
        tag: String,
        mark: Mark,
    },
    Sequence {
        tag: String,
        items: Vec<Rc<Node>>,
        mark: Mark,
        /// 1 + the deepest child's own [`Node::depth`] (1 for an empty
        /// sequence) -- computed once, from already-known children, when
        /// the node is built, *not* by recursing back down through it
        /// later: this is what lets [`Composer::check_anchor_name`]'s
        /// sibling check reject an alias chain's *compounded* depth (one
        /// alias cloning a whole already-deep subtree into a new, only
        /// textually-shallow container) in O(1), without re-walking the
        /// cloned subtree (`crate::MAX_DEPTH`'s doc comment).
        depth: usize,
        /// 1 + every child's own [`Node::size`] -- see [`Node::size`]'s
        /// doc comment: this is what lets an [`Event::Alias`] know, in
        /// O(1) and *before* actually cloning anything, how many new
        /// nodes that one clone is about to allocate.
        size: usize,
    },
    Mapping {
        tag: String,
        pairs: Vec<(Rc<Node>, Rc<Node>)>,
        mark: Mark,
        /// See [`Node::Sequence`]'s `depth` -- 1 + the deepest key/value's
        /// own depth, 1 for an empty mapping.
        depth: usize,
        /// See [`Node::Sequence`]'s `size` -- 1 + every key/value's own
        /// [`Node::size`].
        size: usize,
    },
}

impl Node {
    fn mark(&self) -> Mark {
        match self {
            Node::Scalar { mark, .. }
            | Node::Sequence { mark, .. }
            | Node::Mapping { mark, .. } => *mark,
        }
    }

    /// This node's own nesting depth: 0 for a scalar -- `MAX_DEPTH`
    /// bounds how deep a document's *containers* may nest (`lib.rs`'s
    /// doc comment), and a scalar leaf is not a container, so it must
    /// not consume one of that budget's own units the way a genuinely
    /// nested sequence/mapping does (previously 1, which meant a chain
    /// of exactly `MAX_DEPTH` real containers bottoming in a scalar
    /// -- the ordinary case for a real document -- was rejected one
    /// level early; only a chain bottoming in an *empty* container,
    /// itself still depth 1, actually reached the documented limit) --
    /// or the stored `depth` for a sequence/mapping.
    fn depth(&self) -> usize {
        match self {
            Node::Scalar { .. } => 0,
            Node::Sequence { depth, .. } | Node::Mapping { depth, .. } => *depth,
        }
    }

    /// How many `Node`s this one is made of, counting an aliased child
    /// by the size of the subtree an [`Event::Alias`] for it would
    /// actually clone -- not 1, the way a textually one-line `*x` might
    /// suggest -- so a container built out of aliases reports the
    /// *expanded* count its own clone (should something alias *this*
    /// node in turn) would have to allocate, same as `depth` already
    /// does for nesting (`crate::MAX_NODES`'s doc comment).
    fn size(&self) -> usize {
        match self {
            Node::Scalar { .. } => 1,
            Node::Sequence { size, .. } | Node::Mapping { size, .. } => *size,
        }
    }

    fn is_merge_key(&self) -> bool {
        matches!(self, Node::Scalar { tag, .. } if tag == scalar::TAG_MERGE)
    }
}

struct Composer<'t> {
    text: &'t str,
    /// `line_starts[n]` is the byte offset of the first character of
    /// line `n + 1` (`saphyr_parser::Marker::line()` is 1-indexed) --
    /// used by [`Self::byte_offset`] to turn a `Marker`'s `(line, col)`
    /// into a byte offset into `text` by walking `col` *characters*
    /// (never bytes) forward from the line's start. `Marker::index()`
    /// is deliberately never used for this: despite its own doc comment
    /// claiming bytes, saphyr-parser 0.1.0 counts *characters* there
    /// (`scanner.rs`'s `Marker.index` field doc, `StrInput::skip`
    /// advancing one `char` at a time), so slicing `text` with it
    /// directly panics or misplaces every mark after a non-ASCII
    /// character. Built once, in [`Self::new`], counting `\n`, `\r\n`
    /// and a lone `\r` as one line break each -- matching
    /// `saphyr_parser`'s own `Scanner::skip_linebreak` (which already
    /// moves a `Marker`'s line number across a bare `\r`) -- so a line
    /// after a lone `\r` has a real entry here instead of silently
    /// falling through [`Self::byte_offset`]'s out-of-range fallback to
    /// `text.len()` (see [`advance_marker`]'s doc comment for the same
    /// fix on the other side of every mark this crate computes).
    line_starts: Vec<usize>,
    /// An anchored node, by `saphyr-parser`'s own anchor id -- the same
    /// `Rc` a later [`Event::Alias`] clones (an O(1) refcount bump, not
    /// a deep copy: see [`Node`]'s own doc comment).
    anchors: HashMap<usize, Rc<Node>>,
    anchor_marks: HashMap<String, Mark>,
    /// The last `(Marker, byte offset)` [`Self::byte_offset`] computed --
    /// every marker it's ever asked to convert, across the whole
    /// document, only ever moves forward (`last_event_end`/`span.start`
    /// are themselves monotonic non-decreasing: every event's own span
    /// starts no earlier than the previous event's ended). Recomputing
    /// from this cursor by walking forward only the *newly* covered
    /// characters -- instead of re-scanning from the line's start every
    /// time, as a single-line flow document (e.g. a JSON-like mapping
    /// with 100k keys) would otherwise force `node_prefix`'s two calls
    /// per event to do -- turns what was an O(column) rescan per call,
    /// O(n^2) over the whole line, back into the O(1)-amortized walk it
    /// should be. A marker at or before the cursor (never produced by
    /// the composer itself, but not relied upon) still resolves
    /// correctly, just via the non-incremental fallback below.
    offset_cursor: (Marker, usize),
    /// How many nodes an [`Event::Alias`] has cloned so far, in total,
    /// across the whole document -- checked against `crate::MAX_NODES`
    /// *before* each individual clone (using the anchored node's own,
    /// already-known [`Node::size`], never by performing the clone
    /// first and counting afterward): a chain of aliases each cloning
    /// an already-expanded subtree would otherwise blow past available
    /// memory well before any depth or stack limit ever triggers
    /// (`crate::MAX_NODES`'s doc comment). Deliberately *not* incremented
    /// for nodes composed directly from the input text -- those are
    /// already bounded by the text's own size; only a clone creates
    /// nodes the input's length doesn't account for.
    cloned_nodes: usize,
}

impl<'t> Composer<'t> {
    fn new(text: &'t str) -> Self {
        let mut line_starts = vec![0];
        let mut chars = text.char_indices().peekable();
        while let Some((byte, ch)) = chars.next() {
            match ch {
                '\n' => line_starts.push(byte + 1),
                '\r' if chars.peek().map(|&(_, c)| c) != Some('\n') => {
                    line_starts.push(byte + 1);
                }
                _ => {}
            }
        }
        Composer {
            text,
            line_starts,
            anchors: HashMap::new(),
            anchor_marks: HashMap::new(),
            offset_cursor: (Marker::new(0, 1, 0), 0),
            cloned_nodes: 0,
        }
    }

    /// `marker`'s byte offset into `text`, computed from its `(line,
    /// col)` rather than its `index()` (see [`Self::line_starts`]'s own
    /// doc comment) -- incrementally from [`Self::offset_cursor`] when
    /// `marker` is on the same line at or after it (see that field's doc
    /// comment), falling back to a full line-start-relative walk
    /// otherwise (a new line, or -- never actually produced, but handled
    /// correctly regardless -- a marker before the cursor).
    fn byte_offset(&mut self, marker: Marker) -> usize {
        let (cursor_marker, cursor_byte) = self.offset_cursor;
        let byte = if marker.line() == cursor_marker.line() && marker.col() >= cursor_marker.col() {
            let delta = marker.col() - cursor_marker.col();
            self.text[cursor_byte..]
                .char_indices()
                .nth(delta)
                .map_or(self.text.len(), |(offset, _)| cursor_byte + offset)
        } else {
            let line_start = self
                .line_starts
                .get(marker.line().saturating_sub(1))
                .copied()
                .unwrap_or(self.text.len());
            self.text[line_start..]
                .char_indices()
                .nth(marker.col())
                .map_or(self.text.len(), |(offset, _)| line_start + offset)
        };
        self.offset_cursor = (marker, byte);
        byte
    }

    /// A [`Mark`]'s byte offset into `text`, by the same line-start +
    /// character-walk as [`Self::byte_offset`]'s fallback branch, but
    /// taking a `Mark` (this crate's own, already-0-indexed type) and
    /// not touching [`Self::offset_cursor`] -- used only on an error
    /// path ([`Self::quoted_scalar_escape_mark`]), where the one-off
    /// cost of a full walk from the line's start is irrelevant and
    /// disturbing the cursor used by the hot, success-path conversions
    /// elsewhere would be wrong regardless.
    fn mark_byte_offset(&self, mark: Mark) -> usize {
        let line_start = self
            .line_starts
            .get(mark.line)
            .copied()
            .unwrap_or(self.text.len());
        self.text[line_start..]
            .char_indices()
            .nth(mark.column)
            .map_or(self.text.len(), |(offset, _)| line_start + offset)
    }

    /// Recomputes the mark PyYAML's own scanner would report for one of
    /// three double-quoted-scalar escape-scanning errors that
    /// `saphyr-parser` 0.1.0 always marks at the scalar's own opening
    /// quote (`scanner.rs`'s `resolve_flow_scalar_escape_sequence`,
    /// whose three `Err` branches all pass through the `start_mark` the
    /// scalar began at, never a position inside it) -- unlike PyYAML's
    /// own `scan_flow_scalar_non_spaces`/the escape-handling branch of
    /// it (`scanner.py`), which advances its own mark character by
    /// character while scanning and reports the position actually
    /// reached when the error is raised. Replays that same scan from
    /// the opening quote, using PyYAML's own `ESCAPE_REPLACEMENTS`/
    /// `ESCAPE_CODES` tables, far enough to reach the specific error
    /// `message` names. `None` leaves the caller's original (saphyr)
    /// mark untouched -- deliberately conservative: this only replays
    /// plain non-escaped characters, an escaped literal quote, and
    /// plain space/tab runs on the way to the offending escape, which
    /// is all three corpus cases exercising this need; a line break
    /// *inside* the scalar bails out rather than also replicating
    /// PyYAML's own line-folding rules here. Columns are counted in
    /// characters, never bytes, matching every other mark this crate
    /// computes ([`Self::byte_offset`]'s doc comment).
    fn quoted_scalar_escape_mark(&self, start: Mark, message: &str) -> Option<Mark> {
        const UNKNOWN_ESCAPE: &str =
            "while parsing a quoted scalar, found unknown escape character";
        const BAD_HEX: &str =
            "while parsing a quoted scalar, did not find expected hexadecimal number";
        const BAD_UNICODE: &str =
            "while parsing a quoted scalar, found invalid Unicode character escape code";
        if message != UNKNOWN_ESCAPE && message != BAD_HEX && message != BAD_UNICODE {
            return None;
        }

        let byte = self.mark_byte_offset(start);
        if self.text.as_bytes().get(byte) != Some(&b'"') {
            return None;
        }
        let chars: Vec<char> = self.text[byte + 1..].chars().collect();
        let line = start.line;
        let mut col = start.column + 1;
        let mut i = 0usize;
        loop {
            while i < chars.len()
                && !matches!(
                    chars[i],
                    '\'' | '"'
                        | '\\'
                        | '\0'
                        | ' '
                        | '\t'
                        | '\r'
                        | '\n'
                        | '\u{85}'
                        | '\u{2028}'
                        | '\u{2029}'
                )
            {
                i += 1;
                col += 1;
            }
            match *chars.get(i)? {
                '"' => return None,
                '\'' | ' ' | '\t' => {
                    i += 1;
                    col += 1;
                }
                '\\' => {
                    i += 1;
                    col += 1;
                    let esc = *chars.get(i)?;
                    match esc {
                        '0' | 'a' | 'b' | 't' | '\t' | 'n' | 'v' | 'f' | 'r' | 'e' | ' ' | '"'
                        | '\\' | '/' | 'N' | '_' | 'L' | 'P' => {
                            i += 1;
                            col += 1;
                        }
                        'x' | 'u' | 'U' => {
                            let want = match esc {
                                'x' => 2,
                                'u' => 4,
                                _ => 8,
                            };
                            i += 1;
                            col += 1;
                            let hex_mark = Mark { line, column: col };
                            let mut ok = true;
                            for k in 0..want {
                                match chars.get(i + k) {
                                    Some(c) if c.is_ascii_hexdigit() => {}
                                    _ => {
                                        ok = false;
                                        break;
                                    }
                                }
                            }
                            if !ok {
                                return if message == BAD_HEX {
                                    Some(hex_mark)
                                } else {
                                    None
                                };
                            }
                            let digits: String = chars[i..i + want].iter().collect();
                            let value = u32::from_str_radix(&digits, 16).ok()?;
                            if char::from_u32(value).is_none() {
                                return if message == BAD_UNICODE {
                                    Some(hex_mark)
                                } else {
                                    None
                                };
                            }
                            i += want;
                            col += want;
                        }
                        '\r' | '\n' | '\u{85}' | '\u{2028}' | '\u{2029}' => return None,
                        _ => {
                            return if message == UNKNOWN_ESCAPE {
                                Some(Mark { line, column: col })
                            } else {
                                None
                            };
                        }
                    }
                }
                '\r' | '\n' | '\u{85}' | '\u{2028}' | '\u{2029}' | '\0' => return None,
                _ => unreachable!("loop only stops at a character from the stop set above"),
            }
        }
    }

    /// [`Self::quoted_scalar_escape_mark`] applied to a `Scan` error's
    /// own mark, if it names one of the three errors that function
    /// recognizes -- every other error variant (and every other `Scan`
    /// message) passes through unchanged.
    fn fixup_scalar_error(&self, err: YamlError) -> YamlError {
        if let YamlError::Scan { message, mark } = &err {
            if let Some(fixed) = self.quoted_scalar_escape_mark(*mark, message) {
                return YamlError::Scan {
                    message: message.clone(),
                    mark: fixed,
                };
            }
        }
        err
    }

    /// The node's true start mark (anchor/tag-inclusive) and, if it
    /// carries an anchor, that anchor's name -- both recovered from the
    /// raw text between `last_event_end` and `span.start`, the one place
    /// an anchor/tag token's own position still exists once
    /// `saphyr-parser` has reduced it to just an id/a `Tag` on the event
    /// (see the module docs and [`find_prefix_start`]).
    fn node_prefix(
        &mut self,
        span: Span,
        last_event_end: Marker,
        has_anchor: bool,
    ) -> (Mark, Option<String>) {
        let gap_start = self.byte_offset(last_event_end);
        let gap_end = self.byte_offset(span.start);
        if gap_end <= gap_start {
            return (span.start.into(), None);
        }
        let gap = &self.text[gap_start..gap_end];
        let mark = match find_prefix_start(gap) {
            Some(offset) => advance_marker(last_event_end, gap, offset).into(),
            None => span.start.into(),
        };
        let name = if has_anchor {
            find_anchor_name(gap)
        } else {
            None
        };
        (mark, name)
    }

    /// Checks `name` (an anchor about to be attached to a node at
    /// `mark`) for reuse, rejecting it the way PyYAML's
    /// `Composer.compose_node` does (`found duplicate anchor`) --
    /// saphyr-parser's own anchor table allows reuse outright (its
    /// duplicate check is commented out), so this crate owns the check
    /// instead. Called *before* a container's children are composed
    /// (PyYAML's own `compose_node` registers an anchor right after
    /// creating its node, ahead of filling in its value: `composer.py`
    /// :72-77, :108, :126) -- registering only afterwards, as this
    /// crate's first version did, checks a shallower anchor against a
    /// deeper (not yet even registered) one that textually comes later,
    /// reporting the pair in the wrong order and from the wrong node's
    /// mark.
    fn check_anchor_name(&mut self, name: Option<&str>, mark: Mark) -> Result<(), YamlError> {
        let Some(name) = name else { return Ok(()) };
        if let Some(&first_mark) = self.anchor_marks.get(name) {
            return Err(YamlError::Scan {
                message: format!(
                    "found duplicate anchor {name:?}; first occurrence at line {}, column {}",
                    first_mark.line + 1,
                    first_mark.column + 1
                ),
                mark,
            });
        }
        self.anchor_marks.insert(name.to_string(), mark);
        Ok(())
    }

    /// Stores `node` as `anchor_id`'s value for a later [`Event::Alias`]
    /// to clone -- called once `node`'s children are fully composed,
    /// after [`Self::check_anchor_name`] already ran on the same anchor
    /// before they were. `node` is already the same `Rc` being returned
    /// up to the caller (an `Rc::clone` at each call site, O(1)), so this
    /// never allocates a copy of the subtree -- only [`Self::alias_node`]
    /// ever does, and that one is charged against `crate::MAX_NODES`.
    fn store_anchor(&mut self, anchor_id: usize, node: Rc<Node>) {
        if anchor_id != 0 {
            self.anchors.insert(anchor_id, node);
        }
    }

    /// Forced inline (even in a debug build, where `#[inline(never)]`'s
    /// counterpart `#[inline(always)]` is still honored): this and
    /// [`Self::compose_from_event`] are thin wrappers with no large
    /// locals of their own, so folding them into their one call site
    /// each costs nothing and saves two whole stack frames per nesting
    /// level on top of the split in [`Self::compose_from_event`]'s own
    /// doc comment.
    #[inline(always)]
    fn compose_node<'input>(
        &mut self,
        parser: &mut Parser<'input, Input<'input>>,
        last_event_end: &mut Marker,
        depth: usize,
    ) -> Result<Rc<Node>, YamlError> {
        let before = *last_event_end;
        let (event, span) = pull(parser, last_event_end).map_err(|e| self.fixup_scalar_error(e))?;
        self.compose_from_event(event, span, before, depth, parser, last_event_end)
    }

    /// A thin dispatcher with no large locals of its own -- forced
    /// inline (with [`Self::compose_node`]) into its callers, so a
    /// debug build never reserves it a stack frame of its own on top of
    /// the one each recursive call already pays for. Composing a
    /// container's own body -- the `Vec`/`String` locals that would
    /// otherwise all share one combined, oversized frame if this match
    /// handled them inline -- lives in [`Self::compose_sequence`]/
    /// [`Self::compose_mapping`] instead, each its own, narrower,
    /// `#[inline(never)]` frame: the two tests in `tests/nesting_limit.rs`
    /// that run on a 2 MiB thread exist precisely to catch a regression
    /// here (a debug build's frames are far larger than an optimized
    /// build's, so this only shows up unoptimized).
    #[inline(always)]
    fn compose_from_event<'input>(
        &mut self,
        event: Event<'input>,
        span: Span,
        before: Marker,
        depth: usize,
        parser: &mut Parser<'input, Input<'input>>,
        last_event_end: &mut Marker,
    ) -> Result<Rc<Node>, YamlError> {
        // Checked before doing anything else with a container event --
        // in particular, before recursing into any of its children --
        // so a document nested arbitrarily deep in the text itself (not
        // through an alias) never recurses this function past
        // `MAX_DEPTH` frames, regardless of how deep the text claims to
        // go (crate::MAX_DEPTH's doc comment).
        if depth > crate::MAX_DEPTH && !matches!(event, Event::Scalar(..) | Event::Alias(_)) {
            return Err(YamlError::NestingTooDeep {
                mark: span.start.into(),
            });
        }
        match event {
            Event::Alias(id) => self.alias_node(id, span),
            Event::Scalar(text, style, anchor_id, tag) => {
                self.compose_scalar(text, style, anchor_id, tag, span, before)
            }
            Event::SequenceStart(anchor_id, tag) => {
                self.compose_sequence(anchor_id, tag, span, before, depth, parser, last_event_end)
            }
            Event::MappingStart(anchor_id, tag) => {
                self.compose_mapping(anchor_id, tag, span, before, depth, parser, last_event_end)
            }
            other => unreachable_event(&other),
        }
    }

    /// [`Event::Alias`]'s whole handling: looks up the anchored node
    /// *without* cloning it yet, checks [`Node::size`] against
    /// [`crate::MAX_NODES`] and [`Self::cloned_nodes`]'s running total,
    /// and only then performs the actual clone -- so a subtree whose
    /// clone would blow the budget is never allocated in the first
    /// place (`crate::MAX_NODES`'s doc comment). The clone itself is
    /// now just an `Rc::clone` (O(1), no new `Node`s at all) -- the
    /// budget still charges `size` in full regardless, because
    /// [`construct`] later walks this same `Rc` once per place it's
    /// aliased into and builds a genuinely distinct `Value` each time;
    /// the budget bounds *that* eventual cost, not this tree's own.
    fn alias_node(&mut self, id: usize, span: Span) -> Result<Rc<Node>, YamlError> {
        let size = match self.anchors.get(&id) {
            Some(node) => node.size(),
            // saphyr-parser itself rejects a genuinely undefined anchor
            // name before this point; reaching here means the anchor is
            // still being composed (a circular reference). PyYAML
            // registers an anchor before composing its own children, so
            // it builds a real recursive structure instead -- an
            // unsupported, known divergence, exercised by the golden
            // corpus's `known_divergence_cases` (`generate_yaml_corpus.py`)
            // and checked loosely (by message, not by value) in
            // `golden_corpus.rs`.
            None => {
                return Err(YamlError::Scan {
                    message: "self-referential anchor is not supported".to_string(),
                    mark: span.start.into(),
                });
            }
        };
        if self.cloned_nodes + size > crate::MAX_NODES {
            return Err(YamlError::AliasExpansionTooLarge {
                mark: span.start.into(),
            });
        }
        self.cloned_nodes += size;
        Ok(Rc::clone(
            self.anchors
                .get(&id)
                .expect("just looked up by the same id above"),
        ))
    }

    #[inline(never)]
    fn compose_scalar<'input>(
        &mut self,
        text: std::borrow::Cow<'input, str>,
        style: ScalarStyle,
        anchor_id: usize,
        tag: Option<std::borrow::Cow<'input, Tag>>,
        span: Span,
        before: Marker,
    ) -> Result<Rc<Node>, YamlError> {
        let (mark, name) = self.node_prefix(span, before, anchor_id != 0);
        self.check_anchor_name(name.as_deref(), mark)?;
        let tag_str = scalar_tag(text.as_ref(), style, tag.as_deref());
        let node = Rc::new(Node::Scalar {
            text: text.into_owned(),
            tag: tag_str,
            mark,
        });
        self.store_anchor(anchor_id, Rc::clone(&node));
        Ok(node)
    }

    // One parameter per thing the recursive-descent loop below actually
    // needs; splitting any of them into a bundled struct would only move
    // the same bytes, not shrink this function's -- deliberately kept
    // separate, `#[inline(never)]` -- stack frame (this function's own
    // doc comment on `compose_from_event`).
    #[allow(clippy::too_many_arguments)]
    #[inline(never)]
    fn compose_sequence<'input>(
        &mut self,
        anchor_id: usize,
        tag: Option<std::borrow::Cow<'input, Tag>>,
        span: Span,
        before: Marker,
        depth: usize,
        parser: &mut Parser<'input, Input<'input>>,
        last_event_end: &mut Marker,
    ) -> Result<Rc<Node>, YamlError> {
        let (mark, name) = self.node_prefix(span, before, anchor_id != 0);
        self.check_anchor_name(name.as_deref(), mark)?;
        let tag_str = container_tag(tag.as_deref(), scalar::TAG_SEQ);
        let mut items = Vec::new();
        loop {
            let item_before = *last_event_end;
            let (ev, sp) = pull(parser, last_event_end).map_err(|e| self.fixup_scalar_error(e))?;
            if matches!(ev, Event::SequenceEnd) {
                break;
            }
            items.push(self.compose_from_event(
                ev,
                sp,
                item_before,
                depth + 1,
                parser,
                last_event_end,
            )?);
        }
        let node_depth = node_depth_of(&items);
        if node_depth > crate::MAX_DEPTH {
            return Err(YamlError::NestingTooDeep { mark });
        }
        let node_size = node_size_of(&items);
        let node = Rc::new(Node::Sequence {
            tag: tag_str,
            items,
            mark,
            depth: node_depth,
            size: node_size,
        });
        self.store_anchor(anchor_id, Rc::clone(&node));
        Ok(node)
    }

    #[allow(clippy::too_many_arguments)]
    #[inline(never)]
    fn compose_mapping<'input>(
        &mut self,
        anchor_id: usize,
        tag: Option<std::borrow::Cow<'input, Tag>>,
        span: Span,
        before: Marker,
        depth: usize,
        parser: &mut Parser<'input, Input<'input>>,
        last_event_end: &mut Marker,
    ) -> Result<Rc<Node>, YamlError> {
        let (mark, name) = self.node_prefix(span, before, anchor_id != 0);
        self.check_anchor_name(name.as_deref(), mark)?;
        let tag_str = container_tag(tag.as_deref(), scalar::TAG_MAP);
        let mut pairs = Vec::new();
        loop {
            let key_before = *last_event_end;
            let (kev, ksp) =
                pull(parser, last_event_end).map_err(|e| self.fixup_scalar_error(e))?;
            if matches!(kev, Event::MappingEnd) {
                break;
            }
            let key =
                self.compose_from_event(kev, ksp, key_before, depth + 1, parser, last_event_end)?;
            let value = self.compose_node(parser, last_event_end, depth + 1)?;
            pairs.push((key, value));
        }
        let node_depth = node_depth_of_pairs(&pairs);
        if node_depth > crate::MAX_DEPTH {
            return Err(YamlError::NestingTooDeep { mark });
        }
        let node_size = node_size_of_pairs(&pairs);
        let node = Rc::new(Node::Mapping {
            tag: tag_str,
            pairs,
            mark,
            depth: node_depth,
            size: node_size,
        });
        self.store_anchor(anchor_id, Rc::clone(&node));
        Ok(node)
    }
}

/// 1 + the deepest item's depth (1 for an empty sequence) -- `Node`'s
/// own `depth()` is already O(1) per child (`crate::MAX_DEPTH`'s doc
/// comment), so this never itself recurses into a child's structure.
fn node_depth_of(items: &[Rc<Node>]) -> usize {
    1 + items.iter().map(|n| n.depth()).max().unwrap_or(0)
}

/// Same as [`node_depth_of`], over a mapping's key/value pairs.
fn node_depth_of_pairs(pairs: &[(Rc<Node>, Rc<Node>)]) -> usize {
    1 + pairs
        .iter()
        .flat_map(|(k, v)| [k.depth(), v.depth()])
        .max()
        .unwrap_or(0)
}

/// 1 + the sum of every item's own [`Node::size`] -- see
/// [`Node::size`]'s doc comment; an aliased item's size is already the
/// *expanded* count its own clone allocated, not 1, so this correctly
/// propagates a nested alias's cost up through every container it sits
/// inside, without re-walking any cloned subtree.
fn node_size_of(items: &[Rc<Node>]) -> usize {
    1 + items.iter().map(|n| n.size()).sum::<usize>()
}

/// Same as [`node_size_of`], over a mapping's key/value pairs.
fn node_size_of_pairs(pairs: &[(Rc<Node>, Rc<Node>)]) -> usize {
    1 + pairs
        .iter()
        .map(|(k, v)| k.size() + v.size())
        .sum::<usize>()
}

fn unresolvable(tag: &str, mark: Mark) -> YamlError {
    YamlError::UnresolvableTag {
        tag: tag.to_string(),
        mark,
    }
}

fn resolve_scalar_value(text: &str, tag: &str, mark: Mark) -> Result<Value, YamlError> {
    match tag {
        scalar::TAG_NULL => Ok(Value::None),
        scalar::TAG_BOOL => {
            scalar::construct_bool(text)
                .map(Value::Bool)
                .ok_or(YamlError::InvalidScalar {
                    type_name: "bool",
                    mark,
                })
        }
        scalar::TAG_INT => {
            scalar::construct_int(text)
                .map(Value::Int)
                .ok_or(YamlError::InvalidScalar {
                    type_name: "int",
                    mark,
                })
        }
        scalar::TAG_FLOAT => {
            scalar::construct_float(text)
                .map(Value::Float)
                .ok_or(YamlError::InvalidScalar {
                    type_name: "float",
                    mark,
                })
        }
        scalar::TAG_TIMESTAMP => {
            scalar::construct_timestamp(text).ok_or(YamlError::InvalidScalar {
                type_name: "datetime.datetime",
                mark,
            })
        }
        scalar::TAG_STR => Ok(Value::Str(text.to_string())),
        scalar::TAG_BINARY => {
            scalar::construct_binary(text)
                .map(Value::Bytes)
                .ok_or(YamlError::InvalidScalar {
                    type_name: "bytes",
                    mark,
                })
        }
        other => Err(unresolvable(other, mark)),
    }
}

/// Tracks a mapping's own seen keys for the duplicate-key check,
/// special-casing `NaN`: PyYAML's `construct_yaml_float` returns its one
/// shared `nan_value` object for *every* `.nan` scalar, so two `.nan`
/// keys in the same mapping collide in CPython's dict (an identity check
/// precedes `__eq__`) even though `NaN != NaN` generally -- this is
/// narrow to the duplicate-key check itself, not a change to `Value`'s
/// own, intentionally IEEE-754-faithful equality (electricity-value's
/// `lib.rs`).
#[derive(Default)]
struct SeenKeys {
    seen: HashMap<Value, Mark>,
    nan: Option<Mark>,
}

impl SeenKeys {
    fn check_and_insert(&mut self, key: Value, mark: Mark) -> Result<(), Mark> {
        if let Value::Float(f) = &key {
            if f.is_nan() {
                return match self.nan {
                    Some(first) => Err(first),
                    None => {
                        self.nan = Some(mark);
                        Ok(())
                    }
                };
            }
        }
        if let Some(&first) = self.seen.get(&key) {
            return Err(first);
        }
        self.seen.insert(key, mark);
        Ok(())
    }
}

/// `core/yaml_load.py`'s `DuplicateKeyError` message, word for word:
/// `duplicate key {key!r} at line {L}, column {C} (first defined at line
/// {L}, column {C}); YAML would silently keep only the last one`.
fn duplicate_key_error(key: &Value, first: Mark, second: Mark) -> YamlError {
    YamlError::DuplicateKey {
        message: format!(
            "duplicate key {} at line {}, column {} (first defined at line {}, column {}); \
             YAML would silently keep only the last one",
            key.py_repr(),
            second.line + 1,
            second.column + 1,
            first.line + 1,
            first.column + 1,
        ),
    }
}

/// `SafeConstructor.flatten_mapping`: a single mapping's pairs in their
/// own order, or -- for a sequence of mappings -- each mapping's pairs in
/// *reverse* list order (so, once folded with later entries overwriting
/// earlier ones, the *first* mapping in the list wins a conflict --
/// confirmed: `<<: [*b1, *b2]` with both defining `x` keeps `*b1`'s `x`),
/// ahead of the mapping's own pairs, regardless of where `<<:`
/// appears syntactically. Recurses into a merge source's *own* nested
/// `<<:` (PyYAML's `flatten_mapping` does too, structurally, without
/// running the duplicate-key check on that source's own pairs -- that
/// check only ever runs for a mapping reached as a value in its own
/// right, never through a merge).
fn flatten_pairs<'a>(
    pairs: &'a [(Rc<Node>, Rc<Node>)],
) -> Result<Vec<(&'a Node, &'a Node)>, YamlError> {
    let mut merged: Vec<(&'a Node, &'a Node)> = Vec::new();
    let mut own: Vec<(&'a Node, &'a Node)> = Vec::new();
    for (key, value) in pairs {
        if key.is_merge_key() {
            expand_merge_node(value, &mut merged)?;
        } else {
            own.push((key.as_ref(), value.as_ref()));
        }
    }
    let mut flat = merged;
    flat.extend(own);
    Ok(flat)
}

fn expand_merge_node<'a>(
    value: &'a Node,
    flat: &mut Vec<(&'a Node, &'a Node)>,
) -> Result<(), YamlError> {
    match value {
        Node::Mapping { pairs, .. } => {
            flat.extend(flatten_pairs(pairs)?);
            Ok(())
        }
        Node::Sequence { items, .. } => {
            // PyYAML's `flatten_mapping` validates each item in forward
            // order (raising on the *first* non-mapping item, not the
            // last), reversing only the already-valid groups afterwards
            // for the merge-order semantics below.
            let mut submerge: Vec<Vec<(&'a Node, &'a Node)>> = Vec::new();
            for item in items {
                match item.as_ref() {
                    Node::Mapping { pairs, .. } => submerge.push(flatten_pairs(pairs)?),
                    other => return Err(YamlError::InvalidMerge { mark: other.mark() }),
                }
            }
            for group in submerge.into_iter().rev() {
                flat.extend(group);
            }
            Ok(())
        }
        other => Err(YamlError::InvalidMerge { mark: other.mark() }),
    }
}

/// The result of eagerly dispatching on a node the way
/// `BaseConstructor.construct_object` does: a scalar's constructor is
/// never a generator, so its value is always produced right away; a
/// (validly tagged) sequence/mapping's constructor always *is* one
/// (`construct_yaml_seq`/`_map`), so its own processing is always
/// deferred one level -- `node` is queued into `next_round` and this
/// returns [`Eager::Container`] instead of a value.
enum Eager {
    Scalar(Value),
    Container,
}

fn eager_check<'a>(node: &'a Node, next_round: &mut Vec<&'a Node>) -> Result<Eager, YamlError> {
    match node {
        Node::Scalar { text, tag, mark } => {
            Ok(Eager::Scalar(resolve_scalar_value(text, tag, *mark)?))
        }
        Node::Sequence { tag, mark, .. } => {
            if tag != scalar::TAG_SEQ {
                return Err(unresolvable(tag, *mark));
            }
            next_round.push(node);
            Ok(Eager::Container)
        }
        Node::Mapping { tag, mark, .. } => {
            if tag != scalar::TAG_MAP {
                return Err(unresolvable(tag, *mark));
            }
            next_round.push(node);
            Ok(Eager::Container)
        }
    }
}

/// [`eager_check`], but for a node specifically being resolved *as a
/// mapping key*: `flatten_mapping`'s own per-pair loop retags a bare
/// `=` key (the `value` tag, standing alone) to `str` wherever it finds
/// one -- not just inside a merge source, every mapping -- mutating
/// that key's tag in place before it's ever constructed
/// (`constructor.py:208`: `key_node.tag = 'tag:yaml.org,2002:str'`). `=`
/// as a *value* is unaffected and still has no constructor (the
/// existing `value_and_merge_tag_errors` corpus case).
fn eager_check_key<'a>(node: &'a Node, next_round: &mut Vec<&'a Node>) -> Result<Eager, YamlError> {
    if let Node::Scalar { text, tag, mark } = node {
        if tag == scalar::TAG_VALUE {
            return Ok(Eager::Scalar(resolve_scalar_value(
                text,
                scalar::TAG_STR,
                *mark,
            )?));
        }
    }
    eager_check(node, next_round)
}

/// Replays `construct_document`'s breadth-first error order (module
/// docs): processes the whole tree one nesting level at a time, so that
/// -- matching Circuitry's `_UniqueKeyLoader.construct_mapping` -- a
/// shallower mapping's own duplicate-key check always runs, for all of
/// its own keys, before any nested mapping/sequence's body is even
/// looked at, let alone constructed.
fn validate_order(root: &Node) -> Result<(), YamlError> {
    let mut round: Vec<&Node> = Vec::new();
    eager_check(root, &mut round)?;
    while !round.is_empty() {
        let mut next_round: Vec<&Node> = Vec::new();
        for node in &round {
            process_container_order(node, &mut next_round)?;
        }
        round = next_round;
    }
    Ok(())
}

fn process_container_order<'a>(
    node: &'a Node,
    next_round: &mut Vec<&'a Node>,
) -> Result<(), YamlError> {
    match node {
        Node::Sequence { items, .. } => {
            for item in items {
                eager_check(item, next_round)?;
            }
            Ok(())
        }
        Node::Mapping { pairs, .. } => {
            // `_UniqueKeyLoader.construct_mapping`'s own first pass: every
            // own (non-merge) key, constructed and checked for a repeat,
            // before `flatten_mapping`/the real build below even run.
            let mut seen = SeenKeys::default();
            for (key, _) in pairs {
                if key.is_merge_key() {
                    continue;
                }
                // Not `eager_check_key`: `_UniqueKeyLoader.construct_mapping`'s
                // own pre-pass (what this loop replays) calls
                // `construct_object` on the raw key *before*
                // `super().construct_mapping()` -- and the `flatten_mapping`
                // call inside it -- ever runs, so a bare `=` reached as
                // this mapping's own key here still fails exactly like a
                // non-merge value tag normally would; only a key that
                // survives to the flattened loop below (reached *through*
                // a merge, never iterated by this pre-pass at all) gets
                // retagged (`eager_check_key`'s doc comment).
                if let Eager::Scalar(key_value) = eager_check(key, next_round)? {
                    if let Err(first) = seen.check_and_insert(key_value.clone(), key.mark()) {
                        return Err(duplicate_key_error(&key_value, first, key.mark()));
                    }
                }
            }
            let flattened = flatten_pairs(pairs)?;
            for &(key, value) in &flattened {
                match eager_check_key(key, next_round)? {
                    Eager::Scalar(_) => {}
                    Eager::Container => return Err(YamlError::UnhashableKey { mark: key.mark() }),
                }
                eager_check(value, next_round)?;
            }
            Ok(())
        }
        Node::Scalar { .. } => unreachable!("a scalar is never queued as a container"),
    }
}

/// [`construct`], but for a node specifically being resolved *as a
/// mapping key* -- see [`eager_check_key`]'s doc comment.
fn construct_key(node: &Node) -> Result<Value, YamlError> {
    if let Node::Scalar { text, tag, mark } = node {
        if tag == scalar::TAG_VALUE {
            return resolve_scalar_value(text, scalar::TAG_STR, *mark);
        }
    }
    construct(node)
}

/// The ordinary, depth-first build -- used once [`validate_order`] has
/// already walked the whole tree in Circuitry's own error order and
/// found it clean, so in practice this never itself raises.
fn construct(node: &Node) -> Result<Value, YamlError> {
    match node {
        Node::Scalar { text, tag, mark } => resolve_scalar_value(text, tag, *mark),
        Node::Sequence {
            tag, items, mark, ..
        } => {
            if tag != scalar::TAG_SEQ {
                return Err(unresolvable(tag, *mark));
            }
            items
                .iter()
                .map(|item| construct(item))
                .collect::<Result<_, _>>()
                .map(Value::List)
        }
        Node::Mapping {
            tag, pairs, mark, ..
        } => {
            if tag != scalar::TAG_MAP {
                return Err(unresolvable(tag, *mark));
            }
            construct_mapping_node(pairs)
        }
    }
}

fn construct_mapping_node(pairs: &[(Rc<Node>, Rc<Node>)]) -> Result<Value, YamlError> {
    let mut seen = SeenKeys::default();
    for (key, _) in pairs {
        if key.is_merge_key() {
            continue;
        }
        // Not `construct_key`: see the matching comment in
        // `process_container_order`.
        let key_value = construct(key)?;
        if matches!(key_value, Value::List(_) | Value::Dict(_)) {
            continue;
        }
        if let Err(first) = seen.check_and_insert(key_value.clone(), key.mark()) {
            return Err(duplicate_key_error(&key_value, first, key.mark()));
        }
    }

    let flattened = flatten_pairs(pairs)?;
    let mut dict = Dict::new();
    for &(key, value) in &flattened {
        let key_value = construct_key(key)?;
        if matches!(key_value, Value::List(_) | Value::Dict(_)) {
            return Err(YamlError::UnhashableKey { mark: key.mark() });
        }
        let value = construct(value)?;
        dict.insert(key_value, value);
    }
    Ok(Value::Dict(dict))
}

/// [`construct`], but for [`load_yaml_last_key_wins`]: skips the
/// duplicate-own-key pre-check [`construct_mapping_node`] does, so a
/// repeated mapping key silently keeps its last value (ordinary `dict`
/// construction, not `_UniqueKeyLoader`'s) instead of erroring.
fn construct_lenient(node: &Node) -> Result<Value, YamlError> {
    match node {
        Node::Scalar { text, tag, mark } => resolve_scalar_value(text, tag, *mark),
        Node::Sequence {
            tag, items, mark, ..
        } => {
            if tag != scalar::TAG_SEQ {
                return Err(unresolvable(tag, *mark));
            }
            items
                .iter()
                .map(|item| construct_lenient(item))
                .collect::<Result<_, _>>()
                .map(Value::List)
        }
        Node::Mapping {
            tag, pairs, mark, ..
        } => {
            if tag != scalar::TAG_MAP {
                return Err(unresolvable(tag, *mark));
            }
            construct_mapping_node_lenient(pairs)
        }
    }
}

/// [`construct_key`]'s [`construct_lenient`] counterpart.
fn construct_key_lenient(node: &Node) -> Result<Value, YamlError> {
    if let Node::Scalar { text, tag, mark } = node {
        if tag == scalar::TAG_VALUE {
            return resolve_scalar_value(text, scalar::TAG_STR, *mark);
        }
    }
    construct_lenient(node)
}

/// [`construct_mapping_node`], minus its own-key duplicate pre-check:
/// the flatten-and-insert loop below already overwrites a repeated
/// key's value with the later one by itself (plain `Dict::insert`), so
/// skipping the pre-check is the whole difference -- last key wins,
/// the way plain `yaml.safe_load` (no `_UniqueKeyLoader`) builds a
/// `dict` literal with a repeated key.
fn construct_mapping_node_lenient(pairs: &[(Rc<Node>, Rc<Node>)]) -> Result<Value, YamlError> {
    let flattened = flatten_pairs(pairs)?;
    let mut dict = Dict::new();
    for &(key, value) in &flattened {
        let key_value = construct_key_lenient(key)?;
        if matches!(key_value, Value::List(_) | Value::Dict(_)) {
            return Err(YamlError::UnhashableKey { mark: key.mark() });
        }
        let value = construct_lenient(value)?;
        dict.insert(key_value, value);
    }
    Ok(Value::Dict(dict))
}

#[cfg(test)]
mod last_key_wins_tests {
    use super::{load_yaml, load_yaml_last_key_wins};
    use electricity_value::Value;

    /// Second review of #415, finding 4: `load_yaml_last_key_wins`
    /// keeps the *last* value for a repeated own key, at the *first*
    /// occurrence's position -- plain `dict` literal semantics, not
    /// `_UniqueKeyLoader`'s. A corpus case with two *equal* repeated
    /// values (`name: s` twice) doesn't actually pin this; this does.
    #[test]
    fn duplicate_key_with_different_values_keeps_the_last_value_at_the_first_position() {
        let value = load_yaml_last_key_wins("a: 1\nb: 2\na: 3\n").expect("lenient load");
        let dict = value.as_dict().expect("a mapping");
        assert_eq!(
            dict.keys().map(Value::as_str).collect::<Vec<_>>(),
            vec![Some("a"), Some("b")],
            "the repeated key keeps its first position"
        );
        assert_eq!(
            dict.get(&Value::Str("a".to_string())),
            Some(&Value::from(3_i64))
        );
        assert_eq!(
            dict.get(&Value::Str("b".to_string())),
            Some(&Value::from(2_i64))
        );
    }

    #[test]
    fn merge_key_plus_an_overriding_own_key() {
        let value =
            load_yaml_last_key_wins("base: &b\n  a: 1\n  b: 2\nmerged:\n  <<: *b\n  a: 99\n")
                .expect("lenient load");
        let dict = value.as_dict().expect("a mapping");
        let merged = dict
            .get(&Value::Str("merged".to_string()))
            .and_then(Value::as_dict)
            .expect("merged is a mapping");
        assert_eq!(
            merged.get(&Value::Str("a".to_string())),
            Some(&Value::from(99_i64)),
            "the own key overrides the merged-in value"
        );
        assert_eq!(
            merged.get(&Value::Str("b".to_string())),
            Some(&Value::from(2_i64)),
            "a merged-in key not overridden still comes through"
        );
    }

    /// A bare `=` own key: `load_yaml_last_key_wins` loads it as the
    /// literal string `"="` (plain `yaml.safe_load`'s own behaviour --
    /// no `flatten_mapping` pre-pass retags it, so it is never anything
    /// but an ordinary scalar key); `load_yaml`'s own duplicate-key
    /// pre-pass hits it first, still tagged as YAML's "value" tag
    /// (`tag:yaml.org,2002:value`, which has no scalar constructor of
    /// its own), and errors.
    #[test]
    fn bare_equals_own_key_loads_as_the_string_where_load_yaml_errors() {
        let text = "m:\n  =: 1\n";

        let value = load_yaml_last_key_wins(text).expect("lenient load");
        let inner = value
            .as_dict()
            .expect("a mapping")
            .get(&Value::Str("m".to_string()))
            .and_then(Value::as_dict)
            .expect("m is a mapping");
        assert_eq!(
            inner.get(&Value::Str("=".to_string())),
            Some(&Value::from(1_i64)),
            "a bare '=' own key loads as the literal string \"=\""
        );

        assert!(
            load_yaml(text).is_err(),
            "load_yaml's own duplicate-key pre-pass does not retag a bare '=' key, \
             so it reaches resolve_scalar_value still tagged as YAML's value tag"
        );
    }
}
