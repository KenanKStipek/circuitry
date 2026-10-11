//! `curl_support.py::curl_failure_message`, ported -- signature only
//! (review P2-5 on PR #454's second review, extending M1 gate seam
//! round 2's item S9 to this file too); lane F2 fills the body (the
//! secret-masking/URL-userinfo-stripping Python's own version does
//! before a failure message is ever built, not just the `(curl exit
//! N)` shape).

/// `curl_support.py::curl_failure_message` -- *source* is the adapter's
/// own name, *model* the model being called (if any), *url* the
/// request URL, *returncode*/*stdout*/*stderr* curl's own result,
/// *hint* an adapter-specific suggestion appended verbatim, *secrets*
/// every raw credential value this request sent (masked out of the
/// rendered message before it is ever returned). Signature only; lane
/// F2 fills the body -- returning the raw, unmasked *stderr*/*stdout*
/// here would leak a credential, so this stub deliberately does not
/// even approximate the real shape.
#[allow(clippy::too_many_arguments)]
pub fn curl_failure_message(
    source: &str,
    _model: Option<&str>,
    _url: &str,
    returncode: i32,
    _stdout: &str,
    _stderr: &str,
    _hint: &str,
    _secrets: &[&str],
) -> String {
    format!("{source}: curl failure text is not implemented yet (lane F2, exit {returncode})")
}
