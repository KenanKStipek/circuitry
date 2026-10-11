//! The human-readable text a [`crate::Refusal`] renders as -- moved out
//! of `electricity` (lib)'s own `src/lib.rs` (M1 lane I prerequisites,
//! issue #448) so a mid-run refusal (a freshly-compiled `use` child
//! that [`crate::first_unsupported`] rejects once its own path is
//! actually reached, lane I's own job) can report the exact same
//! preview-marker phrasing an up-front refusal already does, without
//! `electricity-vm` depending on the `electricity` (lib) crate itself
//! (DESIGN.md §2's own dependency direction: `vm` never depends on the
//! top crate that in turn depends on it).
//!
//! [`VERSION`] is this crate's own `CARGO_PKG_VERSION`, which --
//! `version.workspace = true` in every crate's own `Cargo.toml` -- is
//! always the same string `electricity` (lib)'s own `VERSION` const
//! is, so [`format_refusal`]'s own output is unchanged from before this
//! move (every existing CLI/refusal test keeps passing unmodified).

use crate::refusal::{Refusal, RefusalReason};

/// This crate's own version -- see this module's own doc comment for
/// why it is always identical to `electricity` (lib)'s own `VERSION`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// `electricity_bytecode::RefusalReason` -> the human-readable half of
/// the refusal message [`format_refusal`] builds (issue #431's gate
/// lane: "the refusal message keeps the existing preview marker phrase
/// and adds the effect path and the reason").
pub fn refusal_reason_text(reason: &RefusalReason) -> String {
    match reason {
        RefusalReason::Prompt => "a prompt effect".to_string(),
        RefusalReason::Loop => "a loop effect".to_string(),
        RefusalReason::Use => "a use effect".to_string(),
        RefusalReason::Reflector => "a reflector effect".to_string(),
        RefusalReason::Yield => "a yield effect".to_string(),
        RefusalReason::ModelCondition => "an if/while condition in mode: model".to_string(),
        RefusalReason::ModelExpect => "an expect: in mode: model".to_string(),
        RefusalReason::DeclaredPrompts => "a document that declares prompts:".to_string(),
        RefusalReason::UnsupportedToolProvider(provider) => {
            format!("a tool effect with provider {provider:?}")
        }
        RefusalReason::PartialReference => "an unexpanded {{> name}} partial reference".to_string(),
        RefusalReason::DocumentAdapter(adapter) => format!(
            "a document that names adapter {adapter:?}, and electricity does not run preflight checks yet"
        ),
        RefusalReason::ComplexityEnabled => {
            "a run whose effective config enables complexity scoring, routing or decomposition"
                .to_string()
        }
    }
}

/// The exact text an up-front refusal reports: the preview marker
/// phrase (kept so the conformance runner's "refusal keeps the preview
/// marker" skip rule keeps working), the effect path, and the reason.
pub fn format_refusal(refusal: &Refusal) -> String {
    format!(
        "electricity {VERSION} is a preview and cannot run orchestrations yet: {} is {}; use `cof run` instead",
        refusal.path,
        refusal_reason_text(&refusal.reason)
    )
}

/// The same preview-marker phrasing as [`format_refusal`], for a
/// refusal found *mid-run* rather than up front -- a freshly-compiled
/// `use` child whose own path the static walker could never see ahead
/// of time (M1 lane I, `electricity_vm::VmError::Refused`). *child_label*
/// names the child document the way `cli/allowlist.py::check_allowlist`'s
/// own `f"use child {label}: {denial}"` already does for the same kind
/// of per-child reporting, so a user sees the same wording for a
/// static allowlist denial and a mid-run capability refusal of the
/// same child.
pub fn format_refused_child(child_label: &str, refusal: &Refusal) -> String {
    format!(
        "electricity {VERSION} is a preview and cannot run orchestrations yet: use child {child_label}: {} is {}; use `cof run` instead",
        refusal.path,
        refusal_reason_text(&refusal.reason)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::EffectPath;

    #[test]
    fn format_refusal_names_the_path_and_reason() {
        let refusal = Refusal {
            path: EffectPath::root().push_name("step"),
            reason: RefusalReason::Loop,
        };
        let text = format_refusal(&refusal);
        assert!(text.starts_with(&format!(
            "electricity {VERSION} is a preview and cannot run orchestrations yet: "
        )));
        assert!(text.contains("prime.step is a loop effect"));
        assert!(text.ends_with("use `cof run` instead"));
    }

    #[test]
    fn format_refused_child_names_the_child_and_the_path() {
        let refusal = Refusal {
            path: EffectPath::root().push_name("step"),
            reason: RefusalReason::Use,
        };
        let text = format_refused_child("child.yaml", &refusal);
        assert!(text.contains("use child child.yaml: prime.step is a use effect"));
    }
}
