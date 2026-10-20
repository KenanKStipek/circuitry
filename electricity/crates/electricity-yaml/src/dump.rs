//! `yaml.dump(value, default_flow_style=False)` -- a port of PyYAML's
//! `Emitter`/`Serializer`/`Representer` for the values [`Value`] can
//! hold (issue #506's lane L1, shared with M1-L2's `safe_dump(sort_keys
//! =False)`).
//!
//! Scope, deliberately narrower than PyYAML's own `Dumper`: every
//! caller in this workspace always passes `default_flow_style=False`
//! and never `allow_unicode=True`, so this emitter only ever produces
//! block-style collections (an empty list/dict is still the flow form
//! `[]`/`{}` -- PyYAML's own exception to `default_flow_style=False`,
//! confirmed against the real dumper) and always escapes non-ASCII.
//! `sort_keys` is the one knob callers actually vary (reflector always
//! dumps with it on, matching `yaml.dump`'s own default; L2's
//! `safe_dump` turns it off). No caller in this crate ever asks this
//! emitter to choose a literal/folded block scalar style (`|`/`>`) --
//! PyYAML's own default representer never requests one either (every
//! `ScalarNode` it builds leaves `style=None`, for the emitter's
//! `choose_scalar_style` to pick from plain/single/double only), so
//! this port doesn't implement writing one.
//!
//! The three string styles (plain, single-quoted, double-quoted) are
//! literal ports of `emitter.py`'s `analyze_scalar`/`write_plain`/
//! `write_single_quoted`/`write_double_quoted`, including the width-80
//! greedy word-wrap, the single-quoted blank-line fold for an embedded
//! line break (`n` original breaks become `n+1` written ones, so
//! re-parsing folds them back to `n`), and the double-quoted
//! backslash-continuation fold. Which style a scalar gets uses the
//! same two gates PyYAML's `choose_scalar_style` does: whether the
//! *resolver* would read a plain rendering of this text back as the
//! same type it already is (`implicit_tag`, reused from this crate's
//! own loader) and `analyze_scalar`'s own structural flags (leading/
//! trailing whitespace, block indicators, embedded line breaks,
//! special characters).
//!
//! Block collection layout matches PyYAML's own indentation rules
//! exactly: a sequence that is a mapping value is "indentless" (its
//! dashes align with the key that introduced it, not indented further);
//! a sequence or mapping that is itself a sequence *item* is written
//! "compactly" (its first line continues right after the item's own
//! `"- "`, e.g. `"- - 1\n  - 2"`/`"- a: 1\n  b: 2"`), matching
//! `Emitter.expect_block_sequence`'s `indentless` check and
//! `expect_node`'s own sequence/mapping compacting.
//!
//! **Mapping key order, verified against the real dumper, not just the
//! issue's own prose.** `SafeRepresenter.represent_mapping`'s own
//! `sort_keys=True` path is `try: mapping = sorted(mapping) except
//! TypeError: pass` -- a dict with mismatched-type keys (`{1: a, b:
//! c}`) therefore dumps successfully, in its own *original* insertion
//! order, never raising: confirmed directly against `yaml.dump` and
//! against `circuitry.core.reflector._extract_done_flag` itself with
//! PyYAML 6.0.3 (the pinned version), both of which return/dump the
//! unsorted pair with no exception. [`sorted_keys`] reproduces that
//! exact all-or-nothing fallback, not a raised error.

use crate::scalar;
use electricity_value::Value;
use std::fmt;

/// `best_width` -- PyYAML's own fixed default (`Emitter.__init__`'s
/// `width or 80`); none of this workspace's callers override it.
const BEST_WIDTH: usize = 80;
/// `best_indent` -- PyYAML's own fixed default (`indent or 2`).
const BEST_INDENT: usize = 2;

/// What [`dump`] couldn't finish -- today, only nesting deeper than
/// [`crate::MAX_DEPTH`] (this crate's own shared bound for untrusted,
/// possibly model-generated input: a reflector's generated plan is
/// dumped right back out before `use(inline)` ever validates it, so
/// nothing has bounded its depth yet when this function runs). Mapping
/// key sorting, the one other failure mode the issue's own prose
/// describes, does not actually reach this type -- see this module's
/// own doc comment on why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DumpError(pub String);

impl fmt::Display for DumpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for DumpError {}

/// `yaml.dump`'s own keyword knobs this crate's callers ever vary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DumpOptions {
    pub sort_keys: bool,
}

impl Default for DumpOptions {
    /// `yaml.dump`'s own default (`sort_keys=True`) -- reflector's own
    /// two dump call sites never override it.
    fn default() -> Self {
        DumpOptions { sort_keys: true }
    }
}

/// `yaml.dump(value, default_flow_style=False, sort_keys=options.sort_keys)`.
pub fn dump(value: &Value, options: &DumpOptions) -> Result<String, DumpError> {
    let mut out = String::new();
    let open_ended = emit_root(&mut out, value, options, 1)?;
    if open_ended {
        out.push_str("...\n");
    }
    Ok(out)
}

fn depth_error() -> DumpError {
    DumpError(format!(
        "exceeded the maximum nesting depth of {}",
        electricity_value::MAX_DEPTH
    ))
}

/// Writes the whole document; returns whether the root was written as
/// a *plain*-style scalar -- `Emitter.write_plain`'s own `if
/// self.root_context: self.open_ended = True`, the one condition under
/// which `expect_document_end` appends a trailing `"...\n"` (a bare
/// plain scalar document is otherwise ambiguous with a following
/// document's own content; a quoted scalar or any collection, even an
/// empty one, is not).
fn emit_root(
    out: &mut String,
    value: &Value,
    options: &DumpOptions,
    depth: usize,
) -> Result<bool, DumpError> {
    if depth > electricity_value::MAX_DEPTH {
        return Err(depth_error());
    }
    match value {
        Value::Dict(d) if !d.is_empty() => {
            emit_mapping(out, d, 0, options, depth)?;
            out.push('\n');
            Ok(false)
        }
        Value::Dict(_) => {
            out.push_str("{}\n");
            Ok(false)
        }
        Value::List(items) if !items.is_empty() => {
            emit_sequence(out, items, 0, options, depth)?;
            out.push('\n');
            Ok(false)
        }
        Value::List(_) => {
            out.push_str("[]\n");
            Ok(false)
        }
        scalar_value => {
            let (text, needs_str_check) = scalar_text(scalar_value);
            let style = choose_style(&text, false, needs_str_check);
            write_scalar(out, &text, style, 0, BEST_INDENT, true);
            out.push('\n');
            Ok(style == ScalarStyle::Plain)
        }
    }
}

/// *dict*'s own keys, in emission order -- insertion order when
/// `!sort_keys`, or when a full insertion sort hits an incomparable
/// pair partway through (the whole attempt is then discarded, matching
/// `SafeRepresenter.represent_mapping`'s own `try: mapping =
/// sorted(mapping) except TypeError: pass` -- this module's own doc
/// comment); insertion-sorted otherwise.
fn sorted_keys(dict: &electricity_value::Dict, sort_keys: bool) -> Vec<Value> {
    let original: Vec<Value> = dict.keys().cloned().collect();
    if !sort_keys {
        return original;
    }
    let mut keys = original.clone();
    let mut comparable = true;
    'sort: for i in 1..keys.len() {
        let mut j = i;
        while j > 0 {
            match keys[j].py_lt(&keys[j - 1]) {
                Ok(true) => {
                    keys.swap(j, j - 1);
                    j -= 1;
                }
                Ok(false) => break,
                Err(_) => {
                    comparable = false;
                    break 'sort;
                }
            }
        }
    }
    if comparable { keys } else { original }
}

/// Writes every key/value pair of *dict* -- the **caller** is
/// responsible for the cursor already sitting at column *indent*
/// before this call (entry 0 writes no prefix of its own at all;
/// every later entry gets `"\n"` plus *indent* spaces to return to
/// that same column). That one invariant is what lets the exact same
/// function serve the document root (nothing written yet, *indent*
/// always `0`), an ordinary nested mapping (the caller just wrote
/// `"\n"` plus *indent* spaces itself), and a mapping that is a
/// sequence item's own compact first line (the caller just wrote
/// `"- "`, landing the cursor on *indent* by construction -- see
/// [`emit_sequence`]'s own doc comment) with no flag to tell those
/// apart.
/// **Known gap, documented in DESIGN.md's own divergence table (issue
/// #506's lane L1, PR #510's first review):**
/// PyYAML's own `check_simple_key` switches a block-mapping key to the
/// explicit `? key\n: value` form once that key's own emitted length
/// reaches 128 characters or it spans more than one line -- this
/// function always writes the plain `key:` form below, whatever the
/// key's own length or shape. Only reachable through
/// `meta.orchestration_sha256`/the literal re-dumped text a reflector
/// (or M1-L2) ever feeds back into a Mustache render, never the
/// *parsed* value a key maps to.
fn emit_mapping(
    out: &mut String,
    dict: &electricity_value::Dict,
    indent: usize,
    options: &DumpOptions,
    depth: usize,
) -> Result<(), DumpError> {
    if depth > electricity_value::MAX_DEPTH {
        return Err(depth_error());
    }
    let keys = sorted_keys(dict, options.sort_keys);
    for (i, key) in keys.iter().enumerate() {
        let value = dict.get(key).expect("key was read from this dict");
        if i > 0 {
            out.push('\n');
            out.push_str(&" ".repeat(indent));
        }
        let (key_text, key_needs_str_check) = scalar_text(key);
        let key_style = choose_style(&key_text, true, key_needs_str_check);
        let key_col = write_scalar(
            out,
            &key_text,
            key_style,
            indent,
            indent + BEST_INDENT,
            false,
        );
        out.push(':');
        emit_mapping_value(out, value, indent, key_col + 1, options, depth)?;
    }
    Ok(())
}

fn emit_mapping_value(
    out: &mut String,
    value: &Value,
    indent: usize,
    column_after_colon: usize,
    options: &DumpOptions,
    depth: usize,
) -> Result<(), DumpError> {
    match value {
        Value::Dict(d) if d.is_empty() => out.push_str(" {}"),
        Value::Dict(d) => {
            out.push('\n');
            out.push_str(&" ".repeat(indent + BEST_INDENT));
            emit_mapping(out, d, indent + BEST_INDENT, options, depth + 1)?;
        }
        Value::List(items) if items.is_empty() => out.push_str(" []"),
        // Indentless: a mapping value's own sequence keeps the
        // mapping's *own* indent (`Emitter.expect_block_sequence`'s
        // `indentless = self.mapping_context and not self.indention`),
        // so the cursor this call leaves for `emit_sequence`'s own
        // entry 0 is at *indent*, not `indent + BEST_INDENT`.
        Value::List(items) => {
            out.push('\n');
            out.push_str(&" ".repeat(indent));
            emit_sequence(out, items, indent, options, depth + 1)?;
        }
        scalar_value => {
            out.push(' ');
            let (text, needs_str_check) = scalar_text(scalar_value);
            let style = choose_style(&text, false, needs_str_check);
            write_scalar(
                out,
                &text,
                style,
                column_after_colon + 1,
                indent + BEST_INDENT,
                true,
            );
        }
    }
    Ok(())
}

/// Writes every item of *items* -- same cursor-already-positioned
/// contract as [`emit_mapping`]'s own doc comment: entry 0 writes no
/// `"\n"` + indent of its own, only the dash itself (`"-"` plus a
/// space) right where the caller left the cursor, at column *indent*;
/// every later entry returns to that column first.
fn emit_sequence(
    out: &mut String,
    items: &[Value],
    indent: usize,
    options: &DumpOptions,
    depth: usize,
) -> Result<(), DumpError> {
    if depth > electricity_value::MAX_DEPTH {
        return Err(depth_error());
    }
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push('\n');
            out.push_str(&" ".repeat(indent));
        }
        out.push('-');
        match item {
            Value::Dict(d) if d.is_empty() => out.push_str(" {}"),
            Value::Dict(d) => {
                out.push(' ');
                emit_mapping(out, d, indent + BEST_INDENT, options, depth + 1)?;
            }
            Value::List(inner) if inner.is_empty() => out.push_str(" []"),
            // Compact: a sequence item that is itself a non-empty
            // sequence/mapping continues right after this item's own
            // `"- "` (`Emitter.expect_node`'s own sequence/mapping
            // compacting), rather than starting a fresh, further-
            // indented line the way a mapping value's collection does.
            // Writing `"- "` lands the cursor exactly on `indent +
            // BEST_INDENT`, which is also the *indent* this nested
            // call's own entry 0 needs -- no further write here.
            Value::List(inner) => {
                out.push(' ');
                emit_sequence(out, inner, indent + BEST_INDENT, options, depth + 1)?;
            }
            scalar_value => {
                out.push(' ');
                let (text, needs_str_check) = scalar_text(scalar_value);
                let style = choose_style(&text, false, needs_str_check);
                write_scalar(
                    out,
                    &text,
                    style,
                    indent + BEST_INDENT,
                    indent + BEST_INDENT,
                    true,
                );
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------
// Representer: Value -> scalar text
// ---------------------------------------------------------------------

/// *value*'s own scalar text, plus whether the *resolver* gate
/// (`choose_scalar_style`'s own `event.implicit[0]`) should even be
/// consulted for it: `true` for every non-`Str` scalar (its own text
/// always implicitly resolves back to its own type, by construction --
/// PyYAML's representer requests exactly that tag, and `analyze_scalar`'s
/// own structural flags are the only other gate that matters for it),
/// `false` only passed through for a [`Value::Str`], whose plain
/// eligibility depends on whether the *loader*'s own implicit
/// resolution would misread it as something else -- [`choose_style`]'s
/// own job, via [`scalar::implicit_tag`].
///
/// [`Value::Bytes`] has no real caller in this workspace (a parsed
/// orchestration plan's own YAML never produces one) -- represented
/// here as PyYAML's own explicit-tag form, `"!!binary "` followed by
/// the base64 text as an ordinary (quoted, if needed) scalar, rather
/// than the real dumper's own `|`-block-literal-wrapped-at-76-columns
/// form; a documented, low-priority divergence; never pinned by a
/// golden case.
fn scalar_text(value: &Value) -> (String, bool) {
    match value {
        Value::None => ("null".to_string(), false),
        Value::Bool(b) => ((if *b { "true" } else { "false" }).to_string(), false),
        Value::Int(i) => (i.to_string(), false),
        Value::Float(f) => (represent_float(*f), false),
        Value::Str(s) => (s.clone(), true),
        Value::Bytes(bytes) => (format!("!!binary {}", base64_encode(bytes)), false),
        Value::Date(date) => (date.format("%Y-%m-%d").to_string(), false),
        Value::DateTime(naive, offset) => (represent_datetime(naive, offset), false),
        Value::List(_) | Value::Dict(_) => {
            unreachable!("scalar_text is never called for a collection value")
        }
    }
}

/// `SafeRepresenter.represent_float`: `repr(data).lower()`, special-
/// cased for the three non-finite spellings YAML's own resolver (not
/// Python's `repr`) uses, and a bare `e` exponent with no `.` in the
/// mantissa (`repr(1e20)` is `"1e+20"`) gets `".0"` spliced in front of
/// it so the text still parses back as a float, not an int.
fn represent_float(f: f64) -> String {
    if f.is_nan() {
        return ".nan".to_string();
    }
    if f.is_infinite() {
        return if f > 0.0 { ".inf" } else { "-.inf" }.to_string();
    }
    let mut value = Value::Float(f).py_str().to_lowercase();
    if !value.contains('.') && value.contains('e') {
        value = value.replacen('e', ".0e", 1);
    }
    value
}

/// `SafeRepresenter.represent_datetime`: `data.isoformat(' ')` --
/// space separator (never `T`), microseconds only when nonzero, a
/// fixed offset (including UTC) always written as `+HH:MM`/`-HH:MM`
/// (never `Z`), no offset at all for a naive value.
fn represent_datetime(
    naive: &chrono::NaiveDateTime,
    offset: &Option<chrono::FixedOffset>,
) -> String {
    let mut text = naive.format("%Y-%m-%d %H:%M:%S").to_string();
    let micros = naive.and_utc().timestamp_subsec_micros();
    if micros != 0 {
        text.push_str(&format!(".{micros:06}"));
    }
    if let Some(off) = offset {
        let total = off.local_minus_utc();
        let sign = if total < 0 { '-' } else { '+' };
        let total_abs = total.unsigned_abs();
        text.push(sign);
        text.push_str(&format!(
            "{:02}:{:02}",
            total_abs / 3600,
            (total_abs / 60) % 60
        ));
    }
    text
}

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Plain base64 (RFC 4648 with `=` padding), no line wrapping -- see
/// [`scalar_text`]'s own doc comment on why this isn't PyYAML's exact
/// MIME-wrapped block-literal form.
fn base64_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        out.push(BASE64_ALPHABET[(b0 >> 2) as usize] as char);
        out.push(BASE64_ALPHABET[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            BASE64_ALPHABET[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            BASE64_ALPHABET[(b2 & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

// ---------------------------------------------------------------------
// Scalar style choice: analyze_scalar + choose_scalar_style
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScalarStyle {
    Plain,
    Single,
    Double,
}

struct ScalarAnalysis {
    empty: bool,
    multiline: bool,
    allow_block_plain: bool,
    allow_single_quoted: bool,
}

fn is_break(ch: char) -> bool {
    matches!(ch, '\n' | '\u{85}' | '\u{2028}' | '\u{2029}')
}

/// `\0 \t\r\n\x85  ` -- `Emitter.analyze_scalar`'s own
/// `preceded_by_whitespace`/`followed_by_whitespace` character set.
fn is_ws_boundary(ch: char) -> bool {
    matches!(ch, '\0' | ' ' | '\t' | '\r') || is_break(ch)
}

/// Whether `allow_unicode=False`'s own `special_characters` check
/// fires for *ch* -- not `\n` and not plain printable ASCII (`analyze_
/// scalar`'s own full unicode-range carve-out never applies once
/// `allow_unicode` is false, this crate's own callers' only setting --
/// see this module's own doc comment). Only [`analyze_scalar`]'s own
/// use of this (deciding whether plain/single-quoted style is even
/// allowed) exempts `\n`; [`write_double_quoted`]'s own escape
/// condition does not (see [`dq_needs_escape`]).
fn is_special(ch: char) -> bool {
    ch != '\n' && !('\u{20}'..='\u{7e}').contains(&ch)
}

/// `Emitter.write_double_quoted`'s own per-character escape trigger --
/// unlike [`is_special`] (`analyze_scalar`'s notion, which exempts
/// `\n` so a scalar *containing* one can still choose plain/single-
/// quoted style), a double-quoted scalar escapes `\n` exactly like
/// every other non-printable-ASCII character once that style has
/// already been chosen.
fn dq_needs_escape(ch: char) -> bool {
    !('\u{20}'..='\u{7e}').contains(&ch)
}

/// `Emitter.analyze_scalar`, narrowed to the flags [`choose_style`]
/// and the block-collection-only writers below actually consult
/// (`allow_flow_plain`/`allow_double_quoted`/`allow_block` are always
/// true or never read in this crate's own block-only emitter, so
/// they're dropped rather than computed and ignored).
fn analyze_scalar(scalar: &str) -> ScalarAnalysis {
    if scalar.is_empty() {
        return ScalarAnalysis {
            empty: true,
            multiline: false,
            allow_block_plain: true,
            allow_single_quoted: true,
        };
    }

    let chars: Vec<char> = scalar.chars().collect();
    let n = chars.len();

    let mut block_indicators = scalar.starts_with("---") || scalar.starts_with("...");
    let mut line_breaks = false;
    let mut special_characters = false;
    let mut leading_space = false;
    let mut leading_break = false;
    let mut trailing_space = false;
    let mut trailing_break = false;
    let mut break_space = false;
    let mut space_break = false;

    let mut preceded_by_whitespace = true;
    let mut followed_by_whitespace = n == 1 || is_ws_boundary(chars[1]);
    let mut previous_space = false;
    let mut previous_break = false;

    let mut index = 0usize;
    while index < n {
        let ch = chars[index];

        if index == 0 {
            if "#,[]{}&*!|>'\"%@`".contains(ch) {
                block_indicators = true;
            }
            if (ch == '?' || ch == ':') && followed_by_whitespace {
                block_indicators = true;
            }
            if ch == '-' && followed_by_whitespace {
                block_indicators = true;
            }
        } else {
            if ch == ':' && followed_by_whitespace {
                block_indicators = true;
            }
            if ch == '#' && preceded_by_whitespace {
                block_indicators = true;
            }
        }

        if is_break(ch) {
            line_breaks = true;
        }
        if is_special(ch) {
            special_characters = true;
        }

        if ch == ' ' {
            if index == 0 {
                leading_space = true;
            }
            if index == n - 1 {
                trailing_space = true;
            }
            if previous_break {
                break_space = true;
            }
            previous_space = true;
            previous_break = false;
        } else if is_break(ch) {
            if index == 0 {
                leading_break = true;
            }
            if index == n - 1 {
                trailing_break = true;
            }
            if previous_space {
                space_break = true;
            }
            previous_space = false;
            previous_break = true;
        } else {
            previous_space = false;
            previous_break = false;
        }

        index += 1;
        preceded_by_whitespace = is_ws_boundary(ch);
        followed_by_whitespace = index + 1 >= n || is_ws_boundary(chars[index + 1]);
    }

    let mut allow_block_plain = true;
    let mut allow_single_quoted = true;

    if leading_space || leading_break || trailing_space || trailing_break {
        allow_block_plain = false;
    }
    if break_space {
        allow_block_plain = false;
        allow_single_quoted = false;
    }
    if space_break || special_characters {
        allow_block_plain = false;
        allow_single_quoted = false;
    }
    if line_breaks {
        allow_block_plain = false;
    }
    if block_indicators {
        allow_block_plain = false;
    }

    ScalarAnalysis {
        empty: false,
        multiline: line_breaks,
        allow_block_plain,
        allow_single_quoted,
    }
}

/// `Emitter.choose_scalar_style`, narrowed to `event.style is None`
/// (this crate's own representer never requests a style) and
/// `canonical=False`: plain if the resolver agrees (via
/// *needs_str_check*/[`scalar::implicit_tag`]) and `analyze_scalar`
/// allows it in this (never simple-key, multiline/empty) combination;
/// else single-quoted if allowed; else double-quoted, which always
/// succeeds.
fn choose_style(text: &str, simple_key_context: bool, needs_str_check: bool) -> ScalarStyle {
    let analysis = analyze_scalar(text);
    // A non-`Str` scalar's own text always implicitly resolves back to
    // its own type (the representer requested exactly that tag), so
    // only a `Str` needs the loader's own implicit-resolution re-check
    // to decide whether a *plain* rendering would be misread as
    // something else.
    let implicit_ok = !needs_str_check || scalar::implicit_tag(text) == scalar::TAG_STR;
    if implicit_ok
        && !(simple_key_context && (analysis.empty || analysis.multiline))
        && analysis.allow_block_plain
    {
        return ScalarStyle::Plain;
    }
    if analysis.allow_single_quoted && !(simple_key_context && analysis.multiline) {
        return ScalarStyle::Single;
    }
    ScalarStyle::Double
}

fn write_scalar(
    out: &mut String,
    text: &str,
    style: ScalarStyle,
    column: usize,
    fold_indent: usize,
    split: bool,
) -> usize {
    match style {
        ScalarStyle::Plain => write_plain(out, text, column, fold_indent, split),
        ScalarStyle::Single => write_single_quoted(out, text, column, fold_indent, split),
        ScalarStyle::Double => write_double_quoted(out, text, column, fold_indent, split),
    }
}

/// `Emitter.write_plain`: a plain scalar never contains a line break
/// (`analyze_scalar`'s own `allow_block_plain` requires `!line_breaks`),
/// so this only ever folds at a single space once the running column
/// has already passed [`BEST_WIDTH`] -- greedy: a line may overflow by
/// as much as its last word, and only the *next* space becomes the
/// fold point.
fn write_plain(
    out: &mut String,
    text: &str,
    mut column: usize,
    indent: usize,
    split: bool,
) -> usize {
    if text.is_empty() {
        return column;
    }
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut start = 0usize;
    let mut end = 0usize;
    let mut spaces = false;
    while end <= n {
        let ch = if end < n { Some(chars[end]) } else { None };
        if spaces {
            if ch != Some(' ') {
                if start + 1 == end && split && column > BEST_WIDTH && start != 0 && end != n {
                    out.push('\n');
                    out.push_str(&" ".repeat(indent));
                    column = indent;
                } else {
                    let data: String = chars[start..end].iter().collect();
                    column += data.chars().count();
                    out.push_str(&data);
                }
                start = end;
            }
        } else if ch.is_none() || ch == Some(' ') {
            let data: String = chars[start..end].iter().collect();
            column += data.chars().count();
            out.push_str(&data);
            start = end;
        }
        if let Some(c) = ch {
            spaces = c == ' ';
        }
        end += 1;
    }
    column
}

/// `Emitter.write_single_quoted`: like [`write_plain`]'s word-wrap,
/// plus an embedded run of *k* line breaks is written as *k+1*
/// (`Emitter.write_line_break` once for the run's own leading `\n`,
/// then once more per break character) so re-parsing YAML's own
/// line-folding rule collapses it back to exactly *k*, and every
/// literal `'` is doubled.
fn write_single_quoted(
    out: &mut String,
    text: &str,
    mut column: usize,
    indent: usize,
    split: bool,
) -> usize {
    out.push('\'');
    column += 1;
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut start = 0usize;
    let mut end = 0usize;
    let mut spaces = false;
    let mut breaks = false;
    while end <= n {
        let ch = if end < n { Some(chars[end]) } else { None };
        if spaces {
            if ch != Some(' ') {
                if start + 1 == end && split && column > BEST_WIDTH && start != 0 && end != n {
                    out.push('\n');
                    out.push_str(&" ".repeat(indent));
                    column = indent;
                } else {
                    let data: String = chars[start..end].iter().collect();
                    column += data.chars().count();
                    out.push_str(&data);
                }
                start = end;
            }
        } else if breaks {
            if ch.is_none() || !is_break(ch.unwrap()) {
                if chars[start] == '\n' {
                    out.push('\n');
                }
                for &br in &chars[start..end] {
                    out.push(br);
                }
                out.push_str(&" ".repeat(indent));
                column = indent;
                start = end;
            }
        } else if (ch.is_none() || ch == Some(' ') || ch.is_some_and(is_break) || ch == Some('\''))
            && start < end
        {
            let data: String = chars[start..end].iter().collect();
            column += data.chars().count();
            out.push_str(&data);
            start = end;
        }
        if ch == Some('\'') {
            out.push_str("''");
            column += 2;
            start = end + 1;
        }
        if let Some(c) = ch {
            spaces = c == ' ';
            breaks = is_break(c);
        }
        end += 1;
    }
    out.push('\'');
    column + 1
}

const ESCAPE_NAMED: &[(char, char)] = &[
    ('\0', '0'),
    ('\u{07}', 'a'),
    ('\u{08}', 'b'),
    ('\t', 't'),
    ('\n', 'n'),
    ('\u{0b}', 'v'),
    ('\u{0c}', 'f'),
    ('\r', 'r'),
    ('\u{1b}', 'e'),
    ('"', '"'),
    ('\\', '\\'),
    ('\u{85}', 'N'),
    ('\u{a0}', '_'),
    ('\u{2028}', 'L'),
    ('\u{2029}', 'P'),
];

fn escape_char(ch: char) -> String {
    if let Some((_, code)) = ESCAPE_NAMED.iter().find(|(c, _)| *c == ch) {
        return format!("\\{code}");
    }
    let cp = ch as u32;
    if cp <= 0xFF {
        format!("\\x{cp:02X}")
    } else if cp <= 0xFFFF {
        format!("\\u{cp:04X}")
    } else {
        format!("\\U{cp:08X}")
    }
}

/// `Emitter.write_double_quoted`: every character outside printable
/// ASCII (`allow_unicode=False`, this crate's own callers' only
/// setting), plus a literal `"`/`\`, is escaped individually
/// ([`escape_char`]); folding inserts a trailing `\` before the line
/// break and, when the fold lands on a space, an escaped `\ ` so the
/// space survives re-parsing instead of being stripped as leading
/// continuation whitespace.
fn write_double_quoted(
    out: &mut String,
    text: &str,
    mut column: usize,
    indent: usize,
    split: bool,
) -> usize {
    out.push('"');
    column += 1;
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut start = 0usize;
    let mut end = 0usize;
    while end <= n {
        let ch = if end < n { Some(chars[end]) } else { None };
        let needs_escape = match ch {
            None => true,
            Some(c) => matches!(c, '"' | '\\') || dq_needs_escape(c),
        };
        if needs_escape {
            if start < end {
                let data: String = chars[start..end].iter().collect();
                column += data.chars().count();
                out.push_str(&data);
                start = end;
            }
            if let Some(c) = ch {
                let data = escape_char(c);
                column += data.chars().count();
                out.push_str(&data);
                start = end + 1;
            }
        }
        // `Emitter.write_double_quoted`'s own fold check: `start` can
        // already be past `end` here (an escape just fired, right
        // above), so the pending-length arithmetic is signed, matching
        // Python's own `end - start`, never clamped to zero.
        let n_i = n as i64;
        let end_i = end as i64;
        let pending = end_i - start as i64;
        if split
            && n >= 2
            && end_i > 0
            && end_i < n_i - 1
            && (ch == Some(' ') || start >= end)
            && column as i64 + pending > BEST_WIDTH as i64
        {
            let data: String = if start <= end {
                chars[start..end].iter().collect()
            } else {
                String::new()
            };
            if start < end {
                start = end;
            }
            out.push_str(&data);
            out.push('\\');
            out.push('\n');
            out.push_str(&" ".repeat(indent));
            column = indent;
            if start < n && chars[start] == ' ' {
                out.push('\\');
                column += 1;
            }
        }
        end += 1;
    }
    out.push('"');
    column + 1
}
