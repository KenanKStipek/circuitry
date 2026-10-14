//! Update highlights (DESIGN.md §6.3, issue #535): a soft background
//! tint on whatever just changed, fading to nothing over about 1.2
//! seconds. [`Tracker`] only ever stores *when* each stable key last
//! changed — what counts as a change at all is `oscilloscope-core`'s
//! own job ([`oscilloscope_core::changes`]), kept front-end
//! independent per DESIGN.md §6.1. The fade's own timing, easing and
//! colour-mode styling live here, in the terminal-facing crate, and
//! every clock reading this module ever uses is passed in — never
//! `Instant::now()` — so tests (and `--no-animation`) are
//! deterministic.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ratatui::style::{Color, Modifier, Style};

use crate::theme::{ColorMode, Theme, ThemeName, nearest_xterm256};

/// About 1.2 seconds (issue #535's own "fades out in about a
/// second"/"fades to nothing over about 1.2s").
pub const FADE_DURATION: Duration = Duration::from_millis(1200);

/// The 16-colour/`NO_COLOR` case's own bold window (DESIGN.md §6.3:
/// "bold for the first ~0.5s, no colour change").
const BOLD_WINDOW: Duration = Duration::from_millis(500);

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

    /// `true` while any key is still fading — the redraw loop marks
    /// the frame dirty every tick exactly while this holds (issue
    /// #535's own redraw rule), so the existing `REDRAW_INTERVAL` cap
    /// paces the fade instead of a busy loop.
    pub fn has_active(&self, now: Instant) -> bool {
        self.triggered_at
            .values()
            .any(|&t| now.saturating_duration_since(t) < FADE_DURATION)
    }

    /// Linear progress through the fade, `0.0` the instant a key was
    /// marked, `1.0` (and beyond) once [`FADE_DURATION`] has passed.
    /// `None` for a key that was never marked, or whose fade has
    /// fully ended — the caller's "no override" case.
    fn linear_progress(&self, key: &str, now: Instant) -> Option<f64> {
        let t = *self.triggered_at.get(key)?;
        let elapsed = now.saturating_duration_since(t);
        if elapsed >= FADE_DURATION {
            None
        } else {
            Some(elapsed.as_secs_f64() / FADE_DURATION.as_secs_f64())
        }
    }

    /// The style override for `key` right now, under `theme`/`mode`
    /// — `None` means "draw this cell with no highlight at all",
    /// which every caller must treat as leaving its own normal style
    /// untouched (never resetting it to a blank one).
    pub fn style_for(
        &self,
        key: &str,
        now: Instant,
        theme: &Theme,
        mode: ColorMode,
    ) -> Option<Style> {
        let progress = self.linear_progress(key, now)?;
        style_for_progress(progress, theme, mode)
    }
}

/// Ease-out (CSS's own `ease-out` shape: fast at the start, slow
/// approaching the end) — issue #535's own "fades out... easing out".
fn ease_out(p: f64) -> f64 {
    let p = p.clamp(0.0, 1.0);
    1.0 - (1.0 - p) * (1.0 - p)
}

fn mix(a: (u8, u8, u8), b: (u8, u8, u8), t: f64) -> (u8, u8, u8) {
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: u8, y: u8| -> u8 { (x as f64 + (y as f64 - x as f64) * t).round() as u8 };
    (lerp(a.0, b.0), lerp(a.1, b.1), lerp(a.2, b.2))
}

fn as_rgb(color: Color) -> Option<(u8, u8, u8)> {
    match color {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        _ => None,
    }
}

/// `theme`'s own role values, except always the *undowngraded* RGB
/// (`Theme::for_name`'s own output) for the two colours the fade
/// mixes between — `theme` itself may already be `resolve`'s
/// *displayed* theme (indexed, or `NO_COLOR`'s own all-`Reset` one),
/// which has nothing left to mix once it's been downgraded.
type Rgb = (u8, u8, u8);

fn named_theme_fade_colors(theme: &Theme) -> Option<(Rgb, Rgb)> {
    if theme.name == ThemeName::Terminal {
        return None;
    }
    let fresh = Theme::for_name(theme.name);
    let tint = as_rgb(fresh.highlight)?;
    let bg = as_rgb(fresh.background?)?;
    Some((tint, bg))
}

/// Bold-only, for `NO_COLOR` and a 16-colour terminal alike (DESIGN.md
/// §6.3): "modifiers only, never colours" and "bold for the first
/// ~0.5s, no colour change" are the same shape, so one helper covers
/// both call sites.
fn bold_only_style(progress: f64) -> Option<Style> {
    let elapsed = Duration::from_secs_f64(progress * FADE_DURATION.as_secs_f64());
    if elapsed < BOLD_WINDOW {
        Some(Style::default().add_modifier(Modifier::BOLD))
    } else {
        None
    }
}

/// `terminal`'s own fixed step colours (issue #535): since `terminal`
/// paints no background of its own, there is no destination colour
/// to fade *toward* -- a low-chroma slate blue (HSV ~sat 0.25, close
/// to `theme::Theme::dracula`'s own `dim`), subtle enough to read as
/// a highlight rather than an alert on a dark terminal and still
/// legible under dark text on a light one. Each step desaturates
/// this tint toward [`TERMINAL_RAMP_NEUTRAL`] -- a mid grey at
/// roughly the same perceived lightness, never black -- with the
/// final step being "no override at all" (the RGB ramp's own "ending
/// in no override"). An earlier version of this ramp mixed toward
/// black and used a saturated yellow tint, which read as a bright
/// flash rather than a subtle one and lost contrast at both ends.
const TERMINAL_RAMP_TINT: (u8, u8, u8) = (92, 108, 122);
/// The ramp's own fade target (see [`TERMINAL_RAMP_TINT`]): a neutral
/// grey at approximately the tint's own lightness, so the fade only
/// desaturates rather than darkening toward black.
const TERMINAL_RAMP_NEUTRAL: (u8, u8, u8) = (107, 107, 107);

fn step_fraction(progress: f64, steps: usize) -> Option<f64> {
    let eased = ease_out(progress);
    let step = ((eased * steps as f64).floor() as usize).min(steps - 1);
    let fraction = 1.0 - (step as f64 / steps as f64);
    if step == steps - 1 {
        None
    } else {
        Some(fraction)
    }
}

fn style_for_progress(progress: f64, theme: &Theme, mode: ColorMode) -> Option<Style> {
    match mode {
        ColorMode::NoColor | ColorMode::Basic16 => bold_only_style(progress),
        ColorMode::TrueColor => {
            if let Some((tint, bg)) = named_theme_fade_colors(theme) {
                let eased = ease_out(progress);
                let (r, g, b) = mix(tint, bg, eased);
                Some(Style::default().bg(Color::Rgb(r, g, b)))
            } else {
                let fraction = step_fraction(progress, 6)?;
                let (r, g, b) = mix(TERMINAL_RAMP_NEUTRAL, TERMINAL_RAMP_TINT, fraction);
                Some(Style::default().bg(Color::Rgb(r, g, b)))
            }
        }
        ColorMode::Indexed256 => {
            if let Some((tint, bg)) = named_theme_fade_colors(theme) {
                let eased = ease_out(progress);
                let (r, g, b) = mix(tint, bg, eased);
                Some(Style::default().bg(Color::Indexed(nearest_xterm256(r, g, b))))
            } else {
                let fraction = step_fraction(progress, 3)?;
                let (r, g, b) = mix(TERMINAL_RAMP_NEUTRAL, TERMINAL_RAMP_TINT, fraction);
                Some(Style::default().bg(Color::Indexed(nearest_xterm256(r, g, b))))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::resolve;

    fn at(ms: u64) -> Instant {
        // A fixed anchor, well clear of process-start overflow —
        // `Instant` has no public constructor, so tests always derive
        // every timestamp from one `Instant::now()` call plus an
        // offset, never comparing across two separate `now()` calls.
        Instant::now() + Duration::from_millis(ms)
    }

    #[test]
    fn a_retrigger_restarts_the_fade_rather_than_stacking() {
        let base = Instant::now();
        let mut tracker = Tracker::new();
        tracker.mark("row:a", base);
        let mid = base + Duration::from_millis(900);
        assert!(tracker.linear_progress("row:a", mid).unwrap() > 0.5);
        tracker.mark("row:a", mid);
        // Immediately after a retrigger, progress is back near zero —
        // not "0.9 fade plus a fresh one", just one fade.
        assert!(tracker.linear_progress("row:a", mid).unwrap() < 0.01);
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

    #[test]
    fn style_for_is_none_before_any_mark_and_after_the_fade_ends() {
        let base = at(0);
        let mut tracker = Tracker::new();
        let theme = resolve(ThemeName::Dracula, ColorMode::TrueColor).theme;
        assert!(
            tracker
                .style_for("row:a", base, &theme, ColorMode::TrueColor)
                .is_none()
        );
        tracker.mark("row:a", base);
        assert!(
            tracker
                .style_for(
                    "row:a",
                    base + FADE_DURATION + Duration::from_millis(1),
                    &theme,
                    ColorMode::TrueColor
                )
                .is_none()
        );
    }

    #[test]
    fn truecolor_named_theme_starts_at_the_tint_and_ends_at_the_background() {
        let base = at(0);
        let mut tracker = Tracker::new();
        let theme = resolve(ThemeName::Dracula, ColorMode::TrueColor).theme;
        tracker.mark("row:a", base);
        let style_t0 = tracker
            .style_for("row:a", base, &theme, ColorMode::TrueColor)
            .unwrap();
        assert_eq!(style_t0.bg, Some(Color::Rgb(0x44, 0x47, 0x5a)));

        let near_end = base + FADE_DURATION - Duration::from_millis(1);
        let style_end = tracker
            .style_for("row:a", near_end, &theme, ColorMode::TrueColor)
            .unwrap();
        match style_end.bg {
            Some(Color::Rgb(r, g, b)) => {
                // Close to the theme's own background (#282a36), not
                // the tint, this close to the fade's own end.
                assert!(r.abs_diff(0x28) <= 2 && g.abs_diff(0x2a) <= 2 && b.abs_diff(0x36) <= 2);
            }
            other => panic!("expected an RGB background override, got {other:?}"),
        }
    }

    /// Finding 15: an explicit mid-fade point for truecolour, not
    /// just t=0 and near the end -- the tint's own own red channel
    /// (0x44) and the background's (0x28) bracket a real midpoint,
    /// never equal to either endpoint.
    #[test]
    fn truecolor_named_theme_is_between_tint_and_background_mid_fade() {
        let base = at(0);
        let mut tracker = Tracker::new();
        let theme = resolve(ThemeName::Dracula, ColorMode::TrueColor).theme;
        tracker.mark("row:a", base);
        let mid = base + FADE_DURATION / 2;
        let style = tracker
            .style_for("row:a", mid, &theme, ColorMode::TrueColor)
            .unwrap();
        match style.bg {
            Some(Color::Rgb(r, _, _)) => {
                assert!(
                    (0x28..=0x44).contains(&r),
                    "{r:#x} should sit between the background and the tint"
                );
                assert_ne!(r, 0x44, "mid-fade must not still read as t=0");
                assert_ne!(r, 0x28, "mid-fade must not already read as the end");
            }
            other => panic!("expected an RGB background override, got {other:?}"),
        }
    }

    /// Finding 15: an explicit mid-fade point for the 256-colour
    /// ramp's own named-theme path.
    #[test]
    fn indexed_256_named_theme_gives_an_indexed_color_mid_fade() {
        let base = at(0);
        let mut tracker = Tracker::new();
        let theme = resolve(ThemeName::Dracula, ColorMode::Indexed256).theme;
        tracker.mark("row:a", base);
        let mid = base + FADE_DURATION / 2;
        let style = tracker
            .style_for("row:a", mid, &theme, ColorMode::Indexed256)
            .unwrap();
        assert!(matches!(style.bg, Some(Color::Indexed(_))), "{style:?}");
    }

    #[test]
    fn truecolor_terminal_theme_ends_with_no_override() {
        let base = at(0);
        let mut tracker = Tracker::new();
        let theme = resolve(ThemeName::Terminal, ColorMode::TrueColor).theme;
        tracker.mark("row:a", base);
        assert!(
            tracker
                .style_for("row:a", base, &theme, ColorMode::TrueColor)
                .is_some()
        );
        let last_step_start = base + Duration::from_millis(1100);
        assert!(
            tracker
                .style_for("row:a", last_step_start, &theme, ColorMode::TrueColor)
                .is_none(),
            "the last of the six steps must be 'no override', not a colour"
        );
    }

    #[test]
    fn indexed_256_gives_a_three_step_ramp_for_the_terminal_theme() {
        let base = at(0);
        let mut tracker = Tracker::new();
        let theme = resolve(ThemeName::Terminal, ColorMode::Indexed256).theme;
        tracker.mark("row:a", base);
        let first = tracker
            .style_for("row:a", base, &theme, ColorMode::Indexed256)
            .unwrap();
        assert!(matches!(first.bg, Some(Color::Indexed(_))));
        let last_step_start = base + Duration::from_millis(900);
        assert!(
            tracker
                .style_for("row:a", last_step_start, &theme, ColorMode::Indexed256)
                .is_none()
        );
    }

    #[test]
    fn basic_16_is_bold_for_about_half_a_second_then_nothing() {
        let base = at(0);
        let mut tracker = Tracker::new();
        let theme = resolve(ThemeName::Terminal, ColorMode::Basic16).theme;
        tracker.mark("row:a", base);
        let style = tracker
            .style_for(
                "row:a",
                base + Duration::from_millis(100),
                &theme,
                ColorMode::Basic16,
            )
            .unwrap();
        assert_eq!(style.bg, None);
        assert!(style.add_modifier.contains(Modifier::BOLD));
        assert!(
            tracker
                .style_for(
                    "row:a",
                    base + Duration::from_millis(600),
                    &theme,
                    ColorMode::Basic16
                )
                .is_none()
        );
    }

    #[test]
    fn no_color_never_sets_a_background_at_any_point_in_the_fade() {
        let base = at(0);
        let mut tracker = Tracker::new();
        let theme = resolve(ThemeName::Dracula, ColorMode::NoColor).theme;
        tracker.mark("row:a", base);
        for ms in [0, 100, 400, 800, 1100] {
            if let Some(style) = tracker.style_for(
                "row:a",
                base + Duration::from_millis(ms),
                &theme,
                ColorMode::NoColor,
            ) {
                assert_eq!(style.bg, None);
                assert_eq!(style.fg, None);
            }
        }
    }
}
