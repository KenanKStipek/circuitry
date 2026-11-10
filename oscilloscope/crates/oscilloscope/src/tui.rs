//! Lays out one `RenderState` (DESIGN.md §6.3): the header, the plan
//! tree and details panes, the log pane, and the key's own overlays
//! (help, both confirms, the full-value view, the filter prompt).
//! The only module in this crate that imports `ratatui` widgets
//! directly, mirroring `oscilloscope-core`'s own "no terminal code"
//! split one level up.

use oscilloscope_core::model::{RunStatus, StatusKind};
use oscilloscope_core::render::Details;
use oscilloscope_core::treemap::{Measure, TreemapNode};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::keys::{App, Dialog, Pane};
use crate::theme::Theme;

/// Everything a draw function needs to style a cell: the active
/// theme. Threaded by reference through every `draw_*` function
/// below so none of them ever reach for a `Color::` literal of
/// their own.
pub struct DrawCtx<'a> {
    pub theme: &'a Theme,
    /// `t`'s own banner (issue #535): the theme's name, shown in the
    /// header for about 2 seconds after a switch. `None` the rest of
    /// the time.
    pub theme_banner: Option<&'a str>,
    /// Resolves a clock time to this run's own local calendar time
    /// (issue #613): the real system zone in production
    /// (`main::LocalZone`), a fixed offset in tests.
    pub zone: &'a dyn oscilloscope_core::format::ClockZone,
    /// `--no-animation` (issue #626): the header line's own scope-icon
    /// trace draws as a static `◉` instead of an animated braille row.
    pub no_animation: bool,
}

/// A fixed UTC offset for tests that need a [`oscilloscope_core::
/// format::ClockZone`] (issue #613) -- the real system zone is
/// `main::LocalZone`, never used in these tests, so every clock-time
/// assertion here is independent of the machine's own `TZ`.
#[cfg(test)]
fn test_zone() -> chrono::FixedOffset {
    chrono::FixedOffset::east_opt(0).unwrap()
}

/// Issue #527's own small-terminal fallback thresholds: below each,
/// drop one more pane, in this order, until the rest fits. The header
/// line itself never disappears. These are whole-*terminal* widths
/// (DESIGN.md §6.3, the README's TUI section) — checked against
/// `frame.area()`'s own width, never `body`'s. The *height* figures
/// are four rows lower than the panel's own named size (issue #678's
/// shared chrome: the header band, the Totals row, the status line
/// and the key bar cost every screen four rows that used to be the
/// panel's own, so a panel named for an `N`-row terminal is checked
/// against `N - 4` of what is actually left for it, `reduced_area`
/// at every one of these call sites) -- a panel named "80×20" still
/// means a terminal that's actually 80×20, not 80×24.
mod thresholds {
    /// Below this, the two-column layout itself collapses to one.
    /// Issue #635: no screen still uses this pair on its own (the
    /// Flow screen's own `FLOW_TWO_PANE_MIN_WIDTH`/`_MIN_HEIGHT`
    /// below happen to share the same numbers); kept for the Cost
    /// screen's own thresholds just below, which are defined relative
    /// to it. Named for an 80×20 terminal (the panel's own height is
    /// 20 minus the shared chrome's own four rows).
    pub const TWO_COLUMN_MIN_WIDTH: u16 = 80;
    pub const TWO_COLUMN_MIN_HEIGHT: u16 = 16;

    /// The Cost screen's own thresholds (issue #632, DESIGN.md's
    /// "small terminals"): at this size and up, the tokens-over-time
    /// chart sits beside the largest-stores table, under the
    /// treemap. Below it (but still at [`COST_TREEMAP_MIN_WIDTH`]/
    /// `_MIN_HEIGHT`), the chart is dropped and the table takes the
    /// treemap's own row's full width instead. Both figures are whole-
    /// *terminal* sizes, same as every other threshold here -- checked
    /// against `frame.area()`, never `body`, inclusive (100×30 and
    /// 80×20 themselves already qualify).
    pub const COST_CHART_MIN_WIDTH: u16 = 100;
    pub const COST_CHART_MIN_HEIGHT: u16 = 26;
    /// Below this, the treemap itself is dropped too, leaving the
    /// table alone (`⏎` still zooms it) -- the same floor screen 2's
    /// own two-column layout collapses at.
    pub const COST_TREEMAP_MIN_WIDTH: u16 = TWO_COLUMN_MIN_WIDTH;
    pub const COST_TREEMAP_MIN_HEIGHT: u16 = TWO_COLUMN_MIN_HEIGHT;
}
// ---------------------------------------------------------------------
// The Tape (screen 0, issue #678)
// ---------------------------------------------------------------------

/// §3.7's glyph table: a decision/loop's own glyph comes from
/// `RowKind`, regardless of status; everything else comes from
/// `StatusKind` alone.
fn tape_glyph(kind: oscilloscope_core::render::RowKind, status: StatusKind) -> char {
    use oscilloscope_core::render::RowKind;
    // §3.7: ahead/queued/skipped reads `·` regardless of kind -- a
    // decision or loop not yet reached never shows its own `◆`/`⟳`.
    if matches!(status, StatusKind::Pending | StatusKind::Skipped) {
        return '·';
    }
    match kind {
        RowKind::If => '◆',
        RowKind::Loop => '⟳',
        _ => match status {
            StatusKind::Done => '✓',
            StatusKind::Failed | StatusKind::Cancelled | StatusKind::Aborted => '✗',
            StatusKind::FailedHandled => '✗',
            StatusKind::Running | StatusKind::LikelyRunning | StatusKind::RunningOrQueued => '●',
            StatusKind::Pending | StatusKind::Skipped => '·',
        },
    }
}

/// §3.7's glyph colour: a decision/loop's own accent, `fg.faint` for
/// ahead/skipped, the status colour for everything else -- a handled
/// failure reads `status.warning`, not `status.error` (§3.6/§3.7).
fn tape_glyph_style(
    kind: oscilloscope_core::render::RowKind,
    status: StatusKind,
    theme: &Theme,
) -> Style {
    use oscilloscope_core::render::RowKind;
    if matches!(status, StatusKind::Pending | StatusKind::Skipped) {
        return Style::default().fg(theme.dim);
    }
    match kind {
        RowKind::If | RowKind::Loop => Style::default()
            .fg(theme.accent_secondary)
            .add_modifier(Modifier::BOLD),
        _ => match status {
            StatusKind::Done => Style::default().fg(theme.status_done),
            StatusKind::Failed | StatusKind::Cancelled | StatusKind::Aborted => Style::default()
                .fg(theme.status_failed)
                .add_modifier(Modifier::BOLD),
            StatusKind::FailedHandled => Style::default().fg(theme.warning),
            StatusKind::Running | StatusKind::LikelyRunning | StatusKind::RunningOrQueued => {
                Style::default()
                    .fg(theme.accent_primary)
                    .add_modifier(Modifier::BOLD)
            }
            StatusKind::Pending | StatusKind::Skipped => Style::default().fg(theme.dim),
        },
    }
}

fn tape_status_text(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Running => "Running",
        RunStatus::Ok => "Ended",
        RunStatus::Failed => "Failed",
        RunStatus::Cancelled => "Cancelled",
        RunStatus::Aborted => "Aborted",
    }
}

/// The header band (§3.1): ` osi  <where> · <status>` on the left, on
/// `statusbar.bg`; the screen chip and the clock on the right.
fn draw_tape_header_band(
    frame: &mut Frame,
    area: Rect,
    header: &oscilloscope_core::render::Header,
    chip: &str,
    now: &str,
    icon_frame: u64,
    ctx: &DrawCtx,
) {
    let bg = ctx.theme.statusbar_bg;
    let base = Style::default().bg(bg).fg(ctx.theme.statusbar_fg);
    // §3.1: "◌ Starting 0.0s" before the run's own first event, never
    // `RunStatus`'s own "Running" (true only once it's actually
    // under way) -- the same `run_start_ts.is_none()` the Tape's own
    // head rule already keys off of.
    // Scoped to the Tape itself: Flow/Signal/Cost's own existing
    // "Running" reading, even before any state has arrived at all
    // (a state-only poll's very first frame), is unrelated to issue
    // #678's own Starting state and keeps its pre-existing meaning.
    let starting =
        chip == "TAPE" && header.run_start_ts.is_none() && header.status == RunStatus::Running;
    let (glyph, color, status_word) = if starting {
        ('◌', ctx.theme.fg_muted, "Starting")
    } else {
        match header.status {
            RunStatus::Running => (
                '●',
                ctx.theme.accent_primary,
                tape_status_text(header.status),
            ),
            RunStatus::Ok => ('✓', ctx.theme.status_done, tape_status_text(header.status)),
            _ => (
                '✗',
                ctx.theme.status_failed,
                tape_status_text(header.status),
            ),
        }
    };
    // Issue #677's settled §9 point 1: the animated trace takes the
    // `osi` word's own cells while animation is on; the word returns
    // under `--no-animation`.
    let osi_text = if ctx.no_animation {
        " osi  ".to_string()
    } else {
        format!(
            " {}  ",
            oscilloscope_core::icon::header_trace_text(
                icon_frame,
                header.status == RunStatus::Running,
                true
            )
        )
    };
    let osi_color = if ctx.no_animation {
        ctx.theme.accent_primary
    } else {
        crate::icon::trace_color(header.status, ctx.theme)
    };
    let mut left = vec![
        Span::styled(
            osi_text,
            Style::default()
                .fg(osi_color)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{} ", header.document),
            Style::default().fg(ctx.theme.text).bg(bg),
        ),
        Span::styled("· ", Style::default().fg(ctx.theme.dim).bg(bg)),
        Span::styled(
            format!("{glyph} {status_word} "),
            Style::default()
                .fg(color)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            oscilloscope_core::format::format_duration(header.elapsed_s),
            Style::default().fg(ctx.theme.text).bg(bg),
        ),
    ];
    // `t`'s own banner (issue #535), now `T`'s: shown for about 2
    // seconds after a theme switch, reserved out of `pad` the same
    // way the old one-line header did so it never overflows the
    // band together with the clock.
    let banner_text = ctx
        .theme_banner
        .map(|name| format!("theme: {name}  "))
        .unwrap_or_default();
    let left_width: usize = left.iter().map(|s| s.content.chars().count()).sum();
    let clock_text =
        oscilloscope_core::format::format_clock_time(now, ctx.zone).unwrap_or_default();
    let chip_text = format!("  {chip}  ");
    let right_width = banner_text.chars().count()
        + chip_text.chars().count()
        + 2
        + clock_text.chars().count()
        + 1;
    let pad = (area.width as usize).saturating_sub(left_width + right_width);
    left.push(Span::styled(
        " ".repeat(pad),
        Style::default().fg(ctx.theme.text).bg(bg),
    ));
    if !banner_text.is_empty() {
        left.push(Span::styled(
            banner_text,
            Style::default().fg(ctx.theme.dim).bg(bg),
        ));
    }
    left.push(Span::styled(
        chip_text,
        Style::default()
            .fg(ctx.theme.on_accent)
            .bg(ctx.theme.accent_secondary),
    ));
    left.push(Span::styled(format!("  {clock_text} "), base));
    frame.render_widget(Paragraph::new(Line::from(left)).style(base), area);
}

/// The status line's own run-end chrome (DESIGN.md §2.1's
/// `tape--starting`/`tape--ended`/`tape--failed` forms), shared by
/// every screen that watches a run -- `Some` for Starting/Ended/
/// Failed, `None` for Normal, where a caller falls back to its own
/// "newest finished step" line instead (the Tape's own row-based
/// one, the Flow's/Signal's own `last_finished`-based one).
fn shared_run_chrome_status_spans(
    head: &oscilloscope_core::screens::TapeHead,
    header: &oscilloscope_core::render::Header,
    ctx: &DrawCtx,
) -> Option<Vec<Span<'static>>> {
    use oscilloscope_core::screens::TapeHead;
    match head {
        TapeHead::Starting => Some(vec![
            Span::styled(
                " engine launching · waiting for the first event ",
                Style::default().fg(ctx.theme.text),
            ),
            Span::styled(
                format!("· {}", header.engine),
                Style::default().fg(ctx.theme.dim),
            ),
        ]),
        TapeHead::Ended {
            ok,
            duration_s,
            handled_failure,
        } => {
            let total = header.tokens_sent + header.tokens_received;
            let word = if *ok {
                "ok"
            } else {
                match header.status {
                    RunStatus::Cancelled => "cancelled",
                    _ => "aborted",
                }
            };
            let (glyph, glyph_style) = if *ok {
                ('✓', Style::default().fg(ctx.theme.status_done))
            } else {
                ('✗', Style::default().fg(ctx.theme.status_failed))
            };
            let mut spans = vec![
                Span::styled(format!(" {glyph} "), glyph_style),
                Span::styled(
                    format!(
                        "run {word} · {} t · {} ",
                        oscilloscope_core::format::format_tokens_thousands(total),
                        oscilloscope_core::format::format_duration(*duration_s)
                    ),
                    Style::default().fg(ctx.theme.text),
                ),
            ];
            if let Some((label, _)) = handled_failure {
                spans.push(Span::styled(
                    format!("· 1 handled failure · {label}"),
                    Style::default().fg(ctx.theme.dim),
                ));
            }
            Some(spans)
        }
        TapeHead::Failed {
            error,
            failing_path,
        } => {
            let label = failing_path
                .as_deref()
                .map(|p| p.rsplit('.').next().unwrap_or(p).to_string())
                .unwrap_or_default();
            // The status line reads one line; a structured dump
            // after the engine's own prose (a Python-style `[...]`
            // repr of an allowlist, say) isn't part of it -- the
            // detail pane's own Error block still quotes `error` in
            // full.
            let summary = error.split('[').next().unwrap_or(error).trim_end();
            Some(vec![
                Span::styled(" ✗ ", Style::default().fg(ctx.theme.status_failed)),
                Span::styled(
                    format!("run failed · {label} "),
                    Style::default().fg(ctx.theme.text),
                ),
                Span::styled(format!("· {summary}"), Style::default().fg(ctx.theme.dim)),
            ])
        }
        TapeHead::Normal => None,
    }
}

/// The status line (§3.2): the newest finished row, with its tokens
/// and age -- never key instructions, which belong to the key bar.
/// Unchanged by `follow`: issue #678's own golden frames select a
/// row with `follow` explicitly off (a real selection always turns
/// it off) and still read the same finished-step line, so this
/// never varies with it either.
#[allow(clippy::too_many_arguments)]
fn draw_tape_status_line(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::screens::TapeScreen,
    header: &oscilloscope_core::render::Header,
    narrow: bool,
    dialog_open: bool,
    ctx: &DrawCtx,
    now: &str,
) {
    use oscilloscope_core::screens::TapeItem;
    if let Some(spans) = shared_run_chrome_status_spans(&screen.head, header, ctx) {
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }
    // §3.2's "the newest finished step", a loop pass's own leaf
    // (`iter_N`'s own `draft`/`pause`, say) included: `screen.items`
    // is already in finish order while the run is live, so the last
    // `Done`/`FailedHandled` row scanning from the end is literally
    // whichever step finished most recently by the clock.
    let last = screen.items.iter().rev().find_map(|item| match item {
        TapeItem::Row(row)
            if matches!(
                row.status.kind,
                StatusKind::Done | StatusKind::FailedHandled
            ) =>
        {
            Some(row)
        }
        _ => None,
    });
    let Some(row) = last else {
        frame.render_widget(Paragraph::new(Line::from("")), area);
        return;
    };
    let glyph = tape_glyph(row.kind, row.status.kind);
    let age = row
        .ts
        .as_deref()
        .and_then(|t| oscilloscope_core::observe::duration_seconds(t, now));
    // §5: 60-79 columns drops the tokens, the same "info column"
    // narrowing the list's own rows get.
    let tokens = row
        .tokens
        .total()
        .filter(|_| row.tokens.sent.is_some() && !narrow)
        .map(|_| {
            format!(
                " · ↑{} ↓{}",
                oscilloscope_core::format::format_tokens_thousands(row.tokens.sent.unwrap_or(0)),
                oscilloscope_core::format::format_tokens_thousands(
                    row.tokens.received.unwrap_or(0)
                )
            )
        })
        .unwrap_or_default();
    let age_text = age
        .map(|a| format!(" · {} ago", oscilloscope_core::format::format_duration(a)))
        .unwrap_or_default();
    // A modal dialog open (issue #678's own Help) dims every span
    // here to the same `fg.faint`, never the blanket backdrop
    // sweep's own job for this one row -- that sweep touches every
    // cell alike, which would also flatten this row's own trailing
    // blank (nothing ever painted there, left at the frame's own
    // base fill) that the design frame leaves at its own full
    // brightness instead.
    let dim = Style::default().fg(ctx.theme.dim);
    let spans = vec![
        Span::styled(
            format!(" {glyph} "),
            if dialog_open {
                dim
            } else {
                tape_glyph_style(row.kind, row.status.kind, ctx.theme)
            },
        ),
        Span::styled(
            format!("{} finished ", row.label),
            if dialog_open {
                dim
            } else {
                Style::default().fg(ctx.theme.text)
            },
        ),
        Span::styled(format!("{tokens}{age_text}").trim_start().to_string(), dim),
    ];
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// A key bar line (§3.2), shared by every screen: `items` in
/// `keyhint.key`/`keyhint.desc`, on `statusbar.bg`, with the last
/// two items (every caller's own trailing `?`/`q` pair) flush
/// against the bar's right edge and whatever the left items don't
/// use left as a plain run of background-only cells in between --
/// the same shape `draw_too_small`'s own bare `q Quit` already
/// draws by hand. A left item that would run into the right pair
/// is dropped, not clipped mid-word (§3.8's own truncation rule),
/// with a trailing `⋯` marking the cut.
fn draw_key_bar(frame: &mut Frame, area: Rect, items: &[(&str, &str)], ctx: &DrawCtx) {
    let bg = ctx.theme.statusbar_bg;
    let key_style = Style::default()
        .fg(ctx.theme.keyhint_key)
        .bg(bg)
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(ctx.theme.keyhint_desc).bg(bg);
    let split = items.len().saturating_sub(2);
    let (left, right) = items.split_at(split);

    let mut right_spans = Vec::new();
    let mut right_width = 0usize;
    for (i, (key, desc)) in right.iter().enumerate() {
        let key_text = format!("{key} ");
        right_width += key_text.chars().count();
        right_spans.push(Span::styled(key_text, key_style));
        let desc_text = if i + 1 == right.len() {
            format!("{desc} ")
        } else {
            format!("{desc}  ")
        };
        right_width += desc_text.chars().count();
        right_spans.push(Span::styled(desc_text, desc_style));
    }

    let budget = (area.width as usize).saturating_sub(right_width);
    let mut spans = Vec::new();
    let mut used = 0usize;
    let mut dropped = false;
    for (i, (key, desc)) in left.iter().enumerate() {
        let key_text = if i == 0 {
            format!(" {key} ")
        } else {
            format!("{key} ")
        };
        let desc_text = format!("{desc}  ");
        let item_width = key_text.chars().count() + desc_text.chars().count();
        // Room for a trailing "⋯ " once a later item might not fit,
        // so the cut never lands mid-word partway through this one.
        let reserve = if i + 1 < left.len() { 2 } else { 0 };
        if used + item_width + reserve > budget {
            dropped = true;
            break;
        }
        used += item_width;
        spans.push(Span::styled(key_text, key_style));
        spans.push(Span::styled(desc_text, desc_style));
    }
    if dropped {
        spans.push(Span::styled("⋯ ", desc_style));
        used += 2;
    }

    let pad = budget.saturating_sub(used);
    spans.push(Span::styled(" ".repeat(pad), Style::default().bg(bg)));
    spans.extend(right_spans);
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(bg)),
        area,
    );
}

fn tape_key_bar_items(wide: bool) -> Vec<(&'static str, &'static str)> {
    let mut items = vec![("↑↓", "Select"), ("enter", "Open")];
    if wide {
        items.push(("p", "Previews"));
    }
    items.push(("t", "Tokens"));
    if wide {
        items.push(("0-3", "Screens"));
    }
    items.push(("?", "Help"));
    items.push(("q", "Quit"));
    items
}

/// Flow/Cost's own keys (§9's shared chrome around their own,
/// unchanged panels): each screen's own existing keys, kept exactly
/// as before, plus the globals every key bar now carries -- the
/// Tape and Signal draw their own dedicated key bars instead.
fn screen_key_bar_items(
    screen: oscilloscope_core::screens::Screen,
) -> Vec<(&'static str, &'static str)> {
    use oscilloscope_core::screens::Screen;
    let mut items = vec![("↑↓", "Select"), ("tab", "Pane")];
    match screen {
        Screen::Flow => items.extend([
            ("s", "State"),
            ("l", "Log"),
            ("z", "Fold"),
            ("v", "Full Value"),
        ]),
        Screen::Cost => items.extend([("i/o/$", "Measure")]),
        Screen::Tape | Screen::Signal => {}
    }
    items.extend([("e", "Errors"), ("/", "Filter"), ("0-3", "Screens")]);
    items.push(("?", "Help"));
    items.push(("q", "Quit"));
    items
}

/// §3.5's token bar: `▰▰▰▰▱▱▱▱   215 t`, eight cells at `unit`
/// tokens each.
fn tape_token_bar(
    total: u64,
    unit: u64,
    bold: bool,
    live: bool,
    ctx: &DrawCtx,
) -> Vec<Span<'static>> {
    // Rounded, not floored: 389 at a 50-token unit reads a full
    // bar (▰×8), not 7 filled and one empty.
    let filled = ((total as f64 / unit.max(1) as f64).round() as usize).min(8);
    let empty = 8 - filled;
    // A running prompt's own estimate (issue #675) draws the filled
    // cells in the faint style, never the accent colour a finished
    // row's exact total earns -- the bar settles to the normal
    // colour the instant the row's own tokens next read exact.
    let filled_color = if live {
        ctx.theme.dim
    } else {
        ctx.theme.accent_primary
    };
    // The bar's own count never gets the `≈` marker the row's `what`
    // text and the detail pane's own Received line both carry (issue
    // #675): the faint style already says "this is a running
    // estimate", and a live total in the thousands -- a common
    // prompt's own estimate -- needs every one of the column's 16
    // cells just for the digits, with none to spare for a marker on
    // top of them.
    let count_text = oscilloscope_core::format::format_tokens_t(total);
    // The gap before the count shrinks as the count itself widens (the
    // same column-sizing `tape_token_column` already does for a loop's
    // own sparkline+`t/p` suffix) -- the live estimate's own leading
    // `≈` would otherwise push a count of 1,000 tokens or more past the
    // 16-cell column and get cut by the pane's own clipping, silently
    // turning it into a smaller, wrong number.
    let gap = 16usize
        .saturating_sub(8)
        .saturating_sub(count_text.chars().count())
        .max(1);
    // The gap before the count reads as the empty bar's own
    // colour (`design/frames/`'s own span grouping) -- or, once the
    // bar is full and has no empty cells of its own to lend it one,
    // the filled bar's own colour instead.
    let mut spans = if empty > 0 {
        vec![
            Span::styled("▰".repeat(filled), Style::default().fg(filled_color)),
            Span::styled(
                format!("{}{}", "▱".repeat(empty), " ".repeat(gap)),
                Style::default().fg(ctx.theme.dim),
            ),
        ]
    } else {
        vec![Span::styled(
            "▰".repeat(filled) + &" ".repeat(gap),
            Style::default().fg(filled_color),
        )]
    };
    let count_style = if live {
        Style::default().fg(ctx.theme.dim)
    } else if bold {
        Style::default()
            .fg(ctx.theme.text)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(ctx.theme.text)
    };
    spans.push(Span::styled(count_text, count_style));
    spans
}

#[cfg(test)]
mod tape_token_bar_tests {
    use super::*;

    fn printed_width(spans: &[Span<'static>]) -> usize {
        spans.iter().map(|s| s.content.chars().count()).sum()
    }

    /// A live count of 1,000 tokens or more -- a common prompt's own
    /// estimate -- still prints within the column's own 16-cell
    /// budget: the leading marker the faint style's own count carries
    /// must shrink the gap before it, not push the whole thing past
    /// the column and get cut by the pane (issue #675).
    #[test]
    fn a_live_estimate_in_the_thousands_still_fits_the_column() {
        let theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        for total in [41u64, 391, 1902, 9999] {
            let spans = tape_token_bar(total, 50, false, true, &ctx);
            let width = printed_width(&spans);
            assert!(
                width <= 16,
                "a live total of {total} printed {width} cells, over the 16-cell column"
            );
            let full: String = spans.iter().map(|s| s.content.as_ref()).collect();
            let expected_count = oscilloscope_core::format::format_tokens_t(total);
            assert!(
                full.ends_with(&expected_count),
                "the count itself must never be cut: {full:?}"
            );
        }
    }
}

#[cfg(test)]
mod shared_run_chrome_status_spans_tests {
    use super::*;
    use oscilloscope_core::model::RunStatus;
    use oscilloscope_core::screens::TapeHead;

    fn header_with_status(status: RunStatus) -> oscilloscope_core::render::Header {
        let plan = oscilloscope_core::plan::PlanTree::empty();
        let mut model = oscilloscope_core::model::RunModel::new();
        let state = serde_json::json!({"prime": {"value": null, "meta": {"completed_at": "t1"}}});
        let rendered = oscilloscope_core::render::build(
            "do.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            oscilloscope_core::model::ProcessState::Running,
            5.0,
            "t1",
            &[],
        );
        let mut header = rendered.header;
        header.status = status;
        header
    }

    fn spans_text(spans: &[Span<'static>]) -> String {
        spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn ended_ok_reads_run_ok_with_the_handled_failure_suffix() {
        let theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let header = header_with_status(RunStatus::Ok);
        let head = TapeHead::Ended {
            ok: true,
            duration_s: 5.0,
            handled_failure: Some(("flaky".to_string(), "exit 1".to_string())),
        };
        let spans =
            shared_run_chrome_status_spans(&head, &header, &ctx).expect("Ended has its own chrome");
        let text = spans_text(&spans);
        assert!(text.contains("run ok"), "{text}");
        assert!(text.contains("1 handled failure"), "{text}");
    }

    #[test]
    fn ended_not_ok_reads_cancelled_or_aborted_never_run_ok() {
        let theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        for (status, word) in [
            (RunStatus::Cancelled, "run cancelled"),
            (RunStatus::Aborted, "run aborted"),
        ] {
            let header = header_with_status(status);
            let head = TapeHead::Ended {
                ok: false,
                duration_s: 1.0,
                handled_failure: None,
            };
            let spans = shared_run_chrome_status_spans(&head, &header, &ctx)
                .expect("Ended has its own chrome");
            let text = spans_text(&spans);
            assert!(text.contains(word), "{status:?}: {text}");
            assert!(!text.contains("run ok"), "{status:?}: {text}");
        }
    }

    #[test]
    fn normal_has_no_shared_chrome_of_its_own() {
        let theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let header = header_with_status(RunStatus::Running);
        assert_eq!(
            shared_run_chrome_status_spans(&TapeHead::Normal, &header, &ctx),
            None
        );
    }
}

#[cfg(test)]
mod truncate_flow_spans_tests {
    use super::*;

    fn text_of(spans: &[(String, Style)]) -> String {
        spans.iter().map(|(t, _)| t.as_str()).collect()
    }

    #[test]
    fn a_headline_that_fits_exactly_is_left_untouched() {
        let spans = vec![("0/3 passes".to_string(), Style::default())];
        let out = truncate_flow_spans(spans.clone(), rail::STATION_WIDTH);
        assert_eq!(out, spans);
    }

    #[test]
    fn a_long_tool_command_is_cut_to_ten_cells_with_an_ellipsis() {
        let spans = vec![(
            "tool · ls /nonexistent-osi-demo".to_string(),
            Style::default(),
        )];
        let out = truncate_flow_spans(spans, rail::STATION_WIDTH);
        let text = text_of(&out);
        assert_eq!(text.chars().count(), rail::STATION_WIDTH);
        assert!(text.ends_with('…'), "{text:?}");
        assert!(text.starts_with("tool · ls"), "{text:?}");
    }

    #[test]
    fn a_multi_span_detail_keeps_each_spans_own_style_up_to_the_cut() {
        let bold = Style::default().add_modifier(Modifier::BOLD);
        let spans = vec![
            ("1,234,567 t ".to_string(), Style::default()),
            ("+999%".to_string(), bold),
        ];
        let out = truncate_flow_spans(spans, rail::STATION_WIDTH);
        let text = text_of(&out);
        assert_eq!(text.chars().count(), rail::STATION_WIDTH);
        assert!(text.ends_with('…'), "{text:?}");
    }
}

/// §2.1's head rule: `━━ <label> ━━━━...`, filled to `width` -- the Now
/// band, the slots rule and the Starting/Ended/Failed head rules all
/// share this one full-width shape, differing only in colour and
/// whether the outer `━━`/fill and the inner label share one colour
/// (Now, the slots rule) or not (Starting's `border.default` rule
/// around a `fg.muted` label).
fn tape_rule_line(
    label: &str,
    outer: Color,
    inner: Color,
    bold_inner: bool,
    width: usize,
) -> Line<'static> {
    let prefix = "━━ ";
    let suffix_text = format!("{label} ");
    let used = prefix.chars().count() + suffix_text.chars().count();
    let fill = width.saturating_sub(used);
    let mut inner_style = Style::default().fg(inner);
    if bold_inner {
        inner_style = inner_style.add_modifier(Modifier::BOLD);
    }
    Line::from(vec![
        Span::styled(prefix, Style::default().fg(outer)),
        Span::styled(suffix_text, inner_style),
        Span::styled("━".repeat(fill), Style::default().fg(outer)),
    ])
}

/// A Tape pane's own block title (§2.1): the border's own top-left
/// rule shows through as `─` for one cell before the title text
/// starts (`╰─ Tape · ... ────╯`) -- `Block::title`'s own left
/// alignment places a title flush against the border with no gap of
/// its own, so this is the title [`Line`] itself, not a block option.
fn tape_block_title(text: &str, border: Color, ctx: &DrawCtx) -> Line<'static> {
    Line::from(vec![
        Span::styled("─ ", Style::default().fg(border)),
        Span::styled(
            format!("{text} "),
            Style::default()
                .fg(ctx.theme.text)
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

/// Pre-fills `area` with `border`, plain spaces (issue #678's own
/// Tape and detail panes): a row nothing else draws over reads as a
/// border-coloured blank in `design/frames/*.mock`, never the
/// frame's own base fill -- real content, drawn on top right after
/// this, covers every cell it actually uses.
fn fill_border_rows(frame: &mut Frame, area: Rect, border: Color) {
    if area.height == 0 || area.width == 0 {
        return;
    }
    let line = " ".repeat(area.width as usize);
    let lines: Vec<Line> = (0..area.height)
        .map(|_| Line::styled(line.clone(), Style::default().fg(border)))
        .collect();
    frame.render_widget(Paragraph::new(lines), area);
}

/// §3.6's deviation colour band.
fn tape_deviation_style(pct: f64, theme: &Theme) -> Style {
    if pct >= 50.0 {
        Style::default()
            .fg(theme.status_failed)
            .add_modifier(Modifier::BOLD)
    } else if pct >= 25.0 {
        Style::default()
            .fg(theme.warning)
            .add_modifier(Modifier::BOLD)
    } else if pct >= 0.0 {
        Style::default().fg(theme.text)
    } else {
        Style::default()
            .fg(theme.accent_primary)
            .add_modifier(Modifier::BOLD)
    }
}

/// §4's deviation bar (the `t` view): a 4-cell tick either side of
/// a `┼` pivot, growing from the pivot outward as `|pct|/12.5`
/// cells (clamped at 4, so a 47% deviation reads a full tick, not a
/// cell for every percent), filled toward whichever side the sign
/// points -- the far side's own 4 cells stay the plain, unfilled
/// `────`. The same 16 columns the fixed-scale bar fills, so
/// toggling `t` never reflows the column next to it; the deviation
/// colour (§3.6's own band) without [`tape_deviation_style`]'s own
/// bold, which is for the Totals row's own figure, not a line this
/// dense with colour already.
fn tape_usual_deviation_bar(pct: f64, theme: &Theme) -> Vec<Span<'static>> {
    let blocks = ((pct.abs() / 12.5).round() as usize).min(4);
    let color = tape_deviation_style(pct, theme).fg.unwrap_or(theme.text);
    let pct_text = format!("{pct:+.0}%");
    let gap = 6usize.saturating_sub(pct_text.chars().count());
    if pct >= 0.0 {
        let filled = format!("{}{}", "▓".repeat(blocks), "─".repeat(4 - blocks));
        vec![
            Span::styled("────", Style::default().fg(theme.dim)),
            Span::styled("┼", Style::default().fg(theme.fg_muted)),
            Span::styled(
                format!("{filled}{}{pct_text} ", " ".repeat(gap)),
                Style::default().fg(color),
            ),
        ]
    } else {
        let filled = format!("{}{}", "─".repeat(4 - blocks), "▓".repeat(blocks));
        vec![
            Span::styled(filled, Style::default().fg(color)),
            Span::styled("┼", Style::default().fg(theme.fg_muted)),
            Span::styled(
                format!("────{}", " ".repeat(gap)),
                Style::default().fg(theme.dim),
            ),
            Span::styled(format!("{pct_text} "), Style::default().fg(color)),
        ]
    }
}

/// A row's own token column, in whichever view is active (§3.5's bar,
/// or §4's `t` deviation view) -- `None` when the row has nothing to
/// show there at all.
fn tape_token_column(
    tokens: &oscilloscope_core::screens::TapeTokens,
    unit: u64,
    tokens_view_usual: bool,
    ctx: &DrawCtx,
) -> Option<Line<'static>> {
    if !tokens.loop_pass_tokens.is_empty() {
        // Each pass's own level is its tokens in whole bar units
        // (the same `unit` the fixed-scale bar uses), not a fraction
        // of a wider range -- a 130-token pass at the 50-token unit
        // reads ▃ (level 2), not a near-full block.
        let spark: String = tokens
            .loop_pass_tokens
            .iter()
            .map(|&t| {
                let level = ((t / unit.max(1)) as u32).min(7);
                char::from_u32(0x2581 + level).unwrap_or('▁')
            })
            .collect();
        let avg =
            tokens.loop_pass_tokens.iter().sum::<u64>() / tokens.loop_pass_tokens.len() as u64;
        let suffix = format!(
            "{} t/p",
            oscilloscope_core::format::format_tokens_thousands(avg)
        );
        // The same 16-column token column the bar fills, with the
        // suffix right-aligned in it rather than a fixed 3-space gap
        // -- a run with more passes (a wider sparkline) needs a
        // narrower gap to still land "t/p" in the same place.
        let gap = 16usize
            .saturating_sub(spark.chars().count())
            .saturating_sub(suffix.chars().count())
            .max(1);
        return Some(Line::from(vec![
            Span::styled(
                format!("{spark}{}", " ".repeat(gap)),
                Style::default().fg(ctx.theme.accent_secondary),
            ),
            Span::styled(suffix, Style::default().fg(ctx.theme.fg_muted)),
        ]));
    }
    match tokens.total() {
        Some(total) => {
            if tokens_view_usual {
                match tokens.usual {
                    Some(usual) if usual > 0 => {
                        let pct = ((total as f64 - usual as f64) / usual as f64) * 100.0;
                        Some(Line::from(tape_usual_deviation_bar(pct, ctx.theme)))
                    }
                    _ => Some(Line::from(Span::styled(
                        "—",
                        Style::default().fg(ctx.theme.dim),
                    ))),
                }
            } else {
                Some(Line::from(tape_token_bar(
                    total,
                    unit,
                    tokens.is_largest,
                    tokens.live,
                    ctx,
                )))
            }
        }
        None => tokens.usual.map(|usual| {
            Line::from(Span::styled(
                format!(
                    "usually {} t",
                    oscilloscope_core::format::format_tokens_thousands(usual)
                ),
                Style::default().fg(ctx.theme.dim),
            ))
        }),
    }
}

#[allow(clippy::too_many_arguments)]
fn tape_row_line(
    row: &oscilloscope_core::screens::TapeRow,
    selected: bool,
    unit: u64,
    tokens_view_usual: bool,
    narrow: bool,
    wide_detail: bool,
    name_width: usize,
    what_width: usize,
    token_width: usize,
    border_color: Color,
    ctx: &DrawCtx,
) -> Line<'static> {
    // A selected row's own background changes (§2.1's "Selected"),
    // but every span keeps its own foreground -- only the name span
    // switches to `selection.fg` -- so this never uses a line-level
    // `patch_style` with its own `fg`, which would patch every span
    // uniformly and erase the glyph's and token bar's own colours.
    let bg = |s: Style| {
        if selected {
            s.bg(ctx.theme.selection_bg)
        } else {
            s
        }
    };
    let name_style = if selected {
        Style::default()
            .fg(ctx.theme.selection_fg)
            .add_modifier(Modifier::BOLD)
    } else if row.bold
        || (narrow
            && matches!(
                row.status.kind,
                StatusKind::Running | StatusKind::LikelyRunning | StatusKind::RunningOrQueued
            ))
    {
        // A running loop container's own name (never a leaf's --
        // `row.bold` already covers those) reads bold at narrow
        // widths: with no `what` text at all to carry "this is
        // what's happening right now", the name is the only thing
        // left to say so.
        Style::default()
            .fg(ctx.theme.text)
            .add_modifier(Modifier::BOLD)
    } else if matches!(row.status.kind, StatusKind::Pending | StatusKind::Skipped) {
        Style::default().fg(ctx.theme.dim)
    } else {
        Style::default().fg(ctx.theme.text)
    };
    let what_style = if matches!(row.status.kind, StatusKind::Pending | StatusKind::Skipped) {
        Style::default().fg(ctx.theme.dim)
    } else {
        Style::default().fg(ctx.theme.fg_muted)
    };
    // §5: 60–79 columns drops the clock column entirely, not just
    // its text -- the Tape's own 18-cell name field moves 9 columns
    // to the left to fill the space.
    // A row ahead with no timestamp of its own (§3.4) leaves this
    // gap as the pane's own border-coloured fill (`design/frames/`'s
    // own blank convention), never a dim-coloured run of spaces --
    // only a row that has a real clock reading paints over it.
    let ts_text = if narrow {
        None
    } else {
        row.ts
            .as_deref()
            .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone))
            .map(|t| format!("{t} "))
    };
    let ts_style = if ts_text.is_some() {
        Style::default().fg(ctx.theme.dim)
    } else {
        Style::default().fg(border_color)
    };
    let ts_width = if narrow { 0 } else { 9 };
    let mut spans = vec![
        Span::styled(
            format!("{:<ts_width$}", ts_text.unwrap_or_default()),
            bg(ts_style),
        ),
        Span::styled(
            format!("{} ", tape_glyph(row.kind, row.status.kind)),
            bg(tape_glyph_style(row.kind, row.status.kind, ctx.theme)),
        ),
        Span::styled(
            format!("{:<name_width$}", truncate_text(&row.label, name_width)),
            bg(name_style),
        ),
        Span::styled(
            format!(
                "{:<what_width$}",
                tape_what_text(&tape_loop_what(row, wide_detail), what_width)
            ),
            bg(what_style),
        ),
    ];
    match tape_token_column(&row.tokens, unit, tokens_view_usual, ctx) {
        Some(token_line) => {
            // The token column is right-aligned flush to the pane's
            // own inner edge (§3.5: "numbers are always printed next
            // to a bar"), not left-padded by a fixed gap -- a bar and
            // a loop's own sparkline end up the same number of
            // printed characters apart from the right border even
            // though the bar is always 8 cells and a sparkline is as
            // wide as its own pass count. The gap itself reads as the
            // row's own "what" colour (§3.4: the whole row is one span
            // in `design/frames/` when there's nothing to show there
            // yet), not a bare default.
            let printed: usize = token_line
                .spans
                .iter()
                .map(|s| s.content.chars().count())
                .sum();
            let pad = token_width.saturating_sub(printed);
            // A bare "usually N t" (a row ahead with nothing of its
            // own yet) reads left-aligned, its own padding trailing
            // -- only a real bar or a loop's own sparkline, each with
            // a count to line up, is right-aligned.
            let trailing = row.tokens.total().is_none();
            if pad > 0 && !trailing {
                spans.push(Span::styled(" ".repeat(pad), bg(what_style)));
            }
            spans.extend(
                token_line
                    .spans
                    .into_iter()
                    .map(|s| Span::styled(s.content, bg(s.style))),
            );
            if pad > 0 && trailing {
                spans.push(Span::styled(" ".repeat(pad), bg(what_style)));
            }
        }
        None => {
            // Narrow (60-79 columns) has no `what` text of its own at
            // all (`what_width` is 0 there) -- this blank reads as
            // the name's own colour instead, the only one a bare
            // row like this still has.
            let fill_style = if narrow { name_style } else { what_style };
            spans.push(Span::styled(" ".repeat(token_width), bg(fill_style)));
        }
    }
    Line::from(spans)
}

fn truncate_text(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        text.to_string()
    } else if max_chars == 0 {
        String::new()
    } else {
        let truncated: String = text.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{truncated}⋯")
    }
}

/// A row's own `what` text, cut to its own column's width -- one
/// char narrower than [`truncate_text`]'s own plain budget
/// (`what_width - 1`, not `what_width`): text that happens to fill
/// the column exactly, with nothing left over for even one trailing
/// space before the next column starts, still needs to read as cut
/// short, the same `⋯` any longer text already gets, not as if it
/// had simply ended right there by coincidence.
/// A running loop's own `done` count (§3.4's `3 passes · 2 wide ·
/// 0 done`) drops below the 120-column floor's own full height
/// (`wide_detail`) -- the row's own sparkline just past it already
/// carries that figure visually, and 80-119 columns needs the room
/// more than the redundant count.
fn tape_loop_what(
    row: &oscilloscope_core::screens::TapeRow,
    wide_detail: bool,
) -> std::borrow::Cow<'_, str> {
    if wide_detail || row.kind != oscilloscope_core::render::RowKind::Loop {
        return std::borrow::Cow::Borrowed(&row.what);
    }
    match row.what.rfind(" · ") {
        Some(idx) if row.what[idx + 3..].ends_with(" done") => {
            std::borrow::Cow::Owned(row.what[..idx].to_string())
        }
        _ => std::borrow::Cow::Borrowed(&row.what),
    }
}

fn tape_what_text(what: &str, what_width: usize) -> String {
    let budget = what_width.saturating_sub(1);
    if what.chars().count() < budget {
        what.to_string()
    } else if budget == 0 {
        String::new()
    } else {
        let truncated: String = what.chars().take(budget.saturating_sub(1)).collect();
        format!("{truncated}⋯")
    }
}

/// The Tape's own list pane (§2.1): Run Started, finished rows in
/// finish order with their own preview lines, the Now band, the
/// running rows, the slots rule while a loop runs, the rows ahead,
/// Run End, and the Totals row pinned above the bottom border.
#[allow(clippy::too_many_arguments)]
fn draw_tape_list(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::screens::TapeScreen,
    header: &oscilloscope_core::render::Header,
    app: &App,
    narrow: bool,
    wide_detail: bool,
    ctx: &DrawCtx,
) {
    use oscilloscope_core::screens::TapeItem;
    // §2.1/§5: 60-79 columns shortens the title to the done/planned
    // count the Totals row's own Effects field already uses; every
    // other width spells out Done/Running/Ahead, `t`'s own deviation
    // view in place of the unit when it's on, and `p`'s own state
    // once the detail pane has room to show previews at all.
    let title = if narrow {
        let planned = header.effects_planned.unwrap_or(header.effects_done);
        format!(
            "Tape · {}/{planned} · ▰ = {} t",
            header.effects_done, screen.unit
        )
    } else {
        let suffix = if app.tape_tokens_view {
            match header.history_same_hash_count {
                Some(n) if n >= oscilloscope_core::history::P50_MIN_RUNS => {
                    format!("vs Usual · {n} Runs")
                }
                _ => "vs Usual".to_string(),
            }
        } else {
            format!("▰ = {} t", screen.unit)
        };
        let previews = if wide_detail
            && app.dialog == Dialog::None
            && matches!(screen.head, oscilloscope_core::screens::TapeHead::Normal)
        {
            if app.tape_previews_on {
                " · Previews On"
            } else {
                " · Previews Off"
            }
        } else {
            ""
        };
        format!(
            "Tape · {} Done · {} Running · {} Ahead · {suffix}{previews}",
            screen.done, screen.running, screen.ahead
        )
    };
    let border_color = if app.pane == Pane::Tree {
        ctx.theme.border_focused
    } else {
        ctx.theme.border
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(ratatui::symbols::border::ROUNDED)
        .title(tape_block_title(&title, border_color, ctx))
        .border_style(Style::default().fg(border_color));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return;
    }
    // A row neither the list nor the rule/totals row below ever
    // draws into (the design's own frames: a blank row inside the
    // Tape's own border reads as a plain border-coloured blank, not
    // the frame's own base fill) -- filled first, so every real
    // widget below draws over it undisturbed.
    fill_border_rows(frame, inner, border_color);

    // The token field's own width never flexes (the bar's own
    // natural width: 8 cells, a 3-cell gap, up to 5 digits) -- it's
    // the name/what text that gives way to however much room the
    // terminal actually has. §5: 60-79 columns drops the clock and
    // the what column entirely and gives the name whatever's left;
    // 80 and up keeps the clock and gives the what column whatever
    // the name and the token field don't already need.
    let token_width = 16;
    let name_width = if narrow {
        (inner.width as usize).saturating_sub(2 + token_width)
    } else {
        19
    };
    let what_width = if narrow {
        0
    } else {
        // 9 for the clock, 2 for the glyph and its own trailing
        // space.
        (inner.width as usize).saturating_sub(9 + 2 + name_width + token_width)
    };

    let mut lines: Vec<(Option<String>, Line<'static>)> = Vec::new();
    let mut now_band_idx: Option<usize> = None;
    for item in &screen.items {
        match item {
            TapeItem::RunStarted {
                ts,
                document,
                engine,
            } => {
                let mut spans = Vec::new();
                if !narrow {
                    let ts_text = ts
                        .as_deref()
                        .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone))
                        .map(|t| format!("{t} "))
                        .unwrap_or_else(|| " ".repeat(9));
                    spans.push(Span::styled(ts_text, Style::default().fg(ctx.theme.dim)));
                }
                if narrow {
                    let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
                    let width = (inner.width as usize).saturating_sub(used);
                    spans.push(Span::styled(
                        format!("{:<width$}", "■ Run Started"),
                        Style::default().fg(ctx.theme.fg_muted),
                    ));
                } else {
                    spans.push(Span::styled(
                        "■ Run Started        ",
                        Style::default().fg(ctx.theme.fg_muted),
                    ));
                    let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
                    let text = format!("{document} · {engine}");
                    let width = (inner.width as usize).saturating_sub(used);
                    spans.push(Span::styled(
                        format!("{text:<width$}"),
                        Style::default().fg(ctx.theme.dim),
                    ));
                }
                lines.push((None, Line::from(spans)));
            }
            TapeItem::Row(row) => {
                let selected = app.selected_path.as_deref() == Some(row.path.as_str());
                lines.push((
                    Some(row.path.clone()),
                    tape_row_line(
                        row,
                        selected,
                        screen.unit,
                        app.tape_tokens_view,
                        narrow,
                        wide_detail,
                        name_width,
                        what_width,
                        token_width,
                        border_color,
                        ctx,
                    ),
                ));
                // Epic #677's settled point 4: previews are on at
                // 120 columns and up, off below -- `narrow` (60-79)
                // already has no room for a second line at all, but
                // 80-119 doesn't either, below the detail pane's own
                // threshold that `wide_detail` names. A modal dialog
                // open (issue #678's own Help) drops them too, the
                // same as the detail pane's own content -- nothing
                // behind the modal reads as more than one line deep
                // any more.
                if let Some(preview) = &row.preview {
                    if wide_detail && app.dialog == Dialog::None {
                        let preview_style = if selected {
                            Style::default()
                                .fg(ctx.theme.dim)
                                .bg(ctx.theme.selection_bg)
                        } else {
                            Style::default().fg(ctx.theme.dim)
                        };
                        // The preview's own text stays clear of
                        // where a real row's token column would
                        // sit (plus one more gap cell), so it never
                        // reads as if it were trying to line up
                        // under the bar/count instead of the name/
                        // what text above it.
                        let content_zone = (inner.width as usize).saturating_sub(13);
                        let text_width = content_zone.saturating_sub(token_width).saturating_sub(1);
                        let preview_text = truncate_text(preview, text_width);
                        let gutter_style = if selected {
                            preview_style
                        } else {
                            Style::default().fg(border_color)
                        };
                        let mut spans = vec![
                            Span::styled("             ", gutter_style),
                            Span::styled(preview_text.clone(), preview_style),
                        ];
                        {
                            let used = preview_text.chars().count();
                            if used < content_zone {
                                spans.push(Span::styled(
                                    " ".repeat(content_zone - used),
                                    preview_style,
                                ));
                            }
                        }
                        lines.push((Some(row.path.clone()), Line::from(spans)));
                    }
                }
            }
            TapeItem::NowBand { ts, elapsed_s } => {
                now_band_idx = Some(lines.len());
                let ts_text = ts
                    .as_deref()
                    .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone))
                    .unwrap_or_default();
                let label = format!(
                    "Now {ts_text} · {}",
                    oscilloscope_core::format::format_duration(*elapsed_s)
                );
                lines.push((
                    None,
                    tape_rule_line(
                        &label,
                        ctx.theme.accent_primary,
                        ctx.theme.accent_primary,
                        true,
                        inner.width as usize,
                    ),
                ));
            }
            TapeItem::SlotsRule {
                busy,
                slots,
                queued,
            } => {
                let label = format!("{busy} of {slots} Slots Busy · {queued} Pass Queued");
                lines.push((
                    None,
                    tape_rule_line(
                        &label,
                        ctx.theme.accent_primary,
                        ctx.theme.accent_primary,
                        false,
                        inner.width as usize,
                    ),
                ));
            }
            TapeItem::HeadRule => {
                let (outer, inner_color, bold, label) = match &screen.head {
                    oscilloscope_core::screens::TapeHead::Starting => (
                        ctx.theme.border,
                        ctx.theme.fg_muted,
                        true,
                        format!("Starting · {} launching · no events yet", header.engine),
                    ),
                    oscilloscope_core::screens::TapeHead::Ended { ok, duration_s, .. } => {
                        let end = header
                            .run_end_ts
                            .as_deref()
                            .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone))
                            .unwrap_or_default();
                        let outcome_word = if *ok {
                            "OK"
                        } else {
                            tape_status_text(header.status)
                        };
                        (
                            ctx.theme.status_done,
                            ctx.theme.status_done,
                            true,
                            format!(
                                "Run Ended · {outcome_word} · {} · {end}",
                                oscilloscope_core::format::format_duration(*duration_s)
                            ),
                        )
                    }
                    oscilloscope_core::screens::TapeHead::Failed { .. } => {
                        let end = header
                            .run_end_ts
                            .as_deref()
                            .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone))
                            .unwrap_or_default();
                        (
                            ctx.theme.status_failed,
                            ctx.theme.status_failed,
                            true,
                            format!(
                                "Run Failed · {} · {end}",
                                oscilloscope_core::format::format_duration(header.elapsed_s)
                            ),
                        )
                    }
                    oscilloscope_core::screens::TapeHead::Normal => continue,
                };
                lines.push((
                    None,
                    tape_rule_line(&label, outer, inner_color, bold, inner.width as usize),
                ));
            }
            TapeItem::RunEnd {
                usual_total_tokens,
                usual_wall_s,
                eta_ts,
            } => {
                let mut text = "Run End            ".to_string();
                // §5: 60-79 columns drops the usual total, the
                // duration and the ETA here -- the Totals row below
                // already carries the ETA; the leading gap matches
                // every other ahead row's own clock-column gap
                // (§3.4: this row has no clock of its own either).
                let gap = if narrow { String::new() } else { " ".repeat(9) };
                if !narrow {
                    match usual_total_tokens {
                        Some(usual) => {
                            text.push_str(&format!(
                                "usually {} t",
                                oscilloscope_core::format::format_tokens_thousands(*usual)
                            ));
                            // 80-119 columns drops just the duration
                            // (the row still carries its own usual
                            // total and ETA) -- the Totals row
                            // below already has less room there
                            // too, and this row's own gap before
                            // the ETA is tuned for 120's own layout.
                            if wide_detail {
                                if let Some(d) = usual_wall_s {
                                    text.push_str(&format!(
                                        " · {}",
                                        oscilloscope_core::format::format_duration(*d)
                                    ));
                                }
                            }
                            if let Some(eta) = eta_ts.as_deref().and_then(|t| {
                                oscilloscope_core::format::format_clock_time(t, ctx.zone)
                            }) {
                                // The wide gap only makes sense
                                // lined up against the duration it
                                // shares a row with; without one
                                // (80-119 columns), ETA reads right
                                // after the usual total instead.
                                if wide_detail {
                                    text.push_str(&format!("      ETA {eta}"));
                                } else {
                                    text.push_str(&format!(" · ETA {eta}"));
                                }
                            }
                        }
                        // §3.5: below three finished runs, `usually`
                        // and the deviation are absent and the ETA
                        // itself reads `—`, said plainly here since
                        // this row is the one place `ETA` and no
                        // history both show up together.
                        None => text.push_str("ETA — · no history yet"),
                    }
                }
                let width = (inner.width as usize).saturating_sub(gap.chars().count() + 2);
                if text.chars().count() < width {
                    text = format!("{text:<width$}");
                }
                lines.push((
                    None,
                    Line::from(vec![
                        Span::styled(gap, Style::default().fg(border_color)),
                        Span::styled("■ ", Style::default().fg(ctx.theme.fg_muted)),
                        Span::styled(text, Style::default().fg(ctx.theme.dim)),
                    ]),
                ));
            }
        }
    }

    // -1 rule, -1 totals row.
    let viewport = inner.height.saturating_sub(2) as usize;
    // The 80-119 column floor (neither narrow's own tightly packed
    // layout nor 120's own full-height one) keeps one more row of
    // slack at the bottom of its own scrollable area than either
    // (issue #678's own 80x24 vs 60x24: both fit `Run Started`
    // through `Run End` in exactly the same height, but only the
    // narrow one actually shows every row of it) -- the rule and
    // Totals row below still sit right where `viewport` alone
    // would put them; only how many rows this area's own content
    // fills before that blank slack changes.
    let content_rows = if narrow || wide_detail {
        viewport
    } else {
        viewport.saturating_sub(1)
    };
    let total = lines.len();
    let selected_idx = app.selected_path.as_deref().and_then(|p| {
        lines
            .iter()
            .position(|(path, _)| path.as_deref() == Some(p))
    });
    let anchor = if app.follow {
        now_band_idx.unwrap_or(total.saturating_sub(1))
    } else {
        selected_idx.unwrap_or(0)
    };
    // Biased toward the tail, not centred on the anchor: by default
    // this reads like `tail -f`, the run's own most recent and
    // upcoming rows always in view when everything can't fit at
    // once, scrolling up only as far as it has to so the anchor
    // itself (the selection, or the Now band while following)
    // never scrolls out of view above the top.
    let start = if total <= content_rows {
        0
    } else {
        let tail = total - content_rows;
        anchor.min(tail)
    };
    let visible: Vec<Line> = lines
        .into_iter()
        .skip(start)
        .take(content_rows)
        .map(|(_, l)| l)
        .collect();
    let list_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: viewport as u16,
    };
    frame.render_widget(Paragraph::new(visible), list_area);

    let rule_y = inner.y + viewport as u16;
    if rule_y < inner.y + inner.height {
        let rule_area = Rect {
            x: inner.x,
            y: rule_y,
            width: inner.width,
            height: 1,
        };
        frame.render_widget(
            Paragraph::new(Line::from("─".repeat(inner.width as usize)))
                .style(Style::default().fg(ctx.theme.border)),
            rule_area,
        );
    }
    let totals_y = rule_y + 1;
    if totals_y < inner.y + inner.height {
        let totals_area = Rect {
            x: inner.x,
            y: totals_y,
            width: inner.width,
            height: 1,
        };
        draw_totals_row(
            frame,
            totals_area,
            header,
            narrow,
            header.effects_planned_full,
            screen.usual_so_far_tokens,
            Some(border_color),
            matches!(
                screen.head,
                oscilloscope_core::screens::TapeHead::Ended { .. }
                    | oscilloscope_core::screens::TapeHead::Failed { .. }
            ),
            ctx,
        );
    }
}

/// The Totals row (§3.3): tokens sent/received and the run's own
/// total, its deviation from the whole run's own usual tokens once
/// history knows one, effects done of planned, the ETA and (not yet
/// wired to a real figure) the run's own cost -- shared by every
/// screen, pinned above the Tape's own bottom border and, on every
/// other screen, its own outer row below the body.
#[allow(clippy::too_many_arguments)]
fn draw_totals_row(
    frame: &mut Frame,
    area: Rect,
    header: &oscilloscope_core::render::Header,
    compact: bool,
    effects_planned_override: Option<u64>,
    usual_so_far_tokens: Option<u64>,
    gutter_color: Option<Color>,
    // §2.1's Ended/Failed states: the run is already over, so there
    // is no ETA left to show, whatever a stale history-anchored one
    // might still compute to.
    suppress_eta: bool,
    ctx: &DrawCtx,
) {
    let effects_planned = effects_planned_override.or(header.effects_planned);
    // Issue #675: every currently running prompt's own live estimate
    // adds on top of the exact pair, so the Totals row grows while a
    // prompt streams instead of reading zero for it until it lands --
    // `live` marks whether either side actually has one right now, so
    // both figures below can draw in the faint style together and
    // settle back to the normal one the instant nothing is live any
    // more.
    let sent_total = header.tokens_sent + header.tokens_sent_live_estimate;
    let received_total = header.tokens_received + header.tokens_received_live_estimate;
    let live = header.tokens_sent_live_estimate > 0 || header.tokens_received_live_estimate > 0;
    // §3.3's own deviation: the sum of every row that has run so far,
    // its own usual tokens (the Tape's `usual_so_far_tokens`) --
    // comparing against the *whole run's* eventual usual instead
    // would read wildly low on any partial run. Every other screen,
    // with no rows of its own to sum, falls back to the whole run's
    // usual total, same as before.
    let usual_pct = usual_so_far_tokens
        .or(header.history_usual_total_tokens)
        .filter(|_| sent_total + received_total > 0)
        .map(|usual| {
            let total = sent_total + received_total;
            ((total as f64 - usual as f64) / usual.max(1) as f64) * 100.0
        });
    if compact {
        // §5: 60-79 columns drops the ↑/↓ breakdown, the Usual/
        // Effects/Cost words and (since the Tape's own Run End row
        // already dropped it) keeps only the ETA.
        let total = sent_total + received_total;
        let mut spans = vec![
            Span::styled(
                " ",
                Style::default().fg(gutter_color.unwrap_or(ctx.theme.text)),
            ),
            Span::styled(
                "Totals ",
                Style::default()
                    .fg(ctx.theme.text)
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        let mut headline = if live {
            format!("≈{}", oscilloscope_core::format::format_tokens_t(total))
        } else {
            oscilloscope_core::format::format_tokens_t(total)
        };
        if let Some(pct) = usual_pct {
            headline.push_str(&format!(" {pct:+.0}%"));
        }
        headline.push(' ');
        let headline_style = if live {
            Style::default().fg(ctx.theme.dim)
        } else {
            Style::default()
                .fg(ctx.theme.text)
                .add_modifier(Modifier::BOLD)
        };
        spans.push(Span::styled(headline, headline_style));
        let effects_text = effects_planned
            .map(|p| format!("{}/{p}", header.effects_done))
            .unwrap_or_else(|| header.effects_done.to_string());
        spans.push(Span::styled(
            format!("· {effects_text} "),
            Style::default().fg(ctx.theme.text),
        ));
        let eta_text = header
            .history_eta_ts
            .as_deref()
            .filter(|_| !suppress_eta)
            .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone));
        spans.push(Span::styled(
            "· ETA ",
            Style::default().fg(ctx.theme.fg_muted),
        ));
        let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        let width = area.width as usize;
        let eta_value = eta_text.as_deref().unwrap_or("—");
        let padded_eta = if used + eta_value.chars().count() < width {
            format!("{eta_value:<width$}", width = width - used)
        } else {
            eta_value.to_string()
        };
        spans.push(Span::styled(
            padded_eta,
            if eta_text.is_some() {
                Style::default().fg(ctx.theme.text)
            } else {
                Style::default().fg(ctx.theme.dim)
            },
        ));
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }
    let mut spans = vec![
        Span::styled(
            " ",
            Style::default().fg(gutter_color.unwrap_or(ctx.theme.text)),
        ),
        Span::styled(
            "Totals ",
            Style::default()
                .fg(ctx.theme.text)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                "{}↑{} ↓{} · ",
                if live { "≈" } else { "" },
                oscilloscope_core::format::format_tokens_thousands(sent_total),
                oscilloscope_core::format::format_tokens_thousands(received_total)
            ),
            if live {
                Style::default().fg(ctx.theme.dim)
            } else {
                Style::default().fg(ctx.theme.text)
            },
        ),
        Span::styled(
            format!(
                "{} t",
                oscilloscope_core::format::format_tokens_thousands(sent_total + received_total)
            ),
            if live {
                Style::default().fg(ctx.theme.dim)
            } else {
                Style::default()
                    .fg(ctx.theme.text)
                    .add_modifier(Modifier::BOLD)
            },
        ),
    ];
    if let Some(pct) = usual_pct {
        // §3.6: the 0-25% band is bold here even though a row's own
        // deviation isn't -- every other band is already bold.
        spans.push(Span::styled(
            format!("  {pct:+.0}% "),
            tape_deviation_style(pct, ctx.theme).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            "Usual  ",
            Style::default().fg(ctx.theme.fg_muted),
        ));
    } else {
        spans.push(Span::styled(
            "  ",
            Style::default()
                .fg(ctx.theme.text)
                .add_modifier(Modifier::BOLD),
        ));
    }
    let effects_text = effects_planned
        .map(|p| format!("{}/{p}", header.effects_done))
        .unwrap_or_else(|| header.effects_done.to_string());
    spans.push(Span::styled(
        format!("{effects_text} "),
        Style::default().fg(ctx.theme.text),
    ));
    spans.push(Span::styled(
        "Effects  ",
        Style::default().fg(ctx.theme.fg_muted),
    ));
    let eta_text = header
        .history_eta_ts
        .as_deref()
        .filter(|_| !suppress_eta)
        .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone));
    spans.push(Span::styled(
        "ETA ",
        Style::default().fg(ctx.theme.fg_muted),
    ));
    spans.push(Span::styled(
        format!("{}  ", eta_text.as_deref().unwrap_or("—")),
        if eta_text.is_some() || gutter_color.is_none() {
            Style::default().fg(ctx.theme.text)
        } else {
            Style::default().fg(ctx.theme.dim)
        },
    ));
    spans.push(Span::styled(
        "Cost ",
        Style::default().fg(ctx.theme.fg_muted),
    ));
    // The Tape's own Totals row fills the rest of the pane with this
    // dash's own colour (§2.1's own blank-row convention); every
    // other screen keeps its pre-§678 shape, a single bare dash with
    // whatever's behind it showing through.
    if gutter_color.is_some() {
        let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        let width = area.width as usize;
        let dash = if used < width {
            format!("—{}", " ".repeat(width - used - 1))
        } else {
            "—".to_string()
        };
        spans.push(Span::styled(dash, Style::default().fg(ctx.theme.dim)));
    } else {
        spans.push(Span::styled("—", Style::default().fg(ctx.theme.dim)));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The detail pane (§2.1): the selected row's own Status, Type,
/// Tokens, Usual, Share, then the prompt sent and the reply in full.
/// A detail-pane line, padded to `width` with the style of its own
/// last span (issue #678's §2.1 detail pane: a line with real
/// content fills the rest of its own row with that content's own
/// colour, never the pane's border fill, which stays only where
/// nothing draws at all -- `draw_tape_detail`'s own gutter column
/// and every wholly blank row).
fn tape_detail_line(mut spans: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    if used < width {
        let fill_style = spans.last().map(|s| s.style).unwrap_or_default();
        spans.push(Span::styled(" ".repeat(width - used), fill_style));
    }
    Line::from(spans)
}

/// Greedy word-wrap at `width` columns (issue #678's §2.1 detail
/// pane: a prompt sent or a reply, shown in full, never truncated
/// with `⋯`) -- whitespace collapsed first, same as a preview
/// line's own text; a single word wider than `width` is hard-cut
/// rather than left overflowing.
fn tape_word_wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let collapsed: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in collapsed.split(' ') {
        let mut word = word;
        if current.is_empty() && word.chars().count() > width {
            while word.chars().count() > width {
                let (head, rest) = {
                    let mut chars = word.chars();
                    let head: String = chars.by_ref().take(width).collect();
                    (head, chars.as_str().to_string())
                };
                lines.push(head);
                word = Box::leak(rest.into_boxed_str());
            }
        }
        let extra = if current.is_empty() { 0 } else { 1 };
        if current.chars().count() + extra + word.chars().count() > width {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            current.push_str(word);
        } else {
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// The Status line's own glyph, word and style (issue #678's §2.1
/// detail pane) -- not [`tape_glyph`]/[`tape_glyph_style`], which
/// both need a row's own [`oscilloscope_core::render::RowKind`] that
/// [`oscilloscope_core::screens::FocusedStep`] doesn't carry.
fn tape_status_glyph_word_style(kind: StatusKind, theme: &Theme) -> (char, &'static str, Style) {
    match kind {
        StatusKind::Done => ('✓', "Done", Style::default().fg(theme.status_done)),
        StatusKind::Running | StatusKind::LikelyRunning | StatusKind::RunningOrQueued => (
            '●',
            "Running",
            Style::default()
                .fg(theme.accent_primary)
                .add_modifier(Modifier::BOLD),
        ),
        StatusKind::Failed => ('✗', "Failed", Style::default().fg(theme.status_failed)),
        StatusKind::FailedHandled => ('✗', "Handled", Style::default().fg(theme.warning)),
        StatusKind::Pending | StatusKind::Skipped => {
            ('·', "Pending", Style::default().fg(theme.dim))
        }
        StatusKind::Cancelled => ('✗', "Cancelled", Style::default().fg(theme.status_failed)),
        StatusKind::Aborted => ('✗', "Aborted", Style::default().fg(theme.status_failed)),
    }
}

/// The Normal/Usual states' own detail pane (§2.1): the selected
/// row's Status, Type, Tokens, Usual, Share, then the prompt sent and
/// the reply in full, each headed with its own token count.
fn tape_normal_detail_lines(
    screen: &oscilloscope_core::screens::TapeScreen,
    width: usize,
    ctx: &DrawCtx,
) -> Vec<Line<'static>> {
    let muted = Style::default().fg(ctx.theme.fg_muted);
    let default = Style::default().fg(ctx.theme.text);
    let title_bold = Style::default()
        .fg(ctx.theme.text)
        .add_modifier(Modifier::BOLD);
    let Some(step) = &screen.selected else {
        return vec![tape_detail_line(
            vec![Span::styled("nothing selected", default)],
            width,
        )];
    };
    let detail = screen.selected_detail.clone().unwrap_or_default();
    let mut lines = Vec::new();

    let (glyph, word, glyph_style) = tape_status_glyph_word_style(step.status.kind, ctx.theme);
    let finished_clock = detail
        .finished_ts
        .as_deref()
        .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone));
    let mut status_value = String::new();
    if let Some(secs) = step.elapsed_s {
        status_value.push_str(&oscilloscope_core::format::format_duration(secs));
    }
    if let Some(clock) = finished_clock {
        if !status_value.is_empty() {
            status_value.push_str(" · ");
        }
        status_value.push_str(&clock);
    }
    lines.push(tape_detail_line(
        vec![
            Span::styled("Status  ", muted),
            Span::styled(format!("{glyph} {word}  "), glyph_style),
            Span::styled(status_value, default),
        ],
        width,
    ));

    let type_text = match &step.provider_or_model {
        Some(m) => format!("{} · {m}", step.kind_label),
        None => step.kind_label.to_string(),
    };
    lines.push(tape_detail_line(
        vec![
            Span::styled("Type    ", muted),
            Span::styled(type_text, default),
        ],
        width,
    ));

    // Issue #675's Live block: a running prompt shows its own live
    // figures here, in place of the Tokens line below (never both --
    // `detail.tokens_sent`/`_received` are `None` for exactly as long
    // as `detail.live` is `Some`, since a running prompt's own exact
    // tokens don't exist yet).
    if let Some(live) = &detail.live {
        if !live.streaming {
            lines.push(tape_detail_line(
                vec![
                    Span::styled("Live    ", muted),
                    Span::styled("no live figures from this adapter", muted),
                ],
                width,
            ));
        } else {
            let first_token_value = if live.waiting_for_first_token {
                format!(
                    "waiting · {}",
                    live.elapsed_s
                        .map(oscilloscope_core::format::format_duration)
                        .unwrap_or_default()
                )
            } else {
                format!(
                    "after {}",
                    live.first_token_elapsed_s
                        .map(oscilloscope_core::format::format_duration)
                        .unwrap_or_default()
                )
            };
            lines.push(tape_detail_line(
                vec![
                    Span::styled("First token  ", muted),
                    Span::styled(first_token_value, default),
                ],
                width,
            ));
            if let Some((n, is_estimate)) = live.tokens_received_or_estimate() {
                let marker = if is_estimate { "≈" } else { "" };
                let mut received_text = format!(
                    "{marker}{} t",
                    oscilloscope_core::format::format_tokens_thousands(n)
                );
                if let Some(rate) = live.tokens_per_second {
                    received_text.push_str(&format!(" · {} t/s", rate.round() as i64));
                }
                lines.push(tape_detail_line(
                    vec![
                        Span::styled("Received  ", muted),
                        Span::styled(received_text, default),
                    ],
                    width,
                ));
            }
            if let Some((n, is_estimate)) = live.tokens_sent_or_estimate() {
                let sent_text = if is_estimate {
                    format!(
                        "≈{} t",
                        oscilloscope_core::format::format_tokens_thousands(n)
                    )
                } else {
                    format!(
                        "↑{} t",
                        oscilloscope_core::format::format_tokens_thousands(n)
                    )
                };
                lines.push(tape_detail_line(
                    vec![
                        Span::styled("Sent  ", muted),
                        Span::styled(sent_text, default),
                    ],
                    width,
                ));
            }
        }
        if live.attempt.is_some_and(|a| a > 0) {
            lines.push(tape_detail_line(
                vec![
                    Span::styled("Attempt  ", muted),
                    Span::styled(format!("{}", live.attempt.unwrap() + 1), default),
                ],
                width,
            ));
        }
    }

    if let (Some(sent), Some(received)) = (detail.tokens_sent, detail.tokens_received) {
        let total = sent + received;
        lines.push(tape_detail_line(
            vec![
                Span::styled("Tokens  ", muted),
                Span::styled(
                    format!(
                        "↑{} ↓{} ",
                        oscilloscope_core::format::format_tokens_thousands(sent),
                        oscilloscope_core::format::format_tokens_thousands(received)
                    ),
                    default,
                ),
                Span::styled("· ", muted),
                Span::styled(
                    oscilloscope_core::format::format_tokens_t(total),
                    default.add_modifier(Modifier::BOLD),
                ),
            ],
            width,
        ));
    }

    if let Some((usual, count)) = screen.selected_usual {
        let dev = step
            .tokens
            .filter(|_| usual > 0)
            .map(|t| ((t as f64 - usual as f64) / usual as f64) * 100.0);
        let mut spans = vec![
            Span::styled("Usual   ", muted),
            Span::styled(
                format!(
                    "{} t  ",
                    oscilloscope_core::format::format_tokens_thousands(usual)
                ),
                default,
            ),
        ];
        if let Some(d) = dev {
            spans.push(Span::styled(
                format!("{d:+.0}% "),
                tape_deviation_style(d, ctx.theme).add_modifier(Modifier::BOLD),
            ));
        }
        spans.push(Span::styled(format!("· median of {count} runs"), muted));
        lines.push(tape_detail_line(spans, width));
    }

    if let Some(pct) = screen.selected_share_pct {
        lines.push(tape_detail_line(
            vec![
                Span::styled("Share   ", muted),
                Span::styled(format!("{pct}% of this run so far"), default),
            ],
            width,
        ));
    }

    if let Some(prompt_sent) = &detail.prompt_sent {
        lines.push(Line::from(""));
        // A running prompt's own `tokens_sent` reads `None`
        // (`build_tape_selected_detail`'s own doc comment) -- the
        // Live block below already carries the sent figure for one
        // of those, so this heading drops its own count rather than
        // print a bogus `0 t`.
        let mut heading = vec![Span::styled("Prompt Sent ", title_bold)];
        if let Some(tokens) = detail.prompt_sent_tokens {
            heading.push(Span::styled(
                format!(
                    "· {} t",
                    oscilloscope_core::format::format_tokens_thousands(tokens)
                ),
                muted,
            ));
        }
        lines.push(tape_detail_line(heading, width));
        for line in tape_word_wrap(prompt_sent, width.saturating_sub(1)) {
            lines.push(tape_detail_line(vec![Span::styled(line, default)], width));
        }
    }

    if let Some(reply) = &detail.reply {
        lines.push(Line::from(""));
        lines.push(tape_detail_line(
            vec![
                Span::styled("Reply ", title_bold),
                Span::styled(
                    format!(
                        "· {} t",
                        oscilloscope_core::format::format_tokens_thousands(
                            detail.reply_tokens.unwrap_or(0)
                        )
                    ),
                    muted,
                ),
            ],
            width,
        ));
        for line in tape_word_wrap(reply, width.saturating_sub(1)) {
            lines.push(tape_detail_line(vec![Span::styled(line, default)], width));
        }
    }

    lines
}

/// The Starting state's own detail pane (§2.1): Engine, Plan, Shape,
/// Usual, ETA, and the first step.
fn tape_starting_detail_lines(
    detail: &oscilloscope_core::screens::TapeStartingDetail,
    width: usize,
    ctx: &DrawCtx,
) -> Vec<Line<'static>> {
    let muted = Style::default().fg(ctx.theme.fg_muted);
    let default = Style::default().fg(ctx.theme.text);
    let title_bold = Style::default()
        .fg(ctx.theme.text)
        .add_modifier(Modifier::BOLD);
    let mut lines = vec![
        tape_detail_line(
            vec![
                Span::styled("Engine ", muted),
                Span::styled(format!("{} · launching", detail.engine), default),
            ],
            width,
        ),
        tape_detail_line(
            vec![
                Span::styled("Plan   ", muted),
                Span::styled(detail.plan_text.clone(), default),
            ],
            width,
        ),
        tape_detail_line(
            vec![
                Span::styled("Shape  ", muted),
                Span::styled(detail.shape_text.clone(), default),
            ],
            width,
        ),
    ];
    let mut usual_spans = vec![Span::styled("Usual  ", muted)];
    match (
        detail.usual_total_tokens,
        detail.usual_total_s,
        detail.usual_count,
    ) {
        (Some(tokens), Some(wall_s), Some(count)) => {
            usual_spans.push(Span::styled(
                format!(
                    "{} t ",
                    oscilloscope_core::format::format_tokens_thousands(tokens)
                ),
                default.add_modifier(Modifier::BOLD),
            ));
            usual_spans.push(Span::styled(
                format!(
                    "· {} · median of {count} runs",
                    oscilloscope_core::format::format_duration(wall_s)
                ),
                muted,
            ));
        }
        _ => usual_spans.push(Span::styled("—", Style::default().fg(ctx.theme.dim))),
    }
    lines.push(tape_detail_line(usual_spans, width));
    let mut eta_spans = vec![Span::styled("ETA    ", muted)];
    match detail
        .eta_ts
        .as_deref()
        .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone))
    {
        Some(eta) => {
            eta_spans.push(Span::styled(
                format!("{eta} "),
                default.add_modifier(Modifier::BOLD),
            ));
            eta_spans.push(Span::styled("if usual", muted));
        }
        None => eta_spans.push(Span::styled("—", Style::default().fg(ctx.theme.dim))),
    }
    lines.push(tape_detail_line(eta_spans, width));
    lines.push(Line::from(""));
    lines.push(tape_detail_line(
        vec![Span::styled("First Step", title_bold)],
        width,
    ));
    let first_step_text = match detail.first_step_type.is_empty() {
        true => detail.first_step_label.clone(),
        false => format!("{} · {}", detail.first_step_label, detail.first_step_type),
    };
    lines.push(tape_detail_line(
        vec![Span::styled(first_step_text, default)],
        width,
    ));
    if let Some(usual) = detail.first_step_usual {
        lines.push(tape_detail_line(
            vec![Span::styled(
                format!(
                    "usually {} t",
                    oscilloscope_core::format::format_tokens_thousands(usual)
                ),
                muted,
            )],
            width,
        ));
    }
    lines.push(Line::from(""));
    // §1.1's own principle, verbatim: the only content a Starting
    // detail pane has beyond the run's own shape, since nothing has
    // happened yet to show.
    for sentence in [
        "The tape fills from the top as events arrive.",
        "Nothing is shown that has not happened.",
    ] {
        lines.push(tape_detail_line(
            vec![Span::styled(
                truncate_text(sentence, width),
                Style::default().fg(ctx.theme.dim),
            )],
            width,
        ));
    }
    lines
}

/// The Ended state's own detail pane (§2.1): Outcome, Tokens, Usual,
/// Effects, Decision, Largest, the handled failure's error and the
/// run's summary value.
fn tape_ended_detail_lines(
    detail: &oscilloscope_core::screens::TapeEndedDetail,
    width: usize,
    ctx: &DrawCtx,
) -> Vec<Line<'static>> {
    let muted = Style::default().fg(ctx.theme.fg_muted);
    let default = Style::default().fg(ctx.theme.text);
    let title_bold = Style::default()
        .fg(ctx.theme.text)
        .add_modifier(Modifier::BOLD);
    let outcome_style = if detail.ok {
        Style::default()
            .fg(ctx.theme.status_done)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
            .fg(ctx.theme.status_failed)
            .add_modifier(Modifier::BOLD)
    };
    let outcome_word = if detail.ok { "OK" } else { "Failed" };
    let outcome_glyph = if detail.ok { "✓" } else { "✗" };
    let start = detail
        .start_ts
        .as_deref()
        .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone));
    let end = detail
        .end_ts
        .as_deref()
        .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone));
    let range = match (start, end) {
        (Some(s), Some(e)) => format!("{s} → {e}"),
        _ => String::new(),
    };
    let mut lines = vec![
        tape_detail_line(
            vec![
                Span::styled("Outcome  ", muted),
                Span::styled(format!("{outcome_glyph} {outcome_word}  "), outcome_style),
                Span::styled(
                    format!(
                        "{} · {range}",
                        oscilloscope_core::format::format_duration(detail.duration_s)
                    ),
                    default,
                ),
            ],
            width,
        ),
        tape_detail_line(
            vec![
                Span::styled("Tokens   ", muted),
                Span::styled(
                    format!(
                        "↑{} ↓{} · ",
                        oscilloscope_core::format::format_tokens_thousands(detail.tokens_sent),
                        oscilloscope_core::format::format_tokens_thousands(detail.tokens_received)
                    ),
                    default,
                ),
                Span::styled(
                    oscilloscope_core::format::format_tokens_t(
                        detail.tokens_sent + detail.tokens_received,
                    ),
                    default.add_modifier(Modifier::BOLD),
                ),
            ],
            width,
        ),
    ];
    let mut usual_spans = vec![Span::styled("Usual    ", muted)];
    match (detail.usual_total_tokens, detail.usual_dev_pct) {
        (Some(tokens), Some(dev)) => {
            usual_spans.push(Span::styled(
                format!(
                    "{} t ",
                    oscilloscope_core::format::format_tokens_thousands(tokens)
                ),
                default,
            ));
            usual_spans.push(Span::styled(
                format!("{dev:+.0}% "),
                tape_deviation_style(dev, ctx.theme).add_modifier(Modifier::BOLD),
            ));
            let wall = detail
                .usual_total_s
                .map(|s| format!("· {} usual", oscilloscope_core::format::format_duration(s)))
                .unwrap_or_default();
            usual_spans.push(Span::styled(wall, muted));
        }
        _ => usual_spans.push(Span::styled("—", Style::default().fg(ctx.theme.dim))),
    }
    lines.push(tape_detail_line(usual_spans, width));
    let mut effects_spans = vec![Span::styled("Effects  ", muted)];
    let planned_text = detail
        .effects_planned
        .map(|p| format!("{} of {p}", detail.effects_done))
        .unwrap_or_else(|| detail.effects_done.to_string());
    let handled_text = if detail.handled_count > 0 {
        format!(
            " · {} handled failure{}",
            detail.handled_count,
            if detail.handled_count == 1 { "" } else { "s" }
        )
    } else {
        String::new()
    };
    effects_spans.push(Span::styled(
        format!("{planned_text}{handled_text}"),
        default,
    ));
    lines.push(tape_detail_line(effects_spans, width));
    if let Some(decision) = &detail.decision_text {
        lines.push(tape_detail_line(
            vec![
                Span::styled("Decision ", muted),
                Span::styled(decision.clone(), default),
            ],
            width,
        ));
    }
    if let (Some(label), Some(tokens)) = (&detail.largest_label, detail.largest_tokens) {
        lines.push(tape_detail_line(
            vec![
                Span::styled("Largest  ", muted),
                Span::styled(format!("{label} "), default),
                Span::styled(
                    oscilloscope_core::format::format_tokens_t(tokens),
                    default.add_modifier(Modifier::BOLD),
                ),
            ],
            width,
        ));
    }
    if let Some(handled) = &detail.handled_failure {
        lines.push(Line::from(""));
        lines.push(tape_detail_line(
            vec![
                Span::styled("Handled Failure ", title_bold),
                Span::styled(format!("· {}", handled.label), muted),
            ],
            width,
        ));
        lines.push(tape_detail_line(
            vec![Span::styled(handled.what.clone(), default)],
            width,
        ));
        lines.push(tape_detail_line(
            vec![Span::styled(
                truncate_text(&handled.exit_on_error, width),
                muted,
            )],
            width,
        ));
        for line in tape_word_wrap(&handled.error, width.saturating_sub(1)) {
            lines.push(tape_detail_line(vec![Span::styled(line, muted)], width));
        }
    }
    if let Some(summary) = &detail.summary {
        lines.push(Line::from(""));
        lines.push(tape_detail_line(
            vec![Span::styled("Summary", title_bold)],
            width,
        ));
        for line in tape_word_wrap(summary, width.saturating_sub(1)) {
            lines.push(tape_detail_line(vec![Span::styled(line, default)], width));
        }
    }
    lines
}

/// The Failed state's own detail pane (§2.1): the error in full and
/// the list of what was not reached.
fn tape_failed_detail_lines(
    detail: &oscilloscope_core::screens::TapeFailedDetail,
    width: usize,
    ctx: &DrawCtx,
) -> Vec<Line<'static>> {
    let muted = Style::default().fg(ctx.theme.fg_muted);
    let default = Style::default().fg(ctx.theme.text);
    let title_bold = Style::default()
        .fg(ctx.theme.text)
        .add_modifier(Modifier::BOLD);
    let start = detail
        .start_ts
        .as_deref()
        .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone));
    let mut lines = vec![
        tape_detail_line(
            vec![
                Span::styled("Outcome ", muted),
                Span::styled(
                    "✗ Failed  ",
                    Style::default()
                        .fg(ctx.theme.status_failed)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(
                        "{} · {}",
                        oscilloscope_core::format::format_duration(detail.duration_s),
                        start.unwrap_or_default()
                    ),
                    default,
                ),
            ],
            width,
        ),
        tape_detail_line(
            vec![
                Span::styled("Step    ", muted),
                Span::styled(
                    format!("{} · {}", detail.failing_label, detail.failing_type),
                    default,
                ),
            ],
            width,
        ),
        tape_detail_line(
            vec![
                Span::styled("Tokens  ", muted),
                Span::styled(
                    format!(
                        "↑{} ↓{} · ",
                        oscilloscope_core::format::format_tokens_thousands(detail.tokens_sent),
                        oscilloscope_core::format::format_tokens_thousands(detail.tokens_received)
                    ),
                    default,
                ),
                Span::styled(
                    oscilloscope_core::format::format_tokens_t(
                        detail.tokens_sent + detail.tokens_received,
                    ),
                    default.add_modifier(Modifier::BOLD),
                ),
            ],
            width,
        ),
    ];
    let mut usual_spans = vec![Span::styled("Usual   ", muted)];
    match (detail.usual_tokens, detail.usual_dev_pct) {
        (Some(tokens), Some(dev)) => {
            usual_spans.push(Span::styled(
                format!(
                    "{} t ",
                    oscilloscope_core::format::format_tokens_thousands(tokens)
                ),
                default,
            ));
            usual_spans.push(Span::styled(
                format!("{dev:+.0}%"),
                tape_deviation_style(dev, ctx.theme).add_modifier(Modifier::BOLD),
            ));
        }
        _ => usual_spans.push(Span::styled("—", Style::default().fg(ctx.theme.dim))),
    }
    lines.push(tape_detail_line(usual_spans, width));
    lines.push(Line::from(""));
    lines.push(tape_detail_line(
        vec![Span::styled("Error", title_bold)],
        width,
    ));
    for line in tape_word_wrap(&detail.error, width.saturating_sub(1)) {
        lines.push(tape_detail_line(vec![Span::styled(line, default)], width));
    }
    lines.push(Line::from(""));
    lines.push(tape_detail_line(
        vec![Span::styled("Not Reached", title_bold)],
        width,
    ));
    for item in &detail.not_reached {
        lines.push(tape_detail_line(
            vec![Span::styled(
                format!("· {item}"),
                Style::default().fg(ctx.theme.dim),
            )],
            width,
        ));
    }
    lines
}

fn draw_tape_detail(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::screens::TapeScreen,
    app: &App,
    ctx: &DrawCtx,
) {
    use oscilloscope_core::screens::TapeHead;
    let title = match &screen.head {
        TapeHead::Starting => "Run · Starting".to_string(),
        TapeHead::Ended { .. } => "Run · Ended".to_string(),
        TapeHead::Failed { .. } => "Run · Failed".to_string(),
        TapeHead::Normal => match &screen.selected_breadcrumb {
            Some(breadcrumb) => format!("{breadcrumb} · Selected"),
            None => "Tape".to_string(),
        },
    };
    let border_color = if app.pane == Pane::Details {
        ctx.theme.border_focused
    } else {
        ctx.theme.border
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(ratatui::symbols::border::ROUNDED)
        .title(tape_block_title(&title, border_color, ctx))
        .border_style(Style::default().fg(border_color));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    fill_border_rows(frame, inner, border_color);
    let content_area = Rect {
        x: inner.x + 1,
        y: inner.y,
        width: inner.width.saturating_sub(1),
        height: inner.height,
    };
    // A modal dialog open (issue #678's own Help) leaves the detail
    // pane's own border and title, never its content -- the modal
    // itself is the only thing actually readable right now.
    if app.dialog != Dialog::None {
        return;
    }
    let width = content_area.width as usize;
    let lines = match &screen.head {
        TapeHead::Starting => screen
            .starting_detail
            .as_ref()
            .map(|d| tape_starting_detail_lines(d, width, ctx))
            .unwrap_or_default(),
        TapeHead::Ended { .. } => screen
            .ended_detail
            .as_ref()
            .map(|d| tape_ended_detail_lines(d, width, ctx))
            .unwrap_or_default(),
        TapeHead::Failed { .. } => screen
            .failed_detail
            .as_ref()
            .map(|d| tape_failed_detail_lines(d, width, ctx))
            .unwrap_or_default(),
        TapeHead::Normal => tape_normal_detail_lines(screen, width, ctx),
    };
    frame.render_widget(Paragraph::new(lines), content_area);
}

/// The too-small screen (§5): below 60×24, the minimum and
/// `osi --log` -- every screen's own floor, not only the Tape's,
/// since nothing below it has room for the shared chrome at all.
const MIN_WIDTH: u16 = 60;
const MIN_HEIGHT: u16 = 24;

/// §5's own too-small screen (the design's `tape--toosmall` frame):
/// the `osi` word, the minimum each dimension needs and whether it's
/// met, the plain-language line naming both and `osi --log`, and a
/// bare `q Quit` key bar -- the only state that ever draws below the
/// shared 60×24 floor, on every screen.
fn draw_too_small(frame: &mut Frame, area: Rect, ctx: &DrawCtx) {
    let ok_style = Style::default().fg(ctx.theme.status_done);
    let err_style = Style::default().fg(ctx.theme.status_failed);
    let err_bold_style = err_style.add_modifier(Modifier::BOLD);
    let width_ok = area.width >= MIN_WIDTH;
    let height_ok = area.height >= MIN_HEIGHT;
    let dim_line = |label: &str, have: u16, need: u16, ok: bool| {
        Line::from(vec![
            Span::styled(
                format!("                 {label:<9}"),
                Style::default().fg(ctx.theme.fg_muted),
            ),
            Span::styled(
                format!("{have:<5}"),
                if ok { ok_style } else { err_bold_style },
            ),
            Span::styled("needs ", Style::default().fg(ctx.theme.fg_muted)),
            Span::styled(format!("{need:<4}"), Style::default().fg(ctx.theme.text)),
            Span::styled(
                if ok { "✓" } else { "✗" },
                if ok { ok_style } else { err_style },
            ),
        ])
    };
    let lines = vec![
        Line::from(""),
        Line::from(""),
        Line::from(""),
        Line::styled(
            "                            osi",
            Style::default()
                .fg(ctx.theme.accent_primary)
                .add_modifier(Modifier::BOLD),
        ),
        Line::from(""),
        Line::styled(
            "                     Terminal too small",
            Style::default()
                .fg(ctx.theme.text)
                .add_modifier(Modifier::BOLD),
        ),
        Line::from(""),
        dim_line("Width", area.width, MIN_WIDTH, width_ok),
        dim_line("Height", area.height, MIN_HEIGHT, height_ok),
        Line::from(""),
        Line::styled(
            format!("      osi needs {MIN_WIDTH} columns and {MIN_HEIGHT} rows · try osi --log"),
            Style::default().fg(ctx.theme.fg_muted),
        ),
    ];
    frame.render_widget(Paragraph::new(lines), area);
    if area.height > 0 {
        let bg = ctx.theme.statusbar_bg;
        let key_bar_area = Rect {
            x: area.x,
            y: area.y + area.height - 1,
            width: area.width,
            height: 1,
        };
        let pad = (area.width as usize).saturating_sub(2 + 5);
        let spans = vec![
            Span::styled(
                format!("{}q ", " ".repeat(pad)),
                Style::default()
                    .fg(ctx.theme.keyhint_key)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("Quit ", Style::default().fg(ctx.theme.keyhint_desc).bg(bg)),
        ];
        frame.render_widget(
            Paragraph::new(Line::from(spans)).style(Style::default().bg(bg)),
            key_bar_area,
        );
    }
}

/// Screen 0 (issue #678): the header band, the list (with its detail
/// pane at 120 columns and up), the status line and the key bar --
/// the shared chrome every screen will eventually carry; today only
/// the Tape draws it (§9).
#[allow(clippy::too_many_arguments)]
fn draw_tape(
    frame: &mut Frame,
    full_area: Rect,
    screen: &oscilloscope_core::screens::TapeScreen,
    header: &oscilloscope_core::render::Header,
    app: &mut App,
    ctx: &DrawCtx,
    icon_frame: u64,
    now: &str,
) {
    if full_area.width < MIN_WIDTH || full_area.height < MIN_HEIGHT {
        draw_too_small(frame, full_area, ctx);
        return;
    }
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(5),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(full_area);
    draw_tape_header_band(frame, sections[0], header, "TAPE", now, icon_frame, ctx);
    draw_tape_status_line(
        frame,
        sections[2],
        screen,
        header,
        full_area.width < 80,
        app.dialog != Dialog::None,
        ctx,
        now,
    );
    draw_key_bar(
        frame,
        sections[3],
        &tape_key_bar_items(full_area.width >= 80),
        ctx,
    );

    let body = sections[1];
    if app.tape_fullscreen_step && screen.selected.is_some() {
        draw_tape_detail(frame, body, screen, app, ctx);
        return;
    }
    // §5: the detail pane stays down to 100 columns, not just
    // 120 -- only below that does it drop to one pane.
    if full_area.width >= 100 {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(63), Constraint::Percentage(37)])
            .split(body);
        // §5: rows 24-29 drop preview rows first, before anything
        // else gives way, to leave more of a short terminal's own
        // height for the Now band and what's ahead of it.
        draw_tape_list(
            frame,
            cols[0],
            screen,
            header,
            app,
            false,
            full_area.height >= 30,
            ctx,
        );
        draw_tape_detail(frame, cols[1], screen, app, ctx);
    } else {
        draw_tape_list(
            frame,
            body,
            screen,
            header,
            app,
            full_area.width < 80,
            false,
            ctx,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_signal(
    frame: &mut Frame,
    full_area: Rect,
    screen: &oscilloscope_core::screens::SignalScreen,
    header: &oscilloscope_core::render::Header,
    app: &mut App,
    ctx: &DrawCtx,
    icon_frame: u64,
    now: &str,
) {
    if full_area.width < MIN_WIDTH || full_area.height < MIN_HEIGHT {
        draw_too_small(frame, full_area, ctx);
        return;
    }
    // §4: below 80 columns only the Tape is offered -- a resize (or
    // a stale `1` from a wider terminal) bounces straight back to
    // it rather than drawing a screen with no key left to leave it
    // from.
    if full_area.width < 80 {
        app.screen = oscilloscope_core::screens::Screen::Tape;
        return;
    }
    app.signal_now_col = screen.now_col;
    app.signal_has_step = screen.selected.is_some();
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(5),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(full_area);
    draw_tape_header_band(frame, sections[0], header, "SIGNAL", now, icon_frame, ctx);
    draw_signal_status_line(frame, sections[2], screen, header, ctx, now);
    draw_key_bar(
        frame,
        sections[3],
        &signal_key_bar_items(full_area.width >= 80),
        ctx,
    );

    let body = sections[1];
    if app.tape_fullscreen_step && screen.selected.is_some() {
        draw_signal_detail(frame, body, screen, ctx);
        return;
    }
    // §5: the full layout (chart beside/above the Now pane and
    // Readings) needs 120 columns and 30 rows both -- rows 24-29
    // drop Readings the same way columns 80-119 do, even at full
    // width.
    let wide = full_area.width >= 120 && full_area.height >= 30;
    let chart_min_rows = (SIGNAL_SENT_ROWS + 1 + SIGNAL_RECEIVED_ROWS) as u16 + 4;
    let now_pane_max = body.height.saturating_sub(chart_min_rows).max(3);
    let now_pane_rows = signal_now_pane_rows(screen, wide).min(now_pane_max);
    let rows_split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(chart_min_rows),
            Constraint::Length(now_pane_rows),
        ])
        .split(body);
    draw_signal_chart(frame, rows_split[0], screen, header, wide, ctx);
    if wide {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
            .split(rows_split[1]);
        draw_signal_now_pane(frame, cols[0], screen, ctx, true);
        draw_signal_readings(frame, cols[1], screen, ctx);
    } else {
        draw_signal_now_pane(frame, rows_split[1], screen, ctx, false);
    }
}

fn signal_key_bar_items(wide: bool) -> Vec<(&'static str, &'static str)> {
    let mut items = vec![
        ("←→", "Scrub"),
        ("+/-", "Zoom"),
        ("n", "Now"),
        ("enter", "Open"),
    ];
    if wide {
        items.push(("0-3", "Screens"));
    }
    items.push(("?", "Help"));
    items.push(("q", "Quit"));
    items
}

/// §3.2's status line: the most recently finished row, any depth.
fn draw_signal_status_line(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::screens::SignalScreen,
    header: &oscilloscope_core::render::Header,
    ctx: &DrawCtx,
    now: &str,
) {
    if let Some(spans) = shared_run_chrome_status_spans(&screen.head, header, ctx) {
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }
    let Some(last) = &screen.last_finished else {
        frame.render_widget(Paragraph::new(Line::from("")), area);
        return;
    };
    let glyph = tape_glyph(last.kind, last.status);
    let age = last
        .ts
        .as_deref()
        .and_then(|t| oscilloscope_core::observe::duration_seconds(t, now));
    let tokens = last
        .tokens
        .map(|(sent, received)| {
            format!(
                " \u{b7} \u{2191}{} \u{2193}{}",
                oscilloscope_core::format::format_tokens_thousands(sent),
                oscilloscope_core::format::format_tokens_thousands(received)
            )
        })
        .unwrap_or_default();
    let age_text = age
        .map(|a| {
            format!(
                " \u{b7} {} ago",
                oscilloscope_core::format::format_duration(a)
            )
        })
        .unwrap_or_default();
    let dim = Style::default().fg(ctx.theme.dim);
    let spans = vec![
        Span::styled(
            format!(" {glyph} "),
            tape_glyph_style(last.kind, last.status, ctx.theme),
        ),
        Span::styled(
            format!("{} finished ", last.label),
            Style::default().fg(ctx.theme.text),
        ),
        Span::styled(format!("{tokens}{age_text}").trim_start().to_string(), dim),
    ];
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The Now pane's own interior height (border included), before its
/// caller clamps it to whatever the chart can spare: the full tier
/// (120x30 and up) shows every row the Now pane has; the 80-119
/// tier shows the running rows alone.
fn signal_now_pane_rows(screen: &oscilloscope_core::screens::SignalScreen, wide: bool) -> u16 {
    let running = screen.now_pane.running.len();
    if !wide {
        return (running.max(1) + 2) as u16;
    }
    let mut content = running + screen.now_pane.waiting.len();
    if screen.now_pane.loop_summary.is_some() {
        content += 1;
    }
    content += 1; // the frame's own trailing blank row, after Next Signal
    if screen.now_pane.last_burst.is_some() {
        content += 1;
    }
    if screen.now_pane.next_signal.is_some() {
        content += 1;
    }
    (content.max(1) + 2) as u16
}

fn signal_title(screen: &oscilloscope_core::screens::SignalScreen, wide: bool) -> String {
    let column_text = if screen.column_ms >= 1000 {
        format!("{} s", screen.column_ms / 1000)
    } else {
        format!("{} ms", screen.column_ms)
    };
    match (&screen.model_label, wide) {
        (Some(model), true) => format!(
            "Signal \u{b7} Tokens to {model} \u{b7} Sent Above \u{b7} Received Below \u{b7} {column_text} per Column \u{b7} Fixed Scale"
        ),
        (None, true) => format!(
            "Signal \u{b7} Sent Above \u{b7} Received Below \u{b7} {column_text} per Column \u{b7} Fixed Scale"
        ),
        (_, false) => {
            "Signal \u{b7} Sent Above \u{b7} Received Below \u{b7} Fixed Scale".to_string()
        }
    }
}

/// `frac` in `[0, 1]`: one of the eight lower-anchored block glyphs
/// (a bar growing from this cell's own bottom edge, continuing the
/// solid fill below it), rounded to the nearest eighth.
fn signal_sent_partial_glyph(frac: f64) -> char {
    const GLYPHS: [char; 8] = [
        '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}',
        '\u{2588}',
    ];
    let level = (frac.clamp(0.0, 1.0) * 7.0).round() as usize;
    GLYPHS[level.min(7)]
}

/// `frac` in `(0, 1]`: Unicode names only two upper-anchored partial
/// blocks (`▔` an eighth, `▀` a half) -- a received bar's own
/// partial cell (always filled from its own top edge, continuing
/// the solid fill above it) rounds to the nearer of the two, or a
/// full block past three-quarters.
fn signal_received_partial_glyph(frac: f64) -> char {
    if frac >= 0.75 {
        '\u{2588}'
    } else if frac >= 0.3125 {
        '\u{2580}'
    } else {
        '\u{2594}'
    }
}

/// One column's own sent bar: `rows` cells, index 0 the topmost
/// (farthest from the axis), index `rows-1` adjacent to it.
fn signal_sent_column(value: u64, axis_top: u64, rows: usize) -> Vec<Option<char>> {
    let mut out = vec![None; rows];
    if value == 0 || axis_top == 0 || rows == 0 {
        return out;
    }
    let unit = axis_top as f64 / rows as f64;
    let full = ((value as f64 / unit).floor() as usize).min(rows);
    for i in 0..full {
        out[rows - 1 - i] = Some('\u{2588}');
    }
    let remainder = value as f64 - full as f64 * unit;
    if full < rows && remainder > 0.0 {
        out[rows - 1 - full] = Some(signal_sent_partial_glyph(remainder / unit));
    }
    out
}

/// One column's own received bar: `rows` cells, index 0 adjacent to
/// the axis, index `rows-1` the farthest from it.
fn signal_received_column(value: u64, axis_top: u64, rows: usize) -> Vec<Option<char>> {
    let mut out = vec![None; rows];
    if value == 0 || axis_top == 0 || rows == 0 {
        return out;
    }
    let unit = axis_top as f64 / rows as f64;
    let full = ((value as f64 / unit).floor() as usize).min(rows);
    for cell in out.iter_mut().take(full) {
        *cell = Some('\u{2588}');
    }
    let remainder = value as f64 - full as f64 * unit;
    if full < rows && remainder > 0.0 {
        out[full] = Some(signal_received_partial_glyph(remainder / unit));
    }
    out
}

const SIGNAL_SENT_ROWS: usize = 5;
const SIGNAL_RECEIVED_ROWS: usize = 2;

/// The visible window's own leftmost column: `focus_col` (the scrub
/// cursor, or Now while following it) stays on screen, with up to a
/// quarter of `data_width` still ahead of it once the trace has run
/// longer than the pane is wide -- a run that still fits inside
/// `data_width` columns never scrolls at all, column 0 stays the
/// left edge.
fn signal_window_start(focus_col: usize, data_width: usize) -> usize {
    focus_col.saturating_sub(data_width * 3 / 4)
}

fn draw_signal_chart(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::screens::SignalScreen,
    header: &oscilloscope_core::render::Header,
    wide: bool,
    ctx: &DrawCtx,
) {
    let border_color = ctx.theme.border_focused;
    let title = signal_title(screen, wide);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(ratatui::symbols::border::ROUNDED)
        .title(tape_block_title(&title, border_color, ctx))
        .border_style(Style::default().fg(border_color));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    fill_border_rows(frame, inner, border_color);

    let chart_rows = SIGNAL_SENT_ROWS + 1 + SIGNAL_RECEIVED_ROWS;
    if inner.height < chart_rows as u16 + 2 {
        return;
    }
    let axis_row_y = inner.y + SIGNAL_SENT_ROWS as u16;

    // The label field's own width: wide enough for the widest of the
    // four axis labels (`1,200 t`, past three digits on a long or
    // history-heavy run, not just `600 t`), plus the one blank
    // gutter column before the trace itself starts.
    let sent_top_label = oscilloscope_core::format::format_tokens_t(screen.sent_axis_top);
    let sent_half_label = oscilloscope_core::format::format_tokens_t(screen.sent_axis_top / 2);
    let received_top_label = oscilloscope_core::format::format_tokens_t(screen.received_axis_top);
    let label_width = [
        "0",
        sent_top_label.as_str(),
        sent_half_label.as_str(),
        received_top_label.as_str(),
    ]
    .iter()
    .map(|s| s.chars().count())
    .max()
    .unwrap_or(1) as u16;
    let gutter = label_width + 1;
    let data_x = inner.x + gutter;
    let data_width = (inner.width as usize).saturating_sub(gutter as usize);
    let buf = frame.buffer_mut();

    let dim = Style::default().fg(ctx.theme.dim);
    let border_style = Style::default().fg(border_color);
    // A label right-aligned in its own field: the padding before it
    // stays border-coloured, only the text itself is `fg.faint`
    // (from the frames: `80 t`, shorter than `600 t`, is not drawn
    // with a dim leading space).
    let axis_label = |buf: &mut ratatui::buffer::Buffer, y: u16, text: &str| {
        let pad = (label_width as usize).saturating_sub(text.chars().count());
        buf.set_string(inner.x, y, " ".repeat(pad), border_style);
        buf.set_string(inner.x + pad as u16, y, text, dim);
    };
    axis_label(buf, inner.y, &sent_top_label);
    axis_label(buf, inner.y + SIGNAL_SENT_ROWS as u16 / 2, &sent_half_label);
    axis_label(buf, axis_row_y, "0");
    buf.set_string(data_x - 1, axis_row_y, " ", dim);
    axis_label(buf, inner.y + chart_rows as u16 - 1, &received_top_label);

    let sent_style = Style::default().fg(ctx.theme.accent_primary);
    let received_style = Style::default().fg(ctx.theme.status_done);
    let dim_sent_style = sent_style.add_modifier(Modifier::DIM);
    let dim_received_style = received_style.add_modifier(Modifier::DIM);
    let now_sweep_style = Style::default()
        .fg(ctx.theme.accent_primary)
        .add_modifier(Modifier::BOLD);

    // The trace's own visible window: a run that outgrows the pane's
    // own width keeps scrolling so the scrub cursor (or Now, while
    // following it) never runs off the right edge -- every bucket,
    // mark, step row and seconds label below reads at `start_col +
    // col`, never the bare column index on its own.
    let now_col_abs = screen.now_col.unwrap_or(0);
    let focus_col = screen.cursor_col.unwrap_or(now_col_abs);
    let start_col = signal_window_start(focus_col, data_width);
    // `None` (no run started yet) must stay sweep-less, never fall
    // back to column 0 the way `now_col_abs`'s own window-math
    // default does.
    let sweep_col = screen
        .now_col
        .and_then(|c| c.checked_sub(start_col))
        .filter(|&c| c < data_width)
        .unwrap_or(usize::MAX);
    let cursor_col = screen
        .cursor_col
        .and_then(|c| c.checked_sub(start_col))
        .filter(|&c| c < data_width);

    // Two passes: first every column's own glyphs (if any), so the
    // second pass can tell each sent/received row's own blank cells
    // apart by whether they come before or after that row's own
    // first real glyph (from the frames: a row with two separate
    // bars stays accent/received-tinted across the gap between
    // them, not just at the bars themselves).
    let mut sent_glyphs = vec![vec![None; data_width]; SIGNAL_SENT_ROWS];
    let mut received_glyphs = vec![vec![None; data_width]; SIGNAL_RECEIVED_ROWS];
    let mut sent_live_cols = vec![false; data_width];
    let mut received_live_cols = vec![false; data_width];
    for col in 0..data_width {
        let bucket = screen.buckets.get(start_col + col);
        let (sent, sent_live) = bucket.map(|b| (b.sent, b.sent_live)).unwrap_or((0, false));
        let (received, received_live) = bucket
            .map(|b| (b.received, b.received_live))
            .unwrap_or((0, false));
        sent_live_cols[col] = sent_live;
        received_live_cols[col] = received_live;
        for (i, cell) in signal_sent_column(sent, screen.sent_axis_top, SIGNAL_SENT_ROWS)
            .into_iter()
            .enumerate()
        {
            sent_glyphs[i][col] = cell;
        }
        for (i, cell) in
            signal_received_column(received, screen.received_axis_top, SIGNAL_RECEIVED_ROWS)
                .into_iter()
                .enumerate()
        {
            received_glyphs[i][col] = cell;
        }
    }
    for (i, row) in sent_glyphs.iter().enumerate() {
        let labeled = i == 0 || i == SIGNAL_SENT_ROWS / 2;
        let first_filled = row.iter().position(Option::is_some).unwrap_or(usize::MAX);
        let y = inner.y + i as u16;
        let gutter_style = if labeled {
            dim
        } else {
            Style::default().fg(border_color)
        };
        buf.set_string(data_x - 1, y, " ", gutter_style);
        for col in 0..data_width {
            let x = data_x + col as u16;
            if col > sweep_col {
                buf.set_string(x, y, " ", now_sweep_style);
            } else if let Some(ch) = row[col] {
                // A filled cell keeps its own sent/live colour even
                // at the sweep -- only an empty one there draws the
                // sweep's own rule instead (a running prompt's live
                // estimate must still read `≈`, not the sweep's own
                // bold accent, once it lands in the Now column).
                let style = if sent_live_cols[col] {
                    dim_sent_style
                } else {
                    sent_style
                };
                buf.set_string(x, y, ch.to_string(), style);
            } else if col == sweep_col {
                buf.set_string(x, y, "\u{2503}", now_sweep_style);
            } else if col >= first_filled {
                buf.set_string(x, y, " ", sent_style);
            } else {
                let style = if labeled {
                    dim
                } else {
                    Style::default().fg(border_color)
                };
                buf.set_string(x, y, " ", style);
            }
        }
    }
    for (i, row) in received_glyphs.iter().enumerate() {
        let labeled = i == SIGNAL_RECEIVED_ROWS - 1;
        let first_filled = row.iter().position(Option::is_some).unwrap_or(usize::MAX);
        let y = axis_row_y + 1 + i as u16;
        let gutter_style = if labeled {
            dim
        } else {
            Style::default().fg(border_color)
        };
        buf.set_string(data_x - 1, y, " ", gutter_style);
        for col in 0..data_width {
            let x = data_x + col as u16;
            if col > sweep_col {
                buf.set_string(x, y, " ", now_sweep_style);
            } else if let Some(ch) = row[col] {
                let style = if received_live_cols[col] {
                    dim_received_style
                } else {
                    received_style
                };
                buf.set_string(x, y, ch.to_string(), style);
            } else if col == sweep_col {
                buf.set_string(x, y, "\u{2503}", now_sweep_style);
            } else if col >= first_filled {
                buf.set_string(x, y, " ", received_style);
            } else {
                let style = if labeled {
                    dim
                } else {
                    Style::default().fg(border_color)
                };
                buf.set_string(x, y, " ", style);
            }
        }
    }

    // The axis row: `─` for the past, `╌` for the plan ahead, the
    // run's own marks (`✓`/`◆`) over whichever it is.
    let marks: std::collections::BTreeMap<usize, oscilloscope_core::screens::SignalMark> =
        screen.marks.iter().copied().collect();
    for col in 0..data_width {
        let x = data_x + col as u16;
        let ahead = col > sweep_col;
        let (ch, style) = if col == sweep_col {
            ('\u{2503}', now_sweep_style)
        } else {
            match marks.get(&(start_col + col)) {
                Some(oscilloscope_core::screens::SignalMark::Done) => {
                    ('\u{2713}', Style::default().fg(ctx.theme.status_done))
                }
                Some(oscilloscope_core::screens::SignalMark::Decision) => (
                    '\u{25c6}',
                    Style::default()
                        .fg(ctx.theme.accent_secondary)
                        .add_modifier(Modifier::BOLD),
                ),
                None if ahead => ('\u{254c}', dim),
                None => ('\u{2500}', Style::default().fg(ctx.theme.border)),
            }
        };
        buf.set_string(x, axis_row_y, ch.to_string(), style);
    }

    // The scrub cursor, once it differs from Now: a distinct
    // selection tint down the whole chart's own height, never the
    // sweep's own bold accent -- the two stay visually apart even
    // when only one column separates them.
    if let Some(c) = cursor_col {
        if c != sweep_col {
            let cursor_rect = Rect {
                x: data_x + c as u16,
                y: inner.y,
                width: 1,
                height: chart_rows as u16,
            };
            buf.set_style(cursor_rect, Style::default().bg(ctx.theme.selection_bg));
        }
    }

    draw_signal_seconds_row(
        frame, inner, data_x, data_width, start_col, screen, wide, ctx,
    );
    let step_rows_y = axis_row_y + 1 + SIGNAL_RECEIVED_ROWS as u16 + 1;
    let step_rows_available = (inner.y + inner.height)
        .saturating_sub(step_rows_y)
        .saturating_sub(2); // the separator and Totals rows
    draw_signal_step_rows(
        frame,
        Rect {
            x: inner.x,
            y: step_rows_y,
            width: inner.width,
            height: step_rows_available,
        },
        data_x,
        start_col,
        screen,
        ctx,
    );
    let totals_y = inner.y + inner.height - 1;
    let sep_y = totals_y - 1;
    frame.buffer_mut().set_string(
        inner.x,
        sep_y,
        "\u{2500}".repeat(inner.width as usize),
        Style::default().fg(ctx.theme.border),
    );
    draw_totals_row(
        frame,
        Rect {
            x: inner.x,
            y: totals_y,
            width: inner.width,
            height: 1,
        },
        header,
        false,
        None,
        screen.usual_so_far_tokens,
        Some(border_color),
        false,
        ctx,
    );
}

/// The seconds-mark row: ticks spaced at least about six columns
/// apart (`cols_per_second`'s own interval at the default 100 ms per
/// column, a wider few-second step once zoomed out enough that a
/// one-second step would overlap its own labels), replaced at the
/// Now column by its own label and, at the full width tier, `Ahead
/// · plan order · usually ends <clock>` after it.
#[allow(clippy::too_many_arguments)]
fn draw_signal_seconds_row(
    frame: &mut Frame,
    inner: Rect,
    data_x: u16,
    data_width: usize,
    start_col: usize,
    screen: &oscilloscope_core::screens::SignalScreen,
    wide: bool,
    ctx: &DrawCtx,
) {
    let y = inner.y + SIGNAL_SENT_ROWS as u16 + 1 + SIGNAL_RECEIVED_ROWS as u16;
    let dim = Style::default().fg(ctx.theme.dim);
    // The whole data region starts dim (from the frames: the design
    // text never says so, but every cell past the gutter on this
    // row, ticks and the gaps between them alike, reads `fg.faint`
    // in the frame, not a plain border-coloured blank).
    frame
        .buffer_mut()
        .set_string(data_x, y, " ".repeat(data_width), dim);

    let now_label_col = screen
        .now_col
        .and_then(|c| c.checked_sub(start_col))
        .filter(|&c| c < data_width)
        .map(|c| c.saturating_sub(1));
    let now_x = now_label_col.map(|c| data_x + c as u16);
    let now_style = Style::default()
        .fg(ctx.theme.accent_primary)
        .add_modifier(Modifier::BOLD);
    let now_label = now_x.map(|_| {
        let clock = screen
            .now_ts
            .as_deref()
            .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone))
            .unwrap_or_default();
        let elapsed = screen
            .now_elapsed_s
            .map(oscilloscope_core::format::format_duration)
            .unwrap_or_default();
        format!("Now {clock} \u{b7} {elapsed}  ")
    });
    let ahead_x = match (now_x, &now_label) {
        (Some(x), Some(l)) => Some(x + l.chars().count() as u16),
        _ => None,
    };
    let ahead_label = ahead_x.map(|_| {
        if !wide {
            "Ahead \u{25b8}".to_string()
        } else {
            match screen
                .ahead_eta_ts
                .as_deref()
                .and_then(|eta| oscilloscope_core::format::format_clock_time(eta, ctx.zone))
            {
                Some(eta_clock) => {
                    format!("Ahead \u{b7} plan order \u{b7} usually ends {eta_clock}")
                }
                None => "Ahead".to_string(),
            }
        }
    });
    let occupied_end = match (ahead_x, &ahead_label) {
        (Some(x), Some(l)) => x + l.chars().count() as u16,
        _ => now_label_col.map(|c| data_x + c as u16).unwrap_or(data_x),
    };

    let cols_per_second = (1000.0 / screen.column_ms.max(1) as f64).round().max(1.0) as u64;
    // At least six columns between tick labels: at 100 ms per
    // column (ten columns a second) that's every second, same as
    // always; past that, whichever whole number of seconds first
    // keeps consecutive labels from overlapping.
    let seconds_step = (6u64).div_ceil(cols_per_second).max(1);
    let mut seconds = 0u64;
    loop {
        let abs_col = seconds * cols_per_second;
        if abs_col < start_col as u64 {
            seconds += seconds_step;
            continue;
        }
        let col = (abs_col - start_col as u64) as usize;
        if col >= data_width {
            break;
        }
        let x = data_x + col as u16;
        let text = format!("{seconds}s");
        let text_end = x + text.chars().count() as u16;
        let before_now = now_x.is_none_or(|n| text_end <= n);
        let after_everything = wide && x >= occupied_end;
        if before_now {
            frame.buffer_mut().set_string(x, y, &text, dim);
        } else if after_everything {
            frame.buffer_mut().set_string(x, y, &text, dim);
            break;
        }
        seconds += seconds_step;
    }

    if let (Some(x), Some(label)) = (now_x, &now_label) {
        frame.buffer_mut().set_string(x, y, label, now_style);
    }
    if let (Some(x), Some(label)) = (ahead_x, &ahead_label) {
        frame.buffer_mut().set_string(x, y, label, dim);
    }
}

/// One step row's own styled fragments (§2.2's "step rows under the
/// axis"), already positioned left to right -- the caller draws each
/// in turn, clipping to the pane's own right edge. Only ever called
/// for a row whose own start column (`row.col`) is already inside
/// the trace's own visible window; a tool bar that started before
/// it but is still running draws its own plain continuation instead
/// ([`draw_signal_step_rows`]'s own `clipped` branch).
fn signal_step_row_spans(
    row: &oscilloscope_core::screens::SignalStepRow,
    screen: &oscilloscope_core::screens::SignalScreen,
    is_last: bool,
    ctx: &DrawCtx,
) -> Vec<(String, Style)> {
    use oscilloscope_core::screens::SignalStepKind;
    let muted = Style::default().fg(ctx.theme.fg_muted);
    let faint = Style::default().fg(ctx.theme.dim);
    let default = Style::default().fg(ctx.theme.text);
    let bold = default.add_modifier(Modifier::BOLD);
    let mut spans = Vec::new();
    match &row.kind {
        SignalStepKind::Prompt { rest } => {
            let glyph = tape_glyph(row.row_kind, row.status);
            spans.push((
                format!("{glyph} {} ", row.label),
                tape_glyph_style(row.row_kind, row.status, ctx.theme),
            ));
            spans.push((rest.clone(), muted));
        }
        SignalStepKind::Tool { end_col, text } => {
            let style = tape_glyph_style(row.row_kind, row.status, ctx.theme);
            let prefix = format!("\u{2501} {text} ");
            let used_cols = row.col + prefix.chars().count();
            let fill = end_col.saturating_add(1).saturating_sub(used_cols);
            spans.push((format!("{prefix}{}", "\u{2501}".repeat(fill)), style));
        }
        SignalStepKind::Decision { released, suffix } => {
            spans.push((
                format!("\u{25c6} {} \u{b7} ", row.label),
                Style::default()
                    .fg(ctx.theme.accent_secondary)
                    .add_modifier(Modifier::BOLD),
            ));
            for (i, item) in released.iter().enumerate() {
                if i > 0 {
                    spans.push((" \u{b7} ".to_string(), muted));
                }
                spans.push((format!("{} ", item.label), default));
                if let Some(tokens) = item.tokens {
                    spans.push((
                        format!(
                            "{} t ",
                            oscilloscope_core::format::format_tokens_thousands(tokens)
                        ),
                        bold,
                    ));
                }
                if let Some(dev) = item.dev_pct {
                    spans.push((
                        format!("{dev:+.0}% "),
                        tape_deviation_style(dev, ctx.theme).add_modifier(Modifier::BOLD),
                    ));
                }
            }
            if !suffix.is_empty() {
                spans.push((suffix.clone(), muted));
            }
        }
        SignalStepKind::Loop { status_text } => {
            spans.push((
                format!("\u{27f3} {} ", row.label),
                Style::default()
                    .fg(ctx.theme.accent_primary)
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push((format!("{status_text}  "), bold));
            if is_last {
                if let Some(suffix) = &screen.ahead_suffix {
                    spans.push((suffix.clone(), faint));
                }
            }
        }
    }
    if is_last && !matches!(row.kind, SignalStepKind::Loop { .. }) {
        if let Some(suffix) = &screen.ahead_suffix {
            spans.push(("  ".to_string(), faint));
            spans.push((suffix.clone(), faint));
        }
    }
    spans
}

/// A step row visible in the trace's own current window: `visible_col`
/// is where it actually starts drawing (never before the window's
/// own left edge), `clipped` when that's later than the row's own
/// real start column -- a tool bar already running when the window
/// scrolled past its own start.
struct SignalVisibleStepRow<'a> {
    row: &'a oscilloscope_core::screens::SignalStepRow,
    orig_index: usize,
    visible_col: usize,
    clipped: bool,
}

fn draw_signal_step_rows(
    frame: &mut Frame,
    area: Rect,
    data_x: u16,
    start_col: usize,
    screen: &oscilloscope_core::screens::SignalScreen,
    ctx: &DrawCtx,
) {
    use oscilloscope_core::screens::SignalStepKind;
    if area.height == 0 {
        return;
    }
    let last_idx = screen.step_rows.len().saturating_sub(1);
    let right_edge = area.x + area.width;

    // Scrolled off the window's own left edge entirely: a plain
    // point-in-time row (a finished prompt, a decision, a loop's own
    // heading) just isn't there any more; a tool's own bar keeps
    // showing, clipped to the window's own edge, for as long as it
    // is still running past it.
    let mut visible: Vec<SignalVisibleStepRow> = Vec::new();
    for (i, row) in screen.step_rows.iter().enumerate() {
        match &row.kind {
            SignalStepKind::Tool { end_col, .. } => {
                if *end_col < start_col {
                    continue;
                }
                let clipped = row.col < start_col;
                visible.push(SignalVisibleStepRow {
                    row,
                    orig_index: i,
                    visible_col: row.col.max(start_col),
                    clipped,
                });
            }
            _ => {
                if row.col < start_col {
                    continue;
                }
                visible.push(SignalVisibleStepRow {
                    row,
                    orig_index: i,
                    visible_col: row.col,
                    clipped: false,
                });
            }
        }
    }
    // More rows fit in the window than fit the pane's own height:
    // the most recent ones win, the same way the trace itself keeps
    // whichever end is actually in view on screen.
    let shown_from = visible.len().saturating_sub(area.height as usize);
    for (slot, v) in visible[shown_from..].iter().enumerate() {
        let y = area.y + slot as u16;
        let mut x = data_x + (v.visible_col - start_col) as u16;
        if x >= right_edge {
            continue;
        }
        let spans: Vec<(String, Style)> = if v.clipped {
            match &v.row.kind {
                SignalStepKind::Tool { end_col, .. } => {
                    let style = tape_glyph_style(v.row.row_kind, v.row.status, ctx.theme);
                    let fill = end_col.saturating_add(1).saturating_sub(start_col);
                    vec![("\u{2501}".repeat(fill), style)]
                }
                _ => Vec::new(),
            }
        } else {
            signal_step_row_spans(v.row, screen, v.orig_index == last_idx, ctx)
        };
        let mut last_style = None;
        for (text, style) in spans {
            if x >= right_edge {
                break;
            }
            let budget = (right_edge - x) as usize;
            // A plain hard cut, not truncate_text's own ellipsis
            // marker (from the frames: the design text names no
            // such marker here, and the frame's own rows run right
            // up to the border with a clean cut instead).
            let clipped: String = text.chars().take(budget).collect();
            let width = clipped.chars().count() as u16;
            frame.buffer_mut().set_string(x, y, &clipped, style);
            x += width;
            last_style = Some(style);
        }
        // The row's own last span's style continues through its
        // own trailing blanks to the pane's right edge (from the
        // frames: the design text never says this, the frame draws
        // every step row's own tail this way).
        if let Some(style) = last_style {
            if x < right_edge {
                let fill = (right_edge - x) as usize;
                frame.buffer_mut().set_string(x, y, " ".repeat(fill), style);
            }
        }
    }
}

/// Pads `spans` out to `width` columns with its own last span's
/// style (from the frames: the design text never says this, but
/// every row in the Now pane and Readings draws its own trailing
/// blanks in whichever style ended the row, not a plain
/// border-coloured blank).
fn pad_line(spans: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let style = spans.last().map(|s| s.style).unwrap_or_default();
    let mut spans = spans;
    if used < width {
        spans.push(Span::styled(" ".repeat(width - used), style));
    }
    Line::from(spans)
}

fn draw_signal_now_pane(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::screens::SignalScreen,
    ctx: &DrawCtx,
    wide: bool,
) {
    let border_color = ctx.theme.border;
    let title = screen
        .now_pane
        .slots_text
        .as_deref()
        .map(|s| format!("Now \u{b7} {s}"))
        .unwrap_or_else(|| "Now".to_string());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(ratatui::symbols::border::ROUNDED)
        .title(tape_block_title(&title, border_color, ctx))
        .border_style(Style::default().fg(border_color));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    fill_border_rows(frame, inner, border_color);
    let muted = Style::default().fg(ctx.theme.fg_muted);
    let faint = Style::default().fg(ctx.theme.dim);
    let running_style = Style::default()
        .fg(ctx.theme.accent_primary)
        .add_modifier(Modifier::BOLD);
    let bold = Style::default()
        .fg(ctx.theme.text)
        .add_modifier(Modifier::BOLD);
    let mut y = inner.y;
    let bottom = inner.y + inner.height;
    for row in &screen.now_pane.running {
        if y >= bottom {
            return;
        }
        let slot = row
            .slot
            .map(|s| format!(" \u{b7} slot {s}"))
            .unwrap_or_default();
        let line = pad_line(
            vec![
                Span::styled("\u{25cf} ", running_style),
                Span::styled(format!("{:<26}", row.label), bold),
                Span::styled(format!("{}{slot}", row.what), muted),
            ],
            (inner.width - 1) as usize,
        );
        frame
            .buffer_mut()
            .set_line(inner.x + 1, y, &line, inner.width - 1);
        y += 1;
    }
    if !wide {
        return;
    }
    for (label, text) in &screen.now_pane.waiting {
        if y >= bottom {
            return;
        }
        let line = pad_line(
            vec![
                Span::styled(format!("\u{b7} {label:<26}"), faint),
                Span::styled(text.clone(), faint),
            ],
            (inner.width - 1) as usize,
        );
        frame
            .buffer_mut()
            .set_line(inner.x + 1, y, &line, inner.width - 1);
        y += 1;
    }
    if let Some((label, status_text)) = &screen.now_pane.loop_summary {
        if y >= bottom {
            return;
        }
        let accent_secondary_bold = Style::default()
            .fg(ctx.theme.accent_secondary)
            .add_modifier(Modifier::BOLD);
        let line = pad_line(
            vec![
                Span::styled(format!("\u{27f3} {label}  "), accent_secondary_bold),
                Span::styled(status_text.clone(), Style::default().fg(ctx.theme.text)),
            ],
            (inner.width - 1) as usize,
        );
        frame
            .buffer_mut()
            .set_line(inner.x + 1, y, &line, inner.width - 1);
        y += 1;
    }
    if let Some(text) = &screen.now_pane.last_burst {
        if y < bottom {
            let (age, rest) = text.split_once(" \u{b7} ").unwrap_or((text.as_str(), ""));
            let line = pad_line(
                vec![
                    Span::styled("Last Burst  ", muted),
                    Span::styled(format!("{age} "), Style::default().fg(ctx.theme.text)),
                    Span::styled(format!("\u{b7} {rest}"), muted),
                ],
                (inner.width - 1) as usize,
            );
            frame
                .buffer_mut()
                .set_line(inner.x + 1, y, &line, inner.width - 1);
            y += 1;
        }
    }
    if let Some(text) = &screen.now_pane.next_signal {
        if y < bottom {
            let (label, rest) = text.split_once(" \u{b7} ").unwrap_or((text.as_str(), ""));
            let line = pad_line(
                vec![
                    Span::styled("Next Signal ", muted),
                    Span::styled(format!("{label} "), Style::default().fg(ctx.theme.text)),
                    Span::styled(format!("\u{b7} {rest}"), muted),
                ],
                (inner.width - 1) as usize,
            );
            frame
                .buffer_mut()
                .set_line(inner.x + 1, y, &line, inner.width - 1);
        }
    }
}

fn draw_signal_readings(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::screens::SignalScreen,
    ctx: &DrawCtx,
) {
    let border_color = ctx.theme.border;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(ratatui::symbols::border::ROUNDED)
        .title(tape_block_title("Readings", border_color, ctx))
        .border_style(Style::default().fg(border_color));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    fill_border_rows(frame, inner, border_color);
    let muted = Style::default().fg(ctx.theme.fg_muted);
    let default = Style::default().fg(ctx.theme.text);
    let bold = default.add_modifier(Modifier::BOLD);
    let readings = &screen.readings;
    let line_width = (inner.width - 1) as usize;
    let mut lines = vec![
        pad_line(
            vec![
                Span::styled("Sent     ", muted),
                Span::styled(
                    format!(
                        "{} t",
                        oscilloscope_core::format::format_tokens_thousands(readings.sent)
                    ),
                    default,
                ),
            ],
            line_width,
        ),
        pad_line(
            vec![
                Span::styled("Received ", muted),
                Span::styled(
                    format!(
                        "{} t",
                        oscilloscope_core::format::format_tokens_thousands(readings.received)
                    ),
                    default,
                ),
            ],
            line_width,
        ),
    ];
    lines.push(pad_line(
        vec![
            Span::styled("Ratio    ", muted),
            Span::styled(
                readings
                    .ratio
                    .map(|r| format!("{r:.1} : 1"))
                    .unwrap_or_else(|| "\u{2014}".to_string()),
                default,
            ),
        ],
        line_width,
    ));
    lines.push(pad_line(
        match &readings.peak {
            Some((tokens, ts)) => vec![
                Span::styled("Peak     ", muted),
                Span::styled(
                    format!(
                        "{} t ",
                        oscilloscope_core::format::format_tokens_thousands(*tokens)
                    ),
                    bold,
                ),
                Span::styled(
                    format!(
                        "at {}",
                        oscilloscope_core::format::format_clock_time(ts, ctx.zone)
                            .unwrap_or_default()
                    ),
                    muted,
                ),
            ],
            None => vec![
                Span::styled("Peak     ", muted),
                Span::styled("\u{2014}", muted),
            ],
        },
        line_width,
    ));
    lines.push(pad_line(
        match &readings.largest {
            Some((label, tokens)) => vec![
                Span::styled("Largest  ", muted),
                Span::styled(
                    format!(
                        "{label} {} t",
                        oscilloscope_core::format::format_tokens_thousands(*tokens)
                    ),
                    default,
                ),
            ],
            None => vec![
                Span::styled("Largest  ", muted),
                Span::styled("\u{2014}", muted),
            ],
        },
        line_width,
    ));
    lines.push(pad_line(
        match readings.quiet_s {
            Some(s) => vec![
                Span::styled("Quiet    ", muted),
                Span::styled(
                    format!(
                        "{} since the last burst",
                        oscilloscope_core::format::format_duration(s)
                    ),
                    default,
                ),
            ],
            None => vec![
                Span::styled("Quiet    ", muted),
                Span::styled("\u{2014}", muted),
            ],
        },
        line_width,
    ));
    frame.render_widget(
        Paragraph::new(lines),
        Rect {
            x: inner.x + 1,
            y: inner.y,
            width: inner.width - 1,
            height: inner.height,
        },
    );
}

/// `enter`'s own full-screen step view (§4: "opens the step under
/// the cursor"), reusing the same [`FocusedStep`] the Tape's own
/// detail pane builds theirs from -- a simpler block than the
/// Tape's own (no history/share lines, since Signal's own model
/// carries none of that per row), good enough to read a step's
/// prompt and reply in full without leaving the screen.
fn draw_signal_detail(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::screens::SignalScreen,
    ctx: &DrawCtx,
) {
    let Some(step) = &screen.selected else {
        return;
    };
    let border_color = ctx.theme.border_focused;
    let title = format!("{} \u{b7} Selected", step.breadcrumb);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(ratatui::symbols::border::ROUNDED)
        .title(tape_block_title(&title, border_color, ctx))
        .border_style(Style::default().fg(border_color));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    fill_border_rows(frame, inner, border_color);
    let muted = Style::default().fg(ctx.theme.fg_muted);
    let default = Style::default().fg(ctx.theme.text);
    let mut lines = vec![Line::from(vec![
        Span::styled(" Status  ", muted),
        Span::styled(
            format!(
                "{} ",
                oscilloscope_core::format::format_duration(step.elapsed_s.unwrap_or(0.0))
            ),
            default,
        ),
    ])];
    if let oscilloscope_core::screens::FocusDetail::Prompt { snippet, .. } = &step.detail {
        lines.push(Line::from(""));
        lines.push(Line::styled(" Reply", muted));
        if let Some(snippet) = snippet {
            for line in snippet.lines() {
                lines.push(Line::styled(format!(" {line}"), default));
            }
        }
    }
    frame.render_widget(
        Paragraph::new(lines),
        Rect {
            x: inner.x + 1,
            y: inner.y,
            width: inner.width.saturating_sub(1),
            height: inner.height,
        },
    );
}

/// A running (or just-stopped) agent's own `meta.live` summary
/// (issue #637), shared by every pane that shows one: the Flow
/// screen's own selected pane and the Tape's detail pane -- model/
/// engine/turns, tokens and cost, recent tool activity (from the
/// transcript when one is tailed, else `meta.live` itself), context
/// window usage, and files/lines changed. `show_doing_header` labels
/// the tool-activity block its own `doing:` header, which a pane
/// already titled that way (the Flow screen's own) leaves off.
#[allow(clippy::too_many_arguments)]
fn agent_live_lines(
    live: &oscilloscope_core::model::AgentLive,
    transcript_view: Option<&oscilloscope_core::transcript::TranscriptView>,
    ctx: &DrawCtx,
    show_doing_header: bool,
    stale: bool,
    current_tool_elapsed_s: Option<f64>,
    now: &str,
) -> Vec<Line<'static>> {
    use oscilloscope_core::format::format_tokens;
    let dim = Style::default().fg(ctx.theme.dim);
    let mut lines = Vec::new();

    let mut header = Vec::new();
    if let Some(engine) = &live.engine {
        header.push(engine.clone());
    }
    if let Some(model) = &live.model {
        header.push(model.clone());
    }
    if let Some(turns) = live.turns {
        header.push(format!("turn {turns}"));
    }
    if let Some(tool_calls) = live.tool_calls {
        header.push(format!("{tool_calls} tools"));
    }
    if !header.is_empty() {
        lines.push(Line::from(header.join(" · ")));
    }

    let sent = live
        .tokens_sent
        .map(format_tokens)
        .unwrap_or_else(|| "—".to_string());
    let received = live
        .tokens_received
        .map(format_tokens)
        .unwrap_or_else(|| "—".to_string());
    let cost = live
        .cost_usd
        .map(|c| format!("${c:.2}"))
        .unwrap_or_else(|| "$—".to_string());
    lines.push(Line::from(format!("↑{sent} ↓{received} · {cost}")));

    // The tool activity comes right after the headline
    // figures, before context/files/tests -- in Flow's own selected
    // pane, nested under its own `doing:` header and a tighter height
    // budget than the overview's now pane has, this is the one part
    // of the summary that would otherwise be the first thing clipped
    // off the bottom.
    match transcript_view.filter(|v| !v.calls.is_empty()) {
        Some(view) => {
            let strip: String = view
                .calls
                .iter()
                .rev()
                .take(32)
                .rev()
                .map(|c| oscilloscope_core::transcript::tool_glyph(&c.tool))
                .collect();
            lines.push(Line::styled(strip, dim));
            if show_doing_header {
                lines.push(Line::styled("doing:", dim));
            }
            for call in view.calls.iter().rev().take(3).rev() {
                let clock = call
                    .start_t
                    .as_deref()
                    .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone))
                    .map(|c| format!("{c} "))
                    .unwrap_or_default();
                let mut text = format!("{clock}› {}", call.tool);
                if let Some(target) = &call.target {
                    text.push(' ');
                    text.push_str(target);
                }
                match call.ok {
                    Some(true) => text.push_str(" ✓"),
                    Some(false) => text.push_str(" ✗"),
                    // Issue #647: a stopped session's own transcript
                    // can end mid-call, with no later `tool_start` to
                    // ever mark it `call.stopped` -- `stale` alone
                    // (the session itself has stopped) is enough to
                    // call this unfinished, not still running.
                    None if call.stopped || stale => text.push_str(" stopped"),
                    // Issue #647/#647 follow-up: the session's own
                    // elapsed is shown next to `since`, but each call
                    // in this list is its own, possibly concurrent,
                    // tool call -- its own elapsed is `now` minus its
                    // own `start_t`, not the session's single `meta.
                    // live.current_tool` time, which two calls
                    // running at once would otherwise both show.
                    None => match call
                        .start_t
                        .as_deref()
                        .and_then(|t| oscilloscope_core::observe::duration_seconds(t, now))
                    {
                        Some(elapsed) => text.push_str(&format!(
                            " running {}",
                            oscilloscope_core::format::format_duration(elapsed)
                        )),
                        None => text.push_str(" running"),
                    },
                }
                if call.lines_added.is_some() || call.lines_removed.is_some() {
                    text.push_str(&format!(
                        " +{} −{}",
                        call.lines_added.unwrap_or(0),
                        call.lines_removed.unwrap_or(0)
                    ));
                }
                lines.push(Line::from(text));
            }
        }
        None => {
            // Issue #637: without a transcript to read calls from at
            // all (not yet tailed, or an engine that never wrote
            // one), the same activity is shown from `meta.live`'s own
            // `tools`/`current_tool` instead -- names and counts
            // only, since that's all either field carries.
            if !live.tools.is_empty() {
                let counts = live
                    .tools
                    .iter()
                    .map(|(name, usage)| format!("{name} ×{}", usage.calls))
                    .collect::<Vec<_>>()
                    .join(" · ");
                if show_doing_header {
                    lines.push(Line::styled("doing:", dim));
                }
                lines.push(Line::from(counts));
            }
            if let Some(current) = &live.current_tool {
                // Issue #647: a stopped agent's last tool was never
                // finished, so it never actually matched the running
                // label in the first place -- it is the last thing
                // the session was doing, not something still running.
                if stale {
                    lines.push(Line::from(format!("last: {}", current.name)));
                } else {
                    match current_tool_elapsed_s {
                        Some(elapsed) => lines.push(Line::from(format!(
                            "running: {} · {}",
                            current.name,
                            oscilloscope_core::format::format_duration(elapsed)
                        ))),
                        None => lines.push(Line::from(format!("running: {}", current.name))),
                    }
                }
            }
        }
    }

    if let Some(context_tokens) = live.context_tokens {
        match live.context_window {
            Some(window) => {
                lines.push(Line::from(format!(
                    "context {} / {}",
                    format_tokens(context_tokens),
                    format_tokens(window)
                )));
                lines.push(Line::styled(bar(context_tokens, window, 20), dim));
            }
            None => {
                lines.push(Line::from(format!(
                    "context {}",
                    format_tokens(context_tokens)
                )));
            }
        }
    }

    let mut activity = Vec::new();
    if live.files_changed.is_some() || live.lines_added.is_some() || live.lines_removed.is_some() {
        if let Some(files) = live.files_changed {
            activity.push(format!("files {files}"));
        }
        if live.lines_added.is_some() || live.lines_removed.is_some() {
            activity.push(format!(
                "+{} −{}",
                live.lines_added.unwrap_or(0),
                live.lines_removed.unwrap_or(0)
            ));
        }
    }
    if let Some(tests) = transcript_view.and_then(|v| v.tests) {
        use oscilloscope_core::transcript::TestOutcome;
        activity.push(match tests {
            TestOutcome::Running => "tests …".to_string(),
            TestOutcome::Ok => "tests ✓".to_string(),
            TestOutcome::Failed => "tests ✗".to_string(),
        });
    }
    if !activity.is_empty() {
        lines.push(Line::from(activity.join(" · ")));
    }

    lines
}

/// Truncates a line's own displayed text to `max_chars`, ending in
/// `…` rather than letting the terminal clip it mid-word (DESIGN.md:
/// "long values truncate with …"), styling kept span by span.
fn truncate_line(line: Line<'static>, max_chars: usize) -> Line<'static> {
    let total: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
    if total <= max_chars {
        return line;
    }
    if max_chars == 0 {
        return Line::from("");
    }
    let mut budget = max_chars.saturating_sub(1);
    let mut spans = Vec::new();
    for span in line.spans {
        if budget == 0 {
            break;
        }
        let count = span.content.chars().count();
        if count <= budget {
            budget -= count;
            spans.push(span);
        } else {
            let truncated: String = span.content.chars().take(budget).collect();
            spans.push(Span::styled(truncated, span.style));
            budget = 0;
        }
    }
    spans.push(Span::raw("…"));
    Line::from(spans)
}

fn truncate_lines(lines: Vec<Line<'static>>, max_chars: usize) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .map(|l| truncate_line(l, max_chars))
        .collect()
}

#[cfg(test)]
mod truncate_tests {
    use super::*;

    /// A card's own long value ends in `…` once it overflows its own
    /// width (DESIGN.md: "long values truncate with …"), rather than
    /// being clipped mid-word the way the terminal itself would clip
    /// an over-wide line with no wrap.
    #[test]
    fn truncate_line_ends_in_an_ellipsis_once_it_overflows() {
        let line = Line::from("a somewhat long value that will not fit");
        let truncated = truncate_line(line, 10);
        let text: String = truncated.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text.chars().count(), 10);
        assert!(text.ends_with('…'), "{text:?}");
        assert_eq!(text, "a somewha…");
    }

    #[test]
    fn truncate_line_leaves_a_line_that_already_fits_untouched() {
        let line = Line::from("short");
        let truncated = truncate_line(line, 10);
        let text: String = truncated.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "short");
    }

    #[test]
    fn truncate_line_keeps_each_spans_own_style_up_to_the_cut() {
        let line = Line::from(vec![
            Span::styled("abc", Style::default().fg(Color::Red)),
            Span::styled("defgh", Style::default().fg(Color::Blue)),
        ]);
        let truncated = truncate_line(line, 5);
        assert_eq!(truncated.spans[0].content.as_ref(), "abc");
        assert_eq!(truncated.spans[0].style.fg, Some(Color::Red));
        assert_eq!(truncated.spans[1].content.as_ref(), "d");
        assert_eq!(truncated.spans[1].style.fg, Some(Color::Blue));
        assert_eq!(truncated.spans[2].content.as_ref(), "…");
    }
}

/// `print_line`/`format_log_line`'s own clock prefix, `HH:MM:SS `
/// (`main.rs::Clock::format` plus the one space that always follows
/// it) -- always this many bytes, since the clock is always exactly
/// two ASCII digits per field.
const LOG_CLOCK_PREFIX_LEN: usize = 9;

/// Whether `line` (already clock-prefixed) is a report's own line
/// (#610): the leading `▸` right after the clock prefix, written
/// only by `diff::report_line` -- no other line this crate ever
/// builds starts with it, so a front end can tell the two apart from
/// the line's own text alone, with no separate field needed.
fn is_report_log_line(line: &str) -> bool {
    line.get(LOG_CLOCK_PREFIX_LEN..)
        .is_some_and(|rest| rest.starts_with('▸'))
}

fn is_warning_report_log_line(line: &str) -> bool {
    line.get(LOG_CLOCK_PREFIX_LEN..)
        .is_some_and(|rest| rest.starts_with("▸ warning: "))
}

fn status_color(kind: StatusKind, theme: &Theme) -> Color {
    theme.status_color(kind)
}

/// Cuts `label` to `max` characters, with a trailing `…` when it did
/// (§3.8's own row-text ellipsis).
fn truncate_label(label: &str, max: usize) -> String {
    let count = label.chars().count();
    if count <= max {
        label.to_string()
    } else {
        format!(
            "{}…",
            label
                .chars()
                .take(max.saturating_sub(1))
                .collect::<String>()
        )
    }
}

/// Issue #637's Cost section: "the per-turn strip comes from `turn_
/// tokens`" -- the last `width` values, oldest first, each scaled to
/// one of eight block heights relative to the largest value in that
/// same window (never the whole session's own history), so a single
/// unusually large turn doesn't flatten every other bar in sight.
fn turn_tokens_sparkline(values: &[u64], width: usize) -> String {
    const LEVELS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if values.is_empty() || width == 0 {
        return String::new();
    }
    let slice = if values.len() > width {
        &values[values.len() - width..]
    } else {
        values
    };
    let max = slice.iter().copied().max().unwrap_or(0);
    slice
        .iter()
        .map(|&v| {
            if max == 0 {
                LEVELS[0]
            } else {
                let idx = ((v as f64 / max as f64) * (LEVELS.len() - 1) as f64).round() as usize;
                LEVELS[idx.min(LEVELS.len() - 1)]
            }
        })
        .collect()
}

/// A running agent tile's own second line (issue #637's Cost
/// section): the per-turn strip, and, when there's room left after
/// it, the cache bar -- `tokens_cached_in`'s own share of the tile's
/// `total` (its live `tokens_sent`, the one figure a live tile is
/// ever sized from). `None` for a tile that isn't a live agent at
/// all, or one with neither figure to show.
fn live_turn_line(cell: &oscilloscope_core::treemap::TreemapCell, width: usize) -> Option<String> {
    let live_turns = cell.live_turns.as_ref()?;
    let spark = turn_tokens_sparkline(&live_turns.turn_tokens, width.min(12));
    let cache = live_turns.tokens_cached_in.map(|cached| {
        let pct = if cell.total > 0 {
            (cached as f64 / cell.total as f64 * 100.0).round() as u64
        } else {
            0
        };
        format!("cache {pct}%")
    });
    let text = match (spark.is_empty(), cache) {
        (false, Some(cache)) => format!("{spark} {cache}"),
        (false, None) => spark,
        (true, Some(cache)) => cache,
        (true, None) => return None,
    };
    Some(truncate_label(&text, width))
}

/// A bar of filled block characters, `value`'s own share of `max`
/// (issue #527's "scaled to the larger of the two") — `max == 0`
/// draws an empty bar rather than dividing by zero. Shared by the
/// overview's own time card (`draw_time_card`) and, previously,
/// screen 2's token bar.
fn bar(value: u64, max: u64, width: usize) -> String {
    if max == 0 || width == 0 {
        return String::new();
    }
    let filled = ((value as f64 / max as f64) * width as f64).round() as usize;
    "█".repeat(filled.min(width))
}

/// The Cost screen's own squarified-layout cache (issue #632,
/// DESIGN.md's performance rule: "laid out again only when the tree
/// or the area changes") -- owned by the main loop alongside `App`,
/// not folded into it, since it holds ratatui's own `Rect` and the
/// laid-out `TreemapCell`s, neither of which `oscilloscope_core`
/// (pure geometry/model, no ratatui dependency) ever sees.
#[derive(Default)]
pub struct CostLayoutCache {
    key: Option<(Vec<TreemapNode>, Rect)>,
    cells: Vec<oscilloscope_core::treemap::TreemapCell>,
}

impl CostLayoutCache {
    fn cells(
        &mut self,
        nodes: &[TreemapNode],
        area: Rect,
    ) -> &[oscilloscope_core::treemap::TreemapCell] {
        let stale = match &self.key {
            Some((prev_nodes, prev_area)) => prev_nodes.as_slice() != nodes || *prev_area != area,
            None => true,
        };
        if stale {
            self.cells =
                oscilloscope_core::treemap::layout(nodes, area.x, area.y, area.width, area.height);
            self.key = Some((nodes.to_vec(), area));
        }
        &self.cells
    }
}

/// A measure's own value, formatted the way the Cost screen's own
/// treemap and table show it (DESIGN.md: `reviews 517 t`, `$1.10`)
/// -- `total` is always in [`oscilloscope_core::treemap::build_tree_
/// for_measure`]'s own unit for `measure` (tokens, or micro-dollars
/// for [`Measure::Cost`]), never a raw count to format directly.
fn format_measure_value(total: u64, measure: Measure) -> String {
    match measure {
        Measure::Cost => format!(
            "${:.2}",
            oscilloscope_core::treemap::display_cost_usd(total)
        ),
        _ => oscilloscope_core::format::format_tokens(total),
    }
}

/// The Cost screen's own by-store treemap (issue #632, DESIGN.md:
/// "one tile per state store"). Unlike screen 2's old `tokens by
/// key` treemap, there is no single wrapping root cell -- `screen.
/// tree` is already the root's own children, and `app.cost_zoom`
/// (self-healing: `App::cost_level` drops a trailing entry that no
/// longer resolves) picks which level is actually on screen. A tile
/// for a leaf still running shows "tokens when done" instead of a
/// number, whatever its own placeholder weight happens to be
/// (DESIGN.md), gated on exactly the same `status`/`has_children` a
/// `TreemapCell` already carries -- no extra field needed.
fn draw_cost_treemap(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::screens::CostScreen,
    app: &mut App,
    cost_layout: &mut CostLayoutCache,
    ctx: &DrawCtx,
) {
    let level = app.cost_level(&screen.tree).to_vec();
    let mut title = format!(" tokens by store · {} ", screen.measure.label());
    if !app.cost_zoom.is_empty() {
        // The zoomed store's own name at each level, not its full
        // dotted path -- `app.cost_zoom` stacks a *path* per level,
        // and every deeper one already contains every shallower one
        // as a prefix, so joining the raw paths themselves would
        // read `prime.fan › prime.fan.iter_0` instead of `fan ›
        // iter_0`. The measure stays in the title too, so zooming
        // never hides which measure is on screen.
        let breadcrumb: Vec<&str> = app
            .cost_zoom
            .iter()
            .map(|p| p.rsplit('.').next().unwrap_or(p.as_str()))
            .collect();
        title = format!(
            " tokens by store · {} · {} ",
            screen.measure.label(),
            breadcrumb.join(" › ")
        );
    }
    if screen.stores_without_measure > 0 {
        let noun = if screen.stores_without_measure == 1 {
            "store"
        } else {
            "stores"
        };
        title = format!(
            "{} · {} {noun} without {} ",
            title.trim_end(),
            screen.stores_without_measure,
            screen.measure.label()
        );
        title = format!(" {}", title.trim_start());
    }
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if level.is_empty() || inner.width == 0 || inner.height == 0 {
        let paragraph = Paragraph::new("(no stores yet)");
        frame.render_widget(paragraph, inner);
        return;
    }

    let selected = app.cost_selected.clone();
    let cells = cost_layout.cells(&level, inner).to_vec();
    for cell in &cells {
        let rect = Rect::new(cell.rect.x, cell.rect.y, cell.rect.w, cell.rect.h);
        if rect.width == 0 || rect.height == 0 {
            continue;
        }
        let color = cell
            .status
            .map(|s| status_color(s, ctx.theme))
            .unwrap_or(ctx.theme.dim);
        let is_selected = selected.is_some() && cell.path == selected;
        let mut style = Style::default().fg(color);
        if is_selected {
            style = style.add_modifier(Modifier::REVERSED);
        }
        // DESIGN.md: a tile for a step that is still running reads
        // "tokens when done" until it finishes, never a number --
        // true for a container's own tile exactly as much as a
        // leaf's: `cell.total` is always the real, unboosted value
        // ([`oscilloscope_core::treemap::TreemapNode`]'s own doc
        // comment), so a container with at least one real, finished
        // child already shows that real total (never hidden behind
        // "tokens when done" merely because something else under it
        // is still running), while one with nothing but a running
        // placeholder underneath reads exactly like a bare running
        // leaf would. A run that ended (aborted or cancelled) while a
        // leaf was still in flight leaves it in the exact same state
        // -- no `completed_at` ever written -- but it will never
        // reach "done" either, so it reads "no tokens reported"
        // instead, rather than the placeholder weight's own number
        // read back as if it were a real one.
        let value_text = if cell.total > 0 {
            format_measure_value(cell.total, screen.measure)
        } else {
            match cell.status {
                Some(StatusKind::Running) => "tokens when done".to_string(),
                Some(StatusKind::Aborted) | Some(StatusKind::Cancelled) => {
                    "no tokens reported".to_string()
                }
                _ => format_measure_value(cell.total, screen.measure),
            }
        };
        // Issue #637: a running agent's own tile, sized from its
        // live `tokens_sent`, is labelled `live` instead of a bare
        // number -- the one thing `cell.total > 0` alone can no
        // longer tell apart from a container that already has a
        // real, finished child of its own.
        let labelled = if cell.live {
            format!("{} {value_text} live", cell.label)
        } else {
            format!("{} {value_text}", cell.label)
        };
        if cell.has_children {
            let block = Block::default()
                .borders(Borders::ALL)
                .title(truncate_label(
                    &labelled,
                    rect.width.saturating_sub(2) as usize,
                ))
                .border_style(style);
            frame.render_widget(block, rect);
        } else {
            // Same "fill is the area" rule screen 2's old treemap used
            // (DESIGN.md §6.3): a leaf (or a merged `…` bucket) has no
            // border, so its own background fill reads as an area at
            // all.
            let fill = Style::default().bg(color);
            let fill = if is_selected {
                fill.add_modifier(Modifier::REVERSED)
            } else {
                fill
            };
            frame.render_widget(Block::default().style(fill), rect);
            if rect.width >= 2 {
                let label = truncate_label(&labelled, rect.width as usize);
                let text_style = Style::default().bg(color).fg(ctx.theme.on_accent);
                let text_style = if is_selected {
                    text_style.add_modifier(Modifier::REVERSED)
                } else {
                    text_style
                };
                let mut tile_lines = vec![Line::styled(label, text_style)];
                // Issue #637: the per-turn strip and the cache bar,
                // when the tile actually has a second row to draw
                // them on -- never at the cost of the label itself.
                if rect.height >= 2 {
                    if let Some(extra) = live_turn_line(cell, rect.width as usize) {
                        tile_lines.push(Line::styled(extra, text_style));
                    }
                }
                let paragraph = Paragraph::new(tile_lines);
                frame.render_widget(paragraph, rect);
            }
        }
    }
}

/// The tokens-over-time chart's own fixed cycle of glyphs (DESIGN.md:
/// "one band per top-level step"), paired one-for-one with
/// [`phase_color`]'s own cycle -- DESIGN.md's "still reads from
/// glyphs and modifiers, never colour alone" rule applies here same
/// as everywhere else: two adjacent bands must still read apart with
/// colour turned off. Its own length is `crate::screens::build_cost_
/// screen`'s own `max_phase_bands` (issue #641): with more real
/// phases than this, `cost::cap_phase_bands` folds the smallest ones
/// into one further `other` band rather than ever reusing a glyph
/// in the legend.
pub(crate) const PHASE_GLYPHS: [char; 6] = ['█', '▓', '▒', '░', '▐', '▌'];

/// One of a small, fixed cycle of theme colours for the tokens-over-
/// time chart's own bands -- there is no per-phase colour role in
/// `Theme` itself (phases are document-specific, unlike a fixed
/// status), so this cycles through roles that are already visually
/// distinct from one another, the same cycle length as
/// [`PHASE_GLYPHS`].
fn phase_color(index: usize, theme: &Theme) -> Color {
    match index % PHASE_GLYPHS.len() {
        0 => theme.token_in,
        1 => theme.token_out,
        2 => theme.status_running,
        3 => theme.status_done,
        4 => theme.warning,
        _ => theme.error,
    }
}

/// A fill glyph for one of the chart's own bands, paired with
/// [`phase_color`] (same index, same cycle length).
fn phase_glyph(index: usize) -> char {
    PHASE_GLYPHS[index % PHASE_GLYPHS.len()]
}

/// The "tokens over time" stacked chart's own clock axis (DESIGN.md:
/// "a clock axis from the run's start to now") -- the start, middle
/// and end of exactly the span `screen.tokens_over_time` was bucketed
/// over, left-, centre- and right-aligned across `width` columns.
/// Blank once the run has no start time yet (nothing has bucketed
/// data to put an axis under).
fn chart_axis_line(
    screen: &oscilloscope_core::screens::CostScreen,
    ctx: &DrawCtx,
    width: usize,
) -> Line<'static> {
    let Some(start_ts) = screen.chart_start_ts.as_deref() else {
        return Line::from("");
    };
    let format = oscilloscope_core::format::format_clock_time;
    let start = format(start_ts, ctx.zone).unwrap_or_default();
    let end = format(&screen.chart_end_ts, ctx.zone).unwrap_or_default();
    let mid =
        oscilloscope_core::format::midpoint_clock_time(start_ts, &screen.chart_end_ts, ctx.zone)
            .unwrap_or_default();
    let mut row = vec![' '; width];
    place_label(&mut row, 0, &start);
    let mid_start = (width / 2).saturating_sub(mid.chars().count() / 2);
    place_label(&mut row, mid_start, &mid);
    place_label(&mut row, width.saturating_sub(end.chars().count()), &end);
    Line::styled(
        row.into_iter().collect::<String>(),
        Style::default().fg(ctx.theme.dim),
    )
}

/// Overwrites `row` starting at `start` with `text`'s own characters
/// -- later calls (the axis's end label, right of its middle one) can
/// overwrite a few trailing spaces of an earlier one on a narrow
/// pane, but never panics: every index is bounds-checked first.
fn place_label(row: &mut [char], start: usize, text: &str) {
    for (i, c) in text.chars().enumerate() {
        if let Some(slot) = row.get_mut(start + i) {
            *slot = c;
        }
    }
}

/// The chart's own legend (DESIGN.md's "never colour alone" rule): one
/// glyph-and-label pair per phase, truncated to `width` as a whole
/// rather than cut mid-pair.
fn chart_legend_line(
    screen: &oscilloscope_core::screens::CostScreen,
    width: usize,
) -> Line<'static> {
    let mut text = String::new();
    for (idx, (label, _)) in screen.tokens_over_time.iter().enumerate() {
        if idx > 0 {
            text.push(' ');
        }
        text.push(phase_glyph(idx));
        text.push(' ');
        text.push_str(label);
    }
    Line::from(truncate_label(&text, width))
}

/// The "tokens over time" stacked chart (DESIGN.md: "a stacked area
/// chart with one band per top-level step, on a clock axis from the
/// run's start to now"). `screen.tokens_over_time` is already
/// bucketed by [`oscilloscope_core::cost::TOKENS_OVER_TIME_BUCKETS`]
/// equal time spans; this rebins those onto exactly as many columns
/// as the pane's own width has, splitting a bucket's value across
/// every column it spans (or summing several buckets into one when
/// the pane is narrower than the bucket count) rather than capping
/// the chart at the bucket count itself, then fills each column's
/// own rows bottom-up, per phase, proportional to that phase's own
/// share of the column's total -- the nearest a character-cell
/// terminal gets to a real stacked area fill. The bottom two rows
/// (whenever there's room for them) are the clock axis and a glyph
/// legend rather than more bars.
fn draw_cost_chart(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::screens::CostScreen,
    ctx: &DrawCtx,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" tokens over time ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let width = inner.width as usize;
    if width == 0 || inner.height == 0 || screen.tokens_over_time.is_empty() {
        return;
    }
    let reserved = if inner.height >= 3 { 2 } else { 0 };
    let bars_height = inner.height as usize - reserved;
    if bars_height == 0 {
        return;
    }
    let buckets = oscilloscope_core::cost::TOKENS_OVER_TIME_BUCKETS;
    let phases = screen.tokens_over_time.len();
    let columns_n = width;
    let mut columns = vec![vec![0u64; phases]; columns_n];
    for (phase_idx, (_, series)) in screen.tokens_over_time.iter().enumerate() {
        for (bucket_idx, &value) in series.iter().enumerate() {
            if value == 0 {
                continue;
            }
            let col_start = bucket_idx * columns_n / buckets;
            let col_end = ((bucket_idx + 1) * columns_n / buckets)
                .max(col_start + 1)
                .min(columns_n);
            let span = (col_end - col_start) as u64;
            let share = value / span;
            let remainder = value % span;
            for (i, col) in (col_start..col_end).enumerate() {
                let extra = u64::from((i as u64) < remainder);
                columns[col][phase_idx] += share + extra;
            }
        }
    }
    let max_total = columns
        .iter()
        .map(|c| c.iter().sum::<u64>())
        .max()
        .unwrap_or(0);
    if max_total == 0 {
        let paragraph = Paragraph::new("(no tokens yet)");
        frame.render_widget(paragraph, inner);
        return;
    }
    // `grid[row][col]` is the phase index filling that cell, or
    // `None` for blank space above a column's own fill.
    let mut grid: Vec<Vec<Option<usize>>> = vec![vec![None; columns_n]; bars_height];
    for (col_idx, col) in columns.iter().enumerate() {
        let col_total = col.iter().sum::<u64>();
        if col_total == 0 {
            continue;
        }
        let col_height =
            ((col_total as f64 / max_total as f64) * bars_height as f64).round() as usize;
        let col_height = col_height.clamp(1, bars_height);
        let mut filled = 0usize;
        for (phase_idx, &value) in col.iter().enumerate() {
            if filled >= col_height {
                break;
            }
            let share = ((value as f64 / col_total as f64) * col_height as f64).round() as usize;
            let share = share.min(col_height - filled);
            for r in 0..share {
                let row = bars_height - 1 - filled - r;
                grid[row][col_idx] = Some(phase_idx);
            }
            filled += share;
        }
    }
    let mut lines: Vec<Line> = grid
        .into_iter()
        .map(|row| {
            let spans: Vec<Span> = row
                .into_iter()
                .map(|cell| match cell {
                    None => Span::raw(" "),
                    Some(phase_idx) => Span::styled(
                        phase_glyph(phase_idx).to_string(),
                        Style::default().fg(phase_color(phase_idx, ctx.theme)),
                    ),
                })
                .collect();
            Line::from(spans)
        })
        .collect();
    if reserved == 2 {
        lines.push(chart_axis_line(screen, ctx, width));
        lines.push(chart_legend_line(screen, width));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

/// The "largest stores" table (DESIGN.md: "path, tokens in, tokens
/// out, cost (when reported) and share of the total"), with its own
/// footer (the run's own totals, and the gap between them and the
/// table, when there is one).
/// `↑`/`↓`/`⏎`/`esc` select and zoom the table the same way they do
/// the treemap (DESIGN.md: "the table alone, where ⏎ still zooms") --
/// `app.cost_level`'s own self-healing walk of `app.cost_zoom` is
/// what the table's own rows are filtered to, not a second, possibly
/// diverging, copy of that walk, and a row matching `app.cost_
/// selected` is reversed the same way a selected treemap tile is.
fn draw_cost_table(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::screens::CostScreen,
    app: &mut App,
    ctx: &DrawCtx,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" largest stores ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let zoomed = !app.cost_zoom.is_empty();
    let level = app.cost_level(&screen.tree);
    let in_scope: Vec<&str> = level.iter().map(|n| n.path.as_str()).collect();
    let selected = app.cost_selected.clone();
    // Unzoomed, every row already descends from some top-level tile
    // by construction -- skipping the scope check entirely there
    // keeps this `O(rows)`, not `O(rows × level)`, for the common
    // case (issue #527's own 2,000-node redraw-cost bound covers the
    // Cost screen too).
    let in_path_scope = |path: &str| -> bool {
        in_scope.iter().any(|p| {
            path == *p
                || (path.len() > p.len() && path.starts_with(p) && path.as_bytes()[p.len()] == b'.')
        })
    };
    let rows: Vec<&oscilloscope_core::cost::StoreRow> = screen
        .stores
        .rows
        .iter()
        .filter(|r| !zoomed || in_path_scope(&r.path))
        .collect();
    let total_tokens: u64 = rows.iter().map(|r| r.tokens_sent + r.tokens_received).sum();
    // Relative to the zoomed store, not the whole run, once zoomed --
    // `row.label` itself is always root-relative (`oscilloscope_core
    // ::cost::StoreRow`'s own doc comment), so a zoomed view re-
    // derives it from the row's own real path instead of trying to
    // strip a prefix back out of an already-built string.
    let zoom_root = app.cost_zoom.last().map(String::as_str);
    // Issue #641 (finding on look-alike rows): every row's own full,
    // unshortened label, so shortening one row's own label can tell
    // whether dropping down to some width would make it read
    // identically to another's -- and if so, which ancestor segment
    // actually tells them apart.
    let all_labels: Vec<String> = rows
        .iter()
        .map(|row| match zoom_root {
            Some(root) => oscilloscope_core::cost::zoomed_row_label(row, root),
            None => row.label.clone(),
        })
        .collect();
    let mut row_lines: Vec<Line> = Vec::new();
    for row in &rows {
        let share = if total_tokens > 0 {
            (row.tokens_sent + row.tokens_received) as f64 / total_tokens as f64 * 100.0
        } else {
            0.0
        };
        let cost = match row.cost_usd {
            Some(c) => format!("  ${c:.2}"),
            None => String::new(),
        };
        let color = row
            .status
            .map(|s| status_color(s, ctx.theme))
            .unwrap_or(ctx.theme.text);
        let mut style = Style::default().fg(color);
        // The orchestration pane's own selection can name a
        // container (`fan`), not only a leaf -- in table-only mode
        // `↑`/`↓` walk the treemap's own tiles, so a selected
        // container never has a row whose path matches it exactly
        // unless that container also happens to be a `while: mode:
        // model` loop with a `check` row of its own. Every row *under*
        // the selection (its path, plus `.`) is reversed too, the same
        // as the treemap highlights every descendant tile of a
        // selected container.
        let is_selected = selected.as_deref().is_some_and(|sel| {
            row.path == sel
                || (row.path.len() > sel.len()
                    && row.path.starts_with(sel)
                    && row.path.as_bytes()[sel.len()] == b'.')
        });
        if is_selected {
            style = style.add_modifier(Modifier::REVERSED);
        }
        // The same "nothing real yet" rule the treemap's own tiles
        // read by (`draw_cost_treemap`'s `value_text`): a running (or
        // ended-without-finishing) row with nothing reported shows
        // the same placeholder text its tile does, never `↑0 t ↓0 t
        // 0%` read back as if it meant something.
        let share_text = oscilloscope_core::format::format_share_pct(share);
        let value_text = if row.tokens_sent + row.tokens_received > 0 {
            format!(
                "↑{} ↓{}{cost}  {share_text}",
                oscilloscope_core::format::format_tokens(row.tokens_sent),
                oscilloscope_core::format::format_tokens(row.tokens_received),
            )
        } else {
            match row.status {
                Some(StatusKind::Running) => "tokens when done".to_string(),
                Some(StatusKind::Aborted) | Some(StatusKind::Cancelled) => {
                    "no tokens reported".to_string()
                }
                _ => format!(
                    "↑{} ↓{}{cost}  {share_text}",
                    oscilloscope_core::format::format_tokens(row.tokens_sent),
                    oscilloscope_core::format::format_tokens(row.tokens_received),
                ),
            }
        };
        let label = match zoom_root {
            Some(root) => oscilloscope_core::cost::zoomed_row_label(row, root),
            None => row.label.clone(),
        };
        // Shortened from the left (`… › iter_1 › review`), never the
        // right: the leaf is the one segment that actually has to
        // stay readable to tell two same-named rows apart.
        let label_budget = (inner.width as usize).saturating_sub(2 + value_text.chars().count());
        let label = oscilloscope_core::cost::shorten_breadcrumb_from_left(
            &label,
            &all_labels,
            label_budget,
        );
        row_lines.push(Line::styled(format!("{label}  {value_text}"), style));
    }

    let footer = &screen.stores.footer;
    let mut footer_lines: Vec<Line> = Vec::new();
    // The coverage note (`$4.20 (from 3 of 9 prompts)`) gets its own
    // line, not appended to the totals line -- issue #632's small-
    // terminal acceptance item: sharing one line with `total ↑X ↓Y`
    // left only the table pane's own narrower width (beside the
    // chart, DESIGN.md's 62/38 split) for both, truncating the label
    // well before the pane itself ran out of room for it alone.
    footer_lines.push(Line::styled(
        format!(
            "total ↑{} ↓{}",
            oscilloscope_core::format::format_tokens(footer.run_tokens_sent),
            oscilloscope_core::format::format_tokens(footer.run_tokens_received),
        ),
        Style::default().fg(ctx.theme.dim),
    ));
    if let Some(cost_label) = footer.cost_label_for_width(inner.width as usize) {
        footer_lines.push(Line::styled(cost_label, Style::default().fg(ctx.theme.dim)));
    }
    if footer.gap_tokens > 0 {
        footer_lines.push(Line::styled(
            format!(
                "{} not in the table above",
                oscilloscope_core::format::format_tokens(footer.gap_tokens)
            ),
            Style::default().fg(ctx.theme.dim),
        ));
    }

    // The footer (the run's own totals, its gap, its cost coverage)
    // always stays visible, right after the rows: with more rows than
    // fit, rows are what gets cut, ending in a "+N more" line, never
    // the footer itself scrolling off the bottom of a `Paragraph` that
    // simply stops drawing past its own area -- which is exactly what
    // appending the footer after every row, unconditionally, used to
    // risk.
    let reserved = footer_lines.len() + 1; // +1: the blank separator line, always present.
    let available_for_rows = (inner.height as usize).saturating_sub(reserved);
    let mut lines: Vec<Line> = Vec::new();
    if row_lines.len() <= available_for_rows {
        lines.extend(row_lines);
    } else if available_for_rows > 0 {
        let shown = available_for_rows - 1; // -1: this tier's own "+N more" line.
        let dropped = row_lines.len() - shown;
        lines.extend(row_lines.into_iter().take(shown));
        lines.push(Line::styled(
            format!("+{dropped} more"),
            Style::default().fg(ctx.theme.dim),
        ));
    }
    lines.push(Line::from(""));
    lines.extend(footer_lines);

    let lines = truncate_lines(lines, inner.width as usize);
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Screen 3 (`Cost`, issue #632). DESIGN.md's own small-terminal
/// rules: at [`thresholds::COST_CHART_MIN_WIDTH`]/`_MIN_HEIGHT` (the
/// mockup's 100×30) and up, the treemap (62% of the height) sits
/// above the chart and the table, side by side at 62/38; below that
/// but still at [`thresholds::COST_TREEMAP_MIN_WIDTH`]/`_MIN_HEIGHT`
/// (80×20), the chart is dropped and the table takes the full width
/// under the treemap; below that, the table alone, where `⏎` still
/// zooms it.
fn draw_cost_screen(
    frame: &mut Frame,
    frame_area: Rect,
    area: Rect,
    screen: &oscilloscope_core::screens::CostScreen,
    app: &mut App,
    cost_layout: &mut CostLayoutCache,
    ctx: &DrawCtx,
) {
    let show_treemap = frame_area.width >= thresholds::COST_TREEMAP_MIN_WIDTH
        && frame_area.height >= thresholds::COST_TREEMAP_MIN_HEIGHT;
    if !show_treemap {
        draw_cost_table(frame, area, screen, app, ctx);
        return;
    }
    let treemap_rows = oscilloscope_core::layout::golden_major_section(area.height);
    let rest_rows = area.height - treemap_rows;
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(treemap_rows), Constraint::Min(rest_rows)])
        .split(area);
    draw_cost_treemap(frame, rows[0], screen, app, cost_layout, ctx);

    let show_chart = frame_area.width >= thresholds::COST_CHART_MIN_WIDTH
        && frame_area.height >= thresholds::COST_CHART_MIN_HEIGHT;
    if !show_chart {
        draw_cost_table(frame, rows[1], screen, app, ctx);
        return;
    }
    let chart_width = oscilloscope_core::layout::golden_major_section(rows[1].width);
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(chart_width),
            Constraint::Min(rows[1].width - chart_width),
        ])
        .split(rows[1]);
    draw_cost_chart(frame, cols[0], screen, ctx);
    draw_cost_table(frame, cols[1], screen, app, ctx);
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width, height)
}

/// Finding 19: `Clear` resets the popup's own area to the terminal's
/// bare default (no background, no foreground) — a named theme's own
/// base fill (`draw`'s own `Block` at the top of this file) never
/// reaches back over a popup drawn on top of it, so every overlay
/// repaints its own area with the theme's `background`/`text` right
/// after clearing it, the same pair `draw`'s own base fill sets.
fn draw_overlay(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    text: Vec<Line<'static>>,
    w: u16,
    h: u16,
    ctx: &DrawCtx,
) {
    draw_overlay_at(frame, centered(area, w, h), title, text, ctx);
}

/// [`draw_overlay`]'s own body, at an already-placed `popup` rather
/// than one it centres itself -- the Tape's own Help (issue #678's
/// §4/§5: a fixed 72×18 at `2,5`, not centred in the frame the way
/// every other dialog's own popup still is).
fn draw_overlay_at(
    frame: &mut Frame,
    popup: Rect,
    title: &str,
    text: Vec<Line<'static>>,
    ctx: &DrawCtx,
) {
    frame.render_widget(Clear, popup);
    if let Some(bg) = ctx.theme.background {
        frame.render_widget(
            Block::default().style(Style::default().bg(bg).fg(ctx.theme.text)),
            popup,
        );
    }
    let paragraph = Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .alignment(Alignment::Left)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_set(ratatui::symbols::border::ROUNDED)
                .title(title),
        );
    frame.render_widget(paragraph, popup);
}

// Every line here fits inside `HELP_OVERLAY_WIDTH`'s own interior
// with no wrapping, and there are few enough of them that `draw_
// dialog_overlay`'s own `HELP_TEXT.len() + 2` height still fits a
// bare 80×24 terminal (`centered` clamps a taller popup to the
// screen's own height, which used to cut "press any key to close"
// off the bottom there).
/// A row of the Tape's own key/glyph legend (§4's own `Help ·
/// Tape`), two columns wide: each a short key or glyph in `fg.muted`
/// followed by what it does in `fg.default`, exactly `(7, 28, 8, 26)`
/// columns wide -- §6's own "a key in a key bar has a designed
/// effect, and the Help overlay lists exactly the key bars' keys",
/// never a leftover key from an older draft of this same screen,
/// or one some *other* screen's own key bar still has.
const HELP_ROWS: &[(&str, &str, &str, &str)] = &[
    ("↑ ↓", "select a row", "0 1 2 3", "Tape Signal Flow Cost"),
    ("enter", "open the step", "?", "this help"),
    ("p", "prompt previews on or off", "esc", "close · back"),
    (
        "t",
        "tokens: units or vs usual",
        "q",
        "quit · confirms if running",
    ),
];
const HELP_LEGEND_ROWS: &[(&str, &str, &str, &str)] = &[(
    "+25%",
    "above usual · red past +50%",
    "◆ ⟳ ■",
    "decision · loop · run",
)];

// §4 also names `j`/`k`, `PgUp`/`PgDn`, `Home`/`End`, `g`/`G`, `/`,
// `e`, `T` and `f` as Tape keys, and says the Help overlay lists
// all of it -- but every `tape--help` frame leaves its own six
// rows here blank (`design/frames/tape--help--120x30.mock`, rows
// 15-20), so adding them would stop matching the frame on over 300
// cells at once, past the point an allow-list still documents a
// design ambiguity rather than papering over one. Left for the
// frame to settle; see the PR's own design-questions note.

fn tape_help_row(key1: &str, desc1: &str, key2: &str, desc2: &str, ctx: &DrawCtx) -> Line<'static> {
    let muted = Style::default().fg(ctx.theme.fg_muted);
    let default = Style::default().fg(ctx.theme.text);
    Line::from(vec![
        Span::styled(format!("{key1:<7}"), muted),
        Span::styled(format!("{desc1:<28}"), default),
        Span::styled(format!("{key2:<8}"), muted),
        Span::styled(format!("{desc2:<26}"), default),
    ])
}

fn tape_help_lines(ctx: &DrawCtx, unit: u64, history_runs: Option<usize>) -> Vec<Line<'static>> {
    let muted = Style::default().fg(ctx.theme.fg_muted);
    let mut lines: Vec<Line<'static>> = HELP_ROWS
        .iter()
        .map(|(k1, d1, k2, d2)| tape_help_row(k1, d1, k2, d2, ctx))
        .collect();
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("{:<35}{:<34}", "Tokens", "Glyphs"),
        muted,
    )));
    let runs = history_runs.unwrap_or(oscilloscope_core::history::P50_MIN_RUNS);
    lines.push(tape_help_row(
        "▰",
        &format!("= {unit} t · fixed scale"),
        "✓ ✗",
        "done · failed",
        ctx,
    ));
    lines.push(tape_help_row(
        "usual",
        &format!("median of {runs} past runs"),
        "● ·",
        "running · ahead",
        ctx,
    ));
    lines.extend(
        HELP_LEGEND_ROWS
            .iter()
            .map(|(k1, d1, k2, d2)| tape_help_row(k1, d1, k2, d2, ctx)),
    );
    for _ in 0..6 {
        lines.push(Line::from(""));
    }
    let key_style = Style::default()
        .fg(ctx.theme.keyhint_key)
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(ctx.theme.keyhint_desc);
    lines.push(Line::from(vec![
        Span::styled("esc ", key_style),
        Span::styled(format!("{:<7}", "Close"), desc_style),
        Span::styled("? ", key_style),
        Span::styled(format!("{:<56}", "Close"), desc_style),
    ]));
    lines
}

/// §4/§5's own 72×18 overlay (the design's `tape--help` frame) --
/// fixed, not sized from [`HELP_TEXT`]'s own length, so the overlay
/// never resizes as lines are added or removed.
const HELP_OVERLAY_WIDTH: u16 = 72;
const HELP_OVERLAY_HEIGHT: u16 = 18;

/// §4's own Signal keys, plus the globals every screen's key bar
/// shares -- the only keys Signal's own key bar advertises, so the
/// only ones its own Help lists.
fn signal_help_lines(ctx: &DrawCtx) -> Vec<Line<'static>> {
    const ROWS: &[(&str, &str, &str, &str)] = &[
        (
            "← →",
            "scrub the cursor along the axis",
            "+ -",
            "zoom: 50 ms to 1 s a column",
        ),
        (
            "n",
            "cursor back to Now",
            "enter",
            "open the step under the cursor",
        ),
        ("0 1 2 3", "Tape Signal Flow Cost", "?", "this help"),
        ("esc", "close · back", "q", "quit · confirms if running"),
        ("c", "cancel · confirms", "", ""),
    ];
    let mut lines: Vec<Line<'static>> = ROWS
        .iter()
        .map(|(k1, d1, k2, d2)| tape_help_row(k1, d1, k2, d2, ctx))
        .collect();
    for _ in 0..7 {
        lines.push(Line::from(""));
    }
    let key_style = Style::default()
        .fg(ctx.theme.keyhint_key)
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(ctx.theme.keyhint_desc);
    lines.push(Line::from(vec![
        Span::styled("esc ", key_style),
        Span::styled(format!("{:<7}", "Close"), desc_style),
        Span::styled("? ", key_style),
        Span::styled(format!("{:<56}", "Close"), desc_style),
    ]));
    lines
}

/// The Flow's own Help (issue #691, design v1 part 3), the same
/// fixed overlay the Tape's and Signal's own use, listing exactly
/// §4's own Flow keys and the globals every key bar shares -- never
/// the movement aliases, `/`, `e`, `T`, `v`, `z`, `s`, `l`, `Tab`
/// §4 already says are gone from the Flow.
fn flow_help_lines(ctx: &DrawCtx) -> Vec<Line<'static>> {
    const ROWS: &[(&str, &str, &str, &str)] = &[
        ("← →", "scroll the rail", "+ -", "open / fold loop lanes"),
        (
            "↑ ↓",
            "select a pass",
            "enter",
            "open the selection full-screen",
        ),
        ("f", "follow Now", "0 1 2 3", "Tape Signal Flow Cost"),
        ("?", "this help", "esc", "close · back"),
        ("q", "quit · confirms if running", "c", "cancel · confirms"),
    ];
    let mut lines: Vec<Line<'static>> = ROWS
        .iter()
        .map(|(k1, d1, k2, d2)| tape_help_row(k1, d1, k2, d2, ctx))
        .collect();
    for _ in 0..7 {
        lines.push(Line::from(""));
    }
    let key_style = Style::default()
        .fg(ctx.theme.keyhint_key)
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(ctx.theme.keyhint_desc);
    lines.push(Line::from(vec![
        Span::styled("esc ", key_style),
        Span::styled(format!("{:<7}", "Close"), desc_style),
        Span::styled("? ", key_style),
        Span::styled(format!("{:<56}", "Close"), desc_style),
    ]));
    lines
}

/// Cost's own Help: unlike the Tape, Signal and now Flow, it keeps
/// `s`/`l`/`z`/`v`/`Tab` live until its own part of the redesign, so
/// its own Help lists them rather than its own key bar.
fn screen_help_text(screen: oscilloscope_core::screens::Screen) -> (String, Vec<&'static str>) {
    use oscilloscope_core::screens::Screen;
    let (title, screen_lines): (&str, &[&str]) = match screen {
        Screen::Cost => (
            " Help · Cost ",
            &["i o $     measure: tokens in / out / cost"],
        ),
        Screen::Tape | Screen::Signal | Screen::Flow => (" Help ", &[]),
    };
    let mut lines = vec!["↑ ↓       select a row               0-3     Tape Signal Flow Cost"];
    lines.extend_from_slice(screen_lines);
    lines.extend_from_slice(&[
        "v         full value overlay          z       fold on/off",
        "j k       same as ↑ ↓                   PgUp PgDn move a page",
        "Home End  first / last row            g G     same as Home/End",
        "/         filter rows                 e       errors only",
        "f         follow the running row      T       cycle the colour theme",
        "?         this help                   esc     close · back",
        "q         quit · confirms if running   c       cancel · confirms",
    ]);
    (title.to_string(), lines)
}

/// One redraw's whole layout (issue #626): a one-line header, shared
/// by every screen (replacing the old three-row one), then the
/// screen currently on display (`app.screen`) — screen 2 (`Flow`,
/// issue #635) is the tree as control flow, the selected step and the
/// run's story; screen 1 (`Signal`) draws its own header, body and
/// key bar entirely on its own, before this function's shared layout
/// is even reached.
/// `overview`/`cost_screen`/`flow_screen`/`signal_screen` are
/// screens 0, 3, 2 and 1's own models, `Some` only while that screen
/// is actually on display
/// (issue #626's own performance rule: "models for screens not on
/// display aren't built"); `cost_layout` is the Cost screen's own
/// squarify-layout cache, re-laid out only when its own tree or area
/// actually changes. `raw_state` is the engine's own last snapshot,
/// untruncated — `details`' own `prompt_sent`/`value` are already cut
/// to `DETAILS_PREVIEW_CHARS` for the details pane, which `v`'s own
/// full-value overlay must not be.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    frame: &mut Frame,
    state: &oscilloscope_core::render::RenderState,
    details: Option<&Details>,
    log_lines: &[String],
    app: &mut App,
    raw_state: Option<&serde_json::Value>,
    ctx: &DrawCtx,
    icon_frame: u64,
    now: &str,
    tape_screen: Option<&oscilloscope_core::screens::TapeScreen>,
    cost_screen: Option<&oscilloscope_core::screens::CostScreen>,
    flow_screen: Option<&oscilloscope_core::screens::FlowScreen>,
    signal_screen: Option<&oscilloscope_core::screens::SignalScreen>,
    transcript_view: Option<&oscilloscope_core::transcript::TranscriptView>,
    cost_layout: &mut CostLayoutCache,
) {
    let area = frame.area();
    if let Some(bg) = ctx.theme.background {
        // Finding 7: a named theme's own base fill must set both
        // roles, not just `bg` -- with only `bg` painted, a dark-
        // background theme on a *light* terminal (solarized-light,
        // gruvbox-light, catppuccin-latte) drew that terminal's own
        // default foreground over it, which can be as light as the
        // terminal's own default background, not the theme's `text`.
        frame.render_widget(
            Block::default().style(Style::default().bg(bg).fg(ctx.theme.text)),
            area,
        );
    }

    // §5's own floor (issue #678): below 60×24, every screen reads
    // the too-small notice, not only the Tape -- the shared chrome
    // (the header band, the Totals row, the status line, the key
    // bar) leaves nothing below it for any screen's own panels.
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        draw_too_small(frame, area, ctx);
        return;
    }

    // Issue #678: the Tape draws its own header band, status line and
    // key bar (§3) over the whole frame, not the old one-line header
    // every other screen below still shares -- its own dialogs (help,
    // the confirms) are drawn on top the same way, after this return.
    if app.screen == oscilloscope_core::screens::Screen::Tape {
        if let Some(tape_screen) = tape_screen {
            draw_tape(
                frame,
                area,
                tape_screen,
                &state.header,
                app,
                ctx,
                icon_frame,
                now,
            );
        }
        // The Tape's own status line, two rows above the bottom
        // (`draw_tape`'s own layout: header, body, status, key bar,
        // each one row but the body), dims itself.
        let status_line = Rect {
            x: area.x,
            y: area.y + area.height.saturating_sub(2),
            width: area.width,
            height: 1,
        };
        dim_backdrop_for_dialog(frame, area, Some(status_line), app, ctx);
        draw_dialog_overlay(
            frame,
            area,
            app,
            details,
            raw_state,
            ctx,
            tape_screen.map(|s| s.unit).unwrap_or(50),
            state.header.history_same_hash_count,
        );
        return;
    }

    // Signal (issue #688) draws its own header band, status line and
    // key bar too, the same self-contained way the Tape does -- its
    // own panels (the trace, the Now pane, Readings) never share the
    // generic layout Flow/Cost still use below.
    if app.screen == oscilloscope_core::screens::Screen::Signal {
        if let Some(signal_screen) = signal_screen {
            draw_signal(
                frame,
                area,
                signal_screen,
                &state.header,
                app,
                ctx,
                icon_frame,
                now,
            );
        }
        let status_line = Rect {
            x: area.x,
            y: area.y + area.height.saturating_sub(2),
            width: area.width,
            height: 1,
        };
        dim_backdrop_for_dialog(frame, area, Some(status_line), app, ctx);
        draw_dialog_overlay(
            frame,
            area,
            app,
            details,
            raw_state,
            ctx,
            signal_screen.map(|s| s.unit).unwrap_or(50),
            state.header.history_same_hash_count,
        );
        return;
    }

    // The Flow rail (issue #691, part 3 of #677) draws its own header
    // band, status line and key bar too, the same self-contained way
    // the Tape and Signal do -- its own panels (the rail, the
    // selected pass or station, the Story pane) never share the
    // generic layout Cost still uses below.
    if app.screen == oscilloscope_core::screens::Screen::Flow {
        if let Some(flow_screen) = flow_screen {
            draw_flow(
                frame,
                area,
                flow_screen,
                &state.header,
                app,
                transcript_view,
                ctx,
                icon_frame,
                now,
            );
        }
        let status_line = Rect {
            x: area.x,
            y: area.y + area.height.saturating_sub(2),
            width: area.width,
            height: 1,
        };
        dim_backdrop_for_dialog(frame, area, Some(status_line), app, ctx);
        draw_dialog_overlay(
            frame,
            area,
            app,
            details,
            raw_state,
            ctx,
            flow_screen.map(|s| s.unit).unwrap_or(50),
            state.header.history_same_hash_count,
        );
        return;
    }

    // Issue #678's R3: the header band, the Totals row, the status
    // line and the key bar are the outer frame shared by every
    // screen now, not just the Tape -- Flow/Cost keep their own
    // inner panels and their own keys exactly as before, drawn into
    // whatever's left of `area` below the band and above the other
    // three rows.
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(5),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);
    let chip = match app.screen {
        oscilloscope_core::screens::Screen::Tape => "TAPE",
        oscilloscope_core::screens::Screen::Signal => "SIGNAL",
        oscilloscope_core::screens::Screen::Flow => "FLOW",
        oscilloscope_core::screens::Screen::Cost => "COST",
    };
    draw_tape_header_band(
        frame,
        sections[0],
        &state.header,
        chip,
        now,
        icon_frame,
        ctx,
    );

    // The body's own threshold checks (three-pane Flow, and so on)
    // read a `frame_area` as wide as the real terminal but as tall as
    // what is actually left for them below the header band and above
    // the Totals/status/key-bar rows -- the literal `area` would let
    // them believe they have three more rows than `body` really does.
    let reduced_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: area.height.saturating_sub(4),
    };
    let body = sections[1];
    let _ = reduced_area;
    match app.screen {
        oscilloscope_core::screens::Screen::Tape
        | oscilloscope_core::screens::Screen::Signal
        | oscilloscope_core::screens::Screen::Flow => {}
        oscilloscope_core::screens::Screen::Cost => {
            if let Some(cost_screen) = cost_screen {
                draw_cost_screen(
                    frame,
                    reduced_area,
                    body,
                    cost_screen,
                    app,
                    cost_layout,
                    ctx,
                );
            }
        }
    }

    draw_totals_row(
        frame,
        sections[2],
        &state.header,
        false,
        None,
        None,
        None,
        false,
        ctx,
    );
    draw_status_line_generic(frame, sections[3], &state.header, log_lines, ctx);
    draw_key_bar(frame, sections[4], &screen_key_bar_items(app.screen), ctx);

    dim_backdrop_for_dialog(frame, area, None, app, ctx);
    draw_dialog_overlay(frame, area, app, details, raw_state, ctx, 50, None);
}

/// Flow/Cost's own status line (§3.2), until their own part of the
/// redesign gives them a bespoke one the way the Tape and Signal
/// already have -- the run's own latest report, in the theme's own
/// warning/error colour,
/// collapsed to a bare `▸` when it would not fit (issue #610's
/// behaviour, carried over from the one-line header this replaces),
/// else the newest line already in the run's own log.
fn draw_status_line_generic(
    frame: &mut Frame,
    area: Rect,
    header: &oscilloscope_core::render::Header,
    log_lines: &[String],
    ctx: &DrawCtx,
) {
    use oscilloscope_core::observe::ReportLevel;
    if let Some(report) = &header.latest_report {
        let (prefix, style) = match report.level {
            ReportLevel::Warning => ("▸ warning: ", Style::default().fg(ctx.theme.warning)),
            ReportLevel::Info => ("▸ ", Style::default().fg(ctx.theme.status_info)),
        };
        let full = format!(" {prefix}{}", report.message);
        let text = if full.chars().count() as u16 > area.width {
            " ▸".to_string()
        } else {
            full
        };
        frame.render_widget(Paragraph::new(Line::from(Span::styled(text, style))), area);
        return;
    }
    let text = log_lines.last().cloned().unwrap_or_default();
    let style = if is_warning_report_log_line(&text) {
        Style::default().fg(ctx.theme.warning)
    } else if is_report_log_line(&text) {
        Style::default().fg(ctx.theme.status_info)
    } else {
        Style::default().fg(ctx.theme.text)
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(format!(" {text}"), style))),
        area,
    );
}

/// Dims the whole backdrop to `fg.faint` before a dialog draws on
/// top of it (issue #678's own Help frame) -- once a modal has the
/// only real focus, nothing underneath carries its own colour any
/// more, bold included, only the background each cell already had.
fn dim_backdrop_for_dialog(
    frame: &mut Frame,
    area: Rect,
    exclude: Option<Rect>,
    app: &App,
    ctx: &DrawCtx,
) {
    if app.dialog == Dialog::None {
        return;
    }
    let mut style = Style::default()
        .fg(ctx.theme.dim)
        .remove_modifier(Modifier::all());
    // The header/key bars' own distinct `statusbar.bg` tint goes
    // with everything else's own colour -- the whole backdrop
    // reads as one flat, dimmed surface once a modal's up. The
    // Tape's own status line excludes itself: it dims its own
    // spans directly (`draw_tape_status_line`'s own `dialog_open`),
    // leaving its own trailing blank -- nothing ever painted there
    // -- at the frame's own full brightness, not this sweep's.
    if let Some(bg) = ctx.theme.background {
        style = style.bg(bg);
    }
    for y in area.top()..area.bottom() {
        if let Some(skip) = exclude {
            if y >= skip.y && y < skip.y + skip.height {
                continue;
            }
        }
        let row = Rect {
            x: area.x,
            y,
            width: area.width,
            height: 1,
        };
        frame.buffer_mut().set_style(row, style);
    }
}

/// [`draw`]'s own dialog overlays (help, confirm, the full-value and
/// filter popups) — unchanged by issue #626, just pulled out so
/// `draw` itself reads as "header, then the current screen, then
/// whatever's on top of it".
#[allow(clippy::too_many_arguments)]
fn draw_dialog_overlay(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    details: Option<&Details>,
    raw_state: Option<&serde_json::Value>,
    ctx: &DrawCtx,
    tape_unit: u64,
    tape_history_runs: Option<usize>,
) {
    match &app.dialog {
        Dialog::Help if app.screen == oscilloscope_core::screens::Screen::Tape => {
            // §4/§5's own fixed `2,5,72,18` (`design/frames/tape--
            // help`), not centred like every other dialog's own, and
            // its own `bg.overlay`/`border.focus` colours and bold
            // title (`tape_block_title`'s own convention), not the
            // shared overlay's plain theme background and title.
            let popup = Rect {
                x: area.x + 2,
                y: area.y + 5,
                width: HELP_OVERLAY_WIDTH.min(area.width.saturating_sub(2)),
                height: HELP_OVERLAY_HEIGHT.min(area.height.saturating_sub(5)),
            };
            frame.render_widget(Clear, popup);
            let border = ctx.theme.border_focused;
            frame.render_widget(
                Block::default().style(Style::default().bg(ctx.theme.bg_surface)),
                popup,
            );
            let block = Block::default()
                .borders(Borders::ALL)
                .border_set(ratatui::symbols::border::ROUNDED)
                .title(tape_block_title("Help · Tape", border, ctx))
                .border_style(Style::default().fg(border).bg(ctx.theme.bg_surface));
            let inner = block.inner(popup);
            frame.render_widget(block, popup);
            fill_border_rows(frame, inner, border);
            let content_area = Rect {
                x: inner.x + 1,
                y: inner.y,
                width: inner.width.saturating_sub(1),
                height: inner.height,
            };
            let paragraph = Paragraph::new(tape_help_lines(ctx, tape_unit, tape_history_runs))
                .style(Style::default().bg(ctx.theme.bg_surface));
            frame.render_widget(paragraph, content_area);
        }
        Dialog::Help if app.screen == oscilloscope_core::screens::Screen::Signal => {
            let popup = Rect {
                x: area.x + 2,
                y: area.y + 5,
                width: HELP_OVERLAY_WIDTH.min(area.width.saturating_sub(2)),
                height: HELP_OVERLAY_HEIGHT.min(area.height.saturating_sub(5)),
            };
            frame.render_widget(Clear, popup);
            let border = ctx.theme.border_focused;
            frame.render_widget(
                Block::default().style(Style::default().bg(ctx.theme.bg_surface)),
                popup,
            );
            let block = Block::default()
                .borders(Borders::ALL)
                .border_set(ratatui::symbols::border::ROUNDED)
                .title(tape_block_title("Help · Signal", border, ctx))
                .border_style(Style::default().fg(border).bg(ctx.theme.bg_surface));
            let inner = block.inner(popup);
            frame.render_widget(block, popup);
            fill_border_rows(frame, inner, border);
            let content_area = Rect {
                x: inner.x + 1,
                y: inner.y,
                width: inner.width.saturating_sub(1),
                height: inner.height,
            };
            let paragraph = Paragraph::new(signal_help_lines(ctx))
                .style(Style::default().bg(ctx.theme.bg_surface));
            frame.render_widget(paragraph, content_area);
        }
        Dialog::Help if app.screen == oscilloscope_core::screens::Screen::Flow => {
            let popup = Rect {
                x: area.x + 2,
                y: area.y + 5,
                width: HELP_OVERLAY_WIDTH.min(area.width.saturating_sub(2)),
                height: HELP_OVERLAY_HEIGHT.min(area.height.saturating_sub(5)),
            };
            frame.render_widget(Clear, popup);
            let border = ctx.theme.border_focused;
            frame.render_widget(
                Block::default().style(Style::default().bg(ctx.theme.bg_surface)),
                popup,
            );
            let block = Block::default()
                .borders(Borders::ALL)
                .border_set(ratatui::symbols::border::ROUNDED)
                .title(tape_block_title("Help · Flow", border, ctx))
                .border_style(Style::default().fg(border).bg(ctx.theme.bg_surface));
            let inner = block.inner(popup);
            frame.render_widget(block, popup);
            fill_border_rows(frame, inner, border);
            let content_area = Rect {
                x: inner.x + 1,
                y: inner.y,
                width: inner.width.saturating_sub(1),
                height: inner.height,
            };
            let paragraph = Paragraph::new(flow_help_lines(ctx))
                .style(Style::default().bg(ctx.theme.bg_surface));
            frame.render_widget(paragraph, content_area);
        }
        Dialog::Help => {
            let (title, lines) = screen_help_text(app.screen);
            let height = (lines.len() as u16 + 2).max(HELP_OVERLAY_HEIGHT);
            let text = lines.into_iter().map(Line::from).collect();
            draw_overlay(frame, area, &title, text, HELP_OVERLAY_WIDTH, height, ctx);
        }
        Dialog::ConfirmCancel { .. } => {
            let text = vec![
                Line::from("cancel the run?"),
                Line::from(""),
                Line::from("y = cancel   n/esc = back"),
            ];
            draw_overlay(frame, area, " confirm ", text, 36, 5, ctx);
        }
        Dialog::FullValue(field) => {
            // Finding 5: reads straight from `raw_state`, bypassing
            // `details`' own truncated preview entirely — that's the
            // whole point of asking to see it in full.
            // The node the details pane is showing (#610: a state-pane
            // selection such as a named report), not the tree's own.
            let full = details
                .map(|d| d.path.as_str())
                .and_then(|p| oscilloscope_core::render::full_value(p, *field, raw_state));
            let text = vec![Line::from(full.unwrap_or_default())];
            draw_overlay(
                frame,
                area,
                " full value (press any key) ",
                text,
                area.width.saturating_sub(4),
                area.height.saturating_sub(4),
                ctx,
            );
        }
        Dialog::FilterInput(buf) => {
            let text = vec![
                Line::from(format!("/{buf}")),
                Line::from("enter = apply   esc = cancel"),
            ];
            draw_overlay(frame, area, " filter ", text, 40, 4, ctx);
        }
        Dialog::None => {}
    }
}

/// The Flow screen's own pane below the rail (DESIGN.md §2.3, §5):
/// the selected loop pass (`Pass 1 · cats · Selected`), a plain
/// selected station (reusing the overview's own `focus_step_lines`),
/// or a selected decision's own condition/branch/answer -- never more
/// than one of the three at once ([`oscilloscope_core::screens::
/// FlowScreen`]'s own doc comment).
#[allow(clippy::too_many_arguments)]
fn draw_flow_selected_pane(
    frame: &mut Frame,
    area: Rect,
    wide: bool,
    screen: &oscilloscope_core::screens::FlowScreen,
    app: &mut App,
    transcript_view: Option<&oscilloscope_core::transcript::TranscriptView>,
    ctx: &DrawCtx,
    now: &str,
) {
    let title = if let Some(pass) = &screen.selected_pass {
        format!("Pass {} · {} · Selected", pass.index, pass.label)
    } else if let Some(step) = &screen.selected_step {
        format!("{} · Selected", step.breadcrumb)
    } else {
        "Selected".to_string()
    };
    let lines = if let Some(pass) = &screen.selected_pass {
        selected_pass_lines(pass, wide, ctx)
    } else if let Some(decision) = &screen.selected_decision {
        decision_detail_lines(decision, ctx)
    } else if let Some(step) = &screen.selected_step {
        flow_selected_step_lines(step, transcript_view, ctx, now)
    } else {
        vec![Line::from("nothing selected")]
    };
    let border_color = ctx.theme.border;
    let viewport = area.height.saturating_sub(2) as usize;
    app.details_viewport = viewport;
    let total = lines.len();
    app.details_scroll = app.details_scroll.min(total.saturating_sub(1));
    let start = app.details_scroll.min(total);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(ratatui::symbols::border::ROUNDED)
        .title(tape_block_title(&title, border_color, ctx))
        .border_style(Style::default().fg(border_color));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    fill_border_rows(frame, inner, border_color);
    let content_width = inner.width.saturating_sub(1) as usize;
    let visible_lines: Vec<Line> = lines
        .into_iter()
        .skip(start)
        .take(viewport)
        .map(|line| pad_line_to_width(line, content_width, ctx.theme.text))
        .collect();
    let paragraph = ratatui::widgets::Paragraph::new(visible_lines);
    frame.render_widget(
        paragraph,
        Rect {
            x: inner.x + 1,
            y: inner.y,
            width: inner.width.saturating_sub(1),
            height: inner.height,
        },
    );
}

/// Pads `line` to `width` cells with trailing spaces in its own
/// last span's style (DESIGN.md's own frames: a pane's own content
/// row's trailing blank reads in whichever colour the line's own
/// text last used, never the border's own fill colour showing
/// through past a short line) -- `fallback_fg` is for an empty line,
/// which has no style of its own to extend.
fn pad_line_to_width(line: Line<'static>, width: usize, fallback_fg: Color) -> Line<'static> {
    let used: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
    if used >= width {
        return line;
    }
    let fill_style = line
        .spans
        .last()
        .map(|s| s.style)
        .unwrap_or_else(|| Style::default().fg(fallback_fg));
    let mut spans = line.spans;
    spans.push(Span::styled(" ".repeat(width - used), fill_style));
    Line::from(spans)
}

/// `Pass N · label · Selected`'s own body (DESIGN.md §2.3): Status
/// (the pass's own currently active step, with its elapsed time and
/// slot), one row per named step the pass ran with its tokens and
/// deviation, and Produced -- the last such step's own output.
fn selected_pass_lines(
    pass: &oscilloscope_core::screens::SelectedPass,
    wide: bool,
    ctx: &DrawCtx,
) -> Vec<Line<'static>> {
    // Below the three-pane threshold (DESIGN.md's own §5, 100×30),
    // the pane's own label column narrows and "vs usual" drops --
    // the same cramping the Tape's own rows take at this width.
    let label_width = if wide { 9 } else { 7 };
    let mut lines = Vec::new();
    use oscilloscope_core::render::RowKind;
    let glyph = tape_glyph(RowKind::Unknown, pass.active_status.kind);
    let glyph_style = tape_glyph_style(RowKind::Unknown, pass.active_status.kind, ctx.theme);
    let status_word = tape_status_kind_word(pass.active_status.kind);
    let mut status_spans = vec![
        Span::styled(
            format!("{:<label_width$}", "Status"),
            Style::default().fg(ctx.theme.fg_muted),
        ),
        Span::styled(format!("{glyph} {status_word}  "), glyph_style),
    ];
    let mut detail = pass.active_label.clone();
    if let Some(elapsed) = pass.active_elapsed_s {
        detail.push_str(&format!(
            " · {}",
            oscilloscope_core::format::format_duration(elapsed)
        ));
    }
    if let Some(slot) = pass.slot {
        detail.push_str(&format!(" · slot {slot}"));
    }
    status_spans.push(Span::styled(detail, Style::default().fg(ctx.theme.text)));
    lines.push(Line::from(status_spans));
    for step in &pass.steps {
        let glyph = tape_glyph(RowKind::Prompt, step.status.kind);
        let glyph_style = tape_glyph_style(RowKind::Prompt, step.status.kind, ctx.theme);
        let mut spans = vec![
            Span::styled(
                format!("{:<label_width$}", step.label),
                Style::default().fg(ctx.theme.fg_muted),
            ),
            Span::styled(
                format!("{glyph} {}  ", tape_status_kind_word(step.status.kind)),
                glyph_style,
            ),
        ];
        let total = step.sent + step.received;
        if total > 0 {
            spans.push(Span::styled(
                format!(
                    "↑{} ↓{} · ",
                    oscilloscope_core::format::format_tokens_thousands(step.sent),
                    oscilloscope_core::format::format_tokens_thousands(step.received)
                ),
                Style::default().fg(ctx.theme.text),
            ));
            spans.push(Span::styled(
                format!("{}  ", oscilloscope_core::format::format_tokens_t(total)),
                Style::default()
                    .fg(ctx.theme.text)
                    .add_modifier(Modifier::BOLD),
            ));
            if let Some(usual) = step.usual {
                let pct = (total as f64 - usual as f64) / usual.max(1) as f64 * 100.0;
                spans.push(Span::styled(
                    format!("{pct:+.0}% "),
                    tape_deviation_style(pct, ctx.theme),
                ));
                if wide {
                    spans.push(Span::styled(
                        format!("vs usual {usual} t"),
                        Style::default().fg(ctx.theme.fg_muted),
                    ));
                }
            }
        }
        lines.push(Line::from(spans));
    }
    if let Some(step) = pass.steps.last() {
        if let Some(produced) = &step.produced {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{:<label_width$}", "Produced"),
                    Style::default().fg(ctx.theme.fg_muted),
                ),
                Span::styled(produced.clone(), Style::default().fg(ctx.theme.text)),
            ]));
        }
    }
    lines
}

fn tape_status_kind_word(kind: StatusKind) -> &'static str {
    match kind {
        StatusKind::Done => "Done",
        StatusKind::Running | StatusKind::LikelyRunning | StatusKind::RunningOrQueued => "Running",
        StatusKind::Failed | StatusKind::Cancelled | StatusKind::Aborted => "Failed",
        StatusKind::FailedHandled => "Failed",
        StatusKind::Pending | StatusKind::Skipped => "Pending",
    }
}

/// The selected pane's own asked/doing/produced sections (issue
/// #635) -- built from the same [`FocusedStep`]/[`FocusDetail`]
/// model the overview's now pane uses (so issue #612's live agent
/// data, once it lands, fills both panes at once), with the
/// prompt text, the in-flight estimate and the reply read only
/// through `Details::sent`/`received` (never `meta.prompt_sent` or
/// `meta.answer` directly), so a later fix to either flows through
/// here unchanged.
fn flow_selected_step_lines(
    step: &oscilloscope_core::screens::FocusedStep,
    transcript_view: Option<&oscilloscope_core::transcript::TranscriptView>,
    ctx: &DrawCtx,
    now: &str,
) -> Vec<Line<'static>> {
    use oscilloscope_core::model::StatusKind;
    use oscilloscope_core::screens::FocusDetail;
    let dim = Style::default().fg(ctx.theme.dim);
    let mut lines = vec![Line::from(step.breadcrumb.clone())];
    let mut meta = vec![step.kind_label.to_string()];
    if let Some(provider_or_model) = &step.provider_or_model {
        meta.push(provider_or_model.clone());
    }
    lines.push(Line::styled(meta.join(" · "), dim));

    let running = matches!(
        step.status.kind,
        StatusKind::Running | StatusKind::LikelyRunning | StatusKind::RunningOrQueued
    );
    let finished = matches!(
        step.status.kind,
        StatusKind::Done | StatusKind::Failed | StatusKind::FailedHandled
    );

    let asked = match &step.detail {
        FocusDetail::Prompt { snippet, .. } => snippet.clone(),
        FocusDetail::Tool { command } => command.clone(),
        FocusDetail::Agent { asked, .. } => asked.clone(),
        FocusDetail::Plain => None,
    };
    if let Some(asked) = asked {
        lines.push(Line::styled("asked:", dim));
        lines.push(Line::from(asked));
    }

    let mut doing = Vec::new();
    match &step.detail {
        FocusDetail::Prompt {
            tokens, estimated, ..
        } if running => {
            let mut text = "waiting for the reply".to_string();
            if let Some(tokens) = tokens {
                let prefix = if *estimated { "≈" } else { "" };
                text.push_str(&format!(
                    " · {prefix}{} tokens sent",
                    oscilloscope_core::format::format_tokens(*tokens)
                ));
            }
            doing.push(Line::from(text));
        }
        FocusDetail::Tool { .. } if running => {
            let text = match step.elapsed_s {
                Some(elapsed) => format!(
                    "running {}",
                    oscilloscope_core::format::format_duration(elapsed)
                ),
                None => "running".to_string(),
            };
            doing.push(Line::from(text));
        }
        // `live.is_some()` without `running` is an agent node whose
        // own run has already ended (cancelled, failed or aborted,
        // issue #637): `meta.live` is left behind with no
        // `completed_at`, and `stale` (computed from `now` the
        // instant this step was built) is always `true` by then, so
        // this never reads as running -- only `stopped`, with
        // whatever figures the session last reported.
        FocusDetail::Agent {
            live,
            note,
            stale,
            current_tool_elapsed_s,
            ..
        } if running || live.is_some() => {
            if let (Some(since), Some(elapsed)) = (
                step.since_ts
                    .as_deref()
                    .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone)),
                step.elapsed_s,
            ) {
                doing.push(Line::from(format!(
                    "since {since} · {}",
                    oscilloscope_core::format::format_duration(elapsed)
                )));
            }
            match live.as_deref() {
                Some(live) => {
                    if *stale {
                        doing.push(Line::styled("stopped", dim));
                    }
                    // `false`: Flow's own `doing:` header below
                    // already covers this content -- `agent_live_
                    // lines` must not print a second one.
                    doing.extend(agent_live_lines(
                        live,
                        transcript_view,
                        ctx,
                        false,
                        *stale,
                        *current_tool_elapsed_s,
                        now,
                    ));
                }
                None if running => doing.push(Line::styled(*note, dim)),
                None => {}
            }
        }
        _ => {}
    }
    if !doing.is_empty() {
        lines.push(Line::styled("doing:", dim));
        lines.extend(doing);
    }

    let mut produced = Vec::new();
    if finished {
        match &step.detail {
            FocusDetail::Prompt { .. } => {
                if let Some(received) = &step.received {
                    if let Some(snippet) = &received.snippet {
                        produced.push(Line::from(snippet.clone()));
                    }
                }
            }
            FocusDetail::Agent {
                raw_transcript,
                raw_files_changed,
                raw_turns,
                ..
            } => {
                if let Some(value) = &step.value {
                    if !value.is_empty() {
                        produced.push(Line::from(value.clone()));
                    }
                }
                // Issue #637: "produced: meta.raw.transcript,
                // files_changed, the tokens and turns" -- the tokens
                // line below is the same `step.tokens` every other
                // finished tool step's own produced block already
                // shows.
                if let Some(transcript) = raw_transcript {
                    produced.push(Line::styled(transcript.clone(), dim));
                }
                if let Some(files) = raw_files_changed {
                    produced.push(Line::from(format!("files {files}")));
                }
                if let Some(turns) = raw_turns {
                    produced.push(Line::from(format!("turns {turns}")));
                }
            }
            FocusDetail::Tool { .. } | FocusDetail::Plain => {
                if let Some(value) = &step.value {
                    if !value.is_empty() {
                        produced.push(Line::from(value.clone()));
                    }
                }
            }
        }
        if let Some(tokens) = step.tokens {
            produced.push(Line::from(format!(
                "tokens {}",
                oscilloscope_core::format::format_tokens(tokens)
            )));
        }
        if let Some(cost) = step.cost_usd {
            produced.push(Line::from(format!("${cost:.2}")));
        }
        if let Some(error) = &step.error {
            produced.push(Line::styled(
                error.clone(),
                Style::default().fg(ctx.theme.error),
            ));
        }
    } else if running {
        produced.push(Line::styled("(when done)", dim));
    }
    if !produced.is_empty() {
        lines.push(Line::styled("produced:", dim));
        lines.extend(produced);
    }

    lines
}

/// A decision row's own detail (issue #635: "a decision row shows its
/// condition, its mode, the branch it took, and the model's answer
/// for a model-mode condition").
fn decision_detail_lines(
    decision: &oscilloscope_core::screens::SelectedDecision,
    ctx: &DrawCtx,
) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(decision.label.clone())];
    if let Some(condition) = &decision.condition {
        let mode = match condition.mode {
            oscilloscope_core::plan::ConditionMode::Cel => "cel",
            oscilloscope_core::plan::ConditionMode::Model => "model",
        };
        lines.push(Line::styled(
            format!("{mode}  {}", condition.text),
            Style::default().fg(ctx.theme.dim),
        ));
    }
    match &decision.branch {
        Some(branch) => {
            let mut text = format!("→ {branch}");
            if decision.nothing_ran {
                text.push_str(" (nothing ran)");
            }
            if decision.inferred {
                text.push_str("  (inferred)");
            }
            lines.push(Line::from(text));
        }
        None if decision.nothing_ran => lines.push(Line::from("→ none  (inferred)")),
        None => lines.push(Line::styled(
            "not yet evaluated",
            Style::default().fg(ctx.theme.dim),
        )),
    }
    if let Some(evidence) = &decision.inferred_evidence {
        lines.push(Line::styled(
            evidence.clone(),
            Style::default().fg(ctx.theme.dim),
        ));
    }
    if let Some(answer) = &decision.answer {
        lines.push(Line::from(format!("answer  {answer}")));
    }
    lines
}

/// The story pane (DESIGN.md §2.3's ‘Story · Decisions and
/// Failures’): the run's own notable events, oldest first, the
/// latest line followed while scrolled to the bottom -- each line's
/// own clock stays faint, its glyph takes the same role colour the
/// rail's own stations use, and the rest of the line reads as plain
/// text.
fn draw_flow_story_pane(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::screens::FlowScreen,
    ctx: &DrawCtx,
) {
    let border_color = ctx.theme.border;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(ratatui::symbols::border::ROUNDED)
        .title(tape_block_title(
            "Story · Decisions and Failures",
            border_color,
            ctx,
        ))
        .border_style(Style::default().fg(border_color));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    fill_border_rows(frame, inner, border_color);
    let height = inner.height as usize;
    let total = screen.story.len();
    let start = total.saturating_sub(height);
    let content_width = inner.width.saturating_sub(1) as usize;
    let lines: Vec<Line<'static>> = screen.story[start..total]
        .iter()
        .map(|entry| pad_line_to_width(story_entry_line(entry, ctx), content_width, ctx.theme.text))
        .collect();
    frame.render_widget(
        Paragraph::new(lines),
        Rect {
            x: inner.x + 1,
            y: inner.y,
            width: inner.width.saturating_sub(1),
            height: inner.height,
        },
    );
}

/// One Story line's own three-way split (DESIGN.md §2.3): the clock
/// (`fg.faint`), the glyph (the rail's own role colour for `■`/`◆`/
/// `⟳`/`✗`, plain otherwise), and the rest of the line in
/// `fg.default`.
/// The Story pane's own `run started · <run id>` (DESIGN.md §2.3):
/// a short run id, its own first hyphen-delimited segment, never
/// the full UUID `--log` itself writes.
fn shorten_run_id(text: &str) -> String {
    let Some(after) = text.strip_prefix("run started  ") else {
        return text.to_string();
    };
    let short = after.split('-').next().unwrap_or(after);
    format!("run started · {short}")
}

fn story_entry_line(entry: &oscilloscope_core::flow::StoryEntry, ctx: &DrawCtx) -> Line<'static> {
    let text = &entry.text;
    let (clock, rest) = match text.split_once(' ') {
        Some((clock, rest)) if clock.len() == 8 && clock.as_bytes()[2] == b':' => {
            (Some(clock), rest)
        }
        _ => (None, text.as_str()),
    };
    let mut chars = rest.chars();
    let glyph = chars.next();
    let mut rest_after_glyph: String = chars.collect();
    let glyph_space = if rest_after_glyph.starts_with(' ') {
        rest_after_glyph.remove(0);
        " "
    } else {
        ""
    };
    let after_glyph: String = shorten_run_id(&rest_after_glyph);
    let glyph_style = match glyph {
        Some('■') => Style::default().fg(ctx.theme.fg_muted),
        Some('◆') | Some('⟳') => Style::default()
            .fg(ctx.theme.accent_secondary)
            .add_modifier(Modifier::BOLD),
        Some('✗') => Style::default().fg(ctx.theme.status_failed),
        Some('▸') if rest_after_glyph.starts_with("warning: ") => {
            Style::default().fg(ctx.theme.warning)
        }
        Some('▸') => Style::default().fg(ctx.theme.status_info),
        _ => Style::default().fg(ctx.theme.text),
    };
    let mut spans = Vec::new();
    if let Some(clock) = clock {
        spans.push(Span::styled(
            format!("{clock} "),
            Style::default().fg(ctx.theme.dim),
        ));
    }
    if let Some(glyph) = glyph {
        spans.push(Span::styled(format!("{glyph}{glyph_space}"), glyph_style));
    }
    spans.push(Span::styled(
        after_glyph,
        Style::default().fg(ctx.theme.text),
    ));
    Line::from(spans)
}

// ---------------------------------------------------------------------
// The Flow rail (screen 2, issue #691, part 3 of #677)
// ---------------------------------------------------------------------

use oscilloscope_core::rail::{self, EdgeStyle, Fork, ForkKind, Lane, RailItem, Station};

fn flow_edge_color(style: EdgeStyle, theme: &Theme) -> Color {
    match style {
        EdgeStyle::Past => theme.border,
        EdgeStyle::Live => theme.accent_primary,
        EdgeStyle::Ahead => theme.dim,
    }
}

/// Whether `fork`'s own lanes draw folded into their own band
/// (DESIGN.md §2.3/§9: "below 100 columns the lanes start folded",
/// "a loop four or more wide starts folded") -- every fork with four
/// or more lanes, a loop or a parallel `dynamic` alike, starts folded
/// the same way; a decision's own two lanes (the taken branch and
/// the untaken ghost) never fold at all.
fn flow_fork_folded(fork: &Fork, app: &App, area_width: u16) -> bool {
    if fork.kind == ForkKind::Decision {
        return false;
    }
    if let Some(&open) = app.flow_lanes_open.get(&fork.path) {
        return open == 0;
    }
    fork.lanes.len() >= 4 || area_width < 100
}

/// The at most three lanes an open fork actually draws at once
/// (#677 §9.3's settled "`↑↓` scrolls them"): a decision's own two
/// lanes always draw in full -- a decision never folds at all, so it
/// never needs a window either. Any other fork whose lanes run past
/// three (a loop's passes, or a parallel `dynamic` with four or more
/// children) windows the same way. The window slides to keep
/// whichever lane holds the current selection in view, the same
/// way the rail itself slides to keep Now in view; with nothing of
/// this fork's own selected, it slides to the first running lane
/// instead, falling back to the very first lane once nothing in it
/// is running either.
fn flow_lane_window<'a>(fork: &'a Fork, app: &App) -> &'a [Lane] {
    const VISIBLE: usize = 3;
    if fork.kind == ForkKind::Decision || fork.lanes.len() <= VISIBLE {
        return &fork.lanes;
    }
    // The selection is usually one of this lane's own *children*
    // (a pass's own step, say), never the lane's own path itself --
    // matching only `s.path == selected` against the stations
    // already inside it missed every pass but the one holding the
    // exact leaf selected, which a bare pass selection (issue #691's
    // own follow-up) never is at all, so the window never moved.
    let anchor = app
        .selected_path
        .as_deref()
        .and_then(|selected| {
            fork.lanes.iter().position(|lane| {
                lane.path.as_deref().is_some_and(|p| {
                    selected == p
                        || selected
                            .strip_prefix(p)
                            .is_some_and(|rest| rest.starts_with('.'))
                })
            })
        })
        .or_else(|| fork.lanes.iter().position(|l| l.running))
        .unwrap_or(0);
    let max_start = fork.lanes.len() - VISIBLE;
    let start = anchor.saturating_sub(VISIBLE - 1).min(max_start);
    &fork.lanes[start..start + VISIBLE]
}

fn flow_item_width(item: &RailItem, app: &App, area_width: u16) -> usize {
    match item {
        RailItem::Station(_) => rail::STATION_WIDTH,
        RailItem::Fork(fork) => flow_fork_width(fork, app, area_width),
    }
}

fn flow_lane_width(items: &[RailItem], app: &App, area_width: u16) -> usize {
    if items.is_empty() {
        return rail::STATION_WIDTH;
    }
    items
        .iter()
        .map(|it| flow_item_width(it, app, area_width))
        .sum::<usize>()
        + rail::NORMAL_CONNECTOR * (items.len() - 1)
}

fn flow_fork_width(fork: &Fork, app: &App, area_width: u16) -> usize {
    if flow_fork_folded(fork, app, area_width) {
        rail::FOLD_BAND_WIDTH
    } else {
        let max_lane = fork
            .lanes
            .iter()
            .map(|l| flow_lane_width(&l.items, app, area_width))
            .max()
            .unwrap_or(rail::STATION_WIDTH);
        rail::FORK_LEAD + max_lane + rail::FORK_TAIL
    }
}

/// A lane's own painted height: three rows, unless one of its own
/// items is itself an unfolded fork taller than that (a nested `if`
/// or loop inside a loop's own pass, say) -- the outer fork's own
/// next lane must start after that full height, never a flat three
/// rows, or it paints over the inner fork's own lower lanes.
fn flow_lane_height(items: &[RailItem], app: &App, area_width: u16) -> u16 {
    items
        .iter()
        .map(|it| flow_item_height(it, app, area_width))
        .max()
        .unwrap_or(3)
}

fn flow_item_height(item: &RailItem, app: &App, area_width: u16) -> u16 {
    match item {
        RailItem::Station(_) => 3,
        RailItem::Fork(fork) => flow_fork_height(fork, app, area_width),
    }
}

fn flow_fork_height(fork: &Fork, app: &App, area_width: u16) -> u16 {
    if flow_fork_folded(fork, app, area_width) {
        return 3;
    }
    flow_lane_window(fork, app)
        .iter()
        .map(|l| flow_lane_height(&l.items, app, area_width))
        .sum()
}

/// `items`' own `(start, end)` full-coordinate span, in order --
/// no gap at all around a fork (its own lead-in/lead-out already is
/// one), DESIGN.md §2.3's own connector rule for every other pair.
fn flow_layout(items: &[RailItem], app: &App, area_width: u16) -> Vec<(usize, usize)> {
    let mut out = Vec::with_capacity(items.len());
    let mut x = 0usize;
    for (i, item) in items.iter().enumerate() {
        let w = flow_item_width(item, app, area_width);
        out.push((x, x + w));
        x += w;
        let is_fork = matches!(item, RailItem::Fork(_));
        let next_is_fork = items
            .get(i + 1)
            .map(|n| matches!(n, RailItem::Fork(_)))
            .unwrap_or(false);
        if i + 1 < items.len() && !is_fork && !next_is_fork {
            x += rail::NORMAL_CONNECTOR;
        }
    }
    out
}

fn flow_total_width(items: &[RailItem], app: &App, area_width: u16) -> usize {
    flow_layout(items, app, area_width)
        .last()
        .map(|&(_, e)| e)
        .unwrap_or(0)
}

fn flow_lane_has_live(items: &[RailItem]) -> bool {
    items.iter().any(|it| match it {
        RailItem::Station(s) => rail::edge_style(s.status) == EdgeStyle::Live,
        RailItem::Fork(f) => f.lanes.iter().any(|l| flow_lane_has_live(&l.items)),
    })
}

/// The `▾ Now` indicator's own column (DESIGN.md §2.3): the
/// leftmost currently-running leaf anywhere on the rail -- a
/// decision or loop station itself never counts (its own fork is
/// what actually carries the live edge), and a folded fork with a
/// live lane inside reads at its own band's content start instead of
/// descending into lanes it isn't drawing.
fn flow_now_arrow_x(items: &[RailItem], app: &App, area_width: u16) -> Option<usize> {
    let layout = flow_layout(items, app, area_width);
    for (item, &(x_start, _)) in items.iter().zip(layout.iter()) {
        match item {
            RailItem::Station(s) => {
                if !matches!(
                    s.row_kind,
                    oscilloscope_core::render::RowKind::If
                        | oscilloscope_core::render::RowKind::Loop
                ) && rail::edge_style(s.status) == EdgeStyle::Live
                {
                    return Some(x_start);
                }
            }
            RailItem::Fork(fork) => {
                let folded = flow_fork_folded(fork, app, area_width);
                let has_live = fork.lanes.iter().any(|l| flow_lane_has_live(&l.items));
                if !has_live {
                    continue;
                }
                if folded {
                    return Some(x_start + 3);
                }
                if let Some(lane0) = fork.lanes.first() {
                    if let Some(rel) = flow_now_arrow_x(&lane0.items, app, area_width) {
                        return Some(x_start + rail::FORK_LEAD + rel);
                    }
                }
                return Some(x_start + rail::FORK_LEAD);
            }
        }
    }
    None
}

/// The rail's own follow rule (tui-design.md §2.3): never
/// scrolls while Now is in view; once it would leave, scrolls to the
/// top-level item boundary closest to putting Now at 60% of the
/// visible width. A manual scroll (`follow` already off) turns
/// follow back on the instant it lands Now back in view on its own,
/// the same rule the Tape's own selection already follows.
fn flow_resolve_scroll(
    items: &[RailItem],
    app: &mut App,
    area_width: u16,
    visible_width: usize,
) -> usize {
    let total = flow_total_width(items, app, area_width);
    let max_scroll = total.saturating_sub(visible_width);
    let arrow = flow_now_arrow_x(items, app, area_width);
    let in_view = |scroll: usize, arrow: usize| scroll <= arrow && arrow < scroll + visible_width;
    if app.follow {
        if let Some(arrow) = arrow {
            if !in_view(app.flow_scroll, arrow) {
                let target = (arrow as f64 - visible_width as f64 * 0.6).max(0.0) as usize;
                let layout = flow_layout(items, app, area_width);
                let mut best = 0usize;
                let mut best_dist = target;
                for &(start, _) in std::iter::once(&(0, 0)).chain(layout.iter()) {
                    let dist = start.abs_diff(target);
                    if dist < best_dist {
                        best = start;
                        best_dist = dist;
                    }
                }
                app.flow_scroll = best.min(max_scroll);
            }
        }
    } else {
        // The land-back rule (DESIGN.md's own Tape precedent, issue
        // #691's follow-up): `↑↓` moving away turns follow off, but
        // it resumes on its own the instant the selection it left
        // behind is, again, the row follow would itself pick -- not
        // only right at the keypress (`flow_select_lane`'s own
        // one-shot check), but on any later redraw too, since a
        // loop's own "the running pass" is a moving target: the
        // exact pass `↑↓` landed on can start running again several
        // passes later, with no further key pressed at all.
        let rows: Vec<&oscilloscope_core::render::Row> = app.flow_rows.iter().collect();
        let land_back = app.lands_back_on_the_followed_row(&rows);
        if app.flow_manual_scroll || land_back {
            if let Some(arrow) = arrow {
                if in_view(app.flow_scroll, arrow) {
                    app.follow = true;
                }
            }
        }
    }
    // One-shot: only the draw right after a ←→ key press may land
    // follow back on through the manual-scroll flag (the key handler
    // sets this); any other redraw (a clock tick, `f` turning follow
    // off) must never flip it back on just because Now happens to
    // already be in view -- only the land-back rule above, or a
    // fresh manual scroll, may do that.
    app.flow_manual_scroll = false;
    app.flow_scroll = app.flow_scroll.min(max_scroll);
    app.flow_scroll
}

/// Writes `text` into `buf` at the rail's own full-coordinate `x`,
/// clipped to whatever part of it actually falls in
/// `[scroll, scroll + visible_width)` -- the rail's own "runs off the
/// right edge" rule (DESIGN.md §2.3), one cell at a time so a
/// station cut in half by the edge still prints its own visible
/// half.
#[allow(clippy::too_many_arguments)]
fn flow_put(
    buf: &mut ratatui::buffer::Buffer,
    area: Rect,
    scroll: usize,
    visible_width: usize,
    x_full: usize,
    y_rel: u16,
    text: &str,
    style: Style,
) {
    if y_rel >= area.height {
        return;
    }
    for (i, ch) in text.chars().enumerate() {
        let x_full_i = x_full + i;
        if x_full_i < scroll || x_full_i >= scroll + visible_width {
            continue;
        }
        let x = area.x + (x_full_i - scroll) as u16;
        let y = area.y + y_rel;
        if x >= area.x + area.width {
            continue;
        }
        buf.set_string(x, y, ch.to_string(), style);
    }
}

/// Draws `spans` left to right from `x`, padding with the last
/// span's own style up to `min_width` cells (DESIGN.md §2.3's own
/// frames: a station's own trailing padding, and the gap before a
/// plain connector, share whichever span's style was drawn right
/// before them) -- returns that trailing style, so a caller painting
/// the connector gap after this row can extend it.
#[allow(clippy::too_many_arguments)]
fn flow_paint_row(
    buf: &mut ratatui::buffer::Buffer,
    area: Rect,
    scroll: usize,
    visible_width: usize,
    x: usize,
    y: u16,
    spans: &[(String, Style)],
    min_width: usize,
) -> Style {
    let mut cursor = x;
    let mut last_style = Style::default();
    for (text, style) in spans {
        flow_put(buf, area, scroll, visible_width, cursor, y, text, *style);
        cursor += text.chars().count();
        last_style = *style;
    }
    let used = cursor - x;
    if used < min_width {
        flow_put(
            buf,
            area,
            scroll,
            visible_width,
            cursor,
            y,
            &" ".repeat(min_width - used),
            last_style,
        );
    }
    last_style
}

fn flow_headline_spans(
    headline: &oscilloscope_core::rail::Headline,
    status: StatusKind,
    ctx: &DrawCtx,
) -> Vec<(String, Style)> {
    use oscilloscope_core::rail::Headline;
    let muted = Style::default().fg(ctx.theme.fg_muted);
    let faint = Style::default().fg(ctx.theme.dim);
    let default_text = Style::default().fg(ctx.theme.text);
    match headline {
        Headline::Clock(c) => vec![(c.clone(), faint)],
        Headline::Tokens { total, usual } => {
            let mut spans = vec![(
                format!("{} ", oscilloscope_core::format::format_tokens_t(*total)),
                muted,
            )];
            if let Some(usual) = usual {
                let pct = (*total as f64 - *usual as f64) / (*usual).max(1) as f64 * 100.0;
                spans.push((format!("{pct:+.0}%"), tape_deviation_style(pct, ctx.theme)));
            }
            spans
        }
        Headline::PassTokens { label, total } => vec![(
            format!(
                "{label} {} t",
                oscilloscope_core::format::format_tokens_thousands(*total)
            ),
            muted,
        )],
        Headline::Duration(d) => {
            let style = if matches!(
                status,
                StatusKind::Running | StatusKind::LikelyRunning | StatusKind::RunningOrQueued
            ) {
                default_text
            } else {
                muted
            };
            vec![(oscilloscope_core::format::format_duration(*d), style)]
        }
        Headline::Branch(branch) => {
            let text = match branch {
                Some(b) => format!("→ {b}"),
                None => "→ …".to_string(),
            };
            vec![(text, default_text)]
        }
        Headline::LoopProgress { done, total } => {
            let text = match total {
                Some(t) => format!("{done}/{t} passes"),
                None => format!("{done} passes"),
            };
            vec![(text, default_text.add_modifier(Modifier::BOLD))]
        }
        Headline::PassLabel(label) => vec![(label.clone(), faint)],
        Headline::ToolAhead(cmd) => {
            if cmd.is_empty() {
                vec![("tool".to_string(), faint)]
            } else {
                vec![(format!("tool · {cmd}"), faint)]
            }
        }
        Headline::QueuedCommand(cmd) => vec![(cmd.clone(), faint)],
        Headline::ContainerAhead(kind) => vec![(kind.clone(), faint)],
        Headline::UsualTokens(usual) => {
            let text = match usual {
                Some(u) => format!("usual {u}"),
                None => "usual —".to_string(),
            };
            vec![(text, faint)]
        }
        Headline::NotTaken => vec![("not taken".to_string(), faint)],
        Headline::Blank => vec![(String::new(), faint)],
    }
}

fn flow_detail_spans(
    detail: &oscilloscope_core::rail::Detail,
    status: StatusKind,
    unit: u64,
    ctx: &DrawCtx,
) -> Vec<(String, Style)> {
    use oscilloscope_core::rail::Detail;
    let muted = Style::default().fg(ctx.theme.fg_muted);
    let faint = Style::default().fg(ctx.theme.dim);
    let accent = Style::default().fg(ctx.theme.accent_primary);
    match detail {
        Detail::Engine(e) => vec![(e.clone(), faint)],
        Detail::Bar { total, unit: u } => {
            let effective_unit = if *u > 0 { *u } else { unit };
            let filled = ((*total as f64 / effective_unit.max(1) as f64).round() as usize).min(8);
            let empty = 8 - filled;
            vec![(
                format!("{}{}  ", "▰".repeat(filled), "▱".repeat(empty)),
                accent,
            )]
        }
        Detail::Command(cmd) => {
            let style = if matches!(
                status,
                StatusKind::Running | StatusKind::LikelyRunning | StatusKind::RunningOrQueued
            ) {
                muted
            } else {
                faint
            };
            vec![(cmd.clone(), style)]
        }
        Detail::Condition(c) => vec![(c.clone(), faint)],
        Detail::Tokens(t) => vec![(
            format!(
                "{} t",
                oscilloscope_core::format::format_tokens_thousands(*t)
            ),
            muted,
        )],
        Detail::UsualTokens(u) => {
            let text = match u {
                Some(v) => format!("usual {v}"),
                None => "usual —".to_string(),
            };
            vec![(text, faint)]
        }
        Detail::Queued => vec![("queued".to_string(), faint)],
        Detail::Blank => vec![(String::new(), faint)],
    }
}

/// One station's own three lines (DESIGN.md §2.3's ten-cell station)
/// -- returns the headline's and detail's own trailing style, so a
/// plain connector right after this station can extend it across its
/// own blank gap on those two rows (name row connectors draw real
/// dashes instead, never blank).
#[allow(clippy::too_many_arguments)]
fn flow_paint_station(
    buf: &mut ratatui::buffer::Buffer,
    area: Rect,
    scroll: usize,
    visible_width: usize,
    x: usize,
    y0: u16,
    station: &Station,
    unit: u64,
    ctx: &DrawCtx,
) -> (Style, Style) {
    let (glyph, glyph_style) = if station.is_edge {
        ('■', Style::default().fg(ctx.theme.fg_muted))
    } else {
        let style = if station.row_kind == oscilloscope_core::render::RowKind::Loop
            && rail::edge_style(station.status) == EdgeStyle::Live
        {
            // DESIGN.md §2.3: the rail's own running loop reads in
            // the live edge's own accent, not the decision/loop
            // accent every other screen's glyph table always uses.
            Style::default()
                .fg(ctx.theme.accent_primary)
                .add_modifier(Modifier::BOLD)
        } else {
            tape_glyph_style(station.row_kind, station.status, ctx.theme)
        };
        (tape_glyph(station.row_kind, station.status), style)
    };
    let name = truncate_label(&station.name, rail::STATION_WIDTH.saturating_sub(2));
    flow_paint_row(
        buf,
        area,
        scroll,
        visible_width,
        x,
        y0,
        &[(format!("{glyph} {name}"), glyph_style)],
        rail::STATION_WIDTH,
    );
    let headline_last = flow_paint_row(
        buf,
        area,
        scroll,
        visible_width,
        x,
        y0 + 1,
        &truncate_flow_spans(
            flow_headline_spans(&station.headline, station.status, ctx),
            rail::STATION_WIDTH,
        ),
        rail::STATION_WIDTH,
    );
    let detail_last = flow_paint_row(
        buf,
        area,
        scroll,
        visible_width,
        x,
        y0 + 2,
        &truncate_flow_spans(
            flow_detail_spans(&station.detail, station.status, unit, ctx),
            rail::STATION_WIDTH,
        ),
        rail::STATION_WIDTH,
    );
    (headline_last, detail_last)
}

/// Cuts `spans`' own combined text to `max_chars`, the same
/// character-safe, ellipsis-at-the-end truncation a station's own
/// name already gets (`truncate_label`) -- a headline or detail row
/// (`tool · <cmd>`, a queued command, a `use` path) otherwise spills
/// past the station's own ten cells into whatever draws next, and on
/// a fork's last lane nothing draws there to cover it back up.
fn truncate_flow_spans(spans: Vec<(String, Style)>, max_chars: usize) -> Vec<(String, Style)> {
    let total: usize = spans.iter().map(|(text, _)| text.chars().count()).sum();
    if total <= max_chars {
        return spans;
    }
    if max_chars == 0 {
        return vec![(String::new(), Style::default())];
    }
    let mut budget = max_chars.saturating_sub(1);
    let mut out = Vec::new();
    for (text, style) in spans {
        if budget == 0 {
            break;
        }
        let count = text.chars().count();
        if count <= budget {
            budget -= count;
            out.push((text, style));
        } else {
            out.push((text.chars().take(budget).collect(), style));
            budget = 0;
        }
    }
    out.push(("…".to_string(), Style::default()));
    out
}

#[allow(clippy::too_many_arguments)]
fn flow_paint_ghost(
    buf: &mut ratatui::buffer::Buffer,
    area: Rect,
    scroll: usize,
    visible_width: usize,
    x: usize,
    y0: u16,
    name: &str,
    ctx: &DrawCtx,
) {
    let style = Style::default().fg(ctx.theme.dim);
    flow_paint_row(
        buf,
        area,
        scroll,
        visible_width,
        x,
        y0,
        &[(
            format!("· {}", truncate_label(name, rail::STATION_WIDTH - 2)),
            style,
        )],
        rail::STATION_WIDTH,
    );
    flow_paint_row(
        buf,
        area,
        scroll,
        visible_width,
        x,
        y0 + 1,
        &[("not taken".to_string(), style)],
        rail::STATION_WIDTH,
    );
    // The ghost's own third line is blank, left untouched (DESIGN.md
    // §2.3's own frame: not even a faint fill) -- unlike its first
    // two, which fill with its own dim text all the way to wherever
    // its own bridging gap with the rail's own next real content
    // ends.
    let _ = y0;
}

/// A flat run of stations and forks, drawn left to right from `x`
/// along the three rows starting at `y0` -- used both for the rail's
/// own top-level sequence and, recursively, for one lane's own
/// content. Returns the ending `x`.
#[allow(clippy::too_many_arguments)]
fn flow_paint_sequence(
    buf: &mut ratatui::buffer::Buffer,
    area: Rect,
    scroll: usize,
    visible_width: usize,
    items: &[RailItem],
    x: usize,
    y0: u16,
    app: &App,
    area_width: u16,
    unit: u64,
    ctx: &DrawCtx,
) -> (usize, Style, Style) {
    let mut cursor = x;
    let neutral = Style::default().fg(ctx.theme.text);
    let mut last_styles = (neutral, neutral);
    for (i, item) in items.iter().enumerate() {
        match item {
            RailItem::Station(station) => {
                let (h_last, d_last) = flow_paint_station(
                    buf,
                    area,
                    scroll,
                    visible_width,
                    cursor,
                    y0,
                    station,
                    unit,
                    ctx,
                );
                last_styles = (h_last, d_last);
                cursor += rail::STATION_WIDTH;
                if let Some(RailItem::Station(next)) = items.get(i + 1) {
                    let style = Style::default()
                        .fg(flow_edge_color(rail::edge_style(next.status), ctx.theme));
                    flow_put(buf, area, scroll, visible_width, cursor, y0, "── ", style);
                    flow_put(
                        buf,
                        area,
                        scroll,
                        visible_width,
                        cursor,
                        y0 + 1,
                        "   ",
                        h_last,
                    );
                    flow_put(
                        buf,
                        area,
                        scroll,
                        visible_width,
                        cursor,
                        y0 + 2,
                        "   ",
                        d_last,
                    );
                    cursor += rail::NORMAL_CONNECTOR;
                }
            }
            RailItem::Fork(fork) => {
                let next_is_ahead = items.get(i + 1).is_some_and(flow_item_is_ahead);
                let (end, h_last, d_last) = flow_paint_fork(
                    buf,
                    area,
                    scroll,
                    visible_width,
                    cursor,
                    y0,
                    fork,
                    app,
                    area_width,
                    next_is_ahead,
                    unit,
                    last_styles,
                    ctx,
                );
                cursor = end;
                last_styles = (h_last, d_last);
            }
        }
    }
    (cursor, last_styles.0, last_styles.1)
}

fn flow_item_is_ahead(item: &RailItem) -> bool {
    match item {
        RailItem::Station(s) => rail::edge_style(s.status) == EdgeStyle::Ahead,
        RailItem::Fork(f) => rail::edge_style(f.source_status) == EdgeStyle::Ahead,
    }
}

/// A fork's own box (DESIGN.md §2.3): the tee/join on the top lane's
/// own name row, walls down its own two continuation rows, and every
/// other lane in its own three-row band below with a corner cap on
/// its name row and (while it isn't the last lane) a wall continuing
/// through the other two. Folded, the whole thing is one sixteen-
/// character band instead. Returns the ending `x` and the headline/
/// detail rows' own trailing style, for a caller that needs to
/// extend it into whatever follows.
#[allow(clippy::too_many_arguments)]
fn flow_paint_fork(
    buf: &mut ratatui::buffer::Buffer,
    area: Rect,
    scroll: usize,
    visible_width: usize,
    x: usize,
    y0: u16,
    fork: &Fork,
    app: &App,
    area_width: u16,
    next_is_ahead: bool,
    unit: u64,
    prior_styles: (Style, Style),
    ctx: &DrawCtx,
) -> (usize, Style, Style) {
    let tee_color = flow_edge_color(rail::edge_style(fork.source_status), ctx.theme);
    let tee_style = Style::default().fg(tee_color);
    let faint = Style::default().fg(ctx.theme.dim);
    let bold_text = Style::default()
        .fg(ctx.theme.text)
        .add_modifier(Modifier::BOLD);
    if flow_fork_folded(fork, app, area_width) {
        flow_put(
            buf,
            area,
            scroll,
            visible_width,
            x,
            y0 + 1,
            "   ",
            prior_styles.0,
        );
        flow_put(
            buf,
            area,
            scroll,
            visible_width,
            x,
            y0 + 2,
            "   ",
            prior_styles.1,
        );
        flow_put(buf, area, scroll, visible_width, x, y0, "══ ", tee_style);
        let content = format!("{} lanes + ", fork.lanes.len());
        flow_paint_row(
            buf,
            area,
            scroll,
            visible_width,
            x + 3,
            y0,
            &[(
                content,
                Style::default()
                    .fg(ctx.theme.accent_primary)
                    .add_modifier(Modifier::BOLD),
            )],
            rail::STATION_WIDTH,
        );
        let tail_style = if next_is_ahead { faint } else { tee_style };
        flow_put(
            buf,
            area,
            scroll,
            visible_width,
            x + 13,
            y0,
            "══ ",
            tail_style,
        );
        let running = fork.lanes.iter().filter(|l| l.running).count();
        let waiting = fork.lanes.iter().filter(|l| l.waiting).count();
        flow_paint_row(
            buf,
            area,
            scroll,
            visible_width,
            x + 3,
            y0 + 1,
            &[(format!("● {running} run"), bold_text)],
            rail::STATION_WIDTH,
        );
        flow_paint_row(
            buf,
            area,
            scroll,
            visible_width,
            x + 3,
            y0 + 2,
            &[(format!("· {waiting} wait"), faint)],
            rail::STATION_WIDTH,
        );
        flow_put(
            buf,
            area,
            scroll,
            visible_width,
            x + 13,
            y0 + 1,
            "   ",
            bold_text,
        );
        flow_put(
            buf,
            area,
            scroll,
            visible_width,
            x + 13,
            y0 + 2,
            "   ",
            faint,
        );
        return (x + rail::FOLD_BAND_WIDTH, bold_text, faint);
    }
    flow_put(buf, area, scroll, visible_width, x, y0, "─┬─ ", tee_style);
    let content_x = x + rail::FORK_LEAD;
    let max_lane = fork
        .lanes
        .iter()
        .map(|l| flow_lane_width(&l.items, app, area_width))
        .max()
        .unwrap_or(rail::STATION_WIDTH);
    let wall_x = x + 1;
    let tail_x = content_x + max_lane;
    let right_wall_x = tail_x + 1;
    let mut final_headline_style = faint;
    let mut final_detail_style = faint;
    let visible_lanes = flow_lane_window(fork, app);
    let mut cursor_y = y0;
    for (k, lane) in visible_lanes.iter().enumerate() {
        let lane_height = flow_lane_height(&lane.items, app, area_width);
        let row_y = cursor_y;
        cursor_y += lane_height;
        let is_last = k == visible_lanes.len() - 1;
        let (lane_end, h_last, d_last) = match &lane.ghost {
            Some(name) => {
                flow_paint_ghost(
                    buf,
                    area,
                    scroll,
                    visible_width,
                    content_x,
                    row_y,
                    name,
                    ctx,
                );
                (content_x + rail::STATION_WIDTH, faint, faint)
            }
            None => flow_paint_sequence(
                buf,
                area,
                scroll,
                visible_width,
                &lane.items,
                content_x,
                row_y,
                app,
                area_width,
                unit,
                ctx,
            ),
        };
        if lane_end < content_x + max_lane {
            let pad = " ".repeat(content_x + max_lane - lane_end);
            for dy in 0..lane_height {
                flow_put(
                    buf,
                    area,
                    scroll,
                    visible_width,
                    lane_end,
                    row_y + dy,
                    &pad,
                    faint,
                );
            }
        }
        if k == 0 {
            // The top lane's own left wall mirrors the tee's own
            // live/past/ahead colour (DESIGN.md's own frames: a
            // running loop's left wall reads live, a resolved
            // decision's reads faint like every other wall) -- the
            // right wall never does, since it frames whatever comes
            // next, not the fork's own source.
            let left_wall_style = if rail::edge_style(fork.source_status) == EdgeStyle::Live {
                tee_style
            } else {
                faint
            };
            for dy in 1..lane_height {
                let prior_style = if dy == 1 {
                    prior_styles.0
                } else {
                    prior_styles.1
                };
                flow_put(
                    buf,
                    area,
                    scroll,
                    visible_width,
                    x,
                    row_y + dy,
                    " ",
                    prior_style,
                );
                flow_put(
                    buf,
                    area,
                    scroll,
                    visible_width,
                    wall_x,
                    row_y + dy,
                    "│  ",
                    left_wall_style,
                );
            }
            let tail_style = if next_is_ahead { faint } else { tee_style };
            flow_put(
                buf,
                area,
                scroll,
                visible_width,
                tail_x,
                y0,
                "─┴── ",
                tail_style,
            );
            for dy in 1..lane_height {
                let trailing_style = if dy == 1 { h_last } else { d_last };
                flow_put(
                    buf,
                    area,
                    scroll,
                    visible_width,
                    tail_x,
                    row_y + dy,
                    " ",
                    trailing_style,
                );
                flow_put(
                    buf,
                    area,
                    scroll,
                    visible_width,
                    right_wall_x,
                    row_y + dy,
                    "│   ",
                    faint,
                );
            }
            continue;
        }
        let left_corner = if is_last { '└' } else { '├' };
        let right_corner = if is_last { '┘' } else { '┤' };
        let cap_style = if lane.running {
            Style::default().fg(ctx.theme.accent_primary)
        } else {
            faint
        };
        flow_put(
            buf,
            area,
            scroll,
            visible_width,
            wall_x,
            row_y,
            &format!("{left_corner}─ "),
            cap_style,
        );
        flow_put(buf, area, scroll, visible_width, tail_x, row_y, "─", faint);
        flow_put(
            buf,
            area,
            scroll,
            visible_width,
            right_wall_x,
            row_y,
            &format!("{right_corner}"),
            faint,
        );
        if !is_last {
            for dy in 1..lane_height {
                flow_put(
                    buf,
                    area,
                    scroll,
                    visible_width,
                    wall_x,
                    row_y + dy,
                    "│  ",
                    faint,
                );
                let trailing_style = if dy == 1 { h_last } else { d_last };
                flow_put(
                    buf,
                    area,
                    scroll,
                    visible_width,
                    tail_x,
                    row_y + dy,
                    " ",
                    trailing_style,
                );
                flow_put(
                    buf,
                    area,
                    scroll,
                    visible_width,
                    right_wall_x,
                    row_y + dy,
                    "│   ",
                    faint,
                );
            }
        }
        if is_last {
            final_headline_style = faint;
            final_detail_style = faint;
        }
    }
    (
        tail_x + rail::NORMAL_CONNECTOR + 2,
        final_headline_style,
        final_detail_style,
    )
}

fn flow_key_bar_items(area_width: u16) -> Vec<(&'static str, &'static str)> {
    let mut items = vec![("←→", "Scroll")];
    // Below the three-pane threshold (DESIGN.md's own §5, 100×30,
    // the same one the Story pane itself drops below), `↑↓ Lane` goes
    // too -- there's no Story pane left for it to change the
    // selection of.
    if area_width >= 100 {
        items.push(("↑↓", "Lane"));
    }
    items.push(("+/-", "Lanes"));
    items.push(("f", "Follow"));
    items.push(("enter", "Open"));
    if area_width >= 80 {
        items.push(("0-3", "Screens"));
    }
    items.push(("?", "Help"));
    items.push(("q", "Quit"));
    items
}

/// §3.2's status line, shared with the Signal's own.
fn draw_flow_status_line(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::screens::FlowScreen,
    header: &oscilloscope_core::render::Header,
    ctx: &DrawCtx,
    now: &str,
) {
    if let Some(spans) = shared_run_chrome_status_spans(&screen.head, header, ctx) {
        frame.render_widget(Paragraph::new(Line::from(spans)), area);
        return;
    }
    let Some(last) = &screen.last_finished else {
        frame.render_widget(Paragraph::new(Line::from("")), area);
        return;
    };
    let glyph = tape_glyph(last.kind, last.status);
    let age = last
        .ts
        .as_deref()
        .and_then(|t| oscilloscope_core::observe::duration_seconds(t, now));
    let tokens = last
        .tokens
        .map(|(sent, received)| {
            format!(
                " · ↑{} ↓{}",
                oscilloscope_core::format::format_tokens_thousands(sent),
                oscilloscope_core::format::format_tokens_thousands(received)
            )
        })
        .unwrap_or_default();
    let age_text = age
        .map(|a| format!(" · {} ago", oscilloscope_core::format::format_duration(a)))
        .unwrap_or_default();
    let dim = Style::default().fg(ctx.theme.dim);
    let spans = vec![
        Span::styled(
            format!(" {glyph} "),
            tape_glyph_style(last.kind, last.status, ctx.theme),
        ),
        Span::styled(
            format!("{} finished ", last.label),
            Style::default().fg(ctx.theme.text),
        ),
        Span::styled(format!("{tokens}{age_text}").trim_start().to_string(), dim),
    ];
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The `▾ Now <clock> · <elapsed>` line (DESIGN.md §2.3), right-
/// aligned above whichever running stations the rail's own content
/// row below it actually lands under.
fn flow_now_text(header: &oscilloscope_core::render::Header, now: &str, ctx: &DrawCtx) -> String {
    let clock = oscilloscope_core::format::format_clock_time(now, ctx.zone).unwrap_or_default();
    format!(
        "▾ Now {clock} · {}",
        oscilloscope_core::format::format_duration(header.elapsed_s)
    )
}

/// The indicator line (DESIGN.md §2.3's `◂ Start   ▸ flaky · summary
/// · End`): every top-level item not fully inside
/// `[scroll, scroll + visible_width)`, named on whichever side it
/// falls off of -- `Start`/`End` stand in on their own side when
/// nothing there is actually cut off, so the line always names where
/// the run began and will end.
fn flow_indicator_line(
    items: &[RailItem],
    app: &App,
    area_width: u16,
    scroll: usize,
    visible_width: usize,
    ctx: &DrawCtx,
) -> Line<'static> {
    let layout = flow_layout(items, app, area_width);
    let names = |item: &RailItem| -> Option<String> {
        match item {
            RailItem::Station(s) => Some(s.name.clone()),
            RailItem::Fork(_) => None,
        }
    };
    let mut left: Vec<String> = items
        .iter()
        .zip(layout.iter())
        .filter(|&(_, &(start, _))| start < scroll)
        .filter_map(|(item, _)| names(item))
        .collect();
    if left.is_empty() {
        if let Some(first) = items.first().and_then(names) {
            left.push(first);
        }
    }
    let mut right: Vec<String> = items
        .iter()
        .zip(layout.iter())
        .filter(|&(_, &(_, end))| end > scroll + visible_width)
        .filter_map(|(item, _)| names(item))
        .collect();
    if right.is_empty() {
        if let Some(last) = items.last().and_then(names) {
            right.push(last);
        }
    }
    let muted = Style::default().fg(ctx.theme.fg_muted);
    Line::from(vec![
        Span::styled(format!("◂ {}   ", left.join(" · ")), muted),
        Span::styled(format!("▸ {}", right.join(" · ")), muted),
    ])
}

/// The track line under the indicator (DESIGN.md §2.3): the visible
/// window's own share of the rail's full extent, drawn as an accent
/// run over a border-coloured rest.
fn flow_track_line(
    items: &[RailItem],
    app: &App,
    area_width: u16,
    scroll: usize,
    visible_width: usize,
    track_width: usize,
    ctx: &DrawCtx,
) -> Line<'static> {
    let total = flow_total_width(items, app, area_width).max(1);
    let filled = ((track_width as f64 * visible_width as f64 / total as f64).round() as usize)
        .min(track_width);
    let offset = ((track_width as f64 * scroll as f64 / total as f64).round() as usize)
        .min(track_width.saturating_sub(filled));
    let mut spans = Vec::new();
    if offset > 0 {
        spans.push(Span::styled(
            "─".repeat(offset),
            Style::default().fg(ctx.theme.border),
        ));
    }
    spans.push(Span::styled(
        "━".repeat(filled),
        Style::default().fg(ctx.theme.accent_primary),
    ));
    let rest = track_width.saturating_sub(offset + filled);
    if rest > 0 {
        spans.push(Span::styled(
            "─".repeat(rest),
            Style::default().fg(ctx.theme.border),
        ));
    }
    spans.push(Span::styled(" ", Style::default().fg(ctx.theme.border)));
    Line::from(spans)
}

#[cfg(test)]
mod flow_layout_tests {
    use super::*;
    use oscilloscope_core::model::StatusKind;
    use oscilloscope_core::rail::{Detail, Fork, ForkKind, Headline, Lane, RailItem, Station};
    use oscilloscope_core::render::RowKind;

    fn station(path: &str, status: StatusKind) -> RailItem {
        RailItem::Station(Station {
            path: path.to_string(),
            selectable: true,
            is_edge: false,
            row_kind: RowKind::Tool,
            status,
            name: path.to_string(),
            headline: Headline::Blank,
            detail: Detail::Blank,
        })
    }

    fn flow_test_row(
        path: &str,
        depth: usize,
        status: StatusKind,
    ) -> oscilloscope_core::render::Row {
        oscilloscope_core::render::Row {
            path: path.to_string(),
            depth,
            label: path.rsplit('.').next().unwrap().to_string(),
            kind: RowKind::Tool,
            status: oscilloscope_core::model::RowStatus {
                kind: status,
                skip_reason: None,
                retrying: false,
            },
            duration_s: None,
            loop_progress: None,
            dim: None,
            has_children: false,
        }
    }

    fn empty_lane(running: bool, waiting: bool) -> Lane {
        Lane {
            items: Vec::new(),
            ghost: None,
            running,
            waiting,
            path: None,
        }
    }

    #[test]
    fn flow_layout_places_plain_stations_left_to_right_with_the_normal_connector_between() {
        let items = vec![
            station("a", StatusKind::Done),
            station("b", StatusKind::Done),
        ];
        let app = App::new(false);
        let layout = flow_layout(&items, &app, 200);
        assert_eq!(layout[0], (0, rail::STATION_WIDTH));
        assert_eq!(
            layout[1],
            (
                rail::STATION_WIDTH + rail::NORMAL_CONNECTOR,
                rail::STATION_WIDTH * 2 + rail::NORMAL_CONNECTOR
            )
        );
    }

    #[test]
    fn flow_now_arrow_x_is_none_with_nothing_running() {
        let items = vec![
            station("a", StatusKind::Done),
            station("b", StatusKind::Pending),
        ];
        let app = App::new(false);
        assert_eq!(flow_now_arrow_x(&items, &app, 200), None);
    }

    #[test]
    fn flow_now_arrow_x_points_at_the_first_running_leaf() {
        let items = vec![
            station("a", StatusKind::Done),
            station("b", StatusKind::Running),
        ];
        let app = App::new(false);
        let x = flow_now_arrow_x(&items, &app, 200).expect("a live leaf");
        assert_eq!(x, rail::STATION_WIDTH + rail::NORMAL_CONNECTOR);
    }

    /// A folded fork (four or more lanes) with a live lane inside
    /// reads Now at its own band's content start -- the band is
    /// drawn as one unit, never descending into lanes it isn't
    /// showing.
    #[test]
    fn flow_now_arrow_x_reads_a_folded_forks_own_band_start_when_a_lane_inside_is_live() {
        let fork = Fork {
            path: "prime.fan".to_string(),
            kind: ForkKind::Loop,
            source_status: StatusKind::Running,
            lanes: (0..4)
                .map(|i| {
                    let mut lane = empty_lane(i == 0, i != 0);
                    lane.items = vec![station(
                        &format!("p{i}"),
                        if i == 0 {
                            StatusKind::Running
                        } else {
                            StatusKind::Pending
                        },
                    )];
                    lane
                })
                .collect(),
        };
        let items = vec![station("start", StatusKind::Done), RailItem::Fork(fork)];
        let app = App::new(false);
        let x = flow_now_arrow_x(&items, &app, 200).expect("a live lane inside the fold");
        // No connector sits between a plain station and the fork
        // right after it (flow_layout's own rule); the band itself
        // starts 3 cells into the fork (its own '══ ' lead-in).
        assert_eq!(x, rail::STATION_WIDTH + 3);
    }

    /// A decision's own two lanes never fold -- its own painted
    /// height is the sum of both three-row lanes, never a flat three
    /// the way an un-nested lane would be.
    #[test]
    fn flow_fork_height_sums_a_decisions_own_two_lanes() {
        let fork = Fork {
            path: "prime.pick".to_string(),
            kind: ForkKind::Decision,
            source_status: StatusKind::Done,
            lanes: vec![
                Lane {
                    items: vec![station("prime.pick.few", StatusKind::Done)],
                    ghost: None,
                    running: false,
                    waiting: false,
                    path: None,
                },
                Lane {
                    items: Vec::new(),
                    ghost: Some("many".to_string()),
                    running: false,
                    waiting: false,
                    path: None,
                },
            ],
        };
        let app = App::new(false);
        assert_eq!(flow_fork_height(&fork, &app, 200), 6);
    }

    /// A lane holding a nested fork (an unnamed `if` inside a loop's
    /// own pass, both ghost lanes visible) reports that nested
    /// fork's own full height, never a flat three -- or the outer
    /// fork's next lane would start too soon and paint over it.
    #[test]
    fn flow_lane_height_reports_a_nested_forks_own_full_height() {
        let inner = Fork {
            path: "prime.fan.iter_0.__if".to_string(),
            kind: ForkKind::Decision,
            source_status: StatusKind::Pending,
            lanes: vec![
                Lane {
                    items: Vec::new(),
                    ghost: Some("then_step".to_string()),
                    running: false,
                    waiting: false,
                    path: None,
                },
                Lane {
                    items: Vec::new(),
                    ghost: Some("else_step".to_string()),
                    running: false,
                    waiting: false,
                    path: None,
                },
            ],
        };
        let lane_items = vec![RailItem::Fork(inner)];
        let app = App::new(false);
        assert_eq!(flow_lane_height(&lane_items, &app, 200), 6);
    }

    /// A 2-pass loop whose own body holds an unnamed `if` (both
    /// ghost lanes visible, issue's own repro shape): each pass's
    /// own lane is six rows tall, not three, so the loop fork's own
    /// total height is twelve -- the outer fork must step its next
    /// lane by that, never a flat three per lane.
    #[test]
    fn a_loop_whose_body_holds_a_nested_if_is_taller_than_a_flat_three_rows_per_pass() {
        let inner = Fork {
            path: "prime.fan.iter_0.__if".to_string(),
            kind: ForkKind::Decision,
            source_status: StatusKind::Pending,
            lanes: vec![
                Lane {
                    items: Vec::new(),
                    ghost: Some("then_step".to_string()),
                    running: false,
                    waiting: false,
                    path: None,
                },
                Lane {
                    items: Vec::new(),
                    ghost: Some("else_step".to_string()),
                    running: false,
                    waiting: false,
                    path: None,
                },
            ],
        };
        let pass_lane = Lane {
            items: vec![RailItem::Fork(inner)],
            ghost: None,
            running: false,
            waiting: false,
            path: Some("prime.fan.iter_0".to_string()),
        };
        let outer = Fork {
            path: "prime.fan".to_string(),
            kind: ForkKind::Loop,
            source_status: StatusKind::Pending,
            lanes: vec![pass_lane.clone(), pass_lane],
        };
        let app = App::new(false);
        assert_eq!(flow_fork_height(&outer, &app, 200), 12);
    }

    /// A nested loop inside another fork's lane is bounded the same
    /// way: an unfolded 2-pass inner loop reports six rows of its
    /// own (two plain three-row lanes), not three.
    #[test]
    fn a_nested_loop_also_reports_the_sum_of_its_own_lanes() {
        let inner_loop = Fork {
            path: "prime.outer.iter_0.fan".to_string(),
            kind: ForkKind::Loop,
            source_status: StatusKind::Pending,
            lanes: vec![empty_lane(false, true), empty_lane(false, true)],
        };
        let lane_items = vec![RailItem::Fork(inner_loop)];
        let app = App::new(false);
        assert_eq!(flow_lane_height(&lane_items, &app, 200), 6);
    }

    #[test]
    fn flow_resolve_scroll_never_scrolls_while_now_is_in_view() {
        let items = vec![station("a", StatusKind::Running)];
        let mut app = App::new(false);
        let scroll = flow_resolve_scroll(&items, &mut app, 200, 100);
        assert_eq!(scroll, 0);
        assert!(app.follow);
    }

    /// A 40-station synthetic plan, the last one running: once Now
    /// would leave the visible window, the rail scrolls to the
    /// station boundary closest to putting it 60% of the way across
    /// the visible width -- never a different boundary, and never
    /// leaving Now out of view.
    #[test]
    fn flow_resolve_scroll_scrolls_to_the_boundary_closest_to_60_percent_on_a_40_station_plan() {
        let items: Vec<RailItem> = (0..40)
            .map(|i| {
                station(
                    &format!("s{i}"),
                    if i == 39 {
                        StatusKind::Running
                    } else {
                        StatusKind::Done
                    },
                )
            })
            .collect();
        let mut app = App::new(false);
        let visible_width = 100usize;
        let area_width = 200u16;
        let scroll = flow_resolve_scroll(&items, &mut app, area_width, visible_width);
        let arrow =
            flow_now_arrow_x(&items, &app, area_width).expect("the running station's own x");
        assert!(
            scroll <= arrow && arrow < scroll + visible_width,
            "Now must be in view: scroll={scroll} arrow={arrow}"
        );
        let target = (arrow as f64 - visible_width as f64 * 0.6).max(0.0) as usize;
        let layout = flow_layout(&items, &app, area_width);
        let max_scroll = flow_total_width(&items, &app, area_width).saturating_sub(visible_width);
        let best = std::iter::once(0usize)
            .chain(layout.iter().map(|&(s, _)| s))
            .min_by_key(|&s| s.abs_diff(target))
            .unwrap()
            .min(max_scroll);
        assert_eq!(
            scroll, best,
            "must land on the station boundary closest to 60%, clamped to the rail's own end"
        );
        assert!(
            app.follow,
            "follow stays on once it had to scroll to keep Now in view"
        );
    }

    #[test]
    fn flow_resolve_scroll_turns_follow_back_on_once_a_manual_scroll_lands_now_back_in_view() {
        let items = vec![station("a", StatusKind::Running)];
        let mut app = App::new(false);
        app.follow = false;
        app.flow_scroll = 0;
        // The ←→ key handlers set this alongside turning follow off;
        // `flow_resolve_scroll` only re-arms follow on the one draw
        // that flag covers.
        app.flow_manual_scroll = true;
        let scroll = flow_resolve_scroll(&items, &mut app, 200, 100);
        assert_eq!(scroll, 0);
        assert!(
            app.follow,
            "a manual scroll landed Now back in view: follow resumes"
        );
    }

    /// Issue: a redraw with no `←→` key in between (a clock tick, or
    /// the very next frame after `↑↓`/`f` turned follow off) must
    /// never flip follow back on just because Now happens to already
    /// sit in the current view -- only a manual scroll's own next
    /// draw may do that.
    #[test]
    fn flow_resolve_scroll_never_resumes_follow_without_a_manual_scroll_first() {
        let items = vec![station("a", StatusKind::Running)];
        let mut app = App::new(false);
        app.follow = false;
        app.flow_scroll = 0;
        assert!(!app.flow_manual_scroll);
        let scroll = flow_resolve_scroll(&items, &mut app, 200, 100);
        assert_eq!(scroll, 0);
        assert!(
            !app.follow,
            "Now being in view on an ordinary redraw must not resume follow on its own"
        );
        // A second ordinary redraw, same as the first: still no resume.
        let scroll = flow_resolve_scroll(&items, &mut app, 200, 100);
        assert_eq!(scroll, 0);
        assert!(!app.follow);
    }

    /// The manual-scroll flag is one-shot: consumed by the very next
    /// `flow_resolve_scroll` call regardless of outcome, so a later
    /// redraw with no new ←→ press can't ride on a stale flag.
    #[test]
    fn flow_resolve_scrolls_manual_scroll_flag_is_consumed_after_one_draw() {
        let items = vec![station("a", StatusKind::Running)];
        let mut app = App::new(false);
        app.follow = false;
        app.flow_scroll = 0;
        app.flow_manual_scroll = true;
        flow_resolve_scroll(&items, &mut app, 200, 100);
        assert!(
            !app.flow_manual_scroll,
            "the flag is consumed by the draw it armed"
        );
    }

    #[test]
    fn flow_indicator_line_names_start_and_end_with_nothing_off_either_edge() {
        let items = vec![
            station("Start", StatusKind::Done),
            station("End", StatusKind::Done),
        ];
        let app = App::new(false);
        let theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Dracula);
        let zone = test_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: false,
        };
        let line = flow_indicator_line(&items, &app, 200, 0, 200, &ctx);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("Start"));
        assert!(text.contains("End"));
    }

    #[test]
    fn flow_indicator_line_names_whatever_is_off_the_right_edge() {
        let items: Vec<RailItem> = (0..10)
            .map(|i| station(&format!("s{i}"), StatusKind::Done))
            .collect();
        let app = App::new(false);
        let theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Dracula);
        let zone = test_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: false,
        };
        // Visible only wide enough for the first couple of stations.
        let line = flow_indicator_line(&items, &app, 200, 0, 25, &ctx);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("s9"), "{text}");
    }

    #[test]
    fn flow_track_line_fills_the_whole_track_when_the_whole_rail_is_visible() {
        let items: Vec<RailItem> = (0..10)
            .map(|i| station(&format!("s{i}"), StatusKind::Done))
            .collect();
        let app = App::new(false);
        let theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Dracula);
        let zone = test_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: false,
        };
        let total = flow_total_width(&items, &app, 200);
        let line = flow_track_line(&items, &app, 200, 0, total, 50, &ctx);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text.chars().filter(|&c| c == '━').count(), 50);
        assert_eq!(text.chars().filter(|&c| c == '─').count(), 0);
    }

    /// Issue #691's own follow-up: the selection is a *pass* path
    /// (`iter_4`), never a station inside its own lane (the pass's
    /// own children) -- selecting pass 5 of 6 must still slide the
    /// window to show its own lane, matching the lane's own `path`
    /// (DESIGN.md's own frames never select the pass container's
    /// bare station, only a child, so the match is a prefix, not
    /// equality).
    #[test]
    fn flow_lane_window_slides_to_the_lane_whose_path_the_selection_is_inside() {
        let lanes: Vec<Lane> = (0..6)
            .map(|i| Lane {
                items: vec![station(
                    &format!("prime.fan.iter_{i}.nap"),
                    StatusKind::Done,
                )],
                ghost: None,
                running: false,
                waiting: false,
                path: Some(format!("prime.fan.iter_{i}")),
            })
            .collect();
        let fork = Fork {
            path: "prime.fan".to_string(),
            kind: ForkKind::Loop,
            source_status: StatusKind::Done,
            lanes,
        };
        let mut app = App::new(false);
        app.selected_path = Some("prime.fan.iter_4.nap".to_string());
        let visible = flow_lane_window(&fork, &app);
        assert_eq!(visible.len(), 3, "{visible:?}");
        assert!(
            visible
                .iter()
                .any(|l| l.path.as_deref() == Some("prime.fan.iter_4")),
            "pass 5 (iter_4) must be in the visible window: {visible:?}"
        );
    }

    #[test]
    fn flow_fork_folded_by_width_below_100_columns() {
        let fork = Fork {
            path: "p".to_string(),
            kind: ForkKind::Loop,
            source_status: StatusKind::Running,
            lanes: vec![empty_lane(false, false); 2],
        };
        let app = App::new(false);
        assert!(flow_fork_folded(&fork, &app, 99));
        assert!(!flow_fork_folded(&fork, &app, 100));
    }

    #[test]
    fn flow_fork_folded_by_lane_count_at_four_or_more() {
        let fork3 = Fork {
            path: "p".to_string(),
            kind: ForkKind::Loop,
            source_status: StatusKind::Running,
            lanes: vec![empty_lane(false, false); 3],
        };
        let fork4 = Fork {
            lanes: vec![empty_lane(false, false); 4],
            ..fork3.clone()
        };
        let app = App::new(false);
        assert!(!flow_fork_folded(&fork3, &app, 200));
        assert!(flow_fork_folded(&fork4, &app, 200));
    }

    #[test]
    fn flow_fork_folded_never_folds_a_decision() {
        let fork = Fork {
            path: "p".to_string(),
            kind: ForkKind::Decision,
            source_status: StatusKind::Done,
            lanes: vec![empty_lane(false, false); 5],
        };
        let app = App::new(false);
        assert!(!flow_fork_folded(&fork, &app, 50));
    }

    /// Issue #691's own follow-up: the three-lane window and the
    /// `══ N lanes + ══` band apply to every fork, not only loops --
    /// a parallel `dynamic` with five children starts folded the
    /// same way a four-or-more-wide loop already did.
    #[test]
    fn flow_fork_folded_by_lane_count_also_folds_a_parallel_dynamic() {
        let fork = Fork {
            path: "p".to_string(),
            kind: ForkKind::Parallel,
            source_status: StatusKind::Running,
            lanes: vec![empty_lane(false, false); 5],
        };
        let app = App::new(false);
        assert!(flow_fork_folded(&fork, &app, 200));
    }

    /// `+` opens exactly three of a parallel dynamic's own five
    /// lanes at a time, up to its own real lane count -- never a
    /// loop's own `loop_progress`-derived count (a parallel fork has
    /// none), the plan's own named-children count instead.
    #[test]
    fn flow_lane_window_shows_three_of_a_five_child_parallel_forks_lanes_once_opened() {
        let fork = Fork {
            path: "prime.par5".to_string(),
            kind: ForkKind::Parallel,
            source_status: StatusKind::Running,
            lanes: vec![empty_lane(false, false); 5],
        };
        let mut app = App::new(false);
        let mut par5_row = flow_test_row("prime.par5", 1, StatusKind::Running);
        par5_row.kind = RowKind::Dynamic;
        app.flow_rows = std::iter::once(par5_row)
            .chain(
                (1..=5).map(|i| flow_test_row(&format!("prime.par5.p{i}"), 2, StatusKind::Pending)),
            )
            .collect();
        app.selected_path = Some("prime.par5".to_string());
        app.flow_open_lanes();
        assert_eq!(app.flow_lanes_open.get("prime.par5"), Some(&3));
        let visible = flow_lane_window(&fork, &app);
        assert_eq!(visible.len(), 3, "{visible:?}");
    }
}

/// A continuation row under a fork whose own lanes are narrower
/// than a sibling's (DESIGN.md §2.3's own frame: the gap under a
/// plain station like `drafts`, on another fork's own lane rows)
/// reads faint all the way up to wherever that row's own real
/// content starts, never the pane's own unfilled border colour --
/// swept once after painting, since no single fork painting its own
/// lanes knows what a *later* one will still draw on the same row.
fn flow_fill_gaps_before_content(
    buf: &mut ratatui::buffer::Buffer,
    area: Rect,
    untouched_fg: Color,
    fill_fg: Color,
) {
    let faint = Style::default().fg(fill_fg);
    for y in area.y + 1..area.y + area.height {
        let Some(first_content_x) =
            (area.x..area.x + area.width).find(|&x| buf[(x, y)].fg != untouched_fg)
        else {
            continue;
        };
        for x in first_content_x..area.x + area.width {
            if buf[(x, y)].fg == untouched_fg {
                let ch = buf[(x, y)].symbol().to_string();
                buf.set_string(x, y, &ch, faint);
            }
        }
    }
}

/// The rail's own bordered pane (DESIGN.md §2.3): the Now line, the
/// rail itself, the indicator and track lines, a separator and the
/// Totals row, all inside one block the same way the Signal's own
/// chart pane is.
#[allow(clippy::too_many_arguments)]
fn draw_flow_rail_pane(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::screens::FlowScreen,
    header: &oscilloscope_core::render::Header,
    app: &mut App,
    frame_width: u16,
    ctx: &DrawCtx,
    now: &str,
) {
    let border_color = ctx.theme.border_focused;
    let mut title = format!(
        "Flow · {}/{} · ▰ = {} t",
        header.effects_done,
        header.effects_planned.unwrap_or(header.effects_done),
        screen.unit
    );
    // Once the run has ended, there is nothing left to follow --
    // the title drops the word on its own, the same instant the
    // run does, never waiting on the next key press to notice.
    let nothing_to_follow = matches!(
        screen.head,
        oscilloscope_core::screens::TapeHead::Ended { .. }
            | oscilloscope_core::screens::TapeHead::Failed { .. }
    );
    if app.follow && !nothing_to_follow && area.width >= 100 {
        title.push_str(" · Following");
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(ratatui::symbols::border::ROUNDED)
        .title(tape_block_title(&title, border_color, ctx))
        .border_style(Style::default().fg(border_color));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    fill_border_rows(frame, inner, border_color);

    let visible_width = inner.width as usize;
    let scroll = flow_resolve_scroll(&screen.rail.items, app, frame_width, visible_width);

    let now_row = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: 1,
    };
    // Nothing is running at all (every station is either past or
    // still ahead): there is no live edge for the arrow to point at,
    // so the line stays blank rather than pointing at the rail's own
    // left edge, which nothing there is actually live.
    if let Some(arrow_x) = flow_now_arrow_x(&screen.rail.items, app, frame_width) {
        let mut now_text = flow_now_text(header, now, ctx);
        let now_style = Style::default()
            .fg(ctx.theme.accent_primary)
            .add_modifier(Modifier::BOLD);
        let end_x = scroll + visible_width;
        let now_end = arrow_x + now_text.chars().count();
        if now_end < end_x {
            now_text.push_str(&" ".repeat(end_x - now_end));
        }
        flow_put(
            frame.buffer_mut(),
            now_row,
            scroll,
            visible_width,
            arrow_x,
            0,
            &now_text,
            now_style,
        );
    }

    let rail_body_rows = inner.height.saturating_sub(5);
    let rail_area = Rect {
        x: inner.x,
        y: inner.y + 1,
        width: inner.width,
        height: rail_body_rows,
    };
    let buf = frame.buffer_mut();
    flow_paint_sequence(
        buf,
        rail_area,
        scroll,
        visible_width,
        &screen.rail.items,
        0,
        0,
        app,
        frame_width,
        screen.unit,
        ctx,
    );
    flow_fill_gaps_before_content(buf, rail_area, border_color, ctx.theme.dim);

    let indicator_y = inner.y + inner.height.saturating_sub(4);
    frame.render_widget(
        Paragraph::new(pad_line_to_width(
            flow_indicator_line(
                &screen.rail.items,
                app,
                frame_width,
                scroll,
                visible_width,
                ctx,
            ),
            inner.width.saturating_sub(1) as usize,
            ctx.theme.fg_muted,
        )),
        Rect {
            x: inner.x + 1,
            y: indicator_y,
            width: inner.width.saturating_sub(1),
            height: 1,
        },
    );
    frame.render_widget(
        Paragraph::new(flow_track_line(
            &screen.rail.items,
            app,
            frame_width,
            scroll,
            visible_width,
            inner.width.saturating_sub(2) as usize,
            ctx,
        )),
        Rect {
            x: inner.x + 1,
            y: indicator_y + 1,
            width: inner.width.saturating_sub(1),
            height: 1,
        },
    );
    let sep_y = indicator_y + 2;
    frame.buffer_mut().set_string(
        inner.x,
        sep_y,
        "─".repeat(inner.width as usize),
        Style::default().fg(ctx.theme.border),
    );
    draw_totals_row(
        frame,
        Rect {
            x: inner.x,
            y: sep_y + 1,
            width: inner.width,
            height: 1,
        },
        header,
        false,
        None,
        screen.usual_so_far_tokens,
        Some(border_color),
        false,
        ctx,
    );
}

/// Screen 2 (`Flow`, issue #691, part 3 of #677): the rail, the
/// selected pass or station below it, and the Story pane -- draws
/// its own header band, status line and key bar, the same self-
/// contained way the Tape and Signal already do.
#[allow(clippy::too_many_arguments)]
fn draw_flow(
    frame: &mut Frame,
    full_area: Rect,
    screen: &oscilloscope_core::screens::FlowScreen,
    header: &oscilloscope_core::render::Header,
    app: &mut App,
    transcript_view: Option<&oscilloscope_core::transcript::TranscriptView>,
    ctx: &DrawCtx,
    icon_frame: u64,
    now: &str,
) {
    if full_area.width < MIN_WIDTH || full_area.height < MIN_HEIGHT {
        draw_too_small(frame, full_area, ctx);
        return;
    }
    // §4: below 80 columns only the Tape is offered.
    if full_area.width < 80 {
        app.screen = oscilloscope_core::screens::Screen::Tape;
        return;
    }
    app.flow_has_selection = screen.selected_pass.is_some()
        || screen.selected_step.is_some()
        || screen.selected_decision.is_some();
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(5),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(full_area);
    draw_tape_header_band(frame, sections[0], header, "FLOW", now, icon_frame, ctx);
    draw_flow_status_line(frame, sections[2], screen, header, ctx, now);
    draw_key_bar(
        frame,
        sections[3],
        &flow_key_bar_items(full_area.width),
        ctx,
    );

    let body = sections[1];
    // §5: the Story pane needs 100 columns and 30 rows both -- the
    // same tier the frames' own `flow--normal--80x24` drops it at.
    let wide = full_area.width >= 100 && full_area.height >= 30;
    if app.tape_fullscreen_step
        && (screen.selected_pass.is_some()
            || screen.selected_step.is_some()
            || screen.selected_decision.is_some())
    {
        draw_flow_selected_pane(frame, body, wide, screen, app, transcript_view, ctx, now);
        return;
    }
    let base_panes_height = if wide { 8 } else { 4 };
    // DESIGN.md's own frame fixes this at 120×30 and 80×24; a
    // taller terminal gives the extra rows to the Selected/Story
    // panes rather than only the rail, so a step's own `produced:`
    // isn't cut off merely because the terminal is tall.
    let panes_height = base_panes_height + full_area.height.saturating_sub(30) / 2;
    let rows_split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(5), Constraint::Length(panes_height)])
        .split(body);
    draw_flow_rail_pane(
        frame,
        rows_split[0],
        screen,
        header,
        app,
        full_area.width,
        ctx,
        now,
    );
    if wide {
        // DESIGN.md's own frame: 62/58 at 120 columns -- scaled by
        // the same ratio at every other wide width, since nothing
        // ties this split to a rounder fraction.
        let pass_width = (rows_split[1].width as u32 * 31 / 60) as u16;
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(pass_width), Constraint::Min(0)])
            .split(rows_split[1]);
        draw_flow_selected_pane(frame, cols[0], wide, screen, app, transcript_view, ctx, now);
        draw_flow_story_pane(frame, cols[1], screen, ctx);
    } else {
        draw_flow_selected_pane(
            frame,
            rows_split[1],
            wide,
            screen,
            app,
            transcript_view,
            ctx,
            now,
        );
    }
}

#[cfg(test)]
mod golden_tests {
    use super::*;
    use electricity_bytecode::{
        EffectPath, Escape, LeafKind, LoopFlow, LoopId, LoopSpec, NodeKind, OnError, Op, ParamNode,
        Program, PromptContent, PromptOp, PromptType, Region, TemplateText, ToolOp,
    };

    use oscilloscope_core::model::{ProcessState, RunModel};
    use oscilloscope_core::observe::Event;
    use oscilloscope_core::plan::PlanTree;
    use oscilloscope_core::render;

    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use serde_json::{Value, json};

    use crate::keys::App;

    fn tool(path: EffectPath, name: &str, on_error: OnError) -> Op {
        Op {
            path,
            name: Some(name.to_string()),
            kind: NodeKind::Leaf(Box::new(LeafKind::Tool(ToolOp {
                provider: "shell".to_string(),
                params: ParamNode::Literal(electricity_value::Value::None),
                params_json: None,
                prompt: None,
                model: None,
                timeout_ms: None,
                retries: Default::default(),
                expect: None,
                description: None,
                group: None,
                prompt_file: None,
            }))),
            on_error,
            labels: None,
            enabled: true,
        }
    }

    fn prompt(path: EffectPath, name: &str) -> Op {
        Op {
            path,
            name: Some(name.to_string()),
            kind: NodeKind::Leaf(Box::new(LeafKind::Prompt(PromptOp {
                content: PromptContent::Template(TemplateText::new("hi", false, Escape::None)),
                prompt_type: PromptType::Text,
                schema: None,
                model: None,
                provider: None,
                provider_fallbacks: Vec::new(),
                routing_override: None,
                model_params: None,
                timeout_ms: None,
                deterministic: false,
                inputs: None,
                assets: Vec::new(),
                retries: None,
                description: None,
                group: None,
                split_budget_tokens: None,
                split_enabled: None,
                split_max_parts: None,
                split_separator: None,
                split_merge_template: None,
            }))),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        }
    }

    /// A plan shared by every golden test below (DESIGN.md §6.3): a
    /// chain of two leaves (`a` fails the run, `b`'s own failure is
    /// handled) followed by a named tree loop (`fan`, `max_concurrency
    /// 2`) with one leaf body (`nap`) -- enough to exercise every
    /// glyph the issue's acceptance list calls for from one plan.
    fn sample_plan() -> PlanTree {
        let root_path = EffectPath::root();
        let a_path = root_path.clone().push_name("a");
        let b_path = root_path.clone().push_name("b");
        let fan_path = root_path.clone().push_name("fan");
        let pass_path = fan_path.clone().push_pass(LoopId(0));
        let nap_path = pass_path.push_name("nap");

        let root = Op {
            path: root_path,
            name: Some("prime".to_string()),
            kind: NodeKind::Control(Region::Block {
                ops: vec![
                    tool(a_path, "a", OnError::Fail),
                    tool(b_path, "b", OnError::Continue),
                    Op {
                        path: fan_path,
                        name: Some("fan".to_string()),
                        kind: NodeKind::Control(Region::Loop {
                            name: Some("fan".to_string()),
                            spec: LoopSpec::Each {
                                in_path: "prime.items".to_string(),
                                as_name: "item".to_string(),
                                truncate: false,
                            },
                            body: Box::new(Region::Block {
                                ops: vec![tool(nap_path, "nap", OnError::Fail)],
                                overlay: true,
                            }),
                            flow: LoopFlow::Tree,
                            max_concurrency: Some(2),
                            max_iterations: None,
                            min_iterations: 0,
                            collect: None,
                        }),
                        on_error: OnError::Fail,
                        labels: None,
                        enabled: true,
                    },
                ],
                overlay: false,
            }),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        let program = Program {
            root,
            prompts: Default::default(),
            effect_names: Default::default(),
            document: None,
            runtime_block: None,
            interface: None,
            adapter: None,
            model: None,
        };
        PlanTree::from_program(&program)
    }

    /// A named *static* tree dynamic (`fan`, `max_concurrency 1`,
    /// two branches `a`/`b`) -- unlike `sample_plan`'s own `each` loop
    /// (a data-dependent pass count the compiler can't enumerate
    /// ahead of time), both branches have a real path of their own in
    /// the plan, so an unobserved one still gets its own row (review
    /// finding 3 on #458's comment 3: a queued ◌ row).
    fn tree_dynamic_plan() -> PlanTree {
        let root_path = EffectPath::root();
        let fan_path = root_path.clone().push_name("fan");
        let branch_a = fan_path.clone().push_name("a");
        let branch_b = fan_path.clone().push_name("b");
        let root = Op {
            path: root_path,
            name: Some("prime".to_string()),
            kind: NodeKind::Control(Region::Block {
                ops: vec![Op {
                    path: fan_path,
                    name: Some("fan".to_string()),
                    kind: NodeKind::Control(Region::Parallel {
                        branches: vec![
                            tool(branch_a, "a", OnError::Fail),
                            tool(branch_b, "b", OnError::Fail),
                        ],
                        max_concurrency: Some(1),
                        stop_on_error: false,
                    }),
                    on_error: OnError::Fail,
                    labels: None,
                    enabled: true,
                }],
                overlay: false,
            }),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        let program = Program {
            root,
            prompts: Default::default(),
            effect_names: Default::default(),
            document: None,
            runtime_block: None,
            interface: None,
            adapter: None,
            model: None,
        };
        PlanTree::from_program(&program)
    }

    /// Issue #647: with no transcript tailed yet, a running tool's
    /// own elapsed shows right next to `running:`, from `meta.live.
    /// current_tool.started_at` -- not just the session's own total.
    #[test]
    fn agent_live_lines_shows_the_running_tools_own_elapsed() {
        let theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let zone = test_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: false,
        };
        let live = oscilloscope_core::model::AgentLive::from_value(&json!({
            "engine": "pi",
            "current_tool": {"name": "bash", "started_at": "2026-01-01T11:52:30Z"}
        }))
        .unwrap();
        let lines = agent_live_lines(
            &live,
            None,
            &ctx,
            true,
            false,
            Some(24.0),
            "2026-01-01T11:52:54Z",
        );
        let text = lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("running: bash · 24.0s"), "{text}");
    }

    /// Issue #647: once stale (the session has stopped), the last
    /// tool it ran is labelled `last:`, never `running:` -- it never
    /// finished, but it is not running any more either.
    #[test]
    fn agent_live_lines_labels_a_stale_current_tool_as_last_not_running() {
        let theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let zone = test_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: false,
        };
        let live = oscilloscope_core::model::AgentLive::from_value(&json!({
            "engine": "pi",
            "current_tool": {"name": "bash", "started_at": "2026-01-01T11:46:50Z"}
        }))
        .unwrap();
        let lines = agent_live_lines(&live, None, &ctx, true, true, None, "2026-01-01T11:52:54Z");
        let text = lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("last: bash"), "{text}");
        assert!(!text.contains("running:"), "{text}");
    }

    /// Issue #641: two tool calls genuinely running at once (neither
    /// has a `tool_end`, and neither is superseded by a later
    /// `tool_start` with a lower id, so neither reads as `stopped`)
    /// must each show their own elapsed, from their own `start_t` --
    /// not the session's single `meta.live.current_tool` time, which
    /// would otherwise show the same figure for both.
    #[test]
    fn agent_live_lines_shows_each_running_calls_own_elapsed_in_a_transcript() {
        let theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let zone = test_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: false,
        };
        let transcript_lines = vec![
            json!({"t": "2026-01-01T11:50:00Z", "type": "tool_start", "id": 1, "tool": "bash", "target": "pytest -k a"}),
            json!({"t": "2026-01-01T11:51:00Z", "type": "tool_start", "id": 2, "tool": "bash", "target": "pytest -k b"}),
        ];
        let view = oscilloscope_core::transcript::build_transcript_view(&transcript_lines, false);
        let live =
            oscilloscope_core::model::AgentLive::from_value(&json!({"engine": "pi"})).unwrap();
        let lines = agent_live_lines(
            &live,
            Some(&view),
            &ctx,
            true,
            false,
            None,
            "2026-01-01T11:52:00Z",
        );
        let text = lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("pytest -k a running 2m00s"), "{text}");
        assert!(text.contains("pytest -k b running 1m00s"), "{text}");
    }

    /// [`snapshot_at`], for screen 3 (the Cost screen, issue #632):
    /// builds the Cost screen's own model (the by-store treemap,
    /// the largest-stores table, the tokens-over-time chart) the
    /// same way `main.rs`'s own `build_cost_screen_if_active` does,
    /// only while that screen is on display.
    fn cost_snapshot_at(
        width: u16,
        height: u16,
        plan: &PlanTree,
        model: &mut RunModel,
        state: Option<&Value>,
        process: ProcessState,
    ) -> String {
        let rendered = render::build_folded(
            "do-thing.yml",
            "cof",
            plan,
            model,
            state,
            process,
            300.0,
            "2026-01-01T00:05:00Z",
            &[],
            true,
            &Default::default(),
        );
        let empty = Value::Null;
        let flat = oscilloscope_core::model::flatten_state(state.unwrap_or(&empty));
        let statuses: std::collections::BTreeMap<_, _> = rendered
            .unfolded_rows
            .iter()
            .map(|r| (r.path.clone(), r.status.clone()))
            .collect();
        let kinds: std::collections::BTreeMap<_, _> = rendered
            .unfolded_rows
            .iter()
            .map(|r| (r.path.clone(), r.kind))
            .collect();
        let cost_screen = oscilloscope_core::screens::build_cost_screen(
            &rendered.rows,
            &rendered.header,
            &flat,
            &statuses,
            &kinds,
            state,
            Measure::Total,
            model,
            "2026-01-01T00:05:00Z",
            PHASE_GLYPHS.len(),
        );
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Cost;
        let test_theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let test_ctx = DrawCtx {
            theme: &test_theme,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut cost_layout = CostLayoutCache::default();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    None,
                    &[],
                    &mut app,
                    state,
                    &test_ctx,
                    0,
                    "2026-01-01T00:05:00Z",
                    None,
                    Some(&cost_screen),
                    None,
                    None,
                    None,
                    &mut cost_layout,
                )
            })
            .unwrap();
        terminal.backend().to_string()
    }

    /// Three stores with real tokens/cost (`a`, `b`, and `fan`'s own
    /// running pass `nap`, with no tokens yet -- DESIGN.md's "a tile
    /// for a step that is still running reads tokens when done"),
    /// plenty for the treemap, the chart and the table to all show
    /// something real.
    fn cost_running_state() -> Value {
        json!({"prime": {"value": null, "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": null},
            "a": {"value": "ok", "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:01:00Z", "adapter": "scripted", "tokens_sent_total": 900, "tokens_received_total": 400, "cost_usd": 1.2}},
            "b": {"value": "ok", "meta": {"created_at": "2026-01-01T00:01:00Z", "completed_at": "2026-01-01T00:02:00Z", "adapter": "scripted", "tokens_sent_total": 300, "tokens_received_total": 150}},
            "fan": {"value": null, "meta": {"completed_at": null, "progress": {"done": 0, "total": 2}},
                "iter_0": {"value": null, "meta": {"completed_at": null},
                    "nap": {"value": null, "meta": {"created_at": "2026-01-01T00:02:00Z", "completed_at": null, "adapter": "scripted"}}
                }
            }
        }})
    }

    fn cost_finished_state() -> Value {
        json!({
            "runtime": {"last_run": {"completed_at": "2026-01-01T00:05:00Z", "totals": {"wall_time_s": 300.0, "effects_run": 3, "tokens_sent": 1300, "tokens_received": 600, "cost_usd": 1.2}}},
            "prime": {"value": true, "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:05:00Z", "error": null},
            "a": {"value": "ok", "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": "2026-01-01T00:01:00Z", "adapter": "scripted", "tokens_sent_total": 900, "tokens_received_total": 400, "cost_usd": 1.2}},
            "b": {"value": "ok", "meta": {"created_at": "2026-01-01T00:01:00Z", "completed_at": "2026-01-01T00:02:00Z", "adapter": "scripted", "tokens_sent_total": 300, "tokens_received_total": 150}},
            "fan": {"value": null, "meta": {"completed_at": "2026-01-01T00:05:00Z", "progress": {"done": 2, "total": 2}},
                "iter_0": {"value": null, "meta": {"completed_at": "2026-01-01T00:03:00Z"},
                    "nap": {"value": "ok", "meta": {"created_at": "2026-01-01T00:02:00Z", "completed_at": "2026-01-01T00:03:00Z", "adapter": "scripted", "tokens_sent_total": 50, "tokens_received_total": 50}}
                },
                "iter_1": {"value": null, "meta": {"completed_at": "2026-01-01T00:05:00Z"},
                    "nap": {"value": "ok", "meta": {"created_at": "2026-01-01T00:04:00Z", "completed_at": "2026-01-01T00:05:00Z", "adapter": "scripted", "tokens_sent_total": 50, "tokens_received_total": 50}}
                }
            }
        }})
    }

    #[test]
    fn cost_running_at_120x40() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = cost_running_state();
        let text = cost_snapshot_at(
            120,
            40,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
        );
        insta::assert_snapshot!(text);
    }

    /// Issue #637: a running agent's own tile, sized and labelled
    /// from `meta.live.tokens_sent` -- `tokens when done` never
    /// appears for it, unlike every other running store.
    fn cost_agent_running_state() -> Value {
        json!({"prime": {"value": null, "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": null},
            "fix": {"value": null, "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": null,
                "provider": "agent", "live": {
                    "tokens_sent": 2100000,
                    "turn_tokens": [400000, 900000, 600000, 2100000],
                    "tokens_cached_in": 1800000
                }}}
        }})
    }

    fn cost_agent_finished_state() -> Value {
        json!({"prime": {"value": null, "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": "t"},
            "fix": {"value": "done", "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": "t",
                "provider": "agent", "tokens_sent": 2100000, "tokens_received": 18000, "cost_usd": 1.10}}
        }})
    }

    #[test]
    fn cost_tile_running_agent_at_120x40() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = cost_agent_running_state();
        let text = cost_snapshot_at(
            120,
            40,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn cost_tile_running_agent_at_80x24() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = cost_agent_running_state();
        let text = cost_snapshot_at(
            80,
            24,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn cost_tile_finished_agent_at_120x40() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = cost_agent_finished_state();
        let process = ProcessState::exited(false);
        let text = cost_snapshot_at(120, 40, &plan, &mut model, Some(&state), process);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn cost_tile_finished_agent_at_80x24() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = cost_agent_finished_state();
        let process = ProcessState::exited(false);
        let text = cost_snapshot_at(80, 24, &plan, &mut model, Some(&state), process);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn cost_finished_at_120x40() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = cost_finished_state();
        let process = ProcessState::exited(true);
        let text = cost_snapshot_at(120, 40, &plan, &mut model, Some(&state), process);
        insta::assert_snapshot!(text);
    }

    /// Issue #632's own small-terminal acceptance item: at 120x40 the
    /// table pane sits beside the chart (DESIGN.md's 62/38 split,
    /// narrower than the full screen width), and the coverage label
    /// must still show in full there, on its own line, not truncated
    /// mid-word the way sharing the totals line's own width used to
    /// leave it (`$1.20 (from 1 of 4 pr…`).
    #[test]
    fn the_full_cost_coverage_label_shows_at_120x40() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = cost_finished_state();
        let process = ProcessState::exited(true);
        let text = cost_snapshot_at(120, 40, &plan, &mut model, Some(&state), process);
        assert!(
            text.contains("$1.20 (from 1 of 4 prompts)"),
            "the full coverage label did not appear:\n{text}"
        );
    }

    #[test]
    fn cost_running_at_100x30() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = cost_running_state();
        let text = cost_snapshot_at(
            100,
            30,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn cost_finished_at_100x30() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = cost_finished_state();
        let process = ProcessState::exited(true);
        let text = cost_snapshot_at(100, 30, &plan, &mut model, Some(&state), process);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn cost_running_at_80x24() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = cost_running_state();
        let text = cost_snapshot_at(
            80,
            24,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn cost_finished_at_80x24() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = cost_finished_state();
        let process = ProcessState::exited(true);
        let text = cost_snapshot_at(80, 24, &plan, &mut model, Some(&state), process);
        insta::assert_snapshot!(text);
    }

    /// 80×20 itself still qualifies for the treemap and the table
    /// (`COST_TREEMAP_MIN_WIDTH`/`_MIN_HEIGHT` are inclusive, checked
    /// against the whole terminal) -- only below it does the table
    /// show alone.
    #[test]
    fn cost_running_at_80x20() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = cost_running_state();
        let text = cost_snapshot_at(
            80,
            20,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
        );
        insta::assert_snapshot!(text);
    }

    /// Issue #641: a store with well under 1% of the run's total
    /// tokens must read `<1%` in the largest-stores table, never a
    /// plain `0%` that looks the same as an actual zero share.
    #[test]
    fn the_table_shows_a_small_nonzero_share_as_under_one_percent() {
        use oscilloscope_core::plan::PlanTree;
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": "t1"},
            "big": {"value": "y", "meta": {
                "created_at": "t0", "completed_at": "t1", "adapter": "scripted",
                "tokens_sent_total": 10000, "tokens_received_total": 0
            }},
            "tiny": {"value": "y", "meta": {
                "created_at": "t0", "completed_at": "t1", "adapter": "scripted",
                "tokens_sent_total": 30, "tokens_received_total": 0
            }}
        }});
        let text = cost_snapshot_at(
            120,
            40,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(true),
        );
        assert!(text.contains("<1%"), "{text}");
    }

    #[test]
    fn cost_finished_at_80x20() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = cost_finished_state();
        let process = ProcessState::exited(true);
        let text = cost_snapshot_at(80, 20, &plan, &mut model, Some(&state), process);
        insta::assert_snapshot!(text);
    }

    #[test]
    fn cost_running_at_70x20() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = cost_running_state();
        let text = cost_snapshot_at(
            70,
            20,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn cost_finished_at_70x20() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = cost_finished_state();
        let process = ProcessState::exited(true);
        let text = cost_snapshot_at(70, 20, &plan, &mut model, Some(&state), process);
        insta::assert_snapshot!(text);
    }

    /// Below [`thresholds::COST_TREEMAP_MIN_WIDTH`]/`_MIN_HEIGHT`
    /// (DESIGN.md: "the table alone, where ⏎ still zooms") the table
    /// is the only thing on screen -- it must still honour a zoom
    /// already in effect (`app.cost_zoom`) and mark the selected row,
    /// since nothing else on screen can show either any more.
    #[test]
    fn the_table_alone_still_reflects_zoom_and_selection() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = cost_finished_state();
        let rendered = render::build_folded(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(true),
            300.0,
            "2026-01-01T00:05:00Z",
            &[],
            true,
            &Default::default(),
        );
        let flat = oscilloscope_core::model::flatten_state(&state);
        let statuses: std::collections::BTreeMap<_, _> = rendered
            .unfolded_rows
            .iter()
            .map(|r| (r.path.clone(), r.status.clone()))
            .collect();
        let kinds: std::collections::BTreeMap<_, _> = rendered
            .unfolded_rows
            .iter()
            .map(|r| (r.path.clone(), r.kind))
            .collect();
        let cost_screen = oscilloscope_core::screens::build_cost_screen(
            &rendered.rows,
            &rendered.header,
            &flat,
            &statuses,
            &kinds,
            Some(&state),
            Measure::Total,
            &model,
            "2026-01-01T00:05:00Z",
            PHASE_GLYPHS.len(),
        );
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Cost;
        app.cost_zoom = vec!["prime.fan".to_string()];
        app.cost_selected = Some("prime.fan.iter_0.nap".to_string());
        let test_theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let test_ctx = DrawCtx {
            theme: &test_theme,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let backend = TestBackend::new(70, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut cost_layout = CostLayoutCache::default();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    None,
                    &[],
                    &mut app,
                    Some(&state),
                    &test_ctx,
                    0,
                    "2026-01-01T00:05:00Z",
                    None,
                    Some(&cost_screen),
                    None,
                    None,
                    None,
                    &mut cost_layout,
                )
            })
            .unwrap();
        let text = terminal.backend().to_string();
        assert!(text.contains("iter_0"), "{text}");
        assert!(
            !text.contains("a  ↑"),
            "zoomed into fan, a must not show: {text}"
        );
        assert!(
            !text.contains("b  ↑"),
            "zoomed into fan, b must not show: {text}"
        );
        assert!(
            text.contains("iter_0 › nap"),
            "zoomed into fan, iter_0's row must read relative to it: {text}"
        );
        assert!(
            text.contains("iter_1 › nap"),
            "zoomed into fan, iter_1's row must read relative to it: {text}"
        );
        assert!(
            !text.contains("fan ›"),
            "zoomed into fan, no row may still carry fan's own name as a prefix: {text}"
        );
        let buffer = terminal.backend().buffer();
        let tinted = (0..buffer.area.height).any(|y| {
            (0..buffer.area.width).any(|x| buffer[(x, y)].modifier.contains(Modifier::REVERSED))
        });
        assert!(tinted, "the selected row must be reversed: {text}");
    }

    #[test]
    fn pending_plan() {
        // Kept at 70×20 deliberately, as one of this suite's own
        // one-column-fallback goldens.
        let plan = sample_plan();
        let mut model = RunModel::new();
        let text = snapshot_one_column(
            &plan,
            &mut model,
            None,
            ProcessState::Running,
            "cof",
            "prime.a",
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn running_chain() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": null, "meta": {"created_at": "2026-01-01T00:00:00Z", "completed_at": null, "provider": "shell"}}
        }});
        let text = snapshot(
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            "cof",
            "prime.a",
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn the_full_value_overlay_opens_the_node_the_details_pane_shows() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "TREE-ROW-VALUE", "meta": {"created_at": "2026-01-01T11:46:49.000Z", "completed_at": "2026-01-01T11:46:50.000Z", "error": null, "provider": "shell"}},
            "status": {"value": {"message": "m", "level": "info", "data": {"k": "REPORT-DATA"}},
                "meta": {"created_at": "2026-01-01T11:46:50.000Z", "completed_at": "2026-01-01T11:46:50.000Z"}}
        }});
        let rendered = render::build_folded(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            1.0,
            "2026-01-01T11:46:51.000Z",
            &[],
            true,
            &std::collections::BTreeSet::new(),
        );
        // The tree selection is `prime.a`; the details pane shows the
        // named report picked in the state pane.
        let details = render::details_for("prime.status", &plan, &model, Some(&state));
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Flow;
        app.selected_path = Some("prime.a".to_string());
        app.state_selected_path = Some("status".to_string());
        app.details_source = Pane::State;
        app.follow = false;
        app.dialog = Dialog::FullValue(oscilloscope_core::render::FullValueField::Value);
        let test_theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let test_ctx = DrawCtx {
            theme: &test_theme,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    Some(&details),
                    &[],
                    &mut app,
                    Some(&state),
                    &test_ctx,
                    0,
                    "2026-01-01T00:00:00Z",
                    None,
                    None,
                    None,
                    None,
                    None,
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let screen: String = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(screen.contains("full value"), "{screen}");
        assert!(screen.contains("REPORT-DATA"), "{screen}");
        assert!(!screen.contains("TREE-ROW-VALUE"), "{screen}");
    }

    #[test]
    fn the_help_overlay_fits_at_80x24_without_losing_its_last_line() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let rendered = render::build_folded(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            None,
            ProcessState::Running,
            1.0,
            "2026-01-01T00:00:00Z",
            &[],
            true,
            &std::collections::BTreeSet::new(),
        );
        let mut app = App::new(false);
        app.dialog = Dialog::Help;
        let test_theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let test_ctx = DrawCtx {
            theme: &test_theme,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    None,
                    &[],
                    &mut app,
                    None,
                    &test_ctx,
                    0,
                    "2026-01-01T00:00:00Z",
                    None,
                    None,
                    None,
                    None,
                    None,
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        let text = terminal.backend().to_string();
        assert!(text.contains("esc") && text.contains("close"), "{text}");
    }

    #[test]
    fn running_tree_loop_with_max_concurrency() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        model.observe_event(&Event::Dispatch {
            ts: "2026-01-01T00:00:00Z".to_string(),
            path: "prime.fan".to_string(),
            branches: 4,
            concurrency: Some(2),
        });
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "error": null, "provider": "shell"}},
            "b": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "error": null, "provider": "shell"}},
            "fan": {"value": null, "meta": {"completed_at": null, "progress": {"done": 1, "total": 4, "elapsed_s": 1.0, "eta_s": 3.0}},
                "iter_0": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "error": null, "provider": "shell"}}
            }
        }});
        let text = snapshot(
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            "cof",
            "prime.fan",
        );
        insta::assert_snapshot!(text);
    }

    /// Review finding 6: with two passes of a known four-pass loop
    /// already done, "effects done out of planned" must multiply the
    /// loop body's own single leaf template by the loop's `progress.
    /// total` (6 planned: `a`, `b`, four `nap` passes) rather than
    /// counting `nap`'s own template path once (3) — and `done` must
    /// count each finished pass separately (4: `a`, `b`, `iter_0`,
    /// `iter_1`), not just leaves-with-a-template (3).
    #[test]
    fn running_tree_loop_with_two_passes_done() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        model.observe_event(&oscilloscope_core::observe::Event::Dispatch {
            ts: "2026-01-01T00:00:00Z".to_string(),
            path: "prime.fan".to_string(),
            branches: 4,
            concurrency: Some(2),
        });
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "error": null, "provider": "shell"}},
            "b": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "error": null, "provider": "shell"}},
            "fan": {"value": null, "meta": {"completed_at": null, "progress": {"done": 2, "total": 4, "elapsed_s": 2.0, "eta_s": 2.0}},
                "iter_0": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "error": null, "provider": "shell"}},
                "iter_1": {"value": "ok\n", "meta": {"created_at": "t1", "completed_at": "t2", "error": null, "provider": "shell"}}
            }
        }});
        let text = snapshot(
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            "cof",
            "prime.fan",
        );
        insta::assert_snapshot!(text);
    }

    /// Review finding 3 on #458's comment 3: a queued ◌ row, with
    /// `fan`'s own `max_concurrency 1` keeping `b` from running
    /// alongside `a`.
    #[test]
    fn queued_row_under_a_concurrency_bound() {
        let plan = tree_dynamic_plan();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "fan": {"value": null, "meta": {"completed_at": null},
                "a": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "provider": "shell"}}
            }
        }});
        let text = snapshot(
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            "cof",
            "prime.fan.b",
        );
        insta::assert_snapshot!(text);
    }

    /// Review finding 3 on #458's comment 3: the log pane with real
    /// lines, not left empty the way every other golden here leaves
    /// it.
    #[test]
    fn log_pane_with_lines() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "error": null, "provider": "shell"}}
        }});
        let log_lines = vec![
            "11:04:58 ▶ prime.a  tool".to_string(),
            "11:04:58 ✓ prime.a  0.5s  ok".to_string(),
        ];
        let text = snapshot_with_log(
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            "cof",
            "prime.a",
            &log_lines,
        );
        insta::assert_snapshot!(text);
    }

    /// #610: once a report has arrived, the header shows it
    /// (warning-styled) in place of the run id, and the log pane
    /// carries the report's own `▸`/`▸ warning:` line, data preview
    /// included.
    #[test]
    fn header_and_log_show_a_report() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        model.observe_event(&Event::RunStart {
            ts: "2026-01-01T11:04:00Z".to_string(),
            run_id: "run-abc123".to_string(),
            pid: Some(1),
            engine: None,
        });
        model.observe_event(&Event::Report {
            ts: "2026-01-01T11:04:58Z".to_string(),
            path: "prime".to_string(),
            level: oscilloscope_core::observe::ReportLevel::Warning,
            message: "running behind schedule".to_string(),
            data: Some(json!({"behind_s": 42})),
            data_truncated: false,
        });
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "error": null, "provider": "shell"}}
        }});
        let log_lines =
            vec!["11:04:58 ▸ warning: running behind schedule  {\"behind_s\":42}".to_string()];
        let text = snapshot_with_log(
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            "cof",
            "prime.a",
            &log_lines,
        );
        insta::assert_snapshot!(text);
    }

    // Issue #626 moved the header line's own truncation order (the
    // tabs shrink first, then the report, then the document, then
    // the ETA) into `oscilloscope_core::screens::header_line` itself
    // -- `oscilloscope-core`'s own tests cover the order directly
    // (`header_line_shrinks_tabs_to_numbers_first`, `header_line_
    // truncates_report_before_document_at_80_columns`, `header_
    // line_drops_eta_before_status_at_50_columns`, the report-
    // collapses-to-a-bare-mark cases around line 3300). This TUI-
    // level copy used to additionally prove a report reaches the
    // screen through `draw`'s own one-line header, at a terminal
    // narrower than the report text itself (20 columns) -- issue
    // #678's shared 60×24 floor means `draw` never renders that
    // header at all below 60 columns on any screen now, so there is
    // no longer a real terminal size left to exercise it at.

    /// #610: a warning-level report is styled in the theme's own
    /// warning colour in the Flow screen's own story pane (issue
    /// #691 replaced the generic log-pane line this used to also
    /// check, which no screen shows any more).
    #[test]
    fn a_warning_report_is_styled_in_the_themes_warning_colour_in_the_story_pane() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        model.observe_event(&Event::Report {
            ts: "t".to_string(),
            path: "prime".to_string(),
            level: oscilloscope_core::observe::ReportLevel::Warning,
            message: "behind".to_string(),
            data: None,
            data_truncated: false,
        });
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "error": null, "provider": "shell"}}
        }});
        let rendered = render::build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            5.0,
            "2026-01-01T00:00:05Z",
            &[],
        );
        let theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Dracula);
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Flow;
        app.selected_path = Some("prime.a".to_string());
        app.follow = false;
        let log_lines = vec!["11:04:58 ▸ warning: behind".to_string()];
        let story_lines = vec![oscilloscope_core::diff::LogLine {
            ts: Some("2026-01-01T11:04:58Z".to_string()),
            text: "▸ warning: behind".to_string(),
            kind: oscilloscope_core::diff::LogKind::Report,
            path: Some("prime".to_string()),
        }];
        let flow_screen = oscilloscope_core::screens::build_flow(
            &rendered.unfolded_rows,
            &rendered.rows,
            &oscilloscope_core::model::flatten_state(&state),
            &plan,
            &model,
            &rendered.header,
            Some(&state),
            &story_lines,
            Some("prime.a"),
            &test_zone(),
            rendered.header.run_end_ts.as_deref(),
            "2026-01-01T00:00:00Z",
        );
        // 34 tall, not 30: issue #678's shared chrome reserves 4 of
        // those rows (header band, Totals, status line, key bar)
        // outside the body, so the Flow screen's own three-pane
        // threshold (its own story pane, the second `▸` below) needs
        // 4 more than it used to for the same effective body height.
        let backend = TestBackend::new(100, 34);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    None,
                    &log_lines,
                    &mut app,
                    Some(&state),
                    &ctx,
                    0,
                    "2026-01-01T00:00:00Z",
                    None,
                    None,
                    Some(&flow_screen),
                    None,
                    None,
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();

        // The rail's own indicator line (◂ Start ▸ ...) has an
        // unrelated ▸ of its own, so rather than assume an ordinal
        // position, this checks that some ▸ in the buffer carries
        // the theme's warning colour -- the Story pane's own entry.
        let warning_rows: Vec<u16> = (0..buffer.area.height)
            .filter(|&y| {
                (0..buffer.area.width)
                    .any(|x| buffer[(x, y)].symbol() == "▸" && buffer[(x, y)].fg == theme.warning)
            })
            .collect();
        assert_eq!(
            warning_rows.len(),
            1,
            "the Flow screen's own story pane must have one warning-coloured ▸ row: {warning_rows:?}"
        );
    }

    /// #610: a named report writes a state node (#605), but it
    /// must never be a row in the orchestration tree, even once
    /// selected elsewhere brings up its own details.
    #[test]
    fn tree_has_no_row_for_a_named_report() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        // `sample_plan` has no report of its own to mark
        // `prime.status` through `PlanTree::is_report_path` -- the
        // same `--events` `report` line a real engine would also
        // emit for it is what `RunModel::observe` falls back to
        // instead (the "no plan" rule), exactly as `osi watch` or a
        // `use: inline:`/`ref:` child would have to rely on it too.
        model.observe_event(&Event::Report {
            ts: "t0".to_string(),
            path: "prime.status".to_string(),
            level: oscilloscope_core::observe::ReportLevel::Info,
            message: "on track".to_string(),
            data: None,
            data_truncated: false,
        });
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "error": null, "provider": "shell"}},
            "status": {"value": {"message": "on track", "level": "info", "data": null},
                "meta": {"created_at": "t0", "completed_at": "t0"}}
        }});
        let text = snapshot(
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            "cof",
            "prime.a",
        );
        // The report's own state node still appears in the *state*
        // pane (its own details apply there) -- only the
        // orchestration pane itself must never show a `status` row.
        // Issue #632 removed screen 2's own token bar (the previous
        // end anchor here, a full-width strip below every column so
        // it could never recur earlier) -- the orchestration pane is
        // now the last thing screen 2 draws, so everything after its
        // own title is in scope, and nothing later in the buffer
        // (the state/log columns' own trailing blank padding) has a
        // `status` of its own to false-positive on.
        let orchestration_pane = text
            .split_once("Flow ·")
            .map(|(_, after)| after)
            .expect("the flow pane should be in the snapshot");
        assert!(
            !orchestration_pane.contains("status"),
            "a named report must never show as a row in the orchestration tree: {orchestration_pane}"
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn failed() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = json!({
            "runtime": {"last_run": {"completed_at": "t1"}},
            "prime": {"value": null, "meta": {"completed_at": "t1", "error": "prime.a: /bin/ls failed (exit 1): ls: no"},
            "a": {"value": null, "meta": {"created_at": "t0", "completed_at": "t1", "error": "/bin/ls failed (exit 1): ls: no", "provider": "shell"}}
        }});
        let text = snapshot(
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(false),
            "cof",
            "prime.a",
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn failed_and_handled() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = json!({
            "runtime": {"last_run": {"completed_at": "t1"}},
            "prime": {"value": true, "meta": {"completed_at": "t1", "error": null},
            "a": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "error": null, "provider": "shell"}},
            "b": {"value": null, "meta": {"created_at": "t0", "completed_at": "t1", "error": "/bin/ls failed (exit 1): ls: no", "provider": "shell"}}
        }});
        let text = snapshot(
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(false),
            "cof",
            "prime.b",
        );
        insta::assert_snapshot!(text);
    }

    /// Issue #691's own follow-up: once a run has ended, there is
    /// nothing left to follow -- the Flow's own title must drop
    /// `· Following` the instant the run does, never waiting on a
    /// key press to notice (the Tape's own chrome already behaves
    /// this way: its status line reads the run's own end the moment
    /// it happens, regardless of any selection or follow state).
    #[test]
    fn the_title_drops_following_the_instant_the_run_ends() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = json!({
            "runtime": {"last_run": {"completed_at": "t1"}},
            "prime": {"value": true, "meta": {"completed_at": "t1", "error": null},
            "a": {"value": "ok\n", "meta": {"created_at": "t0", "completed_at": "t1", "error": null, "provider": "shell"}}
        }});
        let text = snapshot_at(
            120,
            24,
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(false),
            "cof",
            "prime.a",
            &[],
            true,
        );
        assert!(
            !text.contains("Following"),
            "an ended run has nothing left to follow: {text}"
        );
    }

    #[test]
    fn cancelled_row_with_no_final_write_yet() {
        // The process exited and osi itself forwarded a signal
        // (`interrupted: true`), but the state it has is a snapshot
        // from *before* the final write landed (no `runtime` key at
        // all) -- a transient race, not an ended run. `prime.a`'s own
        // row still reads cancelled (DESIGN.md §2.1 rule 2), but the
        // header, which has no sign this run specifically failed, now
        // correctly reads "aborted" rather than reusing the row's own
        // `interrupted` flag (see
        // `header_reads_cancelled_once_the_final_state_lands` below for
        // the real cancelled-header case).
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "provider": "shell"}}
        }});
        let text = snapshot(
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(true),
            "cof",
            "prime.a",
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn header_reads_cancelled_once_the_final_state_lands() {
        // A regression: a run the user cancelled
        // (first SIGINT) always gets a final write before cof exits
        // (DESIGN.md §4.1) -- `runtime.last_run.completed_at` set and
        // `prime.meta.error` starting with "Interrupted". The header
        // must read "cancelled", not "failed".
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = json!({
            "runtime": {"last_run": {"completed_at": "t1"}},
            "prime": {"value": false, "meta": {"completed_at": "t1", "error": "Interrupted (Ctrl-C/SIGINT)"},
            "a": {"value": null, "meta": {"created_at": "t0", "completed_at": "t1", "error": null, "provider": "shell"}}
        }});
        let text = snapshot(
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(true),
            "cof",
            "prime.a",
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn aborted_no_final_state() {
        // The other one-column-fallback golden kept at 70×20.
        let plan = sample_plan();
        let mut model = RunModel::new();
        let text = snapshot_one_column(
            &plan,
            &mut model,
            None,
            ProcessState::exited(false),
            "cof",
            "prime.a",
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn header_reads_failed_for_an_invalid_document_with_no_final_state() {
        // A regression: an invalid document
        // (cof exits 1 before execution, with no state file and its
        // own `{"ok": false, "error": ...}` JSON on stdout) must read
        // "failed", not "aborted" (DESIGN.md §2.1 rule 7's "no ended
        // state" branch).
        let plan = sample_plan();
        let mut model = RunModel::new();
        let text = snapshot(
            &plan,
            &mut model,
            None,
            ProcessState::Exited {
                interrupted: false,
                likely_failed: true,
                has_failure_reason: true,
                exited_at: None,
            },
            "cof",
            "prime.a",
        );
        insta::assert_snapshot!(text);
    }

    #[test]
    fn osi_watch() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "provider": "shell"}}
        }});
        let text = snapshot(
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            "watch",
            "prime.a",
        );
        insta::assert_snapshot!(text);
    }

    /// Every golden in this module that selects a plain station
    /// funnels through this one draw call (issue #691: the Flow
    /// screen replaces the old tree, but every other screen's own
    /// selected-pane/log-pane behaviour this module checks is
    /// unrelated to that and still needs a real selection to pin a
    /// row's own details against).
    #[allow(clippy::too_many_arguments)]
    fn snapshot_at(
        width: u16,
        height: u16,
        plan: &PlanTree,
        model: &mut RunModel,
        state: Option<&Value>,
        process: ProcessState,
        engine: &str,
        selected: &str,
        log_lines: &[String],
        follow: bool,
    ) -> String {
        let rendered = render::build(
            "do-thing.yml",
            engine,
            plan,
            model,
            state,
            process,
            5.0,
            "2026-01-01T00:00:05Z",
            &[],
        );
        let flow_screen = oscilloscope_core::screens::build_flow(
            &rendered.unfolded_rows,
            &rendered.rows,
            &oscilloscope_core::model::flatten_state(state.unwrap_or(&Value::Null)),
            plan,
            model,
            &rendered.header,
            state,
            &[],
            Some(selected),
            &test_zone(),
            rendered.header.run_end_ts.as_deref(),
            "2026-01-01T00:00:00Z",
        );
        let mut app = App::new(engine == "watch");
        app.screen = oscilloscope_core::screens::Screen::Flow;
        app.selected_path = Some(selected.to_string());
        app.follow = follow;
        let test_theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let test_ctx = DrawCtx {
            theme: &test_theme,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    None,
                    log_lines,
                    &mut app,
                    state,
                    &test_ctx,
                    0,
                    "2026-01-01T00:00:00Z",
                    None,
                    None,
                    Some(&flow_screen),
                    None,
                    None,
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        terminal.backend().to_string()
    }

    /// [`snapshot_at`], at 100x24 -- the two-column size, under
    /// `thresholds::TWO_COLUMN_MIN_WIDTH`/`_MIN_HEIGHT`'s own
    /// opposite, big enough for the Flow rail's own Story pane too.
    #[allow(clippy::too_many_arguments)]
    fn snapshot(
        plan: &PlanTree,
        model: &mut RunModel,
        state: Option<&Value>,
        process: ProcessState,
        engine: &str,
        selected: &str,
    ) -> String {
        snapshot_at(
            100,
            24,
            plan,
            model,
            state,
            process,
            engine,
            selected,
            &[],
            false,
        )
    }

    /// [`snapshot`], at 70x20 -- below 80 columns, so the Flow
    /// screen's own fallback to the Tape is what gets exercised for
    /// any golden here that selects screen 2.
    #[allow(clippy::too_many_arguments)]
    fn snapshot_one_column(
        plan: &PlanTree,
        model: &mut RunModel,
        state: Option<&Value>,
        process: ProcessState,
        engine: &str,
        selected: &str,
    ) -> String {
        snapshot_at(
            70,
            20,
            plan,
            model,
            state,
            process,
            engine,
            selected,
            &[],
            false,
        )
    }

    /// [`snapshot`], with the log pane given real lines (review
    /// finding 3 on #458's comment 3: every golden here left it
    /// empty) -- `snapshot` itself is just this with none.
    #[allow(clippy::too_many_arguments)]
    fn snapshot_with_log(
        plan: &PlanTree,
        model: &mut RunModel,
        state: Option<&Value>,
        process: ProcessState,
        engine: &str,
        selected: &str,
        log_lines: &[String],
    ) -> String {
        snapshot_at(
            100, 24, plan, model, state, process, engine, selected, log_lines, false,
        )
    }

    /// [`snapshot_at`], at a terminal big enough for every pane.
    #[allow(clippy::too_many_arguments)]
    fn snapshot_full_layout(
        plan: &PlanTree,
        model: &mut RunModel,
        state: Option<&Value>,
        process: ProcessState,
        engine: &str,
        selected: &str,
        log_lines: &[String],
    ) -> String {
        snapshot_at(
            120, 40, plan, model, state, process, engine, selected, log_lines, false,
        )
    }

    /// [`sample_plan`]'s own `a` is a tool, not a prompt -- selecting
    /// it here would read as `RowKind::Tool` regardless of any
    /// `prompt_sent`/`tokens_*` meta the state carries, since the
    /// plan always wins over a meta-shape guess, leaving this golden
    /// with no `sent`/`received` blocks to actually show at all. A
    /// plan whose `a` really is a prompt is the one thing this test
    /// needs beyond `sample_plan`'s own shape.
    fn prompt_a_tool_b_plan() -> PlanTree {
        let root_path = EffectPath::root();
        let a_path = root_path.clone().push_name("a");
        let b_path = root_path.clone().push_name("b");
        let root = Op {
            path: root_path,
            name: Some("prime".to_string()),
            kind: NodeKind::Control(Region::Block {
                ops: vec![prompt(a_path, "a"), tool(b_path, "b", OnError::Continue)],
                overlay: false,
            }),
            on_error: OnError::Fail,
            labels: None,
            enabled: true,
        };
        let program = Program {
            root,
            prompts: Default::default(),
            effect_names: Default::default(),
            document: None,
            runtime_block: None,
            interface: None,
            adapter: None,
            model: None,
        };
        PlanTree::from_program(&program)
    }

    /// A selected, finished prompt at 120×40 -- the
    /// `sent`/`received` blocks must land right after `path`/`type`,
    /// before `plan`/the meta list, and each snippet must cap at
    /// [`SNIPPET_MAX_LINES`] wrapped lines so both blocks stay on
    /// screen together rather than one pushing the other past the
    /// details pane's own golden-section height.
    #[test]
    fn full_layout_with_a_selected_finished_prompts_sent_received_blocks() {
        let plan = prompt_a_tool_b_plan();
        let mut model = RunModel::new();
        let long_prompt = "summarize the quarterly report and call out every risk factor you can find in the appendix tables, in plain prose, no bullet points";
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": "the summary text", "meta": {"created_at": "t0", "completed_at": "t1", "adapter": "scripted", "prompt_sent": long_prompt, "prompt_chars": 131, "tokens_sent_total": 120, "tokens_received_total": 340}},
            "b": {"value": "ok\n", "meta": {"created_at": "t1", "completed_at": "t2", "provider": "shell"}}
        }});
        let text = snapshot_full_layout(
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            "cof",
            "prime.a",
            &[],
        );
        insta::assert_snapshot!(text);
    }

    /// Issue #635: a selected prompt that hasn't replied yet shows
    /// `asked:` with its own sent snippet, then `doing:` with
    /// "waiting for the reply" and its own in-flight token count --
    /// `produced:` reads "(when done)" rather than any real value,
    /// since nothing has come back yet.
    #[test]
    fn full_layout_with_a_selected_running_prompt_shows_asked_and_waiting_for_the_reply() {
        let plan = prompt_a_tool_b_plan();
        let mut model = RunModel::new();
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "a": {"value": null, "meta": {"created_at": "t0", "completed_at": null, "adapter": "scripted",
                "prompt_sent": "summarize the quarterly report", "prompt_chars": 30, "tokens_sent_total": 42}}
        }});
        let text = snapshot_full_layout(
            &plan,
            &mut model,
            Some(&state),
            ProcessState::Running,
            "cof",
            "prime.a",
            &[],
        );
        assert!(text.contains("asked:"), "{text}");
        assert!(text.contains("waiting for the reply"), "{text}");
        assert!(text.contains("(when done)"), "{text}");
        insta::assert_snapshot!(text);
    }

    /// A leaf that never got `completed_at` because the run itself
    /// ended first (aborted or cancelled) is in the exact same data
    /// shape as a still-running one -- only its own status tells the
    /// two apart, and it must never read as a real (if tiny) token
    /// count.
    #[test]
    fn an_aborted_leafs_placeholder_weight_never_reads_as_a_real_number() {
        // `cut_off`'s own `weight` (what boosting a running/aborted
        // placeholder actually touches, `treemap`'s own tests) is
        // comparable to its sibling's, large enough to draw a real
        // share of the row -- but its `total` (the real value) is
        // honestly zero, which is what this test pins: `draw_cost_
        // treemap`'s display-text decision reads `total`, never
        // `weight`, so the boosted number itself can never leak into
        // the label text.
        let tree = vec![
            TreemapNode::leaf("big", 1_000_000, Some(StatusKind::Done)),
            TreemapNode {
                key: "cut_off".to_string(),
                path: "cut_off".to_string(),
                total: 0,
                weight: 900_000,
                status: Some(StatusKind::Aborted),
                live: false,
                live_turns: None,
                children: Vec::new(),
            },
        ];
        let screen = oscilloscope_core::screens::CostScreen {
            measure: Measure::Total,
            tree,
            stores_without_measure: 0,
            stores: oscilloscope_core::cost::LargestStores::default(),
            tokens_over_time: Vec::new(),
            chart_start_ts: None,
            chart_end_ts: "2026-01-01T00:00:00Z".to_string(),
        };
        let mut app = App::new(false);
        let test_theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let ctx = DrawCtx {
            theme: &test_theme,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut cost_layout = CostLayoutCache::default();
        terminal
            .draw(|f| {
                let area = f.area();
                draw_cost_treemap(f, area, &screen, &mut app, &mut cost_layout, &ctx);
            })
            .unwrap();
        let text = terminal.backend().to_string();
        assert!(text.contains("no tokens reported"), "{text}");
        assert!(!text.contains("cut_off 900"), "{text}");
    }

    fn cost_table_screen(
        rows: Vec<oscilloscope_core::cost::StoreRow>,
    ) -> oscilloscope_core::screens::CostScreen {
        let run_tokens_sent = rows.iter().map(|r| r.tokens_sent).sum();
        let run_tokens_received = rows.iter().map(|r| r.tokens_received).sum();
        oscilloscope_core::screens::CostScreen {
            measure: Measure::Total,
            tree: Vec::new(),
            stores_without_measure: 0,
            stores: oscilloscope_core::cost::LargestStores {
                rows,
                footer: oscilloscope_core::cost::StoresFooter {
                    run_tokens_sent,
                    run_tokens_received,
                    run_cost_usd: None,
                    gap_tokens: 0,
                    cost_reporting: 0,
                    cost_stores: 0,
                },
            },
            tokens_over_time: Vec::new(),
            chart_start_ts: None,
            chart_end_ts: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    fn draw_cost_table_text(
        screen: &oscilloscope_core::screens::CostScreen,
        width: u16,
        height: u16,
    ) -> String {
        let mut app = App::new(false);
        let test_theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let ctx = DrawCtx {
            theme: &test_theme,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                draw_cost_table(f, area, screen, &mut app, &ctx);
            })
            .unwrap();
        terminal.backend().to_string()
    }

    /// 20 stores in the table pane an 80×24 terminal's own layout
    /// actually carves out for it (DESIGN.md's small-terminal rule:
    /// the treemap above, the table taking the rest -- no chart below
    /// 100 columns wide): the footer (the run's own totals, its cost
    /// coverage) must still show in full, at the bottom, with a
    /// `+N more` line accounting for whatever rows had to drop to
    /// make room for it, never the footer itself scrolled off the
    /// bottom the way appending it after every row used to risk.
    #[test]
    fn the_footer_stays_pinned_with_20_stores_at_80x24() {
        let rows: Vec<oscilloscope_core::cost::StoreRow> = (0..20)
            .map(|i| oscilloscope_core::cost::StoreRow {
                label: format!("store{i}"),
                path: format!("prime.store{i}"),
                tokens_sent: 100,
                tokens_received: 50,
                cost_usd: None,
                status: Some(StatusKind::Done),
            })
            .collect();
        let screen = cost_table_screen(rows);
        // The table pane's own area at a nominal 80×24 terminal
        // (`draw_cost_screen`'s own derivation: a 1-line header, then
        // the treemap's golden-major share of the remaining height,
        // the table taking the rest, full width since the chart needs
        // 100 columns).
        let text = draw_cost_table_text(&screen, 80, 9);
        assert!(
            text.contains("total ↑"),
            "the footer's own totals line must still show: {text}"
        );
        assert!(
            text.contains("more"),
            "rows had to be dropped to fit, so a +N more line must show: {text}"
        );
        assert!(text.contains("store0"), "{text}");
    }

    /// DESIGN.md: a still-running store's table row reads the same
    /// "tokens when done" its tile does, never `↑0 t ↓0 t 0%` read
    /// back as if it meant something.
    #[test]
    fn a_running_rows_own_table_line_reads_tokens_when_done() {
        let rows = vec![
            oscilloscope_core::cost::StoreRow {
                label: "review".to_string(),
                path: "prime.review".to_string(),
                tokens_sent: 0,
                tokens_received: 0,
                cost_usd: None,
                status: Some(StatusKind::Running),
            },
            oscilloscope_core::cost::StoreRow {
                label: "plan".to_string(),
                path: "prime.plan".to_string(),
                tokens_sent: 100,
                tokens_received: 50,
                cost_usd: None,
                status: Some(StatusKind::Done),
            },
        ];
        let screen = cost_table_screen(rows);
        let text = draw_cost_table_text(&screen, 60, 10);
        assert!(
            text.contains("review  tokens when done"),
            "a running row must read \"tokens when done\", not 0 t: {text}"
        );
        assert!(!text.contains("↑0 t ↓0 t"), "{text}");
    }

    /// DESIGN.md's own performance rule, directly, rather than only
    /// through the timing bound [`drawing_a_2000_node_state_stays_
    /// well_under_the_redraw_interval`] checks indirectly -- corrupts
    /// the cache's own stored cells after the first call, then calls
    /// again with the identical nodes and area: a cache *hit* must
    /// return that untouched corruption, not a freshly squarified
    /// (and so no longer corrupt) set of cells.
    #[test]
    fn cost_layout_cache_skips_squarify_when_neither_the_tree_nor_the_area_changed() {
        let nodes = vec![
            TreemapNode::leaf("a", 2000, Some(StatusKind::Done)),
            TreemapNode::leaf("b", 1000, Some(StatusKind::Done)),
        ];
        let area = Rect::new(0, 0, 30, 10);
        let mut cache = CostLayoutCache::default();
        let first = cache.cells(&nodes, area).to_vec();
        assert!(!first.is_empty());
        cache.cells.clear();
        let second = cache.cells(&nodes, area);
        assert!(
            second.is_empty(),
            "a cache hit must skip squarify entirely, not recompute and overwrite the corruption"
        );
        // Changing the area is what must actually invalidate it.
        let third = cache.cells(&nodes, Rect::new(0, 0, 40, 12));
        assert!(
            !third.is_empty(),
            "a changed area must trigger a fresh layout"
        );
    }

    #[test]
    fn the_treemap_says_no_stores_yet_rather_than_drawing_an_empty_grid() {
        let screen = oscilloscope_core::screens::CostScreen {
            measure: Measure::Total,
            tree: Vec::new(),
            stores_without_measure: 0,
            stores: oscilloscope_core::cost::LargestStores::default(),
            tokens_over_time: Vec::new(),
            chart_start_ts: None,
            chart_end_ts: "2026-01-01T00:00:00Z".to_string(),
        };
        let mut app = App::new(false);
        let test_theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let ctx = DrawCtx {
            theme: &test_theme,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut cost_layout = CostLayoutCache::default();
        terminal
            .draw(|f| {
                let area = f.area();
                draw_cost_treemap(f, area, &screen, &mut app, &mut cost_layout, &ctx);
            })
            .unwrap();
        let text = terminal.backend().to_string();
        assert!(text.contains("no stores yet"), "{text}");
    }

    /// A zoomed treemap (DESIGN.md: "the zoomed store's own name at
    /// each level ... shown alongside the measure"): zooming into
    /// `fan` must show only its own children, title the pane with
    /// `fan`'s own name (not `prime.fan`) next to the measure, and
    /// leave `a`/`b` off screen entirely.
    #[test]
    fn zoomed_into_fan_at_120x40() {
        let plan = sample_plan();
        let mut model = RunModel::new();
        let state = cost_finished_state();
        let rendered = render::build_folded(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            ProcessState::exited(true),
            300.0,
            "2026-01-01T00:05:00Z",
            &[],
            true,
            &Default::default(),
        );
        let flat = oscilloscope_core::model::flatten_state(&state);
        let statuses: std::collections::BTreeMap<_, _> = rendered
            .unfolded_rows
            .iter()
            .map(|r| (r.path.clone(), r.status.clone()))
            .collect();
        let kinds: std::collections::BTreeMap<_, _> = rendered
            .unfolded_rows
            .iter()
            .map(|r| (r.path.clone(), r.kind))
            .collect();
        let cost_screen = oscilloscope_core::screens::build_cost_screen(
            &rendered.rows,
            &rendered.header,
            &flat,
            &statuses,
            &kinds,
            Some(&state),
            Measure::Total,
            &model,
            "2026-01-01T00:05:00Z",
            PHASE_GLYPHS.len(),
        );
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Cost;
        app.cost_zoom = vec!["prime.fan".to_string()];
        let test_theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let test_ctx = DrawCtx {
            theme: &test_theme,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut cost_layout = CostLayoutCache::default();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    None,
                    &[],
                    &mut app,
                    Some(&state),
                    &test_ctx,
                    0,
                    "2026-01-01T00:05:00Z",
                    None,
                    Some(&cost_screen),
                    None,
                    None,
                    None,
                    &mut cost_layout,
                )
            })
            .unwrap();
        let text = terminal.backend().to_string();
        assert!(text.contains("tokens by store · tokens · fan"), "{text}");
        assert!(text.contains("iter_0"), "{text}");
        assert!(
            !text.contains("a 900"),
            "a must be off screen while zoomed: {text}"
        );
        assert!(
            text.contains("iter_0 › nap"),
            "zoomed into fan, iter_0's table row must read relative to it: {text}"
        );
        assert!(
            text.contains("iter_1 › nap"),
            "zoomed into fan, iter_1's table row must read relative to it: {text}"
        );
        assert!(
            !text.contains("fan ›"),
            "zoomed into fan, no table row may still carry fan's own name as a prefix: {text}"
        );
    }
}

/// Issue #535's own "buffer test" acceptance item: pins a highlighted
/// cell's style, and that a named theme paints the background of an
/// empty cell -- both read straight from a `ratatui::buffer::Buffer`,
/// not a `row_line`/`Line` style, since that's what the acceptance
/// item actually asks for.
#[cfg(test)]
mod highlight_tests {
    use super::*;
    use oscilloscope_core::model::{RowStatus, StatusKind};
    use oscilloscope_core::render::{Header, RenderState, Row, RowKind};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::theme::{Theme, ThemeName};

    fn minimal_render_state() -> RenderState {
        RenderState {
            header: Header {
                document: "do-thing.yml".to_string(),
                engine: "cof".to_string(),
                run_id: None,
                status: oscilloscope_core::model::RunStatus::Running,
                elapsed_s: 1.0,
                run_start_ts: None,
                run_end_ts: None,
                effects_done: 1,
                effects_planned: Some(1),
                effects_planned_is_lower_bound: false,
                effects_planned_full: None,
                tokens_sent: 0,
                tokens_received: 0,
                tokens_sent_live_estimate: 0,
                tokens_received_live_estimate: 0,
                innermost_loop_eta_s: None,
                innermost_loop_eta_ts: None,
                history_eta_ts: None,
                history_eta_past_usual: false,
                history_eta_remaining_s: None,
                history_eta_low_ts: None,
                history_eta_high_ts: None,
                history_vs_usual_s: None,
                history_usual_total_s: None,
                history_usual_total_tokens: None,
                history_same_hash_count: None,
                history_step_usual: Default::default(),
                latest_report: None,
                engine_alive: true,
            },
            unfolded_rows: Vec::new(),
            rows: vec![Row {
                path: "prime.a".to_string(),
                depth: 1,
                label: "a".to_string(),
                kind: RowKind::Tool,
                status: RowStatus {
                    kind: StatusKind::Done,
                    skip_reason: None,
                    retrying: false,
                },
                duration_s: None,
                loop_progress: None,
                dim: None,
                has_children: false,
            }],
            state_rows: Vec::new(),
            token_bar: Default::default(),
            log: Vec::new(),
        }
    }

    #[test]
    fn the_icon_draws_at_every_width() {
        let rendered = minimal_render_state();
        let theme = Theme::for_name(ThemeName::Terminal);
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let first_cell_is_blank = |width: u16| -> bool {
            let mut app = App::new(false);
            app.screen = oscilloscope_core::screens::Screen::Cost;
            let backend = TestBackend::new(width, 24);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|f| {
                    draw(
                        f,
                        &rendered,
                        None,
                        &[],
                        &mut app,
                        None,
                        &ctx,
                        0,
                        "2026-01-01T00:00:00Z",
                        None,
                        None,
                        None,
                        None,
                        None,
                        &mut CostLayoutCache::default(),
                    )
                })
                .unwrap();
            terminal.backend().buffer()[(1, 0)].symbol() == " "
        };
        // 60, not an arbitrarily narrower width: issue #678's own
        // 60x24 floor means nothing ever renders this band at all
        // below it, on any screen.
        assert!(
            !first_cell_is_blank(60),
            "the icon's own trace must draw even on a narrow terminal"
        );
        assert!(
            !first_cell_is_blank(100),
            "the icon's own trace must draw on a wide terminal too"
        );
    }

    /// Issue #535's own theme acceptance item: a named theme paints
    /// the whole screen's background, so an otherwise-untouched cell
    /// still reads the theme's background colour. `(95, 21)` is
    /// blank space in the Totals row (issue #678's shared chrome),
    /// to the right of its own content -- that row's own `Paragraph`
    /// sets no style of its own, unlike the header band above it,
    /// which fills its entire row, so this is the simplest cell
    /// left showing only the base fill.
    #[test]
    fn a_named_theme_paints_the_background_of_an_empty_cell() {
        let rendered = minimal_render_state();
        let theme = Theme::for_name(ThemeName::Nord);
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Cost;
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    None,
                    &[],
                    &mut app,
                    None,
                    &ctx,
                    0,
                    "2026-01-01T00:00:00Z",
                    None,
                    None,
                    None,
                    None,
                    None,
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(95, 21)].bg, theme.background.unwrap());
    }

    /// Finding 7: the base fill set only `bg`, so a named theme's
    /// own `text` colour never actually painted anything -- a plain
    /// cell (nothing drawn over it but the base fill) must read the
    /// theme's `text`, not the terminal's own default foreground.
    /// `(95, 21)` is blank space in the Totals row (issue #678's
    /// shared chrome), to the right of its own content -- the
    /// simplest cell that's reliably untouched by anything but the
    /// base fill.
    #[test]
    fn a_named_theme_paints_the_default_text_colour_of_an_empty_cell() {
        let rendered = minimal_render_state();
        let theme = Theme::for_name(ThemeName::SolarizedLight);
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Cost;
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    None,
                    &[],
                    &mut app,
                    None,
                    &ctx,
                    0,
                    "2026-01-01T00:00:00Z",
                    None,
                    None,
                    None,
                    None,
                    None,
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(95, 21)].symbol(), " ");
        assert_eq!(buffer[(95, 21)].fg, theme.text);
    }

    #[test]
    fn the_terminal_theme_paints_no_background_at_all() {
        let rendered = minimal_render_state();
        let theme = Theme::for_name(ThemeName::Terminal);
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Cost;
        let backend = TestBackend::new(100, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    None,
                    &[],
                    &mut app,
                    None,
                    &ctx,
                    0,
                    "2026-01-01T00:00:00Z",
                    None,
                    None,
                    None,
                    None,
                    None,
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].bg, Color::Reset);
    }
}

#[cfg(test)]
mod tape_live_tests {
    use super::*;
    use oscilloscope_core::model::{ProcessState, RunModel};
    use oscilloscope_core::plan::PlanTree;
    use oscilloscope_core::render;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use serde_json::{Value, json};

    use crate::keys::App;

    // -------------------------------------------------------------
    // A running prompt's live token figures (issue #675)
    // -------------------------------------------------------------

    fn tape_snapshot_at(width: u16, height: u16, state: &Value, now: &str) -> String {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        let rendered = render::build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            Some(state),
            ProcessState::Running,
            1.0,
            now,
            &[],
        );
        let flat = oscilloscope_core::model::flatten_state(state);
        let tape_screen = oscilloscope_core::screens::build_tape(
            &rendered.rows,
            &rendered.header,
            &flat,
            &plan,
            &model,
            Some(state),
            Some("prime.ask"),
            true,
            now,
        );
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Tape;
        let test_theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let test_ctx = DrawCtx {
            theme: &test_theme,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    None,
                    &[],
                    &mut app,
                    Some(state),
                    &test_ctx,
                    0,
                    now,
                    Some(&tape_screen),
                    None,
                    None,
                    None,
                    None,
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        terminal.backend().to_string()
    }

    fn running_prompt_state(live: Value) -> Value {
        json!({"prime": {"value": null, "meta": {"completed_at": null},
            "ask": {"value": null, "meta": {"created_at": "2026-10-08T11:46:43Z", "completed_at": null,
                "adapter": live["adapter"].clone(), "model": "m", "prompt_chars": 1400,
                "prompt_sent": "Reply as the maintainer.",
                "live": live}}
        }})
    }

    const TAPE_NOW: &str = "2026-10-08T11:46:44Z";

    fn waiting_for_first_token_state() -> Value {
        running_prompt_state(json!({
            "v": 1, "attempt": 0, "adapter": "ollama", "model": "m",
            "started_at": "2026-10-08T11:46:43Z", "updated_at": "2026-10-08T11:46:43Z",
            "streaming": true, "first_token_at": null,
            "tokens_sent": null, "tokens_sent_estimate": 350, "tokens_received": null,
            "tokens_received_estimate": null, "chars_received": 0, "tokens_per_second": null
        }))
    }

    #[test]
    fn tape_running_prompt_waiting_for_first_token_at_120x30() {
        insta::assert_snapshot!(tape_snapshot_at(
            120,
            30,
            &waiting_for_first_token_state(),
            TAPE_NOW
        ));
    }

    #[test]
    fn tape_running_prompt_waiting_for_first_token_at_80x24() {
        insta::assert_snapshot!(tape_snapshot_at(
            80,
            24,
            &waiting_for_first_token_state(),
            TAPE_NOW
        ));
    }

    /// Mid-stream with estimates only -- the Ollama shape.
    fn mid_stream_estimate_state() -> Value {
        running_prompt_state(json!({
            "v": 1, "attempt": 0, "adapter": "ollama", "model": "m",
            "started_at": "2026-10-08T11:46:43Z", "updated_at": "2026-10-08T11:46:44Z",
            "streaming": true, "first_token_at": "2026-10-08T11:46:43.6Z",
            "tokens_sent": null, "tokens_sent_estimate": 350, "tokens_received": null,
            "tokens_received_estimate": 41, "chars_received": 160, "tokens_per_second": 68.0
        }))
    }

    #[test]
    fn tape_running_prompt_mid_stream_estimate_at_120x30() {
        insta::assert_snapshot!(tape_snapshot_at(
            120,
            30,
            &mid_stream_estimate_state(),
            TAPE_NOW
        ));
    }

    #[test]
    fn tape_running_prompt_mid_stream_estimate_at_80x24() {
        insta::assert_snapshot!(tape_snapshot_at(
            80,
            24,
            &mid_stream_estimate_state(),
            TAPE_NOW
        ));
    }

    /// Mid-stream with exact input tokens -- the Anthropic shape
    /// (`tokens_sent` set at `message_start`).
    fn mid_stream_exact_sent_state() -> Value {
        running_prompt_state(json!({
            "v": 1, "attempt": 0, "adapter": "anthropic", "model": "m",
            "started_at": "2026-10-08T11:46:43Z", "updated_at": "2026-10-08T11:46:44Z",
            "streaming": true, "first_token_at": "2026-10-08T11:46:43.7Z",
            "tokens_sent": 348, "tokens_sent_estimate": 350, "tokens_received": null,
            "tokens_received_estimate": 5, "chars_received": 20, "tokens_per_second": null
        }))
    }

    #[test]
    fn tape_running_prompt_mid_stream_exact_sent_at_120x30() {
        insta::assert_snapshot!(tape_snapshot_at(
            120,
            30,
            &mid_stream_exact_sent_state(),
            TAPE_NOW
        ));
    }

    #[test]
    fn tape_running_prompt_mid_stream_exact_sent_at_80x24() {
        insta::assert_snapshot!(tape_snapshot_at(
            80,
            24,
            &mid_stream_exact_sent_state(),
            TAPE_NOW
        ));
    }

    /// A non-streaming adapter: only the clock is live.
    fn non_streaming_state() -> Value {
        running_prompt_state(json!({
            "v": 1, "attempt": 0, "adapter": "litellm", "model": "m",
            "started_at": "2026-10-08T11:46:43Z", "updated_at": "2026-10-08T11:46:44Z",
            "streaming": false, "first_token_at": null,
            "tokens_sent": null, "tokens_sent_estimate": 350, "tokens_received": null,
            "tokens_received_estimate": null, "chars_received": null, "tokens_per_second": null
        }))
    }

    #[test]
    fn tape_running_prompt_non_streaming_adapter_at_120x30() {
        insta::assert_snapshot!(tape_snapshot_at(120, 30, &non_streaming_state(), TAPE_NOW));
    }

    #[test]
    fn tape_running_prompt_non_streaming_adapter_at_80x24() {
        insta::assert_snapshot!(tape_snapshot_at(80, 24, &non_streaming_state(), TAPE_NOW));
    }

    /// A second attempt after a retry.
    fn retry_attempt_state() -> Value {
        running_prompt_state(json!({
            "v": 1, "attempt": 1, "adapter": "ollama", "model": "m",
            "started_at": "2026-10-08T11:46:50Z", "updated_at": "2026-10-08T11:46:51Z",
            "streaming": true, "first_token_at": null,
            "tokens_sent": null, "tokens_sent_estimate": 350, "tokens_received": null,
            "tokens_received_estimate": null, "chars_received": 0, "tokens_per_second": null
        }))
    }

    #[test]
    fn tape_running_prompt_retry_attempt_at_120x30() {
        insta::assert_snapshot!(tape_snapshot_at(
            120,
            30,
            &retry_attempt_state(),
            "2026-10-08T11:46:51Z"
        ));
    }

    #[test]
    fn tape_running_prompt_retry_attempt_at_80x24() {
        insta::assert_snapshot!(tape_snapshot_at(
            80,
            24,
            &retry_attempt_state(),
            "2026-10-08T11:46:51Z"
        ));
    }

    /// A node whose `meta.live` is malformed never crashes -- read as
    /// today, elapsed time alone.
    #[test]
    fn tape_running_prompt_with_malformed_live_never_crashes() {
        let state = json!({"prime": {"value": null, "meta": {"completed_at": null},
            "ask": {"value": null, "meta": {"created_at": "2026-10-08T11:46:43Z", "completed_at": null,
                "adapter": "ollama", "model": "m", "prompt_chars": 40, "live": "not an object"}}
        }});
        insta::assert_snapshot!(tape_snapshot_at(120, 30, &state, TAPE_NOW));
    }

    /// Issue #675's events-only attach: `osi watch` on `--events`
    /// alone drives the same figures from the latest `progress` line,
    /// with no live state (`meta.live`) at all.
    #[test]
    fn tape_running_prompt_events_only_replay_at_120x30() {
        let plan = PlanTree::empty();
        let mut model = RunModel::new();
        model.observe_event(&oscilloscope_core::observe::Event::RunStart {
            ts: "2026-10-08T11:46:43Z".to_string(),
            run_id: "r1".to_string(),
            pid: None,
            engine: Some("cof".to_string()),
        });
        model.observe_event(&oscilloscope_core::observe::Event::Start {
            ts: "2026-10-08T11:46:43Z".to_string(),
            id: Some(1),
            path: "prime.ask".to_string(),
        });
        model.observe_event(&oscilloscope_core::observe::Event::Progress {
            ts: "2026-10-08T11:46:44Z".to_string(),
            id: Some(1),
            path: "prime.ask".to_string(),
            ms: Some(1200),
            tokens_received_estimate: Some(41),
            chars_received: Some(160),
            tokens_sent: None,
            tokens_sent_estimate: Some(350),
            first_token_ms: Some(600),
        });
        let now = "2026-10-08T11:46:44.2Z";
        let rendered = render::build(
            "do-thing.yml",
            "cof",
            &plan,
            &mut model,
            None,
            ProcessState::Running,
            1.2,
            now,
            &[],
        );
        let flat = oscilloscope_core::model::flatten_state(&Value::Null);
        let tape_screen = oscilloscope_core::screens::build_tape(
            &rendered.rows,
            &rendered.header,
            &flat,
            &plan,
            &model,
            None,
            Some("prime.ask"),
            true,
            now,
        );
        let mut app = App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Tape;
        let test_theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let test_ctx = DrawCtx {
            theme: &test_theme,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    None,
                    &[],
                    &mut app,
                    None,
                    &test_ctx,
                    0,
                    now,
                    Some(&tape_screen),
                    None,
                    None,
                    None,
                    None,
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        insta::assert_snapshot!(terminal.backend().to_string());
    }
}

/// The `.mock` frames under `../../design/frames/` (issue #678's
/// "the frames are the contract"): a plain-text grid of `{role
/// [bold|dim] [on:bg-role]}text{/}` spans, one line per screen row,
/// resolved through a real [`Theme`] and compared cell for cell
/// (symbol, fg, bg, bold, dim) against a real `ratatui::buffer::
/// Buffer` [`draw`] actually produced. A frame that cannot match
/// exactly is a design question, not something this parser papers
/// over: it panics on anything it does not recognise rather than
/// guessing.
#[cfg(test)]
mod tape_frame_goldens {
    use super::*;
    use oscilloscope_core::render::{Header, RenderState};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::theme::{Theme, ThemeName};

    /// The Normal/Usual goldens' own status-line row (y=28 at
    /// 120x30): `gate.reply` finishes 2 ms before two loop passes
    /// do, but §3.2 reads the newest finished step regardless, same
    /// as the frame -- `(51, 24)` is the unrelated Run End rounding
    /// cell both goldens already carry.
    const STATUS_LINE_READS_AN_EARLIER_STEP_120X30: [(u16, u16); 43] = [
        (51, 24),
        (3, 28),
        (4, 28),
        (5, 28),
        (6, 28),
        (7, 28),
        (8, 28),
        (10, 28),
        (11, 28),
        (12, 28),
        (13, 28),
        (14, 28),
        (15, 28),
        (16, 28),
        (17, 28),
        (18, 28),
        (20, 28),
        (21, 28),
        (22, 28),
        (23, 28),
        (24, 28),
        (25, 28),
        (26, 28),
        (27, 28),
        (28, 28),
        (29, 28),
        (30, 28),
        (31, 28),
        (32, 28),
        (33, 28),
        (34, 28),
        (35, 28),
        (36, 28),
        (38, 28),
        (39, 28),
        (40, 28),
        (41, 28),
        (42, 28),
        (43, 28),
        (44, 28),
        (45, 28),
        (46, 28),
        (47, 28),
    ];

    /// Same reason as [`STATUS_LINE_READS_AN_EARLIER_STEP_120X30`],
    /// at 80x24's own status-line row (y=22).
    const STATUS_LINE_READS_AN_EARLIER_STEP_80X24: [(u16, u16); 42] = [
        (3, 22),
        (4, 22),
        (5, 22),
        (6, 22),
        (7, 22),
        (8, 22),
        (10, 22),
        (11, 22),
        (12, 22),
        (13, 22),
        (14, 22),
        (15, 22),
        (16, 22),
        (17, 22),
        (18, 22),
        (20, 22),
        (21, 22),
        (22, 22),
        (23, 22),
        (24, 22),
        (25, 22),
        (26, 22),
        (27, 22),
        (28, 22),
        (29, 22),
        (30, 22),
        (31, 22),
        (32, 22),
        (33, 22),
        (34, 22),
        (35, 22),
        (36, 22),
        (38, 22),
        (39, 22),
        (40, 22),
        (41, 22),
        (42, 22),
        (43, 22),
        (44, 22),
        (45, 22),
        (46, 22),
        (47, 22),
    ];

    /// Same reason as [`STATUS_LINE_READS_AN_EARLIER_STEP_120X30`],
    /// at 60x24's own status-line row (y=22) -- narrower, so the
    /// row's own text (and the mismatch) cuts off sooner.
    const STATUS_LINE_READS_AN_EARLIER_STEP_60X24: [(u16, u16); 32] = [
        (3, 22),
        (4, 22),
        (5, 22),
        (6, 22),
        (7, 22),
        (8, 22),
        (10, 22),
        (11, 22),
        (12, 22),
        (13, 22),
        (14, 22),
        (15, 22),
        (16, 22),
        (17, 22),
        (18, 22),
        (20, 22),
        (21, 22),
        (22, 22),
        (23, 22),
        (24, 22),
        (25, 22),
        (26, 22),
        (27, 22),
        (28, 22),
        (29, 22),
        (30, 22),
        (31, 22),
        (32, 22),
        (33, 22),
        (34, 22),
        (35, 22),
        (36, 22),
    ];

    fn frames_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../design/frames")
    }

    /// The frames' own zone (issue #613's "one clock"): drawn in
    /// UTC-8 -- `run2/events.jsonl`'s `run_start` is `20:50:47Z`,
    /// the frames' header reads `12:50:47`.
    fn frame_zone() -> chrono::FixedOffset {
        chrono::FixedOffset::west_opt(8 * 3600).unwrap()
    }

    fn fixtures_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../design/fixtures")
    }

    fn load_json(path: &std::path::Path) -> serde_json::Value {
        serde_json::from_str(
            &std::fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("couldn't read {path:?}: {e}")),
        )
        .unwrap_or_else(|e| panic!("{path:?}: invalid JSON: {e}"))
    }

    /// Prunes a loaded `state.live.json` (a *finished* run's own
    /// document) down to what the real engine would actually have
    /// written to disk by `cutoff` -- a node the real run hadn't
    /// reached by then has no entry at all (not even a `null`
    /// placeholder), and a loop container not yet finished as a
    /// whole has no `value` of its own and no `completed_at`, its
    /// own `progress.done`/`completed_passes` recomputed from
    /// whichever of its own passes actually finished by `cutoff`.
    /// `rows`/the model get the mid-run truth for everything a real
    /// document never persists (what's running right now) from
    /// events alone, same as a real attached run would.
    fn at_or_before(ts: &str, cutoff: &str) -> bool {
        match (
            chrono::DateTime::parse_from_rfc3339(ts),
            chrono::DateTime::parse_from_rfc3339(cutoff),
        ) {
            (Ok(ts), Ok(cutoff)) => ts <= cutoff,
            _ => false,
        }
    }

    fn truncate_document(value: &mut serde_json::Value, cutoff: &str) -> bool {
        let Some(obj) = value.as_object_mut() else {
            return true;
        };
        if !obj.contains_key("meta") {
            // A bare container (a loop's own `iter_N`, or a wrapper
            // key like `prime` itself's non-node children): no
            // `created_at` of its own to check, only its children's.
            let keys: Vec<String> = obj.keys().cloned().collect();
            for key in keys {
                let keep = truncate_document(obj.get_mut(&key).unwrap(), cutoff);
                if !keep {
                    obj.remove(&key);
                }
            }
            return !obj.is_empty();
        }
        let created_at = obj
            .get("meta")
            .and_then(|m| m.get("created_at"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        if !created_at.is_some_and(|c| at_or_before(&c, cutoff)) {
            return false;
        }
        let completed_at = obj
            .get("meta")
            .and_then(|m| m.get("completed_at"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        let own_done = completed_at.is_some_and(|c| at_or_before(&c, cutoff));
        let keys: Vec<String> = obj
            .keys()
            .filter(|k| k.as_str() != "value" && k.as_str() != "meta")
            .cloned()
            .collect();
        for key in &keys {
            let keep = truncate_document(obj.get_mut(key).unwrap(), cutoff);
            if !keep {
                obj.remove(key);
            }
        }
        if !own_done {
            let has_progress = obj.get("meta").is_some_and(|m| m.get("progress").is_some());
            let done = keys
                .iter()
                .filter(|k| k.starts_with("iter_"))
                .filter(|k| {
                    let pass = obj.get(k.as_str()).and_then(serde_json::Value::as_object);
                    pass.is_some_and(|p| {
                        p.values().all(|leaf| {
                            leaf.get("meta")
                                .and_then(|m| m.get("completed_at"))
                                .and_then(serde_json::Value::as_str)
                                .is_some_and(|c| at_or_before(c, cutoff))
                        })
                    })
                })
                .count();
            // `Null`, never removed outright: `flatten_state`'s own
            // `walk` only registers a path that has both `value`
            // and `meta` present at all, and a loop container not
            // yet finished still needs its own `meta.progress` seen.
            obj.insert("value".to_string(), serde_json::Value::Null);
            let meta = obj.get_mut("meta").unwrap().as_object_mut().unwrap();
            meta.remove("completed_at");
            if has_progress {
                meta["progress"]["done"] = serde_json::json!(done);
                meta.remove("completed_passes");
            }
        }
        true
    }

    fn demo_plan() -> oscilloscope_core::plan::PlanTree {
        let doc = fixtures_dir().join("demo.yml");
        let config = fixtures_dir().join("config.json");
        let program =
            oscilloscope_core::plan::compile(&doc, Some(&config), &indexmap::IndexMap::new())
                .unwrap_or_else(|e| panic!("demo.yml doesn't compile: {e}"));
        oscilloscope_core::plan::PlanTree::from_program(&program)
    }

    /// A real, disk-backed [`crate::history_io::HistoryHandle`] over
    /// `design/fixtures/hist/run-h1..3` (issue #678's own "usual"
    /// figures): each recorded with the real recorder (`history::
    /// build_record`, unmodified, the same one `design_fixture_
    /// history_medians_match_the_builders_own_summary` already checks
    /// against `hist/history.json`), then written under `demo.yml`'s
    /// own key so [`crate::history_io::apply_history_eta`] finds them
    /// exactly the way a real run would. The returned [`tempfile::
    /// TempDir`] must outlive the handle.
    fn demo_history() -> (tempfile::TempDir, crate::history_io::HistoryHandle) {
        let plan = demo_plan();
        let dir = tempfile::tempdir().unwrap();
        for run in ["hist/run-h1", "hist/run-h2", "hist/run-h3"] {
            let state = load_json(&fixtures_dir().join(run).join("state.live.json"));
            let mut model = oscilloscope_core::model::RunModel::new();
            model.observe(
                &state,
                &plan,
                oscilloscope_core::model::ProcessState::exited(false),
            );
            let record = oscilloscope_core::history::build_record(
                "demo.yml",
                Some("cof"),
                &state,
                &model,
                &plan,
            )
            .unwrap_or_else(|| panic!("{run}: not a finished, recordable run"));
            oscilloscope_core::history::append_record(dir.path(), "demo.yml", &record).unwrap();
        }
        let handle = crate::history_io::HistoryHandle::for_run(
            Some(dir.path().to_path_buf()),
            "demo.yml".to_string(),
        );
        (dir, handle)
    }

    #[derive(Clone, Copy, PartialEq, Debug)]
    struct MockCellStyle {
        fg: Color,
        bg: Color,
        bold: bool,
        dim: bool,
    }

    struct MockFrame {
        width: u16,
        height: u16,
        cells: Vec<Vec<(char, MockCellStyle)>>,
    }

    fn role_color(role: &str, theme: &Theme) -> Color {
        match role {
            "bg.base" => theme.background.expect("bg.base needs a themed background"),
            "bg.inset" => theme.bg_inset,
            "bg.surface" | "bg.overlay" => theme.bg_surface,
            "fg.default" | "fg.title" => theme.text,
            "fg.muted" => theme.fg_muted,
            "fg.faint" => theme.dim,
            "fg.on-accent" => theme.on_accent,
            "border.default" => theme.border,
            "border.focus" => theme.border_focused,
            "accent.primary" => theme.accent_primary,
            "accent.secondary" => theme.accent_secondary,
            "selection.bg" => theme.selection_bg,
            "selection.fg" => theme.selection_fg,
            "status.success" => theme.status_done,
            "status.warning" => theme.warning,
            "status.error" => theme.status_failed,
            "status.info" => theme.status_info,
            "statusbar.bg" => theme.statusbar_bg,
            "statusbar.fg" => theme.statusbar_fg,
            "keyhint.key" => theme.keyhint_key,
            "keyhint.desc" => theme.keyhint_desc,
            other => panic!("tape_frame_goldens: unknown role {other:?}"),
        }
    }

    /// Parses one `{role [bold|dim] [on:bg-role]}` tag into a style,
    /// against `base` (the pane's own already-painted background --
    /// every span not carrying its own `on:` paints over it, never
    /// a bare `Color::Reset`).
    fn parse_tag(tag: &str, base_bg: Color, theme: &Theme) -> MockCellStyle {
        let mut parts = tag.split(' ');
        let role = parts.next().expect("a span's tag names a role first");
        let mut bold = false;
        let mut dim = false;
        let mut bg = base_bg;
        for part in parts {
            if part == "bold" {
                bold = true;
            } else if part == "dim" {
                dim = true;
            } else if let Some(bg_role) = part.strip_prefix("on:") {
                bg = role_color(bg_role, theme);
            } else if !part.is_empty() {
                panic!("tape_frame_goldens: unknown span attribute {part:?} in {tag:?}");
            }
        }
        MockCellStyle {
            fg: role_color(role, theme),
            bg,
            bold,
            dim,
        }
    }

    /// Parses one already-stripped-of-trailing-newline body line into
    /// its own row of styled cells.
    fn parse_row(line: &str, base_bg: Color, theme: &Theme) -> Vec<(char, MockCellStyle)> {
        let chars: Vec<char> = line.chars().collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < chars.len() {
            assert_eq!(
                chars[i], '{',
                "tape_frame_goldens: expected a span tag at {line:?}[{i}]"
            );
            i += 1;
            let tag_start = i;
            while chars[i] != '}' {
                i += 1;
            }
            let tag: String = chars[tag_start..i].iter().collect();
            i += 1;
            let text_start = i;
            while !(chars[i] == '{'
                && chars.get(i + 1) == Some(&'/')
                && chars.get(i + 2) == Some(&'}'))
            {
                i += 1;
            }
            let text: &[char] = &chars[text_start..i];
            i += 3;
            let style = parse_tag(&tag, base_bg, theme);
            // A literal brace in the body (`{{topic}}`'s own
            // syntax, inside a preview line) is doubled so it can't
            // be mistaken for a tag's own `{`/`{/}`; collapse each
            // doubled pair back to one before painting the cell.
            let mut j = 0;
            while j < text.len() {
                if (text[j] == '{' || text[j] == '}') && text.get(j + 1) == Some(&text[j]) {
                    out.push((text[j], style));
                    j += 2;
                } else {
                    out.push((text[j], style));
                    j += 1;
                }
            }
        }
        out
    }

    /// Parses one `.mock` file (issue #678's own golden format): the
    /// `#!` metadata lines (only `size` is read; the rest -- region
    /// boxes, captions, notes -- document the frame for a human, not
    /// this comparison), then one line of spans per row.
    fn parse_mock(name: &str, theme: &Theme) -> MockFrame {
        let path = frames_dir().join(format!("{name}.mock"));
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("tape_frame_goldens: couldn't read {path:?}: {e}"));
        let mut width = 0u16;
        let mut height = 0u16;
        let mut body_lines: Vec<&str> = Vec::new();
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("#! size: ") {
                let (w, h) = rest.split_once('x').expect("size: WxH");
                width = w.parse().unwrap();
                height = h.parse().unwrap();
            } else if line.starts_with("#!") {
                continue;
            } else {
                body_lines.push(line);
            }
        }
        assert_eq!(
            body_lines.len(),
            height as usize,
            "{name}: expected {height} body lines, got {}",
            body_lines.len()
        );
        let base_bg = theme.background.unwrap_or(Color::Reset);
        let blank = (
            ' ',
            MockCellStyle {
                fg: theme.text,
                bg: base_bg,
                bold: false,
                dim: false,
            },
        );
        let cells = body_lines
            .iter()
            .map(|line| {
                let mut row = if line.is_empty() {
                    Vec::new()
                } else {
                    parse_row(line, base_bg, theme)
                };
                assert!(
                    row.len() <= width as usize,
                    "{name}: row {line:?} is wider than {width} cells"
                );
                // A line shorter than `width` (every blank one, and
                // any with trailing space trimmed by the builder) is
                // the rest of the pane's own base fill, never a
                // separate span of its own.
                row.resize(width as usize, blank);
                row
            })
            .collect();
        MockFrame {
            width,
            height,
            cells,
        }
    }

    /// Compares `buffer` against `mock` cell for cell, panicking with
    /// every mismatch found (not just the first) so a real divergence
    /// is reported all at once.
    /// `allowed_cells`: coordinates this comparison skips outright
    /// (issue #678's own "explicit, documented allow-list", never a
    /// weakened comparison across the whole frame) -- every caller
    /// but one passes `&[]`; see that one's own doc comment for why
    /// its two cells are listed.
    fn assert_matches_mock(
        buffer: &ratatui::buffer::Buffer,
        mock: &MockFrame,
        name: &str,
        allowed_cells: &[(u16, u16)],
    ) {
        assert_eq!(buffer.area.width, mock.width, "{name}: width");
        assert_eq!(buffer.area.height, mock.height, "{name}: height");
        let mut mismatches = Vec::new();
        for y in 0..mock.height {
            for x in 0..mock.width {
                if allowed_cells.contains(&(x, y)) {
                    continue;
                }
                let cell = &buffer[(x, y)];
                let (expected_ch, expected_style) = mock.cells[y as usize][x as usize];
                let actual_bold = cell.modifier.contains(Modifier::BOLD);
                let actual_dim = cell.modifier.contains(Modifier::DIM);
                let actual_ch = cell.symbol().chars().next().unwrap_or(' ');
                if actual_ch != expected_ch
                    || cell.fg != expected_style.fg
                    || cell.bg != expected_style.bg
                    || actual_bold != expected_style.bold
                    || actual_dim != expected_style.dim
                {
                    mismatches.push(format!(
                        "({x},{y}): got {actual_ch:?} fg={:?} bg={:?} bold={actual_bold} dim={actual_dim} -- want {expected_ch:?} fg={:?} bg={:?} bold={} dim={}",
                        cell.fg, cell.bg, expected_style.fg, expected_style.bg, expected_style.bold, expected_style.dim
                    ));
                }
            }
        }
        assert!(
            mismatches.is_empty(),
            "{name}: {} cell mismatch(es):\n{}",
            mismatches.len(),
            mismatches.join("\n")
        );
    }

    #[test]
    fn tape_toosmall_60x16_matches_the_frame() {
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let mock = parse_mock("tape--toosmall--60x16", &theme);
        let zone = frame_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: true,
        };
        let rendered = RenderState {
            header: Header {
                document: "demo.yml".to_string(),
                engine: "cof".to_string(),
                run_id: None,
                status: oscilloscope_core::model::RunStatus::Running,
                elapsed_s: 0.0,
                run_start_ts: None,
                run_end_ts: None,
                effects_done: 0,
                effects_planned: None,
                effects_planned_is_lower_bound: false,
                effects_planned_full: None,
                tokens_sent: 0,
                tokens_received: 0,
                tokens_sent_live_estimate: 0,
                tokens_received_live_estimate: 0,
                innermost_loop_eta_s: None,
                innermost_loop_eta_ts: None,
                history_eta_ts: None,
                history_eta_past_usual: false,
                history_eta_remaining_s: None,
                history_eta_low_ts: None,
                history_eta_high_ts: None,
                history_vs_usual_s: None,
                history_usual_total_s: None,
                history_usual_total_tokens: None,
                history_same_hash_count: None,
                history_step_usual: Default::default(),
                latest_report: None,
                engine_alive: true,
            },
            unfolded_rows: Vec::new(),
            rows: Vec::new(),
            state_rows: Vec::new(),
            token_bar: Default::default(),
            log: Vec::new(),
        };
        let mut app = crate::keys::App::new(false);
        let plan = oscilloscope_core::plan::PlanTree::empty();
        let model = oscilloscope_core::model::RunModel::new();
        let flat = std::collections::BTreeMap::new();
        // The too-small check runs inside `draw_tape` itself, after
        // a real `TapeScreen` already exists -- `main.rs`'s own
        // `build_tape_if_active` builds one whenever the Tape is the
        // screen on display, with no terminal-size check of its own.
        let tape_screen = oscilloscope_core::screens::build_tape(
            &rendered.rows,
            &rendered.header,
            &flat,
            &plan,
            &model,
            None,
            None,
            false,
            "2026-11-08T12:50:47-08:00",
        );
        let backend = TestBackend::new(60, 16);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    None,
                    &[],
                    &mut app,
                    None,
                    &ctx,
                    0,
                    "2026-11-08T12:50:47-08:00",
                    Some(&tape_screen),
                    None,
                    None,
                    None,
                    None,
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        assert_matches_mock(
            terminal.backend().buffer(),
            &mock,
            "tape--toosmall--60x16",
            &[],
        );
    }

    fn load_events(path: &std::path::Path) -> Vec<serde_json::Value> {
        std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("couldn't read {path:?}: {e}"))
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_str(l).expect("valid JSON line"))
            .collect()
    }

    fn observe_all_events(
        model: &mut oscilloscope_core::model::RunModel,
        events: &[serde_json::Value],
    ) {
        for raw in events {
            if let Some(event) = oscilloscope_core::observe::parse_event(raw) {
                model.observe_event(&event);
            }
        }
    }

    #[test]
    fn tape_failed_120x30_matches_the_frame() {
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let mock = parse_mock("tape--failed--120x30", &theme);
        let zone = frame_zone();
        let plan = demo_plan();
        let (_history_dir, history) = demo_history();

        let run_dir = fixtures_dir().join("run");
        let state = load_json(&run_dir.join("state.live.json"));
        let events = load_events(&run_dir.join("events.jsonl"));
        let mut model = oscilloscope_core::model::RunModel::new();
        observe_all_events(&mut model, &events);
        let process = oscilloscope_core::model::ProcessState::exited(false);
        let now = state
            .pointer("/runtime/last_run/completed_at")
            .and_then(serde_json::Value::as_str)
            .unwrap()
            .to_string();
        let mut rendered = oscilloscope_core::render::build_folded(
            "demo.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            process,
            0.0,
            &now,
            &[],
            true,
            &Default::default(),
        );
        crate::history_io::apply_history_eta(
            &history,
            &mut rendered.header,
            Some(&state),
            &plan,
            &now,
        );
        let flat = oscilloscope_core::model::flatten_state(&state);
        let tape_screen = oscilloscope_core::screens::build_tape(
            &rendered.rows,
            &rendered.header,
            &flat,
            &plan,
            &model,
            Some(&state),
            Some("prime.tests"),
            true,
            &now,
        );
        let mut app = crate::keys::App::new(false);
        app.selected_path = Some("prime.tests".to_string());
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: true,
        };
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    None,
                    &[],
                    &mut app,
                    Some(&state),
                    &ctx,
                    0,
                    &now,
                    Some(&tape_screen),
                    None,
                    None,
                    None,
                    None,
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        assert_matches_mock(
            terminal.backend().buffer(),
            &mock,
            "tape--failed--120x30",
            &[],
        );
    }

    /// Builds a real `TapeScreen` (issue #678's own goldens) the same
    /// way `main.rs`'s own redraw loop does: `events` observed first
    /// (empty for a state with no events at all, the Starting
    /// state's own case), then `render::build_folded` and `history_
    /// io::apply_history_eta` against `state`.
    #[allow(clippy::too_many_arguments)]
    fn build_tape_screen(
        plan: &oscilloscope_core::plan::PlanTree,
        history: &crate::history_io::HistoryHandle,
        state: &serde_json::Value,
        events: &[serde_json::Value],
        process: oscilloscope_core::model::ProcessState,
        now: &str,
        engine: &str,
        selected_path: Option<&str>,
        previews_on: bool,
    ) -> (RenderState, oscilloscope_core::screens::TapeScreen) {
        let mut model = oscilloscope_core::model::RunModel::new();
        observe_all_events(&mut model, events);
        let mut rendered = oscilloscope_core::render::build_folded(
            "demo.yml",
            engine,
            plan,
            &mut model,
            Some(state),
            process,
            0.0,
            now,
            &[],
            true,
            &Default::default(),
        );
        crate::history_io::apply_history_eta(history, &mut rendered.header, Some(state), plan, now);
        let flat = oscilloscope_core::model::flatten_state(state);
        let tape_screen = oscilloscope_core::screens::build_tape(
            &rendered.rows,
            &rendered.header,
            &flat,
            plan,
            &model,
            Some(state),
            selected_path,
            previews_on,
            now,
        );
        (rendered, tape_screen)
    }

    /// Draws `tape_screen` at `width`x`height` and compares the
    /// result against `name`'s own `.mock` frame, cell for cell.
    #[allow(clippy::too_many_arguments)]
    fn assert_tape_frame(
        name: &str,
        width: u16,
        height: u16,
        rendered: &RenderState,
        tape_screen: &oscilloscope_core::screens::TapeScreen,
        selected_path: Option<&str>,
        app_setup: impl FnOnce(&mut crate::keys::App),
        now: &str,
        allowed_cells: &[(u16, u16)],
    ) {
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let mock = parse_mock(name, &theme);
        let zone = frame_zone();
        let mut app = crate::keys::App::new(false);
        app.selected_path = selected_path.map(str::to_string);
        app_setup(&mut app);
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: true,
        };
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    rendered,
                    None,
                    &[],
                    &mut app,
                    None,
                    &ctx,
                    0,
                    now,
                    Some(tape_screen),
                    None,
                    None,
                    None,
                    None,
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        assert_matches_mock(terminal.backend().buffer(), &mock, name, allowed_cells);
    }

    #[test]
    fn tape_starting_120x30_matches_the_frame() {
        let plan = demo_plan();
        let (_history_dir, history) = demo_history();
        let state = serde_json::json!({
            "input": {
                "issue": "parse_duration fails on 1h30m with ValueError: invalid duration. 90m works.",
                "topics": ["cats", "dogs", "birds"]
            },
            "runtime": {
                "last_run": {
                    "document_hash": "aafd13c897942266511c4d3a2318e1da69026d296b6b69a22b0ab4459906f4cf"
                },
                "effective_settings": {
                    "model": "scripted-model",
                    "adapter": "scripted"
                }
            }
        });
        let now = "2026-11-08T20:50:47.303Z";
        let (rendered, tape_screen) = build_tape_screen(
            &plan,
            &history,
            &state,
            &[],
            oscilloscope_core::model::ProcessState::Running,
            now,
            "cof 0.1.0",
            None,
            true,
        );
        // Two cells allow-listed (issue #678's own "explicit,
        // documented allow-list, never a weakened comparison"):
        // osi's own history measures a run's wall time from
        // `last_run.started_at` to `completed_at` (`build_record`'s
        // own `duration_seconds`), and `hist/run-h1..3`'s own three
        // runs give 5.056502s, 5.079809s, 5.044445s by that measure
        // -- a median of 5.056502s, which rounds to `5.1s` under
        // this renderer's own round-half-up `format_duration`. The
        // design's own `hist/history.json` instead pools each run's
        // `run_start`→`run_end` *event* timestamps (5.037s, 5.049s,
        // 5.073s), a median of 5.049s, which rounds to `5.0s` --
        // the frame reads `5.0s` in both places this figure appears
        // (the detail pane's own Usual line, the list's own Run End
        // row). The two measures of the same three runs disagree by
        // design, not by a rounding choice this renderer could still
        // make differently; matching the frame here would mean
        // switching osi's own history to the event clock instead of
        // `last_run`'s own fields, which is a data-source decision
        // the design never asked for, not a bug this golden found.
        let allowed_cells = [(97, 5), (51, 16)];
        assert_tape_frame(
            "tape--starting--120x30",
            120,
            30,
            &rendered,
            &tape_screen,
            None,
            |_| {},
            now,
            &allowed_cells,
        );
    }

    #[test]
    fn tape_ended_120x30_matches_the_frame() {
        let plan = demo_plan();
        let (_history_dir, history) = demo_history();
        let run_dir = fixtures_dir().join("run2");
        let state = load_json(&run_dir.join("state.live.json"));
        let events = load_events(&run_dir.join("events.jsonl"));
        let now = state
            .pointer("/runtime/last_run/completed_at")
            .and_then(serde_json::Value::as_str)
            .unwrap()
            .to_string();
        let (rendered, tape_screen) = build_tape_screen(
            &plan,
            &history,
            &state,
            &events,
            oscilloscope_core::model::ProcessState::exited(false),
            &now,
            "cof",
            Some("prime.flaky"),
            true,
        );
        // One cell allow-listed, the same rounding discrepancy as
        // the Starting golden's own: the design's `hist/run-h1..3`
        // give a real median wall time of ~5.0565s, which rounds to
        // `5.1s` under this renderer's own round-half-up
        // `format_duration`; the frame itself reads `5.0s` in the
        // detail pane's own Usual line.
        assert_tape_frame(
            "tape--ended--120x30",
            120,
            30,
            &rendered,
            &tape_screen,
            Some("prime.flaky"),
            |_| {},
            &now,
            &[(103, 4)],
        );
    }

    fn run2_frozen_at_3_62s() -> (serde_json::Value, Vec<serde_json::Value>) {
        let run_dir = fixtures_dir().join("run2");
        let mut state = load_json(&run_dir.join("state.live.json"));
        let now = RUN2_FROZEN_NOW;
        truncate_document(state.get_mut("prime").unwrap(), now);
        // `runtime.last_run.totals`/`completed_at` are themselves
        // only ever written as the run goes/ends; `build_header`'s
        // own token/effects-run figures read `totals` first when
        // present (DESIGN.md's own `_total`-first rule), so a frozen
        // fixture built from the run's own *final* document needs
        // them at the cutoff's own values, not the final ones --
        // `document_hash`/`orchestration_path`/`started_at` stay
        // (a live write has those from the very first one).
        let last_run = state
            .pointer_mut("/runtime/last_run")
            .unwrap()
            .as_object_mut()
            .unwrap();
        last_run.remove("completed_at");
        last_run["totals"] = serde_json::json!({
            "wall_time_s": 3.62,
            "effects_run": 6,
            "tokens_sent": 796,
            "tokens_received": 69,
            "cost_usd": null,
        });
        let events: Vec<serde_json::Value> = load_events(&run_dir.join("events.jsonl"))
            .into_iter()
            .filter(|e| {
                e.get("ts")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|ts| at_or_before(ts, now))
            })
            .collect();
        (state, events)
    }

    const RUN2_FROZEN_NOW: &str = "2026-11-08T20:50:50.923Z";

    #[test]
    fn tape_normal_120x30_matches_the_frame() {
        let plan = demo_plan();
        let (_history_dir, history) = demo_history();
        let now = RUN2_FROZEN_NOW;
        let (state, events) = run2_frozen_at_3_62s();
        let (rendered, tape_screen) = build_tape_screen(
            &plan,
            &history,
            &state,
            &events,
            oscilloscope_core::model::ProcessState::Running,
            now,
            "cof 0.1.0",
            Some("prime.gate.reply"),
            true,
        );
        // One cell allow-listed, the same rounding discrepancy as
        // the Starting/Ended goldens' own: the design's
        // `hist/run-h1..3` give a real median wall time of
        // ~5.0565s, which rounds to `5.1s` under this renderer's own
        // round-half-up `format_duration`; the frame itself reads
        // `5.0s` in the Run End row's own usual figure.
        // A row explicitly selected (issue #678's own scenario)
        // always means a user moved there by hand, not `follow`
        // auto-advancing -- real key handling already turns it off
        // the moment a selection moves (`move_or_scroll`); matters
        // once the list is too short for every row and the Now
        // band both to stay on screen at the same time.
        //
        // The status line's own row (y=28) is allow-listed too: §3.2
        // says the status line shows the newest finished step, and
        // `run2/events.jsonl` finishes `drafts.iter_1.draft` 2 ms
        // after `gate.reply` -- the frame still reads `gate ›
        // reply finished`, the step that finished 2 ms earlier.
        assert_tape_frame(
            "tape--normal--120x30",
            120,
            30,
            &rendered,
            &tape_screen,
            Some("prime.gate.reply"),
            |app| app.follow = false,
            now,
            &STATUS_LINE_READS_AN_EARLIER_STEP_120X30,
        );
    }

    #[test]
    fn tape_usual_120x30_matches_the_frame() {
        let plan = demo_plan();
        let (_history_dir, history) = demo_history();
        let now = RUN2_FROZEN_NOW;
        let (state, events) = run2_frozen_at_3_62s();
        let (rendered, tape_screen) = build_tape_screen(
            &plan,
            &history,
            &state,
            &events,
            oscilloscope_core::model::ProcessState::Running,
            now,
            "cof 0.1.0",
            Some("prime.gate.reply"),
            true,
        );
        // Same cells allow-listed as the Normal golden's own.
        assert_tape_frame(
            "tape--usual--120x30",
            120,
            30,
            &rendered,
            &tape_screen,
            Some("prime.gate.reply"),
            |app| {
                app.follow = false;
                app.tape_tokens_view = true;
            },
            now,
            &STATUS_LINE_READS_AN_EARLIER_STEP_120X30,
        );
    }

    #[test]
    fn tape_normal_80x24_matches_the_frame() {
        let plan = demo_plan();
        let (_history_dir, history) = demo_history();
        let now = RUN2_FROZEN_NOW;
        let (state, events) = run2_frozen_at_3_62s();
        let (rendered, tape_screen) = build_tape_screen(
            &plan,
            &history,
            &state,
            &events,
            oscilloscope_core::model::ProcessState::Running,
            now,
            "cof 0.1.0",
            Some("prime.gate.reply"),
            true,
        );
        // The status-line row (y=22) is allow-listed, same reason
        // as the 120x30 goldens' own.
        assert_tape_frame(
            "tape--normal--80x24",
            80,
            24,
            &rendered,
            &tape_screen,
            Some("prime.gate.reply"),
            |app| app.follow = false,
            now,
            &STATUS_LINE_READS_AN_EARLIER_STEP_80X24,
        );
    }

    #[test]
    fn tape_normal_60x24_matches_the_frame() {
        let plan = demo_plan();
        let (_history_dir, history) = demo_history();
        let now = RUN2_FROZEN_NOW;
        let (state, events) = run2_frozen_at_3_62s();
        let (rendered, tape_screen) = build_tape_screen(
            &plan,
            &history,
            &state,
            &events,
            oscilloscope_core::model::ProcessState::Running,
            now,
            "cof 0.1.0",
            Some("prime.gate.reply"),
            true,
        );
        // The status-line row (y=22) is allow-listed, same reason
        // as the 120x30 goldens' own.
        assert_tape_frame(
            "tape--normal--60x24",
            60,
            24,
            &rendered,
            &tape_screen,
            Some("prime.gate.reply"),
            |app| app.follow = false,
            now,
            &STATUS_LINE_READS_AN_EARLIER_STEP_60X24,
        );
    }

    #[test]
    fn tape_help_120x30_matches_the_frame() {
        let plan = demo_plan();
        let (_history_dir, history) = demo_history();
        let now = RUN2_FROZEN_NOW;
        let (state, events) = run2_frozen_at_3_62s();
        let (rendered, tape_screen) = build_tape_screen(
            &plan,
            &history,
            &state,
            &events,
            oscilloscope_core::model::ProcessState::Running,
            now,
            "cof 0.1.0",
            Some("prime.gate.reply"),
            true,
        );
        assert_tape_frame(
            "tape--help--120x30",
            120,
            30,
            &rendered,
            &tape_screen,
            Some("prime.gate.reply"),
            |app| {
                app.follow = false;
                app.dialog = Dialog::Help;
            },
            now,
            // Two cells allow-listed, documented here, issue #678's
            // own "explicit, documented allow-list, never a
            // weakened comparison":
            // - (119,0) and (119,29): the trailing blank past the
            //   header and key bar's own content -- the `.mock`
            //   parser pads every trimmed line out to the frame's
            //   own width with a bare `fg.default` span of its own
            //   (`parse_mock`'s own padding, not a style the mockup
            //   itself ever wrote), where this renderer's base fill
            //   is `fg.dim`/`fg.faint` instead. Every other cell on
            //   both rows, including their own trailing blanks
            //   elsewhere, matches exactly.
            // - (64,6)-(68,6): the help overlay's own `0 1 2 3`
            //   row names screen `3` `Store` (§2.4); this codebase
            //   still calls it Cost, since an unported screen keeps
            //   its current chip name until its own part lands, so
            //   matching the frame's own text here would mean
            //   renaming it a part early, not fixing a real bug.
            //   `Signal` (part 2) now matches the frame exactly, so
            //   every other cell on this row does too.
            // - the status line's own row (y=28), below the modal:
            //   same reason as the Normal/Usual goldens' own
            //   (`STATUS_LINE_READS_AN_EARLIER_STEP_120X30`'s own
            //   doc comment) -- the Help overlay never covers it.
            &[
                (119, 0),
                (119, 29),
                (64, 6),
                (65, 6),
                (66, 6),
                (67, 6),
                (68, 6),
                (3, 28),
                (4, 28),
                (5, 28),
                (6, 28),
                (7, 28),
                (8, 28),
                (10, 28),
                (11, 28),
                (12, 28),
                (13, 28),
                (14, 28),
                (15, 28),
                (16, 28),
                (17, 28),
                (18, 28),
                (20, 28),
                (21, 28),
                (22, 28),
                (23, 28),
                (24, 28),
                (25, 28),
                (27, 28),
                (28, 28),
                (29, 28),
                (30, 28),
                (31, 28),
                (32, 28),
                (33, 28),
                (34, 28),
                (35, 28),
                (36, 28),
                (38, 28),
                (39, 28),
                (40, 28),
                (41, 28),
                (42, 28),
                (43, 28),
                (44, 28),
                (45, 28),
                (46, 28),
                (47, 28),
            ],
        );
    }

    /// §3.5: below three finished runs of the document, the Run
    /// End row's own ETA reads `—` with a plain `no history yet`,
    /// never blank -- no shipped frame exercises this (every one
    /// has `demo.yml`'s own three `hist/run-h*` runs to go on), so
    /// this builds the same run with a brand new, empty history
    /// directory instead of `demo_history`'s populated one.
    #[test]
    fn run_end_reads_no_history_yet_without_three_finished_runs() {
        let plan = demo_plan();
        let empty_history_dir = tempfile::tempdir().unwrap();
        let history = crate::history_io::HistoryHandle::for_run(
            Some(empty_history_dir.path().to_path_buf()),
            "demo.yml".to_string(),
        );
        let now = RUN2_FROZEN_NOW;
        let (state, events) = run2_frozen_at_3_62s();
        let (_rendered, tape_screen) = build_tape_screen(
            &plan,
            &history,
            &state,
            &events,
            oscilloscope_core::model::ProcessState::Running,
            now,
            "cof 0.1.0",
            Some("prime.gate.reply"),
            true,
        );
        let run_end = tape_screen
            .items
            .iter()
            .find_map(|item| match item {
                oscilloscope_core::screens::TapeItem::RunEnd {
                    usual_total_tokens, ..
                } => Some(*usual_total_tokens),
                _ => None,
            })
            .expect("a Normal-head Tape always carries a Run End item");
        assert_eq!(
            run_end, None,
            "an empty history directory gives no usual total"
        );

        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let zone = frame_zone();
        let mut app = crate::keys::App::new(false);
        app.selected_path = Some("prime.gate.reply".to_string());
        app.follow = false;
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: true,
        };
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_tape(
                    f,
                    f.area(),
                    &tape_screen,
                    &_rendered.header,
                    &mut app,
                    &ctx,
                    0,
                    now,
                )
            })
            .unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(
            text.contains("ETA — · no history yet"),
            "Run End's own row must read the §3.5 fallback text without history"
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn build_signal_screen(
        plan: &oscilloscope_core::plan::PlanTree,
        history: &crate::history_io::HistoryHandle,
        state: &serde_json::Value,
        events: &[serde_json::Value],
        process: oscilloscope_core::model::ProcessState,
        now: &str,
        engine: &str,
        column_ms: u64,
    ) -> (RenderState, oscilloscope_core::screens::SignalScreen) {
        let mut model = oscilloscope_core::model::RunModel::new();
        observe_all_events(&mut model, events);
        let mut rendered = oscilloscope_core::render::build_folded(
            "demo.yml",
            engine,
            plan,
            &mut model,
            Some(state),
            process,
            0.0,
            now,
            &[],
            true,
            &Default::default(),
        );
        crate::history_io::apply_history_eta(history, &mut rendered.header, Some(state), plan, now);
        let flat = oscilloscope_core::model::flatten_state(state);
        let signal_screen = oscilloscope_core::screens::build_signal(
            &rendered.rows,
            &rendered.header,
            &flat,
            plan,
            &model,
            Some(state),
            column_ms,
            None,
            now,
        );
        (rendered, signal_screen)
    }

    /// Signal's own status line (§3.2's "newest finished step"):
    /// `drafts.iter_1.draft` finishes 2 ms after `gate.reply`, but
    /// the frame still reads `gate › reply finished` -- at the
    /// 120x30 tier's own row (y=28).
    const SIGNAL_STATUS_LINE_READS_AN_EARLIER_STEP_120X30: [(u16, u16); 42] = [
        (3, 28),
        (4, 28),
        (5, 28),
        (6, 28),
        (7, 28),
        (8, 28),
        (10, 28),
        (11, 28),
        (12, 28),
        (13, 28),
        (14, 28),
        (15, 28),
        (16, 28),
        (17, 28),
        (18, 28),
        (20, 28),
        (21, 28),
        (22, 28),
        (23, 28),
        (24, 28),
        (25, 28),
        (26, 28),
        (27, 28),
        (28, 28),
        (29, 28),
        (30, 28),
        (31, 28),
        (32, 28),
        (33, 28),
        (34, 28),
        (35, 28),
        (36, 28),
        (38, 28),
        (39, 28),
        (40, 28),
        (41, 28),
        (42, 28),
        (43, 28),
        (44, 28),
        (45, 28),
        (46, 28),
        (47, 28),
    ];

    /// Same reason as
    /// [`SIGNAL_STATUS_LINE_READS_AN_EARLIER_STEP_120X30`], at the
    /// 80x24 tier's own row (y=22).
    const SIGNAL_STATUS_LINE_READS_AN_EARLIER_STEP_80X24: [(u16, u16); 42] = [
        (3, 22),
        (4, 22),
        (5, 22),
        (6, 22),
        (7, 22),
        (8, 22),
        (10, 22),
        (11, 22),
        (12, 22),
        (13, 22),
        (14, 22),
        (15, 22),
        (16, 22),
        (17, 22),
        (18, 22),
        (20, 22),
        (21, 22),
        (22, 22),
        (23, 22),
        (24, 22),
        (25, 22),
        (26, 22),
        (27, 22),
        (28, 22),
        (29, 22),
        (30, 22),
        (31, 22),
        (32, 22),
        (33, 22),
        (34, 22),
        (35, 22),
        (36, 22),
        (38, 22),
        (39, 22),
        (40, 22),
        (41, 22),
        (42, 22),
        (43, 22),
        (44, 22),
        (45, 22),
        (46, 22),
        (47, 22),
    ];

    #[allow(clippy::too_many_arguments)]
    fn assert_signal_frame(
        name: &str,
        width: u16,
        height: u16,
        rendered: &RenderState,
        signal_screen: &oscilloscope_core::screens::SignalScreen,
        now: &str,
        allowed_cells: &[(u16, u16)],
    ) {
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let mock = parse_mock(name, &theme);
        let zone = frame_zone();
        let mut app = crate::keys::App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Signal;
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: true,
        };
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    rendered,
                    None,
                    &[],
                    &mut app,
                    None,
                    &ctx,
                    0,
                    now,
                    None,
                    None,
                    None,
                    Some(signal_screen),
                    None,
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        assert_matches_mock(terminal.backend().buffer(), &mock, name, allowed_cells);
    }

    #[test]
    fn signal_normal_120x30_matches_the_frame() {
        let plan = demo_plan();
        let (_history_dir, history) = demo_history();
        let now = RUN2_FROZEN_NOW;
        let (state, events) = run2_frozen_at_3_62s();
        let (rendered, signal_screen) = build_signal_screen(
            &plan,
            &history,
            &state,
            &events,
            oscilloscope_core::model::ProcessState::Running,
            now,
            "cof 0.1.0",
            100,
        );
        assert_signal_frame(
            "signal--normal--120x30",
            120,
            30,
            &rendered,
            &signal_screen,
            now,
            &SIGNAL_STATUS_LINE_READS_AN_EARLIER_STEP_120X30,
        );
    }

    #[test]
    fn signal_normal_80x24_matches_the_frame() {
        let plan = demo_plan();
        let (_history_dir, history) = demo_history();
        let now = RUN2_FROZEN_NOW;
        let (state, events) = run2_frozen_at_3_62s();
        let (rendered, signal_screen) = build_signal_screen(
            &plan,
            &history,
            &state,
            &events,
            oscilloscope_core::model::ProcessState::Running,
            now,
            "cof 0.1.0",
            100,
        );
        assert_signal_frame(
            "signal--normal--80x24",
            80,
            24,
            &rendered,
            &signal_screen,
            now,
            &SIGNAL_STATUS_LINE_READS_AN_EARLIER_STEP_80X24,
        );
    }

    /// `main.rs`'s own `push_story_line` filter, reproduced here
    /// rather than imported: it's a thin wrapper around a bounded
    /// `Vec` that lives in the binary crate, not `oscilloscope-core`.
    fn is_story_kind(kind: oscilloscope_core::diff::LogKind) -> bool {
        use oscilloscope_core::diff::LogKind;
        matches!(
            kind,
            LogKind::RunStart
                | LogKind::RunEnd
                | LogKind::Decision
                | LogKind::Report
                | LogKind::Failure
        )
    }

    /// The run's own story lines (DESIGN.md §2.3's Story pane), from
    /// `events` (for `■ run started` and every event-sourced line)
    /// and the final `state` (for the state-sourced ones `diff`/
    /// `finish` still produce).
    fn build_story_lines(
        events: &[serde_json::Value],
        state: &serde_json::Value,
        plan: &oscilloscope_core::plan::PlanTree,
        model: &mut oscilloscope_core::model::RunModel,
        run_ended: bool,
    ) -> Vec<oscilloscope_core::diff::LogLine> {
        let mut differ = oscilloscope_core::diff::Differ::new();
        let mut story = Vec::new();
        for raw in events {
            let Some(event) = oscilloscope_core::observe::parse_event(raw) else {
                continue;
            };
            model.observe_event(&event);
            for line in differ.diff_event(&event, plan, model) {
                if is_story_kind(line.kind) {
                    story.push(line);
                }
            }
        }
        for line in differ.diff(state, plan) {
            if is_story_kind(line.kind) {
                story.push(line);
            }
        }
        if run_ended {
            if let Some(line) = differ.finish(state, None) {
                if is_story_kind(line.kind) {
                    story.push(line);
                }
            }
        }
        story
    }

    #[allow(clippy::too_many_arguments)]
    fn build_flow_screen(
        plan: &oscilloscope_core::plan::PlanTree,
        history: &crate::history_io::HistoryHandle,
        state: &serde_json::Value,
        events: &[serde_json::Value],
        process: oscilloscope_core::model::ProcessState,
        now: &str,
        engine: &str,
    ) -> (RenderState, oscilloscope_core::screens::FlowScreen) {
        let mut model = oscilloscope_core::model::RunModel::new();
        let story = build_story_lines(events, state, plan, &mut model, false);
        let mut rendered = oscilloscope_core::render::build_folded(
            "demo.yml",
            engine,
            plan,
            &mut model,
            Some(state),
            process,
            0.0,
            now,
            &[],
            true,
            &Default::default(),
        );
        crate::history_io::apply_history_eta(history, &mut rendered.header, Some(state), plan, now);
        let flat = oscilloscope_core::model::flatten_state(state);
        let flow_screen = oscilloscope_core::screens::build_flow(
            &rendered.unfolded_rows,
            &rendered.rows,
            &flat,
            plan,
            &model,
            &rendered.header,
            Some(state),
            &story,
            None,
            &frame_zone(),
            rendered.header.run_end_ts.as_deref(),
            now,
        );
        (rendered, flow_screen)
    }

    #[allow(clippy::too_many_arguments)]
    fn assert_flow_frame(
        name: &str,
        width: u16,
        height: u16,
        rendered: &RenderState,
        flow_screen: &oscilloscope_core::screens::FlowScreen,
        now: &str,
        app: &mut App,
        allowed_cells: &[(u16, u16)],
    ) {
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let mock = parse_mock(name, &theme);
        let zone = frame_zone();
        app.screen = oscilloscope_core::screens::Screen::Flow;
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: true,
        };
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    rendered,
                    None,
                    &[],
                    app,
                    None,
                    &ctx,
                    0,
                    now,
                    None,
                    None,
                    Some(flow_screen),
                    None,
                    None,
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        assert_matches_mock(terminal.backend().buffer(), &mock, name, allowed_cells);
    }

    #[test]
    fn flow_normal_120x30_matches_the_frame() {
        let plan = demo_plan();
        let (_history_dir, history) = demo_history();
        let now = RUN2_FROZEN_NOW;
        let (state, events) = run2_frozen_at_3_62s();
        let (rendered, flow_screen) = build_flow_screen(
            &plan,
            &history,
            &state,
            &events,
            oscilloscope_core::model::ProcessState::Running,
            now,
            "cof 0.1.0",
        );
        let mut app = App::new(false);
        assert_flow_frame(
            "flow--normal--120x30",
            120,
            30,
            &rendered,
            &flow_screen,
            now,
            &mut app,
            &SIGNAL_STATUS_LINE_READS_AN_EARLIER_STEP_120X30,
        );
    }

    #[test]
    fn flow_folded_120x30_matches_the_frame() {
        let plan = demo_plan();
        let (_history_dir, history) = demo_history();
        let now = RUN2_FROZEN_NOW;
        let (state, events) = run2_frozen_at_3_62s();
        let (rendered, flow_screen) = build_flow_screen(
            &plan,
            &history,
            &state,
            &events,
            oscilloscope_core::model::ProcessState::Running,
            now,
            "cof 0.1.0",
        );
        let mut app = App::new(false);
        app.flow_lanes_open.insert("prime.drafts".to_string(), 0);
        assert_flow_frame(
            "flow--folded--120x30",
            120,
            30,
            &rendered,
            &flow_screen,
            now,
            &mut app,
            &SIGNAL_STATUS_LINE_READS_AN_EARLIER_STEP_120X30,
        );
    }

    #[test]
    fn flow_normal_80x24_matches_the_frame() {
        let plan = demo_plan();
        let (_history_dir, history) = demo_history();
        let now = RUN2_FROZEN_NOW;
        let (state, events) = run2_frozen_at_3_62s();
        let (rendered, flow_screen) = build_flow_screen(
            &plan,
            &history,
            &state,
            &events,
            oscilloscope_core::model::ProcessState::Running,
            now,
            "cof 0.1.0",
        );
        let mut app = App::new(false);
        assert_flow_frame(
            "flow--normal--80x24",
            80,
            24,
            &rendered,
            &flow_screen,
            now,
            &mut app,
            &SIGNAL_STATUS_LINE_READS_AN_EARLIER_STEP_80X24,
        );
    }

    fn empty_header() -> oscilloscope_core::render::Header {
        let plan = oscilloscope_core::plan::PlanTree::empty();
        let mut model = oscilloscope_core::model::RunModel::new();
        oscilloscope_core::render::build_folded(
            "demo.yml",
            "cof",
            &plan,
            &mut model,
            None,
            oscilloscope_core::model::ProcessState::Running,
            0.0,
            "2026-01-01T00:00:00Z",
            &[],
            true,
            &std::collections::BTreeSet::new(),
        )
        .header
    }

    fn draw_chart_only(
        screen: &oscilloscope_core::screens::SignalScreen,
        width: u16,
        height: u16,
        wide: bool,
    ) -> ratatui::buffer::Buffer {
        let header = empty_header();
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &frame_zone(),
            no_animation: true,
        };
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                draw_signal_chart(f, area, screen, &header, wide, &ctx);
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    /// A running prompt's own live estimate, landing in the Now
    /// column exactly as the model already marks it, draws its own
    /// bar in the faint live style -- never the sweep's own bold
    /// accent, which would make `≈` indistinguishable from an
    /// ordinary finished bar.
    #[test]
    fn the_live_column_draws_its_bar_faint_not_in_the_sweeps_own_bold_style() {
        let mut screen = oscilloscope_core::screens::SignalScreen {
            sent_axis_top: 600,
            received_axis_top: 80,
            now_col: Some(0),
            cursor_col: Some(0),
            ..Default::default()
        };
        screen
            .buckets
            .push(oscilloscope_core::screens::SignalBucket {
                sent: 300,
                sent_live: true,
                ..Default::default()
            });
        let buffer = draw_chart_only(&screen, 120, 12, true);
        // The block border takes the first row and column; the
        // gutter (`600 t` etc., width 6) the next six, then the
        // bottom-most sent row (index 4) is five rows down.
        let y = buffer.area.y + 1 + (SIGNAL_SENT_ROWS as u16 - 1);
        let x = buffer.area.x + 1 + 6;
        let cell = &buffer[(x, y)];
        assert_ne!(
            cell.symbol(),
            "\u{2503}",
            "a filled live column keeps its own bar glyph"
        );
        assert!(
            cell.modifier.contains(Modifier::DIM),
            "a live column's own bar is faint"
        );
        assert!(
            !cell.modifier.contains(Modifier::BOLD),
            "never the sweep's own bold style"
        );
    }

    /// A run many times longer than the pane is wide still keeps Now
    /// on screen -- the trace scrolls rather than losing the sweep
    /// (and everything after it) off the right edge.
    #[test]
    fn the_trace_scrolls_to_keep_now_in_view_on_a_long_run() {
        let screen = oscilloscope_core::screens::SignalScreen {
            sent_axis_top: 600,
            received_axis_top: 80,
            now_col: Some(300), // 30s at the default 100ms/column
            cursor_col: Some(300),
            buckets: vec![oscilloscope_core::screens::SignalBucket::default(); 301],
            ..Default::default()
        };
        let buffer = draw_chart_only(&screen, 120, 12, true);
        let axis_row_y = buffer.area.y + SIGNAL_SENT_ROWS as u16;
        let has_sweep =
            (0..buffer.area.width).any(|x| buffer[(x, axis_row_y)].symbol() == "\u{2503}");
        assert!(
            has_sweep,
            "Now's own sweep must still be drawn somewhere on screen"
        );
    }

    /// The scrub cursor, once it differs from Now, is drawn with its
    /// own distinct tint -- pressing ←→ must change something visible,
    /// not just which step `enter` would open.
    #[test]
    fn a_scrubbed_cursor_is_drawn_distinctly_from_the_now_sweep() {
        let screen = oscilloscope_core::screens::SignalScreen {
            sent_axis_top: 600,
            received_axis_top: 80,
            now_col: Some(10),
            cursor_col: Some(5),
            buckets: vec![oscilloscope_core::screens::SignalBucket::default(); 11],
            ..Default::default()
        };
        let buffer = draw_chart_only(&screen, 120, 12, true);
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let y = buffer.area.y + 1;
        let cursor_x = buffer.area.x + 1 + 6 + 5;
        let elsewhere_x = buffer.area.x + 1 + 6 + 2;
        assert_eq!(
            buffer[(cursor_x, y)].bg,
            theme.selection_bg,
            "the cursor's own column is tinted"
        );
        assert_ne!(
            buffer[(elsewhere_x, y)].bg,
            theme.selection_bg,
            "only the cursor's own column is"
        );
    }

    /// An axis top of 1,200 or more still fits its own label field,
    /// with the thousands separator §3.8 asks for -- not cut down to
    /// `1200` the way a fixed five-character field would.
    #[test]
    fn an_axis_top_in_the_thousands_is_never_cut() {
        let screen = oscilloscope_core::screens::SignalScreen {
            sent_axis_top: 1200,
            received_axis_top: 80,
            now_col: Some(0),
            cursor_col: Some(0),
            buckets: vec![oscilloscope_core::screens::SignalBucket::default()],
            ..Default::default()
        };
        let buffer = draw_chart_only(&screen, 120, 12, true);
        let row: String = (0..7)
            .map(|i| {
                buffer[(buffer.area.x + 1 + i, buffer.area.y + 1)]
                    .symbol()
                    .to_string()
            })
            .collect();
        assert_eq!(row, "1,200 t");
    }

    /// At a coarse zoom (1 s per column here), second marks keep at
    /// least six columns between labels rather than printing one
    /// every column and overlapping -- `0s` and `6s`, not `0s6s`.
    #[test]
    fn seconds_marks_stay_spaced_out_at_a_coarse_zoom() {
        let screen = oscilloscope_core::screens::SignalScreen {
            sent_axis_top: 600,
            received_axis_top: 80,
            column_ms: 1000,
            ..Default::default()
        };
        let buffer = draw_chart_only(&screen, 120, 16, true);
        let y = buffer.area.y + 1 + SIGNAL_SENT_ROWS as u16 + 1 + SIGNAL_RECEIVED_ROWS as u16;
        let x0 = buffer.area.x + 1 + 6;
        let row: Vec<String> = (0..16)
            .map(|i| buffer[(x0 + i, y)].symbol().to_string())
            .collect();
        assert_eq!(row[0], "0", "{row:?}");
        assert_eq!(row[1], "s", "{row:?}");
        assert_eq!(row[6], "6", "{row:?}");
        assert_eq!(row[7], "s", "{row:?}");
        assert!(
            row[2..6].iter().all(|c| c == " "),
            "no overlap between labels: {row:?}"
        );
    }

    /// The Help overlay on Signal lists its own keys (§4), not an
    /// older generic list that has none of them.
    #[test]
    fn the_signal_help_overlay_lists_signals_own_keys() {
        let plan = demo_plan();
        let (_history_dir, history) = demo_history();
        let now = RUN2_FROZEN_NOW;
        let (state, events) = run2_frozen_at_3_62s();
        let (rendered, signal_screen) = build_signal_screen(
            &plan,
            &history,
            &state,
            &events,
            oscilloscope_core::model::ProcessState::Running,
            now,
            "cof 0.1.0",
            100,
        );
        let mut app = crate::keys::App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Signal;
        app.dialog = Dialog::Help;
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let zone = frame_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: true,
        };
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    None,
                    &[],
                    &mut app,
                    None,
                    &ctx,
                    0,
                    now,
                    None,
                    None,
                    None,
                    Some(&signal_screen),
                    None,
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        let text = terminal.backend().to_string();
        assert!(text.contains("Help") && text.contains("Signal"), "{text}");
        assert!(text.contains("Scrub"), "{text}");
        assert!(text.contains("Zoom"), "{text}");
        assert!(text.contains("Now"), "{text}");
        assert!(
            !text.contains("fold on/off"),
            "not the generic list:\n{text}"
        );
        assert!(
            !text.contains("errors only"),
            "not the generic list:\n{text}"
        );
    }

    #[test]
    fn the_flow_help_overlay_lists_flows_own_keys() {
        let plan = demo_plan();
        let (_history_dir, history) = demo_history();
        let now = RUN2_FROZEN_NOW;
        let (state, events) = run2_frozen_at_3_62s();
        let (rendered, flow_screen) = build_flow_screen(
            &plan,
            &history,
            &state,
            &events,
            oscilloscope_core::model::ProcessState::Running,
            now,
            "cof 0.1.0",
        );
        let mut app = crate::keys::App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Flow;
        app.dialog = Dialog::Help;
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let zone = frame_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: true,
        };
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal
            .draw(|f| {
                draw(
                    f,
                    &rendered,
                    None,
                    &[],
                    &mut app,
                    None,
                    &ctx,
                    0,
                    now,
                    None,
                    None,
                    Some(&flow_screen),
                    None,
                    None,
                    &mut CostLayoutCache::default(),
                )
            })
            .unwrap();
        let text = terminal.backend().to_string();
        assert!(text.contains("Help") && text.contains("Flow"), "{text}");
        assert!(text.contains("scroll the rail"), "{text}");
        assert!(text.contains("select a pass"), "{text}");
        assert!(text.contains("open / fold loop lanes"), "{text}");
        assert!(
            !text.contains("fold on/off"),
            "not the generic list:\n{text}"
        );
        assert!(
            !text.contains("errors only"),
            "not the generic list:\n{text}"
        );
        assert!(
            !text.contains("collapse / expand a row"),
            "not the old tree-era list:\n{text}"
        );
    }
}
