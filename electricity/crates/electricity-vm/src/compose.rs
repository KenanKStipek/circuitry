//! Run-time prompt composition (`core/prompt_compose.py::
//! render_with_composition`, issue #487/#406) -- the one render path
//! every composable field in the VM uses (runtime-semantics.md §3.4's
//! own table): a tool's `prompt`/`params`/`params_json`, a `use`
//! effect's `inputs`/`inline`, a `yield` effect's `template`, and
//! (once lane G lands) a `prompt` effect's own `template`/`messages[].
//! content`. **Not** for a model-mode `if`/`while`/`expect` template or
//! an asset `ref` (lane G/G2): those render plain Mustache with no
//! `{{> name}}` expansion at all -- a partial tag there is a compile-time
//! error, never something this function ever sees (the rulings on
//! issue #487: "the compose function is for composable fields only").
//!
//! [`render_with_composition`] runs in two phases, exactly like the
//! Python port:
//!
//! 1. [`expand`] rewrites *template*'s own text, replacing every
//!    `{{> name}}` tag -- textually, left to right, never rescanning
//!    replacement text (chevron's own Mustache render never runs yet) --
//!    either with a declared prompt's own text (recursively expanded,
//!    cycle-checked, with its escaped tags rewritten to no-escape ones
//!    and a trailing newline dropped) or with a synthetic `{{{key}}}`
//!    tag bound to a resolved effect value in *extra* (this call's own
//!    hidden bindings).
//! 2. The rewritten text renders once, through
//!    [`electricity_template::render_template_with_root_and_escape`],
//!    against *ctx* overlaid with *extra* (when non-empty) through
//!    [`HiddenOverlay`] -- never a whole-context snapshot: the overlay
//!    wraps whatever [`ScopeRoot`] *ctx* already is, live or not.

use electricity_template::{ScopeRoot, SpliceCtx};
use electricity_value::Value;
use indexmap::IndexMap;
use std::collections::BTreeSet;

/// `core/prompt_compose.py::_MAX_EXPANDED_CHARS` -- a running cap on
/// every resolved fragment's/value's own length, in *code points* (not
/// bytes), charged once per resolution *at every nesting level* (so a
/// fragment nested two levels deep is charged twice -- see [`expand`]'s
/// own doc comment) -- a backstop against a declared prompt that nests
/// itself exponentially, not a precise budget.
const MAX_EXPANDED_CHARS: usize = 8 * 1024 * 1024;

/// `core/prompt_compose.py::_NAME_SHAPE` -- a `{{> name}}` name: a
/// declared prompt or effect name, optionally dotted
/// (`pipeline.outline`) to reach a nested/composed effect's own state.
/// ASCII identifiers only, ported by hand rather than with the `regex`
/// crate (no dependency on it in this crate; `electricity-compiler`'s
/// own `compose.rs` keeps an identical, independent copy for its own
/// compile-time check, rather than this crate depending on that one
/// for a six-line function).
fn is_valid_name(name: &str) -> bool {
    !name.is_empty() && name.split('.').all(is_plain_identifier)
}

fn is_plain_identifier(segment: &str) -> bool {
    let mut chars = segment.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

/// `core/prompt_compose.py::_drop_one_trailing_newline` -- one trailing
/// `\r\n`/`\n` dropped, never two.
fn drop_one_trailing_newline(text: &str) -> &str {
    text.strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .unwrap_or(text)
}

/// `core/prompt_compose.py::_unescape_variable_tags` -- rebuilds *text*
/// (a declared prompt's own resolved text, recursively including any
/// fragment it itself spliced in) from chevron's own tokenizer, with
/// every escaped variable tag (`{{x}}`) rewritten to an explicit
/// no-escape tag (`{{&x}}`), so a spliced declared-prompt fragment's
/// own tags never escape wherever they land once spliced into an
/// including template (#397), even one that keeps escaping everything
/// else (a tool param, say). A set-delimiter tag is rejected -- a
/// declared prompt may not carry one. *label* is `"prompts.{name}"`,
/// the declared prompt's own attribution.
fn unescape_variable_tags(text: &str, label: &str) -> Result<String, String> {
    let tokens = electricity_template::tokens_raw(text).map_err(|err| match err {
        electricity_template::RawTokensError::Syntax(msg)
        | electricity_template::RawTokensError::Render(msg) => {
            format!("{label}: malformed Mustache template: {msg}")
        }
        electricity_template::RawTokensError::TooDeeplyNested(depth) => format!(
            "{label}: template nesting too deep ({depth} levels, max {}).",
            electricity_template::MAX_SECTION_DEPTH
        ),
    })?;
    let mut out = String::with_capacity(text.len());
    for token in tokens {
        use electricity_template::TokenKind;
        match token {
            TokenKind::Literal(s) => out.push_str(&s),
            TokenKind::Variable(key) | TokenKind::NoEscape(key) => {
                out.push_str("{{&");
                out.push_str(&key);
                out.push_str("}}");
            }
            TokenKind::Section(key) => {
                out.push_str("{{#");
                out.push_str(&key);
                out.push_str("}}");
            }
            TokenKind::InvertedSection(key) => {
                out.push_str("{{^");
                out.push_str(&key);
                out.push_str("}}");
            }
            TokenKind::End(key) => {
                out.push_str("{{/");
                out.push_str(&key);
                out.push_str("}}");
            }
            // Should never occur: every `{{> name}}` is fully expanded,
            // recursively, before this runs. Reproduced as-is so
            // `render_template`'s own unconditional rejection catches
            // it with its usual message, rather than silently mangling
            // it.
            TokenKind::Partial(key) => {
                out.push_str("{{>");
                out.push_str(&key);
                out.push_str("}}");
            }
            TokenKind::SetDelimiter(_) => {
                return Err(format!(
                    "{label}: a declared prompt may not use '{{{{=...=}}}}' \
                     (set-delimiter) — it would change the delimiters of the \
                     template that includes it."
                ));
            }
        }
    }
    Ok(out)
}

/// A single running total, shared unchanged across every recursive
/// [`expand`] call a render makes -- `core/prompt_compose.py`'s own
/// *counter*/*extra*/*budget* trio, each threaded the same way through
/// `_expand`'s own recursion.
struct ExpandState<'a> {
    counter: u64,
    extra: IndexMap<String, String>,
    budget: usize,
    splice_ctx: &'a dyn SpliceCtx,
}

impl ExpandState<'_> {
    fn charge(&mut self, size: usize, where_label: &str) -> Result<(), String> {
        self.budget += size;
        if self.budget > MAX_EXPANDED_CHARS {
            return Err(format!(
                "{where_label}: '{{{{> name}}}}' expansion produced over \
                 {MAX_EXPANDED_CHARS} characters — likely runaway nested \
                 declared-prompt growth."
            ));
        }
        Ok(())
    }
}

/// `core/prompt_compose.py::_resolve_effect_text` -- the resolved text
/// `{{> name}}` splices for an effect reference: exactly what
/// `{{{prime.<name>.value}}}` would insert.
///
/// *head* (*name*'s own first segment) present in `ctx["prime"]`
/// (checked with [`ScopeRoot::contains`], never a full materialization
/// -- the rulings on issue #487) means the effect ran, even if skipped
/// or absorbed by its own `on_error`: dotted-get `prime.<name>.value`
/// (via [`ScopeRoot::lookup`]), `""` for a missing/`None` value.
/// *head* merely known (*known_names*, #396's "an untaken `if` branch,
/// a `flow: tree` sibling, one later in the chain" case) is also `""`.
/// Otherwise this is the run-time backstop error for a generated
/// document that skipped `cof check`'s own validation.
fn resolve_effect_text<R: ScopeRoot>(
    name: &str,
    head: &str,
    ctx: &R,
    known_names: &BTreeSet<String>,
    label: &str,
    splice_ctx: &dyn SpliceCtx,
) -> Result<String, String> {
    let head_path = format!("prime.{head}");
    if ctx.contains(&head_path) {
        let value_path = format!("prime.{name}.value");
        let value = ctx.lookup(&value_path)?;
        return match value {
            None | Some(Value::None) => Ok(String::new()),
            Some(v) => stringify_effect_value(&v, splice_ctx),
        };
    }
    if known_names.contains(head) {
        return Ok(String::new());
    }
    Err(format!(
        "{label}: '{{{{> {name}}}}}' does not name a declared prompt or effect."
    ))
}

/// `str(value)`, for an effect's own resolved value -- a `List`/`Dict`
/// *always* goes through *splice_ctx* (never collapsed to `""` just
/// because it's empty, unlike an ordinary `{{var}}` splice: `core/
/// prompt_compose.py::_resolve_effect_text`'s own bare `str(value)` on
/// Python's side, where a `_JsonAwareDict`/`_JsonAwareList`'s own
/// `__str__` always serializes); every other value renders through
/// [`electricity_value::Value::py_str`].
fn stringify_effect_value(value: &Value, splice_ctx: &dyn SpliceCtx) -> Result<String, String> {
    match value {
        Value::List(_) | Value::Dict(_) => splice_ctx.splice(value),
        other => Ok(other.py_str()),
    }
}

/// `core/prompt_compose.py::_expand` -- *template*, with every
/// `{{> name}}` tag replaced, textually, left to right
/// (`_PARTIAL_TAG.sub`'s own replacement order: a tag's own replacement
/// text is never rescanned for a further tag). *seen* is the current
/// expansion path (a per-branch set: the same declared prompt used
/// twice side by side is fine, cycle-checked only along one branch);
/// *state* is this whole render's own shared counter/hidden-bindings
/// map/budget (see [`ExpandState`]).
///
/// The budget is charged once per resolved fragment or effect text *at
/// every nesting level it bubbles up through* -- a fragment nested two
/// levels deep (included by a declared prompt that is itself included
/// by another) is charged once when the innermost `expand` call
/// resolves it, and again when the next level up's own `expand` call
/// receives it back from that recursive call and charges it a second
/// time under its *own* label. This exactly reproduces Python's double
/// counting (`_charge(len(fragment), where=label)` runs once per
/// `replace()` call, and a nested declared prompt's own text passes
/// through one `replace()` call per level on its way back out).
#[allow(clippy::too_many_arguments)]
fn expand<R: ScopeRoot>(
    template: &str,
    ctx: &R,
    declared: &IndexMap<String, String>,
    known_names: &BTreeSet<String>,
    label: &str,
    seen: &BTreeSet<String>,
    state: &mut ExpandState<'_>,
) -> Result<String, String> {
    let bytes = template.as_bytes();
    let mut out = String::with_capacity(template.len());
    let mut i = 0usize;
    loop {
        let Some(offset) = template[i..].find("{{>") else {
            out.push_str(&template[i..]);
            break;
        };
        let start = i + offset;
        out.push_str(&template[i..start]);
        // `(?<!\{)` -- `{{{>x}}}` is not a partial tag (chevron reads
        // it as a no-escape variable named `>x`); Rust's `regex` crate
        // has no lookbehind, so this checks the preceding byte by hand.
        if start > 0 && bytes[start - 1] == b'{' {
            out.push_str(&template[start..start + 3]);
            i = start + 3;
            continue;
        }
        let after = start + 3;
        match template[after..].find('}') {
            None => {
                // No closing brace anywhere in the remainder -- the
                // regex can't match here or anywhere further on
                // (nothing left to close a tag with); copy the rest
                // through literally.
                out.push_str(&template[start..]);
                break;
            }
            Some(brace_offset) => {
                let brace_idx = after + brace_offset;
                if bytes.get(brace_idx + 1) == Some(&b'}') {
                    let raw_name = &template[after..brace_idx];
                    let name = electricity_value::pycompat::py_strip(raw_name);
                    let replacement =
                        replace_one(name, ctx, declared, known_names, label, seen, state)?;
                    out.push_str(&replacement);
                    i = brace_idx + 2;
                } else {
                    // The capture group `[^}]*` can only extend up to
                    // (never past) the first literal `}` -- a lone,
                    // undoubled `}` here means the whole `{{>` attempt
                    // doesn't match at all (`{{>a}b}}` is plain text).
                    // No other `{{>` can start within this 3-byte
                    // window (the bytes already found are `{`, `{`,
                    // `>`), so skipping past it and resuming the scan
                    // is safe.
                    out.push_str(&template[start..start + 3]);
                    i = start + 3;
                }
            }
        }
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
fn replace_one<R: ScopeRoot>(
    name: &str,
    ctx: &R,
    declared: &IndexMap<String, String>,
    known_names: &BTreeSet<String>,
    label: &str,
    seen: &BTreeSet<String>,
    state: &mut ExpandState<'_>,
) -> Result<String, String> {
    if !is_valid_name(name) {
        return Err(format!("{label}: '{{{{> {name}}}}}' is not a valid name."));
    }
    // `"." not in name and head in declared` -- a *dotted* name is
    // never a declared-prompt reference, even if its own head happens
    // to name one (a declared prompt is plain text, never dotted); it
    // falls through to the effect-reference branch below instead,
    // which will fail with the same "does not name a declared prompt
    // or effect" text `cof check`'s own static validation already
    // rejects this shape with.
    let head = name.split('.').next().expect("split yields a segment");
    if !name.contains('.') {
        if let Some(fragment_source) = declared.get(name) {
            if seen.contains(name) {
                return Err(format!(
                    "{label}: cycle among declared prompts at '{name}'."
                ));
            }
            let mut next_seen = seen.clone();
            next_seen.insert(name.to_string());
            let nested_label = format!("prompts.{name}");
            let fragment = expand(
                fragment_source,
                ctx,
                declared,
                known_names,
                &nested_label,
                &next_seen,
                state,
            )?;
            let fragment = unescape_variable_tags(&fragment, &nested_label)?;
            let fragment = drop_one_trailing_newline(&fragment).to_string();
            state.charge(fragment.chars().count(), label)?;
            return Ok(fragment);
        }
    }
    let text = resolve_effect_text(name, head, ctx, known_names, label, state.splice_ctx)?;
    let text = drop_one_trailing_newline(&text).to_string();
    state.charge(text.chars().count(), label)?;
    let key = format!("__circuitry_partial_{}__", state.counter);
    state.counter += 1;
    state.extra.insert(key.clone(), text);
    let mut tag = String::with_capacity(key.len() + 6);
    tag.push_str("{{{");
    tag.push_str(&key);
    tag.push_str("}}}");
    Ok(tag)
}

/// A private overlay root for *this call's own* hidden bindings
/// (`__circuitry_partial_N__` -> resolved effect text), wrapping the
/// caller's own [`ScopeRoot`] *inner* -- `core/prompt_compose.py`'s own
/// `{**ctx, **extra}` shallow merge: a hidden key shadows a same-named
/// top-level key in *inner*, and every other lookup delegates straight
/// through. Built only when *extra* is non-empty (same as Python: the
/// merged dict is only ever constructed when there's something to
/// merge), and never materializes anything from *inner* itself --
/// [`ScopeRoot::whole`] is the one exception, paying whatever *inner*'s
/// own `whole()` already costs (this overlay adds nothing on top but
/// splicing the hidden keys into the result).
struct HiddenOverlay<'a, R> {
    hidden: &'a IndexMap<String, String>,
    inner: &'a R,
}

impl<R: ScopeRoot> ScopeRoot for HiddenOverlay<'_, R> {
    fn lookup(&self, key: &str) -> Result<Option<Value>, String> {
        let mut parts = key.split('.');
        let first = parts.next().expect("split yields at least one segment");
        if let Some(text) = self.hidden.get(first) {
            let mut current = Value::Str(text.clone());
            for part in parts {
                match electricity_template::step(&current, part)? {
                    Some(v) => current = v,
                    None => return Ok(None),
                }
            }
            return Ok(Some(current));
        }
        self.inner.lookup(key)
    }

    fn whole(&self) -> Value {
        let mut base = self.inner.whole();
        if let Value::Dict(dict) = &mut base {
            for (k, v) in self.hidden {
                // `IndexMap::insert` on an already-present key updates
                // it in place (keeping its position); a new key is
                // appended -- exactly the "existing key keeps its own
                // position, a new one is appended" rule this overlay's
                // own doc comment promises.
                dict.insert(Value::Str(k.clone()), Value::Str(v.clone()));
            }
        }
        base
    }

    /// Overrides the default (`lookup`-and-check) impl: a hidden
    /// binding is always a plain `String` already held in *hidden* --
    /// resolving one through `self.lookup` costs nothing *inner*-side
    /// (no store access at all), so only the fall-through branch (the
    /// first segment isn't a hidden key) needs to reach *inner* at all,
    /// and it does so through *inner*'s own `contains`, never `lookup`
    /// (review finding 1 on issue #492's PR: the inherited default would
    /// otherwise call `self.lookup`, which falls through to `inner.
    /// lookup`, which -- for a store-backed `inner` whose path resolves
    /// onto a `Slot::Node` with no segments left -- materializes that
    /// whole subtree just to answer a presence check).
    fn contains(&self, key: &str) -> bool {
        let first = key.split('.').next().expect("split yields at least one segment");
        if self.hidden.contains_key(first) {
            return matches!(self.lookup(key), Ok(Some(_)));
        }
        self.inner.contains(key)
    }
}

/// Renders *template* with `{{> name}}` partials spliced in from
/// *declared_prompts* against *ctx* -- the one rendering path every
/// prompt-text site in the VM uses (runtime-semantics.md §3.4's own
/// table). *label* is the budget/error attribution text (`"template"`
/// for Python's own default) and the final template render's own error
/// attribution; *splice_ctx* is [`electricity_template::PlainCtx`]/
/// [`electricity_template::JsonAwareCtx`], the same split
/// `render_template`'s own callers already choose between; *escape* is
/// the underlying Mustache render's own HTML-escape flag (`true` for
/// Python's own default).
///
/// For composable fields only (runtime-semantics.md §3.4's own table):
/// a model-mode `if`/`while`/`expect` template or an asset `ref`
/// renders through [`electricity_template::render_template_with_root`]
/// directly instead (lane G/G2), with `escape: true` and no `{{> name}}`
/// expansion -- a partial tag there is a compile-time error the walker
/// already rejects, never something this function is ever asked to
/// expand.
pub fn render_with_composition<R: ScopeRoot>(
    template: &str,
    ctx: &R,
    declared_prompts: &indexmap::IndexMap<String, String>,
    effect_names: &BTreeSet<String>,
    label: &str,
    splice_ctx: &dyn SpliceCtx,
    escape: bool,
) -> Result<String, String> {
    let mut state = ExpandState {
        counter: 0,
        extra: IndexMap::new(),
        budget: 0,
        splice_ctx,
    };
    let seen = BTreeSet::new();
    let rewritten = expand(
        template,
        ctx,
        declared_prompts,
        effect_names,
        label,
        &seen,
        &mut state,
    )?;
    if state.extra.is_empty() {
        electricity_template::render_template_with_root_and_escape(
            &rewritten, ctx, splice_ctx, label, escape,
        )
        .map_err(|err| err.to_string())
    } else {
        let overlay = HiddenOverlay {
            hidden: &state.extra,
            inner: ctx,
        };
        electricity_template::render_template_with_root_and_escape(
            &rewritten, &overlay, splice_ctx, label, escape,
        )
        .map_err(|err| err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_template::{JsonAwareCtx, PlainCtx};
    use electricity_value::{Dict, Value};

    fn env() -> (indexmap::IndexMap<String, String>, BTreeSet<String>) {
        (indexmap::IndexMap::new(), BTreeSet::new())
    }

    fn dict(pairs: Vec<(&str, Value)>) -> Value {
        let mut d = Dict::new();
        for (k, v) in pairs {
            d.insert(Value::Str(k.to_string()), v);
        }
        Value::Dict(d)
    }

    #[test]
    fn a_template_with_no_partial_renders_exactly_like_render_template() {
        let (declared_prompts, effect_names) = env();
        let rendered = render_with_composition(
            "hi {{name}}",
            &dict(vec![("name", Value::from("world"))]),
            &declared_prompts,
            &effect_names,
            "template",
            &PlainCtx,
            true,
        )
        .unwrap();
        assert_eq!(rendered, "hi world");
    }

    #[test]
    fn escape_false_renders_a_variable_tag_unescaped() {
        let (declared_prompts, effect_names) = env();
        let rendered = render_with_composition(
            "{{s}}",
            &dict(vec![("s", Value::from("<b>"))]),
            &declared_prompts,
            &effect_names,
            "template",
            &PlainCtx,
            false,
        )
        .unwrap();
        assert_eq!(rendered, "<b>");
    }

    #[test]
    fn a_declared_prompt_is_spliced_in_with_its_own_tags_unescaped() {
        let mut declared_prompts = indexmap::IndexMap::new();
        declared_prompts.insert("voice".to_string(), "be {{tone}}".to_string());
        let effect_names = BTreeSet::new();
        let ctx = dict(vec![
            ("tone", Value::from("<brief>")),
            ("prime", dict(vec![])),
        ]);
        let rendered = render_with_composition(
            "Voice: {{> voice}}.",
            &ctx,
            &declared_prompts,
            &effect_names,
            "template",
            &PlainCtx,
            true,
        )
        .unwrap();
        // The outer template keeps escaping (escape=true), but the
        // declared prompt's own `{{tone}}` was rewritten to `{{&tone}}`
        // before the merged text was ever handed to chevron, so it
        // never escapes regardless.
        assert_eq!(rendered, "Voice: be <brief>.");
    }

    #[test]
    fn a_declared_prompt_drops_one_trailing_newline() {
        let mut declared_prompts = indexmap::IndexMap::new();
        declared_prompts.insert("voice".to_string(), "be brief\n".to_string());
        let effect_names = BTreeSet::new();
        let ctx = dict(vec![("prime", dict(vec![]))]);
        let rendered = render_with_composition(
            "{{> voice}}after",
            &ctx,
            &declared_prompts,
            &effect_names,
            "template",
            &PlainCtx,
            true,
        )
        .unwrap();
        assert_eq!(rendered, "be briefafter");
    }

    #[test]
    fn nested_declared_prompts_expand_recursively() {
        let mut declared_prompts = indexmap::IndexMap::new();
        declared_prompts.insert("outer".to_string(), "[{{> inner}}]".to_string());
        declared_prompts.insert("inner".to_string(), "deep".to_string());
        let effect_names = BTreeSet::new();
        let ctx = dict(vec![("prime", dict(vec![]))]);
        let rendered = render_with_composition(
            "{{> outer}}",
            &ctx,
            &declared_prompts,
            &effect_names,
            "template",
            &PlainCtx,
            true,
        )
        .unwrap();
        assert_eq!(rendered, "[deep]");
    }

    #[test]
    fn a_cycle_among_declared_prompts_fails_with_the_exact_text() {
        let mut declared_prompts = indexmap::IndexMap::new();
        declared_prompts.insert("a".to_string(), "{{> b}}".to_string());
        declared_prompts.insert("b".to_string(), "{{> a}}".to_string());
        let effect_names = BTreeSet::new();
        let ctx = dict(vec![("prime", dict(vec![]))]);
        let err = render_with_composition(
            "{{> a}}",
            &ctx,
            &declared_prompts,
            &effect_names,
            "template",
            &PlainCtx,
            true,
        )
        .unwrap_err();
        // `seen` grows one declared-prompt name per nesting level: `a`
        // (seen={}) nests `b` (seen={a}) nests `a` again -- the cycle
        // is only detected one level further in, at `b`'s own level,
        // so the label attributing it is `prompts.b`, not `prompts.a`.
        assert_eq!(err, "prompts.b: cycle among declared prompts at 'a'.");
    }

    #[test]
    fn the_same_declared_prompt_used_twice_side_by_side_is_fine() {
        let mut declared_prompts = indexmap::IndexMap::new();
        declared_prompts.insert("voice".to_string(), "v".to_string());
        let effect_names = BTreeSet::new();
        let ctx = dict(vec![("prime", dict(vec![]))]);
        let rendered = render_with_composition(
            "{{> voice}}{{> voice}}",
            &ctx,
            &declared_prompts,
            &effect_names,
            "template",
            &PlainCtx,
            true,
        )
        .unwrap();
        assert_eq!(rendered, "vv");
    }

    #[test]
    fn a_set_delimiter_inside_a_declared_prompt_is_rejected() {
        let mut declared_prompts = indexmap::IndexMap::new();
        declared_prompts.insert("voice".to_string(), "{{=<% %>=}}".to_string());
        let effect_names = BTreeSet::new();
        let ctx = dict(vec![("prime", dict(vec![]))]);
        let err = render_with_composition(
            "{{> voice}}",
            &ctx,
            &declared_prompts,
            &effect_names,
            "template",
            &PlainCtx,
            true,
        )
        .unwrap_err();
        assert!(err.contains("a declared prompt may not use"), "got: {err}");
    }

    fn effect_node(value: Value) -> Value {
        let mut meta = Dict::new();
        meta.insert(Value::from("created_at"), Value::None);
        let mut node = Dict::new();
        node.insert(Value::from("value"), value);
        node.insert(Value::from("meta"), Value::Dict(meta));
        Value::Dict(node)
    }

    fn ctx_with_prime(entries: Vec<(&str, Value)>) -> Value {
        let mut prime = Dict::new();
        for (k, v) in entries {
            prime.insert(Value::from(k), v);
        }
        dict(vec![("prime", Value::Dict(prime))])
    }

    #[test]
    fn an_effect_reference_splices_its_resolved_value() {
        let declared_prompts = indexmap::IndexMap::new();
        let effect_names = BTreeSet::new();
        let ctx = ctx_with_prime(vec![("reply", effect_node(Value::from("hello")))]);
        let rendered = render_with_composition(
            "said: {{> reply}}",
            &ctx,
            &declared_prompts,
            &effect_names,
            "template",
            &PlainCtx,
            true,
        )
        .unwrap();
        assert_eq!(rendered, "said: hello");
    }

    #[test]
    fn an_effect_references_value_is_never_re_rendered() {
        let declared_prompts = indexmap::IndexMap::new();
        let effect_names = BTreeSet::new();
        let ctx = ctx_with_prime(vec![("reply", effect_node(Value::from("{{input.x}}")))]);
        let rendered = render_with_composition(
            "{{> reply}}",
            &ctx,
            &declared_prompts,
            &effect_names,
            "template",
            &PlainCtx,
            true,
        )
        .unwrap();
        assert_eq!(rendered, "{{input.x}}");
    }

    #[test]
    fn a_present_but_none_effect_value_renders_empty() {
        let declared_prompts = indexmap::IndexMap::new();
        let effect_names = BTreeSet::new();
        let ctx = ctx_with_prime(vec![("reply", effect_node(Value::None))]);
        let rendered = render_with_composition(
            "[{{> reply}}]",
            &ctx,
            &declared_prompts,
            &effect_names,
            "template",
            &PlainCtx,
            true,
        )
        .unwrap();
        assert_eq!(rendered, "[]");
    }

    #[test]
    fn a_known_but_not_yet_run_effect_renders_empty() {
        let declared_prompts = indexmap::IndexMap::new();
        let mut effect_names = BTreeSet::new();
        effect_names.insert("later".to_string());
        let ctx = dict(vec![("prime", dict(vec![]))]);
        let rendered = render_with_composition(
            "[{{> later}}]",
            &ctx,
            &declared_prompts,
            &effect_names,
            "template",
            &PlainCtx,
            true,
        )
        .unwrap();
        assert_eq!(rendered, "[]");
    }

    #[test]
    fn an_unknown_name_fails_with_the_exact_text() {
        let declared_prompts = indexmap::IndexMap::new();
        let effect_names = BTreeSet::new();
        let ctx = dict(vec![("prime", dict(vec![]))]);
        let err = render_with_composition(
            "{{> nope}}",
            &ctx,
            &declared_prompts,
            &effect_names,
            "template",
            &PlainCtx,
            true,
        )
        .unwrap_err();
        assert_eq!(
            err,
            "template: '{{> nope}}' does not name a declared prompt or effect."
        );
    }

    #[test]
    fn an_invalid_name_fails_with_the_exact_text() {
        let declared_prompts = indexmap::IndexMap::new();
        let effect_names = BTreeSet::new();
        let err = render_with_composition(
            "{{> 1bad}}",
            &Value::None,
            &declared_prompts,
            &effect_names,
            "template",
            &PlainCtx,
            true,
        )
        .unwrap_err();
        assert_eq!(err, "template: '{{> 1bad}}' is not a valid name.");
    }

    #[test]
    fn a_dotted_effect_reference_walks_into_a_nested_container() {
        let declared_prompts = indexmap::IndexMap::new();
        let effect_names = BTreeSet::new();
        let mut child = Dict::new();
        child.insert(Value::from("outline"), effect_node(Value::from("plan")));
        let ctx = ctx_with_prime(vec![("pipeline", Value::Dict(child))]);
        let rendered = render_with_composition(
            "{{> pipeline.outline}}",
            &ctx,
            &declared_prompts,
            &effect_names,
            "template",
            &PlainCtx,
            true,
        )
        .unwrap();
        assert_eq!(rendered, "plan");
    }

    #[test]
    fn a_double_brace_opening_no_escape_tag_is_not_a_partial() {
        let declared_prompts = indexmap::IndexMap::new();
        let effect_names = BTreeSet::new();
        let ctx = dict(vec![(">x", Value::from("literal"))]);
        let rendered = render_with_composition(
            "{{{>x}}}",
            &ctx,
            &declared_prompts,
            &effect_names,
            "template",
            &PlainCtx,
            true,
        )
        .unwrap();
        assert_eq!(rendered, "literal");
    }

    #[test]
    fn a_tag_with_an_undoubled_closing_brace_is_not_a_partial() {
        // `{{>a}b}}` is plain text to the scan (the capture group can't
        // extend past the first `}`), so it reaches `render_template`'s
        // own partial rejection instead of this module's own name/
        // effect resolution.
        let declared_prompts = indexmap::IndexMap::new();
        let effect_names = BTreeSet::new();
        let err = render_with_composition(
            "{{>a}b}}",
            &Value::None,
            &declared_prompts,
            &effect_names,
            "template",
            &PlainCtx,
            true,
        )
        .unwrap_err();
        assert!(
            err.contains("partials are not supported"),
            "expected the plain-template partial rejection, got: {err}"
        );
    }

    #[test]
    fn a_hidden_binding_shadows_a_same_named_top_level_key() {
        let declared_prompts = indexmap::IndexMap::new();
        let effect_names = BTreeSet::new();
        // A *real*, pre-existing top-level key already named
        // `__circuitry_partial_0__` -- the first effect reference this
        // template resolves mints exactly that key, so the shallow
        // `{**ctx, **extra}` merge (`HiddenOverlay::lookup`) must have
        // the hidden binding win outright over *this* ctx entry, not
        // merely over a key the overlay's own synthetic naming could
        // never collide with.
        let mut ctx = ctx_with_prime(vec![("reply", effect_node(Value::from("hidden")))]);
        if let Value::Dict(top) = &mut ctx {
            top.insert(
                Value::from("__circuitry_partial_0__"),
                Value::from("shadowed-out"),
            );
        }
        let rendered = render_with_composition(
            "{{> reply}}:{{__circuitry_partial_0__}}",
            &ctx,
            &declared_prompts,
            &effect_names,
            "template",
            &PlainCtx,
            true,
        )
        .unwrap();
        assert_eq!(rendered, "hidden:hidden");
    }

    #[test]
    fn a_dot_at_the_root_still_resolves_with_a_hidden_binding_present() {
        let declared_prompts = indexmap::IndexMap::new();
        let effect_names = BTreeSet::new();
        let ctx = ctx_with_prime(vec![("reply", effect_node(Value::from("hi")))]);
        let rendered = render_with_composition(
            "{{> reply}}{{#.}}[{{__circuitry_partial_0__}}]{{/.}}",
            &ctx,
            &declared_prompts,
            &effect_names,
            "template",
            &PlainCtx,
            true,
        )
        .unwrap();
        // `{{#.}}` at the root pushes `HiddenOverlay::whole()` itself as
        // the new nearest scope -- the only way to prove the hidden key
        // the overlay splices in is actually reachable through
        // `whole()`, not only through `lookup`.
        assert_eq!(rendered, "hi[hi]");
    }

    #[test]
    fn hidden_overlay_contains_never_materializes_a_store_backed_loop_node() {
        use crate::Store;
        use crate::exec::CtxSource;
        use crate::exec::live_scope::LiveRoot;

        let store = Store::new();
        let prime = store.ensure_dict(&store.root, Value::from("prime")).unwrap();
        // A named-loop-shaped node: large enough that a full
        // `Store::snapshot` of it would be a real cost, not just
        // incidentally harmless -- the same shape review finding 1 on
        // issue #492's PR names ("a `prime.<head>` node can be a loop
        // with thousands of passes").
        let named_loop = store
            .ensure_dict(&prime, Value::from("named_loop"))
            .unwrap();
        for i in 0..200 {
            let pass = store
                .ensure_dict(&named_loop, Value::from(format!("iter_{i}")))
                .unwrap();
            store.set_leaf(&pass, Value::from("padding"), Value::from("x".repeat(64)));
        }
        let chain: crate::exec::CtxChain = vec![CtxSource::Live(std::rc::Rc::clone(&store.root))];
        let live_root = LiveRoot::new(&chain, &store);

        let mut hidden = IndexMap::new();
        hidden.insert("__circuitry_partial_0__".to_string(), "x".to_string());
        let overlay = HiddenOverlay {
            hidden: &hidden,
            inner: &live_root,
        };

        assert!(overlay.contains("prime.named_loop"));
        assert!(!overlay.contains("prime.missing"));
        assert!(overlay.contains("__circuitry_partial_0__"));
        assert_eq!(
            store.snapshot_call_count(),
            0,
            "a presence check must never materialize the whole loop subtree"
        );
    }

    #[test]
    fn a_declared_prompt_inside_a_list_section_sees_each_item() {
        let mut declared_prompts = indexmap::IndexMap::new();
        declared_prompts.insert("line".to_string(), "<{{.}}>".to_string());
        let effect_names = BTreeSet::new();
        let ctx = dict(vec![
            ("prime", dict(vec![])),
            (
                "items",
                Value::List(vec![Value::from("a"), Value::from("b")]),
            ),
        ]);
        let rendered = render_with_composition(
            "{{#items}}{{> line}}{{/items}}",
            &ctx,
            &declared_prompts,
            &effect_names,
            "template",
            &PlainCtx,
            true,
        )
        .unwrap();
        assert_eq!(rendered, "<a><b>");
    }

    #[test]
    fn a_set_delimiter_earlier_in_the_template_does_not_block_a_later_partial() {
        let declared_prompts = indexmap::IndexMap::new();
        let mut effect_names = BTreeSet::new();
        effect_names.insert("later".to_string());
        let ctx = dict(vec![("prime", dict(vec![]))]);
        let rendered = render_with_composition(
            "{{=<% %>=}}[{{> later}}]",
            &ctx,
            &declared_prompts,
            &effect_names,
            "template",
            &PlainCtx,
            true,
        )
        .unwrap();
        // The `{{> name}}` scan is purely textual, so it still finds
        // and expands the tag -- but the synthetic `{{{key}}}` binding
        // it splices in gets rendered against whatever delimiters are
        // *active at that point in the template*, same as Python's own
        // textual `re.sub`: the preceding `{{=<% %>=}}` switches chevron
        // over to `<% %>` before it ever reaches the spliced tag, so
        // the standard `{{{...}}}` syntax no longer opens a tag at all
        // and comes out as literal text. This is the documented
        // divergence-free case (both engines reproduce the identical
        // textual flaw), not a bug to fix.
        assert_eq!(rendered, "[{{{__circuitry_partial_0__}}}]");
    }

    #[test]
    fn the_shared_counter_survives_two_sibling_declared_prompts_each_nesting_an_effect() {
        let mut declared_prompts = indexmap::IndexMap::new();
        declared_prompts.insert("a".to_string(), "<{{> x}}>".to_string());
        declared_prompts.insert("b".to_string(), "<{{> y}}>".to_string());
        let effect_names = BTreeSet::new();
        let ctx = ctx_with_prime(vec![
            ("x", effect_node(Value::from("X"))),
            ("y", effect_node(Value::from("Y"))),
        ]);
        let rendered = render_with_composition(
            "{{> a}}{{> b}}",
            &ctx,
            &declared_prompts,
            &effect_names,
            "template",
            &PlainCtx,
            true,
        )
        .unwrap();
        assert_eq!(rendered, "<X><Y>");
    }

    #[test]
    fn a_chain_charged_to_exactly_the_cap_succeeds() {
        // `a: "{{> b}}", b: "x" * 4_194_304` -- `b`'s own resolved text
        // (length N) is charged once as `prompts.a`'s own nested-fragment
        // charge, then charged *again* as `template`'s own charge when
        // `a` itself resolves (the documented double counting: a
        // fragment nested one level deep is charged once per level on
        // its way back out) -- N + N lands at exactly 8388608, the cap
        // itself, which must still succeed (`> MAX_EXPANDED_CHARS`, not
        // `>=`). Confirmed against Python's own `render_with_composition`
        // first: identical outcome, both engines.
        const N: usize = 4 * 1024 * 1024;
        let mut declared_prompts = indexmap::IndexMap::new();
        declared_prompts.insert("a".to_string(), "{{> b}}".to_string());
        declared_prompts.insert("b".to_string(), "x".repeat(N));
        let effect_names = BTreeSet::new();
        let ctx = dict(vec![("prime", dict(vec![]))]);
        let rendered = render_with_composition(
            "{{> a}}",
            &ctx,
            &declared_prompts,
            &effect_names,
            "template",
            &PlainCtx,
            true,
        )
        .unwrap();
        assert_eq!(rendered.len(), N);
    }

    #[test]
    fn a_chain_charged_one_character_over_the_cap_fails_with_the_exact_text() {
        // The same shape, one character longer -- N + N lands at
        // 8388610, one over the cap, which must fail at the *outer*
        // charge (`a`'s own, labelled "template") with exactly Python's
        // own text. Confirmed against Python's own
        // `render_with_composition` first: identical outcome.
        const N: usize = 4 * 1024 * 1024 + 1;
        let mut declared_prompts = indexmap::IndexMap::new();
        declared_prompts.insert("a".to_string(), "{{> b}}".to_string());
        declared_prompts.insert("b".to_string(), "x".repeat(N));
        let effect_names = BTreeSet::new();
        let ctx = dict(vec![("prime", dict(vec![]))]);
        let err = render_with_composition(
            "{{> a}}",
            &ctx,
            &declared_prompts,
            &effect_names,
            "template",
            &PlainCtx,
            true,
        )
        .unwrap_err();
        assert_eq!(
            err,
            "template: '{{> name}}' expansion produced over 8388608 characters — \
             likely runaway nested declared-prompt growth."
        );
    }

    #[test]
    fn an_exponential_declared_prompt_chain_over_the_cap_fails() {
        let mut declared_prompts = indexmap::IndexMap::new();
        let long_leaf = "x".repeat(1024);
        declared_prompts.insert("p0".to_string(), long_leaf);
        for level in 1..=24 {
            declared_prompts.insert(
                format!("p{level}"),
                format!("{{{{> p{}}}}}{{{{> p{}}}}}", level - 1, level - 1),
            );
        }
        let effect_names = BTreeSet::new();
        let ctx = dict(vec![("prime", dict(vec![]))]);
        let err = render_with_composition(
            "{{> p24}}",
            &ctx,
            &declared_prompts,
            &effect_names,
            "template",
            &PlainCtx,
            true,
        )
        .unwrap_err();
        assert!(err.contains("expansion produced over"), "got: {err}");
    }

    #[test]
    fn params_json_style_effect_values_serialize_as_json_through_json_aware_ctx() {
        let declared_prompts = indexmap::IndexMap::new();
        let effect_names = BTreeSet::new();
        let ctx = ctx_with_prime(vec![(
            "symbols",
            effect_node(Value::List(vec![Value::from("AAPL"), Value::from("MSFT")])),
        )]);
        let serializer: &dyn Fn(&Value) -> Result<String, String> = &|v: &Value| Ok(v.py_repr());
        let json_ctx = JsonAwareCtx::new(serializer);
        // A plain splice (PlainCtx-equivalent) uses `py_repr`, matching
        // `str()` of a native Python list -- this test only pins that
        // an effect reference's own list/dict value really does route
        // through `splice_ctx` rather than always using `py_str`.
        let rendered = render_with_composition(
            "{{> symbols}}",
            &ctx,
            &declared_prompts,
            &effect_names,
            "params_json",
            &json_ctx,
            true,
        )
        .unwrap();
        assert_eq!(rendered, "['AAPL', 'MSFT']");
    }

    #[test]
    fn an_empty_list_effect_value_is_never_collapsed_to_empty_string() {
        let declared_prompts = indexmap::IndexMap::new();
        let effect_names = BTreeSet::new();
        let ctx = ctx_with_prime(vec![("items", effect_node(Value::List(Vec::new())))]);
        let rendered = render_with_composition(
            "[{{> items}}]",
            &ctx,
            &declared_prompts,
            &effect_names,
            "template",
            &PlainCtx,
            true,
        )
        .unwrap();
        assert_eq!(rendered, "[[]]");
    }
}

/// Replays `compose_cases` from `electricity/scripts/generate_compose_corpus.py`'s
/// own `tests/golden/compose_corpus.json` a second time -- against a
/// [`crate::exec::live_scope::LiveRoot`] built over a real [`crate::Store`]
/// instead of the plain [`Value`] root `tests/compose_corpus.rs`'s own
/// external replay uses -- proving the lazy path gives the same text as
/// the eager one, the same parity `exec::live_scope::differential_tests`
/// already checks for every other render site. Lives here, in-crate,
/// rather than in `tests/compose_corpus.rs`, because [`crate::exec::
/// live_scope::LiveRoot`]/[`crate::exec::CtxChain`] are `pub(crate)`
/// (this crate's own lazy-rendering seam, issue #463) and can't be named
/// from an external integration-test crate.
#[cfg(test)]
mod golden_corpus_live_root {
    use super::render_with_composition;
    use crate::{NodeRef, Store};
    use crate::exec::CtxSource;
    use crate::exec::live_scope::LiveRoot;
    use electricity_template::{JsonAwareCtx, PlainCtx};
    use electricity_value::Value;
    use indexmap::IndexMap;
    use serde::Deserialize;
    use std::collections::BTreeSet;

    /// A case's own `ctx` is captured as the original JSON bytes
    /// (`Box<RawValue>`, never `serde_json::Value`) and parsed here
    /// through `electricity-json`'s own hand-rolled reader instead --
    /// unconditionally order-preserving, with no `serde_json` feature
    /// (this crate's own `Cargo.toml` comment on its `serde_json`
    /// dev-dependency explains why `preserve_order` isn't that feature).
    fn value_from_raw(raw: &serde_json::value::RawValue) -> Value {
        electricity_json::loads(raw.get()).expect("corpus JSON is valid")
    }

    #[derive(Deserialize)]
    struct FileCorpus {
        compose_cases: Vec<ComposeCase>,
    }

    /// Deserializes straight from the file's own text, not through an
    /// intermediate `serde_json::Value` -- a `Box<RawValue>` field only
    /// keeps its original byte order when the `Deserializer` driving it
    /// still has the source bytes.
    fn load_corpus() -> FileCorpus {
        let text = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/golden/compose_corpus.json"
        ))
        .expect("compose_corpus.json exists -- run electricity/scripts/generate_compose_corpus.py");
        serde_json::from_str(&text).expect("compose_corpus.json is valid JSON")
    }

    #[derive(Deserialize)]
    struct ComposeCase {
        template: String,
        ctx: Box<serde_json::value::RawValue>,
        declared: IndexMap<String, String>,
        known_effect_names: Vec<String>,
        label: String,
        escape: bool,
        json_aware: bool,
    }

    fn json_aware_splice(value: &Value) -> Result<String, String> {
        electricity_json::dumps_default_str(
            value,
            electricity_json::WriteMode {
                indent: None,
                sort_keys: false,
                ensure_ascii: false,
                separators: electricity_json::Separators::Default,
            },
        )
        .map_err(|err| err.to_string())
    }

    /// Writes *ctx*'s own dict-valued entries (recursively, including
    /// nested dicts at any depth) as real [`Store::ensure_dict`] nodes,
    /// everything else as a plain leaf -- so this replay's own `prime.
    /// <head>` is a genuine [`crate::Slot::Node`], the same shape a real
    /// run's store has, not a single [`Value`] leaf holding the whole
    /// subtree as one already-materialized dict. [`LiveRoot`]'s own
    /// `contains`/`lookup` only actually walk live nodes (`Store::peek`,
    /// never a `materialize`/`snapshot`) when there's a live node under
    /// them to walk through in the first place -- a flat top-level leaf
    /// here would make every one of this replay's own cases resolve
    /// through [`electricity_template::step`] from the very first
    /// segment on, the same as the eager, plain-`Value`-rooted path,
    /// never actually exercising [`LiveRoot`]'s own node-walking branches
    /// at all (issue #492 review finding 7).
    fn seed_store_from_ctx(store: &Store, parent: &NodeRef, value: &Value) {
        let Value::Dict(dict) = value else {
            return;
        };
        for (key, child_value) in dict {
            let Value::Str(key) = key else { continue };
            match child_value {
                Value::Dict(_) => {
                    let child = store
                        .ensure_dict(parent, Value::Str(key.clone()))
                        .expect("a fresh key never collides with a non-dict sibling");
                    seed_store_from_ctx(store, &child, child_value);
                }
                other => store.set_leaf(parent, Value::Str(key.clone()), other.clone()),
            }
        }
    }

    fn run_case<R: electricity_template::ScopeRoot>(
        case: &ComposeCase,
        root: &R,
        known: &BTreeSet<String>,
    ) -> Result<String, String> {
        if case.json_aware {
            let serializer: &dyn Fn(&Value) -> Result<String, String> = &json_aware_splice;
            let json_ctx = JsonAwareCtx::new(serializer);
            render_with_composition(
                &case.template,
                root,
                &case.declared,
                known,
                &case.label,
                &json_ctx,
                case.escape,
            )
        } else {
            render_with_composition(
                &case.template,
                root,
                &case.declared,
                known,
                &case.label,
                &PlainCtx,
                case.escape,
            )
        }
    }

    #[test]
    fn compose_cases_match_between_a_plain_value_root_and_a_live_root() {
        let corpus = load_corpus();
        assert!(!corpus.compose_cases.is_empty());
        for case in &corpus.compose_cases {
            let known: BTreeSet<String> = case.known_effect_names.iter().cloned().collect();
            let eager_root = value_from_raw(&case.ctx);
            let eager = run_case(case, &eager_root, &known);

            let store = Store::new();
            seed_store_from_ctx(&store, &store.root, &eager_root);
            let chain = vec![CtxSource::Live(std::rc::Rc::clone(&store.root))];
            let lazy_root = LiveRoot::new(&chain, &store);
            let lazy = run_case(case, &lazy_root, &known);

            assert_eq!(
                eager, lazy,
                "template {:?} diverged between the eager Value root and the lazy LiveRoot",
                case.template
            );
        }
    }
}
