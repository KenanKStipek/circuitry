//! A chevron port: Circuitry's Mustache templates (`core/templates.py`,
//! wrapping the third-party `chevron` library), over
//! [`electricity_value::Value`], line-for-line rather than a generic
//! Mustache engine (DESIGN.md §3.3, runtime-semantics §3).
//!
//! ```
//! use electricity_template::{render_template, PlainCtx, Value};
//!
//! let mut ctx = Value::Dict(Default::default());
//! ctx.as_dict_mut()
//!     .unwrap()
//!     .insert(Value::Str("name".into()), Value::Str("world".into()));
//! let out = render_template("hello {{name}}", &ctx, &PlainCtx, "template").unwrap();
//! assert_eq!(out, "hello world");
//! ```
//!
//! ## Known divergences from chevron
//!
//! This port matches chevron by output for every shape the production
//! document sets and the conformance corpus exercise (`render.rs`'s module
//! docs). The following are confirmed differences against real chevron,
//! each judged unreachable by an ordinary orchestration template (state is
//! always a dict; values come from YAML/JSON/tool output) and left as-is
//! rather than fixed in this port:
//!
//! - **Falsy root `{{.}}`.** `{{.}}` never falls through chevron's usual
//!   falsy-to-`""` collapse (`_get_key` returns `scopes[0]` for `"."`
//!   before that collapse would apply), so `{{.}}` against a falsy,
//!   non-numeric root (`{}`, `[]`, `None`) renders Python's `str()` of it
//!   (`"{}"`, `"[]"`, `"None"`). This port's `stringify_for_variable`
//!   applies the same falsy-collapse to every scope, including the `.`
//!   case, so it renders `""` there instead. Needs a root that is itself
//!   falsy and a template that reads `{{.}}` directly against it.
//! - **Bytes iterated in a section.** Python's `bytes` registers as a
//!   `collections.abc.Sequence`, so `{{#data}}...{{/data}}` over a
//!   `bytes` value iterates it byte by byte, each byte an `int` scope.
//!   This port's `Value::List` is the only section-iterable type, so
//!   `Value::Bytes` renders its section body once, with the whole byte
//!   string as scope, like any other truthy scalar.
//! - **The attribute-fallback mismatch (tracked upstream, not here).**
//!   `_get_key`'s second lookup step is `getattr(scope, child)`: inside a
//!   section whose scope is a `str` (or any Python object), a tag whose
//!   name happens to match one of that object's attributes or methods
//!   (`count`, `index`, `format`, a date's `.isoformat`/`.year`, ...)
//!   resolves to Python's description of that attribute/bound method —
//!   e.g. `{{#title}}{{title}}{{/title}}` with `title: "Intro"` renders
//!   `<built-in method title of str object at 0x...>`, not `"Intro"` —
//!   instead of falling through to the next scope up the way a plain
//!   missing key would. The text contains a memory address, so it cannot
//!   be ported byte for byte even in principle. This port has no
//!   `getattr` step at all, so a name shadowing an attribute/method
//!   simply falls through to the outer scope — the value a template
//!   author actually meant. This is a genuine Circuitry rendering bug
//!   (reachable by an ordinary `{{#title}}{{title}}{{/title}}`-shaped
//!   template), tracked as a separate upstream issue rather than fixed or
//!   reproduced in this port.
//! - **`int()` leniency.** Python's `int()` accepts leading/trailing
//!   whitespace (`" 1"`), underscore digit grouping (`"1_0"` == 10), and
//!   arbitrary precision. A dotted segment or set-delimiter split in this
//!   port parses with `str::parse::<i64>`, which accepts none of those.
//!   Needs a dotted key or index written with internal whitespace,
//!   underscores, or a number past `i64::MAX`.
//! - **`\x1c`-`\x1f` as whitespace.** Python's `str.isspace()` (chevron's
//!   standalone-tag whitespace check) treats the C0 control characters
//!   `\x1c`-`\x1f` (FS/GS/RS/US) as whitespace; Rust's `char::is_whitespace`
//!   does not. A standalone-tag line padded with only these characters is
//!   trimmed by chevron but not by this port. Needs one of these four
//!   control characters on a line with a tag.
//! - **An empty set-delimiter tag.** `{{= =}}` sets both delimiters to
//!   `""`; chevron's own literal-splitting (`template.split('', 1)`)
//!   raises `ValueError` on an empty separator, which it catches by
//!   treating the *entire rest of the template* as one literal — no more
//!   tags are ever recognized after it. This port's literal-splitting
//!   does not special-case an empty delimiter the same way, so it keeps
//!   tokenizing tags normally afterward. Needs a template that sets an
//!   empty delimiter and still expects tags afterward to stop parsing.
//! - **A same-key inverted section inside a list section.** Chevron
//!   gathers a list section's body by counting nested `('section', key)`
//!   opens against `('end', key)` closes to find its own matching end —
//!   but an inner *inverted* section with the same key is never counted
//!   as an open, so its `{{/key}}` is miscounted as closing the outer
//!   section early (e.g. `{{#a}}{{^a}}x{{/a}}{{/a}}`). This port builds a
//!   real tree (`render.rs`'s `build_tree`) from a stack that already
//!   distinguishes `Section`/`InvertedSection`, so it nests correctly
//!   instead of reproducing chevron's miscount. Needs a list section
//!   containing an inverted section that reuses the same key.
//!
//! Every lookup in `render.rs` (`get_key`/`walk_dotted`/`step`) clones the
//! `Value` it resolves rather than walking by reference; correct, and
//! simple, but it means `render_with_pushed_scope` copies the whole scope
//! stack — root state included — once per list element. No caller exists
//! yet to measure against; walking by reference instead is a later
//! optimization, not a correctness fix, and is deliberately deferred.

mod render;
mod tokenizer;

pub use electricity_value::Value;
pub use render::{JsonAwareCtx, PlainCtx, SpliceCtx};

use tokenizer::{Tag, TokenizeError, TokenizeFailure};

/// Why a template could not be rendered. Mirrors `core/templates.py`'s
/// `TemplateError`, keeping its two-tier message exactly: a malformed
/// template (an unclosed tag, a mismatched section close, an unsupported
/// partial) says `"malformed Mustache template"`; a render-time failure
/// against the data it was handed says `"could not render"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateError {
    label: String,
    kind: TemplateErrorKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TemplateErrorKind {
    Syntax(String),
    Render(String),
}

impl std::fmt::Display for TemplateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.kind {
            TemplateErrorKind::Syntax(msg) => {
                write!(f, "{}: malformed Mustache template: {msg}", self.label)
            }
            TemplateErrorKind::Render(msg) => {
                write!(f, "{}: could not render: {msg}", self.label)
            }
        }
    }
}

impl std::error::Error for TemplateError {}

enum ValidateFailure {
    Syntax(String),
    Other(String),
}

/// The first partial tag's name among *tokens*, in order, if any.
fn first_partial(tokens: &[Tag]) -> Option<&str> {
    tokens.iter().find_map(|tag| match tag {
        Tag::Partial(name) => Some(name.as_str()),
        _ => None,
    })
}

/// Tokenize *template* and reject it if it contains a Mustache partial
/// tag (`{{> name}}`) — partials are not supported (DESIGN.md §1,
/// runtime-semantics §3.1): chevron's own partial loading reads an
/// arbitrary file from the process's working directory by name, which
/// this port never does.
///
/// Chevron's own tokenizer is a *generator*; `core/templates.py`'s
/// `_reject_partials` consumes it one token at a time and raises on the
/// first `"partial"` token, before the generator is ever resumed to
/// produce whatever token comes after it. So a partial tag wins over any
/// later tokenize failure (an unclosed tag, a mismatched section close) —
/// `{{> p}}{{/x}}` reports the partial, not the unopened `{{/x}}`. This
/// port tokenizes eagerly (see `tokenizer.rs`'s module docs), so it
/// replays that ordering by checking the tokens produced *before* a
/// failure (if any) for a partial first, and only falling back to the
/// failure itself when none is found.
fn validate_and_reject_partials(template: &str) -> Result<Vec<Tag>, ValidateFailure> {
    match tokenizer::tokenize(template) {
        Ok(tokens) => {
            if let Some(name) = first_partial(&tokens) {
                return Err(ValidateFailure::Syntax(format!(
                    "partials are not supported: {{{{> {name}}}}}"
                )));
            }
            Ok(tokens)
        }
        Err(TokenizeError {
            failure,
            tokens_before_failure,
        }) => {
            if let Some(name) = first_partial(&tokens_before_failure) {
                return Err(ValidateFailure::Syntax(format!(
                    "partials are not supported: {{{{> {name}}}}}"
                )));
            }
            match failure {
                TokenizeFailure::Syntax(_) => Err(ValidateFailure::Syntax(failure.describe())),
                TokenizeFailure::Index(_) => Err(ValidateFailure::Other(failure.describe())),
            }
        }
    }
}

/// Why *template* is not valid Mustache (with partials rejected), or
/// `None` when it parses. Mirrors `core/templates.py::template_syntax_error`
/// exactly, including that it returns the same joined description
/// whether the failure is chevron's own `ChevronError` or, in the one
/// case chevron itself doesn't raise that type for (an empty tag,
/// `{{}}`), a plain index-out-of-range message.
pub fn template_syntax_error(template: &str) -> Option<String> {
    match validate_and_reject_partials(template) {
        Ok(_) => None,
        Err(ValidateFailure::Syntax(msg)) => Some(msg),
        Err(ValidateFailure::Other(msg)) => Some(msg),
    }
}

/// Render *template* against *root* through *ctx* ([`PlainCtx`] for every
/// ordinary template site, [`JsonAwareCtx`] only for a tool's
/// `params_json`, DESIGN.md §3.3). Mirrors
/// `core/templates.py::render_template` exactly, including which
/// failures are "malformed" vs. "could not render" (see
/// [`TemplateError`]) and *label*'s place in the message
/// (`"{label}: ..."`, the field name the caller is rendering — e.g.
/// `"template"`, `"params_json"`, `"messages[0].content"`).
pub fn render_template(
    template: &str,
    root: &Value,
    ctx: &dyn SpliceCtx,
    label: &str,
) -> Result<String, TemplateError> {
    let tokens = match validate_and_reject_partials(template) {
        Ok(tokens) => tokens,
        Err(ValidateFailure::Syntax(msg)) => {
            return Err(TemplateError {
                label: label.to_string(),
                kind: TemplateErrorKind::Syntax(msg),
            });
        }
        Err(ValidateFailure::Other(msg)) => {
            return Err(TemplateError {
                label: label.to_string(),
                kind: TemplateErrorKind::Render(msg),
            });
        }
    };
    let tree = render::build_tree(&tokens);
    let scopes = [root.clone()];
    render::render_nodes(&tree, &scopes, ctx).map_err(|msg| TemplateError {
        label: label.to_string(),
        kind: TemplateErrorKind::Render(msg),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::Dict;

    fn dict(pairs: Vec<(&str, Value)>) -> Value {
        let mut d = Dict::new();
        for (k, v) in pairs {
            d.insert(Value::Str(k.to_string()), v);
        }
        Value::Dict(d)
    }

    #[test]
    fn plain_variable_and_escaping() {
        let ctx = dict(vec![("s", Value::Str("<b>&'\"".to_string()))]);
        let out = render_template("{{s}}", &ctx, &PlainCtx, "template").unwrap();
        assert_eq!(out, "&lt;b&gt;&amp;'&quot;");
    }

    #[test]
    fn no_escape_variants() {
        let ctx = dict(vec![("s", Value::Str("<b>".to_string()))]);
        assert_eq!(
            render_template("{{{s}}}", &ctx, &PlainCtx, "t").unwrap(),
            "<b>"
        );
        assert_eq!(
            render_template("{{&s}}", &ctx, &PlainCtx, "t").unwrap(),
            "<b>"
        );
    }

    #[test]
    fn missing_key_renders_empty() {
        let ctx = dict(vec![]);
        assert_eq!(
            render_template("[{{missing}}]", &ctx, &PlainCtx, "t").unwrap(),
            "[]"
        );
    }

    #[test]
    fn dotted_missing_segment_renders_empty() {
        let ctx = dict(vec![("a", dict(vec![("b", dict(vec![]))]))]);
        assert_eq!(
            render_template("[{{a.b.c}}]", &ctx, &PlainCtx, "t").unwrap(),
            "[]"
        );
    }

    #[test]
    fn list_index() {
        let ctx = dict(vec![(
            "items",
            Value::List(vec![Value::Str("a".into()), Value::Str("b".into())]),
        )]);
        assert_eq!(
            render_template("{{items.1}}", &ctx, &PlainCtx, "t").unwrap(),
            "b"
        );
    }

    #[test]
    fn section_over_list() {
        let ctx = dict(vec![(
            "items",
            Value::List(vec![Value::Int(1.into()), Value::Int(2.into())]),
        )]);
        assert_eq!(
            render_template("{{#items}}[{{.}}]{{/items}}", &ctx, &PlainCtx, "t").unwrap(),
            "[1][2]"
        );
    }

    #[test]
    fn section_skips_falsy_elements_entirely() {
        // `False`/`0` elements must produce no output at all for their
        // iteration (not even the surrounding literal `<`/`>`) —
        // confirmed against real chevron. `True` is deliberately excluded
        // here since it also triggers the unrelated `.`-plus-`True` bug
        // covered by `dot_true_bug_swaps_to_outer_scope`.
        let ctx = dict(vec![(
            "items",
            Value::List(vec![
                Value::Int(1.into()),
                Value::Bool(false),
                Value::Int(0.into()),
                Value::Str("x".to_string()),
            ]),
        )]);
        assert_eq!(
            render_template("{{#items}}<{{.}}>{{/items}}", &ctx, &PlainCtx, "t").unwrap(),
            "<1><x>"
        );
    }

    #[test]
    fn inverted_section() {
        let ctx = dict(vec![("x", Value::Bool(false))]);
        assert_eq!(
            render_template("{{^x}}empty{{/x}}", &ctx, &PlainCtx, "t").unwrap(),
            "empty"
        );
        let ctx2 = dict(vec![("x", Value::Bool(true))]);
        assert_eq!(
            render_template("{{^x}}empty{{/x}}", &ctx2, &PlainCtx, "t").unwrap(),
            ""
        );
    }

    #[test]
    fn zero_and_false_render_literally() {
        let ctx = dict(vec![("z", Value::Int(0.into())), ("f", Value::Bool(false))]);
        assert_eq!(render_template("{{z}}", &ctx, &PlainCtx, "t").unwrap(), "0");
        assert_eq!(
            render_template("{{f}}", &ctx, &PlainCtx, "t").unwrap(),
            "False"
        );
    }

    #[test]
    fn dot_true_bug_swaps_to_outer_scope() {
        let ctx = dict(vec![
            ("flag", Value::Bool(true)),
            ("marker", Value::Str("OUTER".into())),
        ]);
        let out = render_template("{{#flag}}{{.}}{{/flag}}", &ctx, &PlainCtx, "t").unwrap();
        assert_eq!(out, "{'flag': True, 'marker': 'OUTER'}");
    }

    #[test]
    fn dot_true_bug_raises_without_outer_scope() {
        let err = render_template("{{.}}", &Value::Bool(true), &PlainCtx, "template").unwrap_err();
        assert_eq!(
            err.to_string(),
            "template: could not render: list index out of range"
        );
    }

    #[test]
    fn dot_true_bug_does_not_affect_no_escape() {
        let out = render_template("{{{.}}}", &Value::Bool(true), &PlainCtx, "t").unwrap();
        assert_eq!(out, "True");
    }

    #[test]
    fn partial_is_rejected() {
        let err = render_template("{{> name}}", &dict(vec![]), &PlainCtx, "template").unwrap_err();
        assert_eq!(
            err.to_string(),
            "template: malformed Mustache template: partials are not supported: {{> name}}"
        );
        assert_eq!(
            template_syntax_error("{{> name}}"),
            Some("partials are not supported: {{> name}}".to_string())
        );
    }

    #[test]
    fn partial_wins_over_a_later_unopened_close() {
        // A partial tag encountered first wins over a syntax error that
        // would only surface later in the template -- chevron's own
        // `_reject_partials` consumes its tokenizer lazily and raises on
        // the first partial token before ever resuming the generator to
        // reach the mismatched `{{/x}}`.
        assert_eq!(
            template_syntax_error("{{> p}}{{/x}}"),
            Some("partials are not supported: {{> p}}".to_string())
        );
    }

    #[test]
    fn partial_wins_over_a_later_empty_tag() {
        // Same ordering, but the later failure is the "could not
        // render"-tier `{{}}` index error -- the partial still wins and
        // reports as "malformed", not "could not render".
        assert_eq!(
            template_syntax_error("{{> p}}{{}}"),
            Some("partials are not supported: {{> p}}".to_string())
        );
        let err = render_template("{{> p}}{{}}", &dict(vec![]), &PlainCtx, "template").unwrap_err();
        assert_eq!(
            err.to_string(),
            "template: malformed Mustache template: partials are not supported: {{> p}}"
        );
    }

    #[test]
    fn unclosed_tag_is_syntax_error() {
        assert_eq!(
            template_syntax_error("{{a"),
            Some("unclosed tag at line 1".to_string())
        );
        let err = render_template("{{a", &dict(vec![]), &PlainCtx, "template").unwrap_err();
        assert_eq!(
            err.to_string(),
            "template: malformed Mustache template: unclosed tag at line 1"
        );
    }

    #[test]
    fn empty_tag_is_could_not_render_not_malformed() {
        assert_eq!(
            template_syntax_error("{{}}"),
            Some("string index out of range".to_string())
        );
        let err = render_template("{{}}", &dict(vec![]), &PlainCtx, "template").unwrap_err();
        assert_eq!(
            err.to_string(),
            "template: could not render: string index out of range"
        );
    }

    #[test]
    fn json_aware_ctx_keeps_falsy_containers() {
        let serializer = |v: &Value| -> Result<String, String> {
            match v {
                Value::List(items) if items.is_empty() => Ok("[]".to_string()),
                Value::Dict(d) if d.is_empty() => Ok("{}".to_string()),
                _ => Ok(v.py_repr()),
            }
        };
        let json_ctx = JsonAwareCtx::new(&serializer);
        let ctx = dict(vec![("items", Value::List(vec![]))]);
        assert_eq!(
            render_template("[{{items}}]", &ctx, &json_ctx, "params_json").unwrap(),
            "[[]]"
        );
        assert_eq!(
            render_template("[{{items}}]", &ctx, &PlainCtx, "t").unwrap(),
            "[]"
        );
    }

    #[test]
    fn json_aware_ctx_serializer_error_is_could_not_render() {
        let failing = |_: &Value| -> Result<String, String> { Err("boom".to_string()) };
        let json_ctx = JsonAwareCtx::new(&failing);
        let ctx = dict(vec![("items", Value::List(vec![Value::Int(1.into())]))]);
        let err = render_template("{{items}}", &ctx, &json_ctx, "params_json").unwrap_err();
        assert_eq!(err.to_string(), "params_json: could not render: boom");
    }
}
