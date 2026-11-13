//! The run's own story (issue #691, replacing #635's tree-shaped
//! Flow model): a short log of the run's own notable events, oldest
//! first, built only while screen `2` is actually on display, the
//! same performance rule `screens::build_cost_screen` already
//! follows for screen `3`. The tree this module used to also build
//! (every row indented by its own depth, a decision's own branch,
//! a loop's own passes) is `DESIGN.md` §6.3's pre-#691 Flow,
//! replaced by the rail (`crate::rail`); it is documented there as
//! replaced, not maintained here any further.

use crate::diff::{LogKind, LogLine};
use crate::format::{ClockZone, format_clock_time};

#[derive(Debug, Clone, PartialEq)]
pub struct StoryEntry {
    /// Clock-prefixed and ready to draw (`HH:MM:SS <text>`) -- a real
    /// line reuses `--log`'s own text verbatim (`diff::LogLine::
    /// text`); an inferred decision's own text is built here instead,
    /// since it has no `LogLine` of its own (DESIGN.md: "join the
    /// story but never `--log`").
    pub text: String,
    pub inferred: bool,
}

/// An unnamed `if`'s own inferred decision, read off the rail
/// (`rail::build_rail`'s own synthetic `.__if` station) rather than
/// from any `LogLine` -- an unnamed `if` writes no node and fires no
/// event (DESIGN.md §1.3), so it has nothing else to join the story
/// with.
#[derive(Debug, Clone, PartialEq)]
pub struct InferredDecision {
    pub label: String,
    pub branch: Option<String>,
    pub start_ts: Option<String>,
}

/// `text` (always `"◆ <path> → <branch>"`, `diff`'s own real-decision
/// shape), with its own leading path cut back to its last segment --
/// the rail's own ten-cell stations already read a decision by its
/// bare name alone, and the story reads the same way next to them.
fn shorten_decision_path(text: &str) -> String {
    let Some(rest) = text.strip_prefix("\u{25c6} ") else {
        return text.to_string();
    };
    let Some((path, tail)) = rest.split_once(' ') else {
        return text.to_string();
    };
    let short = path.rsplit('.').next().unwrap_or(path);
    format!("\u{25c6} {short} {tail}")
}

/// The run's own story (DESIGN.md: "the run's notable events, oldest
/// first, with clock times"): `story_lines` is the caller's own
/// bounded buffer of real `--log` lines, raw `ts`/`text` (never
/// pre-formatted -- `LogKind` already filtered it to the story's own
/// kinds: run start/end, decision, report, failure, same as `--log`
/// otherwise is); every unnamed `if`'s own inferred decision joins
/// in too, with the clock time of whichever branch it read as taken.
/// Sorted by each entry's own real instant (`diff::parse_instant`,
/// the same RFC-3339 parser `sort_log_lines` uses), never a plain
/// string compare -- an `--events` line's own millisecond timestamp
/// and a state-sourced one's microsecond one aren't lexicographically
/// comparable at all, and a `None` (an inferred decision whose own
/// branch hasn't resolved a start time yet) sorts last rather than
/// first.
pub fn build_story(
    story_lines: &[LogLine],
    inferred: &[InferredDecision],
    zone: &dyn ClockZone,
) -> Vec<StoryEntry> {
    let mut keyed: Vec<(Option<String>, StoryEntry)> = story_lines
        .iter()
        .map(|l| {
            let clock = l.ts.as_deref().and_then(|ts| format_clock_time(ts, zone));
            let body = if l.kind == LogKind::Decision {
                shorten_decision_path(&l.text)
            } else {
                l.text.clone()
            };
            let text = match &clock {
                Some(clock) => format!("{clock} {body}"),
                None => body,
            };
            (
                l.ts.clone(),
                StoryEntry {
                    text,
                    inferred: false,
                },
            )
        })
        .collect();
    for decision in inferred {
        let Some(branch) = &decision.branch else {
            continue;
        };
        let clock = decision
            .start_ts
            .as_deref()
            .and_then(|ts| format_clock_time(ts, zone));
        let text = match &clock {
            Some(clock) => format!("{clock} \u{25c6} {} \u{2192} {branch}", decision.label),
            None => format!("\u{25c6} {} \u{2192} {branch}", decision.label),
        };
        keyed.push((
            decision.start_ts.clone(),
            StoryEntry {
                text,
                inferred: true,
            },
        ));
    }
    keyed.sort_by(|a, b| match (&a.0, &b.0) {
        (Some(x), Some(y)) => {
            match (crate::diff::parse_instant(x), crate::diff::parse_instant(y)) {
                (Some(ix), Some(iy)) => ix.cmp(&iy),
                _ => x.cmp(y),
            }
        }
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    });
    keyed.into_iter().map(|(_, entry)| entry).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedZone;
    impl crate::format::ClockZone for FixedZone {
        fn local(
            &self,
            utc: chrono::DateTime<chrono::Utc>,
        ) -> chrono::DateTime<chrono::FixedOffset> {
            utc.with_timezone(&chrono::FixedOffset::east_opt(0).unwrap())
        }
    }

    /// Issue #635: the story merges real log lines and inferred
    /// decisions oldest first, by clock time, including two lines
    /// landing in the same second (a real decision and a report
    /// arriving at the identical `ts`).
    #[test]
    fn build_story_orders_real_and_inferred_lines_oldest_first() {
        let real = vec![
            LogLine {
                ts: Some("2026-01-01T00:00:10Z".to_string()),
                text: "◆ prime.gate → then".to_string(),
                kind: crate::diff::LogKind::Decision,
                path: Some("prime.gate".to_string()),
            },
            LogLine {
                ts: Some("2026-01-01T00:00:10Z".to_string()),
                text: "▸ halfway".to_string(),
                kind: crate::diff::LogKind::Report,
                path: Some("prime.status".to_string()),
            },
            LogLine {
                ts: Some("2026-01-01T00:00:05Z".to_string()),
                text: "■ run started  abc".to_string(),
                kind: crate::diff::LogKind::RunStart,
                path: None,
            },
        ];
        let inferred = InferredDecision {
            label: "if".to_string(),
            branch: Some("then".to_string()),
            start_ts: Some("2026-01-01T00:00:07Z".to_string()),
        };
        let entries = build_story(&real, &[inferred], &FixedZone);
        let texts: Vec<&str> = entries.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "00:00:05 ■ run started  abc",
                "00:00:07 ◆ if → then",
                "00:00:10 ◆ gate → then",
                "00:00:10 ▸ halfway",
            ]
        );
        assert!(!entries[0].inferred);
        assert!(entries[1].inferred);
    }

    /// An `--events` line's own millisecond timestamp and a state-
    /// sourced one's microsecond one aren't lexicographically
    /// comparable at all -- a plain string sort would read the
    /// state line's `064+00:00` suffix as sorting after the event
    /// line's `Z`, even though the state line's own instant is
    /// earlier.
    #[test]
    fn build_story_sorts_by_real_instant_not_by_string() {
        let real = vec![
            LogLine {
                ts: Some("2026-01-01T00:00:01.500Z".to_string()),
                text: "▸ later".to_string(),
                kind: crate::diff::LogKind::Report,
                path: Some("prime.status".to_string()),
            },
            LogLine {
                ts: Some("2026-01-01T00:00:01.200064+00:00".to_string()),
                text: "▸ earlier".to_string(),
                kind: crate::diff::LogKind::Report,
                path: Some("prime.status".to_string()),
            },
        ];
        let entries = build_story(&real, &[], &FixedZone);
        let texts: Vec<&str> = entries.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(texts, vec!["00:00:01 ▸ earlier", "00:00:01 ▸ later"]);
    }

    /// An inferred decision with no branch resolved yet never joins
    /// the story at all -- there is nothing settled to report.
    #[test]
    fn an_undecided_inferred_decision_never_joins_the_story() {
        let inferred = InferredDecision {
            label: "if".to_string(),
            branch: None,
            start_ts: None,
        };
        let entries = build_story(&[], &[inferred], &FixedZone);
        assert!(entries.is_empty());
    }
}
