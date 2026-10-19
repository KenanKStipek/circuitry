//! The header line's own trace colour (issue #626). The trace text
//! itself (a braille row, or a static `◉` with `--no-animation`)
//! comes from `oscilloscope_core::icon::header_trace_text`, with no
//! terminal code of its own — this is the one place in `oscilloscope`
//! that turns a run status into the trace's own colour.

use oscilloscope_core::model::RunStatus;
use ratatui::style::Color;

use crate::theme::Theme;

pub fn trace_color(status: RunStatus, theme: &Theme) -> Color {
    match status {
        RunStatus::Running | RunStatus::Ok => theme.icon_trace_ok,
        RunStatus::Failed => theme.icon_trace_failed,
        RunStatus::Cancelled | RunStatus::Aborted => theme.icon_trace_cancelled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{ColorMode, ThemeName, resolve};

    #[test]
    fn trace_color_follows_run_status() {
        let theme = resolve(ThemeName::Terminal, ColorMode::TrueColor).theme;
        assert_eq!(trace_color(RunStatus::Running, &theme), theme.icon_trace_ok);
        assert_eq!(trace_color(RunStatus::Ok, &theme), theme.icon_trace_ok);
        assert_eq!(
            trace_color(RunStatus::Failed, &theme),
            theme.icon_trace_failed
        );
        assert_eq!(
            trace_color(RunStatus::Aborted, &theme),
            theme.icon_trace_cancelled
        );
        assert_eq!(
            trace_color(RunStatus::Cancelled, &theme),
            theme.icon_trace_cancelled
        );
    }
}
