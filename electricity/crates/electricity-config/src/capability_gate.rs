//! `circuitry.capability_gate`, ported -- the run's own consented
//! capability ceiling, and the check a `tool`/`use`/decomposition
//! call makes against it before running (review P2-8 on PR #454's
//! second review: vm-lanes.md named this seam with no stub of its own
//! -- lane R installs the ceiling once preflight/consent lands;
//! [`require_within_ceiling`] is this crate's one stub, lane L2's own
//! decomposition call site, `core/decompose.py::_check_capability_ceiling`).
//!
//! [`capability_ceiling`]/[`capabilities_beyond_ceiling`] are real and
//! complete now: both are pure reads/set-differences of
//! *runtime_config*, nothing left for any lane to fill in.
//! [`require_within_ceiling`] is signature only -- lane R/L2's own
//! exact raise-vs-refuse wiring decides what a electricity run does
//! with a [`CapabilityConsentError`] (M0-H has no consent flow of its
//! own at all yet; `cli/document_consent.py`'s own interactive prompt
//! is out of electricity's scope, not just unbuilt).

use electricity_value::Value;
use std::collections::BTreeSet;
use std::fmt;

/// `circuitry.capability_gate.CAPABILITY_CEILING_KEY` -- the
/// `runtime_config` key holding the run's capability ceiling.
pub const CAPABILITY_CEILING_KEY: &str = "_capability_ceiling";

/// `circuitry.capability_gate.CapabilityConsentError` -- a document or
/// generated plan needs a capability nobody consented to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityConsentError {
    pub message: String,
}

impl fmt::Display for CapabilityConsentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for CapabilityConsentError {}

/// `circuitry.capability_gate.capability_ceiling` -- the installed
/// ceiling, or `None` when this run has none (no `_capability_ceiling`
/// key, or one that isn't a list).
pub fn capability_ceiling(runtime_config: &Value) -> Option<BTreeSet<String>> {
    let entry = runtime_config
        .as_dict()?
        .get(&Value::Str(CAPABILITY_CEILING_KEY.to_string()))?;
    match entry {
        Value::List(items) => Some(
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect(),
        ),
        _ => None,
    }
}

/// `circuitry.capability_gate.capabilities_beyond_ceiling` --
/// *required* capabilities this run's ceiling (if any) does not cover;
/// empty when there is no ceiling installed at all (unrestricted).
pub fn capabilities_beyond_ceiling(
    required: &BTreeSet<String>,
    runtime_config: &Value,
) -> BTreeSet<String> {
    match capability_ceiling(runtime_config) {
        None => BTreeSet::new(),
        Some(ceiling) => required.difference(&ceiling).cloned().collect(),
    }
}

/// `circuitry.capability_gate.require_within_ceiling` -- signature
/// only; lane R/L2 fills the body (the exact message text:
/// `"{label} needs {beyond}, beyond this document's consented
/// capabilities ({ceiling})."`, Python's own sorted, comma-joined
/// lists).
pub fn require_within_ceiling(
    label: &str,
    _required: &BTreeSet<String>,
    _runtime_config: &Value,
) -> Result<(), CapabilityConsentError> {
    Err(CapabilityConsentError {
        message: format!(
            "{label}: capability-ceiling enforcement is not supported by this build of \
             electricity yet (M1-R/M1-L2)"
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::Dict;

    fn runtime_config_with_ceiling(names: &[&str]) -> Value {
        let mut dict = Dict::new();
        dict.insert(
            Value::Str(CAPABILITY_CEILING_KEY.to_string()),
            Value::List(names.iter().map(|n| Value::Str(n.to_string())).collect()),
        );
        Value::Dict(dict)
    }

    #[test]
    fn no_ceiling_key_means_unrestricted() {
        assert_eq!(capability_ceiling(&Value::Dict(Dict::new())), None);
    }

    #[test]
    fn capabilities_beyond_ceiling_is_empty_when_unrestricted() {
        let required: BTreeSet<String> = ["shell", "http"].iter().map(|s| s.to_string()).collect();
        assert_eq!(
            capabilities_beyond_ceiling(&required, &Value::Dict(Dict::new())),
            BTreeSet::new()
        );
    }

    #[test]
    fn capabilities_beyond_ceiling_finds_what_the_ceiling_does_not_cover() {
        let runtime_config = runtime_config_with_ceiling(&["shell"]);
        let required: BTreeSet<String> = ["shell", "http"].iter().map(|s| s.to_string()).collect();
        let beyond = capabilities_beyond_ceiling(&required, &runtime_config);
        assert_eq!(beyond, BTreeSet::from(["http".to_string()]));
    }

    #[test]
    fn a_ceiling_covering_everything_required_finds_nothing_beyond_it() {
        let runtime_config = runtime_config_with_ceiling(&["shell", "http"]);
        let required: BTreeSet<String> = ["shell"].iter().map(|s| s.to_string()).collect();
        assert!(capabilities_beyond_ceiling(&required, &runtime_config).is_empty());
    }
}
