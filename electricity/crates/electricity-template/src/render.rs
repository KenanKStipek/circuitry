//! The rendering half of the chevron port: `Value`-keyed context lookup
//! (`_get_key`), truthiness, and section/inverted-section iteration.
//!
//! This module trades chevron's own single-pass generator-and-mutable-
//! scope-stack algorithm (`chevron/renderer.py`) for an equivalent
//! recursive tree walk: [`crate::tokenizer::tokenize`] already validates
//! section/end-tag balance, so turning the flat token stream into a tree
//! ([`build_tree`]) can't fail, and the tree walk below reproduces every
//! chevron quirk this port is pinned against (see the inline comments and
//! `lib.rs`'s "Known divergences from chevron" section for the
//! confirmed exceptions) by output, not by internal mechanism.

use chrono::{Datelike, Timelike};
use electricity_json::{Separators, WriteMode};
use electricity_value::Value;

use crate::tokenizer::Tag;

/// How a `{{var}}`/`{{{var}}}` splice of a `List`/`Dict` value renders
/// (`core/templates.py`'s own ``_json_aware_ctx``, applied inside
/// ``render_template`` itself rather than only for a tool's
/// `params_json`): the JSON text `json.dumps(value, ensure_ascii=False,
/// default=str)` would produce — matching every Mustache render site,
/// not `Value::py_repr`'s Python `str()` — and never collapsed to `""`
/// when empty, so `{{x}}` against `[]`/`{}` still prints `"[]"`/`"{}"`.
pub fn json_splice(value: &Value) -> Result<String, String> {
    electricity_json::dumps_default_str(
        value,
        WriteMode {
            indent: None,
            sort_keys: false,
            ensure_ascii: false,
            separators: Separators::Default,
        },
    )
    .map_err(|e| e.to_string())
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

fn walk_dotted(scope: &Value, key: &str) -> Result<Option<Value>, String> {
    let mut parts = key.split('.');
    // `str::split` on a non-empty pattern always yields at least one item.
    let first = parts.next().expect("split yields at least one segment");
    let mut current = match step(scope, first)? {
        Some(value) => value,
        None => return Ok(None),
    };
    for part in parts {
        current = match step(&current, part)? {
            Some(value) => value,
            None => return Ok(None),
        };
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
/// (parsed as an int, Python's negative-index wraparound included), one
/// of a `Date`/`DateTime`'s own named calendar fields (`year`, `month`,
/// ...), or (on any other scalar, and on a `Date`/`DateTime` segment
/// that isn't one of those named fields) a render failure.
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
///
/// `pub`, not `pub(crate)`: a store-backed [`ScopeRoot`] (issue #463)
/// resolves a dotted path's live-store-tracked prefix itself (a
/// segment at a time, against its own live tree, never materializing
/// an unrelated sibling), but the moment that walk reaches a plain
/// leaf `Value` -- a tool's own JSON output, a loop-item overlay, a
/// scalar -- continuing the *same* key's remaining segments has to
/// reproduce this exact per-segment contract (the dict/list/str/bytes
/// rules, the scalar-plus-int-parseable-segment error), so this
/// function is this crate's own public seam for that handoff rather
/// than a second copy of it in `electricity-vm`.
pub fn step(current: &Value, part: &str) -> Result<Option<Value>, String> {
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
        // `getattr(scope, child)` on a `datetime.date`/`datetime.datetime`
        // (chevron's `_get_key`, second lookup step) reaches these plain
        // data attributes -- never a method, which `core/templates.py`'s
        // `_get_key` treats as a miss (circuitry#389) -- so this is the
        // one named, non-callable set a `Value::Date`/`Value::DateTime`
        // exposes; anything else (a method name, an unrelated word) falls
        // through to the same scalar rule every other segment on these
        // types already gets.
        Value::Date(d) => match part {
            "year" => Ok(Some(Value::Int((d.year() as i64).into()))),
            "month" => Ok(Some(Value::Int((d.month() as i64).into()))),
            "day" => Ok(Some(Value::Int((d.day() as i64).into()))),
            _ => scalar_step(current, part),
        },
        Value::DateTime(naive, _) => match part {
            "year" => Ok(Some(Value::Int((naive.year() as i64).into()))),
            "month" => Ok(Some(Value::Int((naive.month() as i64).into()))),
            "day" => Ok(Some(Value::Int((naive.day() as i64).into()))),
            "hour" => Ok(Some(Value::Int((naive.hour() as i64).into()))),
            "minute" => Ok(Some(Value::Int((naive.minute() as i64).into()))),
            "second" => Ok(Some(Value::Int((naive.second() as i64).into()))),
            "microsecond" => Ok(Some(Value::Int(
                (i64::from(naive.nanosecond() / 1000)).into(),
            ))),
            _ => scalar_step(current, part),
        },
        _ => scalar_step(current, part),
    }
}

/// The scalar fallback shared by every `step` arm with no index/key
/// concept of its own (`None`/`bool`/`int`/`float`, and a `Date`/
/// `DateTime` segment that isn't one of their own named attributes
/// above): an int-parseable segment is a hard error (mirrors chevron's
/// own bare, un-caught `scope[int(child)]` `TypeError` on these types),
/// anything else is just "not found".
fn scalar_step(current: &Value, part: &str) -> Result<Option<Value>, String> {
    if part.parse::<i64>().is_ok() {
        Err(format!(
            "'{}' object is not subscriptable",
            current.type_name()
        ))
    } else {
        Ok(None)
    }
}

/// What a Mustache scope's own *root* position (the base of the scope
/// chain -- [`Scope::Root`], below) resolves a dotted key against,
/// decoupled from whatever that root's own data actually lives in.
/// [`Value`]'s own impl is the only one this crate needs: every render
/// here, and every existing caller/test, keeps working exactly as
/// before. A second impl, resolving each segment against a live,
/// mutable store instead of an already-materialized `Value` (issue
/// #463), lives in `electricity-vm`, which alone knows what that store
/// looks like; this trait is this crate's own seam for it, kept in one
/// place rather than forking `get_key`/`walk_dotted`/[`render_nodes`]
/// into two near-identical copies.
pub trait ScopeRoot {
    /// Resolves *key* (a dotted path, exactly as a template tag/section
    /// name spells it) the same way [`walk_dotted`] resolves it against
    /// an already-materialized `Value` -- `Ok(None)` for "no such key"
    /// (the caller falls through to an enclosing scope, or to `""`), an
    /// `Err` only for the one case [`step`] itself can raise (a scalar
    /// scope probed with an int-parseable segment).
    fn lookup(&self, key: &str) -> Result<Option<Value>, String>;

    /// This root, in full, as the value `"."` resolves to when it is
    /// the nearest scope -- [`Value`]'s own impl is a plain clone;
    /// a live, store-backed root instead has to materialize whatever
    /// its own current state is (a whole-state render, `{{.}}` against
    /// the untouched root, is one of the few cases such a root *can't*
    /// avoid paying the full cost of -- see that impl's own doc
    /// comment).
    fn whole(&self) -> Value;

    /// Whether *key* (a dotted path, same shape [`ScopeRoot::lookup`]
    /// takes) resolves to anything at all -- `{{> name}}` composition's
    /// own effect-reference presence check (`electricity_vm::compose`,
    /// issue #487): a hit proves the first segment is a real key, even
    /// when the resolved value is itself falsy/`None`, which
    /// [`ScopeRoot::lookup`]'s own `Ok(Some(Value::None))` already
    /// distinguishes from "no such key" (`Ok(None)`) -- this default
    /// is exactly that distinction, built on [`ScopeRoot::lookup`]
    /// alone (so [`Value`]'s own impl needs no change). A store-backed
    /// root (`electricity_vm::exec::live_scope::LiveRoot`) overrides
    /// this with a presence-only walk instead: the path this probes is
    /// always `prime.<name>` or `prime.<name>.value`, and `prime.<name>`
    /// can be a loop with thousands of passes underneath it, so
    /// resolving all the way to a `Value` (this default's own `lookup`
    /// call, which [`Value`]'s own impl clones in full) would
    /// materialize however big that subtree is just to answer a yes/no
    /// question.
    fn contains(&self, key: &str) -> bool {
        matches!(self.lookup(key), Ok(Some(_)))
    }

    /// `core/templates.py::_get_key`'s own bare-sibling-reference
    /// fallback (issue #778): *head* is the first dotted segment of a
    /// key that just missed every scope, including this root. `Some(
    /// path)` means *head* names an effect of an enclosing container at
    /// *path* -- [`crate::render::get_key`] retries the *whole* key
    /// (head and every segment after it) dotted under *path*, walking
    /// every scope again from the nearest pushed overlay (a section's
    /// own item) down to this root, the same single walk the original
    /// key already took, just never through this method a second time
    /// (the retried key already starts with a container's own path,
    /// which is never itself one of that container's own effect names
    /// in an ordinary document, and even if it were, the retry never
    /// consults [`ScopeRoot::bare_fallback`] again, so it cannot
    /// recurse).
    ///
    /// The default (used by a plain [`Value`] root, and by every
    /// [`ScopeRoot`] impl outside `electricity-vm`'s own live, store-
    /// backed one) is `None` -- no enclosing-container chain, so a bare
    /// reference just misses, same as before #778. `electricity_vm`'s
    /// own `exec::live_scope::LiveRoot` is the one real override,
    /// reading whatever chain its `Frame::bare_effect_chain` carries.
    fn bare_fallback(&self, _head: &str) -> Option<String> {
        None
    }
}

impl ScopeRoot for Value {
    fn lookup(&self, key: &str) -> Result<Option<Value>, String> {
        walk_dotted(self, key)
    }

    fn whole(&self) -> Value {
        self.clone()
    }
}

/// A Mustache scope stack, nearest scope first, as a parent-linked chain
/// rather than a `Vec`: pushing a new scope
/// (`{{#section}}`/`{{^section}}`, [`render_with_pushed_scope`]) borrows
/// the enclosing chain instead of cloning it, so rendering a section
/// never copies the outer scopes -- including the whole root context --
/// on every push. Only the chain's own base, [`Scope::Root`], is generic
/// over *how* the root resolves a key ([`ScopeRoot`]); every *pushed*
/// scope (a section's own bound value, always already resolved by the
/// time it is pushed -- see [`render_nodes`]'s own `Section` arm) stays a
/// plain [`Value`] regardless.
pub(crate) enum Scope<'a, R: ScopeRoot> {
    Root(&'a R),
    Pushed(Value, &'a Scope<'a, R>),
}

impl<'a, R: ScopeRoot> Scope<'a, R> {
    pub(crate) fn root(value: &'a R) -> Self {
        Scope::Root(value)
    }

    /// This scope, as a whole `Value` -- `"."`'s own resolution, never
    /// falling back further (same scope only, whichever one it is).
    fn nearest_whole(&self) -> Value {
        match self {
            Scope::Root(root) => root.whole(),
            Scope::Pushed(value, _) => value.clone(),
        }
    }

    fn parent(&self) -> Option<&Scope<'a, R>> {
        match self {
            Scope::Root(_) => None,
            Scope::Pushed(_, parent) => Some(parent),
        }
    }
}

/// `_get_key`: resolve a (possibly dotted) name against the scope stack,
/// nearest scope first. `"."` always means "the nearest scope, as a
/// whole" and never falls back further. Any other key tries a full
/// dotted walk against each scope in turn, keeping the *first* scope
/// where every segment resolves (even if the resolved value is itself
/// falsy) rather than the first scope with a *truthy* value -- chevron
/// never "sees past" a present-but-falsy key to an outer scope's same
/// name. A key absent from every scope resolves to `""`, exactly like
/// chevron's own fallback, so this function never needs a separate
/// not-found signal: every call site already treats `""` as "missing or
/// falsy", which is exactly what chevron's `_get_key` itself returns.
/// Every *pushed* scope keeps walking by reference ([`walk_dotted`]),
/// cloning only the final matched value; the chain's own base defers to
/// whatever [`ScopeRoot::lookup`] its own `R` implements.
pub(crate) fn get_key<R: ScopeRoot>(key: &str, scope: &Scope<R>) -> Result<Value, String> {
    if key == "." {
        return Ok(scope.nearest_whole());
    }
    if let Some(found) = resolve_without_bare_fallback(key, scope)? {
        return Ok(found);
    }
    // #778: every normal scope (every pushed overlay, and the root
    // itself) has missed -- try the bare-sibling-reference fallback
    // once, against the root this chain bottoms out at. The resolved
    // dotted key already starts with a container's own path (e.g.
    // `"prime..."`), so this retries it the same way an ordinary
    // dotted reference would: every scope again, pushed overlays
    // (section items) first, nearest first -- `core/templates.py::
    // _get_key`'s own recursive `_get_key(f"{path}.{key}", scopes,
    // warn, _bare_fallback=False)` call, ported -- never a second
    // `bare_fallback` lookup (`resolve_without_bare_fallback` never
    // consults it).
    let head = key.split('.').next().unwrap_or(key);
    if let Some(path) = scope_root(scope).bare_fallback(head) {
        let dotted = format!("{path}.{key}");
        if let Some(found) = resolve_without_bare_fallback(&dotted, scope)? {
            return Ok(found);
        }
    }
    Ok(Value::Str(String::new()))
}

/// [`get_key`]'s own single walk of *scope*, nearest pushed overlay
/// first, down to the root -- never consulting [`ScopeRoot::
/// bare_fallback`] itself, so a caller already inside the fallback's
/// own retry can reuse this without risking a second, recursive
/// fallback lookup.
fn resolve_without_bare_fallback<R: ScopeRoot>(
    key: &str,
    scope: &Scope<R>,
) -> Result<Option<Value>, String> {
    match scope {
        Scope::Pushed(value, parent) => match walk_dotted(value, key)? {
            Some(found) => Ok(Some(found)),
            None => resolve_without_bare_fallback(key, parent),
        },
        Scope::Root(root) => root.lookup(key),
    }
}

/// The [`ScopeRoot`] *scope*'s own chain bottoms out at -- every
/// [`Scope::Pushed`] overlay's own parent, down to the one [`Scope::
/// Root`].
fn scope_root<'a, R: ScopeRoot>(scope: &'a Scope<'a, R>) -> &'a R {
    match scope {
        Scope::Pushed(_, parent) => scope_root(parent),
        Scope::Root(root) => root,
    }
}

/// How a resolved value becomes the text spliced in for `{{var}}`/
/// `{{{var}}}`/`{{&var}}` (not yet HTML-escaped): a literal zero/`False`
/// prints itself; a `List`/`Dict` always splices as JSON text, truthy or
/// not (matching `core/templates.py::render_template`'s own
/// `_json_aware_ctx` wrapping, applied to every render now, not only a
/// tool's `params_json`); every other truthy value prints via `py_str`;
/// everything else collapses to `""`.
fn stringify_for_variable(value: &Value) -> Result<String, String> {
    if is_zero_or_false(value) {
        return Ok(value.py_str());
    }
    match value {
        Value::List(_) | Value::Dict(_) => json_splice(value),
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
///
/// *escape* (#397/#406) toggles only the final HTML-escape step:
/// `escape: false` makes `{{x}}` insert its value exactly as
/// `{{{x}}}`/`{{&x}}` already do, with the `"."`-plus-`True` quirk above
/// still applying regardless -- it belongs to this tag *type*
/// (`render.py`'s own `'variable'` branch), not to escaping itself.
fn render_variable_escaped<R: ScopeRoot>(
    key: &str,
    scope: &Scope<R>,
    escape: bool,
) -> Result<String, String> {
    let mut value = get_key(key, scope)?;
    if key == "." && matches!(value, Value::Bool(true)) {
        value = scope
            .parent()
            .map(|parent| parent.nearest_whole())
            .ok_or_else(|| "list index out of range".to_string())?;
    }
    let text = stringify_for_variable(&value)?;
    if escape {
        Ok(html_escape(&text))
    } else {
        Ok(text)
    }
}

/// `{{{x}}}`/`{{&x}}`: the resolved value, unescaped, with no special
/// case for `{{{.}}}`/`{{&.}}` resolving to `True` (see
/// [`render_variable_escaped`]).
fn render_variable_raw<R: ScopeRoot>(key: &str, scope: &Scope<R>) -> Result<String, String> {
    let value = get_key(key, scope)?;
    stringify_for_variable(&value)
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
pub(crate) fn render_nodes<R: ScopeRoot>(
    nodes: &[Node],
    scope: &Scope<R>,
    escape: bool,
) -> Result<String, String> {
    let mut out = String::new();
    for node in nodes {
        match node {
            Node::Literal(s) => out.push_str(s),
            Node::Variable(key) => {
                out.push_str(&render_variable_escaped(key, scope, escape)?);
            }
            Node::NoEscape(key) => out.push_str(&render_variable_raw(key, scope)?),
            Node::Section(key, body) => {
                let mut value = get_key(key, scope)?;
                // `Value::List(items) = value` would move `items` out of
                // a type that implements `Drop` (electricity-value's
                // iterative `Drop` impl), which Rust never allows,
                // regardless of this being the enum's only field --
                // `mem::take` swaps the `Vec` out instead, leaving `value`
                // an (unused) empty list behind.
                if let Value::List(items) = &mut value {
                    for element in std::mem::take(items) {
                        if truthy(&element) {
                            out.push_str(&render_with_pushed_scope(body, element, scope, escape)?);
                        }
                    }
                } else if truthy(&value) {
                    out.push_str(&render_with_pushed_scope(body, value, scope, escape)?);
                }
            }
            Node::Inverted(key, body) => {
                let value = get_key(key, scope)?;
                if !truthy(&value) {
                    out.push_str(&render_with_pushed_scope(
                        body,
                        Value::Bool(true),
                        scope,
                        escape,
                    )?);
                }
            }
        }
    }
    Ok(out)
}

fn render_with_pushed_scope<'a, R: ScopeRoot>(
    body: &[Node],
    new_scope: Value,
    scope: &'a Scope<'a, R>,
    escape: bool,
) -> Result<String, String> {
    let pushed = Scope::Pushed(new_scope, scope);
    render_nodes(body, &pushed, escape)
}

#[cfg(test)]
mod bare_fallback_retry_tests {
    use super::*;

    /// A [`ScopeRoot`] whose [`ScopeRoot::bare_fallback`] always answers
    /// *fallback_path*, for a key missing a `.`-less *head* -- enough to
    /// exercise [`get_key`]'s own retry without needing a real
    /// `electricity_vm::Frame`/`Store`.
    struct FakeBareRoot {
        root: Value,
        head: &'static str,
        fallback_path: &'static str,
    }

    impl ScopeRoot for FakeBareRoot {
        fn lookup(&self, key: &str) -> Result<Option<Value>, String> {
            walk_dotted(&self.root, key)
        }

        fn whole(&self) -> Value {
            self.root.clone()
        }

        fn bare_fallback(&self, head: &str) -> Option<String> {
            if head == self.head {
                Some(self.fallback_path.to_string())
            } else {
                None
            }
        }
    }

    fn dict(pairs: Vec<(&str, Value)>) -> Value {
        let mut map = electricity_value::Dict::new();
        for (k, v) in pairs {
            map.insert(Value::Str(k.to_string()), v);
        }
        Value::Dict(map)
    }

    #[test]
    fn the_retry_checks_a_pushed_section_items_own_match_before_the_root() {
        // #778: the dotted retry must walk every scope again,
        // section items first -- not just the root, as electricity once
        // did. Here the fallback path `"container"` names a key both a
        // pushed section item and the root carry; the section item's
        // own value must win, since it is nearer.
        let root_root = dict(vec![(
            "container",
            dict(vec![("x", Value::Str("root-value".to_string()))]),
        )]);
        let fake_root = FakeBareRoot {
            root: root_root,
            head: "x",
            fallback_path: "container",
        };
        let section_item = dict(vec![(
            "container",
            dict(vec![("x", Value::Str("section-item-value".to_string()))]),
        )]);
        let scope = Scope::Root(&fake_root);
        let pushed = Scope::Pushed(section_item, &scope);
        let result = get_key("x", &pushed).unwrap();
        assert_eq!(result, Value::Str("section-item-value".to_string()));
    }

    #[test]
    fn the_retry_falls_through_to_the_root_when_no_pushed_scope_matches() {
        let root_root = dict(vec![(
            "container",
            dict(vec![("x", Value::Str("root-value".to_string()))]),
        )]);
        let fake_root = FakeBareRoot {
            root: root_root,
            head: "x",
            fallback_path: "container",
        };
        let section_item = dict(vec![("unrelated", Value::Str("irrelevant".to_string()))]);
        let scope = Scope::Root(&fake_root);
        let pushed = Scope::Pushed(section_item, &scope);
        let result = get_key("x", &pushed).unwrap();
        assert_eq!(result, Value::Str("root-value".to_string()));
    }

    #[test]
    fn the_fallback_is_never_consulted_a_second_time_during_the_retry() {
        // A pathological root whose own `bare_fallback` would panic if
        // the retry ever called it again -- `resolve_without_bare_
        // fallback` must never do that.
        struct PanicsOnSecondBareFallback {
            root: Value,
            calls: std::cell::Cell<u32>,
        }
        impl ScopeRoot for PanicsOnSecondBareFallback {
            fn lookup(&self, key: &str) -> Result<Option<Value>, String> {
                walk_dotted(&self.root, key)
            }
            fn whole(&self) -> Value {
                self.root.clone()
            }
            fn bare_fallback(&self, _head: &str) -> Option<String> {
                let n = self.calls.get();
                self.calls.set(n + 1);
                assert!(n == 0, "bare_fallback must be consulted at most once");
                Some("container".to_string())
            }
        }
        let fake_root = PanicsOnSecondBareFallback {
            root: dict(vec![(
                "container",
                dict(vec![("x", Value::Str("v".to_string()))]),
            )]),
            calls: std::cell::Cell::new(0),
        };
        let scope = Scope::Root(&fake_root);
        let result = get_key("x", &scope).unwrap();
        assert_eq!(result, Value::Str("v".to_string()));
        assert_eq!(fake_root.calls.get(), 1);
    }
}
