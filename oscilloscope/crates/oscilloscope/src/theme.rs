//! Colour themes (DESIGN.md §6.3, issue #535). One [`Theme`] of named
//! colour roles is the *only* place a [`Color`] literal is allowed to
//! live in this crate — every draw function in `tui.rs` reads a
//! colour through a `&Theme`, never writes `Color::Something` of its
//! own, so a new theme (or `NO_COLOR`, or a narrower terminal) is
//! always a complete, consistent repaint rather than a handful of
//! spots someone forgot.
//!
//! `terminal` is the one theme that isn't a fixed palette at all: it
//! reuses the terminal's own ANSI colours (`Color::Yellow`,
//! `Color::Green`, ...) exactly as this TUI always drew them, and
//! paints no background — choosing no theme must look identical to
//! today. Every other theme is a fixed RGB palette, taken from the
//! named project's own published colour specification (cited above
//! each table below), downgraded for the terminal's own colour depth
//! by [`resolve`].

use oscilloscope_core::model::StatusKind;
use ratatui::style::Color;

/// The eleven themes, in the order `--theme`/`OSP_THEME` accept and
/// `t` cycles through (issue #535).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThemeName {
    Terminal,
    Dracula,
    Nord,
    SolarizedDark,
    SolarizedLight,
    GruvboxDark,
    GruvboxLight,
    CatppuccinMocha,
    CatppuccinLatte,
    TokyoNight,
    OneDark,
}

impl ThemeName {
    pub const ALL: [ThemeName; 11] = [
        ThemeName::Terminal,
        ThemeName::Dracula,
        ThemeName::Nord,
        ThemeName::SolarizedDark,
        ThemeName::SolarizedLight,
        ThemeName::GruvboxDark,
        ThemeName::GruvboxLight,
        ThemeName::CatppuccinMocha,
        ThemeName::CatppuccinLatte,
        ThemeName::TokyoNight,
        ThemeName::OneDark,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ThemeName::Terminal => "terminal",
            ThemeName::Dracula => "dracula",
            ThemeName::Nord => "nord",
            ThemeName::SolarizedDark => "solarized-dark",
            ThemeName::SolarizedLight => "solarized-light",
            ThemeName::GruvboxDark => "gruvbox-dark",
            ThemeName::GruvboxLight => "gruvbox-light",
            ThemeName::CatppuccinMocha => "catppuccin-mocha",
            ThemeName::CatppuccinLatte => "catppuccin-latte",
            ThemeName::TokyoNight => "tokyo-night",
            ThemeName::OneDark => "one-dark",
        }
    }

    /// An unknown `--theme`/`OSP_THEME` name: a usage error listing
    /// every valid one, in cycle order.
    pub fn parse(name: &str) -> Result<ThemeName, String> {
        ThemeName::ALL
            .into_iter()
            .find(|t| t.as_str() == name)
            .ok_or_else(|| {
                let names = ThemeName::ALL
                    .iter()
                    .map(|t| t.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("osp: unknown theme {name:?}; choose one of: {names}")
            })
    }

    /// `t`'s own live cycle (issue #535): wraps from the last theme
    /// back to `terminal`.
    pub fn next(self) -> ThemeName {
        let idx = ThemeName::ALL.iter().position(|&t| t == self).unwrap_or(0);
        ThemeName::ALL[(idx + 1) % ThemeName::ALL.len()]
    }
}

/// Every colour role a draw function can ask for (issue #535's "what
/// a theme sets"). `background` is `None` for `terminal` alone — the
/// one theme that paints no background at all.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    pub name: ThemeName,
    pub background: Option<Color>,
    pub text: Color,
    pub border: Color,
    pub border_focused: Color,
    pub dim: Color,
    pub status_pending: Color,
    pub status_running: Color,
    pub status_done: Color,
    pub status_failed: Color,
    pub status_failed_handled: Color,
    pub status_skipped: Color,
    pub status_cancelled: Color,
    pub status_aborted: Color,
    pub error: Color,
    pub token_in: Color,
    pub token_out: Color,
    /// The foreground drawn over a status-coloured fill (the
    /// treemap's own cells) — one role, not a literal, since every
    /// theme's status colours are bright/mid-tone enough that a dark
    /// foreground reads on all of them.
    pub on_accent: Color,
    /// The highlight tint's own base colour (DESIGN.md §535's update
    /// highlights): a named theme fades *this* colour down to
    /// `background` in RGB; `terminal`'s own stepped ramp (`
    /// highlight.rs`) does not read this field at all, since it has
    /// no fixed background to fade toward.
    pub highlight: Color,
    pub icon_bezel: Color,
    pub icon_trace_ok: Color,
    pub icon_trace_failed: Color,
    pub icon_trace_cancelled: Color,
}

impl Theme {
    pub fn status_color(&self, kind: StatusKind) -> Color {
        match kind {
            StatusKind::Pending => self.status_pending,
            StatusKind::Running | StatusKind::LikelyRunning | StatusKind::RunningOrQueued => {
                self.status_running
            }
            StatusKind::Done => self.status_done,
            StatusKind::Failed => self.status_failed,
            StatusKind::FailedHandled => self.status_failed_handled,
            StatusKind::Skipped => self.status_skipped,
            StatusKind::Cancelled => self.status_cancelled,
            StatusKind::Aborted => self.status_aborted,
        }
    }

    pub fn for_name(name: ThemeName) -> Theme {
        match name {
            ThemeName::Terminal => Theme::terminal(),
            ThemeName::Dracula => Theme::dracula(),
            ThemeName::Nord => Theme::nord(),
            ThemeName::SolarizedDark => Theme::solarized_dark(),
            ThemeName::SolarizedLight => Theme::solarized_light(),
            ThemeName::GruvboxDark => Theme::gruvbox_dark(),
            ThemeName::GruvboxLight => Theme::gruvbox_light(),
            ThemeName::CatppuccinMocha => Theme::catppuccin_mocha(),
            ThemeName::CatppuccinLatte => Theme::catppuccin_latte(),
            ThemeName::TokyoNight => Theme::tokyo_night(),
            ThemeName::OneDark => Theme::one_dark(),
        }
    }

    /// Today's look (issue #434/#527), unchanged: the terminal's own
    /// ANSI colours, no background painted. The one theme with no
    /// fixed RGB palette at all.
    fn terminal() -> Theme {
        Theme {
            name: ThemeName::Terminal,
            background: None,
            text: Color::Reset,
            border: Color::Reset,
            border_focused: Color::Cyan,
            dim: Color::DarkGray,
            status_pending: Color::DarkGray,
            status_running: Color::Yellow,
            status_done: Color::Green,
            status_failed: Color::Red,
            status_failed_handled: Color::LightRed,
            status_skipped: Color::DarkGray,
            status_cancelled: Color::Magenta,
            status_aborted: Color::Red,
            error: Color::Red,
            token_in: Color::Cyan,
            token_out: Color::Yellow,
            on_accent: Color::Black,
            highlight: Color::Yellow,
            icon_bezel: Color::DarkGray,
            icon_trace_ok: Color::Green,
            icon_trace_failed: Color::Red,
            icon_trace_cancelled: Color::Yellow,
        }
    }

    /// Source: Dracula's own published colour specification
    /// (the "Dracula" theme project's palette table — Background,
    /// Foreground, Comment, Red, Green, Yellow, Selection). Every
    /// value below is that published palette unchanged; none needed
    /// a contrast adjustment.
    fn dracula() -> Theme {
        let bg = rgb(0x28, 0x2a, 0x36);
        let text = rgb(0xf8, 0xf8, 0xf2);
        let dim = rgb(0x62, 0x72, 0xa4);
        let red = rgb(0xff, 0x55, 0x55);
        let green = rgb(0x50, 0xfa, 0x7b);
        let yellow = rgb(0xf1, 0xfa, 0x8c);
        let purple = rgb(0xbd, 0x93, 0xf9);
        let cyan = rgb(0x8b, 0xe9, 0xfd);
        let selection = rgb(0x44, 0x47, 0x5a);
        Theme {
            name: ThemeName::Dracula,
            background: Some(bg),
            text,
            border: dim,
            border_focused: cyan,
            dim,
            status_pending: dim,
            status_running: yellow,
            status_done: green,
            status_failed: red,
            status_failed_handled: red,
            status_skipped: dim,
            status_cancelled: purple,
            status_aborted: red,
            error: red,
            token_in: cyan,
            token_out: yellow,
            on_accent: Color::Black,
            highlight: selection,
            icon_bezel: dim,
            icon_trace_ok: green,
            icon_trace_failed: red,
            icon_trace_cancelled: yellow,
        }
    }

    /// Source: the Nord theme's own published palette (nord0-nord15:
    /// Polar Night, Snow Storm, Frost and Aurora). `status_failed`
    /// and `dim` are adjusted from nord11/nord3 to clear this crate's
    /// own contrast floor against nord0 (see `theme::tests::
    /// every_named_theme_clears_its_own_contrast_floor`); every other
    /// value is the published palette unchanged.
    fn nord() -> Theme {
        let bg = rgb(0x2e, 0x34, 0x40);
        let text = rgb(0xd8, 0xde, 0xe9);
        let dim = rgb(0x6f, 0x7d, 0x98); // adjusted from nord3 #4c566a
        let red = rgb(0xcf, 0x88, 0x8f); // adjusted from nord11 #bf616a
        let green = rgb(0xa3, 0xbe, 0x8c);
        let yellow = rgb(0xeb, 0xcb, 0x8b);
        let purple = rgb(0xb4, 0x8e, 0xad);
        let cyan = rgb(0x88, 0xc0, 0xd0);
        let border = rgb(0x43, 0x4c, 0x5e);
        Theme {
            name: ThemeName::Nord,
            background: Some(bg),
            text,
            border,
            border_focused: cyan,
            dim,
            status_pending: dim,
            status_running: yellow,
            status_done: green,
            status_failed: red,
            status_failed_handled: red,
            status_skipped: dim,
            status_cancelled: purple,
            status_aborted: red,
            error: red,
            token_in: cyan,
            token_out: yellow,
            on_accent: Color::Black,
            highlight: border,
            icon_bezel: dim,
            icon_trace_ok: green,
            icon_trace_failed: red,
            icon_trace_cancelled: yellow,
        }
    }

    /// Source: the Solarized specification's dark variant (base03
    /// background, base0 default text, the accent column). `text`,
    /// `red` and the highlight tint are adjusted to clear this
    /// crate's own contrast floor against base03; every other value
    /// is the published palette unchanged.
    fn solarized_dark() -> Theme {
        let bg = rgb(0x00, 0x2b, 0x36); // base03
        let text = rgb(0x83, 0x94, 0x96); // base0, already clears 4.5:1
        let dim = rgb(0x5c, 0x74, 0x7b); // adjusted from base01 #586e75
        let red = rgb(0xe4, 0x64, 0x62); // adjusted from #dc322f
        let green = rgb(0x85, 0x99, 0x00);
        let yellow = rgb(0xb5, 0x89, 0x00);
        let violet = rgb(0x6c, 0x71, 0xc4);
        let cyan = rgb(0x2a, 0xa1, 0x98);
        let highlight = rgb(0x06, 0x2f, 0x3a); // adjusted from base02 #073642
        Theme {
            name: ThemeName::SolarizedDark,
            background: Some(bg),
            text,
            border: highlight,
            border_focused: cyan,
            dim,
            status_pending: dim,
            status_running: yellow,
            status_done: green,
            status_failed: red,
            status_failed_handled: red,
            status_skipped: dim,
            status_cancelled: violet,
            status_aborted: red,
            error: red,
            token_in: cyan,
            token_out: yellow,
            on_accent: Color::Black,
            highlight,
            icon_bezel: dim,
            icon_trace_ok: green,
            icon_trace_failed: red,
            icon_trace_cancelled: yellow,
        }
    }

    /// Source: the Solarized specification's light variant (base3
    /// background, base00 default text). `text`, `red`, `green`,
    /// `yellow`, `dim` and the highlight tint are adjusted to clear
    /// this crate's own contrast floor against base3 — Solarized's
    /// own accent column is tuned to read against *both* variants at
    /// once, which this crate's floor is stricter than.
    fn solarized_light() -> Theme {
        let bg = rgb(0xfd, 0xf6, 0xe3); // base3
        let text = rgb(0x60, 0x74, 0x7c); // adjusted from base00 #657b83
        let dim = rgb(0x82, 0x92, 0x92); // adjusted from base1 #93a1a1
        let red = rgb(0xda, 0x29, 0x25); // adjusted from #dc322f
        let green = rgb(0x69, 0x78, 0x00); // adjusted from #859900
        let yellow = rgb(0x8f, 0x6c, 0x00); // adjusted from #b58900
        let violet = rgb(0x6c, 0x71, 0xc4);
        let cyan = rgb(0x2a, 0xa1, 0x98);
        let highlight = rgb(0xf8, 0xf6, 0xee); // adjusted from base2 #eee8d5
        Theme {
            name: ThemeName::SolarizedLight,
            background: Some(bg),
            text,
            border: highlight,
            border_focused: cyan,
            dim,
            status_pending: dim,
            status_running: yellow,
            status_done: green,
            status_failed: red,
            status_failed_handled: red,
            status_skipped: dim,
            status_cancelled: violet,
            status_aborted: red,
            error: red,
            token_in: cyan,
            token_out: yellow,
            on_accent: Color::Black,
            highlight,
            icon_bezel: dim,
            icon_trace_ok: green,
            icon_trace_failed: red,
            icon_trace_cancelled: yellow,
        }
    }

    /// Source: the Gruvbox specification's dark/hard variant (bg0,
    /// fg, and the bright accent column). `status_failed` is
    /// adjusted from the bright red to clear this crate's own
    /// contrast floor against bg0; every other value is the
    /// published palette unchanged.
    fn gruvbox_dark() -> Theme {
        let bg = rgb(0x28, 0x28, 0x28);
        let text = rgb(0xeb, 0xdb, 0xb2);
        let dim = rgb(0x92, 0x83, 0x74);
        let red = rgb(0xfb, 0x53, 0x3f); // adjusted from bright red #fb4934
        let green = rgb(0xb8, 0xbb, 0x26);
        let yellow = rgb(0xfa, 0xbd, 0x2f);
        let purple = rgb(0xd3, 0x86, 0x9b);
        let aqua = rgb(0x8e, 0xc0, 0x7c);
        let border = rgb(0x3c, 0x38, 0x36);
        Theme {
            name: ThemeName::GruvboxDark,
            background: Some(bg),
            text,
            border,
            border_focused: aqua,
            dim,
            status_pending: dim,
            status_running: yellow,
            status_done: green,
            status_failed: red,
            status_failed_handled: red,
            status_skipped: dim,
            status_cancelled: purple,
            status_aborted: red,
            error: red,
            token_in: aqua,
            token_out: yellow,
            on_accent: Color::Black,
            highlight: border,
            icon_bezel: dim,
            icon_trace_ok: green,
            icon_trace_failed: red,
            icon_trace_cancelled: yellow,
        }
    }

    /// Source: the Gruvbox specification's light/hard variant (bg0,
    /// fg, and the neutral accent column — the light variant's own
    /// *faint* column is for UI chrome, not text, so the table's
    /// neutral column is the right comparison). `status_done` and
    /// `status_running` are adjusted to clear this crate's own
    /// contrast floor against bg0; every other value is the
    /// published palette unchanged.
    fn gruvbox_light() -> Theme {
        let bg = rgb(0xfb, 0xf1, 0xc7);
        let text = rgb(0x3c, 0x38, 0x36);
        let dim = rgb(0x7c, 0x6f, 0x64);
        let red = rgb(0x9d, 0x00, 0x06);
        let green = rgb(0x75, 0x70, 0x0e); // adjusted from #79740e
        let yellow = rgb(0x97, 0x63, 0x11); // adjusted from #b57614
        let purple = rgb(0xb1, 0x62, 0x86);
        let aqua = rgb(0x42, 0x7b, 0x58);
        let border = rgb(0xeb, 0xdb, 0xb2);
        Theme {
            name: ThemeName::GruvboxLight,
            background: Some(bg),
            text,
            border,
            border_focused: aqua,
            dim,
            status_pending: dim,
            status_running: yellow,
            status_done: green,
            status_failed: red,
            status_failed_handled: red,
            status_skipped: dim,
            status_cancelled: purple,
            status_aborted: red,
            error: red,
            token_in: aqua,
            token_out: yellow,
            on_accent: Color::Black,
            highlight: border,
            icon_bezel: dim,
            icon_trace_ok: green,
            icon_trace_failed: red,
            icon_trace_cancelled: yellow,
        }
    }

    /// Source: the Catppuccin Mocha specification's own palette
    /// table (base, text, subtext0, red, green, yellow, mauve, teal,
    /// surface0). Every value below is that published palette
    /// unchanged; none needed a contrast adjustment.
    fn catppuccin_mocha() -> Theme {
        let bg = rgb(0x1e, 0x1e, 0x2e);
        let text = rgb(0xcd, 0xd6, 0xf4);
        let dim = rgb(0xa6, 0xad, 0xc8);
        let red = rgb(0xf3, 0x8b, 0xa8);
        let green = rgb(0xa6, 0xe3, 0xa1);
        let yellow = rgb(0xf9, 0xe2, 0xaf);
        let mauve = rgb(0xcb, 0xa6, 0xf7);
        let teal = rgb(0x94, 0xe2, 0xd5);
        let surface0 = rgb(0x31, 0x32, 0x44);
        Theme {
            name: ThemeName::CatppuccinMocha,
            background: Some(bg),
            text,
            border: surface0,
            border_focused: teal,
            dim,
            status_pending: dim,
            status_running: yellow,
            status_done: green,
            status_failed: red,
            status_failed_handled: red,
            status_skipped: dim,
            status_cancelled: mauve,
            status_aborted: red,
            error: red,
            token_in: teal,
            token_out: yellow,
            on_accent: Color::Black,
            highlight: surface0,
            icon_bezel: dim,
            icon_trace_ok: green,
            icon_trace_failed: red,
            icon_trace_cancelled: yellow,
        }
    }

    /// Source: the Catppuccin Latte specification's own palette
    /// table (base, text, subtext0, red, green, yellow, mauve,
    /// surface0). `status_done` and `status_running` are adjusted to
    /// clear this crate's own contrast floor against base; every
    /// other value is the published palette unchanged.
    fn catppuccin_latte() -> Theme {
        let bg = rgb(0xef, 0xf1, 0xf5);
        let text = rgb(0x4c, 0x4f, 0x69);
        let dim = rgb(0x6c, 0x6f, 0x85);
        let red = rgb(0xd2, 0x0f, 0x39);
        let green = rgb(0x32, 0x7d, 0x22); // adjusted from #40a02b
        let yellow = rgb(0x99, 0x62, 0x13); // adjusted from #df8e1b
        let mauve = rgb(0x88, 0x39, 0xef);
        let surface0 = rgb(0xcc, 0xd0, 0xda);
        Theme {
            name: ThemeName::CatppuccinLatte,
            background: Some(bg),
            text,
            border: surface0,
            border_focused: mauve,
            dim,
            status_pending: dim,
            status_running: yellow,
            status_done: green,
            status_failed: red,
            status_failed_handled: red,
            status_skipped: dim,
            status_cancelled: mauve,
            status_aborted: red,
            error: red,
            token_in: mauve,
            token_out: yellow,
            on_accent: Color::Black,
            highlight: surface0,
            icon_bezel: dim,
            icon_trace_ok: green,
            icon_trace_failed: red,
            icon_trace_cancelled: yellow,
        }
    }

    /// Source: the Tokyo Night specification's own palette table
    /// (bg, fg, comment, red, green, yellow, magenta, cyan). `dim` is
    /// adjusted to clear this crate's own contrast floor against bg;
    /// every other value is the published palette unchanged.
    fn tokyo_night() -> Theme {
        let bg = rgb(0x1a, 0x1b, 0x26);
        let text = rgb(0xc0, 0xca, 0xf5);
        let dim = rgb(0x5b, 0x65, 0x91); // adjusted from #565f89
        let red = rgb(0xf7, 0x76, 0x8e);
        let green = rgb(0x9e, 0xce, 0x6a);
        let yellow = rgb(0xe0, 0xaf, 0x68);
        let magenta = rgb(0xbb, 0x9a, 0xf7);
        let cyan = rgb(0x7d, 0xcf, 0xff);
        let border = rgb(0x29, 0x2e, 0x42);
        Theme {
            name: ThemeName::TokyoNight,
            background: Some(bg),
            text,
            border,
            border_focused: cyan,
            dim,
            status_pending: dim,
            status_running: yellow,
            status_done: green,
            status_failed: red,
            status_failed_handled: red,
            status_skipped: dim,
            status_cancelled: magenta,
            status_aborted: red,
            error: red,
            token_in: cyan,
            token_out: yellow,
            on_accent: Color::Black,
            highlight: border,
            icon_bezel: dim,
            icon_trace_ok: green,
            icon_trace_failed: red,
            icon_trace_cancelled: yellow,
        }
    }

    /// Source: the One Dark (Atom) specification's own palette table
    /// (bg, fg, gray/comment, red, green, yellow, purple, cyan).
    /// `dim` and `status_failed` are adjusted to clear this crate's
    /// own contrast floor against bg; every other value is the
    /// published palette unchanged.
    fn one_dark() -> Theme {
        let bg = rgb(0x28, 0x2c, 0x34);
        let text = rgb(0xab, 0xb2, 0xbf);
        let dim = rgb(0x6c, 0x75, 0x84); // adjusted from #5c6370
        let red = rgb(0xe1, 0x70, 0x78); // adjusted from #e06c75
        let green = rgb(0x98, 0xc3, 0x79);
        let yellow = rgb(0xe5, 0xc0, 0x7b);
        let purple = rgb(0xc6, 0x78, 0xdd);
        let cyan = rgb(0x56, 0xb6, 0xc2);
        let border = rgb(0x3b, 0x40, 0x48);
        Theme {
            name: ThemeName::OneDark,
            background: Some(bg),
            text,
            border,
            border_focused: cyan,
            dim,
            status_pending: dim,
            status_running: yellow,
            status_done: green,
            status_failed: red,
            status_failed_handled: red,
            status_skipped: dim,
            status_cancelled: purple,
            status_aborted: red,
            error: red,
            token_in: cyan,
            token_out: yellow,
            on_accent: Color::Black,
            highlight: border,
            icon_bezel: dim,
            icon_trace_ok: green,
            icon_trace_failed: red,
            icon_trace_cancelled: yellow,
        }
    }
}

fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

/// The colour mode a terminal actually supports (DESIGN.md §535's
/// highlights section, reused here for theme depth): detected once
/// at startup from the same three environment signals, in the same
/// priority order, for both the highlight ramp and theme downgrading
/// — so the two never disagree about what this terminal can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    TrueColor,
    Indexed256,
    Basic16,
    NoColor,
}

impl ColorMode {
    pub fn detect(no_color: Option<&str>, colorterm: Option<&str>, term: Option<&str>) -> ColorMode {
        if no_color.is_some_and(|v| !v.is_empty()) {
            return ColorMode::NoColor;
        }
        if let Some(ct) = colorterm {
            let ct = ct.to_ascii_lowercase();
            if ct.contains("truecolor") || ct.contains("24bit") {
                return ColorMode::TrueColor;
            }
        }
        if let Some(t) = term {
            if t.to_ascii_lowercase().contains("256color") {
                return ColorMode::Indexed256;
            }
        }
        ColorMode::Basic16
    }

    pub fn from_env() -> ColorMode {
        ColorMode::detect(
            std::env::var("NO_COLOR").ok().as_deref(),
            std::env::var("COLORTERM").ok().as_deref(),
            std::env::var("TERM").ok().as_deref(),
        )
    }
}

/// `resolve`'s own result: the theme actually in effect (possibly
/// downgraded), plus a note to log when it silently wasn't the one
/// asked for (issue #535: "falls back to `terminal`... and writes one
/// log line saying so").
pub struct Resolved {
    pub theme: Theme,
    pub downgrade_note: Option<String>,
}

/// Applies `mode` to `requested` (issue #535's "colour depth" rules):
/// truecolour keeps a named theme's RGB as-is; 256 colours maps every
/// role to the nearest xterm-256 index; 16 colours falls back to
/// `terminal` (with a log line, unless `terminal` was already
/// requested); `NO_COLOR` always wins, over any theme, with every
/// role reset to the terminal's own default (no colour at all).
pub fn resolve(requested: ThemeName, mode: ColorMode) -> Resolved {
    match mode {
        ColorMode::NoColor => Resolved {
            theme: no_color_theme(requested),
            downgrade_note: None,
        },
        ColorMode::Basic16 if requested != ThemeName::Terminal => Resolved {
            theme: Theme::for_name(ThemeName::Terminal),
            downgrade_note: Some(format!(
                "{} needs at least a 256-colour terminal; falling back to terminal",
                requested.as_str()
            )),
        },
        ColorMode::Indexed256 if requested != ThemeName::Terminal => Resolved {
            theme: downgrade_to_256(Theme::for_name(requested)),
            downgrade_note: None,
        },
        _ => Resolved {
            theme: Theme::for_name(requested),
            downgrade_note: None,
        },
    }
}

/// `NO_COLOR` (issue #535: "no colours whatever the theme"): every
/// role that would otherwise be a colour becomes `Color::Reset` (the
/// terminal's own default foreground, i.e. no override at all) and
/// the background is painted nowhere — structure still reads from
/// the glyphs and modifiers (bold/reversed/dim) this crate already
/// uses regardless of theme, never from colour alone.
fn no_color_theme(requested: ThemeName) -> Theme {
    Theme {
        name: requested,
        background: None,
        text: Color::Reset,
        border: Color::Reset,
        border_focused: Color::Reset,
        dim: Color::Reset,
        status_pending: Color::Reset,
        status_running: Color::Reset,
        status_done: Color::Reset,
        status_failed: Color::Reset,
        status_failed_handled: Color::Reset,
        status_skipped: Color::Reset,
        status_cancelled: Color::Reset,
        status_aborted: Color::Reset,
        error: Color::Reset,
        token_in: Color::Reset,
        token_out: Color::Reset,
        on_accent: Color::Reset,
        highlight: Color::Reset,
        icon_bezel: Color::Reset,
        icon_trace_ok: Color::Reset,
        icon_trace_failed: Color::Reset,
        icon_trace_cancelled: Color::Reset,
    }
}

fn downgrade_to_256(theme: Theme) -> Theme {
    let d = nearest_xterm256_color;
    Theme {
        background: theme.background.map(d),
        text: d(theme.text),
        border: d(theme.border),
        border_focused: d(theme.border_focused),
        dim: d(theme.dim),
        status_pending: d(theme.status_pending),
        status_running: d(theme.status_running),
        status_done: d(theme.status_done),
        status_failed: d(theme.status_failed),
        status_failed_handled: d(theme.status_failed_handled),
        status_skipped: d(theme.status_skipped),
        status_cancelled: d(theme.status_cancelled),
        status_aborted: d(theme.status_aborted),
        error: d(theme.error),
        token_in: d(theme.token_in),
        token_out: d(theme.token_out),
        on_accent: d(theme.on_accent),
        highlight: d(theme.highlight),
        icon_bezel: d(theme.icon_bezel),
        icon_trace_ok: d(theme.icon_trace_ok),
        icon_trace_failed: d(theme.icon_trace_failed),
        icon_trace_cancelled: d(theme.icon_trace_cancelled),
        ..theme
    }
}

fn nearest_xterm256_color(color: Color) -> Color {
    match color {
        Color::Rgb(r, g, b) => Color::Indexed(nearest_xterm256(r, g, b)),
        other => other,
    }
}

/// The nearest xterm-256 palette index to an RGB colour: the
/// standard 6×6×6 colour cube (indices 16-231, steps `[0, 95, 135,
/// 175, 215, 255]` per channel) and the 24-step greyscale ramp
/// (232-255, `8 + 10*i`), picking whichever of the two is closer —
/// a near-grey RGB value reads better snapped to the smooth
/// greyscale ramp than to the coarser cube.
pub fn nearest_xterm256(r: u8, g: u8, b: u8) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let nearest_level = |c: u8| -> usize {
        LEVELS
            .iter()
            .enumerate()
            .min_by_key(|&(_, &l)| (l as i32 - c as i32).abs())
            .map(|(i, _)| i)
            .unwrap()
    };
    let ri = nearest_level(r);
    let gi = nearest_level(g);
    let bi = nearest_level(b);
    let cube_index = 16 + 36 * ri + 6 * gi + bi;
    let cube_distance = (r as i32 - LEVELS[ri] as i32).abs()
        + (g as i32 - LEVELS[gi] as i32).abs()
        + (b as i32 - LEVELS[bi] as i32).abs();

    let near_gray = r.abs_diff(g) < 10 && g.abs_diff(b) < 10 && r.abs_diff(b) < 10;
    if near_gray {
        let avg = (r as i32 + g as i32 + b as i32) / 3;
        let gray_step = (((avg - 8) as f64 / 10.0).round().clamp(0.0, 23.0)) as i32;
        let gray_value = 8 + 10 * gray_step;
        let gray_distance = (avg - gray_value).abs();
        if gray_distance * 3 < cube_distance {
            return (232 + gray_step) as u8;
        }
    }
    cube_index as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAMED_THEMES: [ThemeName; 10] = [
        ThemeName::Dracula,
        ThemeName::Nord,
        ThemeName::SolarizedDark,
        ThemeName::SolarizedLight,
        ThemeName::GruvboxDark,
        ThemeName::GruvboxLight,
        ThemeName::CatppuccinMocha,
        ThemeName::CatppuccinLatte,
        ThemeName::TokyoNight,
        ThemeName::OneDark,
    ];

    #[test]
    fn terminal_paints_no_background() {
        assert_eq!(Theme::terminal().background, None);
    }

    #[test]
    fn every_named_theme_paints_a_background() {
        for name in NAMED_THEMES {
            assert!(
                Theme::for_name(name).background.is_some(),
                "{} should paint a background",
                name.as_str()
            );
        }
    }

    #[test]
    fn parse_round_trips_every_theme_name() {
        for name in ThemeName::ALL {
            assert_eq!(ThemeName::parse(name.as_str()), Ok(name));
        }
    }

    #[test]
    fn parse_an_unknown_name_lists_every_valid_one() {
        let err = ThemeName::parse("bogus").unwrap_err();
        assert_eq!(
            err,
            "osp: unknown theme \"bogus\"; choose one of: terminal, dracula, nord, \
             solarized-dark, solarized-light, gruvbox-dark, gruvbox-light, \
             catppuccin-mocha, catppuccin-latte, tokyo-night, one-dark"
        );
    }

    #[test]
    fn next_cycles_in_order_and_wraps() {
        assert_eq!(ThemeName::Terminal.next(), ThemeName::Dracula);
        assert_eq!(ThemeName::Dracula.next(), ThemeName::Nord);
        assert_eq!(ThemeName::OneDark.next(), ThemeName::Terminal);
    }

    #[test]
    fn color_mode_detection_follows_the_priority_order() {
        assert_eq!(
            ColorMode::detect(Some("1"), Some("truecolor"), Some("xterm-256color")),
            ColorMode::NoColor
        );
        assert_eq!(
            ColorMode::detect(None, Some("truecolor"), Some("xterm")),
            ColorMode::TrueColor
        );
        assert_eq!(
            ColorMode::detect(None, Some("24bit"), None),
            ColorMode::TrueColor
        );
        assert_eq!(
            ColorMode::detect(None, None, Some("xterm-256color")),
            ColorMode::Indexed256
        );
        assert_eq!(ColorMode::detect(None, None, Some("xterm")), ColorMode::Basic16);
        assert_eq!(ColorMode::detect(None, None, None), ColorMode::Basic16);
        // An empty NO_COLOR (unset in spirit) does not count (the
        // convention is "set to anything", but an explicitly empty
        // value is treated the same as unset here).
        assert_eq!(
            ColorMode::detect(Some(""), None, Some("xterm-256color")),
            ColorMode::Indexed256
        );
    }

    #[test]
    fn no_color_overrides_any_requested_theme() {
        let resolved = resolve(ThemeName::Dracula, ColorMode::NoColor);
        assert_eq!(resolved.theme.background, None);
        assert_eq!(resolved.theme.text, Color::Reset);
        assert_eq!(resolved.theme.status_failed, Color::Reset);
        assert!(resolved.downgrade_note.is_none());
    }

    #[test]
    fn basic_16_color_falls_back_to_terminal_with_a_log_line() {
        let resolved = resolve(ThemeName::Dracula, ColorMode::Basic16);
        assert_eq!(resolved.theme.name, ThemeName::Terminal);
        assert!(resolved.downgrade_note.is_some());

        // terminal itself never needs a fallback note, even on a
        // 16-colour terminal.
        let resolved = resolve(ThemeName::Terminal, ColorMode::Basic16);
        assert_eq!(resolved.theme.name, ThemeName::Terminal);
        assert!(resolved.downgrade_note.is_none());
    }

    #[test]
    fn true_color_keeps_a_named_themes_rgb_as_is() {
        let resolved = resolve(ThemeName::Dracula, ColorMode::TrueColor);
        assert_eq!(resolved.theme.background, Some(Color::Rgb(0x28, 0x2a, 0x36)));
    }

    #[test]
    fn indexed_256_maps_every_role_to_an_indexed_color() {
        let resolved = resolve(ThemeName::Dracula, ColorMode::Indexed256);
        assert!(matches!(resolved.theme.background, Some(Color::Indexed(_))));
        assert!(matches!(resolved.theme.text, Color::Indexed(_)));
        assert!(matches!(resolved.theme.status_failed, Color::Indexed(_)));
        assert!(resolved.downgrade_note.is_none());
    }

    #[test]
    fn nearest_xterm256_matches_the_standard_cube_corners() {
        assert_eq!(nearest_xterm256(255, 0, 0), 196);
        assert_eq!(nearest_xterm256(0, 255, 0), 46);
        assert_eq!(nearest_xterm256(0, 0, 255), 21);
        assert_eq!(nearest_xterm256(0, 0, 0), 16);
        assert_eq!(nearest_xterm256(255, 255, 255), 231);
    }

    #[test]
    fn nearest_xterm256_snaps_a_mid_gray_to_the_grayscale_ramp() {
        assert_eq!(nearest_xterm256(128, 128, 128), 244);
    }

    // WCAG 2.1 relative luminance/contrast ratio (the same formula
    // the issue's own acceptance criterion names): used only to
    // check this crate's own fixed palettes in tests, never at
    // runtime.
    fn linear(c: u8) -> f64 {
        let s = c as f64 / 255.0;
        if s <= 0.03928 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    }

    fn luminance(c: Color) -> f64 {
        let Color::Rgb(r, g, b) = c else {
            panic!("expected an RGB colour, got {c:?}")
        };
        0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
    }

    fn contrast_ratio(a: Color, b: Color) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn every_named_theme_clears_its_own_contrast_floor() {
        for name in NAMED_THEMES {
            let theme = Theme::for_name(name);
            let bg = theme.background.expect("named theme paints a background");
            let check = |role: &str, color: Color, min: f64| {
                let ratio = contrast_ratio(color, bg);
                assert!(
                    ratio >= min,
                    "{}: {role} contrast against the background is {ratio:.2}, need {min}",
                    name.as_str()
                );
            };
            check("text", theme.text, 4.5);
            check("status_running", theme.status_running, 4.5);
            check("status_done", theme.status_done, 4.5);
            check("status_failed", theme.status_failed, 4.5);
            check("error", theme.error, 4.5);
            check("dim", theme.dim, 3.0);

            let highlight_ratio = contrast_ratio(theme.text, theme.highlight);
            assert!(
                highlight_ratio >= 4.5,
                "{}: text on the highlight tint at t=0 is {highlight_ratio:.2}, need 4.5",
                name.as_str()
            );
        }
    }
}
