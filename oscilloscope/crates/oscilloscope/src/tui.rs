//! Lays out one `RenderState` (DESIGN.md §6.3): the header, the plan
//! tree and details panes, the log pane, and the key's own overlays
//! (help, both confirms, the full-value view, the filter prompt).
//! The only module in this crate that imports `ratatui` widgets
//! directly, mirroring `oscilloscope-core`'s own "no terminal code"
//! split one level up.

use oscilloscope_core::model::{RunStatus, StatusKind};
use oscilloscope_core::render::Details;
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
    // Scoped to the Tape itself: Flow/Signal/Store's own existing
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
                        "run {word} · {} · {} ",
                        oscilloscope_core::format::format_tokens(total),
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
                oscilloscope_core::format::format_tokens_arrow(row.tokens.sent.unwrap_or(0)),
                oscilloscope_core::format::format_tokens_arrow(row.tokens.received.unwrap_or(0))
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

/// Issue #704 rule 5: the full-screen step key bar, shared by every
/// screen -- the only keys that actually work while it is open (the
/// key handler checked first intercepts everything else).
fn fullscreen_step_key_bar_items(wide: bool) -> Vec<(&'static str, &'static str)> {
    let mut items = vec![
        ("[ ]", "Step"),
        ("↑↓", "Scroll"),
        // Issue #704 rule 4: `enter` on a pass opens it, in a loop's
        // own Loop section -- a no-op, falling through to `esc`'s
        // own close, everywhere else.
        ("enter", "Open pass"),
        ("m", "Meta"),
    ];
    if wide {
        items.push(("0-3", "Screens"));
    }
    items.push(("?", "Help"));
    items.push(("esc", "Close"));
    items.push(("q", "Quit"));
    items
}

/// Issue #704 rule 5: the full-screen step's own Help, the same
/// fixed overlay every screen's own uses, listing exactly its own
/// keys (never whichever screen opened it, whose own keys don't
/// work while it's open).
fn fullscreen_help_lines(ctx: &DrawCtx) -> Vec<Line<'static>> {
    const ROWS: &[(&str, &str, &str, &str)] = &[
        ("[ ]", "previous / next effect", "↑ ↓", "scroll"),
        (
            "PgUp PgDn",
            "scroll a page",
            "Home End",
            "scroll to the ends",
        ),
        ("enter", "open a pass (Loop section)", "m", "unfold Meta"),
        ("0 1 2 3", "Tape Signal Flow Store", "?", "this help"),
        ("esc", "close · back", "q", "quit · confirms if running"),
    ];
    let mut lines: Vec<Line<'static>> = ROWS
        .iter()
        .map(|(k1, d1, k2, d2)| tape_help_row(k1, d1, k2, d2, ctx))
        .collect();
    // Issue #714: `T` cycles the theme here too.
    lines.push(tape_help_row("T", "colour theme", "", "", ctx));
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

/// §3.5's token bar: `▰▰▰▰▱▱▱▱   215 t`, eight cells at `unit`
/// tokens each -- fewer once the count itself needs more than seven
/// of the column's 16 cells.
fn tape_token_bar(
    total: u64,
    unit: u64,
    bold: bool,
    live: bool,
    ctx: &DrawCtx,
) -> Vec<Span<'static>> {
    let count_text = oscilloscope_core::format::format_tokens(total);
    let count_len = count_text.chars().count();
    // The gap before the count shrinks as the count itself widens (the
    // same column-sizing `tape_token_column` already does for a loop's
    // own sparkline+`t/p` suffix) -- the live estimate's own leading
    // `≈` would otherwise push a count of 1,000 tokens or more past the
    // 16-cell column and get cut by the pane's own clipping, silently
    // turning it into a smaller, wrong number.
    // Issue #702: an agent's own total routinely reaches six digits
    // or more (cached input tokens push it well past a million), so
    // the count alone can outgrow the 8 cells the bar always had
    // left for it, 1 gap space included -- once it does, the bar
    // gives up cells of its own as the count widens, dropped
    // entirely once even one bar cell would no longer leave room for
    // both it and the required gap, rather than let the row's own
    // clipping cut the count's own trailing `t`, or worse, a
    // trailing digit. The `+1` in `15 - count_len` is exactly that
    // one required gap cell; `.min(8)` is the ordinary, small-count
    // case's own full bar.
    let bar_cells = 15usize.saturating_sub(count_len).min(8);
    // Rounded, not floored: 389 at a 50-token unit reads a full
    // bar (▰×8), not 7 filled and one empty.
    let filled = ((total as f64 / unit.max(1) as f64).round() as usize).min(bar_cells);
    let empty = bar_cells - filled;
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
    // The design always prints the count in full, with a space
    // before it (§3.5, §3.8) -- `bar_cells` above already leaves
    // room for exactly one, so this is always `>= 1` short of an
    // unrealistically wide count that would overrun the column even
    // with the bar dropped to nothing.
    let gap = 16usize.saturating_sub(bar_cells).saturating_sub(count_len);
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
    /// the column and get cut by the pane (issue #675). An agent
    /// session's own total reaches six or seven digits once cached
    /// input tokens are counted (issue #702): the bar itself must
    /// give up cells rather than let the count's own trailing `t`,
    /// or a trailing digit, be cut instead.
    #[test]
    fn a_live_estimate_in_the_thousands_still_fits_the_column() {
        let theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        for total in [41u64, 391, 1902, 9999, 11_680, 123_456, 1_234_567] {
            let spans = tape_token_bar(total, 50, false, true, &ctx);
            let width = printed_width(&spans);
            assert!(
                width <= 16,
                "a live total of {total} printed {width} cells, over the 16-cell column"
            );
            let full: String = spans.iter().map(|s| s.content.as_ref()).collect();
            let expected_count = oscilloscope_core::format::format_tokens(total);
            assert!(
                full.ends_with(&expected_count),
                "the count itself must never be cut: {full:?}"
            );
            let before_count = &full[..full.len() - expected_count.len()];
            assert!(
                before_count.ends_with(' '),
                "the design always prints a space before the count: {full:?}"
            );
        }
    }

    /// Issue #702: Claude Code never reports `tokens_received` live
    /// (`docs/plugins/agent.md`) -- the Tape detail pane's own
    /// Tokens line must draw `—` for it, never a false `0`, while
    /// the agent is still running.
    #[test]
    fn a_running_claude_code_agents_null_received_draws_as_a_dash() {
        let plan = oscilloscope_core::plan::PlanTree::empty();
        let mut model = oscilloscope_core::model::RunModel::new();
        let state = serde_json::json!({"prime": {"value": null, "meta": {"completed_at": null},
            "fixer": {"value": null, "meta": {"created_at": "2026-10-08T11:46:43Z",
                "completed_at": null, "provider": "agent",
                "live": {"engine": "claude_code", "updated_at": "2026-10-08T11:46:50Z",
                    "tokens_sent": 1600, "tokens_received": null}}}
        }});
        let rendered = oscilloscope_core::render::build(
            "do.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            oscilloscope_core::model::ProcessState::Running,
            1.0,
            "2026-10-08T11:46:51Z",
            &[],
        );
        let flat = oscilloscope_core::model::flatten_state(&state);
        let tape = oscilloscope_core::screens::build_tape(
            &rendered.rows,
            &rendered.unfolded_rows,
            &rendered.header,
            &flat,
            &plan,
            &model,
            Some(&state),
            Some("prime.fixer"),
            true,
            "2026-10-08T11:46:51Z",
        );
        let theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &test_zone(),
            no_animation: false,
        };
        let lines =
            tape_normal_detail_lines(&tape, 60, 40, None, "2026-10-08T11:46:51Z", false, &ctx);
        let text: String = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("↑1.6k"), "{text}");
        assert!(
            text.contains("↓—"),
            "a never-reported received figure must draw as a dash: {text}"
        );
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
        let suffix = format!("{}/p", oscilloscope_core::format::format_tokens(avg));
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
                    "usually {}",
                    oscilloscope_core::format::format_tokens(usual)
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
                if row.what_is_agent_text {
                    tape_agent_what_text(&tape_loop_what(row, wide_detail), what_width)
                } else {
                    tape_what_text(&tape_loop_what(row, wide_detail), what_width)
                }
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
    if what.chars().count() <= budget {
        what.to_string()
    } else if budget == 0 {
        String::new()
    } else {
        let truncated: String = what.chars().take(budget.saturating_sub(1)).collect();
        format!("{truncated}⋯")
    }
}

/// The running/finished agent row's own cut (issue #703's rules 1
/// and 2: "cut from the right in that order"): drops whole trailing
/// " · "-separated pieces first, never half of one (`· ct⋯`) -- each
/// piece is a discrete figure (a turn, a tool, a token count), never
/// free text worth trimming by character the way [`tape_what_text`]
/// trims a prompt row's own reply preview. Only once even the first
/// piece alone still doesn't fit does this fall back to cutting that
/// one piece by character, the same as [`tape_what_text`] would.
fn tape_agent_what_text(what: &str, what_width: usize) -> String {
    let budget = what_width.saturating_sub(1);
    if what.chars().count() < budget {
        return what.to_string();
    }
    if budget == 0 {
        return String::new();
    }
    let segments: Vec<&str> = what.split(" · ").collect();
    for end in (1..=segments.len()).rev() {
        let cut = format!("{}⋯", segments[..end].join(" · "));
        if cut.chars().count() <= budget {
            return cut;
        }
    }
    let truncated: String = segments[0].chars().take(budget.saturating_sub(1)).collect();
    format!("{truncated}⋯")
}

/// A list line's own role in issue #710's window rule: every line
/// is `Normal` except a prompt's own preview line, which never
/// draws on its own once the window's own top lands exactly on it
/// with its row scrolled out above -- `tape_list_window_start`'s own
/// caller reads this back after slicing the window, to blank that
/// one line rather than let it float with nothing above it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TapeListLineKind {
    Normal,
    Preview,
}

/// Issue #710's window rule, kept pure of everything the list itself
/// draws: which lines are a preview continuation (`kinds`), where the
/// Now area sits (`now_area`, `None` outside `TapeHead::Starting`/
/// `Normal` -- Ended/Failed keep the list's own original tail-biased
/// rule, scrolling up only as far as it must to keep the anchor in
/// view), how tall the list itself is (`content_rows`), whether the
/// Tape is following Now, the current selection, and the window's
/// own previous top (threaded through `App::tape_list_start` across
/// draws, since a manual scroll with follow off moves no further
/// than the selection needs).
///
/// The result is a signed top into `kinds`: a negative value, or one
/// that leaves `kinds.len()` short of `start + content_rows`, is
/// exactly the window's own blank padding above or below the real
/// lines -- the caller clamps it to `0..kinds.len()` to find the
/// real lines to draw, and draws blank for whatever of the window's
/// own `content_rows` that clamp leaves uncovered.
fn tape_list_window_start(
    kinds: &[TapeListLineKind],
    now_area: Option<std::ops::Range<usize>>,
    content_rows: usize,
    follow: bool,
    selected: Option<usize>,
    previous_start: Option<isize>,
) -> isize {
    let total = kinds.len();
    let content_rows_i = content_rows as isize;
    let Some(now_area) = now_area else {
        // Ended/Failed: the list's own original rule, unchanged --
        // biased toward the tail, scrolling up only as far as it
        // must so the anchor (the selection, or the last line while
        // following) never scrolls out of view above the top.
        let anchor = if follow {
            total.saturating_sub(1)
        } else {
            selected.unwrap_or(0)
        };
        return if total <= content_rows {
            0
        } else {
            anchor.min(total - content_rows) as isize
        };
    };
    let h = (now_area.end - now_area.start) as isize;
    if follow || previous_start.is_none() {
        if h >= content_rows_i {
            // Taller than the list itself (many running rows) -- shown
            // from its own first row, nothing above it at all.
            return now_area.start as isize;
        }
        // Centred: the Now area's own middle row sits on the list's
        // middle row, rounding down when the split is uneven. This
        // same arithmetic is also where a window with follow off
        // starts from the very first time it has nothing of its own
        // to resume from yet (`previous_start` is `None` only before
        // the Tape has ever drawn its list).
        let above = (content_rows_i - h) / 2;
        now_area.start as isize - above
    } else {
        // Follow off: keep the selected row in view, scrolling no
        // further than the previous window already needs to, even
        // when the Now area is itself taller than the list.
        let mut start = previous_start.unwrap_or(0);
        if let Some(selected) = selected {
            let selected = selected as isize;
            if selected < start {
                start = selected;
            } else if selected >= start + content_rows_i {
                start = selected - content_rows_i + 1;
            }
        }
        start
    }
}

/// `tape_list_window_start`'s own output, resolved against the real
/// content: the real line indices to draw (clamped to `0..kinds.
/// len()`) and whether the first of them is a preview line whose own
/// row scrolled off above the window, which must draw blank instead
/// of floating on its own (issue #710) -- the only place this can
/// happen is right at the window's own top edge, since a row and its
/// own preview are always adjacent.
fn tape_list_window_range(
    kinds: &[TapeListLineKind],
    start: isize,
    content_rows: usize,
) -> (std::ops::Range<usize>, bool) {
    let total = kinds.len() as isize;
    let content_rows_i = content_rows as isize;
    let real_start = start.max(0) as usize;
    let real_end = (start + content_rows_i).clamp(0, total).max(start.max(0)) as usize;
    let blank_first = start >= 0 && kinds.get(real_start) == Some(&TapeListLineKind::Preview);
    (real_start..real_end, blank_first)
}

#[cfg(test)]
mod tape_list_window_tests {
    use super::*;

    /// `n` plain rows, the kind every ordinary row and blank padding
    /// slot has.
    fn normal(n: usize) -> Vec<TapeListLineKind> {
        vec![TapeListLineKind::Normal; n]
    }

    #[test]
    fn centres_the_now_area_with_a_long_past_and_a_long_plan() {
        // 8 past + 4-row band + 8 ahead = 20 lines, an 11-row list:
        // both sides have more than enough, so the window just picks
        // the arithmetic middle with no padding either side.
        let kinds = normal(20);
        let start = tape_list_window_start(&kinds, Some(8..12), 11, true, None, None);
        // (11 - 4) / 2 = 3 rows above the band.
        assert_eq!(start, 5);
        let (range, blank_first) = tape_list_window_range(&kinds, start, 11);
        assert_eq!(range, 5..16);
        assert!(!blank_first);
    }

    #[test]
    fn pads_above_when_the_past_is_short() {
        // No past at all (the Starting rule is the list's own first
        // line) -- the desired 5 rows above the band exist nowhere,
        // so the window starts negative (blank padding) instead of
        // clamping to the real content's own start.
        let kinds = normal(9);
        let start = tape_list_window_start(&kinds, Some(0..1), 11, true, None, None);
        assert_eq!(start, -5);
        let (range, blank_first) = tape_list_window_range(&kinds, start, 11);
        assert_eq!(range, 0..6);
        assert!(!blank_first);
    }

    #[test]
    fn pads_below_when_the_plan_is_short() {
        // A band near the very end: plenty of past, nothing ahead.
        let kinds = normal(10);
        let start = tape_list_window_start(&kinds, Some(6..10), 11, true, None, None);
        // (11 - 4) / 2 = 3 rows above; the window runs to 3 + 11 = 14,
        // four past the real content's own end (10).
        assert_eq!(start, 3);
        let (range, _) = tape_list_window_range(&kinds, start, 11);
        assert_eq!(range, 3..10);
    }

    #[test]
    fn a_now_area_taller_than_the_list_shows_from_its_own_first_row() {
        let kinds = normal(20);
        let start = tape_list_window_start(&kinds, Some(5..17), 10, true, None, None);
        assert_eq!(start, 5);
        let (range, _) = tape_list_window_range(&kinds, start, 10);
        assert_eq!(range, 5..15);
    }

    #[test]
    fn an_orphaned_preview_line_draws_blank_not_floating() {
        // The window's own arithmetic start (8 - 3 = 5) lands
        // exactly on a preview line (index 5) with its own row
        // (index 4) scrolled off above -- it must draw blank, not
        // show the preview on its own.
        let mut kinds = normal(20);
        kinds[4] = TapeListLineKind::Normal;
        kinds[5] = TapeListLineKind::Preview;
        let start = tape_list_window_start(&kinds, Some(8..12), 11, true, None, None);
        assert_eq!(start, 5);
        let (range, blank_first) = tape_list_window_range(&kinds, start, 11);
        assert_eq!(range, 5..16);
        assert!(blank_first);
    }

    #[test]
    fn follow_off_scrolls_no_further_than_the_selection_above_needs() {
        let kinds = normal(30);
        // Previously centred on a band at 14..18 inside a 11-row
        // list: starts at 14 - 3 = 11.
        let previous = tape_list_window_start(&kinds, Some(14..18), 11, true, None, None);
        assert_eq!(previous, 11);
        // The selection moves to row 2, above the current window --
        // the window follows it up exactly as far as it must, not
        // all the way back to a freshly centred position.
        let start =
            tape_list_window_start(&kinds, Some(14..18), 11, false, Some(2), Some(previous));
        assert_eq!(start, 2);
    }

    #[test]
    fn follow_off_scrolls_no_further_than_the_selection_below_needs() {
        let kinds = normal(30);
        let previous = tape_list_window_start(&kinds, Some(14..18), 11, true, None, None);
        assert_eq!(previous, 11);
        // The selection moves to row 25, below the current window
        // (11..22) -- the window follows it down just enough to put
        // it on the window's own last row.
        let start =
            tape_list_window_start(&kinds, Some(14..18), 11, false, Some(25), Some(previous));
        assert_eq!(start, 15);
    }

    #[test]
    fn follow_off_with_the_selection_already_in_view_does_not_move() {
        let kinds = normal(30);
        let previous = tape_list_window_start(&kinds, Some(14..18), 11, true, None, None);
        let start =
            tape_list_window_start(&kinds, Some(14..18), 11, false, Some(15), Some(previous));
        assert_eq!(start, previous);
    }

    #[test]
    fn follow_off_with_no_previous_window_centres_like_follow_on() {
        // Follow off from the very first draw (no previous window
        // recorded yet) centres exactly as follow on would.
        let kinds = normal(20);
        let on = tape_list_window_start(&kinds, Some(8..12), 11, true, None, None);
        let off = tape_list_window_start(&kinds, Some(8..12), 11, false, Some(0), None);
        assert_eq!(off, on);
    }

    #[test]
    fn follow_off_keeps_the_selection_in_view_even_when_the_now_area_is_taller_than_the_list() {
        // A band of 12 rows (5..17) inside a 10-row list -- on its
        // own this would always show from the band's first row, but
        // follow is off and the selection sits above the previous
        // window, so it must scroll up to meet it instead.
        let kinds = normal(30);
        let start = tape_list_window_start(&kinds, Some(5..17), 10, false, Some(2), Some(5));
        assert_eq!(start, 2);
    }

    #[test]
    fn ended_keeps_the_tail_biased_rule_unaffected_by_the_window() {
        // `now_area: None` (Ended/Failed): the original rule, biased
        // toward the tail, is untouched by any of this.
        let kinds = normal(30);
        let start = tape_list_window_start(&kinds, None, 11, true, None, None);
        assert_eq!(start, 19);
        let start = tape_list_window_start(&kinds, None, 11, false, Some(5), None);
        assert_eq!(start, 5);
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
    app: &mut App,
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
    let mut kinds: Vec<TapeListLineKind> = Vec::new();
    // Issue #710's window rule: the Now area's own extent in
    // `lines` -- `None` outside `TapeHead::Starting`/`Normal`, set
    // once `now_area_start` is known and again once the band itself
    // ends (the first ahead row after it, or `Run End` with no ahead
    // rows at all).
    let mut now_area_start: Option<usize> = None;
    let mut now_area_end: Option<usize> = None;
    let mut in_now_area = false;
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
                kinds.push(TapeListLineKind::Normal);
            }
            TapeItem::Row(row) => {
                // The first ahead row after the band (`in_now_area`
                // set by `NowBand`, below): a running row's own
                // `bold` is the only thing that tells bucket2 (still
                // part of the band) apart from bucket3 (ahead, which
                // ends it) -- both read `bold: false` otherwise, the
                // same as every finished row above the band.
                if in_now_area && !row.bold {
                    now_area_end = Some(lines.len());
                    in_now_area = false;
                }
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
                kinds.push(TapeListLineKind::Normal);
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
                        kinds.push(TapeListLineKind::Preview);
                    }
                }
            }
            TapeItem::NowBand { ts, elapsed_s } => {
                now_area_start = Some(lines.len());
                in_now_area = true;
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
                kinds.push(TapeListLineKind::Normal);
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
                kinds.push(TapeListLineKind::Normal);
            }
            TapeItem::HeadRule => {
                // The Starting state's own Now area is this rule
                // alone (issue #710) -- it is always `lines[0]`
                // (core's own `build_tape` pushes it first, with no
                // `Run Started` row before it yet).
                if matches!(screen.head, oscilloscope_core::screens::TapeHead::Starting) {
                    now_area_start = Some(lines.len());
                }
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
                kinds.push(TapeListLineKind::Normal);
                if matches!(screen.head, oscilloscope_core::screens::TapeHead::Starting) {
                    now_area_end = Some(lines.len());
                }
            }
            TapeItem::RunEnd {
                usual_total_tokens,
                usual_wall_s,
                eta_ts,
            } => {
                // No ahead rows at all between the band and `Run
                // End` -- the band runs right up to it.
                if in_now_area {
                    now_area_end = Some(lines.len());
                    in_now_area = false;
                }
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
                                "usually {}",
                                oscilloscope_core::format::format_tokens(*usual)
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
                kinds.push(TapeListLineKind::Normal);
            }
        }
    }

    // -1 rule, -1 totals row.
    let viewport = inner.height.saturating_sub(2) as usize;
    let now_area = now_area_start.zip(now_area_end).map(|(s, e)| s..e);
    // Issue #710: while a run is live, the list's own `L` for the
    // centred window is the whole of this area, at every width --
    // the 80-119 column floor's own one extra row of slack (issue
    // #678's own 80x24 vs 60x24) is now exactly the centred
    // window's own blank padding, not a separate fixed gap. Ended
    // and Failed keep that floor's slack exactly as before.
    let content_rows = if now_area.is_some() || narrow || wide_detail {
        viewport
    } else {
        viewport.saturating_sub(1)
    };
    let selected_idx = app.selected_path.as_deref().and_then(|p| {
        lines
            .iter()
            .position(|(path, _)| path.as_deref() == Some(p))
    });
    let start = tape_list_window_start(
        &kinds,
        now_area,
        content_rows,
        app.follow,
        selected_idx,
        app.tape_list_start,
    );
    app.tape_list_start = Some(start);
    let (real_range, blank_first) = tape_list_window_range(&kinds, start, content_rows);
    let mut visible: Vec<Line> = lines
        .into_iter()
        .skip(real_range.start)
        .take(real_range.len())
        .map(|(_, l)| l)
        .collect();
    if blank_first {
        if let Some(first) = visible.first_mut() {
            *first = Line::styled(
                " ".repeat(inner.width as usize),
                Style::default().fg(border_color),
            );
        }
    }
    let pad_before = real_range.start as isize - start;
    let list_area = Rect {
        x: inner.x,
        y: inner.y + pad_before as u16,
        width: inner.width,
        height: visible.len() as u16,
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
            format!("≈{}", oscilloscope_core::format::format_tokens(total))
        } else {
            oscilloscope_core::format::format_tokens(total)
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
                oscilloscope_core::format::format_tokens_arrow(sent_total),
                oscilloscope_core::format::format_tokens_arrow(received_total)
            ),
            if live {
                Style::default().fg(ctx.theme.dim)
            } else {
                Style::default().fg(ctx.theme.text)
            },
        ),
        Span::styled(
            oscilloscope_core::format::format_tokens(sent_total + received_total),
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

/// Issue #704 rule 4's own Attempts section, shared by every screen:
/// one line per `meta.fallback_attempts` entry, oldest first --
/// empty for a step the run never retried at all.
fn focused_attempts_lines(
    step: &oscilloscope_core::screens::FocusedStep,
    width: usize,
    ctx: &DrawCtx,
) -> Vec<Line<'static>> {
    // A node's own `fallback_attempts` always carries at least the
    // one pass that decided the outcome, even with no retry at all
    // -- never shown as an "Attempts" section on its own merits
    // unless it actually names more than one attempt, or
    // `retries_used` says an earlier, now-overwritten pass happened.
    if step.attempts.len() <= 1 && step.retries_used.is_none() {
        return Vec::new();
    }
    let muted = Style::default().fg(ctx.theme.fg_muted);
    let default = Style::default().fg(ctx.theme.text);
    let title_bold = Style::default()
        .fg(ctx.theme.text)
        .add_modifier(Modifier::BOLD);
    let title = match step.retries_used {
        Some(n) => format!("Attempts · {n} retried"),
        None => "Attempts".to_string(),
    };
    let mut lines = vec![
        Line::from(""),
        tape_detail_line(vec![Span::styled(title, title_bold)], width),
    ];
    for (i, attempt) in step.attempts.iter().enumerate() {
        let mut text = format!("{}. ", i + 1);
        let mut parts = Vec::new();
        if let Some(adapter) = &attempt.adapter {
            parts.push(adapter.clone());
        }
        if let Some(model) = &attempt.model {
            parts.push(model.clone());
        }
        if let Some(status) = &attempt.status {
            parts.push(status.clone());
        }
        if attempt.tokens_sent.is_some() || attempt.tokens_received.is_some() {
            parts.push(format!(
                "↑{} ↓{}",
                attempt
                    .tokens_sent
                    .map(oscilloscope_core::format::format_tokens_arrow)
                    .unwrap_or_else(|| "—".to_string()),
                attempt
                    .tokens_received
                    .map(oscilloscope_core::format::format_tokens_arrow)
                    .unwrap_or_else(|| "—".to_string())
            ));
        }
        text.push_str(&parts.join(" · "));
        lines.push(tape_detail_line(vec![Span::styled(text, default)], width));
        if let Some(error) = &attempt.error {
            for line in tape_word_wrap(error, width.saturating_sub(3)) {
                lines.push(tape_detail_line(
                    vec![Span::styled(format!("   {line}"), muted)],
                    width,
                ));
            }
        }
    }
    lines
}

/// Issue #704 rule 4's own Asked section for a tool step, for the
/// Tape's own append-only renderer (`tape_normal_detail_lines`)
/// alone -- every other screen's own Asked already shows a tool's
/// own command line inline (`focused_step_full_lines`'s and `flow_
/// selected_step_lines`'s own `asked` match), but the Tape's own
/// detail pane never had an Asked section for anything but a
/// prompt's own "Prompt Sent" at all.
fn focused_tool_asked_lines(
    step: &oscilloscope_core::screens::FocusedStep,
    width: usize,
    ctx: &DrawCtx,
) -> Vec<Line<'static>> {
    use oscilloscope_core::screens::FocusDetail;
    let FocusDetail::Tool {
        command: Some(command),
        ..
    } = &step.detail
    else {
        return Vec::new();
    };
    let default = Style::default().fg(ctx.theme.text);
    let title_bold = Style::default()
        .fg(ctx.theme.text)
        .add_modifier(Modifier::BOLD);
    let mut lines = vec![
        Line::from(""),
        tape_detail_line(vec![Span::styled("Asked", title_bold)], width),
    ];
    for line in tape_word_wrap(command, width.saturating_sub(1)) {
        lines.push(tape_detail_line(vec![Span::styled(line, default)], width));
    }
    lines
}

/// Issue #704 rule 4's own Tool section, shared by every screen: the
/// exit code and full stdout/stderr -- the command line itself is
/// each screen's own Asked section already, never repeated here.
fn focused_tool_lines(
    step: &oscilloscope_core::screens::FocusedStep,
    width: usize,
    ctx: &DrawCtx,
) -> Vec<Line<'static>> {
    use oscilloscope_core::screens::FocusDetail;
    let FocusDetail::Tool {
        exit_code,
        stdout,
        stderr,
        ..
    } = &step.detail
    else {
        return Vec::new();
    };
    if exit_code.is_none() && stdout.is_none() && stderr.is_none() {
        return Vec::new();
    }
    let muted = Style::default().fg(ctx.theme.fg_muted);
    let default = Style::default().fg(ctx.theme.text);
    let title_bold = Style::default()
        .fg(ctx.theme.text)
        .add_modifier(Modifier::BOLD);
    let mut lines = vec![
        Line::from(""),
        tape_detail_line(vec![Span::styled("Tool", title_bold)], width),
    ];
    if let Some(code) = exit_code {
        lines.push(tape_detail_line(
            vec![
                Span::styled("exit ", muted),
                Span::styled(code.to_string(), default),
            ],
            width,
        ));
    }
    if let Some(stdout) = stdout {
        lines.push(tape_detail_line(vec![Span::styled("stdout", muted)], width));
        for line in tape_word_wrap(stdout, width.saturating_sub(1)) {
            lines.push(tape_detail_line(vec![Span::styled(line, default)], width));
        }
    }
    if let Some(stderr) = stderr {
        lines.push(tape_detail_line(vec![Span::styled("stderr", muted)], width));
        for line in tape_word_wrap(stderr, width.saturating_sub(1)) {
            lines.push(tape_detail_line(vec![Span::styled(line, default)], width));
        }
    }
    lines
}

/// Issue #704 rule 4's own Decision section, shared by every screen
/// but the Flow (whose own `decision_detail_lines` already covers a
/// decision's own Selected pane): condition, branch, the model's own
/// answer, and an inferred decision's own evidence.
fn focused_decision_lines(
    step: &oscilloscope_core::screens::FocusedStep,
    width: usize,
    ctx: &DrawCtx,
) -> Vec<Line<'static>> {
    use oscilloscope_core::screens::FocusDetail;
    let FocusDetail::Decision(decision) = &step.detail else {
        return Vec::new();
    };
    let muted = Style::default().fg(ctx.theme.fg_muted);
    let default = Style::default().fg(ctx.theme.text);
    let title_bold = Style::default()
        .fg(ctx.theme.text)
        .add_modifier(Modifier::BOLD);
    let mut lines = vec![
        Line::from(""),
        tape_detail_line(vec![Span::styled("Decision", title_bold)], width),
    ];
    if let Some(condition) = &decision.condition {
        for line in tape_word_wrap(&condition.text, width.saturating_sub("condition ".len())) {
            lines.push(tape_detail_line(
                vec![
                    Span::styled("condition ", muted),
                    Span::styled(line, default),
                ],
                width,
            ));
        }
    }
    let branch_text = match &decision.branch {
        Some(branch) => branch.clone(),
        None if decision.nothing_ran => "none".to_string(),
        None => "pending".to_string(),
    };
    lines.push(tape_detail_line(
        vec![
            Span::styled("branch ", muted),
            Span::styled(branch_text, default),
        ],
        width,
    ));
    if let Some(answer) = &decision.answer {
        lines.push(tape_detail_line(
            vec![
                Span::styled("answer ", muted),
                Span::styled(answer.clone(), default),
            ],
            width,
        ));
    }
    if let Some(evidence) = &decision.inferred_evidence {
        lines.push(tape_detail_line(
            vec![Span::styled(evidence.clone(), muted)],
            width,
        ));
    }
    lines
}

/// Issue #704 rule 4's own Loop section, shared by every screen: one
/// line per pass, its own status and tokens so far.
fn focused_loop_lines(
    step: &oscilloscope_core::screens::FocusedStep,
    width: usize,
    ctx: &DrawCtx,
) -> Vec<Line<'static>> {
    use oscilloscope_core::screens::FocusDetail;
    let FocusDetail::Loop(passes) = &step.detail else {
        return Vec::new();
    };
    if passes.is_empty() {
        return Vec::new();
    }
    let muted = Style::default().fg(ctx.theme.fg_muted);
    let default = Style::default().fg(ctx.theme.text);
    let title_bold = Style::default()
        .fg(ctx.theme.text)
        .add_modifier(Modifier::BOLD);
    let mut lines = vec![
        Line::from(""),
        tape_detail_line(vec![Span::styled("Loop", title_bold)], width),
    ];
    for pass in passes {
        let (glyph, word, glyph_style) = tape_status_glyph_word_style(pass.status.kind, ctx.theme);
        let tokens_text = pass
            .tokens
            .map(oscilloscope_core::format::format_tokens)
            .unwrap_or_else(|| "—".to_string());
        lines.push(tape_detail_line(
            vec![
                Span::styled(format!("{glyph} {word}  "), glyph_style),
                Span::styled(format!("{:<16}", pass.label), default),
                Span::styled(tokens_text, muted),
            ],
            width,
        ));
    }
    lines
}

/// Issue #704 rule 4's own "enter on a pass opens it": which of
/// `lines` (already built, by `focused_loop_lines`'s own padded-
/// label text, unique per pass) is which pass's own line -- `App::
/// loop_pass_line_paths`' own source, so `enter` while the full-
/// screen step is open can tell which pass the cursor is actually
/// over without the Loop section's own line count (Status/Asked/
/// Produced/.../Meta, each only present when the node has data for
/// it) ever needing to be recomputed separately from what was
/// already drawn.
fn loop_pass_line_paths(
    lines: &[Line<'static>],
    step: &oscilloscope_core::screens::FocusedStep,
) -> Vec<(usize, String)> {
    use oscilloscope_core::screens::FocusDetail;
    let FocusDetail::Loop(passes) = &step.detail else {
        return Vec::new();
    };
    passes
        .iter()
        .filter_map(|pass| {
            let label = format!("{:<16}", pass.label);
            let idx = lines
                .iter()
                .position(|line| line.to_string().contains(&label))?;
            Some((idx, pass.path.clone()))
        })
        .collect()
}

/// Issue #704 rule 4's own Meta section, shared by every screen:
/// every other key the run recorded, folded by default (`m`
/// unfolds).
fn focused_meta_lines(
    step: &oscilloscope_core::screens::FocusedStep,
    width: usize,
    ctx: &DrawCtx,
    unfolded: bool,
) -> Vec<Line<'static>> {
    if step.meta_extra.is_empty() {
        return Vec::new();
    }
    let muted = Style::default().fg(ctx.theme.fg_muted);
    let default = Style::default().fg(ctx.theme.text);
    let title_bold = Style::default()
        .fg(ctx.theme.text)
        .add_modifier(Modifier::BOLD);
    let title = if unfolded {
        "Meta".to_string()
    } else {
        format!("Meta · {} keys · m to unfold", step.meta_extra.len())
    };
    let mut lines = vec![
        Line::from(""),
        tape_detail_line(vec![Span::styled(title, title_bold)], width),
    ];
    if unfolded {
        for (key, value) in &step.meta_extra {
            for (i, line) in tape_word_wrap(value, width.saturating_sub(key.len() + 2))
                .into_iter()
                .enumerate()
            {
                let spans = if i == 0 {
                    vec![
                        Span::styled(format!("{key}: "), muted),
                        Span::styled(line, default),
                    ]
                } else {
                    vec![Span::styled(line, default)]
                };
                lines.push(tape_detail_line(spans, width));
            }
        }
    }
    lines
}

/// The Normal/Usual states' own detail pane (§2.1): the selected
/// row's Status, Type, Tokens, Usual, Share, then the prompt sent and
/// the reply in full, each headed with its own token count.
fn tape_normal_detail_lines(
    screen: &oscilloscope_core::screens::TapeScreen,
    width: usize,
    // Kept for every caller's own symmetry with the full-screen
    // step's own viewport-sized call, though nothing here still
    // reads it: the Agent block's own transcript no longer caps to
    // it (issue #704 rule 4, "nothing is cut").
    _height: usize,
    transcript_view: Option<&oscilloscope_core::transcript::TranscriptView>,
    now: &str,
    meta_unfolded: bool,
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
                let mut received_text =
                    format!("{marker}{}", oscilloscope_core::format::format_tokens(n));
                if let Some(rate) = live.tokens_per_second {
                    received_text.push_str(&format!(
                        " · {}/s",
                        oscilloscope_core::format::format_tokens(rate.round() as u64)
                    ));
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
                    format!("≈{}", oscilloscope_core::format::format_tokens(n))
                } else {
                    format!("↑{}", oscilloscope_core::format::format_tokens(n))
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

    if detail.tokens_sent.is_some() || detail.tokens_received.is_some() {
        // An agent session's own figures can be partial (Claude
        // Code never reports `tokens_received` live) -- the one that
        // is still unknown reads `—`, never a false `0`.
        let sent_text = detail
            .tokens_sent
            .map(oscilloscope_core::format::format_tokens_arrow)
            .unwrap_or_else(|| "—".to_string());
        let received_text = detail
            .tokens_received
            .map(oscilloscope_core::format::format_tokens_arrow)
            .unwrap_or_else(|| "—".to_string());
        let total = detail.tokens_sent.unwrap_or(0) + detail.tokens_received.unwrap_or(0);
        lines.push(tape_detail_line(
            vec![
                Span::styled("Tokens  ", muted),
                Span::styled(format!("↑{sent_text} ↓{received_text} "), default),
                Span::styled("· ", muted),
                Span::styled(
                    oscilloscope_core::format::format_tokens(total),
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
                format!("{}  ", oscilloscope_core::format::format_tokens(usual)),
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

    // Issue #703's rule 3: an agent step's own detail pane adds the
    // one shared Agent block after Tokens/Usual/Share, which still
    // read from the step's own totals the same as any other tool --
    // Live, Prompt Sent and Reply above and below are already `None`
    // for a non-prompt row, so they never draw here.
    if let oscilloscope_core::screens::FocusDetail::Agent {
        stale, live, note, ..
    } = &step.detail
    {
        if *stale {
            lines.push(tape_detail_line(
                vec![Span::styled("stopped", muted)],
                width,
            ));
        }
        let running = matches!(
            step.status.kind,
            oscilloscope_core::model::StatusKind::Running
                | oscilloscope_core::model::StatusKind::LikelyRunning
                | oscilloscope_core::model::StatusKind::RunningOrQueued
        );
        // Issue #704 rule 4: the detail pane (the small one and the
        // full-screen step alike, both scrollable) reads the whole
        // transcript, never cut to this pane's own current height --
        // nothing the run recorded is cut.
        for line in agent_block_lines(step, transcript_view, ctx, now, None) {
            lines.push(tape_detail_line(line.spans, width));
        }
        if live.is_none() && running {
            lines.push(tape_detail_line(vec![Span::styled(*note, muted)], width));
        }
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
                format!("· {}", oscilloscope_core::format::format_tokens(tokens)),
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
                        "· {}",
                        oscilloscope_core::format::format_tokens(detail.reply_tokens.unwrap_or(0))
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

    // Issue #704 rule 4: the sections every screen's own selection
    // now shares, each only when the node has data for it -- a
    // prompt's own Produced is already the Reply block above, never
    // repeated here.
    if !matches!(
        step.detail,
        oscilloscope_core::screens::FocusDetail::Prompt { .. }
    ) {
        if let Some(value) = &step.value_full {
            lines.push(Line::from(""));
            lines.push(tape_detail_line(
                vec![Span::styled("Produced", title_bold)],
                width,
            ));
            for line in tape_word_wrap(value, width.saturating_sub(1)) {
                lines.push(tape_detail_line(vec![Span::styled(line, default)], width));
            }
        }
    }
    if let Some(error) = &step.error {
        lines.push(Line::from(""));
        lines.push(tape_detail_line(
            vec![Span::styled("Error", title_bold)],
            width,
        ));
        for line in tape_word_wrap(error, width.saturating_sub(1)) {
            lines.push(tape_detail_line(
                vec![Span::styled(line, Style::default().fg(ctx.theme.error))],
                width,
            ));
        }
    }
    lines.extend(focused_tool_asked_lines(step, width, ctx));
    lines.extend(focused_attempts_lines(step, width, ctx));
    lines.extend(focused_tool_lines(step, width, ctx));
    lines.extend(focused_decision_lines(step, width, ctx));
    lines.extend(focused_loop_lines(step, width, ctx));
    lines.extend(focused_meta_lines(step, width, ctx, meta_unfolded));

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
                format!("{} ", oscilloscope_core::format::format_tokens(tokens)),
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
                    "usually {}",
                    oscilloscope_core::format::format_tokens(usual)
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
        "Finished steps stack up above Now.",
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
                        oscilloscope_core::format::format_tokens_arrow(detail.tokens_sent),
                        oscilloscope_core::format::format_tokens_arrow(detail.tokens_received)
                    ),
                    default,
                ),
                Span::styled(
                    oscilloscope_core::format::format_tokens(
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
                format!("{} ", oscilloscope_core::format::format_tokens(tokens)),
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
                    oscilloscope_core::format::format_tokens(tokens),
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
                        oscilloscope_core::format::format_tokens_arrow(detail.tokens_sent),
                        oscilloscope_core::format::format_tokens_arrow(detail.tokens_received)
                    ),
                    default,
                ),
                Span::styled(
                    oscilloscope_core::format::format_tokens(
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
                format!("{} ", oscilloscope_core::format::format_tokens(tokens)),
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

#[allow(clippy::too_many_arguments)]
fn draw_tape_detail(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::screens::TapeScreen,
    app: &mut App,
    transcript_view: Option<&oscilloscope_core::transcript::TranscriptView>,
    now: &str,
    ctx: &DrawCtx,
) {
    use oscilloscope_core::screens::TapeHead;
    // Issue #704 rule 1: once the run has ended (or failed), the
    // detail pane shows the run summary for as long as `selected_
    // path` is still `tape_run_end_target` (the row `tape_select_
    // run_end_once` moved the selection to the instant the run
    // ended) -- a comparison made fresh on every draw, not a sticky
    // flag, so moving the selection away through *any* screen or key
    // (the Signal, the Flow, `[`/`]` in the full-screen step, not
    // only the Tape's own ↑↓) ends summary mode, and moving back onto
    // that exact row re-enters it, the same as any other row's own
    // details always have.
    let ended_row_selected = app.tape_run_end_target.is_some()
        && app.tape_run_end_target.as_deref() == app.selected_path.as_deref();
    let title = match &screen.head {
        TapeHead::Starting => "Run · Starting".to_string(),
        TapeHead::Ended { .. } if ended_row_selected => "Run · Ended".to_string(),
        TapeHead::Failed { .. } if ended_row_selected => "Run · Failed".to_string(),
        TapeHead::Ended { .. } | TapeHead::Failed { .. } | TapeHead::Normal => {
            match &screen.selected_breadcrumb {
                Some(breadcrumb) => format!("{breadcrumb} · Selected"),
                None => "Tape".to_string(),
            }
        }
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
        TapeHead::Ended { .. } if ended_row_selected => screen
            .ended_detail
            .as_ref()
            .map(|d| tape_ended_detail_lines(d, width, ctx))
            .unwrap_or_default(),
        TapeHead::Failed { .. } if ended_row_selected => screen
            .failed_detail
            .as_ref()
            .map(|d| tape_failed_detail_lines(d, width, ctx))
            .unwrap_or_default(),
        TapeHead::Ended { .. } | TapeHead::Failed { .. } | TapeHead::Normal => {
            tape_normal_detail_lines(
                screen,
                width,
                content_area.height as usize,
                transcript_view,
                now,
                app.step_meta_unfolded,
                ctx,
            )
        }
    };
    if let Some(step) = &screen.selected {
        app.loop_pass_line_paths = loop_pass_line_paths(&lines, step);
    }
    // Issue #704 rule 4: the full-screen step scrolls with ↑↓/PgUp/
    // PgDn/Home/End (`App::scroll_details`, the same shared scroll
    // position the Flow's own Selected pane already uses) -- a no-op
    // in the ordinary detail pane, whose content already fits.
    let viewport = content_area.height as usize;
    app.details_viewport = viewport;
    let total = lines.len();
    // Issue #704 rule 4: `End` lands on the last full page, not the
    // last single line with the rest of the viewport left blank --
    // the clamp is against a whole page's worth, never `total - 1`.
    app.details_scroll = app
        .details_scroll
        .min(total.saturating_sub(viewport.max(1)));
    let start = app.details_scroll.min(total);
    let visible: Vec<Line> = lines.into_iter().skip(start).collect();
    frame.render_widget(Paragraph::new(visible), content_area);
}

/// The too-small screen (§5): below 60×24, the minimum and
/// `osi --log` -- every screen's own floor, not only the Tape's,
/// since nothing below it has room for the shared chrome at all.
const MIN_WIDTH: u16 = 60;
const MIN_HEIGHT: u16 = 24;

/// Select's own width floor (§5): its narrowest frame is 80x24, with
/// nothing narrower ever shown -- higher than every other screen's
/// shared 60-column [`MIN_WIDTH`].
const SELECT_MIN_WIDTH: u16 = 80;

/// §5's own too-small screen (the design's `tape--toosmall` frame):
/// the `osi` word, the minimum each dimension needs and whether it's
/// met, the plain-language line naming both and `osi --log`, and a
/// bare `q Quit` key bar -- the only state that ever draws below the
/// shared 60×24 floor, on every screen.
fn draw_too_small(frame: &mut Frame, area: Rect, ctx: &DrawCtx) {
    draw_too_small_with_min_width(frame, area, ctx, MIN_WIDTH);
}

/// [`draw_too_small`], with its own width floor rather than the
/// shared [`MIN_WIDTH`] -- Select's own body needs 80 columns, wider
/// than every other screen's shared 60 (§5), so it alone draws this
/// notice that much sooner.
fn draw_too_small_with_min_width(frame: &mut Frame, area: Rect, ctx: &DrawCtx, min_width: u16) {
    let ok_style = Style::default().fg(ctx.theme.status_done);
    let err_style = Style::default().fg(ctx.theme.status_failed);
    let err_bold_style = err_style.add_modifier(Modifier::BOLD);
    let width_ok = area.width >= min_width;
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
        dim_line("Width", area.width, min_width, width_ok),
        dim_line("Height", area.height, MIN_HEIGHT, height_ok),
        Line::from(""),
        Line::styled(
            format!("      osi needs {min_width} columns and {MIN_HEIGHT} rows · try osi --log"),
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

/// The Select screen's own body (issue #698, §2.5): either the
/// running list or, with nothing running, the Recent Runs panel --
/// never both. Drawn by [`draw_select`], which owns the shared
/// chrome (header, status line, key bar) around whichever of these
/// is showing.
pub enum SelectBody<'a> {
    Running(&'a [oscilloscope_core::select::Row]),
    Empty(&'a [oscilloscope_core::select::RecentRow]),
}

/// The Select screen (issue #698): the shared header band and key
/// bar (§3), with its own centred running list or full-width Recent
/// Runs panel in between -- below the shared floor (§5), the same
/// too-small notice every other screen reads, but at its own,
/// higher width floor ([`SELECT_MIN_WIDTH`]): the 80-column frame is
/// already this screen's own narrowest, with nothing narrower ever
/// shown. `cursor` indexes whichever list `body` holds; out of range
/// (an empty list) draws no selected row at all.
pub fn draw_select(
    frame: &mut Frame,
    body: &SelectBody,
    cursor: usize,
    now: &str,
    scan_enabled: bool,
    ctx: &DrawCtx,
) {
    let area = frame.area();
    if let Some(bg) = ctx.theme.background {
        frame.render_widget(
            Block::default().style(Style::default().bg(bg).fg(ctx.theme.text)),
            area,
        );
    }
    if area.width < SELECT_MIN_WIDTH || area.height < MIN_HEIGHT {
        draw_too_small_with_min_width(frame, area, ctx, SELECT_MIN_WIDTH);
        return;
    }
    let running = match body {
        SelectBody::Running(rows) => rows.len().saturating_sub(1),
        SelectBody::Empty(_) => 0,
    };
    draw_select_header_band(
        frame,
        Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        },
        running,
        now,
        ctx,
    );
    let body_area = Rect {
        x: area.x,
        y: area.y + 1,
        width: area.width,
        height: area.height.saturating_sub(3),
    };
    match body {
        SelectBody::Running(rows) => draw_select_list(frame, body_area, rows, cursor, ctx),
        SelectBody::Empty(recent) => {
            draw_select_empty_panel(frame, body_area, recent, cursor, scan_enabled, ctx)
        }
    }
    let status_area = Rect {
        x: area.x,
        y: area.y + area.height - 2,
        width: area.width,
        height: 1,
    };
    draw_select_status_line(frame, status_area, body, ctx);
    let key_bar_area = Rect {
        x: area.x,
        y: area.y + area.height - 1,
        width: area.width,
        height: 1,
    };
    draw_key_bar(
        frame,
        key_bar_area,
        &[
            ("↑↓", "Select"),
            ("enter", "Open"),
            ("?", "Help"),
            ("q", "Quit"),
        ],
        ctx,
    );
}

/// The Select screen's own `?` overlay (issue #698, §4): exactly its
/// own key bar's keys, same shape as every other screen's own Help
/// (`draw_dialog_overlay`'s `Dialog::Help` arms) -- not frame-tested,
/// since no `select--*` frame shows it open.
pub fn draw_select_help(frame: &mut Frame, ctx: &DrawCtx) {
    let area = frame.area();
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
        .title(tape_block_title("Help · Select", border, ctx))
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
    let key_style = Style::default()
        .fg(ctx.theme.keyhint_key)
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(ctx.theme.keyhint_desc);
    let row = |k: &str, d: &str| {
        Line::from(vec![
            Span::styled(format!("{k:<14}"), key_style),
            Span::styled(d.to_string(), desc_style),
        ])
    };
    let lines = vec![
        row("↑↓ j k", "Select"),
        row("Home End g G", "Jump to the ends"),
        row("enter", "Open the selected run"),
        row("T", "Colour theme"),
        row("?", "This help"),
        row("q", "Quit"),
        row("c", "Inert here (no run attached to cancel)"),
    ];
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(ctx.theme.bg_surface)),
        content_area,
    );
}

/// The header band (§3.1), Select's own shape: ` osi  Select ·
/// <status>` on the left, the `SELECT` chip and the clock on the
/// right -- never animated (issue #698: Select watches no one run's
/// own process to animate a trace for).
fn draw_select_header_band(
    frame: &mut Frame,
    area: Rect,
    running_count: usize,
    now: &str,
    ctx: &DrawCtx,
) {
    let bg = ctx.theme.statusbar_bg;
    let base = Style::default().bg(bg).fg(ctx.theme.statusbar_fg);
    let mut left = vec![
        Span::styled(
            " osi  ",
            Style::default()
                .fg(ctx.theme.accent_primary)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("Select ", Style::default().fg(ctx.theme.text).bg(bg)),
        Span::styled("· ", Style::default().fg(ctx.theme.dim).bg(bg)),
    ];
    if running_count > 0 {
        left.push(Span::styled(
            "● ",
            Style::default()
                .fg(ctx.theme.accent_primary)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ));
        left.push(Span::styled(
            format!("{running_count} Running"),
            Style::default().fg(ctx.theme.text).bg(bg),
        ));
    } else {
        left.push(Span::styled(
            "◌ Nothing Running",
            Style::default()
                .fg(ctx.theme.fg_muted)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ));
    }
    let banner_text = ctx
        .theme_banner
        .map(|name| format!("theme: {name}  "))
        .unwrap_or_default();
    let left_width: usize = left.iter().map(|s| s.content.chars().count()).sum();
    let clock_text =
        oscilloscope_core::format::format_clock_time(now, ctx.zone).unwrap_or_default();
    let chip_text = "  SELECT  ".to_string();
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

/// The running list's own box (§2.5): centred, 60 cells wide at 120
/// columns and up, 56 below that (`design/frames/select--normal--
/// 120x30`/`--80x24`) -- `All` first, then one row per run, `cursor`
/// drawing the selected one with `selection.bg`.
fn draw_select_list(
    frame: &mut Frame,
    area: Rect,
    rows: &[oscilloscope_core::select::Row],
    cursor: usize,
    ctx: &DrawCtx,
) {
    let run_count = rows.len().saturating_sub(1);
    let box_width: u16 = if area.width >= 100 {
        60
    } else {
        56.min(area.width.saturating_sub(4))
    };
    let box_height = rows.len() as u16 + 2;
    let box_x = area.x + area.width.saturating_sub(box_width) / 2;
    let box_y = area.y + area.height.saturating_sub(box_height) / 2;
    let rect = Rect {
        x: box_x,
        y: box_y,
        width: box_width,
        height: box_height.min(area.height),
    };
    let border = ctx.theme.border_focused;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(ratatui::symbols::border::ROUNDED)
        .title(tape_block_title(
            &format!("Running Orchestrations · {run_count}"),
            border,
            ctx,
        ))
        .border_style(Style::default().fg(border));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    // The cursor can sit on a row the box isn't tall enough to show
    // all of at once (more running rows than the terminal's own
    // height leaves room for) -- the window scrolls to keep it in
    // view, the selected row landing at the window's own bottom edge
    // the moment it would otherwise scroll off, the same rule the
    // Store table's own cursor already follows.
    let max_visible = inner.height as usize;
    let max_start = rows.len().saturating_sub(max_visible);
    let start = cursor
        .saturating_sub(max_visible.saturating_sub(1))
        .min(max_start);

    let config_width = (inner.width as usize).saturating_sub(32);
    for (i, row) in rows.iter().enumerate().skip(start).take(max_visible) {
        let line_rect = Rect {
            x: inner.x,
            y: inner.y + (i - start) as u16,
            width: inner.width,
            height: 1,
        };
        let spans = select_row_spans(row, i == cursor, config_width, ctx);
        frame.render_widget(Paragraph::new(Line::from(spans)), line_rect);
    }
}

/// One [`draw_select_list`] row's own spans -- shared between `All`
/// and a run row, and between a selected and a plain one, so the
/// field widths (doc 15, config `config_width`, engine 11) can never
/// drift apart between the two stylings.
fn select_row_spans(
    row: &oscilloscope_core::select::Row,
    selected: bool,
    config_width: usize,
    ctx: &DrawCtx,
) -> Vec<Span<'static>> {
    use oscilloscope_core::select::Row as SelectRow;
    let doc_text = match row {
        SelectRow::All { .. } => "All".to_string(),
        SelectRow::Run { document_label, .. } => document_label.clone(),
    };
    let tail = match row {
        SelectRow::All { running } => {
            // A refresh can leave exactly one run after another ends
            // while Select stays open with more than one still on
            // screen (the single-run auto-attach only ever applies
            // at `run_select`'s own, one-time startup check).
            let noun = if *running == 1 { "run" } else { "runs" };
            format!(
                "{:<w$}",
                format!("{running} {noun} · tiled"),
                w = config_width + 11 + 1
            )
        }
        SelectRow::Run {
            config_label, run, ..
        } => format!(
            "{:<cw$}{:>11} ",
            config_label.clone().unwrap_or_else(|| "—".to_string()),
            run.engine.as_deref().unwrap_or("—"),
            cw = config_width
        ),
    };
    let rest = format!("{doc_text:<15}{tail}");
    if selected {
        let sel_bg = ctx.theme.selection_bg;
        vec![
            Span::styled(
                " ❯ ".to_string(),
                Style::default()
                    .fg(ctx.theme.accent_primary)
                    .bg(sel_bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "● ",
                Style::default()
                    .fg(ctx.theme.accent_primary)
                    .bg(sel_bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(rest, Style::default().fg(ctx.theme.selection_fg).bg(sel_bg)),
        ]
    } else {
        let doc_style = match row {
            SelectRow::All { .. } => Style::default()
                .fg(ctx.theme.text)
                .add_modifier(Modifier::BOLD),
            SelectRow::Run { .. } => Style::default().fg(ctx.theme.text),
        };
        vec![
            Span::styled("   ", Style::default().fg(ctx.theme.text)),
            Span::styled(
                "● ",
                Style::default()
                    .fg(ctx.theme.accent_primary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("{doc_text:<15}"), doc_style),
            Span::styled(tail, Style::default().fg(ctx.theme.fg_muted)),
        ]
    }
}

/// The status line (§3.2): what the screen is doing -- the engines'
/// own counts on the running list, a fixed line with nothing
/// running.
fn draw_select_status_line(frame: &mut Frame, area: Rect, body: &SelectBody, ctx: &DrawCtx) {
    let text = match body {
        SelectBody::Running(rows) => {
            let mut cof = 0usize;
            let mut electricity = 0usize;
            for row in rows.iter() {
                if let oscilloscope_core::select::Row::Run { run, .. } = row {
                    match run.engine.as_deref() {
                        Some("cof") => cof += 1,
                        Some("electricity") => electricity += 1,
                        _ => {}
                    }
                }
            }
            let mut parts = Vec::new();
            if cof > 0 {
                parts.push(format!("{cof} on cof"));
            }
            if electricity > 0 {
                parts.push(format!("{electricity} on electricity"));
            }
            parts.push("refreshing every 2s".to_string());
            format!(" {}", parts.join(" · "))
        }
        SelectBody::Empty(_) => " nothing running · refreshing every 2s".to_string(),
    };
    frame.render_widget(
        Paragraph::new(Line::styled(text, Style::default().fg(ctx.theme.text))),
        area,
    );
}

/// The empty Select screen's own panel (§2.5): a full-width box
/// naming where discovery looked (just `registry` with
/// `scan_enabled` false -- `OSI_PROCESS_SCAN=0` turns off both the
/// process scan and the current-directory source together), the
/// Recent Runs table, and the
/// three ways to start a run.
fn draw_select_empty_panel(
    frame: &mut Frame,
    area: Rect,
    rows: &[oscilloscope_core::select::RecentRow],
    cursor: usize,
    scan_enabled: bool,
    ctx: &DrawCtx,
) {
    let border = ctx.theme.border_focused;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(ratatui::symbols::border::ROUNDED)
        .title(tape_block_title("No Running Orchestrations", border, ctx))
        .border_style(Style::default().fg(border));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let text_style = Style::default().fg(ctx.theme.text);
    let muted_style = Style::default().fg(ctx.theme.fg_muted);
    let title_style = Style::default()
        .fg(ctx.theme.text)
        .add_modifier(Modifier::BOLD);
    let header_style = Style::default()
        .fg(ctx.theme.text)
        .add_modifier(Modifier::BOLD);

    let mut y = inner.y;
    render_line(
        frame,
        Rect {
            x: inner.x,
            y,
            width: inner.width,
            height: 1,
        },
        vec![
            Span::styled(" Looked ", muted_style),
            Span::styled(
                if scan_enabled {
                    "registry · process scan · live state in cwd"
                } else {
                    "registry"
                },
                text_style,
            ),
        ],
    );
    y += 1;
    render_line(
        frame,
        Rect {
            x: inner.x,
            y,
            width: inner.width,
            height: 1,
        },
        vec![
            Span::styled(" Found  ", muted_style),
            Span::styled("0 running", text_style),
        ],
    );
    y += 2;
    render_line(
        frame,
        Rect {
            x: inner.x,
            y,
            width: inner.width,
            height: 1,
        },
        vec![Span::styled(" Recent Runs · open read-only", title_style)],
    );
    y += 1;
    render_line(
        frame,
        Rect {
            x: inner.x,
            y,
            width: inner.width,
            height: 1,
        },
        vec![Span::styled(
            "      DOC      CONFIG         STARTED   TOOK  TOKENS OUTCOME",
            header_style,
        )],
    );
    y += 1;
    // `y` has already drawn 5 rows (`Looked`, `Found`, a blank, the
    // `Recent Runs` title and this header) and `Start One` below
    // needs 5 more of its own (a blank, its own title, three
    // examples) -- the table's own window scrolls to keep the
    // cursor in view within whatever's left, the same rule
    // `draw_select_list`'s own running rows already follow, rather
    // than letting `enter` open a row that scrolled off with no
    // indication it had.
    let trailing_rows = 5u16;
    let max_visible = (inner.height.saturating_sub(trailing_rows) as usize)
        .saturating_sub((y - inner.y) as usize);
    let max_start = rows.len().saturating_sub(max_visible);
    let start = cursor
        .saturating_sub(max_visible.saturating_sub(1))
        .min(max_start);
    for (i, row) in rows.iter().enumerate().skip(start).take(max_visible) {
        if y >= inner.y + inner.height {
            break;
        }
        let spans = recent_row_spans(row, i == cursor, ctx.zone, ctx);
        render_line(
            frame,
            Rect {
                x: inner.x,
                y,
                width: inner.width,
                height: 1,
            },
            spans,
        );
        y += 1;
    }
    y += 1;
    if y < inner.y + inner.height {
        render_line(
            frame,
            Rect {
                x: inner.x,
                y,
                width: inner.width,
                height: 1,
            },
            vec![Span::styled(" Start One", title_style)],
        );
    }
    y += 1;
    for line_text in [
        " osi demo.yml config.json      run a document and attach",
        " osi watch ./run                attach to a run directory",
        " osi --engine electricity …     run on the Rust engine",
    ] {
        if y >= inner.y + inner.height {
            break;
        }
        render_line(
            frame,
            Rect {
                x: inner.x,
                y,
                width: inner.width,
                height: 1,
            },
            vec![Span::styled(line_text.to_string(), muted_style)],
        );
        y += 1;
    }
}

fn render_line(frame: &mut Frame, rect: Rect, spans: Vec<Span<'static>>) {
    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
}

/// One Recent Runs row's own spans (issue #698): DOC(9) CONFIG(15)
/// STARTED(10) then TOOK+TOKENS combined (13, `{:<5}{:>7} `) and
/// OUTCOME -- field widths measured from `design/frames/select--
/// empty--80x24`.
fn recent_row_spans(
    row: &oscilloscope_core::select::RecentRow,
    selected: bool,
    zone: &dyn oscilloscope_core::format::ClockZone,
    ctx: &DrawCtx,
) -> Vec<Span<'static>> {
    let glyph = if row.gone {
        '—'
    } else if row.ok {
        '✓'
    } else {
        '✗'
    };
    let config_text = row.config_label.clone().unwrap_or_else(|| "—".to_string());
    let started =
        oscilloscope_core::format::format_clock_time(&row.started_at, zone).unwrap_or_default();
    let doc_field = format!("{:<9}", row.document_label);
    let config_field = format!("{config_text:<15}");
    let started_field = format!("{started:<10}");
    let tokens_text = oscilloscope_core::format::format_tokens(row.tokens);
    let duration_text = oscilloscope_core::format::format_duration(row.took_s);
    let took_tokens_field = format!("{duration_text:<5}{tokens_text:>7} ");
    if selected {
        // The frame's own first and last content columns stay
        // border-coloured even on the selected row (`design/frames/
        // select--empty--80x24`'s own trailing `{border.focus} │{/}`)
        // -- `selection.bg` covers only the 76 columns in between, so
        // the OUTCOME column is padded to 24, not the full remaining
        // width, and neither this span nor any other reaches column 0
        // or the very last one.
        let outcome_field = format!("{:<24}", row.outcome_label);
        let sel_bg = ctx.theme.selection_bg;
        let glyph_color = if row.gone {
            ctx.theme.fg_muted
        } else if row.ok {
            ctx.theme.status_done
        } else {
            ctx.theme.status_failed
        };
        vec![
            Span::styled(" ".to_string(), Style::default().fg(ctx.theme.text)),
            Span::styled(
                " ❯ ".to_string(),
                Style::default()
                    .fg(ctx.theme.accent_primary)
                    .bg(sel_bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{glyph} "),
                Style::default().fg(glyph_color).bg(sel_bg),
            ),
            Span::styled(
                format!(
                    "{doc_field}{config_field}{started_field}{took_tokens_field}{outcome_field}"
                ),
                Style::default().fg(ctx.theme.selection_fg).bg(sel_bg),
            ),
        ]
    } else {
        let (glyph_color, glyph_bold) = if row.gone {
            (ctx.theme.fg_muted, false)
        } else if row.ok {
            (ctx.theme.status_done, false)
        } else {
            (ctx.theme.status_failed, true)
        };
        let mut glyph_style = Style::default().fg(glyph_color);
        if glyph_bold {
            glyph_style = glyph_style.add_modifier(Modifier::BOLD);
        }
        vec![
            Span::styled("    ".to_string(), Style::default().fg(ctx.theme.text)),
            Span::styled(format!("{glyph} "), glyph_style),
            Span::styled(doc_field, Style::default().fg(ctx.theme.text)),
            Span::styled(
                format!("{config_field}{started_field}"),
                Style::default().fg(ctx.theme.fg_muted),
            ),
            Span::styled(
                format!("{took_tokens_field}{}", row.outcome_label),
                Style::default().fg(ctx.theme.text),
            ),
        ]
    }
}

/// One tile of the All screen (issue #698, §2.6): the same
/// [`oscilloscope_core::multi::TileRender`] a single-run attach
/// would build from this run's own model/plan/state, plus the
/// labels the tile's own mini header band shows (the document and
/// config are muted text in the band itself, never repeated inside
/// the tile's own Run Started row -- that row carries the time and
/// engine instead).
pub struct AllTile {
    pub document_label: String,
    pub config_label: Option<String>,
    pub render: oscilloscope_core::multi::TileRender,
    /// This run's own position in launch order, across every page
    /// (never a page-relative slot index) -- a Signal tile's own
    /// sent trace keeps the same run colour wherever it's drawn.
    pub run_index: usize,
}

/// The All screen's own body (issue #698, §2.6): the header band,
/// one mini document band plus one bordered panel per tile in the
/// grid, the status line (the newest finished step across every
/// run) and the key bar. `tiles` is this page's own real tiles,
/// already in display order; `more_count` is `0` on a page with
/// nothing left to page to.
#[allow(clippy::too_many_arguments)]
pub fn draw_all(
    frame: &mut Frame,
    full_area: Rect,
    tiles: &[AllTile],
    grid: oscilloscope_core::multi::GridLayout,
    focus: usize,
    more_count: usize,
    screen: oscilloscope_core::screens::Screen,
    running_count: usize,
    status_line: Option<(&str, &oscilloscope_core::screens::SignalLastFinished)>,
    // The largest elapsed time across every run, not only this
    // page's own drawn tiles (issue #698's own header reads "2.8s"
    // for the whole grid, paged or not) -- the caller's to compute,
    // since only it still has every slot once paging has cut this
    // call's own `tiles` down to one page.
    elapsed_s: f64,
    // Shown in place of the usual status line for one redraw --
    // `u` bouncing straight back here because the terminal was too
    // small for the Combined Signal's own layout, the only caller
    // today.
    status_note: Option<&str>,
    now: &str,
    ctx: &DrawCtx,
) {
    if let Some(bg) = ctx.theme.background {
        frame.render_widget(
            Block::default().style(Style::default().bg(bg).fg(ctx.theme.text)),
            full_area,
        );
    }
    if full_area.width < oscilloscope_core::multi::TILE_MIN_WIDTH
        || full_area.height < oscilloscope_core::multi::TILE_MIN_HEIGHT + 4
    {
        draw_too_small_with_min_width(
            frame,
            full_area,
            ctx,
            oscilloscope_core::multi::TILE_MIN_WIDTH,
        );
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
    draw_all_header_band(
        frame,
        sections[0],
        screen,
        running_count,
        elapsed_s,
        now,
        ctx,
    );
    if let Some(note) = status_note {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" {note}"),
                Style::default().fg(ctx.theme.dim),
            ))),
            sections[2],
        );
    } else {
        draw_all_status_line(frame, sections[2], status_line, more_count, now, ctx);
    }
    draw_key_bar(frame, sections[3], &all_key_bar_items(), ctx);

    let body = sections[1];
    let base_row_height = body.height / grid.rows as u16;
    let row_remainder = body.height % grid.rows as u16;
    let base_col_width = body.width / grid.columns as u16;
    let col_remainder = body.width % grid.columns as u16;

    let mut y = body.y;
    for row in 0..grid.rows {
        let row_height = base_row_height
            + if row + 1 == grid.rows {
                row_remainder
            } else {
                0
            };
        let mut x = body.x;
        for col in 0..grid.columns {
            let col_width = base_col_width
                + if col + 1 == grid.columns {
                    col_remainder
                } else {
                    0
                };
            let cell = Rect {
                x,
                y,
                width: col_width,
                height: row_height,
            };
            let index = row * grid.columns + col;
            let cell_sections = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1),
                    Constraint::Min(oscilloscope_core::multi::TILE_MIN_HEIGHT),
                ])
                .split(cell);
            if let Some(tile) = tiles.get(index) {
                draw_all_tile_band(frame, cell_sections[0], tile, ctx);
                draw_all_tile_panel(frame, cell_sections[1], tile, index == focus, now, ctx);
            } else if index + 1 == grid.slots() && more_count > 0 {
                draw_all_more_tile(frame, cell_sections[1], more_count, ctx);
            }
            x += col_width;
        }
        y += row_height;
    }
}

fn all_key_bar_items() -> Vec<(&'static str, &'static str)> {
    vec![
        ("0-3", "Screens"),
        ("←→↑↓", "Focus"),
        ("enter", "Open"),
        ("u", "Combined Signal"),
        ("esc", "Select"),
        ("?", "Help"),
        ("q", "Quit"),
    ]
}

/// The All screen's own header band (§2.6/§3.1): `osi  All ·
/// ● 6 Running · 2.8s`, the `ALL · <screen>` chip and the clock.
fn draw_all_header_band(
    frame: &mut Frame,
    area: Rect,
    screen: oscilloscope_core::screens::Screen,
    running_count: usize,
    elapsed_s: f64,
    now: &str,
    ctx: &DrawCtx,
) {
    let bg = ctx.theme.statusbar_bg;
    let base = Style::default().bg(bg).fg(ctx.theme.statusbar_fg);
    let mut left = vec![
        Span::styled(
            " osi  ",
            Style::default()
                .fg(ctx.theme.accent_primary)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("All ", Style::default().fg(ctx.theme.text).bg(bg)),
        Span::styled("· ", Style::default().fg(ctx.theme.dim).bg(bg)),
        Span::styled(
            "● ",
            Style::default()
                .fg(ctx.theme.accent_primary)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{running_count} Running "),
            Style::default().fg(ctx.theme.text).bg(bg),
        ),
        Span::styled("· ", Style::default().fg(ctx.theme.dim).bg(bg)),
        Span::styled(
            oscilloscope_core::format::format_duration(elapsed_s),
            Style::default().fg(ctx.theme.text).bg(bg),
        ),
    ];
    let banner_text = ctx
        .theme_banner
        .map(|name| format!("theme: {name}  "))
        .unwrap_or_default();
    let left_width: usize = left.iter().map(|s| s.content.chars().count()).sum();
    let clock_text =
        oscilloscope_core::format::format_clock_time(now, ctx.zone).unwrap_or_default();
    let chip_text = format!("  ALL · {}  ", all_screen_chip_word(screen));
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

fn all_screen_chip_word(screen: oscilloscope_core::screens::Screen) -> &'static str {
    match screen {
        oscilloscope_core::screens::Screen::Tape => "TAPE",
        oscilloscope_core::screens::Screen::Signal => "SIGNAL",
        oscilloscope_core::screens::Screen::Flow => "FLOW",
        oscilloscope_core::screens::Screen::Store => "STORE",
    }
}

/// The status line (§3.2): the newest finished step across every
/// run, `None` with nothing finished anywhere yet. At a one-cell
/// grid, where the extra runs have no `… n more` slot of their own
/// to draw in, their count sits here instead, trailing whatever
/// else the line already shows.
fn draw_all_status_line(
    frame: &mut Frame,
    area: Rect,
    status_line: Option<(&str, &oscilloscope_core::screens::SignalLastFinished)>,
    more_count: usize,
    now: &str,
    ctx: &DrawCtx,
) {
    let more_text = if more_count > 0 {
        format!("… {more_count} more · PageDown")
    } else {
        String::new()
    };
    let Some((document, last)) = status_line else {
        if more_text.is_empty() {
            frame.render_widget(Paragraph::new(Line::from("")), area);
        } else {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!(" {more_text}"),
                    Style::default().fg(ctx.theme.dim),
                ))),
                area,
            );
        }
        return;
    };
    let glyph = tape_glyph(last.kind, last.status);
    let age = last
        .ts
        .as_deref()
        .and_then(|t| oscilloscope_core::observe::duration_seconds(t, now));
    let age_text = age
        .map(|a| format!(" · {} ago", oscilloscope_core::format::format_duration(a)))
        .unwrap_or_default();
    let trailer = if more_text.is_empty() {
        String::new()
    } else {
        format!(" · {more_text}")
    };
    let spans = vec![
        Span::styled(
            format!(" {glyph} "),
            tape_glyph_style(last.kind, last.status, ctx.theme),
        ),
        Span::styled(
            format!("{document} · {} finished", last.label),
            Style::default().fg(ctx.theme.text),
        ),
        Span::styled(
            format!("{age_text}{trailer}"),
            Style::default().fg(ctx.theme.dim),
        ),
    ];
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// One tile's own mini document band (§2.6): `triage.yml
/// config-triage.json · ● 2.2s   cof`, on the statusbar colours, the
/// same band style the main header uses but scoped to one run.
fn draw_all_tile_band(frame: &mut Frame, area: Rect, tile: &AllTile, ctx: &DrawCtx) {
    let bg = ctx.theme.statusbar_bg;
    let base = Style::default().bg(bg).fg(ctx.theme.statusbar_fg);
    let header = &tile.render.header;
    let (glyph, glyph_color) = match header.status {
        oscilloscope_core::model::RunStatus::Running => ('●', ctx.theme.accent_primary),
        oscilloscope_core::model::RunStatus::Ok => ('✓', ctx.theme.status_done),
        _ => ('✗', ctx.theme.status_failed),
    };
    let config_text = tile
        .config_label
        .as_deref()
        .map(|c| format!("{c} "))
        .unwrap_or_default();
    let mut left = vec![
        Span::styled(
            format!(" {} ", tile.document_label),
            Style::default()
                .fg(ctx.theme.text)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(config_text, Style::default().fg(ctx.theme.fg_muted).bg(bg)),
        Span::styled("· ", Style::default().fg(ctx.theme.dim).bg(bg)),
        Span::styled(
            format!("{glyph} "),
            Style::default()
                .fg(glyph_color)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            oscilloscope_core::format::format_duration(header.elapsed_s),
            Style::default().fg(ctx.theme.text).bg(bg),
        ),
    ];
    let engine_word = header
        .engine
        .split_whitespace()
        .next()
        .unwrap_or(&header.engine)
        .to_string();
    let left_width: usize = left.iter().map(|s| s.content.chars().count()).sum();
    let right_text = format!("{engine_word} ");
    let pad = (area.width as usize).saturating_sub(left_width + right_text.chars().count());
    left.push(Span::styled(" ".repeat(pad), base));
    left.push(Span::styled(right_text, base));
    frame.render_widget(Paragraph::new(Line::from(left)).style(base), area);
}

/// A `… n more` tile (§9.6): the grid's own last slot once a page
/// can't show every running run at once.
fn draw_all_more_tile(frame: &mut Frame, area: Rect, more_count: usize, ctx: &DrawCtx) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(ratatui::symbols::border::ROUNDED)
        .border_style(Style::default().fg(ctx.theme.border));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return;
    }
    fill_border_rows(frame, inner, ctx.theme.border);
    let mid = inner.y + inner.height / 2;
    let text = format!("… {more_count} more");
    let line_area = Rect {
        x: inner.x,
        y: mid,
        width: inner.width,
        height: 1,
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            text,
            Style::default().fg(ctx.theme.dim),
        )))
        .alignment(Alignment::Center),
        line_area,
    );
}

/// One tile's own panel (issue #698): a bordered box whose title is
/// the tile's own screen-specific counts
/// (`Tape · 1 Done · 1 Running · 10 Ahead · ▰ = 50 t`), dispatched to
/// whichever of the four screens `screen` names -- the same shared
/// `0`-`3` every tile draws at once.
fn draw_all_tile_panel(
    frame: &mut Frame,
    area: Rect,
    tile: &AllTile,
    focused: bool,
    now: &str,
    ctx: &DrawCtx,
) {
    match &tile.render.screen {
        oscilloscope_core::multi::TileScreen::Tape(s) => {
            draw_all_tape_tile(frame, area, s, &tile.render.header, focused, ctx)
        }
        oscilloscope_core::multi::TileScreen::Signal(s) => draw_all_signal_tile(
            frame,
            area,
            s,
            &tile.render.header,
            focused,
            tile.run_index,
            ctx,
        ),
        oscilloscope_core::multi::TileScreen::Flow(s) => {
            draw_all_flow_tile(frame, area, s, &tile.render.header, focused, now, ctx)
        }
        oscilloscope_core::multi::TileScreen::Store(s) => {
            draw_all_store_tile(frame, area, s, &tile.render.header, focused, ctx)
        }
    }
}

/// A Tape tile's own content: Run Started (time and engine, not the
/// document -- already in the band above), the finished rows (the
/// oldest folding into `… n earlier`), the Now band, the running
/// rows, the slots rule, the rows ahead (folding into `… n more`),
/// Run End, and the Totals row pinned above the bottom border --
/// never the clock column or a detail pane (issue #698, Settled
/// point 5). `content` is the panel's own interior shrunk by one
/// more column on each side (the frames' own gutter around every
/// row but the rule lines and the Totals row, which already carry
/// their own).
fn draw_all_tape_tile(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::screens::TapeScreen,
    header: &oscilloscope_core::render::Header,
    focused: bool,
    ctx: &DrawCtx,
) {
    let border_color = if focused {
        ctx.theme.border_focused
    } else {
        ctx.theme.border
    };
    let title = format!(
        "Tape · {} Done · {} Running · {} Ahead · ▰ = {} t",
        screen.done, screen.running, screen.ahead, screen.unit
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(ratatui::symbols::border::ROUNDED)
        .title(tape_block_title(&title, border_color, ctx))
        .border_style(Style::default().fg(border_color));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height < 3 {
        fill_border_rows(frame, inner, border_color);
        return;
    }
    fill_border_rows(frame, inner, border_color);
    let content = Rect {
        x: inner.x + 1,
        y: inner.y,
        width: inner.width.saturating_sub(2),
        height: inner.height,
    };
    let name_width = 19usize;
    let token_width = 16usize;
    let what_width = (content.width as usize).saturating_sub(2 + name_width + token_width);

    let viewport = (inner.height as usize).saturating_sub(2);
    let folded = oscilloscope_core::multi::fold_tape_tile(&screen.items, viewport);
    // The focused tile's own first running row reads as if it were
    // selected (the frames' own convention for a tile with no real
    // cursor of its own) -- every other tile's running rows, and
    // every row once this has already fired once, read plainly.
    let mut highlighted_running_once = false;
    // Each line goes straight to its own one-row area as it's built
    // -- rule lines (the Now band, the slots rule, the head rule)
    // span the panel's own full `inner` width flush to the border,
    // unlike every other row, which sits in `content` (one column
    // narrower on each side, the frames' own gutter); a single
    // shared `Paragraph` at one width could never give the two
    // their own, different ones.
    for (i, line) in folded.iter().enumerate() {
        if i >= viewport {
            break;
        }
        let row_y = content.y + i as u16;
        let content_row = Rect {
            x: content.x,
            y: row_y,
            width: content.width,
            height: 1,
        };
        let full_row = Rect {
            x: inner.x,
            y: row_y,
            width: inner.width,
            height: 1,
        };
        use oscilloscope_core::multi::TapeTileLine;
        use oscilloscope_core::screens::TapeItem;
        match line {
            TapeTileLine::Item(TapeItem::RunStarted { ts, engine, .. }) => {
                let prefix = "■ Run Started        ";
                let used = prefix.chars().count();
                let ts_text = ts
                    .as_deref()
                    .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone))
                    .unwrap_or_default();
                let text = format!("{ts_text} · {engine}");
                let width = (content.width as usize).saturating_sub(used);
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled(prefix, Style::default().fg(ctx.theme.fg_muted)),
                        Span::styled(
                            format!("{text:<width$}"),
                            Style::default().fg(ctx.theme.dim),
                        ),
                    ])),
                    content_row,
                );
            }
            TapeTileLine::Item(TapeItem::Row(real_row)) => {
                // A tile's own ahead `if` row drops its condition
                // text (the frames: `gate               if`, never
                // `if · kind != ""` the way a single-run Tape's own
                // ahead row still reads it) -- the one row text a
                // tile narrows from what a full attach would show.
                let trimmed_what;
                let row: &oscilloscope_core::screens::TapeRow = if real_row.kind
                    == oscilloscope_core::render::RowKind::If
                    && real_row.status.kind == StatusKind::Pending
                {
                    trimmed_what = oscilloscope_core::screens::TapeRow {
                        what: "if".to_string(),
                        ..real_row.clone()
                    };
                    &trimmed_what
                } else {
                    real_row
                };
                let is_running = matches!(
                    row.status.kind,
                    StatusKind::Running | StatusKind::LikelyRunning | StatusKind::RunningOrQueued
                );
                let selected = focused && is_running && !highlighted_running_once;
                if selected {
                    highlighted_running_once = true;
                    // The one-column gutter either side of `content`
                    // (the frames' own left/right margin around every
                    // row but the rule lines) bleeds into the
                    // selection background too, for the one row that
                    // has one -- every other row leaves it as the
                    // plain border-coloured fill `fill_border_rows`
                    // already painted.
                    frame.render_widget(
                        Paragraph::new(Span::styled(
                            " ",
                            Style::default()
                                .fg(ctx.theme.accent_primary)
                                .bg(ctx.theme.selection_bg)
                                .add_modifier(Modifier::BOLD),
                        )),
                        Rect {
                            x: inner.x,
                            y: row_y,
                            width: 1,
                            height: 1,
                        },
                    );
                    frame.render_widget(
                        Paragraph::new(Span::styled(
                            " ",
                            Style::default()
                                .fg(ctx.theme.fg_muted)
                                .bg(ctx.theme.selection_bg),
                        )),
                        Rect {
                            x: inner.x + inner.width - 1,
                            y: row_y,
                            width: 1,
                            height: 1,
                        },
                    );
                }
                frame.render_widget(
                    // A tile's own width (56-60 cells) is well
                    // below the 100-column floor a single-run
                    // attach needs even to consider the wide
                    // detail layout -- `wide_detail` is false here
                    // for the same reason it is at any single-run
                    // width that narrow, so a loop row's own
                    // redundant ` · N done` gives way the same way.
                    Paragraph::new(tape_row_line(
                        row,
                        selected,
                        screen.unit,
                        false,
                        true,
                        false,
                        name_width,
                        what_width,
                        token_width,
                        border_color,
                        ctx,
                    )),
                    content_row,
                );
            }
            TapeTileLine::Item(TapeItem::NowBand { ts, elapsed_s }) => {
                let ts_text = ts
                    .as_deref()
                    .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone))
                    .unwrap_or_default();
                let label = format!(
                    "Now {ts_text} · {}",
                    oscilloscope_core::format::format_duration(*elapsed_s)
                );
                frame.render_widget(
                    Paragraph::new(tape_rule_line(
                        &label,
                        ctx.theme.accent_primary,
                        ctx.theme.accent_primary,
                        true,
                        full_row.width as usize,
                    )),
                    full_row,
                );
            }
            TapeTileLine::Item(TapeItem::SlotsRule {
                busy,
                slots,
                queued,
            }) => {
                // A tile drops the trailing `· N Pass Queued` once
                // nothing actually is (the frames: `2 of 2 Slots
                // Busy` alone, never `· 0 Pass Queued`).
                let label = if *queued > 0 {
                    format!("{busy} of {slots} Slots Busy · {queued} Pass Queued")
                } else {
                    format!("{busy} of {slots} Slots Busy")
                };
                frame.render_widget(
                    Paragraph::new(tape_rule_line(
                        &label,
                        ctx.theme.accent_primary,
                        ctx.theme.accent_primary,
                        false,
                        full_row.width as usize,
                    )),
                    full_row,
                );
            }
            TapeTileLine::Item(TapeItem::HeadRule) => {
                if let Some(spans) = shared_run_chrome_status_spans(&screen.head, header, ctx) {
                    frame.render_widget(Paragraph::new(Line::from(spans)), content_row);
                }
            }
            TapeTileLine::Item(TapeItem::RunEnd { eta_ts, .. }) => {
                let eta_text = eta_ts
                    .as_deref()
                    .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone));
                let mut text = "Run End            ".to_string();
                match eta_text {
                    Some(eta) => text.push_str(&format!("ETA {eta}")),
                    None => text.push_str("ETA — · no history yet"),
                }
                let width = (content.width as usize).saturating_sub(2);
                if text.chars().count() < width {
                    text = format!("{text:<width$}");
                }
                frame.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled("■ ", Style::default().fg(ctx.theme.fg_muted)),
                        Span::styled(text, Style::default().fg(ctx.theme.dim)),
                    ])),
                    content_row,
                );
            }
            TapeTileLine::EarlierFold(n) => {
                let text = format!("… {n} earlier");
                let width = content.width as usize;
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        format!("{text:<width$}"),
                        Style::default().fg(ctx.theme.dim),
                    ))),
                    content_row,
                );
            }
            TapeTileLine::MoreFold(n) => {
                let text = format!("… {n} more");
                let width = content.width as usize;
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        format!("{text:<width$}"),
                        Style::default().fg(ctx.theme.dim),
                    ))),
                    content_row,
                );
            }
            TapeTileLine::RunningSeparator => {
                frame.render_widget(
                    Paragraph::new(Line::from(Span::styled(
                        "━".repeat(full_row.width as usize),
                        Style::default().fg(ctx.theme.accent_primary),
                    ))),
                    full_row,
                );
            }
        }
    }

    let rule_y = inner.y + viewport as u16;
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
    let totals_area = Rect {
        x: inner.x,
        y: rule_y + 1,
        width: inner.width,
        height: 1,
    };
    draw_totals_row(
        frame,
        totals_area,
        header,
        false,
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

/// A Signal tile's own content (issue #698): the fifteen-row chart
/// on the shared scale every tile's own
/// [`oscilloscope_core::screens::SignalScreen`] has already been
/// overridden to carry, two finished-step labels and a Now pane
/// whose title carries the slots.
fn draw_all_signal_tile(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::screens::SignalScreen,
    header: &oscilloscope_core::render::Header,
    focused: bool,
    run_index: usize,
    ctx: &DrawCtx,
) {
    let border_color = if focused {
        ctx.theme.border_focused
    } else {
        ctx.theme.border
    };
    if area.height < 10 {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_set(ratatui::symbols::border::ROUNDED)
            .border_style(Style::default().fg(border_color));
        frame.render_widget(block, area);
        return;
    }
    // Issue #698, §2.6: "a fifteen-row chart" -- fixed, not grown to
    // fit the tile's own actual height the way a single-run Signal's
    // own chart pane is, so every tile's own chart reads the same
    // height regardless of how tall its own row in the grid is; the
    // Now pane underneath it takes whatever is left over.
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(15), Constraint::Min(3)])
        .split(area);
    let sent_color = ctx.theme.run_colors()[oscilloscope_core::multi::run_role_index(run_index)];
    draw_signal_chart(
        frame,
        sections[0],
        screen,
        header,
        false,
        border_color,
        Some("Signal · Sent Above · Received Below · Shared Scale"),
        oscilloscope_core::multi::format_shared_axis_label,
        sent_color,
        // A tile has no scrub of its own: its window always follows
        // Now (issue #734).
        None,
        ctx,
    );
    draw_signal_tile_now_pane(frame, sections[1], screen, ctx);
}

/// A Flow tile (issue #698): the 80-column Flow (folded lanes, no
/// Story pane), clipped to the tile -- snapshot-tested, no frame of
/// its own.
fn draw_all_flow_tile(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::screens::FlowScreen,
    header: &oscilloscope_core::render::Header,
    focused: bool,
    now: &str,
    ctx: &DrawCtx,
) {
    let border_color = if focused {
        ctx.theme.border_focused
    } else {
        ctx.theme.border
    };
    let mut app = App::new(false);
    app.selected_path = screen.selected_step.as_ref().map(|s| s.path.clone());
    draw_flow_rail_pane(
        frame,
        area,
        screen,
        header,
        &mut app,
        area.width,
        border_color,
        ctx,
        now,
    );
}

/// A Store tile (issue #698): the area diagram alone, clipped to
/// the tile -- snapshot-tested, no frame of its own.
fn draw_all_store_tile(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::store::StoreScreen,
    _header: &oscilloscope_core::render::Header,
    focused: bool,
    ctx: &DrawCtx,
) {
    let border_color = if focused {
        ctx.theme.border_focused
    } else {
        ctx.theme.border
    };
    let title = format!("Store · ▰ = {} t", screen.unit);
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
    let tiles_area = Rect {
        x: inner.x + 1,
        y: inner.y,
        width: inner.width.saturating_sub(3),
        height: inner.height,
    };
    if tiles_area.width > 0 && tiles_area.height > 0 {
        let tiles = oscilloscope_core::store::layout_store(
            &screen.tree,
            tiles_area.x,
            tiles_area.y,
            tiles_area.width,
            tiles_area.height,
        );
        draw_store_tiles(frame, &tiles, ctx.theme);
    }
}

/// The All screen's own Help overlay (§4): exactly the All key bar's
/// own keys plus the globals, the same shape [`draw_select_help`]
/// already follows.
pub fn draw_all_help(frame: &mut Frame, ctx: &DrawCtx) {
    let area = frame.area();
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
        .title(tape_block_title("Help · All", border, ctx))
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
    let key_style = Style::default()
        .fg(ctx.theme.keyhint_key)
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(ctx.theme.keyhint_desc);
    let row = |k: &str, d: &str| {
        Line::from(vec![
            Span::styled(format!("{k:<16}"), key_style),
            Span::styled(d.to_string(), desc_style),
        ])
    };
    let lines = vec![
        row("0-3", "Screens on every tile"),
        row("←→↑↓", "Move the focus"),
        row("enter", "Open the focused run"),
        row("PageUp PageDown", "Turn the page"),
        row("u", "Combined Signal"),
        row("T", "Colour theme"),
        row("c", "Inert here (no run attached to cancel)"),
        row("esc", "Back to Select"),
        row("?", "This help"),
        row("q", "Quit"),
    ];
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(ctx.theme.bg_surface)),
        content_area,
    );
}

/// The three chrome rows every leg of this screen carries
/// outside the body (the header band, the status line and the key
/// bar) -- the Combined Signal's own floor is this plus however
/// many body rows `oscilloscope_core::combined::combined_body_
/// rows_min` needs for `run_count` runs.
const COMBINED_CHROME_ROWS: u16 = 3;
/// The Combined Signal's own floor at `run_count` runs
/// (`design/tui-design.md` §5 names no narrower rule of its own
/// for this screen): grows with every run past what the chart's
/// own legend can fit without growing (issue #713) since each one
/// always keeps its own legend row. The caller checks this before
/// ever entering the screen (`u` bounces straight back to the
/// tiles below it, with a status note); [`draw_combined_signal`]
/// itself only ever falls back as far as the shared [`MIN_WIDTH`]/
/// [`MIN_HEIGHT`] floor every screen has.
pub(crate) const COMBINED_MIN_WIDTH: u16 = 120;
pub(crate) fn combined_min_height(run_count: usize) -> u16 {
    oscilloscope_core::combined::combined_body_rows_min(run_count) as u16 + COMBINED_CHROME_ROWS
}

fn combined_key_bar_items() -> Vec<(&'static str, &'static str)> {
    vec![
        ("←→", "Scrub"),
        ("n", "Now"),
        ("↑↓", "Select"),
        ("enter", "Open"),
        ("esc", "Tiles"),
        ("?", "Help"),
        ("q", "Quit"),
    ]
}

/// The Combined Signal's own header band (issue #698, `u`): `osi
/// All · ● 6 Running · 2.8s`, the `ALL · COMBINED SIGNAL` chip --
/// the same band the grid's own header draws, with its own chip
/// word and no screen-switch meaning behind `running_count` (every
/// run shown here is, by definition, still running).
fn draw_combined_header_band(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::combined::CombinedSignalScreen,
    now: &str,
    ctx: &DrawCtx,
) {
    let bg = ctx.theme.statusbar_bg;
    let base = Style::default().bg(bg).fg(ctx.theme.statusbar_fg);
    let mut left = vec![
        Span::styled(
            " osi  ",
            Style::default()
                .fg(ctx.theme.accent_primary)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("All ", Style::default().fg(ctx.theme.text).bg(bg)),
        Span::styled("· ", Style::default().fg(ctx.theme.dim).bg(bg)),
        Span::styled(
            "● ",
            Style::default()
                .fg(ctx.theme.accent_primary)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{} Running ", screen.totals.run_count),
            Style::default().fg(ctx.theme.text).bg(bg),
        ),
        Span::styled("· ", Style::default().fg(ctx.theme.dim).bg(bg)),
        Span::styled(
            oscilloscope_core::format::format_duration(screen.now_elapsed_s),
            Style::default().fg(ctx.theme.text).bg(bg),
        ),
    ];
    let banner_text = ctx
        .theme_banner
        .map(|name| format!("theme: {name}  "))
        .unwrap_or_default();
    let left_width: usize = left.iter().map(|s| s.content.chars().count()).sum();
    let clock_text =
        oscilloscope_core::format::format_clock_time(now, ctx.zone).unwrap_or_default();
    let chip_text = "  ALL · COMBINED SIGNAL  ".to_string();
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

/// The status line (§3.2): the newest finished step across every
/// run, same as the grid's own.
fn draw_combined_status_line(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::combined::CombinedSignalScreen,
    now: &str,
    ctx: &DrawCtx,
) {
    let entries: Vec<(
        String,
        Option<oscilloscope_core::screens::SignalLastFinished>,
    )> = screen
        .runs
        .iter()
        .map(|r| (r.document_label.clone(), r.last_finished.clone()))
        .collect();
    let Some((document, last)) = oscilloscope_core::multi::newest_finished_across_runs(&entries)
    else {
        frame.render_widget(Paragraph::new(Line::from("")), area);
        return;
    };
    let glyph = tape_glyph(last.kind, last.status);
    let age = last
        .ts
        .as_deref()
        .and_then(|t| oscilloscope_core::observe::duration_seconds(t, now));
    let age_text = age
        .map(|a| format!(" · {} ago", oscilloscope_core::format::format_duration(a)))
        .unwrap_or_default();
    let spans = vec![
        Span::styled(
            format!(" {glyph} "),
            tape_glyph_style(last.kind, last.status, ctx.theme),
        ),
        Span::styled(
            format!("{document} · {} finished", last.label),
            Style::default().fg(ctx.theme.text),
        ),
        Span::styled(age_text, Style::default().fg(ctx.theme.dim)),
    ];
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The Combined Signal (issue #698, `u` on All): the stacked trace
/// with its own legend and Totals row in one panel, the Per Run
/// table and Readings below it.
#[allow(clippy::too_many_arguments)]
pub fn draw_combined_signal(
    frame: &mut Frame,
    full_area: Rect,
    screen: &oscilloscope_core::combined::CombinedSignalScreen,
    cursor_run: usize,
    scrub_col: Option<usize>,
    now: &str,
    ctx: &DrawCtx,
) {
    if let Some(bg) = ctx.theme.background {
        frame.render_widget(
            Block::default().style(Style::default().bg(bg).fg(ctx.theme.text)),
            full_area,
        );
    }
    // Below the shared floor every screen needs at all, the
    // generic too-small notice shows. Below the Combined Signal's
    // own, taller floor (`COMBINED_MIN_WIDTH`/`COMBINED_MIN_HEIGHT`)
    // but at or above this one, the caller never reaches here at
    // all: it bounces back to the tiles with its own status note
    // first, the same way the single-run Signal screen bounces to
    // the Tape below 80 columns, rather than this screen drawing
    // its own too-small notice for a floor already narrower than
    // the grid it came from.
    if full_area.width < MIN_WIDTH || full_area.height < MIN_HEIGHT {
        draw_too_small_with_min_width(frame, full_area, ctx, COMBINED_MIN_WIDTH);
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
    draw_combined_header_band(frame, sections[0], screen, now, ctx);
    draw_combined_status_line(frame, sections[2], screen, now, ctx);
    draw_key_bar(frame, sections[3], &combined_key_bar_items(), ctx);

    let body = sections[1];
    // The chart (with its own legend and Totals) and the bottom
    // block (Per Run and Readings) split the body between them by
    // `oscilloscope_core::combined::combined_vertical_split`
    // (issue #713): the chart only ever gives up rows to the
    // table, never the other way around, down to the chart's own
    // floor (every run's own legend row, the trace at its own
    // floor). Below that floor the caller already bounced back to
    // the tiles before ever drawing this screen; reaching `None`
    // here falls back the same way the shared floor does.
    let Some(split) = oscilloscope_core::combined::combined_vertical_split(
        body.height as usize,
        screen.runs.len(),
    ) else {
        draw_too_small_with_min_width(frame, full_area, ctx, COMBINED_MIN_WIDTH);
        return;
    };
    let chart_height = split.chart_rows() as u16;
    let lower_height = split.lower_rows as u16;
    let chart_area = Rect {
        x: body.x,
        y: body.y,
        width: body.width,
        height: chart_height,
    };
    let lower_area = Rect {
        x: body.x,
        y: body.y + chart_height,
        width: body.width,
        height: lower_height,
    };
    draw_combined_chart(frame, chart_area, screen, scrub_col, now, ctx, &split);
    // The frame's own 180-wide split is 136/44, not a round
    // percentage -- kept as that exact ratio rather than one that
    // only coincidentally rounds the same way at this one width.
    let table_width = (lower_area.width as u32 * 136 / 180) as u16;
    let lower = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(table_width), Constraint::Min(0)])
        .split(lower_area);
    draw_combined_per_run_table(frame, lower[0], screen, cursor_run, ctx);
    draw_combined_readings(frame, lower[1], screen, ctx);
}

fn format_seconds_1(v: f64) -> String {
    format!("{v:.1}")
}

fn combined_chart_title() -> String {
    [
        "Combined Signal",
        "Every Running Orchestration",
        "Sent Above",
        "Received Below",
        "100 ms per Column",
        "Stacked by Run",
    ]
    .join(" \u{b7} ")
}

/// The chart: stacked sent/received columns, the Now sweep,
/// the dashed axis ahead, the seconds row and, inside the same
/// panel, the legend rows and the Totals row.
fn draw_combined_chart(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::combined::CombinedSignalScreen,
    scrub_col: Option<usize>,
    now: &str,
    ctx: &DrawCtx,
    split: &oscilloscope_core::combined::CombinedVerticalSplit,
) {
    let title = combined_chart_title();
    let border_color = ctx.theme.border_focused;
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

    let sent_rows = split.sent_rows;
    let received_rows = split.received_rows;
    let chart_rows = split.trace_rows();
    if inner.height < chart_rows as u16 {
        return;
    }

    let max_doc_len = screen
        .runs
        .iter()
        .map(|r| r.document_label.chars().count())
        .max()
        .unwrap_or(0);
    let label_field_width = (max_doc_len + 1).max(5) as u16;
    let gutter: u16 = 2;
    let data_x = inner.x + label_field_width + gutter;
    // One trailing blank column past the trace's own last drawn one
    // (from the frame: the dashed axis stops one column short of
    // the panel's own right border).
    let data_width =
        (inner.width as usize).saturating_sub((label_field_width + gutter) as usize + 1);
    if data_width == 0 {
        return;
    }

    let dim = Style::default().fg(ctx.theme.dim);
    let border_style = Style::default().fg(border_color);
    let axis_row_y = inner.y + sent_rows as u16;
    {
        let buf = frame.buffer_mut();
        let axis_label = |buf: &mut ratatui::buffer::Buffer, y: u16, text: &str| {
            let pad = (label_field_width as usize).saturating_sub(text.chars().count());
            buf.set_string(inner.x, y, " ".repeat(pad), border_style);
            buf.set_string(inner.x + pad as u16, y, text, dim);
        };
        let sent_top_label =
            oscilloscope_core::multi::format_shared_axis_label(screen.sent_axis_top);
        let sent_half_label =
            oscilloscope_core::multi::format_shared_axis_label(screen.sent_axis_top / 2);
        let received_top_label =
            oscilloscope_core::multi::format_shared_axis_label(screen.received_axis_top);
        axis_label(buf, inner.y, &sent_top_label);
        axis_label(buf, inner.y + sent_rows as u16 / 2, &sent_half_label);
        axis_label(buf, axis_row_y, "0");
        buf.set_string(data_x - 1, axis_row_y, " ", dim);
        axis_label(buf, inner.y + chart_rows as u16 - 1, &received_top_label);
    }

    let now_col_abs = screen.now_col;
    let focus_col = scrub_col.unwrap_or(now_col_abs);
    // The window doesn't scroll until the focus column would sit
    // past three quarters of the way across it; once it would, the
    // window scrolls to keep the focus column pinned there instead
    // (so a run past a few seconds in keeps Now, or the scrub
    // cursor, in view rather than running off the right edge) --
    // the same rule the single-run Signal and its tiles already use
    // ([`trace_window_start`]).
    let start_col = trace_window_start(focus_col, data_width);
    let sweep_col = now_col_abs
        .checked_sub(start_col)
        .filter(|&c| c < data_width);
    let cursor_col = scrub_col
        .and_then(|c| c.checked_sub(start_col))
        .filter(|&c| c < data_width);

    let run_colors = ctx.theme.run_colors();
    let now_sweep_style = Style::default()
        .fg(ctx.theme.accent_primary)
        .add_modifier(Modifier::BOLD);

    for col in 0..data_width {
        let abs_col = start_col + col;
        let x = data_x + col as u16;
        let sent_values: Vec<u64> = screen
            .runs
            .iter()
            .map(|r| r.buckets.sent.get(&abs_col).copied().unwrap_or(0))
            .collect();
        let sent_cells = oscilloscope_core::combined::stack_column(
            &sent_values,
            screen.sent_axis_top,
            sent_rows,
            oscilloscope_core::combined::sent_partial_glyph,
        );
        for (row_idx, cell) in sent_cells.iter().enumerate() {
            let y = inner.y + row_idx as u16;
            draw_combined_chart_cell(
                frame,
                x,
                y,
                *cell,
                Some(col) == sweep_col,
                run_colors,
                now_sweep_style,
                border_color,
            );
        }
        let received_values: Vec<u64> = screen
            .runs
            .iter()
            .map(|r| r.buckets.received.get(&abs_col).copied().unwrap_or(0))
            .collect();
        let mut received_cells = oscilloscope_core::combined::stack_column(
            &received_values,
            screen.received_axis_top,
            received_rows,
            oscilloscope_core::combined::received_partial_glyph,
        );
        received_cells.reverse();
        for (row_idx, cell) in received_cells.iter().enumerate() {
            let y = axis_row_y + 1 + row_idx as u16;
            draw_combined_chart_cell(
                frame,
                x,
                y,
                *cell,
                Some(col) == sweep_col,
                run_colors,
                now_sweep_style,
                border_color,
            );
        }
        let ahead = sweep_col.is_some_and(|s| col > s);
        let (ch, style) = if Some(col) == sweep_col {
            ('\u{2503}', now_sweep_style)
        } else if ahead {
            ('\u{254c}', dim)
        } else {
            ('\u{2500}', Style::default().fg(ctx.theme.border))
        };
        frame
            .buffer_mut()
            .set_string(x, axis_row_y, ch.to_string(), style);
    }

    if let Some(c) = cursor_col {
        if Some(c) != sweep_col {
            let cursor_rect = Rect {
                x: data_x + c as u16,
                y: inner.y,
                width: 1,
                height: chart_rows as u16,
            };
            frame
                .buffer_mut()
                .set_style(cursor_rect, Style::default().bg(ctx.theme.selection_bg));
        }
    }

    let seconds_row_y = axis_row_y + 1 + received_rows as u16;
    draw_combined_seconds_row(
        frame,
        inner,
        seconds_row_y,
        data_x,
        data_width,
        start_col,
        screen,
        now,
        ctx,
    );

    let legend_y = seconds_row_y + 1;
    draw_combined_legend(
        frame,
        Rect {
            x: inner.x,
            y: legend_y,
            width: inner.width,
            height: split.legend_rows as u16,
        },
        screen,
        data_x,
        start_col,
        sweep_col,
        ctx,
    );

    let totals_y = inner.y + inner.height - 1;
    if split.separator_rows > 0 {
        let sep_y = totals_y - 1;
        frame.buffer_mut().set_string(
            inner.x,
            sep_y,
            "\u{2500}".repeat(inner.width as usize),
            Style::default().fg(ctx.theme.border),
        );
    }
    draw_combined_totals_row(
        frame,
        Rect {
            x: inner.x,
            y: totals_y,
            width: inner.width,
            height: 1,
        },
        screen,
        ctx,
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_combined_chart_cell(
    frame: &mut Frame,
    x: u16,
    y: u16,
    cell: Option<oscilloscope_core::combined::StackedCell>,
    at_sweep: bool,
    run_colors: [Color; 6],
    now_sweep_style: Style,
    border_color: Color,
) {
    if let Some(c) = cell {
        let color = run_colors[oscilloscope_core::multi::run_role_index(c.run_index)];
        frame
            .buffer_mut()
            .set_string(x, y, c.glyph.to_string(), Style::default().fg(color));
    } else if at_sweep {
        frame
            .buffer_mut()
            .set_string(x, y, "\u{2503}", now_sweep_style);
    } else {
        frame
            .buffer_mut()
            .set_string(x, y, " ", Style::default().fg(border_color));
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_combined_seconds_row(
    frame: &mut Frame,
    inner: Rect,
    y: u16,
    data_x: u16,
    data_width: usize,
    start_col: usize,
    screen: &oscilloscope_core::combined::CombinedSignalScreen,
    now: &str,
    ctx: &DrawCtx,
) {
    let dim = Style::default().fg(ctx.theme.dim);
    frame.buffer_mut().set_string(
        inner.x,
        y,
        " ".repeat(inner.width as usize),
        Style::default().fg(ctx.theme.border),
    );
    let now_rel = screen.now_col.checked_sub(start_col);
    let clock_text =
        oscilloscope_core::format::format_clock_time(now, ctx.zone).unwrap_or_default();
    let now_label = format!(
        "Now {clock_text} \u{b7} {}",
        oscilloscope_core::format::format_duration(screen.now_elapsed_s)
    );
    // The label starts one column before the sweep itself, not at
    // it (from the frame).
    let now_span = now_rel.map(|r| {
        let start = r.saturating_sub(1);
        (start, start + now_label.chars().count())
    });
    for sec in (start_col / 10)..=((start_col + data_width) / 10) {
        let col = sec * 10;
        if col < start_col || col - start_col >= data_width {
            continue;
        }
        let tick_rel = col - start_col;
        let label = format!("{sec}s");
        let tick_len = label.chars().count();
        if tick_rel + tick_len > data_width {
            continue;
        }
        // At least one blank column always separates a tick from
        // the Now label -- a tick merely not overlapping it still
        // runs straight into it with no gap at all otherwise.
        if let Some((start, end)) = now_span {
            if tick_rel + tick_len >= start && tick_rel <= end {
                continue;
            }
        }
        frame
            .buffer_mut()
            .set_string(data_x + tick_rel as u16, y, &label, dim);
    }
    if let Some((start, _)) = now_span {
        if start < data_width {
            frame.buffer_mut().set_string(
                data_x + start as u16,
                y,
                &now_label,
                Style::default()
                    .fg(ctx.theme.accent_primary)
                    .add_modifier(Modifier::BOLD),
            );
        }
    }
}

/// One legend row per run: `\u{25a0} <doc>`, the finished-prompt
/// label at its own column (the token figure the first thing cut
/// when it would run into the running label), and the running
/// label fixed at the Now column.
#[allow(clippy::too_many_arguments)]
fn draw_combined_legend(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::combined::CombinedSignalScreen,
    data_x: u16,
    start_col: usize,
    sweep_col: Option<usize>,
    ctx: &DrawCtx,
) {
    if area.height == 0 {
        return;
    }
    let run_colors = ctx.theme.run_colors();
    let running_col = sweep_col.map(|c| data_x as usize + c + 2);
    let muted = Style::default().fg(ctx.theme.fg_muted);
    let running_style = Style::default()
        .fg(ctx.theme.accent_primary)
        .add_modifier(Modifier::BOLD);
    for (i, run) in screen.runs.iter().enumerate() {
        if i as u16 >= area.height {
            break;
        }
        let y = area.y + i as u16;
        let color = run_colors[oscilloscope_core::multi::run_role_index(run.role)];
        let buf = frame.buffer_mut();
        buf.set_string(area.x, y, "\u{25a0} ", Style::default().fg(color));
        buf.set_string(area.x + 2, y, &run.document_label, muted);
        if let (Some(label), Some(col)) = (&run.finished_label, run.finished_col) {
            if let Some(rel) = col.checked_sub(start_col) {
                let x = data_x as usize + rel;
                let limit = running_col.unwrap_or(area.x as usize + area.width as usize);
                let text = oscilloscope_core::combined::finished_legend_text(
                    label,
                    run.finished_extra,
                    run.tokens_total,
                    x,
                    limit,
                );
                frame
                    .buffer_mut()
                    .set_string(x as u16, y, &text, Style::default().fg(color));
            }
        }
        if let Some(running_col) = running_col {
            if let Some(first) = run.running.first() {
                let extra = run.running.len() - 1;
                let suffix = if extra > 0 {
                    format!(" +{extra}")
                } else {
                    String::new()
                };
                // Cut at the legend's own right edge, so a long step
                // name never runs over the panel's border.
                let right = area.x as usize + area.width as usize;
                if running_col < right {
                    let room = right - running_col;
                    let full = format!("\u{25cf} {first}{suffix}");
                    let text = if full.chars().count() <= room {
                        full
                    } else {
                        let cut: String = full.chars().take(room.saturating_sub(1)).collect();
                        format!("{cut}\u{22ef}")
                    };
                    frame
                        .buffer_mut()
                        .set_string(running_col as u16, y, text, running_style);
                }
            }
        }
    }
}

/// The cross-run Totals row.
fn draw_combined_totals_row(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::combined::CombinedSignalScreen,
    ctx: &DrawCtx,
) {
    let t = &screen.totals;
    let quiet_text = t
        .quiet_s
        .map(|q| format!("{}s", format_seconds_1(q)))
        .unwrap_or_else(|| "\u{2014}".to_string());
    let sent_text = oscilloscope_core::format::format_tokens_arrow(t.sent);
    let received_text = oscilloscope_core::format::format_tokens_arrow(t.received);
    let total_text = oscilloscope_core::format::format_tokens(t.total);
    let peak_text = oscilloscope_core::format::format_tokens(t.peak_tokens);
    let peak_time_text = format_seconds_1(t.peak_time_s);
    let rate_text = oscilloscope_core::format::format_tokens(t.rate_tokens_per_s.round() as u64);
    let spans = vec![
        Span::styled(" ", Style::default().fg(ctx.theme.text)),
        Span::styled(
            "Totals ",
            Style::default()
                .fg(ctx.theme.text)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("\u{2191}{sent_text} \u{2193}{received_text} \u{b7} "),
            Style::default().fg(ctx.theme.text),
        ),
        Span::styled(
            format!("{total_text}  "),
            Style::default()
                .fg(ctx.theme.text)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("across {} runs  Peak ", t.run_count),
            Style::default().fg(ctx.theme.fg_muted),
        ),
        Span::styled(format!("{peak_text} "), Style::default().fg(ctx.theme.text)),
        Span::styled(
            format!("at {peak_time_text}s  Rate "),
            Style::default().fg(ctx.theme.fg_muted),
        ),
        Span::styled(
            format!("{rate_text}/s  "),
            Style::default().fg(ctx.theme.text),
        ),
        Span::styled("Quiet ", Style::default().fg(ctx.theme.fg_muted)),
        Span::styled(quiet_text, Style::default().fg(ctx.theme.text)),
    ];
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The Per Run table's own left-aligned columns, each an offset
/// from the panel's own `inner.x` -- derived from the frame itself,
/// not from a guessed field width.
const PER_RUN_NAME_COL: u16 = 5;
const PER_RUN_CONFIG_COL: u16 = 20;
const PER_RUN_ENGINE_COL: u16 = 43;
const PER_RUN_NOW_COL: u16 = 55;
/// `TOKENS`/`SHARE`/`DONE` are right-aligned instead, each this
/// many columns in from the panel's own right edge -- derived from
/// the 136-wide frame (`inner.width` 134 there) so they scale with
/// the table's own width rather than sitting at the offsets that
/// one width alone happened to give them.
const PER_RUN_TOKENS_RIGHT_OF_EDGE: u16 = 14;
const PER_RUN_SHARE_RIGHT_OF_EDGE: u16 = 8;
const PER_RUN_DONE_RIGHT_OF_EDGE: u16 = 1;
/// The widest `TOKENS` text this screen ever draws (`"999,999 t"`),
/// reserved so the NOW column's own text never grows into it even
/// before a row's real figure is known.
const PER_RUN_TOKENS_RESERVE: u16 = 9;
const PER_RUN_NOW_GUTTER: u16 = 2;

/// `inner`'s own right-aligned column, `right_of_edge` columns in
/// from its last column.
fn per_run_right_x(inner: Rect, right_of_edge: u16) -> u16 {
    inner.x + inner.width.saturating_sub(1 + right_of_edge)
}

fn draw_combined_per_run_header(frame: &mut Frame, inner: Rect, style: Style) {
    let buf = frame.buffer_mut();
    buf.set_string(inner.x + PER_RUN_NAME_COL, inner.y, "RUN", style);
    buf.set_string(inner.x + PER_RUN_CONFIG_COL, inner.y, "CONFIG", style);
    buf.set_string(inner.x + PER_RUN_ENGINE_COL, inner.y, "ENGINE", style);
    buf.set_string(inner.x + PER_RUN_NOW_COL, inner.y, "NOW", style);
    set_right_aligned(
        buf,
        per_run_right_x(inner, PER_RUN_TOKENS_RIGHT_OF_EDGE),
        inner.y,
        "TOKENS",
        style,
    );
    set_right_aligned(
        buf,
        per_run_right_x(inner, PER_RUN_SHARE_RIGHT_OF_EDGE),
        inner.y,
        "SHARE",
        style,
    );
    set_right_aligned(
        buf,
        per_run_right_x(inner, PER_RUN_DONE_RIGHT_OF_EDGE),
        inner.y,
        "DONE",
        style,
    );
}

/// `text` right-aligned so its own last character lands at `right_x`.
fn set_right_aligned(
    buf: &mut ratatui::buffer::Buffer,
    right_x: u16,
    y: u16,
    text: &str,
    style: Style,
) {
    let start = right_x.saturating_sub(text.chars().count() as u16 - 1);
    buf.set_string(start, y, text, style);
}

/// The Per Run table: one row per run in launch order, a
/// `\u{276f}` cursor on the selected one.
fn draw_combined_per_run_table(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::combined::CombinedSignalScreen,
    cursor_run: usize,
    ctx: &DrawCtx,
) {
    let border_color = ctx.theme.border;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(ratatui::symbols::border::ROUNDED)
        .title(tape_block_title("Per Run", border_color, ctx))
        .border_style(Style::default().fg(border_color));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return;
    }
    fill_border_rows(frame, inner, border_color);
    let header_style = Style::default()
        .fg(ctx.theme.text)
        .add_modifier(Modifier::BOLD);
    draw_combined_per_run_header(frame, inner, header_style);
    let run_colors = ctx.theme.run_colors();
    // NOW's own preferred width (from the frame: wide enough for
    // `classify.yml`'s own two running steps, narrow enough that
    // `review.yml`'s own three still cut to two plus `+1`) --
    // capped by whatever room the table's own current width still
    // leaves before TOKENS' widest text could begin, less a
    // gutter, so a narrower table shrinks it instead of letting it
    // run into TOKENS/SHARE/DONE's own right-aligned columns.
    const PER_RUN_NOW_PREFERRED_WIDTH: u16 = 40;
    let tokens_left_edge = per_run_right_x(inner, PER_RUN_TOKENS_RIGHT_OF_EDGE)
        .saturating_sub(PER_RUN_TOKENS_RESERVE.saturating_sub(1));
    let available_gap = tokens_left_edge
        .saturating_sub(PER_RUN_NOW_GUTTER)
        .saturating_sub(inner.x + PER_RUN_NOW_COL);
    let now_width = available_gap.min(PER_RUN_NOW_PREFERRED_WIDTH) as usize;
    // The window scrolls to keep the cursor in view once the runs
    // outgrow the table's own rows, the same way the Store's own
    // Largest table already does: the selected row sits at the
    // window's own bottom edge the moment it would otherwise scroll
    // off, rather than the table always starting at the first run
    // and leaving the cursor nowhere on screen.
    let max_rows = inner.height.saturating_sub(1) as usize;
    let max_start = screen.runs.len().saturating_sub(max_rows);
    let start = cursor_run
        .saturating_sub(max_rows.saturating_sub(1))
        .min(max_start);
    for (i, run) in screen.runs.iter().enumerate().skip(start).take(max_rows) {
        let y = inner.y + 1 + (i - start) as u16;
        if y >= inner.y + inner.height {
            break;
        }
        let selected = i == cursor_run;
        let color = run_colors[oscilloscope_core::multi::run_role_index(run.role)];
        if selected {
            frame.buffer_mut().set_style(
                Rect {
                    x: inner.x,
                    y,
                    width: inner.width,
                    height: 1,
                },
                Style::default().bg(ctx.theme.selection_bg),
            );
        }
        // The selected row reads in one flat colour (`selection.fg`)
        // throughout -- never the unselected row's own text/muted
        // split (from the frame).
        let row_style = |natural: Color| {
            let fg = if selected {
                ctx.theme.selection_fg
            } else {
                natural
            };
            let base = Style::default().fg(fg);
            if selected {
                base.bg(ctx.theme.selection_bg)
            } else {
                base
            }
        };
        let buf = frame.buffer_mut();
        let cell_bg = |base: Style| {
            if selected {
                base.bg(ctx.theme.selection_bg)
            } else {
                base
            }
        };
        let cursor = if selected { "\u{276f}" } else { " " };
        // The cursor and swatch keep the run's own colour even when
        // selected -- only the rest of the row switches to
        // `selection.fg` (from the frame).
        buf.set_string(
            inner.x + 1,
            y,
            cursor,
            cell_bg(Style::default().fg(color)).add_modifier(Modifier::BOLD),
        );
        buf.set_string(
            inner.x + 3,
            y,
            "\u{25a0}",
            cell_bg(Style::default().fg(color)),
        );
        buf.set_string(
            inner.x + PER_RUN_NAME_COL,
            y,
            &run.document_label,
            row_style(ctx.theme.text),
        );
        let config_text = run.config_label.as_deref().unwrap_or("\u{2014}");
        buf.set_string(
            inner.x + PER_RUN_CONFIG_COL,
            y,
            config_text,
            row_style(ctx.theme.fg_muted),
        );
        // The engine reads in the same muted span as the config
        // (from the frame), not the text colour the NOW/TOKENS/DONE
        // columns use.
        buf.set_string(
            inner.x + PER_RUN_ENGINE_COL,
            y,
            &run.engine_label,
            row_style(ctx.theme.fg_muted),
        );
        let now_text =
            oscilloscope_core::combined::join_steps_with_overflow(&run.running, now_width);
        buf.set_string(
            inner.x + PER_RUN_NOW_COL,
            y,
            &now_text,
            row_style(ctx.theme.text),
        );
        let tokens_text = oscilloscope_core::format::format_tokens(run.tokens_total);
        set_right_aligned(
            buf,
            per_run_right_x(inner, PER_RUN_TOKENS_RIGHT_OF_EDGE),
            y,
            &tokens_text,
            row_style(ctx.theme.text),
        );
        let share_text = oscilloscope_core::combined::format_share(run.share_pct);
        set_right_aligned(
            buf,
            per_run_right_x(inner, PER_RUN_SHARE_RIGHT_OF_EDGE),
            y,
            &share_text,
            row_style(ctx.theme.fg_muted),
        );
        let done_text = match run.effects_total {
            Some(total) => format!("{}/{total}", run.effects_done),
            None => run.effects_done.to_string(),
        };
        set_right_aligned(
            buf,
            per_run_right_x(inner, PER_RUN_DONE_RIGHT_OF_EDGE),
            y,
            &done_text,
            row_style(ctx.theme.text),
        );
    }
}

/// Readings: Runs, Steps, Largest, Scale -- the label field
/// always leaves one real space before the value, even when the
/// label itself is longer than the padding width (`Largest`, seven
/// characters against a width of six).
fn draw_combined_readings(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::combined::CombinedSignalScreen,
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
    if inner.height == 0 {
        return;
    }
    fill_border_rows(frame, inner, border_color);
    let muted = Style::default().fg(ctx.theme.fg_muted);
    let text_style = Style::default().fg(ctx.theme.text);
    let bold = Style::default()
        .fg(ctx.theme.text)
        .add_modifier(Modifier::BOLD);
    let r = &screen.readings;
    let mut lines: Vec<Line> = vec![
        Line::from(vec![
            Span::styled(format!("{:<6} ", "Runs"), muted),
            Span::styled(
                format!(
                    "{} running \u{b7} {} engine{}",
                    r.run_count,
                    r.engine_count,
                    if r.engine_count == 1 { "" } else { "s" }
                ),
                text_style,
            ),
        ]),
        Line::from(vec![
            Span::styled(format!("{:<6} ", "Steps"), muted),
            Span::styled(format!("{} running now", r.steps_running), text_style),
        ]),
    ];
    if let Some(doc) = &r.largest_doc {
        let share_text = oscilloscope_core::combined::format_share(r.largest_share_pct);
        let tokens_text = oscilloscope_core::format::format_tokens(r.largest_tokens);
        lines.push(Line::from(vec![
            Span::styled(format!("{:<6} ", "Largest"), muted),
            Span::styled(format!("{doc} "), text_style),
            Span::styled(format!("{tokens_text} "), bold),
            Span::styled(format!("\u{b7} {share_text}"), muted),
        ]));
    }
    // `format_shared_axis_label` already carries its own ` t` unit
    // below 1,000 (`400 t`) but not at or above it (`8.0k`) -- the
    // number alone here, with ` t` added back just once.
    let scale_number = oscilloscope_core::multi::format_shared_axis_label(screen.sent_axis_top);
    let scale_number = scale_number.strip_suffix(" t").unwrap_or(&scale_number);
    lines.push(Line::from(vec![
        Span::styled(format!("{:<6} ", "Scale"), muted),
        Span::styled(
            format!("{scale_number} t per column \u{b7} shared"),
            text_style,
        ),
    ]));
    frame.render_widget(
        // A narrower panel than the golden's own (below 180 columns)
        // wraps a line too long for it rather than truncating it --
        // the panel has rows to spare for a wrapped line long before
        // it runs out of the four lines themselves (issue #713), and
        // the golden's own width never wraps any of them.
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        Rect {
            x: inner.x + 1,
            y: inner.y,
            width: inner.width.saturating_sub(1),
            height: inner.height,
        },
    );
}

/// The Combined Signal's own Help overlay.
pub fn draw_combined_signal_help(frame: &mut Frame, ctx: &DrawCtx) {
    let area = frame.area();
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
        .title(tape_block_title("Help \u{b7} Combined Signal", border, ctx))
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
    let key_style = Style::default()
        .fg(ctx.theme.keyhint_key)
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(ctx.theme.keyhint_desc);
    let row = |k: &str, d: &str| {
        Line::from(vec![
            Span::styled(format!("{k:<16}"), key_style),
            Span::styled(d.to_string(), desc_style),
        ])
    };
    let lines = vec![
        row("\u{2190}\u{2192}", "Scrub the chart"),
        row("n", "Back to Now"),
        row("\u{2191}\u{2193}", "Select a run"),
        row("enter", "Open the selected run on its Signal"),
        row("T", "Colour theme"),
        row("c", "Inert here (no run attached to cancel)"),
        row("esc", "Back to the tiles"),
        row("?", "This help"),
        row("q", "Quit"),
    ];
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().bg(ctx.theme.bg_surface)),
        content_area,
    );
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
    transcript_view: Option<&oscilloscope_core::transcript::TranscriptView>,
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
    let key_bar_items = if app.tape_fullscreen_step {
        fullscreen_step_key_bar_items(full_area.width >= 80)
    } else {
        tape_key_bar_items(full_area.width >= 80)
    };
    draw_key_bar(frame, sections[3], &key_bar_items, ctx);

    let body = sections[1];
    if app.tape_fullscreen_step && screen.selected.is_some() {
        draw_tape_detail(frame, body, screen, app, transcript_view, now, ctx);
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
        draw_tape_detail(frame, cols[1], screen, app, transcript_view, now, ctx);
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
    transcript_view: Option<&oscilloscope_core::transcript::TranscriptView>,
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
    app.signal_drawn_col = screen.cursor_col;
    app.signal_has_step = screen.selected.is_some();
    // Issue #704 rule 2: free scrubbing (`app.signal_cursor_col`
    // itself `Some`) moves the shared selection to follow whatever
    // step the cursor actually lands on, the same way the Tape's
    // own `↑↓` already does -- not scrubbing, the selection already
    // drove the cursor's own column (`screens::build_signal`'s
    // `selection_col`), so this is a no-op then. Writes back through
    // `select_tree_path_from_scrub`, never `select_tree_path`, which
    // would otherwise clear `signal_cursor_col` on the very call
    // that is driven by it.
    if app.signal_cursor_col.is_some() {
        if let Some(step) = &screen.selected {
            app.select_tree_path_from_scrub(Some(step.path.clone()));
        }
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
    draw_tape_header_band(frame, sections[0], header, "SIGNAL", now, icon_frame, ctx);
    draw_signal_status_line(frame, sections[2], screen, header, ctx, now);
    let key_bar_items = if app.tape_fullscreen_step {
        fullscreen_step_key_bar_items(full_area.width >= 80)
    } else {
        signal_key_bar_items(full_area.width >= 80)
    };
    draw_key_bar(frame, sections[3], &key_bar_items, ctx);

    let body = sections[1];
    if app.tape_fullscreen_step && screen.selected.is_some() {
        draw_focused_step_detail(
            frame,
            body,
            screen.selected.as_ref(),
            transcript_view,
            now,
            app,
            ctx,
        );
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
    draw_signal_chart(
        frame,
        rows_split[0],
        screen,
        header,
        wide,
        ctx.theme.border_focused,
        None,
        oscilloscope_core::multi::format_shared_axis_label,
        ctx.theme.accent_primary,
        // The real scrub cursor (issue #734): `None` while following
        // Now, never `screen.cursor_col`, which the shared tree
        // selection can hold onto long after the trace has moved on.
        app.signal_cursor_col,
        ctx,
    );
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
                oscilloscope_core::format::format_tokens_arrow(sent),
                oscilloscope_core::format::format_tokens_arrow(received)
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
/// solid fill below it), rounded to the nearest eighth -- `round(8
/// times frac)`, the eighths frames draw a partial bar at, never
/// `round(7 times frac)`, which reads a whole level high at several
/// fractions in the lower half, such as .26 (the frame's own `▂`,
/// not `▃`) and .375 (`▃`, not `▄`).
fn signal_sent_partial_glyph(frac: f64) -> char {
    const GLYPHS: [char; 8] = [
        '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}',
        '\u{2588}',
    ];
    let level = (frac.clamp(0.0, 1.0) * 8.0).round() as usize;
    GLYPHS[level.saturating_sub(1).min(7)]
}

/// `frac` in `(0, 1]`: Unicode names only two upper-anchored partial
/// blocks (`▔` an eighth, `▀` a half) -- a received bar's own
/// partial cell (always filled from its own top edge, continuing
/// the solid fill above it) rounds to the nearer of the two, or a
/// full block past three-quarters.
fn signal_received_partial_glyph(frac: f64) -> Option<char> {
    if frac >= 0.75 {
        Some('\u{2588}')
    } else if frac >= 0.3125 {
        Some('\u{2580}')
    } else {
        Some('\u{2594}')
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
        out[full] = signal_received_partial_glyph(remainder / unit);
    }
    out
}

const SIGNAL_SENT_ROWS: usize = 5;
const SIGNAL_RECEIVED_ROWS: usize = 2;

/// The visible window's own leftmost column (issue #734: shared by
/// the single-run Signal, its tiles and the Combined Signal, rather
/// than each copying its own version): `focus_col` (the scrub
/// cursor, or Now while following it) stays on screen, with up to a
/// quarter of `data_width` still ahead of it once the trace has run
/// longer than the pane is wide -- a run that still fits inside
/// `data_width` columns never scrolls at all, column 0 stays the
/// left edge.
fn trace_window_start(focus_col: usize, data_width: usize) -> usize {
    focus_col.saturating_sub(data_width * 3 / 4)
}

#[allow(clippy::too_many_arguments)]
fn draw_signal_chart(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::screens::SignalScreen,
    header: &oscilloscope_core::render::Header,
    wide: bool,
    border_color: Color,
    title_override: Option<&str>,
    axis_label_format: fn(u64) -> String,
    // A tile's own run colour for its sent trace (issue #698) --
    // `ctx.theme.accent_primary`, a single-run Signal's own fixed
    // colour, everywhere else.
    sent_color: Color,
    // The real scrub cursor (issue #734), never the shared tree
    // selection's own column (`screen.cursor_col`, which may follow
    // a long-finished step instead): `None` while following Now, so
    // the window keeps the Now sweep in view even when the shared
    // selection sits somewhere the trace long since scrolled past.
    // Always `None` for a tile, which has no scrub of its own.
    scrub_col: Option<usize>,
    ctx: &DrawCtx,
) {
    let title = title_override
        .map(str::to_string)
        .unwrap_or_else(|| signal_title(screen, wide));
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
    // four axis labels (`1.2k`, past three digits on a long or
    // history-heavy run, not just `600 t`), plus the one blank
    // gutter column before the trace itself starts.
    let sent_top_label = axis_label_format(screen.sent_axis_top);
    let sent_half_label = axis_label_format(screen.sent_axis_top / 2);
    let received_top_label = axis_label_format(screen.received_axis_top);
    // The sent side's own labels set the field's width (`0`, the
    // top and its own half) for a tile (`title_override.is_some()`):
    // the shared `k`-scale's own received label is never wider
    // there than the sent ones, including `400 t` against `5.0k`'s
    // four cells, so it overflows its own gutter rather than
    // widening it, same as `saturating_sub` already lets it. A
    // single-run Signal's own received label, with history, can
    // still be wider than its sent ones (`5.1k` against `300 t`),
    // so it stays in this computation there.
    let label_width = ["0", sent_top_label.as_str(), sent_half_label.as_str()]
        .iter()
        .chain(
            title_override
                .is_none()
                .then_some(&received_top_label.as_str()),
        )
        .map(|s| s.chars().count())
        .max()
        .unwrap_or(1) as u16;
    // A tile's own axis labels (identified by its own title
    // override) leave one more blank column before the data starts
    // than a single run's own axis ever needed (the frames' own
    // `all--signal--180x48`: the axis row's `0` sits one column
    // further left than its own dashes' start would otherwise put
    // it).
    let gutter = label_width + 1 + u16::from(title_override.is_some());
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

    let sent_style = Style::default().fg(sent_color);
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
    // col`, never the bare column index on its own. Issue #734: the
    // real scrub cursor, never `screen.cursor_col` (which, with no
    // scrub of its own, falls back to the shared tree selection's
    // own column -- a long-finished step's, say), or an old
    // selection parks the window there forever while Now sweeps on
    // off screen.
    let now_col_abs = screen.now_col.unwrap_or(0);
    let focus_col = scrub_col.unwrap_or(now_col_abs);
    let start_col = trace_window_start(focus_col, data_width);
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
        frame,
        inner,
        data_x,
        data_width,
        start_col,
        screen,
        wide,
        title_override.is_none(),
        ctx,
    );
    let step_rows_y = axis_row_y + 1 + SIGNAL_RECEIVED_ROWS as u16 + 1;
    let step_rows_available = (inner.y + inner.height)
        .saturating_sub(step_rows_y)
        .saturating_sub(2); // the separator and Totals rows
    if title_override.is_some() {
        draw_signal_tile_finished_labels(
            frame,
            Rect {
                x: inner.x,
                y: step_rows_y,
                width: inner.width,
                height: step_rows_available,
            },
            data_x,
            start_col,
            &screen.tile_finished_labels,
            ctx,
        );
    } else {
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
    }
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
    // A tile's own seconds row (identified the same way its own
    // extra gutter column is, by `title_override`) drops the
    // `Ahead`/`Ahead · plan order · usually ends ...` label after
    // Now entirely -- the frame leaves the rest of the row blank.
    show_ahead_label: bool,
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
    let ahead_x = if show_ahead_label {
        match (now_x, &now_label) {
            (Some(x), Some(l)) => Some(x + l.chars().count() as u16),
            _ => None,
        }
    } else {
        None
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
                        format!("{} ", oscilloscope_core::format::format_tokens(tokens)),
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

/// A Signal tile's own two rows under the chart (issue #698): its
/// two most recent finished-prompt label groups, oldest first, each
/// `✓ name [+N] · T t` in `fg.muted` -- never a single-run Signal's
/// own time-positioned step rows, which a tile has no room for.
fn draw_signal_tile_finished_labels(
    frame: &mut Frame,
    area: Rect,
    data_x: u16,
    start_col: usize,
    groups: &[oscilloscope_core::multi::SignalFinishedLabelGroup],
    ctx: &DrawCtx,
) {
    if area.height == 0 {
        return;
    }
    let style = Style::default().fg(ctx.theme.fg_muted);
    let right_edge = area.x + area.width;
    for row in 0..area.height as usize {
        let y = area.y + row as u16;
        let Some(g) = groups.get(row) else {
            continue;
        };
        // Scrolled off the window's own left edge entirely: a
        // label is a plain point-in-time mark, the same as the
        // single-run Signal's own finished-prompt step row -- it
        // just isn't there any more, never clamped to column 0 the
        // way `col.saturating_sub(start_col)` alone would for a
        // group to the left of the window.
        if g.col < start_col {
            continue;
        }
        let extra_text = if g.extra > 0 {
            format!(" +{}", g.extra)
        } else {
            String::new()
        };
        let tokens_text = format!(
            " \u{b7} {}",
            oscilloscope_core::format::format_tokens(g.tokens)
        );
        let label_x = data_x + (g.col - start_col) as u16;
        if label_x >= right_edge {
            continue;
        }
        let budget = (right_edge - label_x) as usize;
        // The frame's own form is `\u{2713} name [+N] \u{b7} T t` --
        // a budget too narrow for all of it drops the ` \u{b7} T t`
        // part first (the trace above already carries the exact
        // figure), then cuts the name itself with `\u22ef`, never
        // mid-token-count with no mark at all.
        let head = format!("\u{2713} {}{extra_text}", g.label);
        let clipped = if head.chars().count() + tokens_text.chars().count() <= budget {
            format!("{head}{tokens_text}")
        } else {
            truncate_text(&head, budget)
        };
        let width = clipped.chars().count() as u16;
        frame.buffer_mut().set_string(label_x, y, &clipped, style);
        let fill_from = label_x + width;
        if fill_from < right_edge {
            let fill = (right_edge - fill_from) as usize;
            frame
                .buffer_mut()
                .set_string(fill_from, y, " ".repeat(fill), style);
        }
    }
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

/// A Signal tile's own Now pane: the running leaf rows, then the
/// rows ahead, each a glyph, an 18-cell name field (20 together
/// with the glyph and its own trailing space, a name over 17 chars
/// cut with `⋯` so one space always separates it from the `what`
/// text that follows) and the Tape's own `what` text for it --
/// never the single-run pane's breadcrumbs or its waiting/loop-
/// summary/Last-Burst/Next-Signal sections, which a tile has no
/// room for.
fn draw_signal_tile_now_pane(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::screens::SignalScreen,
    ctx: &DrawCtx,
) {
    let border_color = ctx.theme.border;
    let title = screen
        .tile_now_slots_text
        .as_deref()
        .map(|s| format!("Now · {s}"))
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
    let running_glyph_style = Style::default()
        .fg(ctx.theme.accent_primary)
        .add_modifier(Modifier::BOLD);
    let bold = Style::default()
        .fg(ctx.theme.text)
        .add_modifier(Modifier::BOLD);
    let muted = Style::default().fg(ctx.theme.fg_muted);
    let faint = Style::default().fg(ctx.theme.dim);
    const NAME_WIDTH: usize = 18;
    let mut y = inner.y;
    let bottom = inner.y + inner.height;
    for row in &screen.tile_now_running {
        if y >= bottom {
            break;
        }
        let name = truncate_text(&row.label, NAME_WIDTH - 1);
        let line = pad_line(
            vec![
                Span::styled("● ", running_glyph_style),
                Span::styled(format!("{name:<NAME_WIDTH$}"), bold),
                Span::styled(row.what.clone(), muted),
            ],
            (inner.width - 1) as usize,
        );
        frame
            .buffer_mut()
            .set_line(inner.x + 1, y, &line, inner.width - 1);
        y += 1;
    }
    for row in &screen.tile_now_ahead {
        if y >= bottom {
            break;
        }
        let name = truncate_text(&row.label, NAME_WIDTH - 1);
        let line = pad_line(
            vec![
                Span::styled("· ", faint),
                Span::styled(format!("{name:<NAME_WIDTH$}"), faint),
                Span::styled(row.what.clone(), faint),
            ],
            (inner.width - 1) as usize,
        );
        frame
            .buffer_mut()
            .set_line(inner.x + 1, y, &line, inner.width - 1);
        y += 1;
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
    // Sent/Received draw in the faint style, and lead with `≈`, for
    // exactly as long as a live part is still in them -- the same
    // marker the Totals row and a streaming row put before an
    // estimated figure.
    let sent_style = if readings.sent_live { muted } else { default };
    let received_style = if readings.received_live {
        muted
    } else {
        default
    };
    let sent_marker = if readings.sent_live { "≈" } else { "" };
    let received_marker = if readings.received_live { "≈" } else { "" };
    let mut lines = vec![
        pad_line(
            vec![
                Span::styled("Sent     ", muted),
                Span::styled(
                    format!(
                        "{sent_marker}{}",
                        oscilloscope_core::format::format_tokens(readings.sent)
                    ),
                    sent_style,
                ),
            ],
            line_width,
        ),
        pad_line(
            vec![
                Span::styled("Received ", muted),
                Span::styled(
                    format!(
                        "{received_marker}{}",
                        oscilloscope_core::format::format_tokens(readings.received)
                    ),
                    received_style,
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
                    format!("{} ", oscilloscope_core::format::format_tokens(*tokens)),
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
                        "{label} {}",
                        oscilloscope_core::format::format_tokens(*tokens)
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
/// the cursor"), reusing the same [`oscilloscope_core::screens::
/// FocusedStep`] the Tape's own detail pane builds theirs from -- a
/// simpler block than the Tape's own (no history/share lines, since
/// neither the Signal's nor the Store's own model carries that per
/// row), good enough to read a step's prompt and reply in full
/// without leaving the screen. Shared by the Signal and the Store,
/// whose own full-screen step views are otherwise identical.
fn draw_focused_step_detail(
    frame: &mut Frame,
    area: Rect,
    selected: Option<&oscilloscope_core::screens::FocusedStep>,
    transcript_view: Option<&oscilloscope_core::transcript::TranscriptView>,
    now: &str,
    app: &mut App,
    ctx: &DrawCtx,
) {
    let Some(step) = selected else {
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
    let width = (inner.width as usize).saturating_sub(1);
    let lines = focused_step_full_lines(
        step,
        transcript_view,
        now,
        app.step_meta_unfolded,
        width,
        ctx,
    );
    app.loop_pass_line_paths = loop_pass_line_paths(&lines, step);
    let viewport = inner.height as usize;
    app.details_viewport = viewport;
    let total = lines.len();
    app.details_scroll = app
        .details_scroll
        .min(total.saturating_sub(viewport.max(1)));
    let start = app.details_scroll.min(total);
    let visible: Vec<Line> = lines.into_iter().skip(start).take(viewport).collect();
    frame.render_widget(
        Paragraph::new(visible),
        Rect {
            x: inner.x + 1,
            y: inner.y,
            width: inner.width.saturating_sub(1),
            height: inner.height,
        },
    );
}

/// Issue #704 rule 4: the full-screen step view's own content, shared
/// by the Signal's and the Store's own full-screen step (the Tape's
/// own `tape_normal_detail_lines` and the Flow's own `flow_selected_
/// step_lines` each keep their own existing Status/Asked/Produced/
/// Agent rendering, append-only, to keep their own frame goldens
/// cell-exact) -- Status, Asked, Produced, Error, Tokens, the Agent
/// block, then every section `focused_*_lines` already shares.
fn focused_step_full_lines(
    step: &oscilloscope_core::screens::FocusedStep,
    transcript_view: Option<&oscilloscope_core::transcript::TranscriptView>,
    now: &str,
    meta_unfolded: bool,
    width: usize,
    ctx: &DrawCtx,
) -> Vec<Line<'static>> {
    use oscilloscope_core::model::StatusKind;
    use oscilloscope_core::screens::FocusDetail;
    let muted = Style::default().fg(ctx.theme.fg_muted);
    let default = Style::default().fg(ctx.theme.text);
    let title_bold = Style::default()
        .fg(ctx.theme.text)
        .add_modifier(Modifier::BOLD);
    let (glyph, word, glyph_style) = tape_status_glyph_word_style(step.status.kind, ctx.theme);
    let mut status_value = String::new();
    if let Some(secs) = step.elapsed_s {
        status_value.push_str(&oscilloscope_core::format::format_duration(secs));
    }
    // Issue #704 rule 4's own Status section, "start → end": both
    // clock times when the step has actually finished, its own
    // start alone while it hasn't.
    let start_clock = step
        .since_ts
        .as_deref()
        .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone));
    let end_clock = step
        .finished_ts
        .as_deref()
        .and_then(|t| oscilloscope_core::format::format_clock_time(t, ctx.zone));
    if let Some(start) = &start_clock {
        if !status_value.is_empty() {
            status_value.push_str(" · ");
        }
        status_value.push_str(start);
        if let Some(end) = &end_clock {
            status_value.push_str(" → ");
            status_value.push_str(end);
        }
    }
    let mut lines = vec![tape_detail_line(
        vec![
            Span::styled("Status  ", muted),
            Span::styled(format!("{glyph} {word}  "), glyph_style),
            Span::styled(status_value, default),
        ],
        width,
    )];

    let asked = match &step.detail {
        FocusDetail::Prompt { .. } => step.prompt_sent_full.clone(),
        FocusDetail::Tool { command, .. } => command.clone(),
        _ => None,
    };
    if asked.is_some() || step.params_rendered_full.is_some() {
        lines.push(Line::from(""));
        lines.push(tape_detail_line(
            vec![Span::styled("Asked", title_bold)],
            width,
        ));
        if let Some(asked) = &asked {
            for line in tape_word_wrap(asked, width.saturating_sub(1)) {
                lines.push(tape_detail_line(vec![Span::styled(line, default)], width));
            }
        }
        // Issue #704 rule 4's own "rendered params": the whole
        // `meta.params_rendered` object, not only the `command`/
        // `args` a tool's own command line above already read out of
        // it -- shown even when it duplicates that line, since a non-
        // shell tool's own rendered params have no command line at
        // all to stand in for them.
        if let Some(params) = &step.params_rendered_full {
            if asked.is_some() {
                lines.push(Line::from(""));
            }
            lines.push(tape_detail_line(
                vec![Span::styled("params ", muted)],
                width,
            ));
            for line in tape_word_wrap(params, width.saturating_sub(1)) {
                lines.push(tape_detail_line(vec![Span::styled(line, default)], width));
            }
        }
    }

    if !matches!(step.detail, FocusDetail::Prompt { .. }) {
        if let Some(value) = &step.value_full {
            lines.push(Line::from(""));
            lines.push(tape_detail_line(
                vec![Span::styled("Produced", title_bold)],
                width,
            ));
            for line in tape_word_wrap(value, width.saturating_sub(1)) {
                lines.push(tape_detail_line(vec![Span::styled(line, default)], width));
            }
        }
    } else if let Some(received) = &step.received {
        if let Some(value) = &step.value_full {
            lines.push(Line::from(""));
            lines.push(tape_detail_line(
                vec![
                    Span::styled("Produced ", title_bold),
                    Span::styled(
                        received
                            .tokens
                            .map(|t| format!("· {}", oscilloscope_core::format::format_tokens(t)))
                            .unwrap_or_default(),
                        muted,
                    ),
                ],
                width,
            ));
            for line in tape_word_wrap(value, width.saturating_sub(1)) {
                lines.push(tape_detail_line(vec![Span::styled(line, default)], width));
            }
        }
    }

    if let Some(error) = &step.error {
        lines.push(Line::from(""));
        lines.push(tape_detail_line(
            vec![Span::styled("Error", title_bold)],
            width,
        ));
        for line in tape_word_wrap(error, width.saturating_sub(1)) {
            lines.push(tape_detail_line(
                vec![Span::styled(line, Style::default().fg(ctx.theme.error))],
                width,
            ));
        }
    }

    if step.tokens.is_some() || step.usual_tokens.is_some() {
        lines.push(Line::from(""));
        let mut spans = vec![Span::styled("Tokens  ", muted)];
        if let Some(total) = step.tokens {
            spans.push(Span::styled(
                oscilloscope_core::format::format_tokens(total),
                default.add_modifier(Modifier::BOLD),
            ));
        }
        if let Some(pct) = step.share_pct {
            spans.push(Span::styled(format!("  {pct}% of this run so far"), muted));
        }
        lines.push(tape_detail_line(spans, width));
        // Issue #704 rule 4's own Tokens section, "totals across
        // attempts": only shown once there actually were more than
        // one -- the figure above is already the winning attempt's
        // own (`node_tokens`' own final-state rule), never repeated
        // here.
        if step.attempts.len() > 1 {
            let attempts_total: u64 = step
                .attempts
                .iter()
                .map(|a| a.tokens_sent.unwrap_or(0) + a.tokens_received.unwrap_or(0))
                .sum();
            lines.push(tape_detail_line(
                vec![
                    Span::styled("        ", muted),
                    Span::styled(
                        oscilloscope_core::format::format_tokens(attempts_total),
                        default,
                    ),
                    Span::styled(
                        format!(" total across {} attempts", step.attempts.len()),
                        muted,
                    ),
                ],
                width,
            ));
        }
        if let Some((usual, count)) = step.usual_tokens {
            let dev = step
                .tokens
                .filter(|_| usual > 0)
                .map(|t| ((t as f64 - usual as f64) / usual as f64) * 100.0);
            let mut spans = vec![
                Span::styled("Usual   ", muted),
                Span::styled(
                    format!("{}  ", oscilloscope_core::format::format_tokens(usual)),
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
    }

    if let FocusDetail::Agent {
        stale, live, note, ..
    } = &step.detail
    {
        lines.push(Line::from(""));
        lines.push(tape_detail_line(
            vec![Span::styled("Agent", title_bold)],
            width,
        ));
        if *stale {
            lines.push(tape_detail_line(
                vec![Span::styled("stopped", muted)],
                width,
            ));
        }
        let running = matches!(
            step.status.kind,
            StatusKind::Running | StatusKind::LikelyRunning | StatusKind::RunningOrQueued
        );
        // Issue #704 rule 4: the full-screen view is scrollable (`App::
        // details_scroll`/`scroll_details`), so the transcript reads
        // in full here, never cut to fit a fixed line budget the way
        // the Tape's own narrower, un-scrolled-by-section detail pane
        // still is -- nothing the run recorded is cut.
        for line in agent_block_lines(step, transcript_view, ctx, now, None) {
            lines.push(tape_detail_line(line.spans, width));
        }
        if live.is_none() && running {
            lines.push(tape_detail_line(vec![Span::styled(*note, muted)], width));
        }
    }

    lines.extend(focused_attempts_lines(step, width, ctx));
    lines.extend(focused_tool_lines(step, width, ctx));
    lines.extend(focused_decision_lines(step, width, ctx));
    lines.extend(focused_loop_lines(step, width, ctx));
    lines.extend(focused_meta_lines(step, width, ctx, meta_unfolded));

    lines
}

/// One Agent block field line (issue #703's rule 3): an eight-cell
/// label, then the value in the pane's own default text colour.
fn agent_field_line(
    label: &'static str,
    value: String,
    label_style: Style,
    value_style: Style,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("{label:<8}"), label_style),
        Span::styled(value, value_style),
    ])
}

/// The one Agent block (issue #703's rule 3), shared by every pane
/// that shows a running or finished agent step: the Tape's own
/// detail pane (`tape_normal_detail_lines`), the full-screen step
/// (`draw_focused_step_detail`) and the Flow screen's own Selected
/// pane -- Asked, Turn, Tool, Tools, Context, Files, Tests, Said
/// (the transcript's own `last_reply`) and Cost, each left out
/// whenever the engine didn't report it, then the transcript tail
/// (newest at the bottom, a running call with its own elapsed, a
/// leading `…` once the tailed window no longer reaches the
/// session's own start). `step.detail` must be `FocusDetail::Agent`;
/// an empty `Vec` otherwise, which no caller here ever actually asks
/// for (each already matched on the same variant first).
fn agent_block_lines(
    step: &oscilloscope_core::screens::FocusedStep,
    transcript_view: Option<&oscilloscope_core::transcript::TranscriptView>,
    ctx: &DrawCtx,
    now: &str,
    max_lines: Option<usize>,
) -> Vec<Line<'static>> {
    use oscilloscope_core::format::{format_duration, format_tokens_k};
    use oscilloscope_core::screens::FocusDetail;
    let FocusDetail::Agent {
        asked,
        live,
        current_tool_elapsed_s,
        raw_turns,
        raw_tool_calls,
        raw_tool_counts,
        raw_files_changed,
        raw_lines_added,
        raw_lines_removed,
        raw_context_tokens,
        raw_context_window,
        ..
    } = &step.detail
    else {
        return Vec::new();
    };
    let muted = Style::default().fg(ctx.theme.fg_muted);
    let default = Style::default().fg(ctx.theme.text);
    let dim = Style::default().fg(ctx.theme.dim);
    let mut lines = Vec::new();

    if let Some(asked) = asked {
        lines.push(agent_field_line("Asked", asked.clone(), muted, default));
    }

    let turn_text = match live.as_deref() {
        Some(live) => {
            let mut text = if live.phase.as_deref() == Some("repair") {
                "repair".to_string()
            } else {
                live.turns.map(|t| t.to_string()).unwrap_or_default()
            };
            if let Some(attempt) = live.attempt.filter(|&a| a > 1) {
                if !text.is_empty() {
                    text.push_str(" · ");
                }
                text.push_str(&format!("attempt {attempt}"));
            }
            (!text.is_empty()).then_some(text)
        }
        None => raw_turns.map(|t| t.to_string()),
    };
    if let Some(turn_text) = turn_text {
        lines.push(agent_field_line("Turn", turn_text, muted, default));
    }

    if let Some(current) = live.as_deref().and_then(|l| l.current_tool.as_ref()) {
        let text = match current_tool_elapsed_s {
            Some(elapsed) => format!("{} · {}", current.name, format_duration(*elapsed)),
            None => current.name.clone(),
        };
        lines.push(agent_field_line("Tool", text, muted, default));
    }

    let tool_counts_text = match live.as_deref() {
        Some(live) if !live.tools.is_empty() => Some(&live.tools),
        Some(_) => None,
        None if !raw_tool_counts.is_empty() => Some(raw_tool_counts),
        None => None,
    }
    .map(|tools| {
        tools
            .iter()
            .map(|(name, usage)| format!("{name} ×{}", usage.calls))
            .collect::<Vec<_>>()
            .join(" · ")
    })
    .or_else(|| {
        // `raw_tool_calls` is the only figure left once a finished
        // session made calls but kept no per-name breakdown at all.
        (live.is_none() && raw_tool_counts.is_empty())
            .then(|| raw_tool_calls.map(|n| format!("{n} calls")))
            .flatten()
    });
    if let Some(tools) = tool_counts_text {
        lines.push(agent_field_line("Tools", tools, muted, default));
    }

    let (context_tokens, context_window) = match live.as_deref() {
        Some(live) => (live.context_tokens, live.context_window),
        None => (*raw_context_tokens, *raw_context_window),
    };
    if let Some(tokens) = context_tokens {
        let value = match context_window {
            Some(window) => format!("{}/{}", format_tokens_k(tokens), format_tokens_k(window)),
            None => format_tokens_k(tokens),
        };
        lines.push(agent_field_line("Context", value, muted, default));
        if let Some(window) = context_window {
            lines.push(Line::styled(
                format!("        {}", bar(tokens, window, 20)),
                dim,
            ));
        }
    }

    let (files_changed, lines_added, lines_removed) = match live.as_deref() {
        Some(live) => (live.files_changed, live.lines_added, live.lines_removed),
        None => (*raw_files_changed, *raw_lines_added, *raw_lines_removed),
    };
    if files_changed.is_some() || lines_added.is_some() || lines_removed.is_some() {
        let mut text = files_changed
            .map(|files| format!("{files} changed"))
            .unwrap_or_default();
        if lines_added.is_some() || lines_removed.is_some() {
            if !text.is_empty() {
                text.push_str(" · ");
            }
            // Never a bogus `−0` for whichever side the engine
            // didn't report.
            let mut sides = Vec::new();
            if let Some(added) = lines_added {
                sides.push(format!("+{added}"));
            }
            if let Some(removed) = lines_removed {
                sides.push(format!("−{removed}"));
            }
            text.push_str(&sides.join(" "));
        }
        lines.push(agent_field_line("Files", text, muted, default));
    }

    if let Some(tests) = transcript_view.and_then(|v| v.tests) {
        use oscilloscope_core::transcript::TestOutcome;
        let text = match tests {
            TestOutcome::Running => "running".to_string(),
            TestOutcome::Ok => "✓".to_string(),
            TestOutcome::Failed => "✗".to_string(),
        };
        lines.push(agent_field_line("Tests", text, muted, default));
    }

    if let Some(said) = transcript_view.and_then(|v| v.last_reply.as_deref()) {
        lines.push(agent_field_line("Said", said.to_string(), muted, default));
    }

    let cost = match live.as_deref() {
        Some(live) => live.cost_usd,
        None => step.cost_usd,
    };
    if let Some(cost) = cost {
        lines.push(agent_field_line(
            "Cost",
            format!("${cost:.2}"),
            muted,
            default,
        ));
    }

    // The tail's own row budget (issue #703's rule 3): whatever is
    // left of the caller's own pane height once the fields above
    // have taken their share -- newest calls kept, oldest dropped
    // first, with a leading `…` whenever either the read window or
    // this budget cut the list short. `None` (every unit test but
    // the PTY one, which has no real pane to fit into) keeps today's
    // unbounded reading of the whole tail.
    let tail_budget = max_lines.unwrap_or(usize::MAX).saturating_sub(lines.len());
    if let Some(view) = transcript_view.filter(|v| !v.calls.is_empty()) {
        if tail_budget == 0 {
            return lines;
        }
        lines.push(Line::from(""));
        let budget_for_calls = tail_budget - 1;
        let ellipsis = view.truncated || view.calls.len() > budget_for_calls;
        let take = if ellipsis {
            budget_for_calls.saturating_sub(1)
        } else {
            budget_for_calls
        };
        if ellipsis && budget_for_calls > 0 {
            lines.push(Line::styled("…", dim));
        }
        let calls = &view.calls[view.calls.len().saturating_sub(take)..];
        for call in calls {
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
                Some(true) => {
                    text.push_str(" ✓");
                    if let Some(ms) = call.ms {
                        text.push_str(&format!(" {}", format_duration(ms as f64 / 1000.0)));
                    }
                }
                Some(false) => {
                    text.push_str(" ✗");
                    if let Some(ms) = call.ms {
                        text.push_str(&format!(" {}", format_duration(ms as f64 / 1000.0)));
                    }
                }
                // Issue #647: a stopped session's own transcript can
                // end mid-call, with no later `tool_start` to ever
                // mark it `call.stopped` -- the step's own status
                // (not running any more) is enough on its own to
                // call this unfinished, not still running.
                None if call.stopped
                    || !matches!(
                        step.status.kind,
                        oscilloscope_core::model::StatusKind::Running
                            | oscilloscope_core::model::StatusKind::LikelyRunning
                            | oscilloscope_core::model::StatusKind::RunningOrQueued
                    ) =>
                {
                    text.push_str(" stopped")
                }
                None => match call
                    .start_t
                    .as_deref()
                    .and_then(|t| oscilloscope_core::observe::duration_seconds(t, now))
                {
                    Some(elapsed) => {
                        text.push_str(&format!(" running {}", format_duration(elapsed)))
                    }
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

    lines
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
    ("↑ ↓", "select a row", "0 1 2 3", "Tape Signal Flow Store"),
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

// §4 also names `j`/`k`, `PgUp`/`PgDn`, `Home`/`End`, `g`/`G`, `/`
// and `e` as Tape keys, and says the Help overlay lists all of it
// -- but every `tape--help` frame leaves five of its own six rows
// here blank (`design/frames/tape--help--120x30.mock`, rows
// 16-20; row 15 is `T`'s own, issue #714), so adding the rest
// would stop matching the frame on over 300 cells at once, past
// the point an allow-list still documents a design ambiguity
// rather than papering over one. Left for the frame to settle; see
// the PR's own design-questions note.

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
    // Issue #714: `T` (colour theme) in the first of what the
    // frame's own blank rows above otherwise still leave open.
    lines.push(tape_help_row("T", "colour theme", "", "", ctx));
    for _ in 0..5 {
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
/// only ones its own Help lists. `is_watch` (issue #698's own §4:
/// `q`/`c` behave differently on a watch, which owns no process of
/// its own to confirm quitting or ever cancel) swaps the two rows
/// that otherwise describe a run `osi` launched itself.
fn signal_help_lines(ctx: &DrawCtx, is_watch: bool) -> Vec<Line<'static>> {
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
        ("0 1 2 3", "Tape Signal Flow Store", "?", "this help"),
    ];
    const QUIT_ROWS: &[(&str, &str, &str, &str)] = &[
        ("esc", "close · back", "q", "quit · confirms if running"),
        ("c", "cancel · confirms", "", ""),
    ];
    const WATCH_QUIT_ROWS: &[(&str, &str, &str, &str)] = &[
        ("esc", "close · back", "q", "quit at once"),
        ("c", "inert · launched only", "", ""),
    ];
    let quit_rows: &[(&str, &str, &str, &str)] = if is_watch { WATCH_QUIT_ROWS } else { QUIT_ROWS };
    let mut lines: Vec<Line<'static>> = ROWS
        .iter()
        .chain(quit_rows.iter())
        .map(|(k1, d1, k2, d2)| tape_help_row(k1, d1, k2, d2, ctx))
        .collect();
    // Issue #714: `T` cycles the theme here too.
    lines.push(tape_help_row("T", "colour theme", "", "", ctx));
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

/// The Flow's own Help (issue #691, design v1 part 3), the same
/// fixed overlay the Tape's and Signal's own use, listing exactly
/// §4's own Flow keys and the globals every key bar shares -- never
/// the movement aliases, `/`, `e`, `T`, `v`, `z`, `s`, `l`, `Tab`
/// §4 already says are gone from the Flow.
fn flow_help_lines(ctx: &DrawCtx, is_watch: bool) -> Vec<Line<'static>> {
    const ROWS: &[(&str, &str, &str, &str)] = &[
        ("← →", "select a station", "+ -", "open / fold loop lanes"),
        (
            "↑ ↓",
            "select a pass",
            "enter",
            "open the selection full-screen",
        ),
        ("f", "follow Now", "0 1 2 3", "Tape Signal Flow Store"),
        ("?", "this help", "esc", "close · back"),
    ];
    let quit_row: (&str, &str, &str, &str) = if is_watch {
        ("q", "quit at once", "c", "inert · launched only")
    } else {
        ("q", "quit · confirms if running", "c", "cancel · confirms")
    };
    let mut lines: Vec<Line<'static>> = ROWS
        .iter()
        .chain(std::iter::once(&quit_row))
        .map(|(k1, d1, k2, d2)| tape_help_row(k1, d1, k2, d2, ctx))
        .collect();
    // Issue #714: `T` cycles the theme here too.
    lines.push(tape_help_row("T", "colour theme", "", "", ctx));
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

/// The Store's own Help (issue #695, design v1 part 4), the same
/// fixed overlay the Tape's, Signal's and Flow's own use, listing
/// exactly §4's own Store keys (`↑↓`/its aliases, `enter`) and the
/// globals every key bar shares -- never `i`/`o`/`$` (Cost's own
/// measures, gone), `Tab`, `s`/`l`/`z`/`v`, `e` or `/`.
fn store_help_lines(ctx: &DrawCtx, unit: u64, is_watch: bool) -> Vec<Line<'static>> {
    let mut rows: Vec<(&str, &str, &str, &str)> = vec![
        ("↑ ↓", "select a row", "enter", "open the selected store"),
        ("j k", "same as ↑ ↓", "PgUp PgDn", "move a page"),
        ("Home End", "first / last row", "g G", "same as Home/End"),
        ("0 1 2 3", "Tape Signal Flow Store", "?", "this help"),
    ];
    if is_watch {
        rows.push(("esc", "close · back", "q", "quit at once"));
        rows.push(("c", "inert · launched only", "", ""));
    } else {
        rows.push(("esc", "close · back", "q", "quit · confirms if running"));
        rows.push(("c", "cancel · confirms", "", ""));
    }
    let mut lines: Vec<Line<'static>> = rows
        .iter()
        .map(|(k1, d1, k2, d2)| tape_help_row(k1, d1, k2, d2, ctx))
        .collect();
    lines.push(Line::from(""));
    lines.push(tape_help_row(
        "▰",
        &format!("= {unit} t · fixed scale"),
        "",
        "",
        ctx,
    ));
    // Issue #714: `T` cycles the theme here too.
    lines.push(tape_help_row("T", "colour theme", "", "", ctx));
    for _ in 0..3 {
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

fn store_key_bar_items(wide: bool) -> Vec<(&'static str, &'static str)> {
    let mut items = vec![("↑↓", "Select"), ("enter", "Open")];
    if wide {
        items.push(("0-3", "Screens"));
    }
    items.push(("?", "Help"));
    items.push(("q", "Quit"));
    items
}

/// §3.2's status line: the most recently finished row, any depth --
/// the same shared chrome the Signal's and Flow's own status lines
/// already read.
fn draw_store_status_line(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::store::StoreScreen,
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
                oscilloscope_core::format::format_tokens_arrow(sent),
                oscilloscope_core::format::format_tokens_arrow(received)
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

fn fill_rect(frame: &mut Frame, rect: Rect, bg: Color) {
    frame.buffer_mut().set_style(rect, Style::default().bg(bg));
}

/// §3.6's deviation colour, without the table's own extra bold
/// (issue #695's rule 7: "the table's own deviations are regular")
/// -- the same bands [`tape_deviation_style`] uses, kept separate
/// since that one always bolds the outer two bands.
fn store_table_deviation_style(pct: f64, theme: &Theme) -> Style {
    if pct >= 50.0 {
        Style::default().fg(theme.status_failed)
    } else if pct >= 25.0 {
        Style::default().fg(theme.warning)
    } else if pct >= 0.0 {
        Style::default().fg(theme.text)
    } else {
        Style::default().fg(theme.accent_primary)
    }
}

/// Issue #695's rules 4 and 8: unchanged under `width` columns;
/// otherwise cut to `width - 2` plus a trailing `⋯`, always leaving
/// at least one trailing space of its own (the frames cut a label
/// that exactly fills its own field too, `drafts › iter_1 › draft`
/// at 23 columns in a 23-wide field); nothing at all once `width`
/// itself is under 2. The one cut rule the Largest Stores table's
/// own plain-string labels (`store_cut_label`) and a tile's own
/// styled lines (`store_cut_spans`) share.
fn store_cut_keep_len(total_chars: usize, width: usize) -> Option<usize> {
    if total_chars < width {
        None
    } else if width < 2 {
        Some(0)
    } else {
        Some(width - 2)
    }
}

/// Cuts `spans`' own combined text down to `width` columns, with a
/// trailing `⋯` in the style of whichever span the cut falls in --
/// a tile's own text is never wrapped, only clipped by its own rect
/// otherwise, so a label wider than its tile needs this the same
/// way the Largest Stores table's `store_cut_label` already cuts a
/// plain string, through the one shared rule ([`store_cut_keep_len`])
/// both follow.
fn store_cut_spans(spans: Vec<Span<'static>>, width: usize) -> Vec<Span<'static>> {
    let total: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    let Some(mut remaining) = store_cut_keep_len(total, width) else {
        return spans;
    };
    if remaining == 0 && width < 2 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut cut_style = spans.last().map(|s| s.style).unwrap_or_default();
    for span in spans {
        if remaining == 0 {
            cut_style = span.style;
            break;
        }
        let len = span.content.chars().count();
        if len <= remaining {
            remaining -= len;
            out.push(span);
        } else {
            let cut: String = span.content.chars().take(remaining).collect();
            cut_style = span.style;
            out.push(Span::styled(cut, span.style));
            remaining = 0;
        }
    }
    out.push(Span::styled("\u{22ef}", cut_style));
    out
}

/// A leaf tile's own content lines (issue #695's rule 4): `name · N
/// t` on one line when it fits, else `name` then `N t` on the next;
/// the deviation line (absent without history); the arrows line --
/// as many as the tile's own height allows, dropped from the bottom,
/// each cut on the right with `⋯` when it overruns the tile's own
/// width.
fn store_leaf_lines(
    leaf: &oscilloscope_core::store::StoreLeaf,
    width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let width = width as usize;
    // A live leaf reads faint; in a theme that paints no background
    // (`terminal`), dim text on the leaf's bright-black fill would be
    // the hardest text in the diagram to read, so it is set in italic
    // there instead.
    let faint = if !leaf.live {
        Modifier::empty()
    } else if theme.background.is_none() {
        Modifier::ITALIC
    } else {
        Modifier::DIM
    };
    let name_style = Style::default()
        .fg(theme.text)
        .add_modifier(Modifier::BOLD | faint);
    let value_style = Style::default().fg(theme.text).add_modifier(faint);
    let muted_style = Style::default().fg(theme.fg_muted).add_modifier(faint);
    let tok_text = oscilloscope_core::format::format_tokens(leaf.tokens());
    let mut lines = Vec::new();
    let one_line = format!(" {} \u{b7} {tok_text}", leaf.label);
    if one_line.chars().count() <= width {
        lines.push(pad_line(
            store_cut_spans(
                vec![
                    Span::styled(format!(" {} ", leaf.label), name_style),
                    Span::styled(format!("\u{b7} {tok_text}"), value_style),
                ],
                width,
            ),
            width,
        ));
    } else {
        lines.push(pad_line(
            store_cut_spans(
                vec![Span::styled(format!(" {}", leaf.label), name_style)],
                width,
            ),
            width,
        ));
        lines.push(pad_line(
            store_cut_spans(
                vec![Span::styled(format!(" {tok_text}"), value_style)],
                width,
            ),
            width,
        ));
    }
    if let Some(pct) = leaf.deviation_pct {
        let dev_style = tape_deviation_style(pct, theme).add_modifier(Modifier::BOLD | faint);
        lines.push(pad_line(
            store_cut_spans(
                vec![
                    Span::styled(format!(" {pct:+.0}% "), dev_style),
                    Span::styled("vs usual", muted_style),
                ],
                width,
            ),
            width,
        ));
    }
    let sent_text = if leaf.tokens_sent_unknown {
        "\u{2014}".to_string()
    } else {
        oscilloscope_core::format::format_tokens_arrow(leaf.tokens_sent)
    };
    let received_text = if leaf.tokens_received_unknown {
        "\u{2014}".to_string()
    } else {
        oscilloscope_core::format::format_tokens_arrow(leaf.tokens_received)
    };
    lines.push(pad_line(
        store_cut_spans(
            vec![Span::styled(
                format!(" \u{2191}{sent_text} \u{2193}{received_text}"),
                muted_style,
            )],
            width,
        ),
        width,
    ));
    lines
}

/// A container tile's own title line (issue #695's rules 1/6/12): `●
/// {name}` in the accent colour while running, until the run itself
/// ends; `<n>/<m> with tokens` as a second line once its own
/// children don't fit.
fn store_container_lines(
    label: &str,
    running: bool,
    collapsed: Option<oscilloscope_core::store::StoreCollapsedContainer>,
    tokens: u64,
    width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    let width = width as usize;
    let (name_text, name_style) = if running {
        (
            format!(" \u{25cf} {label} "),
            Style::default()
                .fg(theme.accent_primary)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        (
            format!(" {label} "),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        )
    };
    let tok_text = format!(
        "\u{b7} {}",
        oscilloscope_core::format::format_tokens(tokens)
    );
    let mut lines = vec![pad_line(
        store_cut_spans(
            vec![
                Span::styled(name_text, name_style),
                Span::styled(tok_text, Style::default().fg(theme.fg_muted)),
            ],
            width,
        ),
        width,
    )];
    if let Some(c) = collapsed {
        lines.push(pad_line(
            store_cut_spans(
                vec![Span::styled(
                    format!(
                        " {}/{} with tokens",
                        c.children_with_tokens, c.children_planned
                    ),
                    Style::default().fg(theme.fg_muted),
                )],
                width,
            ),
            width,
        ));
    }
    lines
}

fn render_tile_lines(frame: &mut Frame, rect: Rect, lines: Vec<Line<'static>>) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let take: Vec<Line<'static>> = lines.into_iter().take(rect.height as usize).collect();
    frame.render_widget(Paragraph::new(take), rect);
}

fn store_rect_to_ratatui(rect: oscilloscope_core::store::StoreRect) -> Rect {
    Rect {
        x: rect.x,
        y: rect.y,
        width: rect.w,
        height: rect.h,
    }
}

/// A container tile's own bounds in the `terminal` theme (issue
/// #714): `bg_inset` paints nothing there (`Color::Reset`, same as
/// the unfilled pane -- no ANSI colour reads as a safe "recessed"
/// fill against every terminal's own background, the way bright
/// black safely reads as a leaf's own fill), so a container differs
/// from both a leaf's solid fill and the pane itself by this guide
/// line alone, drawn over the tile's own already-painted left edge
/// after its text (`store_container_lines`'s own leading space on
/// every row, never a character worth keeping). A no-op for every
/// named theme, whose own `bg_inset` already is a real fill.
fn mark_inset_frame(frame: &mut Frame, rect: Rect, theme: &Theme) {
    if theme.background.is_some() {
        return;
    }
    let style = Style::default().fg(theme.border_focused);
    for y in rect.y..rect.y.saturating_add(rect.height) {
        if let Some(cell) = frame.buffer_mut().cell_mut((rect.x, y)) {
            cell.set_symbol("\u{2502}").set_style(style);
        }
    }
}

/// The area diagram's own tiles (issue #695's rule 2): a container's
/// own rect fills with `bg.inset`, a leaf's with `bg.surface`,
/// painted in the order [`oscilloscope_core::store::layout_store`]
/// returns them -- a container always precedes its own children, so
/// painting in that order lets a child's own smaller rect paint over
/// its parent's fill without any extra bookkeeping.
fn draw_store_tiles(
    frame: &mut Frame,
    tiles: &[oscilloscope_core::store::StoreTile],
    theme: &Theme,
) {
    use oscilloscope_core::store::StoreTileKind;
    for tile in tiles {
        let rect = store_rect_to_ratatui(tile.rect);
        if rect.width == 0 || rect.height == 0 {
            continue;
        }
        match &tile.kind {
            StoreTileKind::Leaf(leaf) => {
                fill_rect(frame, rect, theme.bg_surface);
                let lines = store_leaf_lines(leaf, rect.width, theme);
                render_tile_lines(frame, rect, lines);
            }
            StoreTileKind::Container { running, collapsed } => {
                fill_rect(frame, rect, theme.bg_inset);
                let lines = store_container_lines(
                    &tile.label,
                    *running,
                    *collapsed,
                    tile.tokens,
                    rect.width,
                    theme,
                );
                render_tile_lines(frame, rect, lines);
                mark_inset_frame(frame, rect, theme);
            }
            StoreTileKind::More { count, tokens } => {
                fill_rect(frame, rect, theme.bg_inset);
                // The leading space (never cut, like every other
                // tile's own first column) keeps column 0 blank for
                // `mark_inset_frame` to mark, below.
                let text = format!(
                    " \u{2026} {count} more \u{b7} {}",
                    oscilloscope_core::format::format_tokens(*tokens)
                );
                let line = pad_line(
                    store_cut_spans(
                        vec![Span::styled(text, Style::default().fg(theme.fg_muted))],
                        rect.width as usize,
                    ),
                    rect.width as usize,
                );
                render_tile_lines(frame, rect, vec![line]);
                mark_inset_frame(frame, rect, theme);
            }
        }
    }
}

/// Screen 3's own area diagram and Totals row (issue #695, design
/// v1 part 4): a bordered pane, always focused (the Store has only
/// one primary pane), with the tiles inset one column on the left
/// and two on the right of its own interior (rule 2's margins), a
/// one-cell gutter between siblings, and the shared Totals row
/// pinned above its own bottom border, a blank row and a rule above
/// that.
fn draw_store_chart(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::store::StoreScreen,
    header: &oscilloscope_core::render::Header,
    title_full: bool,
    ctx: &DrawCtx,
) {
    let border_color = ctx.theme.border_focused;
    let title = if title_full {
        "Store \u{b7} Tokens by Store \u{b7} Area = Tokens Used So Far"
    } else {
        "Store \u{b7} Area = Tokens Used So Far"
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(ratatui::symbols::border::ROUNDED)
        .title(tape_block_title(title, border_color, ctx))
        .border_style(Style::default().fg(border_color));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height <= 3 {
        return;
    }
    let tiles_area = Rect {
        x: inner.x + 1,
        y: inner.y,
        width: inner.width.saturating_sub(3),
        height: inner.height - 3,
    };
    if tiles_area.width > 0 && tiles_area.height > 0 {
        let tiles = oscilloscope_core::store::layout_store(
            &screen.tree,
            tiles_area.x,
            tiles_area.y,
            tiles_area.width,
            tiles_area.height,
        );
        draw_store_tiles(frame, &tiles, ctx.theme);
    }
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

/// The Largest Stores table's own column widths (issue #695's rule
/// 8) -- `bar`(8 cells + 2 trailing)/`tokens` are fixed; `wide` is
/// the same boolean that decides whether Readings sits beside the
/// table. The label column grows with `content_width` rather than
/// sitting fixed, so a path isn't cut on a wider terminal than
/// either frame shows, but never below the frames' own 27 cells
/// (`wide`) or 23 (not), reserving the same share/deviation width
/// the two frames pin (24/34 cells) for anything beyond that.
/// `widest_tokens_len` is the widest `TOKENS` text the table
/// actually draws (issue #714): the column is sized for it, with
/// room for at least one trailing space before `SHARE` -- `format!
/// ("{:<width$}", ...)` never truncates, so a fixed 8-column floor
/// (every frame's own figures stay well under it, so the floor
/// alone still sizes those) ran an eight-digit count's own trailing
/// `t` straight into `SHARE` once a real run's totals reached one
/// (`35,396,916 t66%`, the issue's own example).
fn store_table_columns(
    wide: bool,
    content_width: usize,
    widest_tokens_len: usize,
) -> (usize, usize, usize) {
    let (min_label, bar_w, min_tokens_w, reserved_combined) = if wide {
        (27usize, 10usize, 8usize, 24usize)
    } else {
        (23usize, 10usize, 8usize, 34usize)
    };
    let tokens_w = min_tokens_w.max(widest_tokens_len + 1);
    let label_w = content_width
        .saturating_sub(bar_w + tokens_w + reserved_combined)
        .max(min_label);
    (label_w, bar_w, tokens_w)
}

/// The table's own share/deviation split ("from the frames", issue
/// #695's rule 8): deviation's own field keeps growing with the
/// pane's own width (12 trailing cells, beyond its own text, in
/// every row this part's frames show); share takes whatever is left
/// of the fixed 12-cell gap between the two fields' own text.
fn store_table_tail_widths(dev_text_len: usize, combined: usize) -> (usize, usize) {
    let share = 12usize.saturating_sub(dev_text_len).max(1);
    let share = share.min(combined);
    (share, combined - share)
}

fn store_cut_label(label: &str, width: usize) -> String {
    let total = label.chars().count();
    match store_cut_keep_len(total, width) {
        None => label.to_string(),
        Some(0) if width < 2 => String::new(),
        Some(keep) => format!("{}\u{22ef}", label.chars().take(keep).collect::<String>()),
    }
}

/// The Largest Stores table (issue #695's rule 8): full path from
/// the run's root, cut on the right with `…`, the fixed-scale bar,
/// tokens, share and deviation, with a cursor -- up to `max_rows`,
/// the rest simply cut (DESIGN.md: "no +N more line").
fn draw_store_table(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::store::StoreScreen,
    max_rows: usize,
    wide: bool,
    ctx: &DrawCtx,
) {
    let border_color = ctx.theme.border;
    let title = format!("Largest Stores \u{b7} \u{25b0} = {} t", screen.unit);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_set(ratatui::symbols::border::ROUNDED)
        .title(tape_block_title(&title, border_color, ctx))
        .border_style(Style::default().fg(border_color));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width < 5 || inner.height == 0 {
        return;
    }
    fill_border_rows(frame, inner, border_color);
    let content_width = (inner.width - 3) as usize;
    let widest_tokens_len = screen
        .table
        .iter()
        .map(|row| {
            oscilloscope_core::format::format_tokens(row.tokens())
                .chars()
                .count()
        })
        .max()
        .unwrap_or(0);
    let (label_w, bar_w, tokens_w) = store_table_columns(wide, content_width, widest_tokens_len);
    let combined = content_width.saturating_sub(label_w + bar_w + tokens_w);
    let theme = ctx.theme;
    // "From the frames": the header's own `TOKENS` lands one column
    // to the left of where the data rows' own tokens column starts,
    // at both pinned widths -- not literally aligned to it. Once an
    // eight-digit figure widens `tokens_w` past its own 8-column
    // floor (issue #714), `TOKENS` itself pads out to match, keeping
    // that same one-column gap before `SHARE` rather than running
    // into it.
    let header_text = format!(
        "   {:<label_bar_width$}{:<tokens_header_width$}SHARE VS USUAL",
        "STORE",
        "TOKENS",
        label_bar_width = (label_w + bar_w).saturating_sub(1),
        tokens_header_width = tokens_w.saturating_sub(1),
    );
    let mut lines = vec![pad_line(
        vec![Span::styled(
            header_text,
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        )],
        content_width + 3,
    )];
    // Issue #695: the cursor can sit on a row past the first
    // `max_rows`, once `↓`/`PgDn`/`End` moved it there -- the window
    // scrolls to keep it in view, with the selected row at the
    // window's own bottom edge the moment it would otherwise scroll
    // off, rather than always starting at row 0 and letting the
    // cursor itself disappear past the table's own bottom border.
    let total_rows = screen.table.len();
    let selected_index = screen
        .cursor_path
        .as_deref()
        .and_then(|path| screen.table.iter().position(|r| r.path == path));
    let max_start = total_rows.saturating_sub(max_rows);
    let start = selected_index
        .map(|idx| idx.saturating_sub(max_rows.saturating_sub(1)))
        .unwrap_or(0)
        .min(max_start);
    for (i, row) in screen.table.iter().enumerate().skip(start).take(max_rows) {
        let selected = screen.cursor_path.as_deref() == Some(row.path.as_str());
        let bg = selected.then_some(theme.selection_bg);
        let label_fg = if selected {
            theme.selection_fg
        } else {
            theme.text
        };
        let share_fg = if selected {
            theme.selection_fg
        } else {
            theme.fg_muted
        };
        let tokens_style = {
            let mut s = Style::default().fg(label_fg);
            if i == 0 {
                s = s.add_modifier(Modifier::BOLD);
            }
            s
        };
        let dev_text = row
            .deviation_pct
            .map(|p| format!("{p:+.0}%"))
            .unwrap_or_else(|| "\u{2014}".to_string());
        let dev_style = row
            .deviation_pct
            .map(|p| store_table_deviation_style(p, theme))
            .unwrap_or_else(|| Style::default().fg(theme.dim));
        let (share_w, dev_w) = store_table_tail_widths(dev_text.chars().count(), combined);
        let bar_unit = screen.unit.max(1);
        let filled = ((row.tokens() as f64 / bar_unit as f64).round() as usize).min(8);
        let cursor = Span::styled(
            if selected { " \u{276f} " } else { "   " }.to_string(),
            Style::default()
                .fg(theme.accent_primary)
                .add_modifier(Modifier::BOLD),
        );
        let mut spans = vec![cursor];
        spans.push(Span::styled(
            format!("{:<1$}", store_cut_label(&row.label, label_w), label_w),
            Style::default().fg(label_fg),
        ));
        spans.push(Span::styled(
            "\u{25b0}".repeat(filled),
            Style::default().fg(theme.accent_primary),
        ));
        spans.push(Span::styled(
            format!(
                "{}{}",
                "\u{25b1}".repeat(8 - filled),
                " ".repeat(bar_w.saturating_sub(8))
            ),
            Style::default().fg(theme.dim),
        ));
        spans.push(Span::styled(
            format!(
                "{:<1$}",
                oscilloscope_core::format::format_tokens(row.tokens()),
                tokens_w
            ),
            tokens_style,
        ));
        spans.push(Span::styled(
            format!(
                "{:<1$}",
                oscilloscope_core::format::format_share_pct(row.share_pct),
                share_w
            ),
            Style::default().fg(share_fg),
        ));
        spans.push(Span::styled(format!("{dev_text:<dev_w$}"), dev_style));
        if let Some(bg) = bg {
            for span in &mut spans {
                span.style = span.style.bg(bg);
            }
        }
        lines.push(Line::from(spans));
    }
    frame.render_widget(
        Paragraph::new(lines),
        Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: inner.height,
        },
    );
}

/// Readings (issue #695's rule 9): Largest, Stores with tokens,
/// Ratio sent to received, and the loop's own progress -- absent
/// without a loop in the document at all.
fn draw_store_readings(
    frame: &mut Frame,
    area: Rect,
    screen: &oscilloscope_core::store::StoreScreen,
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
    if inner.width < 2 || inner.height == 0 {
        return;
    }
    fill_border_rows(frame, inner, border_color);
    let theme = ctx.theme;
    let muted = Style::default().fg(theme.fg_muted);
    let default = Style::default().fg(theme.text);
    let bold = default.add_modifier(Modifier::BOLD);
    let content_width = (inner.width - 1) as usize;
    let readings = &screen.readings;
    let mut lines = vec![pad_line(
        match &readings.largest {
            Some((label, tokens, share)) => vec![
                Span::styled("Largest ", muted),
                Span::styled(format!("{label} "), default),
                Span::styled(
                    format!("{} ", oscilloscope_core::format::format_tokens(*tokens)),
                    bold,
                ),
                Span::styled(
                    format!(
                        "\u{b7} {}",
                        oscilloscope_core::format::format_share_pct(*share)
                    ),
                    muted,
                ),
            ],
            None => vec![
                Span::styled("Largest ", muted),
                Span::styled("\u{2014}", muted),
            ],
        },
        content_width,
    )];
    lines.push(pad_line(
        vec![
            Span::styled("Stores  ", muted),
            Span::styled(
                format!("{} with tokens so far", readings.stores_with_tokens),
                default,
            ),
        ],
        content_width,
    ));
    lines.push(pad_line(
        vec![
            Span::styled("Ratio   ", muted),
            Span::styled(
                readings
                    .ratio
                    .map(|r| format!("{r:.1} : 1 sent to received"))
                    .unwrap_or_else(|| "\u{2014}".to_string()),
                default,
            ),
        ],
        content_width,
    ));
    if let Some((label, with_tokens, total)) = &readings.loop_progress {
        lines.push(pad_line(
            vec![
                Span::styled("Loop    ", muted),
                Span::styled(
                    format!("{label} \u{b7} {with_tokens} of {total} passes have tokens"),
                    default,
                ),
            ],
            content_width,
        ));
    }
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

/// The table's own pane height for `rows` data rows: a header row
/// plus `rows`, inside a border (issue #695's §2.4 frames: 7 at
/// 120x30's 4 rows, 6 at 80x24's 3).
fn store_table_pane_height(rows: usize) -> u16 {
    rows as u16 + 3
}

#[allow(clippy::too_many_arguments)]
fn draw_store(
    frame: &mut Frame,
    full_area: Rect,
    screen: &oscilloscope_core::store::StoreScreen,
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
    // §4: below 80 columns only the Tape is offered -- the Store
    // bounces to it on its own, the same way the Signal does.
    if full_area.width < 80 {
        app.screen = oscilloscope_core::screens::Screen::Tape;
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
    draw_tape_header_band(frame, sections[0], header, "STORE", now, icon_frame, ctx);
    draw_store_status_line(frame, sections[2], screen, header, ctx, now);
    let key_bar_items = if app.tape_fullscreen_step {
        fullscreen_step_key_bar_items(full_area.width >= 80)
    } else {
        store_key_bar_items(full_area.width >= 80)
    };
    draw_key_bar(frame, sections[3], &key_bar_items, ctx);

    let body = sections[1];
    // Issue #704 rule 4: the full-screen view reads `raw_selected`
    // (the exact selection, never climbed to an ancestor the way
    // `selected`, the table's own cursor, is) -- it falls back to
    // `selected` only when nothing stepped there at all (`enter` on
    // the table's own fallback-to-first-row cursor, never set
    // through `app.selected_path`).
    let fullscreen_step = screen.raw_selected.as_ref().or(screen.selected.as_ref());
    if app.tape_fullscreen_step && fullscreen_step.is_some() {
        draw_focused_step_detail(frame, body, fullscreen_step, transcript_view, now, app, ctx);
        return;
    }
    let title_full = full_area.width >= 120;
    // §5's own rule 11: Readings only at 100 columns and up *and* 30
    // rows and up; the table shows 4 rows at 30 rows and up, 3 below
    // that -- 80-99 is always one pane, no Readings, three rows.
    if full_area.width < 100 {
        let table_height = store_table_pane_height(3);
        let rows_split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(6), Constraint::Length(table_height)])
            .split(body);
        draw_store_chart(frame, rows_split[0], screen, header, title_full, ctx);
        draw_store_table(frame, rows_split[1], screen, 3, false, ctx);
        return;
    }
    let show_readings = full_area.height >= 30;
    let max_rows = if show_readings { 4 } else { 3 };
    let table_height = store_table_pane_height(max_rows);
    let rows_split = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(6), Constraint::Length(table_height)])
        .split(body);
    draw_store_chart(frame, rows_split[0], screen, header, title_full, ctx);
    if show_readings {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
            .split(rows_split[1]);
        draw_store_table(frame, cols[0], screen, max_rows, true, ctx);
        draw_store_readings(frame, cols[1], screen, ctx);
    } else {
        draw_store_table(frame, rows_split[1], screen, max_rows, true, ctx);
    }
}

/// One redraw's whole layout (issue #626): a one-line header, shared
/// by every screen (replacing the old three-row one), then the
/// screen currently on display (`app.screen`). Every screen now
/// draws its own header band, status line and key bar entirely on
/// its own (the Tape, Signal, Flow and Store, issue #695), before
/// this function's own shared layout below is ever reached.
/// `tape_screen`/`store_screen`/`flow_screen`/`signal_screen` are
/// screens 0, 3, 2 and 1's own models, `Some` only while that screen
/// is actually on display (issue #626's own performance rule:
/// "models for screens not on display aren't built"). `raw_state` is
/// the engine's own last snapshot, untruncated — `details`' own
/// `prompt_sent`/`value` are already cut to `DETAILS_PREVIEW_CHARS`
/// for the details pane, which `v`'s own full-value overlay must not
/// be.
#[allow(clippy::too_many_arguments)]
pub fn draw(
    frame: &mut Frame,
    state: &oscilloscope_core::render::RenderState,
    details: Option<&Details>,
    // Every screen now draws its own status line from the run's own
    // state (issue #695) -- kept in the signature only so main.rs's
    // own `--log` tail doesn't need a parallel code path.
    _log_lines: &[String],
    app: &mut App,
    raw_state: Option<&serde_json::Value>,
    ctx: &DrawCtx,
    icon_frame: u64,
    now: &str,
    tape_screen: Option<&oscilloscope_core::screens::TapeScreen>,
    store_screen: Option<&oscilloscope_core::store::StoreScreen>,
    flow_screen: Option<&oscilloscope_core::screens::FlowScreen>,
    signal_screen: Option<&oscilloscope_core::screens::SignalScreen>,
    transcript_view: Option<&oscilloscope_core::transcript::TranscriptView>,
) {
    let area = frame.area();
    if let Some(bg) = ctx.theme.background {
        // A named theme's own base fill must set both roles, not
        // just `bg` -- with only `bg` painted, a dark-
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
                transcript_view,
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
    // own panels (the trace, the Now pane, Readings) never share any
    // generic layout; every screen now draws its own.
    if app.screen == oscilloscope_core::screens::Screen::Signal {
        if let Some(signal_screen) = signal_screen {
            draw_signal(
                frame,
                area,
                signal_screen,
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
            signal_screen.map(|s| s.unit).unwrap_or(50),
            state.header.history_same_hash_count,
        );
        return;
    }

    // The Flow rail (issue #691, part 3 of #677) draws its own header
    // band, status line and key bar too, the same self-contained way
    // the Tape and Signal do -- its own panels (the rail, the
    // selected pass or station, the Story pane) never share the
    // generic layout below.
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

    // The Store (issue #695, design v1 part 4, replacing Cost) draws
    // its own header band, status line and key bar too, the same
    // self-contained way the Tape, Signal and Flow do.
    if app.screen == oscilloscope_core::screens::Screen::Store {
        if let Some(store_screen) = store_screen {
            draw_store(
                frame,
                area,
                store_screen,
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
            store_screen.map(|s| s.unit).unwrap_or(50),
            state.header.history_same_hash_count,
        );
    }
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
        // Issue #704 rule 5: while the full-screen step is open,
        // `?` lists its own keys (`[ ]`/`↑↓`/`enter`/`m`/`esc`), the
        // same ones every screen's own fullscreen key bar already
        // does -- never whichever screen happened to open it, whose
        // own keys don't work right now at all.
        Dialog::Help if app.tape_fullscreen_step => {
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
                .title(tape_block_title("Help · Step", border, ctx))
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
            let paragraph = Paragraph::new(fullscreen_help_lines(ctx))
                .style(Style::default().bg(ctx.theme.bg_surface));
            frame.render_widget(paragraph, content_area);
        }
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
            let paragraph = Paragraph::new(signal_help_lines(ctx, app.is_watch))
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
            let paragraph = Paragraph::new(flow_help_lines(ctx, app.is_watch))
                .style(Style::default().bg(ctx.theme.bg_surface));
            frame.render_widget(paragraph, content_area);
        }
        Dialog::Help if app.screen == oscilloscope_core::screens::Screen::Store => {
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
                .title(tape_block_title("Help · Store", border, ctx))
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
            let paragraph = Paragraph::new(store_help_lines(ctx, tape_unit, app.is_watch))
                .style(Style::default().bg(ctx.theme.bg_surface));
            frame.render_widget(paragraph, content_area);
        }
        // Every screen has its own guarded `Dialog::Help` arm above;
        // this never actually draws.
        Dialog::Help => {}
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
    let viewport = area.height.saturating_sub(2) as usize;
    let content_width = (area.width as usize).saturating_sub(3);
    let lines = if let Some(pass) = &screen.selected_pass {
        selected_pass_lines(pass, wide, ctx)
    } else if let Some(decision) = &screen.selected_decision {
        decision_detail_lines(decision, ctx)
    } else if let Some(step) = &screen.selected_step {
        flow_selected_step_lines(
            step,
            transcript_view,
            ctx,
            now,
            Some(viewport),
            content_width,
            app.step_meta_unfolded,
        )
    } else {
        vec![Line::from("nothing selected")]
    };
    let border_color = ctx.theme.border;
    app.details_viewport = viewport;
    let total = lines.len();
    app.details_scroll = app
        .details_scroll
        .min(total.saturating_sub(viewport.max(1)));
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
                    oscilloscope_core::format::format_tokens_arrow(step.sent),
                    oscilloscope_core::format::format_tokens_arrow(step.received)
                ),
                Style::default().fg(ctx.theme.text),
            ));
            spans.push(Span::styled(
                format!("{}  ", oscilloscope_core::format::format_tokens(total)),
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
                        format!(
                            "vs usual {}",
                            oscilloscope_core::format::format_tokens(usual)
                        ),
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
#[allow(clippy::too_many_arguments)]
fn flow_selected_step_lines(
    step: &oscilloscope_core::screens::FocusedStep,
    transcript_view: Option<&oscilloscope_core::transcript::TranscriptView>,
    ctx: &DrawCtx,
    now: &str,
    max_lines: Option<usize>,
    content_width: usize,
    meta_unfolded: bool,
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

    // Issue #703's rule 3: an agent step's own detail, running or
    // finished, is the one shared Agent block -- never this generic
    // asked:/doing:/produced: shape, which only Prompt/Tool/Plain
    // steps still use below.
    if let FocusDetail::Agent {
        stale, live, note, ..
    } = &step.detail
    {
        if *stale {
            lines.push(Line::styled("stopped", dim));
        }
        let note_reserved = usize::from(live.is_none() && running);
        let error_reserved = usize::from(step.error.is_some());
        let block_budget =
            max_lines.map(|max| max.saturating_sub(lines.len() + note_reserved + error_reserved));
        lines.extend(agent_block_lines(
            step,
            transcript_view,
            ctx,
            now,
            block_budget,
        ));
        // The fallback note only ever applies in the brief window
        // before the agent tool's very first `meta.live` write, which
        // the block above has nothing to show for at all yet.
        if live.is_none() && running {
            lines.push(Line::styled(*note, dim));
        }
        if let Some(error) = &step.error {
            lines.push(Line::styled(
                error.clone(),
                Style::default().fg(ctx.theme.error),
            ));
        }
        return lines;
    }

    let asked = match &step.detail {
        FocusDetail::Prompt { snippet, .. } => snippet.clone(),
        FocusDetail::Tool { command, .. } => command.clone(),
        _ => None,
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
            _ => {
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

    // Issue #704 rule 4: the sections every screen's own selection
    // now shares -- `asked:`/`produced:` above already cover a
    // tool's own command and value, so only the data they don't
    // reach is new here.
    lines.extend(focused_attempts_lines(step, content_width, ctx));
    lines.extend(focused_tool_lines(step, content_width, ctx));
    lines.extend(focused_decision_lines(step, content_width, ctx));
    lines.extend(focused_loop_lines(step, content_width, ctx));
    lines.extend(focused_meta_lines(step, content_width, ctx, meta_unfolded));

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

/// The rail's own frontier: the right edge of the last top-level
/// item that has actually run (a Past or Live station anywhere
/// inside it). Once a run has ended there is no `▾ Now` left to
/// follow, so follow parks the view on this instead -- the End
/// station for a run that finished, the failed or cancelled station
/// otherwise -- so a run that ends in a burst right after a long
/// loop does not leave its last stations off the right edge.
fn flow_frontier_x(items: &[RailItem], app: &App, area_width: u16) -> Option<usize> {
    fn has_run(items: &[RailItem]) -> bool {
        items.iter().any(|it| match it {
            RailItem::Station(s) => rail::edge_style(s.status) != EdgeStyle::Ahead,
            RailItem::Fork(f) => f.lanes.iter().any(|l| has_run(&l.items)),
        })
    }
    let layout = flow_layout(items, app, area_width);
    items
        .iter()
        .zip(layout.iter())
        .rev()
        .find(|(item, _)| has_run(std::slice::from_ref(*item)))
        .map(|(_, &(_, end))| end.saturating_sub(1))
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
    run_ended: bool,
) -> usize {
    let total = flow_total_width(items, app, area_width);
    let max_scroll = total.saturating_sub(visible_width);
    let arrow = flow_now_arrow_x(items, app, area_width);
    let in_view = |scroll: usize, arrow: usize| scroll <= arrow && arrow < scroll + visible_width;
    if app.follow {
        // While the run is live, follow tracks `▾ Now`; once it has
        // ended, the rail's own frontier (`flow_frontier_x`) stands in
        // for it, so the view parks where the run stopped.
        let anchor = if run_ended {
            arrow.or_else(|| flow_frontier_x(items, app, area_width))
        } else {
            arrow
        };
        if let Some(arrow) = anchor {
            if !in_view(app.flow_scroll, arrow) {
                app.flow_scroll =
                    flow_scroll_to_boundary(items, app, area_width, visible_width, arrow)
                        .min(max_scroll);
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
        let mut resumed_follow = false;
        if app.flow_manual_scroll || land_back {
            if let Some(arrow) = arrow {
                if in_view(app.flow_scroll, arrow) {
                    app.follow = true;
                    resumed_follow = true;
                }
            }
        }
        // Issue #704 rule 2 (the Flow's own station cursor, "scrolled
        // into view"): with follow off, the view has no other way to
        // move at all (`←→` moves the selection itself, never
        // scrolls, once follow is off -- `keys::flow_select_station`),
        // so it must always keep the selected station in view on its
        // own, the same snap-to-boundary rule follow itself uses.
        if !resumed_follow {
            if let Some(selected) = app.selected_path.clone() {
                if let Some(x) = flow_selected_x(items, app, area_width, &selected) {
                    if !in_view(app.flow_scroll, x) {
                        app.flow_scroll =
                            flow_scroll_to_boundary(items, app, area_width, visible_width, x)
                                .min(max_scroll);
                    }
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

/// The top-level item boundary closest to putting `x` at 60% of the
/// visible width -- `flow_resolve_scroll`'s own snapping rule,
/// shared by its follow-the-arrow branch and its follow-off
/// selection-tracking branch.
fn flow_scroll_to_boundary(
    items: &[RailItem],
    app: &App,
    area_width: u16,
    visible_width: usize,
    x: usize,
) -> usize {
    let target = (x as f64 - visible_width as f64 * 0.6).max(0.0) as usize;
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
    best
}

/// Whether `item` is, or contains anywhere in its own lanes, the
/// station, fork host or loop/`dynamic` lane path named `path` --
/// [`flow_selected_x`]'s own membership test, recursing to any
/// depth (never stopping at the first lane the way [`flow_now_
/// arrow_x`] does, since a selection can be inside any lane, running
/// or not).
fn flow_item_contains(item: &RailItem, path: &str) -> bool {
    match item {
        RailItem::Station(s) => s.path == path,
        RailItem::Fork(fork) => {
            fork.path == path
                || fork.lanes.iter().any(|lane| {
                    lane.path
                        .as_deref()
                        .is_some_and(|p| p == path || path.starts_with(&format!("{p}.")))
                        || lane.items.iter().any(|it| flow_item_contains(it, path))
                })
        }
    }
}

/// Issue #704 rule 2: the selected effect's own x on the rail, for
/// `flow_resolve_scroll`'s own follow-off scrolling -- the start of
/// whichever top-level item owns it, at any depth inside a fork's
/// own lanes (never only the x of its own leaf station, which a
/// folded fork never draws at all) -- `None` when `path` is nowhere
/// on the rail (a selection some other screen made that the Flow
/// doesn't have, DESIGN.md's own "nothing selected" case).
fn flow_selected_x(items: &[RailItem], app: &App, area_width: u16, path: &str) -> Option<usize> {
    let layout = flow_layout(items, app, area_width);
    items
        .iter()
        .zip(layout.iter())
        .find(|(item, _)| flow_item_contains(item, path))
        .map(|(_, &(start, _))| start)
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
                format!("{} ", oscilloscope_core::format::format_tokens(*total)),
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
                "{label} {}",
                oscilloscope_core::format::format_tokens(*total)
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
                Some(u) => format!(
                    "usual {}",
                    oscilloscope_core::format::format_tokens_arrow(*u)
                ),
                None => "usual —".to_string(),
            };
            vec![(text, faint)]
        }
        Headline::NotTaken => vec![("not taken".to_string(), faint)],
        Headline::Received { tokens, estimated } => {
            let marker = if *estimated { "≈" } else { "" };
            vec![(
                format!(
                    "↓{marker}{}",
                    oscilloscope_core::format::format_tokens(*tokens)
                ),
                default_text,
            )]
        }
        Headline::AgentTurn(text) => vec![(text.clone(), default_text)],
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
        Detail::Tokens(t) => vec![(oscilloscope_core::format::format_tokens(*t), muted)],
        Detail::UsualTokens(u) => {
            let text = match u {
                Some(v) => format!(
                    "usual {}",
                    oscilloscope_core::format::format_tokens_arrow(*v)
                ),
                None => "usual —".to_string(),
            };
            vec![(text, faint)]
        }
        Detail::Queued => vec![("queued".to_string(), faint)],
        Detail::Rate(rate) => vec![(
            format!(
                "{}/s",
                oscilloscope_core::format::format_tokens(rate.round() as u64)
            ),
            faint,
        )],
        Detail::Waiting => vec![("waiting".to_string(), faint)],
        Detail::Attempt(attempt) => vec![(format!("attempt {attempt}"), faint)],
        Detail::AgentActivity(text) => vec![(text.clone(), muted)],
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
    decided: bool,
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
    // `not taken` is a verdict: only a decided fork's untaken lane
    // reads it. While the decision is still pending both of its
    // lanes are merely ahead, with a blank headline.
    let headline = if decided { "not taken" } else { "" };
    flow_paint_row(
        buf,
        area,
        scroll,
        visible_width,
        x,
        y0 + 1,
        &[(headline.to_string(), style)],
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
                    rail::edge_style(fork.source_status) == EdgeStyle::Past,
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

fn flow_key_bar_items(
    area_width: u16,
    selecting_stations: bool,
) -> Vec<(&'static str, &'static str)> {
    // Issue #704 rule 3: with follow off or the run ended, ←→ moves
    // the selected lane's own station cursor instead of scrolling --
    // the bar's own label follows whichever one actually happens.
    let mut items = vec![(
        "←→",
        if selecting_stations {
            "Select"
        } else {
            "Scroll"
        },
    )];
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
                oscilloscope_core::format::format_tokens_arrow(sent),
                oscilloscope_core::format::format_tokens_arrow(received)
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
        let scroll = flow_resolve_scroll(&items, &mut app, 200, 100, false);
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
        let scroll = flow_resolve_scroll(&items, &mut app, area_width, visible_width, false);
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
        let scroll = flow_resolve_scroll(&items, &mut app, 200, 100, false);
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
        let scroll = flow_resolve_scroll(&items, &mut app, 200, 100, false);
        assert_eq!(scroll, 0);
        assert!(
            !app.follow,
            "Now being in view on an ordinary redraw must not resume follow on its own"
        );
        // A second ordinary redraw, same as the first: still no resume.
        let scroll = flow_resolve_scroll(&items, &mut app, 200, 100, false);
        assert_eq!(scroll, 0);
        assert!(!app.follow);
    }

    /// The manual-scroll flag is one-shot: consumed by the very next
    /// `flow_resolve_scroll` call regardless of outcome, so a later
    /// redraw with no new ←→ press can't ride on a stale flag.
    /// Once the run has ended there is no `▾ Now`; with follow on,
    /// the view parks on the frontier (the End station of a finished
    /// run), so a burst of fast final steps never leaves them off the
    /// right edge.
    #[test]
    fn flow_resolve_scroll_parks_on_the_frontier_once_the_run_ended() {
        let mut items: Vec<RailItem> = (0..40)
            .map(|i| station(&format!("s{i}"), StatusKind::Done))
            .collect();
        items.push(station("End", StatusKind::Done));
        let mut app = App::new(false);
        app.follow = true;
        app.flow_scroll = 0;
        let area_width = 120u16;
        let visible_width = 100usize;
        let scroll = flow_resolve_scroll(&items, &mut app, area_width, visible_width, true);
        let frontier = flow_frontier_x(&items, &app, area_width).expect("the End station ran");
        assert!(
            scroll <= frontier && frontier < scroll + visible_width,
            "the frontier at {frontier} must be in view after scrolling to {scroll}"
        );
        assert!(
            scroll > 0,
            "a 41-station ended rail cannot stay at scroll 0"
        );
        assert!(app.follow, "follow stays on; it parked the view itself");
    }

    /// A moment with nothing running in a run that is still live is
    /// not the frontier rule: the rail stays put until Now returns.
    #[test]
    fn flow_resolve_scroll_does_not_park_on_the_frontier_while_the_run_is_live() {
        let mut items: Vec<RailItem> = (0..40)
            .map(|i| station(&format!("s{i}"), StatusKind::Done))
            .collect();
        items.push(station("End", StatusKind::Pending));
        let mut app = App::new(false);
        app.follow = true;
        app.flow_scroll = 0;
        let scroll = flow_resolve_scroll(&items, &mut app, 120, 100, false);
        assert_eq!(scroll, 0, "no Now and no ended run: nothing to scroll to");
    }

    /// A failed run's frontier is the station that failed, not the
    /// planned tail behind it.
    #[test]
    fn flow_frontier_is_the_last_station_that_ran() {
        let items = vec![
            station("a", StatusKind::Done),
            station("b", StatusKind::Failed),
            station("c", StatusKind::Pending),
            station("End", StatusKind::Pending),
        ];
        let app = App::new(false);
        let layout = flow_layout(&items, &app, 120);
        assert_eq!(
            flow_frontier_x(&items, &app, 120),
            Some(layout[1].1 - 1),
            "the failed station b is the frontier"
        );
    }

    #[test]
    fn flow_resolve_scrolls_manual_scroll_flag_is_consumed_after_one_draw() {
        let items = vec![station("a", StatusKind::Running)];
        let mut app = App::new(false);
        app.follow = false;
        app.flow_scroll = 0;
        app.flow_manual_scroll = true;
        flow_resolve_scroll(&items, &mut app, 200, 100, false);
        assert!(
            !app.flow_manual_scroll,
            "the flag is consumed by the draw it armed"
        );
    }

    /// Issue #704 rule 2: with follow off (the ordinary state once
    /// a run has ended and the user has looked at anything),
    /// `flow_resolve_scroll` must bring the selected station into
    /// view on its own -- `←→` only moves the selection then, never
    /// scrolls, so nothing else ever would.
    #[test]
    fn flow_resolve_scroll_brings_the_selected_station_into_view_with_follow_off() {
        let items: Vec<RailItem> = (0..40)
            .map(|i| station(&format!("s{i}"), StatusKind::Done))
            .collect();
        let mut app = App::new(false);
        app.follow = false;
        app.flow_scroll = 0;
        app.selected_path = Some("s39".to_string());
        let visible_width = 100usize;
        let area_width = 200u16;
        let scroll = flow_resolve_scroll(&items, &mut app, area_width, visible_width, true);
        let x = flow_selected_x(&items, &app, area_width, "s39").expect("s39 is on the rail");
        assert!(
            scroll <= x && x < scroll + visible_width,
            "the selected station must be in view: scroll={scroll} x={x}"
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
    fn selected_pass_lines_scales_its_own_vs_usual_figure() {
        let theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Dracula);
        let zone = test_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: false,
        };
        let pass = oscilloscope_core::screens::SelectedPass {
            index: 0,
            label: "iter_0".to_string(),
            active_label: "Draft".to_string(),
            active_status: oscilloscope_core::model::RowStatus {
                kind: StatusKind::Done,
                skip_reason: None,
                retrying: false,
            },
            active_elapsed_s: None,
            slot: None,
            steps: vec![oscilloscope_core::screens::SelectedPassStep {
                label: "Draft".to_string(),
                status: oscilloscope_core::model::RowStatus {
                    kind: StatusKind::Done,
                    skip_reason: None,
                    retrying: false,
                },
                sent: 1_200,
                received: 100,
                usual: Some(1_500_000),
                produced: None,
            }],
        };
        let lines = selected_pass_lines(&pass, true, &ctx);
        let text: String = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect();
        assert!(text.contains("vs usual 1.5 Mt"), "{text}");
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
    border_color: Color,
    ctx: &DrawCtx,
    now: &str,
) {
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
    let scroll = flow_resolve_scroll(
        &screen.rail.items,
        app,
        frame_width,
        visible_width,
        nothing_to_follow,
    );

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
    // Issue #704 rule 3: `←→`'s own station list for the next key,
    // the selected lane's stations against the rail this redraw
    // just built -- one frame stale at worst, the same lifecycle
    // `flow_rows` already has.
    app.flow_lane_stations =
        oscilloscope_core::rail::lane_stations(&screen.rail, app.selected_path.as_deref());
    // Issue #704 rule 3: what ←/→ step out to once the selection is
    // already at the selected lane's own first/last station -- same
    // lifecycle as `flow_lane_stations` above.
    app.flow_step_out_before = app
        .selected_path
        .as_deref()
        .and_then(|p| oscilloscope_core::rail::step_out_of_lane(&screen.rail, p, false));
    app.flow_step_out_after = app
        .selected_path
        .as_deref()
        .and_then(|p| oscilloscope_core::rail::step_out_of_lane(&screen.rail, p, true));
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
    let key_bar_items = if app.tape_fullscreen_step {
        fullscreen_step_key_bar_items(full_area.width >= 80)
    } else {
        let run_ended = matches!(
            screen.head,
            oscilloscope_core::screens::TapeHead::Ended { .. }
                | oscilloscope_core::screens::TapeHead::Failed { .. }
        );
        flow_key_bar_items(full_area.width, !app.follow || run_ended)
    };
    draw_key_bar(frame, sections[3], &key_bar_items, ctx);

    let body = sections[1];
    // §5: the Story pane needs 100 columns and 30 rows both -- the
    // same tier the frames' own `flow--normal--80x24` drops it at.
    let wide = full_area.width >= 100 && full_area.height >= 30;
    // Issue #704 rule 4: a plain station's own full-screen step
    // reads the same shared renderer the Signal's and Store's
    // already do (every section, the transcript in full, never the
    // Selected pane's own shorter `flow_selected_step_lines`, which
    // stays exactly as it was for the small, non-full-screen pane
    // below) -- a pass or a decision has no such step of its own
    // (`FocusDetail::Loop`/`Decision` belong to the *host* row, not
    // to one of a loop's own passes), so those two keep their own
    // panes even in full screen.
    if app.tape_fullscreen_step && screen.selected_step.is_some() {
        draw_focused_step_detail(
            frame,
            body,
            screen.selected_step.as_ref(),
            transcript_view,
            now,
            app,
            ctx,
        );
        return;
    }
    if app.tape_fullscreen_step
        && (screen.selected_pass.is_some() || screen.selected_decision.is_some())
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
        ctx.theme.border_focused,
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

    /// A minimal running agent [`oscilloscope_core::screens::FocusedStep`]
    /// (issue #703), for `agent_block_lines`'s own unit tests -- every
    /// field this module itself never reads stays at its own default.
    fn agent_focused_step(
        live: Option<oscilloscope_core::model::AgentLive>,
        stale: bool,
        current_tool_elapsed_s: Option<f64>,
        status_kind: StatusKind,
    ) -> oscilloscope_core::screens::FocusedStep {
        use oscilloscope_core::model::RowStatus;
        use oscilloscope_core::screens::{AGENT_LIVE_DATA_NOTE, FocusDetail, FocusedStep};
        FocusedStep {
            path: "prime.agent1".to_string(),
            breadcrumb: "agent1".to_string(),
            kind_label: "agent",
            provider_or_model: None,
            since_ts: None,
            finished_ts: None,
            elapsed_s: None,
            detail: FocusDetail::Agent {
                asked: None,
                live: live.map(Box::new),
                stale,
                current_tool_elapsed_s,
                note: AGENT_LIVE_DATA_NOTE,
                raw_transcript: None,
                raw_files_changed: None,
                raw_turns: None,
                raw_tool_calls: None,
                raw_tool_counts: Default::default(),
                raw_lines_added: None,
                raw_lines_removed: None,
                raw_context_tokens: None,
                raw_context_window: None,
            },
            status: RowStatus {
                kind: status_kind,
                skip_reason: None,
                retrying: false,
            },
            error: None,
            value: None,
            received: None,
            tokens: None,
            cost_usd: None,
            value_full: None,
            prompt_sent_full: None,
            params_rendered_full: None,
            usual_tokens: None,
            share_pct: None,
            attempts: Vec::new(),
            retries_used: None,
            meta_extra: Vec::new(),
            usual_busy_p50_s: None,
            usual_busy_p90_s: None,
        }
    }

    /// Issue #703's rules 1/2: a running or finished agent row's
    /// own "what" text is cut from the right by dropping whole
    /// ` · `-separated pieces, never half of one -- `ctx 41k` goes
    /// first, whole, never `· ct⋯`, and so on back toward the
    /// elapsed time, which stays on its own once nothing else fits.
    #[test]
    fn tape_agent_what_text_cuts_from_the_right_at_segment_boundaries() {
        let what = "1m12s · turn 4 · bash 12.0s · ctx 41k/200k";
        assert_eq!(tape_agent_what_text(what, 100), what);
        // Just short of the whole string: the context piece drops
        // entirely, never a partial `ctx 41k⋯` or `· ct⋯`.
        let cut = tape_agent_what_text(what, what.chars().count());
        assert_eq!(cut, "1m12s · turn 4 · bash 12.0s⋯");
        assert!(!cut.contains("ctx"), "{cut}");
        // Narrower still: the tool segment drops too.
        let cut = tape_agent_what_text(what, 24);
        assert_eq!(cut, "1m12s · turn 4⋯");
        assert!(!cut.contains("bash"), "{cut}");
        // Narrower yet: even the turn segment drops, leaving the
        // elapsed time alone.
        let cut = tape_agent_what_text(what, 12);
        assert_eq!(cut, "1m12s⋯");
        // Narrower than even the first segment alone: falls back to
        // cutting that one piece by character, same as any other
        // row's own text would.
        let cut = tape_agent_what_text(what, 4);
        assert_eq!(cut, "1m⋯");
    }

    /// Issue #647, carried over by #703's shared Agent block: with no
    /// transcript tailed yet, a running tool's own elapsed shows next
    /// to its own name in the Tool field, from `meta.live.current_
    /// tool.started_at` -- not just the session's own total.
    #[test]
    fn agent_block_lines_shows_the_running_tools_own_elapsed() {
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
        let step = agent_focused_step(Some(live), false, Some(24.0), StatusKind::Running);
        let lines = agent_block_lines(&step, None, &ctx, "2026-01-01T11:52:54Z", None);
        let text = lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("bash · 24.0s"), "{text}");
    }

    /// Issue #647, carried over by #703's shared Agent block: once
    /// stale (the session has stopped), the last tool it ran shows
    /// with no elapsed of its own -- it never finished, but nothing
    /// is still running it either.
    #[test]
    fn agent_block_lines_shows_a_stale_current_tool_with_no_elapsed() {
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
        let step = agent_focused_step(Some(live), true, None, StatusKind::Running);
        let lines = agent_block_lines(&step, None, &ctx, "2026-01-01T11:52:54Z", None);
        let text = lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("bash"), "{text}");
        assert!(!text.contains("24.0s"), "{text}");
        assert!(!text.contains("running"), "{text}");
    }

    /// Issue #641: two tool calls genuinely running at once (neither
    /// has a `tool_end`, and neither is superseded by a later
    /// `tool_start` with a lower id, so neither reads as `stopped`)
    /// must each show their own elapsed, from their own `start_t` --
    /// not the session's single `meta.live.current_tool` time, which
    /// would otherwise show the same figure for both.
    #[test]
    fn agent_block_lines_shows_each_running_calls_own_elapsed_in_a_transcript() {
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
        let step = agent_focused_step(Some(live), false, None, StatusKind::Running);
        let lines = agent_block_lines(&step, Some(&view), &ctx, "2026-01-01T11:52:00Z", None);
        let text = lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("pytest -k a running 2m00s"), "{text}");
        assert!(text.contains("pytest -k b running 1m00s"), "{text}");
    }

    fn agent_fixtures_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
    }

    /// Compiles the recorded fixture's own `do.yml` against its own
    /// `config.fixture.json` (`scripts/record_fixtures.py`) into a
    /// real [`PlanTree`] -- never a hand-written one, so a later
    /// change to the recorded orchestration can't quietly drift from
    /// what this test still assumes it says.
    fn agent_fixture_plan(name: &str) -> PlanTree {
        let dir = agent_fixtures_dir().join(name);
        let program = oscilloscope_core::plan::compile(
            &dir.join("do.yml"),
            Some(&dir.join("config.fixture.json")),
            &indexmap::IndexMap::new(),
        )
        .unwrap_or_else(|e| panic!("{name}: do.yml doesn't compile: {e}"));
        PlanTree::from_program(&program)
    }

    /// Every state snapshot `cof` wrote for the named fixture, oldest
    /// first (`snapshots.jsonl`'s own wrapper around each one).
    fn agent_fixture_snapshots(name: &str) -> Vec<Value> {
        let text = std::fs::read_to_string(agent_fixtures_dir().join(name).join("snapshots.jsonl"))
            .unwrap_or_else(|e| panic!("{name}: couldn't read snapshots.jsonl: {e}"));
        text.lines()
            .filter(|l| !l.is_empty())
            .map(|line| {
                let wrapper: Value = serde_json::from_str(line).expect("valid JSON line");
                wrapper["state"].clone()
            })
            .collect()
    }

    /// The first snapshot whose own `prime.top_agent.meta.live.turns`
    /// has reached `turns` -- the same selector `oscilloscope-core`'s
    /// own `agent_live_fixtures.rs` uses, since timing jitter makes a
    /// snapshot's own elapsed wall-clock time useless here.
    fn agent_fixture_state_at_turn(name: &str, turns: u64) -> Value {
        for state in agent_fixture_snapshots(name) {
            let live = state
                .pointer("/prime/top_agent/meta/live")
                .and_then(oscilloscope_core::model::AgentLive::from_value);
            if live.is_some_and(|l| l.turns == Some(turns)) {
                return state;
            }
        }
        panic!("{name}: no snapshot ever reached turns={turns}");
    }

    /// The real transcript file `cof` wrote for the fixture's own
    /// `top_agent` step, tailed and parsed exactly the way osi's own
    /// TUI does -- never hand-written transcript lines.
    fn agent_fixture_transcript_view(name: &str) -> oscilloscope_core::transcript::TranscriptView {
        let path = agent_fixtures_dir()
            .join(name)
            .join("top_agent_transcript.jsonl");
        let lines = oscilloscope_core::observe::tail_transcript_lines(
            &path,
            oscilloscope_core::observe::TRANSCRIPT_TAIL_BYTES,
        );
        oscilloscope_core::transcript::build_transcript_view(&lines, false)
    }

    /// The same `prime.top_agent` step, built the way the Tape's own
    /// detail pane and the Flow's own Selected pane each build
    /// theirs -- [`agent_block_lines`] must read identically off
    /// either, since both panes share the one function (issue #703's
    /// rule 3). Returns the whole [`TapeScreen`] (its own row text is
    /// also checked from it) alongside the Flow's own step.
    fn agent_fixture_screens(
        name: &str,
        state: &Value,
        process: ProcessState,
        now: &str,
    ) -> (
        oscilloscope_core::screens::TapeScreen,
        oscilloscope_core::screens::FocusedStep,
    ) {
        let plan = agent_fixture_plan(name);
        let mut model = RunModel::new();
        let rendered = render::build_folded(
            "do.yml",
            "cof",
            &plan,
            &mut model,
            Some(state),
            process,
            1.0,
            now,
            &[],
            true,
            &Default::default(),
        );
        let flat = oscilloscope_core::model::flatten_state(state);
        let tape_screen = oscilloscope_core::screens::build_tape(
            &rendered.rows,
            &rendered.unfolded_rows,
            &rendered.header,
            &flat,
            &plan,
            &model,
            Some(state),
            Some("prime.top_agent"),
            true,
            now,
        );
        let zone = test_zone();
        let flow_screen = oscilloscope_core::screens::build_flow(
            &rendered.unfolded_rows,
            &rendered.rows,
            &flat,
            &plan,
            &model,
            &rendered.header,
            Some(state),
            &[],
            Some("prime.top_agent"),
            &zone,
            rendered.header.run_end_ts.as_deref(),
            now,
        );
        let flow_step = flow_screen
            .selected_step
            .expect("the Flow selected top_agent");
        (tape_screen, flow_step)
    }

    /// The same recorded-fixture pipeline [`agent_fixture_screens`]
    /// already builds, for any path -- issue #704's own full-screen
    /// sections, checked straight off a real run's own state rather
    /// than hand-written JSON.
    fn fixture_tape_screen(
        name: &str,
        selected_path: &str,
        process: ProcessState,
        now: &str,
    ) -> (
        oscilloscope_core::render::RenderState,
        oscilloscope_core::screens::TapeScreen,
    ) {
        let state = agent_fixture_snapshots(name)
            .into_iter()
            .last()
            .unwrap_or_else(|| panic!("{name}: no snapshot at all"));
        let plan = agent_fixture_plan(name);
        let mut model = RunModel::new();
        let rendered = render::build_folded(
            "do.yml",
            "cof",
            &plan,
            &mut model,
            Some(&state),
            process,
            1.0,
            now,
            &[],
            true,
            &Default::default(),
        );
        let flat = oscilloscope_core::model::flatten_state(&state);
        let tape_screen = oscilloscope_core::screens::build_tape(
            &rendered.rows,
            &rendered.unfolded_rows,
            &rendered.header,
            &flat,
            &plan,
            &model,
            Some(&state),
            Some(selected_path),
            true,
            now,
        );
        (rendered, tape_screen)
    }

    fn agent_block_lines_text(lines: &[Line]) -> String {
        lines
            .iter()
            .map(|l| l.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Issue #703's rule 3, against the recorded `agent_live_pi`
    /// fixture (`tests/fixtures/agent_live_pi`): a finished session's
    /// own block, built straight from its real `meta.raw` and its
    /// real transcript file, reads identically whichever of the
    /// Tape's own detail pane or the Flow's own Selected pane built
    /// the step.
    #[test]
    fn agent_block_lines_matches_the_recorded_agent_live_pi_fixtures_finished_session() {
        let theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let zone = test_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: false,
        };
        let states = agent_fixture_snapshots("agent_live_pi");
        let state = states.last().expect("at least one snapshot");
        let now = state["prime"]["meta"]["completed_at"]
            .as_str()
            .expect("the run itself finished")
            .to_string();
        let (tape_screen, flow_step) =
            agent_fixture_screens("agent_live_pi", state, ProcessState::exited(false), &now);
        let tape_step = tape_screen
            .selected
            .clone()
            .expect("the Tape selected top_agent");
        let view = agent_fixture_transcript_view("agent_live_pi");

        let tape_text = agent_block_lines_text(&agent_block_lines(
            &tape_step,
            Some(&view),
            &ctx,
            &now,
            None,
        ));
        let flow_text = agent_block_lines_text(&agent_block_lines(
            &flow_step,
            Some(&view),
            &ctx,
            &now,
            None,
        ));
        assert_eq!(
            tape_text, flow_text,
            "the same block from the Tape's own step and the Flow's own step"
        );

        assert!(tape_text.contains("Asked   Fix the bug."), "{tape_text}");
        assert!(tape_text.contains("Turn    3"), "{tape_text}");
        assert!(
            tape_text.contains(&format!(
                "Context {}",
                oscilloscope_core::format::format_tokens_k(5000)
            )),
            "{tape_text}"
        );
        assert!(
            tape_text.contains("Files   1 changed \u{b7} +3 \u{2212}1"),
            "{tape_text}"
        );
        assert!(tape_text.contains("Tests   \u{2713}"), "{tape_text}");
        assert!(
            tape_text.contains("Said    Done. MARKER_REPLY all tests pass."),
            "{tape_text}"
        );
        assert!(tape_text.contains("Cost    $0.04"), "{tape_text}");
        // The transcript tail's own ended calls (issue #703's rule 3:
        // `✓ 1.2s`), from the real recorded `ms`.
        assert!(
            tape_text.contains("\u{203a} bash pytest -k MARKER_COMMAND \u{2713} 2.5s"),
            "{tape_text}"
        );
        assert!(
            tape_text.contains("\u{203a} edit src/MARKER_PATH.py \u{2713} 2.5s +3 \u{2212}1"),
            "{tape_text}"
        );
    }

    /// Issue #704 rule 4's own Attempts section, against the
    /// recorded prompt_retried_cof fixture: a retried prompt's own
    /// full-screen step names the attempt that decided the outcome
    /// and the retry count a later pass's own chain otherwise
    /// overwrites out of meta.fallback_attempts.
    #[test]
    fn fullscreen_sections_show_attempts_from_the_recorded_prompt_retried_fixture() {
        let theme = crate::theme::Theme::for_name(crate::theme::ThemeName::CatppuccinMocha);
        let zone = test_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: true,
        };
        let now = "2026-10-19T16:15:06.890638+00:00";
        let (_rendered, tape_screen) = fixture_tape_screen(
            "prompt_retried_cof",
            "prime.ask",
            ProcessState::exited(false),
            now,
        );
        let text = agent_block_lines_text(&tape_normal_detail_lines(
            &tape_screen,
            80,
            40,
            None,
            now,
            false,
            &ctx,
        ));
        assert!(
            text.contains("Attempts") && text.contains("1 retried"),
            "{text}"
        );
        assert!(
            text.contains("scripted")
                && text.contains("scripted-model")
                && text.contains("succeeded"),
            "{text}"
        );
    }

    /// Issue #704 rule 4's own Tokens section, "totals across
    /// attempts": the shared full-screen renderer sums every
    /// recorded attempt's own tokens, shown only once there was
    /// more than one -- no recorded fixture's own `meta.fallback_
    /// attempts` keeps more than its own final pass's one surviving
    /// entry (`docs/orchestration-reference.md`'s own "only the
    /// final pass's chain survives"), so this builds its own step
    /// directly rather than reading one from a run.
    #[test]
    fn fullscreen_tokens_shows_the_total_across_attempts() {
        use oscilloscope_core::model::RowStatus;
        use oscilloscope_core::screens::{AttemptDetail, FocusDetail, FocusedStep};
        let theme = crate::theme::Theme::for_name(crate::theme::ThemeName::CatppuccinMocha);
        let zone = test_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: true,
        };
        let step = FocusedStep {
            path: "prime.ask".to_string(),
            breadcrumb: "ask".to_string(),
            kind_label: "prompt",
            provider_or_model: None,
            since_ts: None,
            finished_ts: None,
            elapsed_s: None,
            detail: FocusDetail::Prompt {
                tokens: Some(33),
                estimated: false,
                snippet: None,
            },
            status: RowStatus {
                kind: StatusKind::Done,
                skip_reason: None,
                retrying: false,
            },
            error: None,
            value: None,
            received: None,
            tokens: Some(33),
            cost_usd: None,
            value_full: None,
            prompt_sent_full: None,
            params_rendered_full: None,
            usual_tokens: None,
            share_pct: None,
            attempts: vec![
                AttemptDetail {
                    adapter: Some("ollama".to_string()),
                    model: Some("m1".to_string()),
                    status: Some("failed".to_string()),
                    tokens_sent: Some(100),
                    tokens_received: Some(0),
                    error: Some("timed out".to_string()),
                },
                AttemptDetail {
                    adapter: Some("scripted".to_string()),
                    model: Some("scripted-model".to_string()),
                    status: Some("succeeded".to_string()),
                    tokens_sent: Some(11),
                    tokens_received: Some(22),
                    error: None,
                },
            ],
            retries_used: Some(1),
            meta_extra: Vec::new(),
            usual_busy_p50_s: None,
            usual_busy_p90_s: None,
        };
        let text = agent_block_lines_text(&focused_step_full_lines(
            &step, None, "now", false, 80, &ctx,
        ));
        assert!(text.contains("total across 2 attempts"), "{text}");
        assert!(text.contains("133 t"), "{text}");
    }

    /// Issue #704 rule 4's own Error section, against the recorded
    /// parity_tool_failed_continue_cof fixture: a handled tool
    /// failure's own full-screen step shows the error in full even
    /// though the run went on.
    #[test]
    fn fullscreen_sections_show_the_error_from_the_recorded_parity_tool_failed_continue_fixture() {
        let theme = crate::theme::Theme::for_name(crate::theme::ThemeName::CatppuccinMocha);
        let zone = test_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: true,
        };
        let now = "2026-01-01T00:00:10Z";
        let (_rendered, tape_screen) = fixture_tape_screen(
            "parity_tool_failed_continue_cof",
            "prime.flaky",
            ProcessState::exited(false),
            now,
        );
        let text = agent_block_lines_text(&tape_normal_detail_lines(
            &tape_screen,
            80,
            40,
            None,
            now,
            false,
            &ctx,
        ));
        assert!(text.contains("Error"), "{text}");
        assert!(text.contains("json: parse failed"), "{text}");
    }

    /// Issue #704 rule 4's own Asked section: the Tape's own detail
    /// pane (and so its own full-screen step, the same renderer)
    /// shows a tool's own command line, against the recorded
    /// each_tree_loop_concurrency fixture's own shell tool step.
    #[test]
    fn tape_detail_shows_the_tool_command_line_from_the_recorded_each_tree_loop_concurrency_fixture()
     {
        let theme = crate::theme::Theme::for_name(crate::theme::ThemeName::CatppuccinMocha);
        let zone = test_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: true,
        };
        let now = "2026-01-01T00:00:10Z";
        let leaf = "prime.each_tree.iter_0.nap";
        let (_rendered, tape_screen) = fixture_tape_screen(
            "each_tree_loop_concurrency",
            leaf,
            ProcessState::exited(true),
            now,
        );
        let text = agent_block_lines_text(&tape_normal_detail_lines(
            &tape_screen,
            80,
            40,
            None,
            now,
            false,
            &ctx,
        ));
        assert!(text.contains("Asked"), "{text}");
        let oscilloscope_core::screens::FocusDetail::Tool {
            command: Some(command),
            ..
        } = &tape_screen.selected.as_ref().unwrap().detail
        else {
            panic!("{leaf} must resolve its own tool command");
        };
        assert!(text.contains(command.as_str()), "{text}");
    }

    /// Issue #704 rule 4's own Asked section, "rendered params":
    /// the shared full-screen renderer shows the whole `params_
    /// rendered` object, not only the command line built from its
    /// own `command`/`args`, against the recorded parity_tool_
    /// failed_continue_cof fixture's own tool step.
    #[test]
    fn fullscreen_asked_shows_rendered_params_from_the_recorded_parity_tool_failed_continue_fixture()
     {
        let theme = crate::theme::Theme::for_name(crate::theme::ThemeName::CatppuccinMocha);
        let zone = test_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: true,
        };
        let now = "2026-01-01T00:00:10Z";
        let (_rendered, tape_screen) = fixture_tape_screen(
            "parity_tool_failed_continue_cof",
            "prime.flaky",
            ProcessState::exited(false),
            now,
        );
        let step = tape_screen
            .selected
            .as_ref()
            .expect("prime.flaky resolves its own step");
        assert!(
            step.params_rendered_full.is_some(),
            "a tool step must carry its own rendered params"
        );
        let text =
            agent_block_lines_text(&focused_step_full_lines(step, None, now, false, 80, &ctx));
        assert!(text.contains("params"), "{text}");
        assert!(
            text.contains(step.params_rendered_full.as_deref().unwrap()),
            "{text}"
        );
    }

    /// Issue #704 rule 4's own Status section, "start → end": the
    /// shared full-screen renderer shows both clock times for a
    /// finished step, against the recorded parity_tool_failed_
    /// continue_cof fixture's own handled failure.
    #[test]
    fn fullscreen_status_shows_start_and_end_clocks_from_the_recorded_parity_tool_failed_continue_fixture()
     {
        let theme = crate::theme::Theme::for_name(crate::theme::ThemeName::CatppuccinMocha);
        let zone = test_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: true,
        };
        let now = "2026-01-01T00:00:10Z";
        let (_rendered, tape_screen) = fixture_tape_screen(
            "parity_tool_failed_continue_cof",
            "prime.flaky",
            ProcessState::exited(false),
            now,
        );
        let step = tape_screen
            .selected
            .as_ref()
            .expect("prime.flaky resolves its own step");
        assert!(step.since_ts.is_some(), "{step:?}");
        assert!(step.finished_ts.is_some(), "{step:?}");
        let text =
            agent_block_lines_text(&focused_step_full_lines(step, None, now, false, 80, &ctx));
        let start = oscilloscope_core::format::format_clock_time(
            step.since_ts.as_deref().unwrap(),
            ctx.zone,
        )
        .unwrap();
        let end = oscilloscope_core::format::format_clock_time(
            step.finished_ts.as_deref().unwrap(),
            ctx.zone,
        )
        .unwrap();
        assert!(text.contains(&format!("{start} → {end}")), "{text}");
    }

    /// Issue #704 rule 4's own Loop section, against the recorded
    /// each_tree_loop_concurrency fixture: a finished loop's own
    /// full-screen step lists every pass, in plan order.
    #[test]
    fn fullscreen_sections_show_the_loop_from_the_recorded_each_tree_loop_concurrency_fixture() {
        let theme = crate::theme::Theme::for_name(crate::theme::ThemeName::CatppuccinMocha);
        let zone = test_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: true,
        };
        let now = "2026-01-01T00:00:10Z";
        let (_rendered, tape_screen) = fixture_tape_screen(
            "each_tree_loop_concurrency",
            "prime.each_tree",
            ProcessState::exited(true),
            now,
        );
        let text = agent_block_lines_text(&tape_normal_detail_lines(
            &tape_screen,
            80,
            40,
            None,
            now,
            false,
            &ctx,
        ));
        assert!(text.contains("Loop"), "{text}");
        for pass in ["pass 1", "pass 2", "pass 3", "pass 4"] {
            assert!(text.contains(pass), "missing {pass} in:\n{text}");
        }
    }

    /// Issue #704 rule 4: `enter` on the Loop section's own pass line
    /// opens that pass -- against the recorded each_tree_loop_
    /// concurrency fixture's own finished loop, scrolling to each
    /// pass's own line in turn and pressing `enter` lands the shared
    /// selection on that exact pass.
    #[test]
    fn enter_on_a_loop_passs_own_line_opens_it() {
        let theme = crate::theme::Theme::for_name(crate::theme::ThemeName::CatppuccinMocha);
        let zone = test_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: true,
        };
        let now = "2026-01-01T00:00:10Z";
        let (_rendered, tape_screen) = fixture_tape_screen(
            "each_tree_loop_concurrency",
            "prime.each_tree",
            ProcessState::exited(true),
            now,
        );
        let mut app = crate::keys::App::new(false);
        app.selected_path = Some("prime.each_tree".to_string());
        app.tape_fullscreen_step = true;
        let mut terminal = Terminal::new(TestBackend::new(80, 40)).unwrap();
        terminal
            .draw(|f| {
                draw_focused_step_detail(
                    f,
                    f.area(),
                    tape_screen.selected.as_ref(),
                    None,
                    now,
                    &mut app,
                    &ctx,
                )
            })
            .unwrap();
        assert!(
            !app.loop_pass_line_paths.is_empty(),
            "the loop's own passes must each resolve a line"
        );
        app.handle_key(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE,
            ),
            &[],
            &[],
            false,
            false,
            &[],
        );
        assert_eq!(
            app.selected_path.as_deref(),
            Some("prime.each_tree.iter_0"),
            "enter with nothing scrolled opens the first pass's own line"
        );
        assert!(
            app.tape_fullscreen_step,
            "enter on a pass never closes the full-screen view"
        );
    }

    /// Issue #704 rule 4: `End` lands on the last full page of the
    /// full-screen step's own content, not the last single line
    /// with the rest of the pane left blank.
    #[test]
    fn end_scrolls_to_the_last_full_page_not_the_last_line_alone() {
        use oscilloscope_core::model::RowStatus;
        use oscilloscope_core::screens::{FocusDetail, FocusedStep};
        let theme = crate::theme::Theme::for_name(crate::theme::ThemeName::CatppuccinMocha);
        let zone = test_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: true,
        };
        let step = FocusedStep {
            path: "prime.a".to_string(),
            breadcrumb: "a".to_string(),
            kind_label: "tool",
            provider_or_model: None,
            since_ts: None,
            finished_ts: None,
            elapsed_s: None,
            detail: FocusDetail::Plain,
            status: RowStatus {
                kind: StatusKind::Done,
                skip_reason: None,
                retrying: false,
            },
            error: None,
            value: None,
            received: None,
            tokens: None,
            cost_usd: None,
            value_full: None,
            prompt_sent_full: None,
            params_rendered_full: None,
            usual_tokens: None,
            share_pct: None,
            attempts: Vec::new(),
            retries_used: None,
            meta_extra: (0..50)
                .map(|i| (format!("key{i}"), format!("value{i}")))
                .collect(),
            usual_busy_p50_s: None,
            usual_busy_p90_s: None,
        };
        let mut app = crate::keys::App::new(false);
        app.tape_fullscreen_step = true;
        app.step_meta_unfolded = true;
        app.details_scroll = usize::MAX;
        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal
            .draw(|f| {
                draw_focused_step_detail(f, f.area(), Some(&step), None, "now", &mut app, &ctx)
            })
            .unwrap();
        assert!(
            app.details_scroll + app.details_viewport >= 50,
            "the last page must reach the end: scroll={} viewport={}",
            app.details_scroll,
            app.details_viewport
        );
        assert!(
            app.details_scroll + 1 < 50,
            "End must show a full page, not just the last line: scroll={}",
            app.details_scroll
        );
    }

    /// Issue #704's own snapshot of the full-screen step view against
    /// the recorded agent_live_pi fixture's own finished session, at
    /// a given terminal size.
    fn fullscreen_step_snapshot(width: u16, height: u16) -> String {
        let states = agent_fixture_snapshots("agent_live_pi");
        let state = states.last().expect("at least one snapshot").clone();
        let now = state["prime"]["meta"]["completed_at"]
            .as_str()
            .expect("the run itself finished")
            .to_string();
        let (rendered, tape_screen) = fixture_tape_screen(
            "agent_live_pi",
            "prime.top_agent",
            ProcessState::exited(false),
            &now,
        );
        let theme = crate::theme::Theme::for_name(crate::theme::ThemeName::CatppuccinMocha);
        let zone = test_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: true,
        };
        let mut app = crate::keys::App::new(false);
        app.selected_path = Some("prime.top_agent".to_string());
        app.tape_fullscreen_step = true;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
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
                    &now,
                    Some(&tape_screen),
                    None,
                    None,
                    None,
                    None,
                )
            })
            .unwrap();
        terminal.backend().to_string()
    }

    #[test]
    fn fullscreen_step_view_matches_the_snapshot_at_120x30() {
        insta::assert_snapshot!(fullscreen_step_snapshot(120, 30));
    }

    #[test]
    fn fullscreen_step_view_matches_the_snapshot_at_80x24() {
        insta::assert_snapshot!(fullscreen_step_snapshot(80, 24));
    }

    /// Issue #703's rule 3, against the recorded `agent_live_claude_
    /// code` fixture mid-turn (`docs/plugins/agent.md`'s own "never
    /// reports `tokens_received`/`cost_usd` live" case for Claude
    /// Code): a running session's own block reads its real live
    /// figures, including a field the engine never reports at all
    /// staying left out rather than a bogus `0`/empty line.
    #[test]
    fn agent_block_lines_matches_the_recorded_agent_live_claude_code_fixtures_running_session() {
        let theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let zone = test_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: false,
        };
        let state = agent_fixture_state_at_turn("agent_live_claude_code", 1);
        let updated_at = state["prime"]["top_agent"]["meta"]["live"]["updated_at"]
            .as_str()
            .expect("a live snapshot has its own updated_at");
        let now = (chrono::DateTime::parse_from_rfc3339(updated_at).unwrap()
            + chrono::Duration::seconds(2))
        .to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
        let (tape_screen, flow_step) = agent_fixture_screens(
            "agent_live_claude_code",
            &state,
            ProcessState::Running,
            &now,
        );
        let tape_step = tape_screen
            .selected
            .clone()
            .expect("the Tape selected top_agent");
        let view = agent_fixture_transcript_view("agent_live_claude_code");

        let tape_text = agent_block_lines_text(&agent_block_lines(
            &tape_step,
            Some(&view),
            &ctx,
            &now,
            None,
        ));
        let flow_text = agent_block_lines_text(&agent_block_lines(
            &flow_step,
            Some(&view),
            &ctx,
            &now,
            None,
        ));
        assert_eq!(
            tape_text, flow_text,
            "the same block from the Tape's own step and the Flow's own step"
        );

        assert!(tape_text.contains("Turn    1"), "{tape_text}");
        assert!(tape_text.contains("Tool    Read"), "{tape_text}");
        assert!(
            tape_text.contains("Tools   Read \u{d7}1 \u{b7} Task \u{d7}1"),
            "{tape_text}"
        );
        assert!(
            tape_text.contains(&format!(
                "Context {}",
                oscilloscope_core::format::format_tokens_k(1600)
            )),
            "{tape_text}"
        );
        // `docs/plugins/agent.md`: Claude Code never reports these
        // two live -- never a bogus `$0.00`/`0 t` line for either.
        assert!(!tape_text.contains("Cost"), "{tape_text}");
        assert!(!tape_text.contains("Files"), "{tape_text}");
        // The real transcript file is the whole finished run's own
        // (fixtures are captured once, after the run ends), so its
        // tail shows every call that session ever made, each with
        // its own real duration.
        assert!(
            tape_text.contains("\u{203a} Read MARKER_PATH.md \u{2713} 2.5s"),
            "{tape_text}"
        );
        assert!(
            tape_text.contains("\u{203a} Edit src/MARKER_PATH.py \u{2713} 2.5s +3 \u{2212}1"),
            "{tape_text}"
        );
        assert!(
            tape_text.contains("Said    Done. MARKER_REPLY"),
            "{tape_text}"
        );
    }

    /// Issue #703's rule 1 and rule 3, against the recorded `agent_
    /// live_pi_cancelled` fixture: an interrupted run's own leftover
    /// `meta.live` (no `completed_at`, `AgentLive::is_recent` long
    /// since false) reads `stopped` on the Tape row and leaves its
    /// transcript's own unfinished call `stopped` too, never still
    /// "running".
    #[test]
    fn agent_block_lines_matches_the_recorded_agent_live_pi_cancelled_fixtures_stale_leftover() {
        let theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let zone = test_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: false,
        };
        let states = agent_fixture_snapshots("agent_live_pi_cancelled");
        let state = states.last().expect("at least one snapshot");
        let updated_at = state["prime"]["top_agent"]["meta"]["live"]["updated_at"]
            .as_str()
            .expect("the leftover live is still there");
        let now = (chrono::DateTime::parse_from_rfc3339(updated_at).unwrap()
            + chrono::Duration::seconds(6))
        .to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
        let (tape_screen, flow_step) = agent_fixture_screens(
            "agent_live_pi_cancelled",
            state,
            ProcessState::exited(true),
            &now,
        );
        let tape_step = tape_screen
            .selected
            .clone()
            .expect("the Tape selected top_agent");
        assert_eq!(tape_step.status.kind, StatusKind::Cancelled);

        let row_what = tape_screen
            .items
            .iter()
            .find_map(|item| match item {
                oscilloscope_core::screens::TapeItem::Row(row) if row.path == "prime.top_agent" => {
                    Some(row.what.clone())
                }
                _ => None,
            })
            .expect("top_agent has its own row");
        assert!(row_what.contains("stopped \u{b7} turn 1"), "{row_what}");

        let view = agent_fixture_transcript_view("agent_live_pi_cancelled");
        let tape_text = agent_block_lines_text(&agent_block_lines(
            &tape_step,
            Some(&view),
            &ctx,
            &now,
            None,
        ));
        let flow_text = agent_block_lines_text(&agent_block_lines(
            &flow_step,
            Some(&view),
            &ctx,
            &now,
            None,
        ));
        assert_eq!(
            tape_text, flow_text,
            "the same block from the Tape's own step and the Flow's own step"
        );

        assert!(tape_text.contains("Turn    1"), "{tape_text}");
        assert!(tape_text.contains("Tool    bash"), "{tape_text}");
        assert!(
            !tape_text.contains("bash \u{b7} "),
            "{tape_text}: stale, so no elapsed next to the tool"
        );
        assert!(tape_text.contains("Tests   running"), "{tape_text}");
        assert!(
            tape_text.contains("\u{203a} bash pytest -k MARKER_COMMAND stopped"),
            "{tape_text}"
        );
    }
    /// Issue #703's rule 3: a call superseded by a retried attempt's
    /// own lower id (`transcript::ToolCall::stopped`) reads `stopped`
    /// in the tail even while the session itself is still running --
    /// never "running", which only the attempt that's actually still
    /// in flight gets.
    #[test]
    fn agent_block_lines_shows_a_stopped_call_from_an_earlier_retried_attempt() {
        let theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let zone = test_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: false,
        };
        let view = oscilloscope_core::transcript::TranscriptView {
            calls: vec![oscilloscope_core::transcript::ToolCall {
                start_t: Some("2026-01-01T11:50:00Z".to_string()),
                end_t: None,
                tool: "bash".to_string(),
                target: Some("pytest -k a".to_string()),
                ok: None,
                ms: None,
                lines_added: None,
                lines_removed: None,
                stopped: true,
            }],
            ..Default::default()
        };
        let live =
            oscilloscope_core::model::AgentLive::from_value(&json!({"engine": "pi"})).unwrap();
        let step = agent_focused_step(Some(live), false, None, StatusKind::Running);
        let lines = agent_block_lines(&step, Some(&view), &ctx, "2026-01-01T11:52:00Z", None);
        let text = agent_block_lines_text(&lines);
        assert!(text.contains("pytest -k a stopped"), "{text}");
        assert!(!text.contains("running"), "{text}");
    }

    /// Issue #703's rule 3: a leading ellipsis marks the tail
    /// whenever the read window didn't reach back to the session's
    /// own start, whether or not the pane's own height also had to
    /// cut it.
    #[test]
    fn agent_block_lines_shows_a_leading_ellipsis_when_the_read_window_was_cut() {
        let theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let zone = test_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: false,
        };
        let view = oscilloscope_core::transcript::TranscriptView {
            calls: vec![oscilloscope_core::transcript::ToolCall {
                start_t: Some("2026-01-01T11:50:00Z".to_string()),
                end_t: Some("2026-01-01T11:50:02Z".to_string()),
                tool: "bash".to_string(),
                target: Some("pytest -k a".to_string()),
                ok: Some(true),
                ms: Some(2000),
                lines_added: None,
                lines_removed: None,
                stopped: false,
            }],
            truncated: true,
            ..Default::default()
        };
        let live =
            oscilloscope_core::model::AgentLive::from_value(&json!({"engine": "pi"})).unwrap();
        let step = agent_focused_step(Some(live), false, None, StatusKind::Running);
        let lines = agent_block_lines(&step, Some(&view), &ctx, "2026-01-01T11:52:00Z", None);
        let text = agent_block_lines_text(&lines);
        let ellipsis_idx = text.find('…').expect("the ellipsis line");
        let call_idx = text.find("pytest -k a").expect("the call line");
        assert!(ellipsis_idx < call_idx, "{text}");
    }

    /// Issue #703's rule 3: once more calls have been made than the
    /// caller's own row budget allows, the newest stay and the
    /// oldest drop first, with the same leading ellipsis -- a pane
    /// that never scrolls (the Tape's own detail pane, the full-
    /// screen step) must still show the tail actually growing rather
    /// than stall once it first fills the pane.
    #[test]
    fn agent_block_lines_keeps_the_newest_calls_once_the_row_budget_cuts_the_tail() {
        let theme = crate::theme::Theme::for_name(crate::theme::ThemeName::Terminal);
        let zone = test_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: false,
        };
        let call = |n: u32| oscilloscope_core::transcript::ToolCall {
            start_t: Some(format!("2026-01-01T11:50:0{n}Z")),
            end_t: Some(format!("2026-01-01T11:50:0{n}Z")),
            tool: "bash".to_string(),
            target: Some(format!("pytest -k {n}")),
            ok: Some(true),
            ms: Some(0),
            lines_added: None,
            lines_removed: None,
            stopped: false,
        };
        let view = oscilloscope_core::transcript::TranscriptView {
            calls: vec![call(1), call(2), call(3), call(4), call(5)],
            ..Default::default()
        };
        let live =
            oscilloscope_core::model::AgentLive::from_value(&json!({"engine": "pi"})).unwrap();
        let step = agent_focused_step(Some(live), false, None, StatusKind::Running);
        // Only the Asked field is absent here, so the block's own
        // fields take 0 lines -- a budget of 3 leaves room for the
        // blank separator, one ellipsis line and one call.
        let lines = agent_block_lines(&step, Some(&view), &ctx, "2026-01-01T11:52:00Z", Some(3));
        let text = agent_block_lines_text(&lines);
        assert!(text.contains("pytest -k 5"), "{text}");
        assert!(!text.contains("pytest -k 1"), "{text}");
        assert!(!text.contains("pytest -k 4"), "{text}");
        assert!(text.contains("…"), "{text}");
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
            config: None,
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
        // Screen::Tape, the only screen that still draws its own
        // content below 80 columns -- Signal, Flow and Store (issue
        // #695) all bounce to it instead at that width, which would
        // leave this test's own narrowest case drawing nothing at
        // all for one real frame.
        let plan = oscilloscope_core::plan::PlanTree::empty();
        let flat = std::collections::BTreeMap::new();
        let model = oscilloscope_core::model::RunModel::new();
        let tape_screen = oscilloscope_core::screens::build_tape(
            &rendered.rows,
            &rendered.unfolded_rows,
            &rendered.header,
            &flat,
            &plan,
            &model,
            None,
            None,
            true,
            "2026-01-01T00:00:00Z",
        );
        let first_cell_is_blank = |width: u16| -> bool {
            let mut app = App::new(false);
            app.screen = oscilloscope_core::screens::Screen::Tape;
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
                        Some(&tape_screen),
                        None,
                        None,
                        None,
                        None,
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
        app.screen = oscilloscope_core::screens::Screen::Store;
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
        app.screen = oscilloscope_core::screens::Screen::Store;
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
        app.screen = oscilloscope_core::screens::Screen::Store;
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
            &rendered.unfolded_rows,
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
            config: None,
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
            &rendered.unfolded_rows,
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
    /// as the frame -- `(50, 21)` is the unrelated Run End rounding
    /// cell both goldens already carry, at its own centred window's
    /// row (issue #710).
    const STATUS_LINE_READS_AN_EARLIER_STEP_120X30: [(u16, u16); 43] = [
        (50, 21),
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
                None,
                None,
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
            None,
            None,
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
            "table.header" => theme.text,
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
    /// a bare `Color::Reset`). Issue #695: a tag can also be a bare
    /// `{on:bg-role}` with no role at all -- the Store's own
    /// container padding and inset rows, always blank spaces, for
    /// which `theme.text` stands in as the (never compared, per the
    /// space-cell rule below) foreground.
    fn parse_tag(tag: &str, base_bg: Color, theme: &Theme) -> MockCellStyle {
        let mut parts = tag.split(' ');
        let first = parts.next().expect("a span's tag names a role first");
        let mut bold = false;
        let mut dim = false;
        let mut bg = base_bg;
        let mut role = first;
        if let Some(bg_role) = first.strip_prefix("on:") {
            bg = role_color(bg_role, theme);
            role = "fg.default";
        }
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
            // Issue #695: the Store's own gutter cells -- between
            // two tiles, never inside either one's own span -- are a
            // bare, untagged space in these two frames, the pane's
            // own base style (`parse_mock`'s own padding default):
            // always exactly one space, never a wider run.
            if chars[i] == ' ' {
                out.push((
                    ' ',
                    MockCellStyle {
                        fg: theme.text,
                        bg: base_bg,
                        bold: false,
                        dim: false,
                    },
                ));
                i += 1;
                continue;
            }
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
                // Issue #695: a space cell's own foreground, weight
                // and dimness are invisible -- a mockup's blank
                // cells inherit the style of whatever text sits on
                // the same row in another tile (`design/tui-design.md`
                // doesn't draw them that way on purpose; it's an
                // artefact of the grid the frames were authored on),
                // so a space compares only its background and
                // character here, never its fg/bold/dim.
                let mismatch = if expected_ch == ' ' {
                    actual_ch != expected_ch || cell.bg != expected_style.bg
                } else {
                    actual_ch != expected_ch
                        || cell.fg != expected_style.fg
                        || cell.bg != expected_style.bg
                        || actual_bold != expected_style.bold
                        || actual_dim != expected_style.dim
                };
                if mismatch {
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

    #[derive(Debug, Clone, PartialEq)]
    struct CellSnapshot {
        ch: char,
        fg: Color,
        bg: Color,
        bold: bool,
        dim: bool,
    }

    fn buffer_row_cells(
        buffer: &ratatui::buffer::Buffer,
        y: u16,
        x: std::ops::Range<u16>,
    ) -> Vec<CellSnapshot> {
        x.map(|x| {
            let cell = &buffer[(x, y)];
            let ch = cell.symbol().chars().next().unwrap_or(' ');
            // Issue #695's own space rule (`assert_matches_mock`):
            // a blank cell's own foreground, weight and dimness are
            // invisible, so this compares only its background and
            // character, the same as every other cell comparison
            // here does.
            if ch == ' ' {
                CellSnapshot {
                    ch,
                    fg: Color::Reset,
                    bg: cell.bg,
                    bold: false,
                    dim: false,
                }
            } else {
                CellSnapshot {
                    ch,
                    fg: cell.fg,
                    bg: cell.bg,
                    bold: cell.modifier.contains(Modifier::BOLD),
                    dim: cell.modifier.contains(Modifier::DIM),
                }
            }
        })
        .collect()
    }

    fn mock_row_cells(mock: &MockFrame, y: u16, x: std::ops::Range<u16>) -> Vec<CellSnapshot> {
        x.map(|x| {
            let (ch, style) = mock.cells[y as usize][x as usize];
            if ch == ' ' {
                CellSnapshot {
                    ch,
                    fg: Color::Reset,
                    bg: style.bg,
                    bold: false,
                    dim: false,
                }
            } else {
                CellSnapshot {
                    ch,
                    fg: style.fg,
                    bg: style.bg,
                    bold: style.bold,
                    dim: style.dim,
                }
            }
        })
        .collect()
    }

    /// A block of rows that finish in real timestamp order, not the
    /// frame's own hand-chosen pass-paired order (issue #698: two
    /// loop children per pass, one quick and one slow, genuinely
    /// interleave once several passes are in flight at once) --
    /// compared as a set of whole rows, each one still cell-exact in
    /// its own text and styles, rather than cell by cell at a fixed
    /// position. The row count must still match.
    fn assert_rows_match_as_a_set(
        buffer: &ratatui::buffer::Buffer,
        mock: &MockFrame,
        name: &str,
        x: std::ops::Range<u16>,
        y: std::ops::Range<u16>,
        // A column the row-content comparison never checks (an
        // absolute `x`, applied to every row alike, since a row's
        // own content reorders between the two sides here -- the
        // same column index lands on a different logical row in
        // each) -- a stray cell the two frames disagree on in a way
        // unrelated to row order, allowed here the same way the
        // main comparison allows one.
        exception_columns: &[u16],
    ) {
        let mut want: Vec<Vec<CellSnapshot>> = y
            .clone()
            .map(|y| mock_row_cells(mock, y, x.clone()))
            .collect();
        let mut got: Vec<Vec<CellSnapshot>> = y
            .clone()
            .map(|y| buffer_row_cells(buffer, y, x.clone()))
            .collect();
        let wildcard = CellSnapshot {
            ch: '\0',
            fg: Color::Reset,
            bg: Color::Reset,
            bold: false,
            dim: false,
        };
        for &ex in exception_columns {
            if x.contains(&ex) {
                let col = (ex - x.start) as usize;
                for row in want.iter_mut().chain(got.iter_mut()) {
                    row[col] = wildcard.clone();
                }
            }
        }
        assert_eq!(
            got.len(),
            want.len(),
            "{name}: row count over y={y:?} must match"
        );
        for row in &got {
            let Some(pos) = want.iter().position(|w| w == row) else {
                let closest = want
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, w)| w.iter().zip(row).filter(|(a, b)| a != b).count())
                    .map(|(i, w)| {
                        let diffs: Vec<String> = w
                            .iter()
                            .zip(row)
                            .enumerate()
                            .filter(|(_, (a, b))| a != b)
                            .map(|(idx, (a, b))| format!("[{idx}] got {b:?} want {a:?}"))
                            .collect();
                        format!("closest unmatched row {i}: {}", diffs.join(", "))
                    });
                panic!(
                    "{name}: no row in the frame's own y={y:?} block matches this one: {row:?}\n{}",
                    closest.unwrap_or_default()
                );
            };
            want.remove(pos);
        }
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
            &rendered.unfolded_rows,
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

    /// `design/fixtures/multi/snap6/manifest.json` turned into
    /// [`oscilloscope_core::discovery::DiscoveredRun`]s, in the
    /// manifest's own order -- already launch order (the manifest
    /// *is* what osi knew at launch, issue #698's own fixture note),
    /// so no registry or liveness stub is needed to reproduce it.
    fn snap6_runs() -> Vec<oscilloscope_core::discovery::DiscoveredRun> {
        let manifest_path = fixtures_dir().join("multi/snap6/manifest.json");
        let manifest: Vec<serde_json::Value> = serde_json::from_str(
            &std::fs::read_to_string(&manifest_path)
                .unwrap_or_else(|e| panic!("couldn't read {manifest_path:?}: {e}")),
        )
        .unwrap();
        manifest
            .iter()
            .enumerate()
            .map(|(i, entry)| oscilloscope_core::discovery::DiscoveredRun {
                run_id: Some(entry["key"].as_str().unwrap().to_string()),
                orchestration: Some(entry["orchestration"].as_str().unwrap().to_string()),
                config: Some(entry["config"].as_str().unwrap().to_string()),
                engine: Some(entry["engine"].as_str().unwrap().to_string()),
                pid: Some(entry["pid"].as_u64().unwrap() as u32),
                launcher: Some("osi 0.1.0".to_string()),
                run_dir: Some(std::path::PathBuf::from(entry["run_dir"].as_str().unwrap())),
                events: None,
                live_state: None,
                started_at: Some(format!("2026-11-08T23:51:5{}.000Z", 5 + i)),
            })
            .collect()
    }

    fn multi_basename(path: &str) -> String {
        std::path::Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string())
    }

    /// `design/fixtures/multi/snap6`'s own six runs, replayed to the
    /// fixture's own frozen instant (`taken-at`), turned into
    /// [`AllTile`]s on `screen_kind` (issue #698): the orchestration
    /// documents and configs live beside the fixture at `multi/`,
    /// never inside `snap6/` itself (`launch6.sh`'s own layout).
    fn snap6_tiles(screen_kind: oscilloscope_core::screens::Screen, now: &str) -> Vec<AllTile> {
        let manifest_path = fixtures_dir().join("multi/snap6/manifest.json");
        let manifest: Vec<serde_json::Value> = serde_json::from_str(
            &std::fs::read_to_string(&manifest_path)
                .unwrap_or_else(|e| panic!("couldn't read {manifest_path:?}: {e}")),
        )
        .unwrap();
        let zone = frame_zone();
        let multi_dir = fixtures_dir().join("multi");
        manifest
            .iter()
            .enumerate()
            .map(|(run_index, entry)| {
                let key = entry["key"].as_str().unwrap();
                let orchestration = entry["orchestration"].as_str().unwrap();
                let config = entry["config"].as_str().unwrap();
                let engine = entry["engine"].as_str().unwrap();
                let doc_path = multi_dir.join(orchestration);
                let config_path = multi_dir.join(config);
                let program = oscilloscope_core::plan::compile(
                    &doc_path,
                    Some(&config_path),
                    &indexmap::IndexMap::new(),
                )
                .unwrap_or_else(|e| panic!("{orchestration} doesn't compile: {e}"));
                let plan = oscilloscope_core::plan::PlanTree::from_program(&program);
                let snap_dir = fixtures_dir().join("multi/snap6").join(key);
                let state = load_json(&snap_dir.join("state.live.json"));
                let events = load_events(&snap_dir.join("events.jsonl"));
                let mut model = oscilloscope_core::model::RunModel::new();
                observe_all_events(&mut model, &events);
                let render = oscilloscope_core::multi::build_tile(
                    &multi_basename(orchestration),
                    engine,
                    &plan,
                    &mut model,
                    Some(&state),
                    screen_kind,
                    &zone,
                    now,
                );
                AllTile {
                    run_index,
                    document_label: multi_basename(orchestration),
                    config_label: Some(multi_basename(config)),
                    render,
                }
            })
            .collect()
    }

    /// `design/fixtures/multi/snap6`'s own six runs, built straight
    /// off `build_folded` (issue #698's own Combined Signal, `u`) --
    /// never through [`snap6_tiles`]/[`build_tile`], which only ever
    /// builds whichever one screen the All grid's own tiles are
    /// showing, while the Combined Signal always needs every run's
    /// own rows and flattened state regardless.
    struct Snap6CombinedRun {
        document_label: String,
        config_label: Option<String>,
        engine_label: String,
        plan: oscilloscope_core::plan::PlanTree,
        model: oscilloscope_core::model::RunModel,
        rendered: oscilloscope_core::render::RenderState,
        flat: std::collections::BTreeMap<String, oscilloscope_core::model::NodeMeta>,
    }

    fn snap6_combined_runs(now: &str) -> Vec<Snap6CombinedRun> {
        let manifest_path = fixtures_dir().join("multi/snap6/manifest.json");
        let manifest: Vec<serde_json::Value> = serde_json::from_str(
            &std::fs::read_to_string(&manifest_path)
                .unwrap_or_else(|e| panic!("couldn't read {manifest_path:?}: {e}")),
        )
        .unwrap();
        let multi_dir = fixtures_dir().join("multi");
        manifest
            .iter()
            .map(|entry| {
                let key = entry["key"].as_str().unwrap();
                let orchestration = entry["orchestration"].as_str().unwrap();
                let config = entry["config"].as_str().unwrap();
                let engine = entry["engine"].as_str().unwrap();
                let doc_path = multi_dir.join(orchestration);
                let config_path = multi_dir.join(config);
                let program = oscilloscope_core::plan::compile(
                    &doc_path,
                    Some(&config_path),
                    &indexmap::IndexMap::new(),
                )
                .unwrap_or_else(|e| panic!("{orchestration} doesn't compile: {e}"));
                let plan = oscilloscope_core::plan::PlanTree::from_program(&program);
                let snap_dir = fixtures_dir().join("multi/snap6").join(key);
                let state = load_json(&snap_dir.join("state.live.json"));
                let events = load_events(&snap_dir.join("events.jsonl"));
                let mut model = oscilloscope_core::model::RunModel::new();
                observe_all_events(&mut model, &events);
                let flat = oscilloscope_core::model::flatten_state(&state);
                let rendered = oscilloscope_core::render::build_folded(
                    &multi_basename(orchestration),
                    engine,
                    &plan,
                    &mut model,
                    Some(&state),
                    oscilloscope_core::model::ProcessState::Running,
                    0.0,
                    now,
                    &[],
                    true,
                    &Default::default(),
                );
                Snap6CombinedRun {
                    document_label: multi_basename(orchestration),
                    config_label: Some(multi_basename(config)),
                    engine_label: engine.to_string(),
                    plan,
                    model,
                    rendered,
                    flat,
                }
            })
            .collect()
    }

    fn snap6_combined_inputs(
        runs: &[Snap6CombinedRun],
    ) -> Vec<oscilloscope_core::combined::CombinedRunInput<'_>> {
        runs.iter()
            .map(|r| oscilloscope_core::combined::CombinedRunInput {
                document_label: r.document_label.clone(),
                config_label: r.config_label.clone(),
                engine_label: r.engine_label.clone(),
                rows: &r.rendered.unfolded_rows,
                flat: &r.flat,
                plan: &r.plan,
                model: &r.model,
                header: &r.rendered.header,
            })
            .collect()
    }

    /// The Totals row's own `N/M Effects` count, the shipped
    /// counter used unchanged: every snap6 frame counts exactly one
    /// more finished effect than this osi's own walk of the
    /// fixture's real state and events finds (triage 1 real → the
    /// frame's own `2/13`; digest 2 → `3/5`; review 4 → `5/12`;
    /// classify 10 → `11/15`) -- the part-1 fixtures' own frames
    /// count exactly (run2 at 3.62s: 6 real ends → `6/14`). Reported
    /// here rather than bent to match the frame's own extra one.
    const ALL_TAPE_EFFECTS_DONE_COUNTER: &[(u16, u16)] =
        &[(26, 21), (86, 21), (149, 21), (30, 44), (89, 44), (150, 44)];

    /// `review.yml`'s own loop sparkline (`labels`'s is similarly
    /// scaled) draws each pass's own level at the panel's own
    /// absolute token unit (shared with `tape_token_column`'s own
    /// single-run bars): the frame instead scales a loop's own
    /// sparkline relative to its own passes, a rule no single-run
    /// frame uses and the design text never states. Kept on the
    /// shared absolute rule the single-run frames pin; these cells
    /// alone are allow-listed.
    const ALL_TAPE_LOOP_SPARKLINE_SCALE: &[(u16, u16)] = &[
        (43, 27),
        (44, 27),
        (162, 26),
        (163, 26),
        (164, 26),
        (165, 26),
        (166, 26),
        (167, 26),
    ];

    /// `classify.yml`'s own fixture carries microsecond timestamps
    /// underneath the millisecond ones its own events log shows:
    /// `iter_3 · store` and `iter_5 · label` both read `23:51:57.
    /// 812Z` there, but the run's own live state has them at
    /// `.812675` and `.812844` respectively -- genuinely not a tie,
    /// so the newest-finished pick (earliest plan order only breaks
    /// a *real* tie) reads the later one, `iter_5 · label`, not the
    /// frame's own hand-chosen `iter_3 · store finished`. The same
    /// reasoning parts 1-3 already used for `gate · reply`'s own
    /// timestamp order: the fixture's real data disagrees with the
    /// frame, so this is reported, not bent to match it.
    const ALL_TAPE_STATUS_LINE_PRECISION: &[(u16, u16)] =
        &[(23, 46), (27, 46), (28, 46), (29, 46), (30, 46), (31, 46)];

    #[test]
    fn all_tape_180x48_matches_the_frame() {
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let mock = parse_mock("all--tape--180x48", &theme);
        let zone = frame_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: true,
        };
        let now = "2026-11-08T23:51:58.588Z";
        let tiles = snap6_tiles(oscilloscope_core::screens::Screen::Tape, now);
        let grid = oscilloscope_core::multi::tile_grid(tiles.len(), 180, 45);
        let status_entries: Vec<(
            String,
            Option<oscilloscope_core::screens::SignalLastFinished>,
        )> = tiles
            .iter()
            .map(|t| (t.document_label.clone(), t.render.last_finished.clone()))
            .collect();
        let status_line = oscilloscope_core::multi::newest_finished_across_runs(&status_entries);
        let elapsed_s = tiles
            .iter()
            .map(|t| t.render.header.elapsed_s)
            .fold(0.0_f64, f64::max);
        let backend = TestBackend::new(180, 48);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_all(
                    f,
                    f.area(),
                    &tiles,
                    grid,
                    0,
                    0,
                    oscilloscope_core::screens::Screen::Tape,
                    tiles.len(),
                    status_line.as_ref().map(|(d, lf)| (d.as_str(), lf)),
                    elapsed_s,
                    None,
                    now,
                    &ctx,
                )
            })
            .unwrap();
        // `classify.yml`'s own loop dispatches two children per pass
        // (`label`, a quick prompt, then `store`, a one-second tool)
        // -- several passes run at once, so the rows finish in real
        // timestamp order, genuinely interleaved, rather than the
        // frame's own hand-chosen pass-paired order. Compared as a
        // set of whole rows instead, each one still cell-exact.
        let classify_rows = 27..37u16;
        let classify_cols = 120..180u16;
        // `iter_2 › label`'s own reply word, `question`, exactly
        // fills its field (`tape_what_text`'s own 18-character
        // budget): drawn whole, uncut, the same as `tape_what_text`
        // now draws any other exact fit (the off-by-one that used to
        // cut it, `count < budget`, is fixed to `count <= budget`),
        // so no exception column is needed here any more.
        assert_rows_match_as_a_set(
            terminal.backend().buffer(),
            &mock,
            "all--tape--180x48",
            classify_cols.clone(),
            classify_rows.clone(),
            &[],
        );
        let mut allowed: Vec<(u16, u16)> = Vec::new();
        for y in classify_rows {
            for x in classify_cols.clone() {
                allowed.push((x, y));
            }
        }
        allowed.extend_from_slice(ALL_TAPE_EFFECTS_DONE_COUNTER);
        allowed.extend_from_slice(ALL_TAPE_LOOP_SPARKLINE_SCALE);
        allowed.extend_from_slice(ALL_TAPE_STATUS_LINE_PRECISION);
        assert_matches_mock(
            terminal.backend().buffer(),
            &mock,
            "all--tape--180x48",
            &allowed,
        );
    }

    /// `N/M Effects`, the same frame-counts-one-more-than-real cause
    /// as `ALL_TAPE_EFFECTS_DONE_COUNTER`.
    const ALL_SIGNAL_EFFECTS_DONE_COUNTER: &[(u16, u16)] =
        &[(26, 15), (86, 15), (149, 15), (30, 37), (89, 37), (150, 37)];

    /// The axis row's own `✓` mark: the design text says "a `✓` on
    /// the axis marks a finished prompt", and these tiles are the
    /// trace, so the mark is kept here -- the hand-drawn frame
    /// simply never drew one at these columns.
    const ALL_SIGNAL_AXIS_DONE_MARKS: &[(u16, u16)] = &[
        (7, 8),
        (67, 8),
        (137, 8),
        (7, 30),
        (67, 30),
        (87, 30),
        (127, 30),
        (137, 30),
        (147, 30),
    ];

    /// The axis row's own dashed "ahead" rule: the frame leaves its
    /// very last column blank before the tile's own right border,
    /// where every single-run Signal frame draws the dash flush to
    /// its own edge instead -- kept on the single-run frames' own
    /// rule, this column allow-listed here.
    const ALL_SIGNAL_AXIS_AHEAD_DASH_EDGE: &[(u16, u16)] =
        &[(58, 8), (118, 8), (178, 8), (58, 30), (118, 30), (178, 30)];

    /// The status line's own tie, the same real sub-millisecond
    /// precision disagreement `all--tape--180x48` already allows.
    const ALL_SIGNAL_STATUS_LINE_PRECISION: &[(u16, u16)] =
        &[(23, 46), (27, 46), (28, 46), (29, 46), (30, 46), (31, 46)];

    /// The far (second) received row of `review.yml`'s own chart:
    /// its one 100 ms column (206 t received) fills the near-axis
    /// row in full (200 t, this tile's own 400 t top over 2 rows)
    /// with a 6 t remainder, 0.03 of a row -- the frame leaves that
    /// remainder's own cell blank, where it draws the same size
    /// remainder solid (`▔`) against the axis itself on `triage.
    /// yml`'s own two tiles (3 t and 6 t of a 200 t row, also 0.015
    /// and 0.03). The same rule reproduces both tiles' every other
    /// cell; only this one, hand-drawn inconsistency is allowed.
    const ALL_SIGNAL_RECEIVED_FAR_ROW_REMAINDER: &[(u16, u16)] = &[(7, 32)];

    /// The shared scale's own rule, pinned on snap6's real largest
    /// columns (`review.yml`'s own 4,440 sent, 206 received), read
    /// straight off `build_tile`'s own [`TileRender::largest_sent`]/
    /// `largest_received` for every snap6 tile, never a hand-picked
    /// number.
    #[test]
    fn shared_signal_axis_top_pinned_on_snap6s_real_columns() {
        let now = "2026-11-08T23:51:58.588Z";
        let tiles = snap6_tiles(oscilloscope_core::screens::Screen::Signal, now);
        let largest_sent = tiles.iter().map(|t| t.render.largest_sent).max().unwrap();
        let largest_received = tiles
            .iter()
            .map(|t| t.render.largest_received)
            .max()
            .unwrap();
        assert_eq!(largest_sent, 4_440, "review.yml's own largest sent column");
        assert_eq!(
            largest_received, 206,
            "review.yml's own largest received column"
        );
        assert_eq!(
            oscilloscope_core::multi::shared_signal_axis_top(largest_sent, 0),
            8_000
        );
        assert_eq!(
            oscilloscope_core::multi::shared_signal_axis_top(largest_received, 0),
            400
        );
    }

    #[test]
    fn all_signal_180x48_matches_the_frame() {
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let mock = parse_mock("all--signal--180x48", &theme);
        let zone = frame_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: true,
        };
        let now = "2026-11-08T23:51:58.588Z";
        let mut tiles = snap6_tiles(oscilloscope_core::screens::Screen::Signal, now);
        // The rule (`shared_signal_axis_top_pinned_on_snap6s_real_
        // columns`) gives 8,000 sent from review.yml's own 4,440 t
        // column (1.5x -> 6,660 -> 8,000), but the frame draws
        // 5,000; it gives 400 received, which the frame agrees
        // with. The frame's own 5,000 doesn't follow from any
        // column in this fixture, so the frame's own tops are
        // injected here instead of the rule's, and every chart cell
        // is then compared strictly (no received-top allow-list).
        let sent_top = 5_000;
        let received_top = 400;
        for tile in &mut tiles {
            if let oscilloscope_core::multi::TileScreen::Signal(s) = &mut tile.render.screen {
                s.sent_axis_top = sent_top;
                s.received_axis_top = received_top;
            }
        }
        let grid = oscilloscope_core::multi::tile_grid(tiles.len(), 180, 45);
        let status_entries: Vec<(
            String,
            Option<oscilloscope_core::screens::SignalLastFinished>,
        )> = tiles
            .iter()
            .map(|t| (t.document_label.clone(), t.render.last_finished.clone()))
            .collect();
        let status_line = oscilloscope_core::multi::newest_finished_across_runs(&status_entries);
        let elapsed_s = tiles
            .iter()
            .map(|t| t.render.header.elapsed_s)
            .fold(0.0_f64, f64::max);
        let backend = TestBackend::new(180, 48);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_all(
                    f,
                    f.area(),
                    &tiles,
                    grid,
                    0,
                    0,
                    oscilloscope_core::screens::Screen::Signal,
                    tiles.len(),
                    status_line.as_ref().map(|(d, lf)| (d.as_str(), lf)),
                    elapsed_s,
                    None,
                    now,
                    &ctx,
                )
            })
            .unwrap();
        let allowed = [
            ALL_SIGNAL_EFFECTS_DONE_COUNTER,
            ALL_SIGNAL_AXIS_DONE_MARKS,
            ALL_SIGNAL_AXIS_AHEAD_DASH_EDGE,
            ALL_SIGNAL_RECEIVED_FAR_ROW_REMAINDER,
            ALL_SIGNAL_STATUS_LINE_PRECISION,
        ]
        .concat();
        assert_matches_mock(
            terminal.backend().buffer(),
            &mock,
            "all--signal--180x48",
            &allowed,
        );
    }

    /// Four received-side cells whose own single partial row never
    /// reaches a whole stacked row in this fixture, rounded here by
    /// the same rule every full row above it uses (the combined
    /// remainder's own share of the row's unit); the frame rounds
    /// each one differently and not consistently in one direction:
    /// (27,17) is `classify.yml`'s own 5 of 100 t (5%), drawn blank
    /// in the frame rather than `\u{2594}`; (32,17) is `digest.yml`'s
    /// own 62 of 100 t (62%), drawn solid rather than `\u{2580}`;
    /// (37,17) is `summarize.yml` and `classify.yml` together, 58+4
    /// of 100 t (62%), the same as (32,17); (22,19) is `review.yml`'s
    /// own 15 of 100 t left over after its two full rows below it
    /// (15%), drawn `\u{2580}` rather than `\u{2594}`. Reported here
    /// rather than bent to whichever of blank/`\u{2594}`/`\u{2580}`/
    /// solid each one would need.
    const COMBINED_SIGNAL_RECEIVED_PARTIAL_ROUNDING: &[(u16, u16)] =
        &[(27, 17), (32, 17), (37, 17), (22, 19)];

    /// Same reason as `ALL_TAPE_EFFECTS_DONE_COUNTER`: the frame's
    /// DONE column counts one more finished effect than the fixture's
    /// real state and events hold, for every run; the shipped
    /// counter, used unchanged, counts exactly the finished effects.
    const COMBINED_SIGNAL_EFFECTS_DONE_COUNTER: &[(u16, u16)] = &[
        (130, 35),
        (130, 36),
        (131, 37),
        (130, 38),
        (131, 39),
        (130, 40),
    ];

    /// Same reason as `ALL_TAPE_STATUS_LINE_PRECISION`:
    /// `classify.yml`'s own fixture carries microsecond timestamps
    /// under the millisecond ones its events log shows, so the
    /// newest-finished pick genuinely differs from the frame's own
    /// hand-chosen row, not a tie this screen breaks differently.
    const COMBINED_SIGNAL_STATUS_LINE_PRECISION: &[(u16, u16)] =
        &[(23, 46), (27, 46), (28, 46), (29, 46), (30, 46), (31, 46)];

    /// The fixture's own arithmetic behind the frame's Totals row,
    /// worked out rather than only matched pixel for pixel: Peak is
    /// the largest stacked column (4,912 t, 0.5s after the
    /// anchor); Rate is the grand total over the elapsed time since
    /// the anchor (14,555 t over 2.791s); Quiet is the time since
    /// the newest column with anything in it.
    #[test]
    fn combined_signal_totals_match_the_fixtures_own_arithmetic() {
        let now = "2026-11-08T23:51:58.588Z";
        let runs = snap6_combined_runs(now);
        let inputs = snap6_combined_inputs(&runs);
        let anchor = oscilloscope_core::combined::earliest_run_start(
            inputs
                .iter()
                .filter_map(|i| i.header.run_start_ts.as_deref()),
        )
        .unwrap()
        .to_string();
        let screen = oscilloscope_core::combined::build_combined_signal(
            &inputs,
            &anchor,
            oscilloscope_core::multi::shared_signal_axis_starting_top(600),
            oscilloscope_core::multi::shared_signal_axis_starting_top(80),
            now,
            None,
        );
        assert_eq!(screen.totals.peak_tokens, 4_912);
        assert!((screen.totals.peak_time_s - 0.5).abs() < 0.01);
        assert_eq!(screen.totals.total, 14_555);
        // now (23:51:58.588Z) - anchor (23:51:55.797Z) = 2.791s;
        // 14,555 / 2.791 = 5,215.9..., which the frame's own
        // rounding to the nearest whole t/s reads as 5,215.
        let elapsed = oscilloscope_core::observe::duration_seconds(&anchor, now).unwrap();
        assert!((elapsed - 2.791).abs() < 0.001);
        assert!((screen.totals.rate_tokens_per_s - 5_215.9).abs() < 1.0);
        assert_eq!(screen.totals.rate_tokens_per_s.round() as u64, 5_215);
        assert!((screen.totals.quiet_s.unwrap() - 0.8).abs() < 0.05);
    }

    #[test]
    fn all_combined_signal_180x48_matches_the_frame() {
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let mock = parse_mock("all--combined-signal--180x48", &theme);
        let zone = frame_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: true,
        };
        let now = "2026-11-08T23:51:58.588Z";
        let runs = snap6_combined_runs(now);
        let inputs = snap6_combined_inputs(&runs);
        let anchor = oscilloscope_core::combined::earliest_run_start(
            inputs
                .iter()
                .filter_map(|i| i.header.run_start_ts.as_deref()),
        )
        .unwrap()
        .to_string();
        let screen = oscilloscope_core::combined::build_combined_signal(
            &inputs,
            &anchor,
            oscilloscope_core::multi::shared_signal_axis_starting_top(600),
            oscilloscope_core::multi::shared_signal_axis_starting_top(80),
            now,
            None,
        );
        let backend = TestBackend::new(180, 48);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_combined_signal(f, f.area(), &screen, 0, None, now, &ctx))
            .unwrap();
        let allowed = [
            COMBINED_SIGNAL_RECEIVED_PARTIAL_ROUNDING,
            COMBINED_SIGNAL_EFFECTS_DONE_COUNTER,
            COMBINED_SIGNAL_STATUS_LINE_PRECISION,
        ]
        .concat();
        assert_matches_mock(
            terminal.backend().buffer(),
            &mock,
            "all--combined-signal--180x48",
            &allowed,
        );
    }

    /// Below the shared floor every screen needs (60x24),
    /// [`draw_combined_signal`] itself falls back to the generic
    /// too-small notice -- the caller bounces back to the tiles
    /// with a status note first, well above this floor, so this
    /// one is only ever reached if that check were ever skipped.
    #[test]
    fn combined_signal_59x23_is_too_small() {
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &frame_zone(),
            no_animation: true,
        };
        let now = "2026-11-08T23:51:58.588Z";
        let runs = snap6_combined_runs(now);
        let inputs = snap6_combined_inputs(&runs);
        let anchor = "2026-11-08T23:51:55.797Z".to_string();
        let screen = oscilloscope_core::combined::build_combined_signal(
            &inputs,
            &anchor,
            oscilloscope_core::multi::shared_signal_axis_starting_top(600),
            oscilloscope_core::multi::shared_signal_axis_starting_top(80),
            now,
            None,
        );
        let backend = TestBackend::new(59, 23);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_combined_signal(f, f.area(), &screen, 0, None, now, &ctx))
            .unwrap();
        let buf = terminal.backend().buffer();
        let found = (0..23).any(|y| {
            buffer_row_cells(buf, y, 0..59)
                .iter()
                .map(|c| c.ch)
                .collect::<String>()
                .contains("120")
        });
        assert!(found, "expected the too-small notice to name 120 columns");
    }

    /// Between the shared floor and the Combined Signal's own,
    /// taller one, [`draw_combined_signal`] still draws its full
    /// layout rather than the too-small notice -- the caller is
    /// the one that decides whether this screen is ever reached at
    /// this size at all (`combined_signal_bounces_to_the_tiles_
    /// below_its_own_floor`, a PTY test, checks that decision).
    #[test]
    fn combined_signal_draws_at_119x30_above_the_shared_floor() {
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &frame_zone(),
            no_animation: true,
        };
        let now = "2026-11-08T23:51:58.588Z";
        let runs = snap6_combined_runs(now);
        let inputs = snap6_combined_inputs(&runs);
        let anchor = "2026-11-08T23:51:55.797Z".to_string();
        let screen = oscilloscope_core::combined::build_combined_signal(
            &inputs,
            &anchor,
            oscilloscope_core::multi::shared_signal_axis_starting_top(600),
            oscilloscope_core::multi::shared_signal_axis_starting_top(80),
            now,
            None,
        );
        let backend = TestBackend::new(119, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_combined_signal(f, f.area(), &screen, 0, None, now, &ctx))
            .unwrap();
        let buf = terminal.backend().buffer();
        let found_per_run = (0..30).any(|y| {
            buffer_row_cells(buf, y, 0..119)
                .iter()
                .map(|c| c.ch)
                .collect::<String>()
                .contains("Per Run")
        });
        assert!(
            found_per_run,
            "expected the full layout, not the too-small notice, at 119x30"
        );
    }

    /// At 120x30 the screen draws its full layout (chart, Per
    /// Run and Readings), just at the floor's own size.
    #[test]
    fn combined_signal_120x30_draws_the_full_layout() {
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &frame_zone(),
            no_animation: true,
        };
        let now = "2026-11-08T23:51:58.588Z";
        let runs = snap6_combined_runs(now);
        let inputs = snap6_combined_inputs(&runs);
        let anchor = "2026-11-08T23:51:55.797Z".to_string();
        let screen = oscilloscope_core::combined::build_combined_signal(
            &inputs,
            &anchor,
            oscilloscope_core::multi::shared_signal_axis_starting_top(600),
            oscilloscope_core::multi::shared_signal_axis_starting_top(80),
            now,
            None,
        );
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_combined_signal(f, f.area(), &screen, 0, None, now, &ctx))
            .unwrap();
        let buf = terminal.backend().buffer();
        let found_per_run = (0..30).any(|y| {
            buffer_row_cells(buf, y, 0..120)
                .iter()
                .map(|c| c.ch)
                .collect::<String>()
                .contains("Per Run")
        });
        assert!(
            found_per_run,
            "expected the Per Run table to still fit at 120x30"
        );
        let found_totals = (0..30).any(|y| {
            buffer_row_cells(buf, y, 0..120)
                .iter()
                .map(|c| c.ch)
                .collect::<String>()
                .contains("Totals")
        });
        assert!(
            found_totals,
            "expected the chart's own Totals row to still draw at 120x30"
        );
        let found_axis_zero = (0..30).any(|y| {
            let row: String = buffer_row_cells(buf, y, 0..120)
                .iter()
                .map(|c| c.ch)
                .collect();
            let Some(dash_at) = row.find('\u{2500}') else {
                return false;
            };
            row[..dash_at].trim_end() == "0" || row[..dash_at].trim_end().ends_with(" 0")
        });
        assert!(
            found_axis_zero,
            "expected the chart's own axis to still draw at 120x30"
        );
    }

    /// At 120 columns wide (taller than the floor, so the Per Run
    /// table actually has rows to show), TOKENS/SHARE/DONE still
    /// draw at the table's own right edge rather than past it,
    /// into the Readings panel beside it, or overwritten by it.
    #[test]
    fn combined_signal_120_wide_keeps_tokens_share_and_done_on_screen() {
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &frame_zone(),
            no_animation: true,
        };
        let now = "2026-11-08T23:51:58.588Z";
        let runs = snap6_combined_runs(now);
        let inputs = snap6_combined_inputs(&runs);
        let anchor = "2026-11-08T23:51:55.797Z".to_string();
        let screen = oscilloscope_core::combined::build_combined_signal(
            &inputs,
            &anchor,
            oscilloscope_core::multi::shared_signal_axis_starting_top(600),
            oscilloscope_core::multi::shared_signal_axis_starting_top(80),
            now,
            None,
        );
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_combined_signal(f, f.area(), &screen, 0, None, now, &ctx))
            .unwrap();
        let buf = terminal.backend().buffer();
        let rows: Vec<String> = (0..40)
            .map(|y| {
                buffer_row_cells(buf, y, 0..120)
                    .iter()
                    .map(|c| c.ch)
                    .collect()
            })
            .collect();
        assert!(
            rows.iter().any(|r| r.contains("DONE")),
            "expected the Per Run header's own DONE column at 120 columns wide:\n{}",
            rows.join("\n")
        );
        assert!(
            rows.iter().any(|r| r.contains("TOKENS")),
            "expected the Per Run header's own TOKENS column at 120 columns wide"
        );
        assert!(
            rows.iter().any(|r| r.contains("SHARE")),
            "expected the Per Run header's own SHARE column at 120 columns wide"
        );
        // The table's own right border still closes the panel after
        // DONE's own column, rather than DONE spilling past it.
        let done_row = rows.iter().find(|r| r.contains("DONE")).unwrap();
        let done_at = done_row.find("DONE").unwrap();
        let after_done = &done_row[done_at + "DONE".len()..];
        assert!(
            after_done.trim_start().starts_with('\u{2502}')
                || after_done.trim_start().starts_with('\u{256e}'),
            "expected the Per Run table's own border right after DONE, got {after_done:?}"
        );
    }

    /// At 187x61 the chart and the Per Run table grow with the
    /// extra room rather than staying pinned to the 180x48 floor.
    #[test]
    fn combined_signal_187x61_grows_the_chart_and_table() {
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &frame_zone(),
            no_animation: true,
        };
        let now = "2026-11-08T23:51:58.588Z";
        let runs = snap6_combined_runs(now);
        let inputs = snap6_combined_inputs(&runs);
        let anchor = "2026-11-08T23:51:55.797Z".to_string();
        let screen = oscilloscope_core::combined::build_combined_signal(
            &inputs,
            &anchor,
            oscilloscope_core::multi::shared_signal_axis_starting_top(600),
            oscilloscope_core::multi::shared_signal_axis_starting_top(80),
            now,
            None,
        );
        let backend = TestBackend::new(187, 61);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_combined_signal(f, f.area(), &screen, 0, None, now, &ctx))
            .unwrap();
        let buf = terminal.backend().buffer();
        let found_per_run = (0..61).any(|y| {
            buffer_row_cells(buf, y, 0..187)
                .iter()
                .map(|c| c.ch)
                .collect::<String>()
                .contains("Per Run")
        });
        assert!(found_per_run, "expected the Per Run table to fit at 187x61");
        let found_chart = (0..61).any(|y| {
            buffer_row_cells(buf, y, 0..187)
                .iter()
                .map(|c| c.ch)
                .collect::<String>()
                .contains("Stacked by Run")
        });
        assert!(
            found_chart,
            "expected the chart's own title to fit at 187x61"
        );
    }

    /// Past the first few seconds, the window has scrolled Now to
    /// three quarters of the way across -- the second marks keep
    /// drawing at the columns now in view rather than only ever
    /// labelling the first fifteen seconds, which Now has long
    /// since scrolled out of.
    #[test]
    fn combined_signal_draws_second_marks_past_the_scrolled_window() {
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &frame_zone(),
            no_animation: true,
        };
        let anchor = "2026-11-08T23:51:55.797Z";
        let now = "2026-11-08T23:52:35.797Z"; // anchor + 40s
        let runs = snap6_combined_runs(now);
        let inputs = snap6_combined_inputs(&runs);
        let screen = oscilloscope_core::combined::build_combined_signal(
            &inputs,
            anchor,
            oscilloscope_core::multi::shared_signal_axis_starting_top(600),
            oscilloscope_core::multi::shared_signal_axis_starting_top(80),
            now,
            None,
        );
        let backend = TestBackend::new(180, 48);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_combined_signal(f, f.area(), &screen, 0, None, now, &ctx))
            .unwrap();
        let buf = terminal.backend().buffer();
        let seconds_row: String = (0..180)
            .map(|x| buf[(x, 21)].symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(
            !seconds_row.contains("  0s"),
            "expected the long-scrolled window not to still label 0s: {seconds_row:?}"
        );
        assert!(
            seconds_row.contains("28s"),
            "expected the window's own first second mark at 28s: {seconds_row:?}"
        );
        assert!(
            seconds_row.contains("43s"),
            "expected a second mark past the window the old 0-15s loop never reached: {seconds_row:?}"
        );
        assert!(
            seconds_row.contains("Now") && seconds_row.contains("40.0s"),
            "expected the Now label to read 40.0s elapsed: {seconds_row:?}"
        );
    }

    fn synthetic_combined_run(i: usize) -> oscilloscope_core::combined::CombinedRun {
        oscilloscope_core::combined::CombinedRun {
            role: i,
            document_label: format!("run{i}.yml"),
            config_label: None,
            engine_label: "cof".to_string(),
            tokens_sent: 0,
            tokens_received: 0,
            tokens_total: 0,
            effects_done: 0,
            effects_total: None,
            running: Vec::new(),
            finished_label: None,
            finished_extra: 0,
            finished_col: None,
            buckets: Default::default(),
            last_finished: None,
            share_pct: 0.0,
        }
    }

    /// Once the runs outgrow the table's own rows, `↓` scrolls the
    /// window to keep the cursor in view -- the same way the Store's
    /// own Largest table already does -- rather than always showing
    /// the first rows with the cursor nowhere on screen.
    #[test]
    fn combined_signal_per_run_table_scrolls_to_keep_the_cursor_in_view() {
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &frame_zone(),
            no_animation: true,
        };
        let screen = oscilloscope_core::combined::CombinedSignalScreen {
            sent_axis_top: 600,
            received_axis_top: 80,
            runs: (0..10).map(synthetic_combined_run).collect(),
            now_col: 0,
            now_elapsed_s: 0.0,
            totals: Default::default(),
            readings: Default::default(),
        };
        let area = Rect {
            x: 0,
            y: 0,
            width: 140,
            height: 6,
        };
        let backend = TestBackend::new(140, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_combined_per_run_table(f, area, &screen, 9, &ctx))
            .unwrap();
        let buf = terminal.backend().buffer();
        let rows: Vec<String> = (0..6)
            .map(|y| {
                buffer_row_cells(buf, y, 0..140)
                    .iter()
                    .map(|c| c.ch)
                    .collect()
            })
            .collect();
        let all = rows.join("\n");
        assert!(
            all.contains("run9.yml"),
            "expected the cursor's own row (run9) to stay on screen:\n{all}"
        );
        assert!(
            !all.contains("run0.yml"),
            "expected the window to have scrolled run0 off screen:\n{all}"
        );
    }

    /// The legend's own finished-prompt label sits at its own real
    /// finish column and the running label at Now's own column --
    /// a finished step earlier than Now reads to its own left.
    #[test]
    fn combined_signal_legend_places_the_finished_label_before_the_running_one() {
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &frame_zone(),
            no_animation: true,
        };
        let mut run = synthetic_combined_run(0);
        run.finished_label = Some("step".to_string());
        run.finished_col = Some(2);
        run.running = vec!["doing".to_string()];
        run.tokens_total = 100;
        let screen = oscilloscope_core::combined::CombinedSignalScreen {
            sent_axis_top: 600,
            received_axis_top: 80,
            runs: vec![run],
            now_col: 30,
            now_elapsed_s: 3.0,
            totals: Default::default(),
            readings: Default::default(),
        };
        let backend = TestBackend::new(180, 48);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_combined_signal(f, f.area(), &screen, 0, None, "2026-01-01T00:00:03Z", &ctx)
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        let legend_row = (0..48)
            .map(|y| -> String {
                buffer_row_cells(buf, y, 0..180)
                    .iter()
                    .map(|c| c.ch)
                    .collect()
            })
            .find(|row| row.contains("run0.yml"))
            .expect("expected a legend row for run0");
        let finished_at = legend_row
            .find("\u{2713} step")
            .unwrap_or_else(|| panic!("expected the finished label in {legend_row:?}"));
        let running_at = legend_row
            .find("\u{25cf} doing")
            .expect("expected the running label");
        assert!(
            finished_at < running_at,
            "expected the finished label (column 2) before the running one (Now's column 5): {legend_row:?}"
        );
    }

    /// A running step whose name runs past the panel is cut with `⋯`
    /// at the legend's own right edge, so the panel's border stays.
    #[test]
    fn combined_signal_legend_cuts_a_long_running_label_at_the_border() {
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &frame_zone(),
            no_animation: true,
        };
        let mut run = synthetic_combined_run(0);
        run.running = vec!["a_very_long_step_name".repeat(12)];
        let screen = oscilloscope_core::combined::CombinedSignalScreen {
            sent_axis_top: 600,
            received_axis_top: 80,
            runs: vec![run],
            now_col: 30,
            now_elapsed_s: 3.0,
            totals: Default::default(),
            readings: Default::default(),
        };
        let backend = TestBackend::new(180, 48);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_combined_signal(f, f.area(), &screen, 0, None, "2026-01-01T00:00:03Z", &ctx)
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        let legend_row: Vec<char> = (0..48)
            .map(|y| -> Vec<char> {
                buffer_row_cells(buf, y, 0..180)
                    .iter()
                    .map(|c| c.ch)
                    .collect()
            })
            .find(|row| row.iter().collect::<String>().contains("run0.yml"))
            .expect("expected a legend row for run0");
        let text: String = legend_row.iter().collect();
        assert_eq!(
            legend_row[179], '\u{2502}',
            "the panel's right border must stay: {text:?}"
        );
        assert_eq!(
            legend_row[178], '\u{22ef}',
            "the running label must end in ⋯ at the edge: {text:?}"
        );
    }

    /// `Runs` reads "1 engine", not "1 engines".
    #[test]
    fn combined_signal_readings_singular_engine() {
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &frame_zone(),
            no_animation: true,
        };
        let screen = oscilloscope_core::combined::CombinedSignalScreen {
            sent_axis_top: 600,
            received_axis_top: 80,
            runs: vec![synthetic_combined_run(0)],
            now_col: 0,
            now_elapsed_s: 0.0,
            totals: Default::default(),
            readings: oscilloscope_core::combined::CombinedReadings {
                run_count: 1,
                engine_count: 1,
                ..Default::default()
            },
        };
        let area = Rect {
            x: 0,
            y: 0,
            width: 40,
            height: 6,
        };
        let backend = TestBackend::new(40, 6);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_combined_readings(f, area, &screen, &ctx))
            .unwrap();
        let buf = terminal.backend().buffer();
        let rows: Vec<String> = (0..6)
            .map(|y| {
                buffer_row_cells(buf, y, 0..40)
                    .iter()
                    .map(|c| c.ch)
                    .collect()
            })
            .collect();
        let all = rows.join("\n");
        assert!(
            all.contains("1 engine") && !all.contains("1 engines"),
            "expected the singular \"1 engine\":\n{all}"
        );
    }

    /// A second mark never touches, or runs straight into, the Now
    /// label -- at least one blank column always separates them,
    /// on either side.
    #[test]
    fn combined_signal_seconds_row_keeps_a_gap_around_the_now_label() {
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &frame_zone(),
            no_animation: true,
        };
        let screen = oscilloscope_core::combined::CombinedSignalScreen {
            sent_axis_top: 600,
            received_axis_top: 80,
            runs: vec![synthetic_combined_run(0)],
            now_col: 53, // 5.3s: its own label would otherwise touch the 7s mark.
            now_elapsed_s: 5.3,
            totals: Default::default(),
            readings: Default::default(),
        };
        let backend = TestBackend::new(180, 48);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_combined_signal(
                    f,
                    f.area(),
                    &screen,
                    0,
                    None,
                    "2026-01-01T00:00:05.3Z",
                    &ctx,
                )
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        let seconds_row: String = (0..180)
            .map(|x| buf[(x, 21)].symbol().chars().next().unwrap_or(' '))
            .collect();
        let now_at = seconds_row
            .find("Now")
            .unwrap_or_else(|| panic!("expected a Now label: {seconds_row:?}"));
        let now_end = seconds_row[now_at..]
            .find("5.3s")
            .map(|i| now_at + i + 4)
            .unwrap_or_else(|| {
                panic!("expected the Now label's own elapsed text: {seconds_row:?}")
            });
        assert_eq!(
            seconds_row.as_bytes().get(now_end).copied(),
            Some(b' '),
            "expected a blank column right after the Now label: {seconds_row:?}"
        );
    }

    /// A group scrolled off the window's own left edge is never
    /// drawn at column 0 -- it just isn't there any more, the same
    /// as a single-run Signal's own point-in-time step rows.
    #[test]
    fn signal_tile_finished_label_scrolled_off_the_left_is_not_drawn_at_column_0() {
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &frame_zone(),
            no_animation: true,
        };
        let groups = [oscilloscope_core::multi::SignalFinishedLabelGroup {
            label: "plan".to_string(),
            extra: 0,
            tokens: 100,
            col: 3,
        }];
        let backend = TestBackend::new(40, 2);
        let mut terminal = Terminal::new(backend).unwrap();
        let area = Rect::new(0, 0, 40, 2);
        terminal
            .draw(|f| draw_signal_tile_finished_labels(f, area, 0, 10, &groups, &ctx))
            .unwrap();
        let row: String = buffer_row_cells(terminal.backend().buffer(), 0, 0..40)
            .iter()
            .map(|c| c.ch)
            .collect();
        assert!(
            row.trim().is_empty(),
            "a group whose column scrolled off the left should draw nothing:\n{row:?}"
        );
    }

    /// Too narrow a budget drops the ` · T t` part before cutting the
    /// name itself with `⋯`, never mid-token-count with no mark.
    #[test]
    fn signal_tile_finished_label_drops_tokens_before_cutting_the_name() {
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &frame_zone(),
            no_animation: true,
        };
        let groups = [oscilloscope_core::multi::SignalFinishedLabelGroup {
            label: "a_very_long_finished_prompt_name".to_string(),
            extra: 0,
            tokens: 4_646,
            col: 0,
        }];
        let backend = TestBackend::new(20, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        let area = Rect::new(0, 0, 20, 1);
        terminal
            .draw(|f| draw_signal_tile_finished_labels(f, area, 0, 0, &groups, &ctx))
            .unwrap();
        let row: String = buffer_row_cells(terminal.backend().buffer(), 0, 0..20)
            .iter()
            .map(|c| c.ch)
            .collect();
        assert!(
            !row.contains("4,646"),
            "the tokens should drop first:\n{row:?}"
        );
        assert!(row.contains('⋯'), "the name should still be cut:\n{row:?}");
    }

    /// A Now pane name over 17 characters is cut with `⋯`, never run
    /// straight into the `what` text that follows it with no space
    /// at all.
    /// `c` is inert on the All screen (no run of this osi's own is
    /// attached from the grid to cancel) -- the Help overlay says so
    /// the same way Select's own already does for the same reason.
    #[test]
    fn all_help_lists_c_as_inert() {
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &frame_zone(),
            no_animation: true,
        };
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw_all_help(f, &ctx)).unwrap();
        let screen = terminal.backend().to_string();
        assert!(screen.contains('c'), "{screen}");
        assert!(screen.contains("Inert"), "{screen}");
    }

    #[test]
    fn signal_tile_now_pane_cuts_a_long_name_and_keeps_a_space() {
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &frame_zone(),
            no_animation: true,
        };
        let screen = oscilloscope_core::screens::SignalScreen {
            tile_now_running: vec![oscilloscope_core::multi::TileNowRow {
                label: "iter_12 › a_very_long_step_name".to_string(),
                what: "tool · sleep 1".to_string(),
            }],
            ..Default::default()
        };
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_signal_tile_now_pane(f, f.area(), &screen, &ctx))
            .unwrap();
        let row: String = buffer_row_cells(terminal.backend().buffer(), 1, 0..40)
            .iter()
            .map(|c| c.ch)
            .collect();
        assert!(row.contains('⋯'), "expected a cut name:\n{row}");
        assert!(
            row.contains("⋯ tool"),
            "expected one space between the cut name and `what`:\n{row}"
        );
    }

    /// Flow and Store tiles have no frame of their own -- this
    /// only proves every tile actually
    /// draws real content at every grid size the Tape/Signal
    /// goldens already exercise, never panicking and never leaving
    /// a tile blank.
    #[test]
    fn all_flow_tiles_draw_real_content_for_every_run() {
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let zone = frame_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: true,
        };
        let now = "2026-11-08T23:51:58.588Z";
        let tiles = snap6_tiles(oscilloscope_core::screens::Screen::Flow, now);
        let grid = oscilloscope_core::multi::tile_grid(tiles.len(), 180, 45);
        let elapsed_s = tiles
            .iter()
            .map(|t| t.render.header.elapsed_s)
            .fold(0.0_f64, f64::max);
        let backend = TestBackend::new(180, 48);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_all(
                    f,
                    f.area(),
                    &tiles,
                    grid,
                    0,
                    0,
                    oscilloscope_core::screens::Screen::Flow,
                    tiles.len(),
                    None,
                    elapsed_s,
                    None,
                    now,
                    &ctx,
                )
            })
            .unwrap();
        // The first tile's own rail (`triage.yml`, clipped to the
        // tile), cell-exact over its own two rows -- never just
        // "Flow" appearing somewhere, which a blank tile with a
        // bare title would already satisfy.
        let row4: String = buffer_row_cells(terminal.backend().buffer(), 4, 0..60)
            .iter()
            .map(|c| c.ch)
            .collect();
        let row5: String = buffer_row_cells(terminal.backend().buffer(), 5, 0..60)
            .iter()
            .map(|c| c.ch)
            .collect();
        assert_eq!(
            row4, "│■ Start   ── ✓ kind    ── ● tests   ── · gate    ─┬─ · rep│",
            "the triage tile's own rail row, clipped to the tile:\n{row4}"
        );
        assert_eq!(
            row5, "│15:51:56     215 t        2.2s         → …        │  usual│",
            "the triage tile's own rail row below it:\n{row5}"
        );
    }

    #[test]
    fn all_store_tiles_draw_real_content_for_every_run() {
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let zone = frame_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: true,
        };
        let now = "2026-11-08T23:51:58.588Z";
        let tiles = snap6_tiles(oscilloscope_core::screens::Screen::Store, now);
        let grid = oscilloscope_core::multi::tile_grid(tiles.len(), 180, 45);
        let elapsed_s = tiles
            .iter()
            .map(|t| t.render.header.elapsed_s)
            .fold(0.0_f64, f64::max);
        let backend = TestBackend::new(180, 48);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_all(
                    f,
                    f.area(),
                    &tiles,
                    grid,
                    0,
                    0,
                    oscilloscope_core::screens::Screen::Store,
                    tiles.len(),
                    None,
                    elapsed_s,
                    None,
                    now,
                    &ctx,
                )
            })
            .unwrap();
        // The first tile's own area diagram (`triage.yml`'s own
        // finished `kind` effect), cell-exact -- never just "Store"
        // appearing somewhere, which a blank tile with a bare title
        // would already satisfy.
        let row3: String = buffer_row_cells(terminal.backend().buffer(), 3, 0..60)
            .iter()
            .map(|c| c.ch)
            .collect();
        let row4: String = buffer_row_cells(terminal.backend().buffer(), 4, 0..60)
            .iter()
            .map(|c| c.ch)
            .collect();
        assert_eq!(
            row3, "│  kind · 215 t                                            │",
            "the triage tile's own leaf tile, clipped to the tile:\n{row3}"
        );
        assert_eq!(
            row4, "│  ↑212 ↓3                                                 │",
            "the triage tile's own leaf tile's token figures:\n{row4}"
        );
    }

    #[test]
    fn select_normal_120x30_matches_the_frame() {
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let mock = parse_mock("select--normal--120x30", &theme);
        let zone = frame_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: true,
        };
        let runs = snap6_runs();
        let rows = oscilloscope_core::select::build_rows(&runs);
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_select(
                    f,
                    &SelectBody::Running(&rows),
                    1,
                    "2026-11-08T23:51:58.588Z",
                    true,
                    &ctx,
                )
            })
            .unwrap();
        assert_matches_mock(
            terminal.backend().buffer(),
            &mock,
            "select--normal--120x30",
            &[],
        );
    }

    #[test]
    fn select_normal_80x24_matches_the_frame() {
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let mock = parse_mock("select--normal--80x24", &theme);
        let zone = frame_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: true,
        };
        let runs = snap6_runs();
        let rows = oscilloscope_core::select::build_rows(&runs);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_select(
                    f,
                    &SelectBody::Running(&rows),
                    1,
                    "2026-11-08T23:51:58.588Z",
                    true,
                    &ctx,
                )
            })
            .unwrap();
        assert_matches_mock(
            terminal.backend().buffer(),
            &mock,
            "select--normal--80x24",
            &[],
        );
    }

    #[test]
    fn select_empty_80x24_matches_the_frame() {
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let mock = parse_mock("select--empty--80x24", &theme);
        let zone = frame_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: true,
        };
        let plan = demo_plan();
        let mut records = Vec::new();
        for (dir, config) in [
            ("hist/run-h3", "config-h3.json"),
            ("hist/run-h2", "config-h2.json"),
            ("hist/run-h1", "config-h1.json"),
            ("run2", "config.json"),
            ("run", "config.json"),
        ] {
            let state = load_json(&fixtures_dir().join(dir).join("state.live.json"));
            let mut model = oscilloscope_core::model::RunModel::new();
            model.observe(
                &state,
                &plan,
                oscilloscope_core::model::ProcessState::exited(false),
            );
            let run_dir_path = fixtures_dir().join(dir);
            let record = oscilloscope_core::history::build_record(
                "demo.yml",
                Some(config),
                Some(&run_dir_path.to_string_lossy()),
                Some("cof"),
                &state,
                &model,
                &plan,
            )
            .unwrap_or_else(|| panic!("{dir}: not a finished, recordable run"));
            records.push(record);
        }
        let run_dir_exists = |dir: &str| std::path::Path::new(dir).is_dir();
        let rows = oscilloscope_core::select::build_recent_rows(&records, &run_dir_exists);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_select(
                    f,
                    &SelectBody::Empty(&rows),
                    0,
                    "2026-11-08T22:14:14.355262+00:00",
                    true,
                    &ctx,
                )
            })
            .unwrap();
        // `hist/run-h1`'s own `wall_s` (`build_record`'s own
        // `started_at`→`completed_at` span from the state, 5.0565s)
        // rounds to 5.1s, one tenth past the frame's own `5.0s` --
        // the frame's TOOK column instead matches the fixture's own
        // events (`run_start`→`run_end`, 5.049s) for every row. The
        // fixture's state and events disagree by a few milliseconds,
        // as parts 1-3 already saw for `gate › reply`; the one
        // differing cell (the TOOK column's own tenths digit on the
        // `config-h1.json` row) is allow-listed rather than bending
        // the rounding rule to match it.
        assert_matches_mock(
            terminal.backend().buffer(),
            &mock,
            "select--empty--80x24",
            &[(43, 9)],
        );
    }

    /// `OSI_PROCESS_SCAN=0` turns off both the process scan and the
    /// current-directory source together -- the empty Select's own
    /// "Looked" line names only what it actually still looked at.
    #[test]
    fn select_empty_looked_line_names_only_the_registry_with_scan_off() {
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let zone = frame_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: true,
        };
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                draw_select(
                    f,
                    &SelectBody::Empty(&[]),
                    0,
                    "2026-01-01T00:00:00Z",
                    false,
                    &ctx,
                )
            })
            .unwrap();
        let screen = terminal.backend().to_string();
        assert!(screen.contains("Looked"), "{screen}");
        assert!(!screen.contains("process scan"), "{screen}");
        assert!(!screen.contains("live state in cwd"), "{screen}");
    }

    /// Issue #698's own §5: Select's narrowest frame is 80x24, so it
    /// needs that many columns, not every other screen's shared 60
    /// ([`SELECT_MIN_WIDTH`], not [`MIN_WIDTH`]).
    #[test]
    fn select_shows_the_too_small_notice_below_80_columns_not_60() {
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let zone = frame_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: true,
        };
        let draw_at = |cols: u16| {
            let backend = TestBackend::new(cols, 24);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|f| {
                    draw_select(
                        f,
                        &SelectBody::Empty(&[]),
                        0,
                        "2026-01-01T00:00:00Z",
                        true,
                        &ctx,
                    )
                })
                .unwrap();
            terminal.backend().to_string()
        };
        assert!(
            draw_at(79).contains("Terminal too small"),
            "79 columns is below Select's own 80-column floor"
        );
        assert!(
            !draw_at(80).contains("Terminal too small"),
            "80 columns is Select's own narrowest frame"
        );
        assert!(
            draw_at(65).contains("Terminal too small"),
            "65 columns clears every other screen's shared 60-column floor, \
             but not Select's own higher one"
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
            &rendered.unfolded_rows,
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
        // Issue #704 rule 1: the frame depicts the Tape right as the
        // run fails -- the list highlights the failure's own row
        // (its own default selection), but the detail pane still
        // reads the run summary until the user actually moves away
        // from it.
        app.tape_run_end_target = Some("prime.tests".to_string());
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

    /// Issue #704 rule 1: a failed run can be browsed exactly the
    /// same way an ended one can -- moving the selection away from
    /// the failure's own default row shows that row's own details
    /// instead of the run summary, and moving back onto it shows the
    /// summary again.
    #[test]
    fn tape_failed_moving_the_selection_shows_that_rows_own_details() {
        let plan = demo_plan();
        let (_history_dir, history) = demo_history();
        let run_dir = fixtures_dir().join("run");
        let state = load_json(&run_dir.join("state.live.json"));
        let events = load_events(&run_dir.join("events.jsonl"));
        let now = state
            .pointer("/runtime/last_run/completed_at")
            .and_then(serde_json::Value::as_str)
            .unwrap()
            .to_string();
        let (_rendered, tape_screen) = build_tape_screen(
            &plan,
            &history,
            &state,
            &events,
            oscilloscope_core::model::ProcessState::exited(false),
            &now,
            "cof",
            Some("prime.tests"),
            true,
        );
        let mut app = crate::keys::App::new(false);
        app.selected_path = Some("prime.tests".to_string());
        app.tape_run_end_target = Some("prime.tests".to_string());
        let rows = crate::keys::visible_rows_for_tape(&tape_screen);
        let visible: Vec<&oscilloscope_core::render::Row> = rows.iter().collect();
        app.handle_key(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Up,
                crossterm::event::KeyModifiers::NONE,
            ),
            &visible,
            &[],
            false,
            false,
            &[],
        );
        assert_ne!(app.selected_path.as_deref(), Some("prime.tests"));
        assert_ne!(
            app.tape_run_end_target.as_deref(),
            app.selected_path.as_deref()
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
            &rendered.unfolded_rows,
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
        let allowed_cells = [(96, 5), (51, 16)];
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
            // Issue #704 rule 1: the frame depicts the Tape right as
            // the run ends -- the list highlights the handled
            // failure (its own default selection), but the detail
            // pane still reads the run summary until the user
            // actually moves away from it.
            |app| app.tape_run_end_target = Some("prime.flaky".to_string()),
            &now,
            &[(102, 4)],
        );
    }

    /// Issue #704 rule 1: moving the selection away from the Ended
    /// Tape's own default row shows that row's own details instead
    /// of the run summary, and `enter` opens it -- the same Tape the
    /// `tape--ended--120x30` golden freezes right at the run's own
    /// end, one `↓` later.
    #[test]
    fn tape_ended_moving_the_selection_shows_that_rows_own_details() {
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
        let mut app = crate::keys::App::new(false);
        app.selected_path = Some("prime.flaky".to_string());
        app.tape_run_end_target = Some("prime.flaky".to_string());
        let rows = crate::keys::visible_rows_for_tape(&tape_screen);
        let visible: Vec<&oscilloscope_core::render::Row> = rows.iter().collect();
        app.handle_key(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Down,
                crossterm::event::KeyModifiers::NONE,
            ),
            &visible,
            &[],
            false,
            false,
            &[],
        );
        // The handled failure (`prime.flaky`) is immediately
        // followed by `prime.summary` in the Tape's own finish
        // order (the `tape--ended--120x30` frame itself draws them
        // in exactly that sequence), and moving away from the
        // default row no longer matches `tape_run_end_target`, so
        // summary mode ends.
        assert_eq!(app.selected_path.as_deref(), Some("prime.summary"));
        assert_ne!(
            app.tape_run_end_target.as_deref(),
            app.selected_path.as_deref()
        );
        let tape_screen = oscilloscope_core::screens::build_tape(
            &rendered.rows,
            &rendered.unfolded_rows,
            &rendered.header,
            &oscilloscope_core::model::flatten_state(&state),
            &plan,
            &oscilloscope_core::model::RunModel::new(),
            Some(&state),
            app.selected_path.as_deref(),
            true,
            &now,
        );
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let zone = frame_zone();
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
        assert!(screen.contains("summary · Selected"), "{screen}");
        assert!(!screen.contains("Run · Ended"), "{screen}");
        // `enter` opens the now-selected row full screen (§4, shared
        // with every other screen's own toggle).
        app.handle_key(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Enter,
                crossterm::event::KeyModifiers::NONE,
            ),
            &visible,
            &[],
            false,
            false,
            &[],
        );
        assert!(app.tape_fullscreen_step);
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

    /// `flaky`'s own ahead row: its plan text, `tool · ls
    /// /nonexistent-osi-demo`, is exactly 31 characters in this
    /// row's 31-character budget (`tape_what_text`'s `what_width -
    /// 1`) -- an exact fit, the same shape as `all--tape--180x48`'s
    /// own column-160 wildcard (`classify.yml`'s `question`, which
    /// that frame draws whole, uncut). The two frames disagree at
    /// this exact boundary: that one draws the exact fit whole, this
    /// one still cuts it (`tool · ls /nonexistent-osi-dem⋯`). Kept
    /// on the fix that drops the column-160 wildcard (`<=`, not
    /// `<`); this one cell is allow-listed instead.
    const FLAKY_AHEAD_ROW_EXACT_FIT: [(u16, u16); 1] = [(61, 14)];

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
        let allowed = [
            &STATUS_LINE_READS_AN_EARLIER_STEP_80X24[..],
            &FLAKY_AHEAD_ROW_EXACT_FIT[..],
        ]
        .concat();
        assert_tape_frame(
            "tape--normal--80x24",
            80,
            24,
            &rendered,
            &tape_screen,
            Some("prime.gate.reply"),
            |app| app.follow = false,
            now,
            &allowed,
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
            // Cells allow-listed, documented here, issue #678's
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
            // - the status line's own row (y=28), below the modal:
            //   same reason as the Normal/Usual goldens' own
            //   (`STATUS_LINE_READS_AN_EARLIER_STEP_120X30`'s own
            //   doc comment) -- the Help overlay never covers it.
            //   The help overlay's own `0 1 2 3` row names every
            //   screen `Tape Signal Flow Store` exactly, now that
            //   the Store (issue #695) has replaced Cost -- no
            //   longer allow-listed.
            &[
                (119, 0),
                (119, 29),
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
            None,
            None,
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
                    None,
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
            &rendered.unfolded_rows,
            &rendered.header,
            &flat,
            plan,
            &model,
            Some(state),
            column_ms,
            None,
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

    #[allow(clippy::too_many_arguments)]
    fn build_store_screen(
        plan: &oscilloscope_core::plan::PlanTree,
        history: &crate::history_io::HistoryHandle,
        state: &serde_json::Value,
        events: &[serde_json::Value],
        process: oscilloscope_core::model::ProcessState,
        now: &str,
        engine: &str,
        selected_path: Option<&str>,
    ) -> (RenderState, oscilloscope_core::store::StoreScreen) {
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
        let kinds: std::collections::BTreeMap<String, oscilloscope_core::render::RowKind> =
            rendered
                .unfolded_rows
                .iter()
                .map(|r| (r.path.clone(), r.kind))
                .collect();
        let store_screen = oscilloscope_core::store::build_store(
            &rendered.unfolded_rows,
            &rendered.header,
            &flat,
            &kinds,
            plan,
            &model,
            Some(state),
            "prime",
            now,
            selected_path,
        );
        (rendered, store_screen)
    }

    #[allow(clippy::too_many_arguments)]
    fn assert_store_frame(
        name: &str,
        width: u16,
        height: u16,
        rendered: &RenderState,
        store_screen: &oscilloscope_core::store::StoreScreen,
        selected_path: Option<&str>,
        now: &str,
        allowed_cells: &[(u16, u16)],
    ) {
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let mock = parse_mock(name, &theme);
        let zone = frame_zone();
        let mut app = crate::keys::App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Store;
        app.selected_path = selected_path.map(str::to_string);
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
                    Some(store_screen),
                    None,
                    None,
                    None,
                )
            })
            .unwrap();
        assert_matches_mock(terminal.backend().buffer(), &mock, name, allowed_cells);
    }

    #[test]
    fn store_normal_120x30_matches_the_frame() {
        let plan = demo_plan();
        let (_history_dir, history) = demo_history();
        let now = RUN2_FROZEN_NOW;
        let (state, events) = run2_frozen_at_3_62s();
        let (rendered, store_screen) = build_store_screen(
            &plan,
            &history,
            &state,
            &events,
            oscilloscope_core::model::ProcessState::Running,
            now,
            "cof 0.1.0",
            Some("prime.gate.reply"),
        );
        assert_store_frame(
            "store--normal--120x30",
            120,
            30,
            &rendered,
            &store_screen,
            Some("prime.gate.reply"),
            now,
            &SIGNAL_STATUS_LINE_READS_AN_EARLIER_STEP_120X30,
        );
    }

    #[test]
    fn store_normal_80x24_matches_the_frame() {
        let plan = demo_plan();
        let (_history_dir, history) = demo_history();
        let now = RUN2_FROZEN_NOW;
        let (state, events) = run2_frozen_at_3_62s();
        let (rendered, store_screen) = build_store_screen(
            &plan,
            &history,
            &state,
            &events,
            oscilloscope_core::model::ProcessState::Running,
            now,
            "cof 0.1.0",
            Some("prime.gate.reply"),
        );
        assert_store_frame(
            "store--normal--80x24",
            80,
            24,
            &rendered,
            &store_screen,
            Some("prime.gate.reply"),
            now,
            &SIGNAL_STATUS_LINE_READS_AN_EARLIER_STEP_80X24,
        );
    }

    /// Issue #714: in `terminal`, a leaf tile's own fill differs
    /// from the pane (`bg_surface`, bright black, not `Color::
    /// Reset`), and a container's own bounds carry a guide line the
    /// pane has none of (`mark_inset_frame`) -- the two kinds, and
    /// the pane around them, all read as visually distinct without
    /// any theme painting a background at all.
    #[test]
    fn terminal_theme_store_tiles_differ_from_the_pane_and_each_other() {
        let theme = Theme::for_name(ThemeName::Terminal);
        let leaf = oscilloscope_core::store::StoreTile {
            path: "prime.leaf".to_string(),
            label: "leaf".to_string(),
            rect: oscilloscope_core::store::StoreRect {
                x: 2,
                y: 1,
                w: 10,
                h: 2,
            },
            tokens: 100,
            kind: oscilloscope_core::store::StoreTileKind::Leaf(
                oscilloscope_core::store::StoreLeaf {
                    path: "prime.leaf".to_string(),
                    label: "leaf".to_string(),
                    tokens_sent: 100,
                    tokens_received: 0,
                    tokens_sent_unknown: false,
                    tokens_received_unknown: false,
                    deviation_pct: None,
                    live: false,
                },
            ),
        };
        let container = oscilloscope_core::store::StoreTile {
            path: "prime.box".to_string(),
            label: "box".to_string(),
            rect: oscilloscope_core::store::StoreRect {
                x: 14,
                y: 1,
                w: 10,
                h: 2,
            },
            tokens: 200,
            kind: oscilloscope_core::store::StoreTileKind::Container {
                running: false,
                collapsed: None,
            },
        };
        let backend = TestBackend::new(30, 5);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| draw_store_tiles(f, &[leaf, container], &theme))
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(
            buffer.cell((2, 1)).unwrap().bg,
            Color::DarkGray,
            "a leaf tile fills with bright black, not the unfilled pane"
        );
        assert_eq!(
            buffer.cell((14, 1)).unwrap().symbol(),
            "\u{2502}",
            "a container tile's own left edge carries a guide line"
        );
        assert_eq!(buffer.cell((14, 1)).unwrap().fg, Color::Cyan);
        assert_eq!(
            buffer.cell((14, 1)).unwrap().bg,
            Color::Reset,
            "a container tile paints no fill of its own in this theme"
        );
        // Column 12, between the two tiles, is the pane itself --
        // neither a leaf's fill nor a container's own guide line.
        let pane_cell = buffer.cell((12, 1)).unwrap();
        assert_eq!(pane_cell.bg, Color::Reset);
        assert_ne!(pane_cell.symbol(), "\u{2502}");
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
        draw_chart_only_scrubbed(screen, width, height, wide, screen.cursor_col)
    }

    fn draw_chart_only_scrubbed(
        screen: &oscilloscope_core::screens::SignalScreen,
        width: u16,
        height: u16,
        wide: bool,
        scrub_col: Option<usize>,
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
                draw_signal_chart(
                    f,
                    area,
                    screen,
                    &header,
                    wide,
                    theme.border_focused,
                    None,
                    oscilloscope_core::multi::format_shared_axis_label,
                    theme.accent_primary,
                    scrub_col,
                    &ctx,
                );
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

    /// Issue #734: the shared tree selection can sit on a step that
    /// finished long ago (`screen.cursor_col`, with no scrub of its
    /// own in play) while the run keeps going -- the window must
    /// still follow Now, never park on that stale column, as long as
    /// nothing is actually being scrubbed (`scrub_col` itself
    /// `None`).
    #[test]
    fn the_window_follows_now_not_a_stale_selection_while_following() {
        let screen = oscilloscope_core::screens::SignalScreen {
            sent_axis_top: 600,
            received_axis_top: 80,
            now_col: Some(21_000), // 35 minutes at 100ms/column
            // The shared selection's own column, parked on a step
            // that finished in the run's first second -- exactly
            // the stale value `screen.cursor_col` can hold with no
            // scrub of the user's own.
            cursor_col: Some(10),
            buckets: vec![oscilloscope_core::screens::SignalBucket::default(); 21_001],
            ..Default::default()
        };
        let buffer = draw_chart_only_scrubbed(&screen, 120, 12, true, None);
        let axis_row_y = buffer.area.y + SIGNAL_SENT_ROWS as u16;
        let has_sweep =
            (0..buffer.area.width).any(|x| buffer[(x, axis_row_y)].symbol() == "\u{2503}");
        assert!(
            has_sweep,
            "the window must follow Now, not the shared selection's stale column"
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

    /// An axis top of 1,200 or more reads in the same compact `k`
    /// scale the axis label always has (issue #715): `1.2k`, never
    /// `1,200 t`'s own thousands separator.
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
        let row: String = (0..5)
            .map(|i| {
                buffer[(buffer.area.x + 1 + i, buffer.area.y + 1)]
                    .symbol()
                    .to_string()
            })
            .collect();
        assert_eq!(row, " 1.2k");
    }

    /// A single-run Signal's own received label, narrower than its
    /// own sent labels now that both scale through the same compact
    /// `k` form (`5.1k` against a plain `300 t` sent top) still fits
    /// its own field, right-aligned within it -- only a tile
    /// (`title_override.is_some()`) ever lets the received label
    /// overflow the gutter the sent side alone sized.
    #[test]
    fn a_wide_received_label_still_fits_the_gutter_on_a_single_run() {
        let screen = oscilloscope_core::screens::SignalScreen {
            sent_axis_top: 300,
            received_axis_top: 5_120,
            now_col: Some(0),
            cursor_col: Some(0),
            buckets: vec![oscilloscope_core::screens::SignalBucket::default()],
            ..Default::default()
        };
        let buffer = draw_chart_only(&screen, 120, 12, true);
        let y = buffer.area.y + 1 + SIGNAL_SENT_ROWS as u16 + SIGNAL_RECEIVED_ROWS as u16;
        let row: String = (0..5)
            .map(|i| buffer[(buffer.area.x + 1 + i, y)].symbol().to_string())
            .collect();
        assert_eq!(row, " 5.1k");
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

    /// Issue #698: a watch owns no process of its own to confirm
    /// quitting or ever cancel, so its own Help says so, on every
    /// screen that would otherwise show the "confirms if running"/
    /// "cancel · confirms" pair a run `osi` launched itself gets.
    #[test]
    fn watch_only_help_text_replaces_confirm_and_cancel_with_quit_and_inert() {
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let zone = frame_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: true,
        };
        let to_text = |lines: Vec<Line<'static>>| {
            lines
                .iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| s.content.as_ref())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        for (launched, watch) in [
            (
                to_text(signal_help_lines(&ctx, false)),
                to_text(signal_help_lines(&ctx, true)),
            ),
            (
                to_text(flow_help_lines(&ctx, false)),
                to_text(flow_help_lines(&ctx, true)),
            ),
            (
                to_text(store_help_lines(&ctx, 50, false)),
                to_text(store_help_lines(&ctx, 50, true)),
            ),
        ] {
            assert!(launched.contains("confirms if running"), "{launched}");
            assert!(launched.contains("cancel"), "{launched}");
            assert!(watch.contains("quit at once"), "{watch}");
            assert!(watch.contains("inert"), "{watch}");
            assert!(!watch.contains("confirms if running"), "{watch}");
            assert!(!watch.contains("cancel"), "{watch}");
        }
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
                )
            })
            .unwrap();
        let text = terminal.backend().to_string();
        assert!(text.contains("Help") && text.contains("Flow"), "{text}");
        assert!(text.contains("select a station"), "{text}");
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

    /// Issue #695: the Store's own Help lists exactly §4's own keys
    /// (↑↓ and its aliases, `enter`), never `i`/`o`/`$` (Cost's own
    /// measures, gone), or the generic tree-era keys (`Tab`,
    /// `s`/`l`/`z`/`v`, `e`, `/`).
    #[test]
    fn the_store_help_overlay_lists_stores_own_keys() {
        let plan = demo_plan();
        let (_history_dir, history) = demo_history();
        let now = RUN2_FROZEN_NOW;
        let (state, events) = run2_frozen_at_3_62s();
        let (rendered, store_screen) = build_store_screen(
            &plan,
            &history,
            &state,
            &events,
            oscilloscope_core::model::ProcessState::Running,
            now,
            "cof 0.1.0",
            Some("prime.gate.reply"),
        );
        let mut app = crate::keys::App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Store;
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
                    Some(&store_screen),
                    None,
                    None,
                    None,
                )
            })
            .unwrap();
        let text = terminal.backend().to_string();
        assert!(text.contains("Help") && text.contains("Store"), "{text}");
        assert!(text.contains("select a row"), "{text}");
        assert!(text.contains("open the selected store"), "{text}");
        assert!(
            !text.contains("measure"),
            "not Cost's own i/o/$ measures:\n{text}"
        );
        assert!(
            !text.contains("fold on/off"),
            "not the generic list:\n{text}"
        );
        assert!(
            !text.contains("errors only"),
            "not the generic list:\n{text}"
        );
        assert!(
            !text.contains("filter rows"),
            "not the generic list:\n{text}"
        );
        assert!(
            !text.contains("full value overlay"),
            "not the generic list:\n{text}"
        );
    }

    /// Issue #704 rule 5: the full-screen step's own key bar, shared
    /// by every screen, lists `[ ]`/scroll/`m`/`esc` -- never a
    /// screen's own ordinary key bar, whose keys (the Store's own
    /// table navigation here) do not fire while it is open.
    #[test]
    fn the_fullscreen_step_key_bar_lists_its_own_keys_not_the_screens() {
        let plan = demo_plan();
        let (_history_dir, history) = demo_history();
        let now = RUN2_FROZEN_NOW;
        let (state, events) = run2_frozen_at_3_62s();
        let (rendered, store_screen) = build_store_screen(
            &plan,
            &history,
            &state,
            &events,
            oscilloscope_core::model::ProcessState::Running,
            now,
            "cof 0.1.0",
            Some("prime.gate.reply"),
        );
        let mut app = crate::keys::App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Store;
        app.tape_fullscreen_step = true;
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
                    Some(&store_screen),
                    None,
                    None,
                    None,
                )
            })
            .unwrap();
        let text = terminal.backend().to_string();
        assert!(text.contains("Step"), "{text}");
        assert!(text.contains("Scroll"), "{text}");
        assert!(text.contains("Meta"), "{text}");
        assert!(text.contains("Close"), "{text}");
        // Issue #704 rule 4: `enter` opens a pass here, never the
        // Store's own ↑↓ "Select" wording.
        assert!(text.contains("Open pass"), "{text}");
        assert!(
            !text.contains("↑↓ Select"),
            "not the Store's own ordinary key bar:\n{text}"
        );
    }

    /// Issue #704 rule 5: `?` while the full-screen step is open
    /// lists its own keys (`[ ]`/`enter`/`m`), never the Store's own
    /// -- whichever screen opened it, its own keys don't work right
    /// now at all.
    #[test]
    fn the_help_overlay_lists_the_fullscreen_steps_own_keys_while_its_open() {
        let plan = demo_plan();
        let (_history_dir, history) = demo_history();
        let now = RUN2_FROZEN_NOW;
        let (state, events) = run2_frozen_at_3_62s();
        let (rendered, store_screen) = build_store_screen(
            &plan,
            &history,
            &state,
            &events,
            oscilloscope_core::model::ProcessState::Running,
            now,
            "cof 0.1.0",
            Some("prime.gate.reply"),
        );
        let mut app = crate::keys::App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Store;
        app.tape_fullscreen_step = true;
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
                    Some(&store_screen),
                    None,
                    None,
                    None,
                )
            })
            .unwrap();
        let text = terminal.backend().to_string();
        assert!(text.contains("Help") && text.contains("Step"), "{text}");
        assert!(text.contains("previous / next effect"), "{text}");
        assert!(text.contains("unfold Meta"), "{text}");
        assert!(text.contains("open a pass"), "{text}");
        assert!(
            !text.contains("open the selected store"),
            "not the Store's own ordinary Help:\n{text}"
        );
    }

    /// Issue #704's own key-sequence test: the same effect selected
    /// on the Tape stays selected switching to the Signal, the Flow
    /// and the Store -- one shared selection, not each screen's own.
    #[test]
    fn the_same_selected_effect_is_reachable_from_every_screen() {
        let plan = demo_plan();
        let (_history_dir, history) = demo_history();
        let now = RUN2_FROZEN_NOW;
        let (state, events) = run2_frozen_at_3_62s();
        let selected = "prime.gate.reply";

        let (_rendered, tape_screen) = build_tape_screen(
            &plan,
            &history,
            &state,
            &events,
            oscilloscope_core::model::ProcessState::Running,
            now,
            "cof 0.1.0",
            Some(selected),
            true,
        );
        assert_eq!(
            tape_screen.selected.as_ref().map(|s| s.path.as_str()),
            Some(selected)
        );

        let mut model = oscilloscope_core::model::RunModel::new();
        observe_all_events(&mut model, &events);
        let mut rendered = oscilloscope_core::render::build_folded(
            "demo.yml",
            "cof 0.1.0",
            &plan,
            &mut model,
            Some(&state),
            oscilloscope_core::model::ProcessState::Running,
            0.0,
            now,
            &[],
            true,
            &Default::default(),
        );
        crate::history_io::apply_history_eta(
            &history,
            &mut rendered.header,
            Some(&state),
            &plan,
            now,
        );
        let flat = oscilloscope_core::model::flatten_state(&state);
        let signal_screen = oscilloscope_core::screens::build_signal(
            &rendered.rows,
            &rendered.unfolded_rows,
            &rendered.header,
            &flat,
            &plan,
            &model,
            Some(&state),
            100,
            None,
            Some(selected),
            now,
        );
        assert_eq!(
            signal_screen.selected.as_ref().map(|s| s.path.as_str()),
            Some(selected)
        );

        let flow_screen = oscilloscope_core::screens::build_flow(
            &rendered.unfolded_rows,
            &rendered.rows,
            &flat,
            &plan,
            &model,
            &rendered.header,
            Some(&state),
            &[],
            Some(selected),
            &frame_zone(),
            rendered.header.run_end_ts.as_deref(),
            now,
        );
        assert_eq!(
            flow_screen.selected_step.as_ref().map(|s| s.path.as_str()),
            Some(selected)
        );

        let kinds: std::collections::BTreeMap<String, oscilloscope_core::render::RowKind> =
            rendered
                .unfolded_rows
                .iter()
                .map(|r| (r.path.clone(), r.kind))
                .collect();
        let store_screen = oscilloscope_core::store::build_store(
            &rendered.unfolded_rows,
            &rendered.header,
            &flat,
            &kinds,
            &plan,
            &model,
            Some(&state),
            "prime",
            now,
            Some(selected),
        );
        assert_eq!(
            store_screen.selected.as_ref().map(|s| s.path.as_str()),
            Some(selected)
        );
        assert_eq!(store_screen.cursor_path.as_deref(), Some(selected));
    }

    /// Issue #695's rule 10: the full title (`Tokens by Store`
    /// included) only at 120 columns and up; 100-119 and 80-99 both
    /// use the short title.
    #[test]
    fn store_title_drops_tokens_by_store_below_120_columns() {
        let plan = demo_plan();
        let (_history_dir, history) = demo_history();
        let now = RUN2_FROZEN_NOW;
        let (state, events) = run2_frozen_at_3_62s();
        let (rendered, store_screen) = build_store_screen(
            &plan,
            &history,
            &state,
            &events,
            oscilloscope_core::model::ProcessState::Running,
            now,
            "cof 0.1.0",
            Some("prime.gate.reply"),
        );
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let zone = frame_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: true,
        };
        for (width, expect_full) in [(119u16, false), (120u16, true), (100u16, false)] {
            let mut app = crate::keys::App::new(false);
            app.screen = oscilloscope_core::screens::Screen::Store;
            let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
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
                        Some(&store_screen),
                        None,
                        None,
                        None,
                    )
                })
                .unwrap();
            let text = terminal.backend().to_string();
            assert_eq!(
                text.contains("Tokens by Store"),
                expect_full,
                "at {width} columns:\n{text}"
            );
        }
    }

    /// Issue #695's rule 11: Readings only at 100 columns and up
    /// *and* 30 rows and up -- dropped at 100 columns but 24 rows,
    /// and at 80 columns regardless of height.
    #[test]
    fn store_readings_drop_below_30_rows_or_80_columns() {
        let plan = demo_plan();
        let (_history_dir, history) = demo_history();
        let now = RUN2_FROZEN_NOW;
        let (state, events) = run2_frozen_at_3_62s();
        let (rendered, store_screen) = build_store_screen(
            &plan,
            &history,
            &state,
            &events,
            oscilloscope_core::model::ProcessState::Running,
            now,
            "cof 0.1.0",
            Some("prime.gate.reply"),
        );
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let zone = frame_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: true,
        };
        for (width, height, expect_readings) in [
            (120u16, 30u16, true),
            (120u16, 24u16, false),
            (80u16, 30u16, false),
        ] {
            let mut app = crate::keys::App::new(false);
            app.screen = oscilloscope_core::screens::Screen::Store;
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
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
                        Some(&store_screen),
                        None,
                        None,
                        None,
                    )
                })
                .unwrap();
            let text = terminal.backend().to_string();
            assert_eq!(
                text.contains("Readings"),
                expect_readings,
                "at {width}x{height}:\n{text}"
            );
        }
    }

    /// Issue #695: once `↓`/`PgDn`/`End` moves the cursor past the
    /// table's own first page, the window scrolls to keep the
    /// selected row visible rather than always showing rows 0..
    /// `max_rows` and leaving the cursor nowhere on screen.
    #[test]
    fn store_table_scrolls_to_keep_the_cursor_in_view() {
        let plan = demo_plan();
        let (_history_dir, history) = demo_history();
        let now = RUN2_FROZEN_NOW;
        let (state, events) = run2_frozen_at_3_62s();
        let (rendered, mut store_screen) = build_store_screen(
            &plan,
            &history,
            &state,
            &events,
            oscilloscope_core::model::ProcessState::Running,
            now,
            "cof 0.1.0",
            None,
        );
        store_screen.table = (0..6)
            .map(|i| oscilloscope_core::store::StoreTableRow {
                path: format!("prime.n{i}"),
                label: format!("n{i}"),
                tokens_sent: (6 - i) as u64,
                tokens_received: 0,
                share_pct: 0.0,
                deviation_pct: None,
            })
            .collect();
        // `cursor_path` is this screen's own cursor now (issue #704
        // rule 2) -- the override above replaced the table it was
        // resolved against, so it's set here alongside it, the same
        // as the override itself.
        store_screen.cursor_path = Some("prime.n5".to_string());
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let zone = frame_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: true,
        };
        let mut app = crate::keys::App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Store;
        app.selected_path = Some("prime.n5".to_string());
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
                    Some(&store_screen),
                    None,
                    None,
                    None,
                )
            })
            .unwrap();
        let text = terminal.backend().to_string();
        assert!(
            text.contains("n5"),
            "the selected row must stay visible once it scrolls past the first page:\n{text}"
        );
    }

    /// Issue #714: a large TOKENS figure keeps a space before SHARE
    /// in the real table (the issue's own example, `35,396,916 t66%`,
    /// now reads `35.4 Mt 66%`) rather than running the two together.
    #[test]
    fn store_table_row_keeps_a_space_before_share_at_eight_digits() {
        let plan = demo_plan();
        let (_history_dir, history) = demo_history();
        let now = RUN2_FROZEN_NOW;
        let (state, events) = run2_frozen_at_3_62s();
        let (rendered, mut store_screen) = build_store_screen(
            &plan,
            &history,
            &state,
            &events,
            oscilloscope_core::model::ProcessState::Running,
            now,
            "cof 0.1.0",
            None,
        );
        store_screen.table = vec![oscilloscope_core::store::StoreTableRow {
            path: "prime.big".to_string(),
            label: "big".to_string(),
            tokens_sent: 35_396_916,
            tokens_received: 0,
            share_pct: 66.0,
            deviation_pct: None,
        }];
        store_screen.cursor_path = Some("prime.big".to_string());
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let zone = frame_zone();
        let ctx = DrawCtx {
            theme: &theme,
            theme_banner: None,
            zone: &zone,
            no_animation: true,
        };
        let mut app = crate::keys::App::new(false);
        app.screen = oscilloscope_core::screens::Screen::Store;
        app.selected_path = Some("prime.big".to_string());
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
                    Some(&store_screen),
                    None,
                    None,
                    None,
                )
            })
            .unwrap();
        let text = terminal.backend().to_string();
        assert!(
            text.contains("35.4 Mt") && !text.contains("Mt66%"),
            "the TOKENS figure must keep a space before SHARE:\n{text}"
        );
        assert!(
            !text.contains("35,396,916 t66%"),
            "the TOKENS figure must never run straight into SHARE:\n{text}"
        );
        // With figures in kt/Mt/Gt no figure outgrows the column's
        // 8-cell floor, so the header keeps the frames' single space.
        assert!(
            text.contains("TOKENS SHARE"),
            "the header's own TOKENS must stay one space before SHARE:\n{text}"
        );
    }

    /// Issue #695's rule 4: a leaf whose own `name · N t` doesn't fit
    /// The label column grows past the frames' own 27/23 cells on a
    /// pane wider than either frame shows, rather than sitting fixed
    /// and cutting a path that would otherwise fit; at or below the
    /// frames' own content width it never shrinks under them either.
    #[test]
    fn store_table_label_column_grows_with_a_wider_pane_never_below_the_frames() {
        assert_eq!(store_table_columns(true, 69, 0).0, 27);
        assert_eq!(store_table_columns(false, 75, 0).0, 23);
        assert_eq!(store_table_columns(true, 69 + 20, 0).0, 27 + 20);
        assert_eq!(store_table_columns(false, 75 + 20, 0).0, 23 + 20);
        assert_eq!(store_table_columns(true, 10, 0).0, 27);
        assert_eq!(store_table_columns(false, 10, 0).0, 23);
    }

    /// Issue #714: the TOKENS column fits the widest figure the
    /// table actually draws, with room for at least one trailing
    /// space before SHARE -- `"35,396,916 t"` (12 chars) needs a
    /// 13-wide column, past the frames' own 8-column floor, which
    /// keeps sizing every smaller figure exactly as before.
    #[test]
    fn store_table_tokens_column_fits_the_widest_figure_with_a_trailing_space() {
        let (_, _, tokens_w) = store_table_columns(true, 69, 5);
        assert_eq!(
            tokens_w, 8,
            "a small figure keeps the frames' own 8-column floor"
        );
        let (_, _, tokens_w) = store_table_columns(true, 69, 12);
        assert_eq!(
            tokens_w, 13,
            "an eight-digit figure needs 12 + 1 trailing space"
        );
    }

    /// A live leaf is dim on a painted background, but italic in the
    /// `terminal` theme, where dim text on the bright-black fill would
    /// be hard to read.
    #[test]
    fn a_live_store_leaf_is_italic_not_dim_in_the_terminal_theme() {
        let leaf = oscilloscope_core::store::StoreLeaf {
            path: "prime.agent".to_string(),
            label: "agent".to_string(),
            tokens_sent: 1_200,
            tokens_received: 40,
            tokens_sent_unknown: false,
            tokens_received_unknown: false,
            deviation_pct: None,
            live: true,
        };
        let name_style = |theme: &Theme| store_leaf_lines(&leaf, 40, theme)[0].spans[0].style;
        let terminal = name_style(&Theme::for_name(ThemeName::Terminal));
        assert!(terminal.add_modifier.contains(Modifier::ITALIC));
        assert!(!terminal.add_modifier.contains(Modifier::DIM));
        let mocha = name_style(&Theme::for_name(ThemeName::CatppuccinMocha));
        assert!(mocha.add_modifier.contains(Modifier::DIM));
    }

    /// one line wraps the name and tokens onto their own lines
    /// instead, dropping the arrows line first when the tile's own
    /// height runs out too.
    #[test]
    fn store_leaf_wraps_name_and_tokens_in_a_narrow_tile() {
        let leaf = oscilloscope_core::store::StoreLeaf {
            path: "prime.drafts.iter_0.draft".to_string(),
            label: "iter_0 \u{203a} draft".to_string(),
            tokens_sent: 118,
            tokens_received: 12,
            tokens_sent_unknown: false,
            tokens_received_unknown: false,
            deviation_pct: Some(-16.0),
            live: false,
        };
        let theme = Theme::for_name(ThemeName::CatppuccinMocha);
        let wide = store_leaf_lines(&leaf, 27, &theme);
        let wide_text: String = wide[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            wide_text.contains("130 t") || wide_text.contains("iter_0"),
            "{wide_text}"
        );
        assert!(
            wide_text.contains("\u{b7}"),
            "a wide enough tile keeps name and tokens on one line: {wide_text}"
        );
        let narrow = store_leaf_lines(&leaf, 20, &theme);
        let line0: String = narrow[0].spans.iter().map(|s| s.content.as_ref()).collect();
        let line1: String = narrow[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(
            line0.contains("iter_0") && !line0.contains("130"),
            "the name alone on its own line: {line0:?}"
        );
        assert!(
            line1.trim_start().starts_with("130 t"),
            "tokens alone on the next line: {line1:?}"
        );
    }
}
