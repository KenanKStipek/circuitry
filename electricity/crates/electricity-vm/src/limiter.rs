//! `Limiter`: the run-wide concurrency limiter a tool/dynamic-tree
//! dispatch acquires a slot from (`core/concurrency.py::
//! RunConcurrencyLimiter`) -- group-then-global order, `waiting_for`
//! reported only while actually blocked.
//!
//! A group's own slot is always acquired before the global one, and
//! released in reverse order on drop -- the fixed order Python's own
//! docstring explains rules out the classic two-lock deadlock, since a
//! leaf blocked on its group has not reached the global semaphore yet.
//! [`Limiter::try_acquire`] is the *additive*, never-blocking twin
//! [`Limiter::acquire`] needs: `Ok(Some(_))` on an immediate grant,
//! `Ok(None)` on a miss (with nothing held -- any resource it did manage
//! to grab before missing the other one is released before it returns),
//! so a caller can write `meta.waiting_for` into the store itself on a
//! miss before falling back to the blocking [`Limiter::acquire`]. This
//! crate never writes the store and never takes an observer callback of
//! its own -- that stays the caller's job (`exec::dynamic`/`exec::tool`,
//! lane B/C).

use electricity_value::Value;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// [`Limiter::acquire`]/[`Limiter::try_acquire`]'s own error -- today,
/// only an unknown `group:` name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimiterError(pub String);

impl fmt::Display for LimiterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for LimiterError {}

/// A held concurrency slot -- releases it on drop. Fields are declared
/// (and so dropped) global-first, group-second: the reverse of
/// acquisition order, matching `core/concurrency.py::RunConcurrencyLimiter.
/// acquire`'s own `for sem in reversed(held): sem.release()`. Releasing
/// two independent semaphores in either order is equally correct; this
/// order is kept only for fidelity with the Python reference.
#[derive(Debug)]
pub struct SlotGuard {
    _global: Option<OwnedSemaphorePermit>,
    _group: Option<OwnedSemaphorePermit>,
}

/// The run-wide concurrency limiter (`core/concurrency.py::
/// RunConcurrencyLimiter`'s own group-then-global acquisition order). A
/// limiter built with no global cap and no groups ([`Limiter::new`])
/// never blocks anything -- Python's own "no `_concurrency_limiter` key
/// means no run-wide limiter is configured" default.
pub struct Limiter {
    global: Option<Arc<Semaphore>>,
    groups: HashMap<String, Arc<Semaphore>>,
}

impl Default for Limiter {
    fn default() -> Self {
        Self::new()
    }
}

impl Limiter {
    /// No global cap, no groups -- every `acquire`/`try_acquire` grants
    /// immediately.
    pub fn new() -> Self {
        Limiter {
            global: None,
            groups: HashMap::new(),
        }
    }

    /// A limiter with a global cap of *max_concurrency* permits (`None`
    /// for no global cap, matching `runtime.max_concurrency` unset) and
    /// one named semaphore per entry in *groups* (`runtime.
    /// concurrency_groups`). Both are taken as already-validated,
    /// positive permit counts -- parsing `runtime.max_concurrency`/
    /// `runtime.concurrency_groups` out of a document's own JSON/YAML
    /// value, with Python's own `"Invalid runtime concurrency
    /// configuration:\n  - ..."` error text, is `electricity-config`'s
    /// job (issue #431's Lane D section), not this crate's.
    pub fn with_limits(
        max_concurrency: Option<usize>,
        groups: impl IntoIterator<Item = (String, usize)>,
    ) -> Self {
        Limiter {
            global: max_concurrency.map(|n| Arc::new(Semaphore::new(n))),
            groups: groups
                .into_iter()
                .map(|(name, limit)| (name, Arc::new(Semaphore::new(limit))))
                .collect(),
        }
    }

    /// Acquires a slot for *group* (`None` for the global-only limit),
    /// blocking until one is free in both the named group's own
    /// semaphore (first) and the global one (second). Python reports
    /// `waiting_for` by writing it straight into `meta` on the store
    /// (`core/concurrency.py::RunConcurrencyLimiter`), not through a
    /// callback -- this method takes neither an observer nor a
    /// `waiting_for` hook of its own; a caller that wants to report a
    /// wait calls [`Limiter::try_acquire`] first and writes the store
    /// itself on a miss, then falls back to this method.
    ///
    /// `Err` only for an unknown *group* name (`core/concurrency.py::
    /// UnknownConcurrencyGroupError`'s own text, word for word).
    pub async fn acquire(&self, group: Option<&str>) -> Result<SlotGuard, LimiterError> {
        let group_sem = self.resolve_group(group)?;
        let group_permit = match group_sem {
            Some(sem) => Some(
                sem.acquire_owned()
                    .await
                    .expect("a Limiter's own semaphores are never closed"),
            ),
            None => None,
        };
        let global_permit = match &self.global {
            Some(sem) => Some(
                Arc::clone(sem)
                    .acquire_owned()
                    .await
                    .expect("a Limiter's own semaphores are never closed"),
            ),
            None => None,
        };
        Ok(SlotGuard {
            _global: global_permit,
            _group: group_permit,
        })
    }

    /// [`Limiter::acquire`]'s non-blocking twin: `Ok(Some(_))` grants a
    /// slot exactly when both the group's own semaphore (if *group* is
    /// `Some`) and the global one would have been free immediately;
    /// `Ok(None)` on a miss in either, with nothing left held -- a group
    /// slot this call did manage to grab before missing the global one
    /// is released again before returning, never left dangling for the
    /// caller to leak.
    pub fn try_acquire(&self, group: Option<&str>) -> Result<Option<SlotGuard>, LimiterError> {
        let group_sem = self.resolve_group(group)?;
        let group_permit = match group_sem {
            Some(sem) => match Arc::clone(&sem).try_acquire_owned() {
                Ok(permit) => Some(permit),
                Err(_) => return Ok(None),
            },
            None => None,
        };
        let global_permit = match &self.global {
            Some(sem) => match Arc::clone(sem).try_acquire_owned() {
                Ok(permit) => Some(permit),
                // `group_permit` (if any) is dropped here, releasing it,
                // since this function never holds one resource while
                // reporting a miss on the other.
                Err(_) => return Ok(None),
            },
            None => None,
        };
        Ok(Some(SlotGuard {
            _global: global_permit,
            _group: group_permit,
        }))
    }

    /// *group*'s own semaphore, or `Err` with the exact
    /// `UnknownConcurrencyGroupError` text Python raises (`group {group!r}
    /// is not defined in runtime.concurrency_groups — known groups: ...`,
    /// `core/concurrency.py::RunConcurrencyLimiter.acquire`).
    fn resolve_group(&self, group: Option<&str>) -> Result<Option<Arc<Semaphore>>, LimiterError> {
        let Some(name) = group else {
            return Ok(None);
        };
        self.groups.get(name).cloned().map(Some).ok_or_else(|| {
            let mut known: Vec<&str> = self.groups.keys().map(String::as_str).collect();
            known.sort_unstable();
            let known = if known.is_empty() {
                "(none configured)".to_string()
            } else {
                known.join(", ")
            };
            LimiterError(format!(
                "group {} is not defined in runtime.concurrency_groups — known groups: {known}.",
                Value::Str(name.to_string()).py_repr()
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn an_unconfigured_limiter_never_blocks() {
        let limiter = Limiter::new();
        let guard = limiter.try_acquire(None).unwrap();
        assert!(guard.is_some());
    }

    #[tokio::test]
    async fn an_unconfigured_limiter_still_rejects_an_unknown_group() {
        let limiter = Limiter::new();
        assert!(limiter.try_acquire(Some("anything")).is_err());
    }

    #[tokio::test]
    async fn an_unknown_group_errors_with_the_known_groups_list() {
        let limiter = Limiter::with_limits(None, [("io".to_string(), 1)]);
        let err = limiter.try_acquire(Some("missing")).unwrap_err();
        assert_eq!(
            err.0,
            "group 'missing' is not defined in runtime.concurrency_groups — known groups: io."
        );
    }

    #[tokio::test]
    async fn try_acquire_grants_a_free_global_slot_without_blocking() {
        let limiter = Limiter::with_limits(Some(1), []);
        let guard = limiter.try_acquire(None).unwrap();
        assert!(guard.is_some());
    }

    #[tokio::test]
    async fn try_acquire_misses_when_the_global_slot_is_held() {
        let limiter = Limiter::with_limits(Some(1), []);
        let held = limiter.acquire(None).await.unwrap();
        assert!(limiter.try_acquire(None).unwrap().is_none());
        drop(held);
    }

    #[tokio::test]
    async fn a_dropped_guard_frees_the_global_slot() {
        let limiter = Limiter::with_limits(Some(1), []);
        let guard = limiter.acquire(None).await.unwrap();
        assert!(limiter.try_acquire(None).unwrap().is_none());
        drop(guard);
        assert!(limiter.try_acquire(None).unwrap().is_some());
    }

    #[tokio::test]
    async fn try_acquire_releases_the_group_slot_on_a_global_miss() {
        let limiter = Limiter::with_limits(Some(1), [("io".to_string(), 1)]);
        // Hold the global slot alone (no group), so a grouped attempt
        // below is bound to miss on the global check *after* it has
        // already grabbed the group's own slot.
        let global_guard = limiter.acquire(None).await.unwrap();
        assert!(limiter.try_acquire(Some("io")).unwrap().is_none());
        drop(global_guard);
        // If the earlier miss had leaked the group's own slot, this
        // would also report a miss -- the group semaphore's only permit
        // would still be held.
        assert!(limiter.try_acquire(Some("io")).unwrap().is_some());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_waiter_blocked_on_a_full_group_does_not_bypass_it_for_the_global_slot() {
        let limiter = Limiter::with_limits(Some(5), [("io".to_string(), 1)]);
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let (done_tx, mut done_rx) = tokio::sync::oneshot::channel();

        let holder = async {
            let guard = limiter.acquire(Some("io")).await.unwrap();
            ready_tx.send(()).unwrap();
            release_rx.await.unwrap();
            drop(guard);
        };
        let waiter = async {
            let _guard = limiter.acquire(Some("io")).await.unwrap();
            done_tx.send(()).unwrap();
        };

        tokio::pin!(holder);
        tokio::pin!(waiter);

        // Drive the holder until it has the slot, then start the waiter
        // and give it a chance to register against the (now full) group
        // semaphore -- cooperative `current_thread` scheduling, no real
        // parallelism, so this ordering is deterministic.
        tokio::select! {
            _ = &mut holder => unreachable!("holder must not finish before being released"),
            _ = ready_rx => {}
        }
        for _ in 0..4 {
            tokio::select! {
                _ = &mut waiter => unreachable!("waiter must not acquire while the group is held"),
                _ = tokio::task::yield_now() => {}
            }
        }
        // Deterministic, not timing-based: the waiter's own channel send
        // has not happened, checked by polling the channel itself rather
        // than racing a sleep against it.
        assert!(matches!(
            done_rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));

        release_tx.send(()).unwrap();
        tokio::select! {
            _ = &mut holder => {}
            _ = tokio::time::sleep(Duration::from_secs(2)) => panic!("holder never finished releasing"),
        }
        tokio::select! {
            _ = &mut waiter => {}
            _ = tokio::time::sleep(Duration::from_secs(2)) => panic!("waiter never acquired after the group was freed"),
        }
        assert!(done_rx.try_recv().is_ok());
    }
}
