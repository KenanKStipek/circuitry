//! The rendering half of the chevron port: `Value`-keyed context lookup
//! (`_get_key`), truthiness, section/inverted-section iteration, and the
//! two splice contexts (`core/templates.py` / `core/tool.py:195`'s
//! `PlainCtx`/`JsonAwareCtx` split, DESIGN.md §3.3).
//!
//! This module trades chevron's own single-pass generator-and-mutable-
//! scope-stack algorithm (`chevron/renderer.py`) for an equivalent
//! recursive tree walk: [`crate::tokenizer::tokenize`] already validates
//! section/end-tag balance, so turning the flat token stream into a tree
//! ([`build_tree`]) can't fail, and the tree walk below reproduces every
//! chevron quirk this port is pinned against (see the inline comments and
//! `lib.rs`'s "Known divergences from chevron" section for the
//! confirmed exceptions) by output, not by internal mechanism.

use electricity_value::Value;

use crate::tokenizer::Tag;

/// How a `{{var}}`/`{{{var}}}` splice of a `List`/`Dict` value renders,
/// and how a falsy one is treated. The default (`PlainCtx`) renders a
/// list/dict via [`Value::py_repr`] (Python's `str()` on a list/dict is
/// its `repr`) and collapses a falsy (empty) one to `""`, same as any
/// other falsy scalar. `JsonAwareCtx` (used only for a tool's
/// `params_json`, DESIGN.md §3.3's Quirk Q2) instead calls a
/// caller-supplied serializer, and — because `core/tool.py`'s
/// `_JsonAwareDict`/`_JsonAwareList` set
/// `_CHEVRON_return_scope_when_falsy = True` — never collapses an empty
/// list/dict to `""`: it always serializes, so an empty one still prints
/// `"[]"`/`"{}"`.
pub trait SpliceCtx {
    /// Render a `List`/`Dict` value for a `{{var}}`/`{{{var}}}` splice.
    /// The error string is surfaced verbatim as a render failure
    /// (`"{label}: could not render: {0}"`); electricity-template does
    /// not dictate its wording since the serializer is caller-supplied.
    fn splice(&self, value: &Value) -> Result<String, String>;

    /// Whether a falsy (empty) `List`/`Dict` still renders via
    /// [`SpliceCtx::splice`] rather than collapsing to `""`.
    fn keep_falsy_containers(&self) -> bool;
}

/// The default context: a list/dict splices as Python's `str()` would
/// render it (`[1, 'a']`, `{'a': 1}`), and an empty one is just another
/// falsy value.
pub struct PlainCtx;

impl SpliceCtx for PlainCtx {
    fn splice(&self, value: &Value) -> Result<String, String> {
        Ok(value.py_repr())
    }

    fn keep_falsy_containers(&self) -> bool {
        false
    }
}

/// The context used only for a tool's `params_json` field: a list/dict
/// splices through a caller-supplied serializer (real `json.dumps` on
/// the Python side) instead of `py_repr`, and never collapses an empty
/// one to `""`. Deliberately does not depend on `electricity-json`
/// (built in parallel, DESIGN.md §3.3): the tool-effect compiler supplies
/// the serializer once both crates exist.
pub struct JsonAwareCtx<'a> {
    serializer: &'a dyn Fn(&Value) -> Result<String, String>,
}

impl<'a> JsonAwareCtx<'a> {
    pub fn new(serializer: &'a dyn Fn(&Value) -> Result<String, String>) -> Self {
        Self { serializer }
    }
}

impl SpliceCtx for JsonAwareCtx<'_> {
    fn splice(&self, value: &Value) -> Result<String, String> {
        (self.serializer)(value)
    }

    fn keep_falsy_containers(&self) -> bool {
        true
    }
}

/// A render-tree node, built once per template from its (already
/// syntax-validated) token stream.
#[derive(Debug, Clone)]
pub(crate) enum Node {
    Literal(String),
    Variable(String),
    NoEscape(String),
    Section(String, Vec<Node>),
    Inverted(String, Vec<Node>),
}

/// Turn a flat, already-validated token stream into a tree. Infallible:
/// [`crate::tokenizer::tokenize`] has already rejected any unbalanced
/// section/end nesting, so every `Section`/`InvertedSection` token here is
/// guaranteed a matching `End` later in the same stream. `SetDelimiter`
/// tokens are already fully absorbed by tokenizing (they only ever
/// affected how later tags were *parsed*) and produce no node; `Partial`
/// tokens never reach this function (rejected earlier, see `lib.rs`), but
/// are matched here rather than ignored by a wildcard so a future partial
/// variant can't silently fall through unnoticed.
pub(crate) fn build_tree(tokens: &[Tag]) -> Vec<Node> {
    let mut iter = tokens.iter();
    build_tree_inner(&mut iter)
}

fn build_tree_inner<'a, I: Iterator<Item = &'a Tag>>(iter: &mut I) -> Vec<Node> {
    let mut nodes = Vec::new();
    while let Some(tag) = iter.next() {
        match tag {
            Tag::Literal(s) => nodes.push(Node::Literal(s.clone())),
            Tag::Variable(k) => nodes.push(Node::Variable(k.clone())),
            Tag::NoEscape(k) => nodes.push(Node::NoEscape(k.clone())),
            Tag::SetDelimiter(_) | Tag::Partial(_) => {}
            Tag::Section(k) => {
                let body = build_tree_inner(iter);
                nodes.push(Node::Section(k.clone(), body));
            }
            Tag::InvertedSection(k) => {
                let body = build_tree_inner(iter);
                nodes.push(Node::Inverted(k.clone(), body));
            }
            Tag::End(_) => return nodes,
        }
    }
    nodes
}

/// Python truthiness: `None`, `False`, a numeric zero, and any empty
/// `str`/`bytes`/`list`/`dict` are falsy; everything else (including
/// `NaN`, which is `!= 0`) is truthy. `Date`/`DateTime` have no Python
/// `__bool__`/`__len__`, so they're always truthy, same as any plain
/// object.
pub(crate) fn truthy(value: &Value) -> bool {
    match value {
        Value::None => false,
        Value::Bool(b) => *b,
        Value::Int(i) => !i.is_zero(),
        Value::Float(f) => *f != 0.0,
        Value::Str(s) => !s.is_empty(),
        Value::Bytes(b) => !b.is_empty(),
        Value::List(items) => !items.is_empty(),
        Value::Dict(d) => !d.is_empty(),
        Value::Date(_) | Value::DateTime(..) => true,
    }
}

/// `scope in (0, False)` (`_get_key`): the one case where a *falsy*
/// resolved value is still rendered as itself (`"0"`, `"0.0"`, `"False"`)
/// rather than collapsed to `""` — this exists in chevron purely to avoid
/// `0 or ''`/`False or ''` accidentally erasing a literal zero/`False`.
fn is_zero_or_false(value: &Value) -> bool {
    match value {
        Value::Bool(b) => !*b,
        Value::Int(i) => i.is_zero(),
        Value::Float(f) => *f == 0.0,
        _ => false,
    }
}

/// `_get_key`: resolve a (possibly dotted) name against the scope stack,
/// nearest scope first. `"."` always means "the nearest scope, as a
/// whole" and never falls back further. Any other key tries a full
/// dotted walk against each scope in turn, keeping the *first* scope
/// where every segment resolves (even if the resolved value is itself
/// falsy) rather than the first scope with a *truthy* value — chevron
/// never "sees past" a present-but-falsy key to an outer scope's same
/// name. A key absent from every scope resolves to `""`, exactly like
/// chevron's own fallback, so this function never needs a separate
/// not-found signal: every call site already treats `""` as "missing or
/// falsy", which is exactly what chevron's `_get_key` itself returns.
pub(crate) fn get_key(key: &str, scopes: &[Value]) -> Result<Value, String> {
    if key == "." {
        return Ok(scopes
            .first()
            .cloned()
            .unwrap_or_else(|| Value::Str(String::new())));
    }
    for scope in scopes {
        if let Some(found) = walk_dotted(scope, key)? {
            return Ok(found);
        }
    }
    Ok(Value::Str(String::new()))
}

fn walk_dotted(scope: &Value, key: &str) -> Result<Option<Value>, String> {
    let mut current = scope.clone();
    for part in key.split('.') {
        match step(&current, part)? {
            Some(next) => current = next,
            None => return Ok(None),
        }
    }
    Ok(Some(current))
}

/// Python's `s[-n:]`-style negative-index wraparound, shared by every
/// sequence step below: `n` wraps once against `len`, and anything still
/// out of range is "not found" (an `IndexError` chevron's own `_get_key`
/// catches at the scope-loop level), not a hard failure.
fn wrapped_index(len: usize, n: i64) -> Option<usize> {
    let len = len as i64;
    let idx = if n < 0 { n + len } else { n };
    (idx >= 0 && idx < len).then_some(idx as usize)
}

/// One dotted-path segment: a dict key by name, a list/str/bytes index
/// (parsed as an int, Python's negative-index wraparound included), or
/// (on any other scalar) a render failure.
///
/// A dict lookup that misses by string key never falls back to an int
/// key (confirmed against real chevron: `scope[child]` on a dict raises
/// `KeyError`, which `_get_key`'s *inner* `except (TypeError,
/// AttributeError)` doesn't catch, so the int-index fallback chevron does
/// reach for a list -- `TypeError` on `list[child]` -- is never reached
/// for a dict at all; `{{d.1}}` against `{"d": {1: "x"}}` renders empty,
/// not `"x"`, even though `electricity-value`'s `Dict` can hold an int
/// key).
///
/// `_get_key`'s int-index fallback (`scope[int(child)]`) is the *last* of
/// three attempts (`scope[child]`, then `getattr(scope, child)`, then
/// this) and, uniquely among the three, isn't wrapped in its own
/// `try`/`except` -- so on a scalar (`None`/`bool`/`int`/`float`/a date),
/// once `int(child)` itself parses, `scope[int(child)]` raises a bare
/// `TypeError` ("'NoneType' object is not subscriptable", etc.) that
/// escapes `_get_key` entirely rather than being treated as "not found,
/// try the next scope". A `part` that *doesn't* parse as an int never
/// reaches that subscript at all (`int(child)` itself raises `ValueError`,
/// which the outer per-scope `try` does catch), so it's just "not found"
/// here too, same as a dict/list miss.
fn step(current: &Value, part: &str) -> Result<Option<Value>, String> {
    match current {
        Value::Dict(d) => Ok(d.get(&Value::Str(part.to_string())).cloned()),
        Value::List(items) => {
            let Ok(n) = part.parse::<i64>() else {
                return Ok(None);
            };
            Ok(wrapped_index(items.len(), n).map(|idx| items[idx].clone()))
        }
        Value::Str(s) => {
            let Ok(n) = part.parse::<i64>() else {
                return Ok(None);
            };
            let chars: Vec<char> = s.chars().collect();
            Ok(wrapped_index(chars.len(), n).map(|idx| Value::Str(chars[idx].to_string())))
        }
        Value::Bytes(b) => {
            let Ok(n) = part.parse::<i64>() else {
                return Ok(None);
            };
            Ok(wrapped_index(b.len(), n).map(|idx| Value::Int((b[idx] as i64).into())))
        }
        _ => {
            if part.parse::<i64>().is_ok() {
                Err(format!(
                    "'{}' object is not subscriptable",
                    current.type_name()
                ))
            } else {
                Ok(None)
            }
        }
    }
}

/// How a resolved value becomes the text spliced in for `{{var}}`/
/// `{{{var}}}`/`{{&var}}` (not yet HTML-escaped): a literal zero/`False`
/// prints itself; a `List`/`Dict` splices via `ctx` if truthy, or if
/// falsy and `ctx` keeps falsy containers (`JsonAwareCtx`); every other
/// truthy value prints via `py_str`; everything else collapses to `""`.
fn stringify_for_variable(value: &Value, ctx: &dyn SpliceCtx) -> Result<String, String> {
    if is_zero_or_false(value) {
        return Ok(value.py_str());
    }
    match value {
        Value::List(_) | Value::Dict(_) => {
            if truthy(value) || ctx.keep_falsy_containers() {
                ctx.splice(value)
            } else {
                Ok(String::new())
            }
        }
        _ if truthy(value) => Ok(value.py_str()),
        _ => Ok(String::new()),
    }
}

/// `& < > "` escaped, `'` left alone — chevron's own table, not the full
/// HTML5 escape set (`renderer.py::_html_escape`).
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// `{{x}}`: HTML-escaped. Carries chevron's own `"."`-plus-`True` bug
/// (`renderer.py`'s `elif tag == 'variable'` branch only): when the whole
/// key is exactly `"."` and it resolves to the literal `True` (not just
/// anything truthy), chevron discards that resolution and renders the
/// *next* scope up instead — confirmed against real chevron, including
/// that it raises `"list index out of range"` when there is no scope to
/// fall back to (e.g. `True` is the template's own root context). Never
/// triggers for `{{{x}}}`/`{{&x}}`, which read the resolved value
/// directly (confirmed: `renderer.py`'s `'no escape'` branch has no such
/// check).
fn render_variable_escaped(
    key: &str,
    scopes: &[Value],
    ctx: &dyn SpliceCtx,
) -> Result<String, String> {
    let mut value = get_key(key, scopes)?;
    if key == "." && matches!(value, Value::Bool(true)) {
        value = scopes
            .get(1)
            .cloned()
            .ok_or_else(|| "list index out of range".to_string())?;
    }
    let text = stringify_for_variable(&value, ctx)?;
    Ok(html_escape(&text))
}

/// `{{{x}}}`/`{{&x}}`: the resolved value, unescaped, with no special
/// case for `{{{.}}}`/`{{&.}}` resolving to `True` (see
/// [`render_variable_escaped`]).
fn render_variable_raw(key: &str, scopes: &[Value], ctx: &dyn SpliceCtx) -> Result<String, String> {
    let value = get_key(key, scopes)?;
    stringify_for_variable(&value, ctx)
}

/// Render a tree against a scope stack (nearest scope first). A section
/// over a `List` iterates each *truthy* element, binding it as the new
/// nearest scope and rendering the body once per element — a falsy
/// element (confirmed against real chevron, including that this
/// suppresses the body's literal text too, not just its tags) produces
/// no output for that iteration at all, as if it were simply skipped. A
/// section over any other truthy value renders its body once, binding
/// that value as the new nearest scope; over a falsy value, it renders
/// nothing. An inverted section flips this: body renders once (scope
/// pushed as the literal `True`) iff the key resolves falsy, nothing
/// otherwise.
pub(crate) fn render_nodes(
    nodes: &[Node],
    scopes: &[Value],
    ctx: &dyn SpliceCtx,
) -> Result<String, String> {
    let mut out = String::new();
    for node in nodes {
        match node {
            Node::Literal(s) => out.push_str(s),
            Node::Variable(key) => out.push_str(&render_variable_escaped(key, scopes, ctx)?),
            Node::NoEscape(key) => out.push_str(&render_variable_raw(key, scopes, ctx)?),
            Node::Section(key, body) => {
                let mut value = get_key(key, scopes)?;
                // `Value::List(items) = value` would move `items` out of
                // a type that implements `Drop` (electricity-value's
                // iterative `Drop` impl), which Rust never allows,
                // regardless of this being the enum's only field --
                // `mem::take` swaps the `Vec` out instead, leaving `value`
                // an (unused) empty list behind.
                if let Value::List(items) = &mut value {
                    for element in std::mem::take(items) {
                        if truthy(&element) {
                            out.push_str(&render_with_pushed_scope(body, element, scopes, ctx)?);
                        }
                    }
                } else if truthy(&value) {
                    out.push_str(&render_with_pushed_scope(body, value, scopes, ctx)?);
                }
            }
            Node::Inverted(key, body) => {
                let value = get_key(key, scopes)?;
                if !truthy(&value) {
                    out.push_str(&render_with_pushed_scope(
                        body,
                        Value::Bool(true),
                        scopes,
                        ctx,
                    )?);
                }
            }
        }
    }
    Ok(out)
}

fn render_with_pushed_scope(
    body: &[Node],
    new_scope: Value,
    scopes: &[Value],
    ctx: &dyn SpliceCtx,
) -> Result<String, String> {
    let mut pushed = Vec::with_capacity(scopes.len() + 1);
    pushed.push(new_scope);
    pushed.extend_from_slice(scopes);
    render_nodes(body, &pushed, ctx)
}
