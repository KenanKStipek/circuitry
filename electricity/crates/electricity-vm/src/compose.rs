//! Run-time prompt composition (`core/prompt_compose.py::
//! render_with_composition`) -- signature only; lane P fills the body
//! (issue #449's gate lane, item 8; the run-time half of #406):
//! `{{> name}}` partial expansion against a document's own declared
//! `prompts:`/effect names, no-escape Mustache rendering, the 8 MiB
//! cap, and the "not-yet-run effect renders empty" rule.

use electricity_value::Value;
use std::collections::BTreeSet;

/// Renders *template* with `{{> name}}` partials spliced in from
/// *declared_prompts* against *ctx* -- the one rendering path every
/// prompt-text site in the VM uses once this lane lands (runtime-
/// semantics.md \u00a73.4's own table). *label* is the budget/error
/// attribution text (`"template"` for Python's own default) and
/// the final template render's own error attribution; *escape*
/// is the underlying Mustache render's own HTML-escape flag (`true`
/// for Python's own default) -- review finding/P2-e on PR #454:
/// `prompt_compose.py:913-921`'s own `render_with_composition`
/// takes both, and a splice-expanded template's own non-partial
/// tags still render through them exactly as a bare
/// `render_template` call would.
pub fn render_with_composition(
    template: &str,
    _ctx: &Value,
    _declared_prompts: &indexmap::IndexMap<String, String>,
    _effect_names: &BTreeSet<String>,
    _label: &str,
    _escape: bool,
) -> Result<String, String> {
    Err(format!(
        "{template:?}: run-time prompt composition is not supported by this build of \
         electricity yet (M1-P)"
    ))
}
