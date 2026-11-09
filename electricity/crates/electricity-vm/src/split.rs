//! Deterministic context splitting for the size trigger (`core/split.py`,
//! issue #672/#682) -- pure functions only, no template rendering, no
//! state, no I/O. Given a container value (a list, a mapping, or a
//! string) and a *room* budget in estimated tokens, [`split_list`]/
//! [`split_mapping`]/[`split_text`] produce whole-unit, in-order parts,
//! each of which -- rendered the same way Mustache renders a bare
//! `{{{x}}}`/`{{x}}` tag, i.e. `str(value)` for a non-string value, the
//! value itself for a string -- estimates to no more than *room* tokens.
//!
//! [`estimate_tokens`] duplicates `core/complexity.py::_estimate_tokens`'s
//! own ceiling-division-by-4, exactly like the Python module it mirrors
//! duplicates it independently of `core.complexity`, so the two must be
//! kept in sync (pinned by a test alongside `electricity-complexity`'s own
//! estimator -- see `decompose.rs`'s own doc comment).
//!
//! A single unit -- one list item, one mapping entry, or (after every
//! level of text-structure splitting has been tried) one character --
//! that alone does not fit *room* returns [`NoSplitPoint`]; splitting is
//! otherwise always possible for text (the final, structural fallback is
//! a hard cut at `room * 4` characters, which always fits by
//! construction) but is a real, reported failure for a list/mapping
//! whose one unit is simply too big.
//!
//! Balancing: for *n* = `ceil(total_tokens / room)` parts, a part fills
//! with whole units, in order, up to a *target* of
//! `ceil(total_tokens / n)` tokens -- never *room*, which *n*'s own
//! definition guarantees *target* never exceeds -- so parts come out
//! close to even in size rather than cramming every part to the room
//! ceiling except the last.

use electricity_value::{Dict, Value};
use std::sync::OnceLock;

/// `core/split.py::NoSplitPoint`: a single unit does not fit *room* on
/// its own -- splitting stops here. `0` names what didn't fit:
/// `"item <index>"` for a list, `"key <repr>"` for a mapping, or
/// `"a run of text"` for a string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoSplitPoint(pub String);

impl std::fmt::Display for NoSplitPoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} does not fit the room", self.0)
    }
}

/// `core/split.py::estimate_tokens` -- ceiling division by 4 *code
/// points* (never bytes: a multi-byte character counts once, matching
/// Python's `len(str)`).
pub fn estimate_tokens(text: &str) -> i64 {
    let len = text.chars().count() as i64;
    if len == 0 {
        return 0;
    }
    (len + 3) / 4
}

/// `core/split.py::_pack`: partition `0..unit_count` into whole-unit
/// `[start, end)` ranges. `render_slice(start, end)` is the exact text a
/// part spanning that range would render to, measured directly at every
/// candidate boundary.
fn pack<F, L>(
    unit_count: usize,
    render_slice: F,
    label_at: L,
    room: i64,
) -> Result<Vec<(usize, usize)>, NoSplitPoint>
where
    F: Fn(usize, usize) -> String,
    L: Fn(usize) -> String,
{
    if unit_count == 0 {
        return Ok(Vec::new());
    }

    let total_tokens = estimate_tokens(&render_slice(0, unit_count));
    let n = if room > 0 {
        std::cmp::max(1, ceil_div(total_tokens, room))
    } else {
        unit_count as i64
    };
    let target = if n > 0 {
        ceil_div(total_tokens, n)
    } else {
        total_tokens
    };

    let mut ranges = Vec::new();
    let mut start = 0usize;
    let mut end = 0usize;
    while end < unit_count {
        if end == start {
            if estimate_tokens(&render_slice(start, start + 1)) > room {
                return Err(NoSplitPoint(label_at(start)));
            }
            end = start + 1;
            continue;
        }
        if estimate_tokens(&render_slice(start, end + 1)) > target {
            ranges.push((start, end));
            start = end;
            continue;
        }
        end += 1;
    }
    ranges.push((start, end));
    Ok(ranges)
}

fn ceil_div(total: i64, divisor: i64) -> i64 {
    if divisor <= 0 {
        return total;
    }
    if total <= 0 {
        return 0;
    }
    (total + divisor - 1) / divisor
}

/// `core/split.py::split_list` -- cut *items* into contiguous, whole-item,
/// balanced slices.
pub fn split_list(items: &[Value], room: i64) -> Result<Vec<Vec<Value>>, NoSplitPoint> {
    if items.is_empty() {
        return Ok(Vec::new());
    }
    if room < 1 {
        return Err(NoSplitPoint("item 0".to_string()));
    }
    let render_slice = |start: usize, end: usize| Value::List(items[start..end].to_vec()).py_repr();
    let ranges = pack(items.len(), render_slice, |i| format!("item {i}"), room)?;
    Ok(ranges
        .into_iter()
        .map(|(start, end)| items[start..end].to_vec())
        .collect())
}

/// `core/split.py::split_mapping` -- cut *mapping* into contiguous,
/// whole-entry, balanced groups, in stored key order.
pub fn split_mapping(mapping: &Dict, room: i64) -> Result<Vec<Dict>, NoSplitPoint> {
    let keys: Vec<Value> = mapping.keys().cloned().collect();
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    if room < 1 {
        return Err(NoSplitPoint(format!("key {}", keys[0].py_repr())));
    }
    let slice_dict = |start: usize, end: usize| -> Dict {
        let mut d = Dict::new();
        for key in &keys[start..end] {
            d.insert(
                key.clone(),
                mapping.get(key).cloned().unwrap_or(Value::None),
            );
        }
        d
    };
    let render_slice = |start: usize, end: usize| Value::Dict(slice_dict(start, end)).py_repr();
    let label_at = |i: usize| format!("key {}", keys[i].py_repr());
    let ranges = pack(keys.len(), render_slice, label_at, room)?;
    Ok(ranges
        .into_iter()
        .map(|(start, end)| slice_dict(start, end))
        .collect())
}

fn blank_line_re() -> &'static fancy_regex::Regex {
    static RE: OnceLock<fancy_regex::Regex> = OnceLock::new();
    RE.get_or_init(|| fancy_regex::Regex::new(r"\n[ \t]*\n+").expect("fixed pattern"))
}

fn newline_re() -> &'static fancy_regex::Regex {
    static RE: OnceLock<fancy_regex::Regex> = OnceLock::new();
    RE.get_or_init(|| fancy_regex::Regex::new(r"\n").expect("fixed pattern"))
}

fn sentence_re() -> &'static fancy_regex::Regex {
    static RE: OnceLock<fancy_regex::Regex> = OnceLock::new();
    RE.get_or_init(|| fancy_regex::Regex::new(r"[.?!] (?=[A-Z\n])").expect("fixed pattern"))
}

fn whitespace_re() -> &'static fancy_regex::Regex {
    static RE: OnceLock<fancy_regex::Regex> = OnceLock::new();
    RE.get_or_init(|| fancy_regex::Regex::new(r"\s+").expect("fixed pattern"))
}

/// `core/split.py::_split_keep_leading`: *text* cut right after each
/// *pattern* match; concatenating the returned chunks reconstructs
/// *text* exactly.
fn split_keep_leading(text: &str, re: &fancy_regex::Regex) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut pos = 0usize;
    for found in re.find_iter(text) {
        let Ok(m) = found else { continue };
        let end = m.end();
        if end <= pos {
            continue;
        }
        chunks.push(text[pos..end].to_string());
        pos = end;
    }
    if pos < text.len() {
        chunks.push(text[pos..].to_string());
    }
    chunks
}

/// `core/split.py::_atomic_units_at`'s own level chain: blank lines,
/// single newlines, sentence ends, whitespace, then a hard character
/// cut (level 4, terminal).
fn atomic_units_at(text: &str, room: i64, level: usize) -> Vec<String> {
    if estimate_tokens(text) <= room {
        return vec![text.to_string()];
    }
    if level >= 4 {
        let chunk_chars = std::cmp::max(1, room * 4) as usize;
        let chars: Vec<char> = text.chars().collect();
        let mut units = Vec::new();
        let mut i = 0usize;
        while i < chars.len() {
            let end = std::cmp::min(i + chunk_chars, chars.len());
            units.push(chars[i..end].iter().collect::<String>());
            i = end;
        }
        return units;
    }
    let re = match level {
        0 => blank_line_re(),
        1 => newline_re(),
        2 => sentence_re(),
        _ => whitespace_re(),
    };
    let pieces = split_keep_leading(text, re);
    if pieces.len() <= 1 {
        return atomic_units_at(text, room, level + 1);
    }
    let mut units = Vec::new();
    for piece in pieces {
        units.extend(atomic_units_at(&piece, room, level + 1));
    }
    units
}

/// `core/split.py::split_text` -- cut *text* into balanced,
/// whole-structural-unit, in-order parts.
pub fn split_text(text: &str, room: i64) -> Result<Vec<String>, NoSplitPoint> {
    if text.is_empty() {
        return Ok(Vec::new());
    }
    if room < 1 {
        return Err(NoSplitPoint("a run of text".to_string()));
    }
    let units = atomic_units_at(text, room, 0);
    let render_slice = |start: usize, end: usize| units[start..end].concat();
    let ranges = pack(
        units.len(),
        render_slice,
        |_| "a run of text".to_string(),
        room,
    )?;
    Ok(ranges
        .into_iter()
        .map(|(start, end)| units[start..end].concat())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_tokens_is_ceiling_division_by_four_code_points() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("a"), 1);
        assert_eq!(estimate_tokens("abcd"), 1);
        assert_eq!(estimate_tokens("abcde"), 2);
        // multi-byte: each counts as one code point, not one byte.
        assert_eq!(estimate_tokens("日本語語"), 1);
    }

    #[test]
    fn split_list_balances_whole_items() {
        let items: Vec<Value> = (0..10).map(Value::from).collect();
        let parts = split_list(&items, 5).unwrap();
        assert!(parts.len() >= 2);
        let total: usize = parts.iter().map(Vec::len).sum();
        assert_eq!(total, items.len());
    }

    #[test]
    fn split_list_empty_is_empty() {
        assert_eq!(split_list(&[], 10).unwrap(), Vec::<Vec<Value>>::new());
    }

    #[test]
    fn split_list_room_below_one_refuses_item_zero() {
        let items = vec![Value::from(1i64)];
        let err = split_list(&items, 0).unwrap_err();
        assert_eq!(err.0, "item 0");
    }

    #[test]
    fn split_list_one_oversized_item_refuses_by_index() {
        let items = vec![Value::Str("x".to_string()), Value::Str("y".repeat(100))];
        let err = split_list(&items, 2).unwrap_err();
        assert_eq!(err.0, "item 1");
    }

    #[test]
    fn split_mapping_balances_whole_entries() {
        let mut d = Dict::new();
        for i in 0..10 {
            d.insert(Value::Str(format!("k{i}")), Value::from(i));
        }
        let parts = split_mapping(&d, 10).unwrap();
        assert!(parts.len() >= 2);
        let total: usize = parts.iter().map(Dict::len).sum();
        assert_eq!(total, d.len());
    }

    #[test]
    fn split_text_whitespace_level_reconstructs_exactly() {
        let text = "a b c d e f g h i j k l m n o p";
        let parts = split_text(text, 4).unwrap();
        assert_eq!(parts.join(""), text);
        assert!(parts.len() >= 2);
    }

    #[test]
    fn split_text_hard_cut_never_splits_a_multibyte_char() {
        let text = "日".repeat(20);
        let parts = split_text(&text, 1).unwrap();
        for part in &parts {
            assert!(part.chars().count() <= 4);
        }
        assert_eq!(parts.concat(), text);
    }

    #[test]
    fn split_text_room_below_one_refuses() {
        let err = split_text("x", 0).unwrap_err();
        assert_eq!(err.0, "a run of text");
    }

    #[test]
    fn split_text_empty_is_empty() {
        assert_eq!(split_text("", 10).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn split_text_blank_lines_split_before_sentences() {
        let text = "Para one.\n\nPara two.\n\nPara three.";
        let parts = split_text(text, 3).unwrap();
        assert_eq!(parts.join(""), text);
    }
}
