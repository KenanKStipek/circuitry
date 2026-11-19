//! `OSI_EXIT_TRACE`: a debugging aid, off unless the environment
//! names a file (issue #825). When set, osi appends one JSON line to
//! that file for its own exit: pid, exit code, phase, and whatever
//! detail that phase has to give (a signal's sender pid and
//! `si_code`, which poll/read/draw check saw a lost terminal, ...).
//! Best-effort throughout -- a failure to open or write the trace
//! file is silently swallowed, the same as this feature being off
//! altogether, since it must never be the reason a real run fails.
//! With the variable unset, nothing here ever runs at all.

use std::io::Write;
use std::sync::{Mutex, OnceLock};

static TRACE_FILE: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();

fn file() -> Option<&'static Mutex<std::fs::File>> {
    TRACE_FILE
        .get_or_init(|| {
            let path = std::env::var_os("OSI_EXIT_TRACE")?;
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .ok()
                .map(Mutex::new)
        })
        .as_ref()
}

/// Appends one trace line, if `OSI_EXIT_TRACE` is set. `detail` is
/// whatever extra context `phase` has to give -- a signal's sender
/// pid and cause, a lost-terminal check's name, an `io::Error`'s own
/// message -- distinct per phase, so it's a bare [`serde_json::Value`]
/// rather than one fixed shape.
pub fn record(phase: &str, code: u8, detail: serde_json::Value) {
    let Some(mutex) = file() else {
        return;
    };
    let line = serde_json::json!({
        "pid": std::process::id(),
        "code": code,
        "phase": phase,
        "detail": detail,
    });
    let Ok(mut text) = serde_json::to_string(&line) else {
        return;
    };
    text.push('\n');
    if let Ok(mut f) = mutex.lock() {
        let _ = f.write_all(text.as_bytes());
        let _ = f.flush();
    }
}
