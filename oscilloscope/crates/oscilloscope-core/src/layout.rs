//! Golden-section column/row splits (DESIGN.md §6.3, issue #535).
//! Pure integer geometry, same spirit as `treemap::layout`: a front
//! end only ever calls these to decide column/row widths, never
//! recomputes the ratio itself.

/// φ² = φ + 1 (the golden ratio's own identity), used directly so
/// callers never need `f64::sqrt` of their own.
const PHI_SQUARED: f64 = 2.618_033_988_749_895;

/// The left column's own minimum width (issue #535): the golden
/// section already clears this at the two-column threshold (80
/// columns gives 31), so this only guards a later change to that
/// threshold.
pub const LEFT_MIN: u16 = 30;

/// `round(total / φ²)`, with no minimum applied — the golden
/// section's own smaller share of `total`. Used for both the
/// left/right column split and the details/orchestration row split
/// inside the left column (the same ratio, just applied to rows
/// instead of columns).
pub fn golden_section(total: u16) -> u16 {
    ((total as f64) / PHI_SQUARED).round() as u16
}

/// The left column's own width (issue #535): the golden section,
/// floored at [`LEFT_MIN`]. The right column takes `total -
/// left_column_width(total)`.
pub fn left_column_width(total: u16) -> u16 {
    golden_section(total).max(LEFT_MIN).min(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn golden_section_at_the_listed_widths() {
        assert_eq!(golden_section(80), 31);
        assert_eq!(golden_section(100), 38);
        assert_eq!(golden_section(120), 46);
        assert_eq!(golden_section(160), 61);
        assert_eq!(golden_section(200), 76);
        assert_eq!(golden_section(300), 115);
    }

    #[test]
    fn left_column_matches_the_golden_section_above_the_minimum() {
        for width in [80, 100, 120, 160, 200, 300] {
            assert_eq!(left_column_width(width), golden_section(width));
        }
    }

    #[test]
    fn left_column_never_drops_below_left_min() {
        // The golden section of 80 already clears LEFT_MIN (31 > 30);
        // this pins the floor would bite below that, were the
        // two-column threshold ever lowered.
        assert_eq!(left_column_width(60), LEFT_MIN);
        assert_eq!(golden_section(60), 23);
    }

    #[test]
    fn left_column_never_exceeds_total() {
        assert_eq!(left_column_width(10), 10);
    }
}
