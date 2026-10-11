//! Run-time prompt composition (`core/prompt_compose.py::
//! render_with_composition`) -- lane P fills the real `{{> name}}`
//! splice; this lane (#449's gate, item 8) already routes every call
//! site through this one function so P's own landing never has to add
//! this function's own parameters to a call site in another lane's
//! final file (`params.rs`/`exec/tool.rs`, review finding 1 on PR
//! #454). A template with no `{{> name}}` tag at all renders exactly
//! as a bare `render_template` call would -- the refusal walker
//! already keeps every other kind of template out of this function
//! entirely (`RefusalReason::PartialReference`), so this is the one
//! path every M0 render still takes, byte for byte. Only a template
//! that does carry a partial tag -- unreachable through the walker
//! today, reachable only by calling this function directly, as this
//! module's own tests below do -- hits the M1-P stub error, pending
//! real `{{> name}}` expansion against *declared_prompts*/
//! *effect_names*, the 8 MiB cap, and the "not-yet-run effect renders
//! empty" rule.

use electricity_template::{ScopeRoot, SpliceCtx};
use std::collections::BTreeSet;

/// Renders *template* with `{{> name}}` partials spliced in from
/// *declared_prompts* against *ctx* -- the one rendering path every
/// prompt-text site in the VM uses (runtime-semantics.md \u00a73.4's
/// own table). *label* is the budget/error attribution text
/// (`"template"` for Python's own default) and the final template
/// render's own error attribution; *splice_ctx* is
/// [`electricity_template::PlainCtx`]/[`electricity_template::JsonAwareCtx`],
/// same split `render_template`'s own callers already choose between;
/// *escape* is the underlying Mustache render's own HTML-escape flag
/// (`true` for Python's own default, carried here from the field's own
/// compiled [`electricity_bytecode::Escape`] already, review
/// finding/P2-e on PR #454) -- not yet consumed by [`render_template`]
/// itself (which has no such toggle today; wiring it in is lane P's
/// own "no-escape rendering" deliverable), kept here so that lane's
/// landing only has to fill this function's own body, never widen its
/// signature again.
pub fn render_with_composition<R: ScopeRoot>(
    template: &str,
    ctx: &R,
    _declared_prompts: &indexmap::IndexMap<String, String>,
    _effect_names: &BTreeSet<String>,
    label: &str,
    splice_ctx: &dyn SpliceCtx,
    _escape: bool,
) -> Result<String, String> {
    if !has_partial(template) {
        return electricity_template::render_template_with_root(template, ctx, splice_ctx, label)
            .map_err(|err| err.to_string());
    }
    Err(format!(
        "{template:?}: run-time prompt composition is not supported by this build of \
         electricity yet (M1-P)"
    ))
}

/// Whether *template* contains a `{{> name}}` partial tag -- same test
/// `electricity_bytecode::refusal::has_partial`/`param_node_has_partial`
/// run at refuse-up-front time, so a document that reaches this
/// function at all never has one (this module's own tests below call
/// it directly to exercise the M1-P stub path the walker otherwise
/// keeps unreachable).
fn has_partial(template: &str) -> bool {
    template.contains("{{>")
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_template::PlainCtx;
    use electricity_value::Value;

    fn env() -> (indexmap::IndexMap<String, String>, BTreeSet<String>) {
        (indexmap::IndexMap::new(), BTreeSet::new())
    }

    #[test]
    fn a_template_with_no_partial_renders_exactly_like_render_template() {
        let (declared_prompts, effect_names) = env();
        let rendered = render_with_composition(
            "hi {{name}}",
            &Value::Dict({
                let mut d = electricity_value::Dict::new();
                d.insert(Value::Str("name".to_string()), Value::from("world"));
                d
            }),
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
    fn a_partial_reaches_the_m1_p_stub_error_not_templates_own_rejection() {
        let (declared_prompts, effect_names) = env();
        let err = render_with_composition(
            "{{> greeting}}",
            &Value::None,
            &declared_prompts,
            &effect_names,
            "template",
            &PlainCtx,
            true,
        )
        .unwrap_err();
        assert!(
            err.contains("run-time prompt composition is not supported"),
            "expected the M1-P stub error, got: {err}"
        );
        assert!(
            !err.contains("partials are not supported"),
            "a partial must reach this function's own stub, not \
             electricity-template's partial rejection: {err}"
        );
    }
}
