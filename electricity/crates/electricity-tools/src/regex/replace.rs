//! `re.sub`'s own substitution semantics (backreferences, `count`) --
//! signature only (M1 gate seam round 2, item S9); lane D2 fills the
//! body.

/// `re.sub(pattern, repl, string, count=count)` -- signature only;
/// lane D2 fills the body.
pub fn replace(
    _pattern: &str,
    _repl: &str,
    _text: &str,
    _count: Option<u32>,
) -> Result<String, String> {
    Err("regex replace is not implemented yet (lane D2)".to_string())
}
