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

/// The golden section's own larger share of `total` --
/// `total - golden_section(total)`, so the two always add back up to
/// `total` exactly.
pub fn golden_major_section(total: u16) -> u16 {
    total - golden_section(total)
}

/// The left column's own width (issue #613: the larger share goes to
/// the pane that grows with the run -- the orchestration tree, in the
/// left column, below the details pane -- so the left column itself
/// takes the golden section's *larger* share here, floored at
/// [`LEFT_MIN`]). The right column takes `total -
/// left_column_width(total)`.
pub fn left_column_width(total: u16) -> u16 {
    golden_major_section(total).max(LEFT_MIN).min(total)
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
    fn golden_major_section_always_completes_the_minor_one() {
        for width in [80, 100, 120, 160, 200, 300] {
            assert_eq!(golden_major_section(width) + golden_section(width), width);
        }
    }

    #[test]
    fn left_column_matches_the_golden_majority_above_the_minimum() {
        // Issue #613: the left column (details above the
        // orchestration tree, the pane that grows with the run) gets
        // the golden section's *larger* share, not its smaller one.
        for width in [80, 100, 120, 160, 200, 300] {
            assert_eq!(left_column_width(width), golden_major_section(width));
            assert!(left_column_width(width) > width - left_column_width(width));
        }
    }

    #[test]
    fn left_column_never_drops_below_left_min() {
        // The golden majority of 80 already clears LEFT_MIN (49 > 30);
        // this pins where the floor would bite below that, were the
        // two-column threshold ever lowered.
        assert_eq!(left_column_width(40), LEFT_MIN);
        assert_eq!(golden_major_section(40), 25);
    }

    #[test]
    fn left_column_never_exceeds_total() {
        assert_eq!(left_column_width(10), 10);
    }
}
