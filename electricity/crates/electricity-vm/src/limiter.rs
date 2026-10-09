//! `Limiter`: the run-wide concurrency limiter a tool/dynamic-tree
//! dispatch acquires a slot from (`core/concurrency.py::
//! RunConcurrencyLimiter`) -- group-then-global order, `waiting_for`
//! reported only while actually blocked.
//!
//! Lane A stub: [`Limiter::acquire`] always errors; lane B's own
//! implementation is a real async semaphore (group semaphore first, then
//! the global one, released in reverse order on drop).

use std::fmt;

/// [`Limiter::acquire`]'s own error -- today, always "not implemented".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimiterError(pub String);

impl fmt::Display for LimiterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for LimiterError {}

/// A held concurrency slot -- releases it on drop (lane B's own
/// semaphore-permit-holding implementation; this stub holds nothing).
#[derive(Debug)]
pub struct SlotGuard {
    _private: (),
}

/// The run-wide concurrency limiter (`core/concurrency.py::
/// RunConcurrencyLimiter`'s own group-then-global acquisition order).
#[derive(Default)]
pub struct Limiter {
    _private: (),
}

impl Limiter {
    pub fn new() -> Self {
        Limiter { _private: () }
    }

    /// Acquires a slot for *group* (`None` for the global-only limit) --
    /// lane B's own implementation blocks (reporting `waiting_for` to the
    /// [`crate::observer::RunObserver`] while it does) until a slot is
    /// free, in both the named group's own semaphore and the global one.
    ///
    /// Lane A stub: always `Err`.
    pub async fn acquire(&self, group: Option<&str>) -> Result<SlotGuard, LimiterError> {
        let _ = group;
        Err(LimiterError(
            "electricity_vm::Limiter::acquire is not implemented yet (lane B, issue #431)"
                .to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn acquire_is_a_lane_b_stub() {
        let limiter = Limiter::new();
        let err = limiter.acquire(Some("io")).await.unwrap_err();
        assert!(err.0.contains("lane B"));
    }
}
