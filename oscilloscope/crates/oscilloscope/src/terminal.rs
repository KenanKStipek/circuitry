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
#[cfg(not(test))]
use crossterm::terminal::LeaveAlternateScreen;
use crossterm::terminal::{EnterAlternateScreen, disable_raw_mode, enable_raw_mode};

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

/// Wraps whatever panic hook was already installed (std's default,
/// ordinarily) rather than replacing it outright, so the panic
/// message itself is never swallowed — only delayed until the
/// terminal it would otherwise have printed into is gone. Split out
/// from `enter()` (review finding 8) so a test can prove the
/// restore-on-panic behaviour itself without a real terminal:
/// `enter()`'s own `enable_raw_mode`/`EnterAlternateScreen` need one,
/// which `cargo test` never has, but installing and exercising the
/// hook doesn't.
fn install_panic_hook() {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
    }));
}

pub struct TerminalGuard;

impl TerminalGuard {
    pub fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        // Finding 14: raw mode is already on at this point — if
        // entering the alternate screen fails, it must come back off
        // again before returning `Err`, or the caller's own fallback
        // (a plain wait, or the plain supervise loop) runs with the
        // terminal left half in raw mode and `ENTERED` still false
        // (nothing would ever call `restore()` for a guard that was
        // never actually constructed).
        if let Err(err) = execute!(io::stdout(), EnterAlternateScreen) {
            let _ = disable_raw_mode();
            return Err(err);
        }
        ENTERED.store(true, Ordering::SeqCst);
        install_panic_hook();
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
    use std::sync::Mutex;

    /// Every test here reads or writes the shared `ENTERED` static
    /// (and, for the panic-hook one, the process-wide panic hook
    /// too) — serialised so `cargo test`'s own parallel threads can't
    /// race each other on either (review finding 8).
    static LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn restore_clears_the_entered_flag_even_with_no_real_terminal() {
        // `enter()` itself needs a real tty (unavailable in a plain
        // `cargo test` run) and is exercised for real by the
        // pseudo-terminal end-to-end test instead; this proves
        // `restore`'s own bookkeeping half in isolation. Setting
        // `ENTERED` directly, rather than going through `enter()`,
        // keeps this test from ever touching the real terminal.
        let _guard = LOCK.lock().unwrap();
        ENTERED.store(true, Ordering::SeqCst);
        assert!(is_entered());
        restore();
        assert!(!is_entered());
    }

    #[test]
    fn restore_with_nothing_entered_is_a_no_op() {
        let _guard = LOCK.lock().unwrap();
        ENTERED.store(false, Ordering::SeqCst);
        restore();
        assert!(!is_entered());
    }

    #[test]
    fn a_panic_after_entering_is_still_restored_by_the_installed_hook() {
        // Review finding 8: #434 asks for a test proving restore on
        // panic; the installed hook (not a direct `restore()` call)
        // is what has to run here, the same way it would for a real
        // panic during `run_tui`/`run_tui_watch`'s own loop.
        let _guard = LOCK.lock().unwrap();
        ENTERED.store(true, Ordering::SeqCst);
        install_panic_hook();
        let result = std::panic::catch_unwind(|| {
            panic!("deliberate panic to exercise the installed panic hook");
        });
        assert!(result.is_err());
        assert!(
            !is_entered(),
            "the panic hook should have called restore() before unwinding"
        );
    }
}
