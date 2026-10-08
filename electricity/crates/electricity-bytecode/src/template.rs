//! Lane A: `TemplateText`/`Escape` (issue #408's Scope section;
//! runtime-semantics §3.4, #397).

use serde::Serialize;

/// Whether a rendered template's substitutions get the chevron
/// (mustache-style) default HTML escape, or none at all.
///
/// `None` marks prompt text (#397): a prompt sent to a model is never
/// HTML — escaping it would corrupt the text the model actually sees.
/// `Html` is every other templated field (tool `params`, `use` `inputs`,
/// `yield`'s `inputs`-side composition, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Escape {
    Html,
    None,
}

/// One templated string, kept in both its raw and (once rendered) tagged
/// forms.
///
/// The raw `source` is kept rather than a pre-tokenized form because
/// `{{> name}}` partial expansion (runtime-semantics §3.4, the compile-
/// time half of #406 this lane sets up for) splices text into `source`
/// *before* tokenizing — tokenizing first would have nothing to splice
/// into.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TemplateText {
    pub source: String,
    /// `true` for the fields where `{{> name}}` partials are allowed
    /// (runtime-semantics §3.4's table): prompt `template`/`messages[].
    /// content`, `yield.template`, a declared prompt's own text, tool
    /// `prompt`/`params`/`params_json`, and `use.inline`/`use.inputs`.
    /// `false` only for `if`/`while.template`, `expect.template` and
    /// asset `ref` — there a `{{> name}}` fragment is always the plain
    /// malformed-tag error, never a partial reference.
    pub composable: bool,
    pub escape: Escape,
}

impl TemplateText {
    pub fn new(source: impl Into<String>, composable: bool, escape: Escape) -> Self {
        TemplateText {
            source: source.into(),
            composable,
            escape,
        }
    }
}
