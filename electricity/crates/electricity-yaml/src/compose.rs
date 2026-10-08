//! The composer: walks `saphyr-parser`'s event stream into a `Value`,
//! applying PyYAML's scalar resolution ([`crate::scalar`]), anchors and
//! aliases, `<<:` merge keys, and Circuitry's duplicate-key check
//! (runtime-semantics §1.1, DESIGN.md §3.2).

use crate::error::{Mark, YamlError};
use crate::scalar;
use electricity_value::{Dict, Value};
use saphyr_parser::{Event, Parser, ScalarStyle, Span, StrInput, Tag};
use std::collections::HashMap;

type Input<'input> = StrInput<'input>;

/// `yaml.safe_load(text)`, minus silent duplicate keys: `core/yaml_load.py`'s
/// `load_yaml`.
pub fn load_yaml(text: &str) -> Result<Value, YamlError> {
    let mut parser = Parser::new_from_str(text);
    let (first, _) = next_event(&mut parser)?;
    debug_assert!(matches!(first, Event::StreamStart));

    let (second, _span) = next_event(&mut parser)?;
    match second {
        // No document at all (empty input, or only comments/whitespace):
        // `yaml.safe_load` returns `None` for this (confirmed empirically).
        Event::StreamEnd => Ok(Value::None),
        Event::DocumentStart(_) => {
            let mut composer = Composer::new();
            let value = composer.compose_node(&mut parser)?;

            let (end, _) = next_event(&mut parser)?;
            debug_assert!(matches!(end, Event::DocumentEnd));

            let (after, after_span) = next_event(&mut parser)?;
            match after {
                Event::StreamEnd => Ok(value),
                Event::DocumentStart(_) => Err(YamlError::MultipleDocuments {
                    mark: after_span.start.into(),
                }),
                other => unreachable_event(&other),
            }
        }
        other => unreachable_event(&other),
    }
}

fn unreachable_event(ev: &Event<'_>) -> ! {
    unreachable!("unexpected top-level event from saphyr-parser: {ev:?}")
}

fn next_event<'input>(
    parser: &mut Parser<'input, Input<'input>>,
) -> Result<(Event<'input>, Span), YamlError> {
    match parser.next() {
        Some(Ok(pair)) => Ok(pair),
        Some(Err(err)) => Err(YamlError::Scan {
            message: err.info().to_string(),
            mark: (*err.marker()).into(),
        }),
        None => unreachable!("saphyr-parser's stream ended without StreamEnd"),
    }
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

/// `true` for the bare, non-specific tag (`!` alone, with no suffix) —
/// distinct from an *absent* tag, even though both end up resolving the
/// same way below. PyYAML runs full implicit resolution on its text for
/// `!` regardless of scalar style (confirmed: `! on`, quoted or plain,
/// both resolve to the bool `True` — only an absent tag restricts
/// implicit resolution to plain-style scalars).
fn is_non_specific(tag: &Tag) -> bool {
    tag.handle.is_empty() && tag.suffix == "!"
}

/// The tag a scalar resolves to: an explicit, specific tag always wins.
/// Otherwise (no tag, or the bare non-specific `!`) a plain scalar
/// resolves implicitly; quoted/literal/folded scalars are `str` — except
/// that the bare `!` forces implicit resolution even then
/// (runtime-semantics §1.1; known, narrow divergence: unlike CPython's
/// `re`, this crate's regex `$` does not also match just before a
/// trailing `\n`, so `! |\n  on\n` — a construct that already raises a
/// bare `KeyError` under Circuitry's real loader — resolves to the
/// string `"on\n"` here instead of erroring).
fn scalar_tag(text: &str, style: ScalarStyle, tag: Option<&Tag>) -> String {
    match tag {
        Some(t) if !is_non_specific(t) => canonical_tag(t),
        Some(_) => scalar::implicit_tag(text).to_string(),
        None if style == ScalarStyle::Plain => scalar::implicit_tag(text).to_string(),
        None => scalar::TAG_STR.to_string(),
    }
}

struct Composer {
    anchors: HashMap<usize, Value>,
}

impl Composer {
    fn new() -> Self {
        Composer {
            anchors: HashMap::new(),
        }
    }

    fn register_anchor(&mut self, anchor_id: usize, value: &Value) {
        if anchor_id != 0 {
            self.anchors.insert(anchor_id, value.clone());
        }
    }

    fn compose_node<'input>(
        &mut self,
        parser: &mut Parser<'input, Input<'input>>,
    ) -> Result<Value, YamlError> {
        let (value, _span) = self.compose_node_spanned(parser)?;
        Ok(value)
    }

    fn compose_node_spanned<'input>(
        &mut self,
        parser: &mut Parser<'input, Input<'input>>,
    ) -> Result<(Value, Span), YamlError> {
        let (event, span) = next_event(parser)?;
        let value = self.compose_from_event(event, span, parser)?;
        Ok((value, span))
    }

    fn compose_from_event<'input>(
        &mut self,
        event: Event<'input>,
        span: Span,
        parser: &mut Parser<'input, Input<'input>>,
    ) -> Result<Value, YamlError> {
        match event {
            Event::Alias(id) => self.anchors.get(&id).cloned().ok_or_else(|| {
                // saphyr-parser itself rejects a genuinely undefined
                // anchor name before this point; reaching here means the
                // anchor is still being composed (a circular reference) —
                // an unsupported, documented divergence (DESIGN.md §3.2),
                // not something exercised by the corpus.
                YamlError::Scan {
                    message: "self-referential anchor is not supported".to_string(),
                    mark: span.start.into(),
                }
            }),
            Event::Scalar(text, style, anchor_id, tag) => {
                let value = self.resolve_scalar(text.as_ref(), style, tag.as_deref(), span)?;
                self.register_anchor(anchor_id, &value);
                Ok(value)
            }
            Event::SequenceStart(anchor_id, tag) => {
                validate_tag(tag.as_deref(), scalar::TAG_SEQ, span)?;
                let mut items = Vec::new();
                loop {
                    let (ev, sp) = next_event(parser)?;
                    if matches!(ev, Event::SequenceEnd) {
                        break;
                    }
                    items.push(self.compose_from_event(ev, sp, parser)?);
                }
                let value = Value::List(items);
                self.register_anchor(anchor_id, &value);
                Ok(value)
            }
            Event::MappingStart(anchor_id, tag) => {
                validate_tag(tag.as_deref(), scalar::TAG_MAP, span)?;
                let value = self.compose_mapping(parser)?;
                self.register_anchor(anchor_id, &value);
                Ok(value)
            }
            other => unreachable_event(&other),
        }
    }

    fn resolve_scalar(
        &self,
        text: &str,
        style: ScalarStyle,
        tag: Option<&Tag>,
        span: Span,
    ) -> Result<Value, YamlError> {
        let mark: Mark = span.start.into();
        match scalar_tag(text, style, tag).as_str() {
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
            other => Err(YamlError::UnresolvableTag {
                tag: other.to_string(),
                mark,
            }),
        }
    }

    /// Consumes one mapping's key/value event pairs up to `MappingEnd`,
    /// applies the duplicate-key check to its own (non-`<<:`) keys, then
    /// folds `<<:` merge sources and the mapping's own pairs into one
    /// `Dict` (runtime-semantics §1.1's merge-key rules).
    fn compose_mapping<'input>(
        &mut self,
        parser: &mut Parser<'input, Input<'input>>,
    ) -> Result<Value, YamlError> {
        struct Entry {
            is_merge: bool,
            key: Value,
            key_mark: Mark,
            value: Value,
            value_mark: Mark,
        }

        let mut entries = Vec::new();
        loop {
            let (key_event, key_span) = next_event(parser)?;
            if matches!(key_event, Event::MappingEnd) {
                break;
            }
            let key_mark: Mark = key_span.start.into();
            let is_merge = match &key_event {
                Event::Scalar(text, style, _, tag) => {
                    scalar_tag(text.as_ref(), *style, tag.as_deref()) == scalar::TAG_MERGE
                }
                _ => false,
            };

            let key = if is_merge {
                Value::None
            } else {
                self.compose_from_event(key_event, key_span, parser)?
            };
            if !is_merge && matches!(key, Value::List(_) | Value::Dict(_)) {
                return Err(YamlError::UnhashableKey { mark: key_mark });
            }
            let (value, value_span) = self.compose_node_spanned(parser)?;
            entries.push(Entry {
                is_merge,
                key,
                key_mark,
                value,
                value_mark: value_span.start.into(),
            });
        }

        let mut first_seen: HashMap<Value, Mark> = HashMap::new();
        for entry in &entries {
            if entry.is_merge {
                continue;
            }
            if let Some(prev_mark) = first_seen.get(&entry.key) {
                return Err(duplicate_key_error(&entry.key, *prev_mark, entry.key_mark));
            }
            first_seen.insert(entry.key.clone(), entry.key_mark);
        }

        // `SafeConstructor.flatten_mapping` always prepends every `<<:`
        // entry's expansion ahead of the mapping's own pairs in
        // `node.value`, regardless of where `<<:` appears syntactically
        // (confirmed: a `z:` written *before* `<<:` still ends up after
        // the merged-in keys in the final key order).
        let mut merged: Vec<(Value, Value)> = Vec::new();
        let mut own: Vec<(Value, Value)> = Vec::new();
        for entry in entries {
            if entry.is_merge {
                expand_merge(entry.value, entry.value_mark, &mut merged)?;
            } else {
                own.push((entry.key, entry.value));
            }
        }
        let mut flat = merged;
        flat.extend(own);

        let mut dict = Dict::new();
        for (k, v) in flat {
            dict.insert(k, v);
        }
        Ok(Value::Dict(dict))
    }
}

fn validate_tag(tag: Option<&Tag>, expected: &str, span: Span) -> Result<(), YamlError> {
    match tag {
        None => Ok(()),
        Some(t) if is_non_specific(t) => Ok(()),
        Some(t) => {
            let full = canonical_tag(t);
            if full == expected {
                Ok(())
            } else {
                Err(YamlError::UnresolvableTag {
                    tag: full,
                    mark: span.start.into(),
                })
            }
        }
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

/// Expands one `<<:` entry's value into `flat`, matching
/// `SafeConstructor.flatten_mapping`: a single mapping's pairs in their
/// own order, or — for a sequence of mappings — each mapping's pairs in
/// *reverse* list order (so, once folded with later entries overwriting
/// earlier ones, the *first* mapping in the list wins a conflict —
/// confirmed: `<<: [*b1, *b2]` with both defining `x` keeps `*b1`'s `x`).
fn expand_merge(value: Value, mark: Mark, flat: &mut Vec<(Value, Value)>) -> Result<(), YamlError> {
    match value {
        Value::Dict(d) => {
            flat.extend(d);
            Ok(())
        }
        Value::List(items) => {
            for item in items.into_iter().rev() {
                match item {
                    Value::Dict(d) => flat.extend(d),
                    _ => return Err(YamlError::InvalidMerge { mark }),
                }
            }
            Ok(())
        }
        _ => Err(YamlError::InvalidMerge { mark }),
    }
}
