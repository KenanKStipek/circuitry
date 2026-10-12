//! Generated, checked-in source -- never hand-edit a file under here;
//! re-run its own `electricity/scripts/generate_*.py` instead.

pub(crate) mod python_codec_names;
mod unicode_alnum_ranges;

/// Whether `c` is alphanumeric per CPython's `str.isalnum()` -- true for
/// `isalpha()` (Unicode category Lu/Ll/Lt/Lm/Lo), `isdecimal()`/
/// `isdigit()` (Nd, plus a few extra digit-valued characters), or
/// `isnumeric()` (Nl/No) -- not Rust's own, broader `char::
/// is_alphanumeric()` (the derived `Alphabetic`/`Numeric` properties;
/// see the generator's own doc comment for the exact, confirmed gap).
pub(crate) fn is_py_alnum(c: char) -> bool {
    let cp = c as u32;
    unicode_alnum_ranges::ALNUM_RANGES
        .binary_search_by(|&(lo, hi)| {
            if cp < lo {
                std::cmp::Ordering::Greater
            } else if cp > hi {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_alphanumerics_are_alnum() {
        for c in ['a', 'Z', '0', '9'] {
            assert!(is_py_alnum(c));
        }
    }

    #[test]
    fn ascii_punctuation_is_not_alnum() {
        for c in ['-', '_', ' ', ';', '.'] {
            assert!(!is_py_alnum(c));
        }
    }

    #[test]
    fn a_combining_mark_with_only_the_derived_alphabetic_property_is_not_alnum() {
        // U+0345 COMBINING GREEK YPOGEGRAMMENI: Rust's own
        // `char::is_alphanumeric()` says `true` (derived
        // `Other_Alphabetic`); CPython's `isalnum()` says `false`
        // (category Mn, not a Letter category) -- the exact gap the
        // generator's own doc comment names.
        assert!(!is_py_alnum('\u{345}'));
        assert!('\u{345}'.is_alphanumeric());
    }
}
