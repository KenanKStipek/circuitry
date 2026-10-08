//! Lane B: `structural_errors`/`unknown_key_warnings`, porting
//! `core/document_check.py`'s structural pass: near-miss unknown keys
//! ([`crate::difflib`]), JSON Schema through [`crate::schema_instance`],
//! `group:` placement, and `interface.inputs` checks.

use crate::not_implemented;
use electricity_value::Value;

/// Every structural error in *document*, in Circuitry's own order.
///
/// Stub (lane A): always reports one error until lane B lands.
pub fn structural_errors(document: &Value) -> Vec<String> {
    let _ = document;
    vec![not_implemented("structural_errors", "B")]
}

/// Unknown-key warnings, from the same walk `structural_errors` does.
///
/// Stub (lane A): always reports one warning until lane B lands.
pub fn unknown_key_warnings(document: &Value) -> Vec<String> {
    let _ = document;
    vec![not_implemented("unknown_key_warnings", "B")]
}
