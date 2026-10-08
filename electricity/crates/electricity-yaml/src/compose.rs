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

type Input<'input> = StrInput<'input>;

/// `yaml.safe_load(text)`, minus silent duplicate keys: `core/yaml_load.py`'s
/// `load_yaml`.
pub fn load_yaml(text: &str) -> Result<Value, YamlError> {
    let mut parser = Parser::new_from_str(text);
    let mut last_event_end = Marker::new(0, 1, 0);

    let (first, span) = pull(&mut parser, &mut last_event_end)?;
    debug_assert!(matches!(first, Event::StreamStart));

    let (second, _span) = pull(&mut parser, &mut last_event_end)?;
    match second {
        // No document at all (empty input, or only comments/whitespace):
        // `yaml.safe_load` returns `None` for this (confirmed empirically).
        Event::StreamEnd => Ok(Value::None),
        Event::DocumentStart(_) => {
            let mut composer = Composer::new(text);
            let root = composer.compose_node(&mut parser, &mut last_event_end)?;

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
fn advance_marker(start: Marker, gap: &str, upto: usize) -> Marker {
    let mut index = start.index();
    let mut line = start.line();
    let mut col = start.col();
    for ch in gap[..upto].chars() {
        index += ch.len_utf8();
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
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
#[derive(Clone)]
enum Node {
    Scalar {
        text: String,
        tag: String,
        mark: Mark,
    },
    Sequence {
        tag: String,
        items: Vec<Node>,
        mark: Mark,
    },
    Mapping {
        tag: String,
        pairs: Vec<(Node, Node)>,
        mark: Mark,
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

    fn is_merge_key(&self) -> bool {
        matches!(self, Node::Scalar { tag, .. } if tag == scalar::TAG_MERGE)
    }
}

struct Composer<'t> {
    text: &'t str,
    anchors: HashMap<usize, Node>,
    anchor_marks: HashMap<String, Mark>,
}

impl<'t> Composer<'t> {
    fn new(text: &'t str) -> Self {
        Composer {
            text,
            anchors: HashMap::new(),
            anchor_marks: HashMap::new(),
        }
    }

    /// The node's true start mark (anchor/tag-inclusive) and, if it
    /// carries an anchor, that anchor's name -- both recovered from the
    /// raw text between `last_event_end` and `span.start`, the one place
    /// an anchor/tag token's own position still exists once
    /// `saphyr-parser` has reduced it to just an id/a `Tag` on the event
    /// (see the module docs and [`find_prefix_start`]).
    fn node_prefix(
        &self,
        span: Span,
        last_event_end: Marker,
        has_anchor: bool,
    ) -> (Mark, Option<String>) {
        let gap_start = last_event_end.index();
        let gap_end = span.start.index();
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

    /// Registers `node`'s anchor (if any), rejecting a reused anchor
    /// name the way PyYAML's `Composer.compose_node` does (`found
    /// duplicate anchor`) -- saphyr-parser's own anchor table allows
    /// reuse outright (its duplicate check is commented out), so this
    /// crate owns the check instead.
    fn register_anchor(
        &mut self,
        anchor_id: usize,
        name: Option<String>,
        node: &Node,
    ) -> Result<(), YamlError> {
        if anchor_id == 0 {
            return Ok(());
        }
        if let Some(name) = name {
            if let Some(&first_mark) = self.anchor_marks.get(&name) {
                return Err(YamlError::Scan {
                    message: format!(
                        "found duplicate anchor {name:?}; first occurrence at line {}, column {}",
                        first_mark.line + 1,
                        first_mark.column + 1
                    ),
                    mark: node.mark(),
                });
            }
            self.anchor_marks.insert(name, node.mark());
        }
        self.anchors.insert(anchor_id, node.clone());
        Ok(())
    }

    fn compose_node<'input>(
        &mut self,
        parser: &mut Parser<'input, Input<'input>>,
        last_event_end: &mut Marker,
    ) -> Result<Node, YamlError> {
        let before = *last_event_end;
        let (event, span) = pull(parser, last_event_end)?;
        self.compose_from_event(event, span, before, parser, last_event_end)
    }

    fn compose_from_event<'input>(
        &mut self,
        event: Event<'input>,
        span: Span,
        before: Marker,
        parser: &mut Parser<'input, Input<'input>>,
        last_event_end: &mut Marker,
    ) -> Result<Node, YamlError> {
        match event {
            Event::Alias(id) => self.anchors.get(&id).cloned().ok_or_else(|| {
                // saphyr-parser itself rejects a genuinely undefined
                // anchor name before this point; reaching here means the
                // anchor is still being composed (a circular reference).
                // PyYAML registers an anchor before composing its own
                // children, so it builds a real recursive structure
                // instead -- an unsupported, known divergence, exercised
                // by the golden corpus's `known_divergence_cases`
                // (`generate_yaml_corpus.py`) and checked loosely (by
                // message, not by value) in `golden_corpus.rs`.
                YamlError::Scan {
                    message: "self-referential anchor is not supported".to_string(),
                    mark: span.start.into(),
                }
            }),
            Event::Scalar(text, style, anchor_id, tag) => {
                let (mark, name) = self.node_prefix(span, before, anchor_id != 0);
                let tag_str = scalar_tag(text.as_ref(), style, tag.as_deref());
                let node = Node::Scalar {
                    text: text.into_owned(),
                    tag: tag_str,
                    mark,
                };
                self.register_anchor(anchor_id, name, &node)?;
                Ok(node)
            }
            Event::SequenceStart(anchor_id, tag) => {
                let (mark, name) = self.node_prefix(span, before, anchor_id != 0);
                let tag_str = container_tag(tag.as_deref(), scalar::TAG_SEQ);
                let mut items = Vec::new();
                loop {
                    let item_before = *last_event_end;
                    let (ev, sp) = pull(parser, last_event_end)?;
                    if matches!(ev, Event::SequenceEnd) {
                        break;
                    }
                    items.push(self.compose_from_event(
                        ev,
                        sp,
                        item_before,
                        parser,
                        last_event_end,
                    )?);
                }
                let node = Node::Sequence {
                    tag: tag_str,
                    items,
                    mark,
                };
                self.register_anchor(anchor_id, name, &node)?;
                Ok(node)
            }
            Event::MappingStart(anchor_id, tag) => {
                let (mark, name) = self.node_prefix(span, before, anchor_id != 0);
                let tag_str = container_tag(tag.as_deref(), scalar::TAG_MAP);
                let mut pairs = Vec::new();
                loop {
                    let key_before = *last_event_end;
                    let (kev, ksp) = pull(parser, last_event_end)?;
                    if matches!(kev, Event::MappingEnd) {
                        break;
                    }
                    let key =
                        self.compose_from_event(kev, ksp, key_before, parser, last_event_end)?;
                    let value = self.compose_node(parser, last_event_end)?;
                    pairs.push((key, value));
                }
                let node = Node::Mapping {
                    tag: tag_str,
                    pairs,
                    mark,
                };
                self.register_anchor(anchor_id, name, &node)?;
                Ok(node)
            }
            other => unreachable_event(&other),
        }
    }
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
fn flatten_pairs<'a>(pairs: &'a [(Node, Node)]) -> Result<Vec<(&'a Node, &'a Node)>, YamlError> {
    let mut merged: Vec<(&'a Node, &'a Node)> = Vec::new();
    let mut own: Vec<(&'a Node, &'a Node)> = Vec::new();
    for (key, value) in pairs {
        if key.is_merge_key() {
            expand_merge_node(value, &mut merged)?;
        } else {
            own.push((key, value));
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
                match item {
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
                if let Eager::Scalar(key_value) = eager_check(key, next_round)? {
                    if let Err(first) = seen.check_and_insert(key_value.clone(), key.mark()) {
                        return Err(duplicate_key_error(&key_value, first, key.mark()));
                    }
                }
            }
            let flattened = flatten_pairs(pairs)?;
            for &(key, value) in &flattened {
                match eager_check(key, next_round)? {
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

/// The ordinary, depth-first build -- used once [`validate_order`] has
/// already walked the whole tree in Circuitry's own error order and
/// found it clean, so in practice this never itself raises.
fn construct(node: &Node) -> Result<Value, YamlError> {
    match node {
        Node::Scalar { text, tag, mark } => resolve_scalar_value(text, tag, *mark),
        Node::Sequence { tag, items, mark } => {
            if tag != scalar::TAG_SEQ {
                return Err(unresolvable(tag, *mark));
            }
            items
                .iter()
                .map(construct)
                .collect::<Result<_, _>>()
                .map(Value::List)
        }
        Node::Mapping { tag, pairs, mark } => {
            if tag != scalar::TAG_MAP {
                return Err(unresolvable(tag, *mark));
            }
            construct_mapping_node(pairs)
        }
    }
}

fn construct_mapping_node(pairs: &[(Node, Node)]) -> Result<Value, YamlError> {
    let mut seen = SeenKeys::default();
    for (key, _) in pairs {
        if key.is_merge_key() {
            continue;
        }
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
        let key_value = construct(key)?;
        if matches!(key_value, Value::List(_) | Value::Dict(_)) {
            return Err(YamlError::UnhashableKey { mark: key.mark() });
        }
        let value = construct(value)?;
        dict.insert(key_value, value);
    }
    Ok(Value::Dict(dict))
}
