//! `TerminalGuard`: raw mode and the alternate screen, entered once
//! and restored on every exit path (issue #434's "Terminal safety" —
//! normal end, osp's own error, a panic, and a forwarded SIGINT/
//! SIGTERM/SIGHUP, which the existing `SignalWatcher`/main-loop
//! machinery already turns into a normal return from the TUI loop
//! rather than an immediate `exit`, so `Drop` alone covers those
//! three). A panic is the one path `Drop` can't be fully trusted for
//! on its own (unwinding runs *through* this guard's own frame only
//! if nothing between the panic and here caught it first) — the panic
//! hook installed by `enter` restores the terminal before the default
//! handler's own message ever tries to print into whatever raw mode
//! and the alternate screen were showing.
//!
//! `osp` itself being `SIGKILL`ed is the one exception (README.md):
//! nothing can run any cleanup code after that at all, terminal
//! restoration included.

use std::io::{self};
use std::panic;
use std::sync::atomic::{AtomicBool, Ordering};

use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, enable_raw_mode};
#[cfg(not(test))]
use crossterm::terminal::{LeaveAlternateScreen, disable_raw_mode};

static ENTERED: AtomicBool = AtomicBool::new(false);

/// Whether a `TerminalGuard` is currently entered — exposed so a test
/// can prove `enter`/`restore` toggle it correctly without a real
/// terminal (the pseudo-terminal end-to-end test is what proves raw
/// mode and the alternate screen themselves are actually restored on
/// a real tty, after a panic or a forwarded signal).
#[allow(dead_code)] // exercised by this module's own unit test only
pub fn is_entered() -> bool {
    ENTERED.load(Ordering::SeqCst)
}

/// Leaves raw mode and the alternate screen. A no-op, deliberately,
/// when nothing is currently entered — called from both the panic
/// hook and `Drop` on every real exit, the second call would
/// otherwise write a redundant (if harmless) `LeaveAlternateScreen`
/// straight to stdout even with no alternate screen ever entered at
/// all, which is exactly what a test exercising this function with no
/// real terminal, and no prior `enter()`, would otherwise do.
pub fn restore() {
    if ENTERED.swap(false, Ordering::SeqCst) {
        #[cfg(not(test))]
        {
            // `cfg(test)` only to keep a unit test (no real tty, and no
            // real `enter()` to match) from writing a live
            // `LeaveAlternateScreen` straight into the test runner's own
            // stdout -- the real escape sequences are only ever worth
            // sending once something upstream actually wrote
            // `EnterAlternateScreen` in the first place, which under test
            // never happens (the pseudo-terminal end-to-end test
            // exercises the real sequence, on a real terminal).
            let _ = disable_raw_mode();
            let _ = execute!(io::stdout(), LeaveAlternateScreen);
        }
    }
}

pub struct TerminalGuard;

impl TerminalGuard {
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        execute!(io::stdout(), EnterAlternateScreen)?;
        ENTERED.store(true, Ordering::SeqCst);

        // Wraps whatever hook was already installed (std's default,
        // ordinarily) rather than replacing it outright, so the panic
        // message itself is never swallowed — only delayed until the
        // terminal it would otherwise have printed into is gone.
        let previous = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            restore();
            previous(info);
        }));

        Ok(TerminalGuard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_clears_the_entered_flag_even_with_no_real_terminal() {
        // `enter()` itself needs a real tty (unavailable in a plain
        // `cargo test` run) and is exercised for real by the
        // pseudo-terminal end-to-end test instead; this proves
        // `restore`'s own bookkeeping half in isolation. Setting
        // `ENTERED` directly, rather than going through `enter()`,
        // keeps this test from ever touching the real terminal.
        ENTERED.store(true, Ordering::SeqCst);
        assert!(is_entered());
        restore();
        assert!(!is_entered());
    }

    #[test]
    fn restore_with_nothing_entered_is_a_no_op() {
        ENTERED.store(false, Ordering::SeqCst);
        restore();
        assert!(!is_entered());
    }
}
