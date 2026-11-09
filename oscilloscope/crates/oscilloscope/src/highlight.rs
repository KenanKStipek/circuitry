//! Update tracking (DESIGN.md §6.3, issue #535): [`Tracker`] stores
//! *when* each stable key last changed -- what counts as a change at
//! all is `oscilloscope-core`'s own job ([`oscilloscope_core::
//! changes`]), kept front-end independent per DESIGN.md §6.1. The
//! redraw loop keeps redrawing every tick while any key is still
//! within [`FADE_DURATION`] of its own mark (`has_active`), the same
//! pacing a visible fade would need if a screen ever drew one;
//! today no screen does, so this module is the tracking half of
//! that feature with no rendering side left to call it.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// About 1.2 seconds (issue #535's own "fades out in about a
/// second"/"fades to nothing over about 1.2s").
pub const FADE_DURATION: Duration = Duration::from_millis(1200);

/// Tracks the most recent trigger time per stable key (a row path, a
/// state-pane path, a treemap key path, a log line's sequence
/// number, a token-bar field, a header field — whatever `changes::
/// diff` reports as the key, turned into a `String` by the caller).
/// A retriggered key restarts its own fade; fades never stack, since
/// each key has exactly one trigger time, not a queue of them.
#[derive(Debug, Default)]
pub struct Tracker {
    triggered_at: HashMap<String, Instant>,
}

impl Tracker {
    pub fn new() -> Tracker {
        Tracker::default()
    }

    pub fn mark(&mut self, key: &str, now: Instant) {
        self.triggered_at.insert(key.to_string(), now);
    }

    /// Drops every key whose fade has already ended — keeps the map
    /// from growing for the life of a long run.
    pub fn prune(&mut self, now: Instant) {
        self.triggered_at
            .retain(|_, &mut t| now.saturating_duration_since(t) < FADE_DURATION);
    }

    /// `true` while any key is still fading -- test-only, since no
    /// screen draws a fade from this any more ([`Tracker`]'s own doc
    /// comment), and the redraw loop no longer uses this to force an
    /// extra tick either (a fade nothing draws is not a reason to
    /// redraw).
    #[cfg(test)]
    pub(crate) fn has_active(&self, now: Instant) -> bool {
        self.triggered_at
            .values()
            .any(|&t| now.saturating_duration_since(t) < FADE_DURATION)
    }

    /// Whether `key` specifically is still within its own fade,
    /// never marked at all, or already past it -- test-only, since
    /// no screen draws a fade from this any more ([`Tracker`]'s own
    /// doc comment).
    #[cfg(test)]
    pub(crate) fn is_active(&self, key: &str, now: Instant) -> bool {
        self.triggered_at
            .get(key)
            .is_some_and(|&t| now.saturating_duration_since(t) < FADE_DURATION)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_retrigger_restarts_the_fade_rather_than_stacking() {
        let base = Instant::now();
        let mut tracker = Tracker::new();
        tracker.mark("row:a", base);
        let mid = base + Duration::from_millis(900);
        assert!(!tracker.has_active(mid + FADE_DURATION - Duration::from_millis(900)));
        tracker.mark("row:a", mid);
        // Immediately after a retrigger, the fade is active again as
        // if just marked -- not "already ending", which a stacked
        // fade would read as.
        assert!(tracker.has_active(mid));
        assert!(tracker.has_active(mid + FADE_DURATION - Duration::from_millis(1)));
    }

    #[test]
    fn prune_drops_only_fully_ended_keys() {
        let base = Instant::now();
        let mut tracker = Tracker::new();
        tracker.mark("row:a", base);
        tracker.mark("row:b", base - Duration::from_secs(10));
        tracker.prune(base);
        assert!(tracker.triggered_at.contains_key("row:a"));
        assert!(!tracker.triggered_at.contains_key("row:b"));
    }

    #[test]
    fn has_active_reports_nothing_once_every_fade_has_ended() {
        let base = Instant::now();
        let mut tracker = Tracker::new();
        tracker.mark("row:a", base);
        assert!(tracker.has_active(base));
        assert!(!tracker.has_active(base + FADE_DURATION + Duration::from_millis(1)));
    }
}
