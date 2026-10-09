//! electricity-redaction: credential-shaped-value redaction, and the
//! `meta.raw` size cap (issue #431's Scope section, lane C). A **lane A
//! stub**: [`redact`] is the final signature every later lane calls, but
//! its body is a documented pass-through, not yet `cli/redaction.py`'s
//! real deny-list -- a tool's `meta.raw`/`meta.params_rendered` is
//! unredacted until lane C lands, which is unsafe for a real run (never
//! treat this crate's current behaviour as the security boundary
//! `cli/redaction.py`'s own module docs describe -- it is exactly the
//! documented absence of one). [`cap_raw`] is a real, self-contained
//! port: the 64 KiB `meta.raw` truncation rule has no redaction logic of
//! its own to stub out.

use electricity_value::Value;

/// `cli/redaction.py::REDACTED` -- the literal electricity-redaction's
/// real implementation (lane C) replaces a sensitive value with.
pub const REDACTED: &str = "***REDACTED***";

/// `core/tool.py::_RAW_META_MAX_BYTES` -- the cap on a tool's `meta.raw`,
/// serialized, after redaction.
pub const RAW_META_MAX_BYTES: usize = 64 * 1024;

/// `cli/redaction.py::redact` -- walks *value* for a sensitive key name
/// (`api_key`, `password`, `authorization`, ...), a userinfo-bearing URL,
/// or a JWT/API-key-shaped string, replacing each with [`REDACTED`] (URL
/// userinfo) or a wholesale [`REDACTED`] string (everything else).
///
/// Lane A stub: returns *value* unchanged. Every later lane that calls
/// this (the `--out`/`--live-state`/`--events` writers, `runtime.
/// effective_settings`, a tool's `meta.raw`/`meta.params_rendered`) gets
/// the unredacted value until lane C lands -- a document containing a
/// real credential must not be run against this preview release for
/// that reason.
pub fn redact(value: Value) -> Value {
    value
}

/// `core/tool.py::_capped_raw`'s own truncation-marker shape: `{
/// "_truncated": true, "_original_bytes": N, "_preview": "..." }`,
/// substituted for `meta.raw` wholesale once its redacted, JSON-encoded
/// form exceeds [`RAW_META_MAX_BYTES`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawCapMarker {
    pub original_bytes: usize,
    /// *encoded_redacted_raw*'s own first [`RAW_META_MAX_BYTES`] bytes,
    /// lossily decoded as UTF-8 (`bytes.decode("utf-8", errors="ignore")`
    /// -- a multi-byte codepoint split by the cut keeps only its valid
    /// prefix, dropping the rest, exactly as Python's `errors="ignore"`
    /// does for a trailing partial sequence).
    pub preview: String,
}

/// `len(encoded) <= _RAW_META_MAX_BYTES`'s own two-way branch: `None`
/// when *encoded_redacted_raw* (a tool's already-[`redact`]ed `raw`,
/// JSON-encoded -- `json.dumps(redacted, ensure_ascii=False,
/// default=str).encode("utf-8")`) already fits; `Some` with the
/// [`RawCapMarker`] a caller substitutes for the whole `meta.raw` value
/// otherwise. A real, self-contained port (lane C's own redaction has
/// already run by the time a caller reaches this) -- not a stub.
pub fn cap_raw(encoded_redacted_raw: &[u8]) -> Option<RawCapMarker> {
    if encoded_redacted_raw.len() <= RAW_META_MAX_BYTES {
        return None;
    }
    let preview = String::from_utf8_lossy(&encoded_redacted_raw[..RAW_META_MAX_BYTES]).into_owned();
    Some(RawCapMarker {
        original_bytes: encoded_redacted_raw.len(),
        preview,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_is_a_pass_through_stub() {
        let value = Value::Str("super-secret-api-key".to_string());
        assert_eq!(redact(value.clone()), value);
    }

    #[test]
    fn cap_raw_is_none_at_or_under_the_limit() {
        let encoded = vec![b'a'; RAW_META_MAX_BYTES];
        assert_eq!(cap_raw(&encoded), None);
    }

    #[test]
    fn cap_raw_substitutes_a_marker_once_over_the_limit() {
        let encoded = vec![b'a'; RAW_META_MAX_BYTES + 1];
        let marker = cap_raw(&encoded).unwrap();
        assert_eq!(marker.original_bytes, RAW_META_MAX_BYTES + 1);
        assert_eq!(marker.preview.len(), RAW_META_MAX_BYTES);
    }
}
