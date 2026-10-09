//! `Limiter`: the run-wide concurrency limiter a *leaf* effect (`tool`,
//! and in a later milestone `prompt`) acquires a slot from before it
//! dispatches (`core/concurrency.py::RunConcurrencyLimiter`) --
//! group-then-global order, `waiting_for` reported only while actually
//! blocked. A container (`dynamic`, `if`, and in a later milestone
//! `loop`/`use`/`reflector`) never acquires a slot itself, only the
//! leaves it eventually dispatches (`core/concurrency.py`'s own module
//! docstring) -- a container that did would be able to hold a slot one
//! of its own children is waiting on, which is exactly the nesting
//! deadlock a `max_concurrency: 1` run would hit immediately.
//!
//! A group's own slot is always acquired before the global one, and
//! released in reverse order on drop -- the fixed order Python's own
//! docstring explains rules out the classic two-lock deadlock, since a
//! leaf blocked on its group has not reached the global semaphore yet.
//! [`Limiter::acquire_reporting`] is the `waiting_for` path: it fires its
//! callback with [`SlotEvent::Waiting`] the moment a resource (the group,
//! then the global one) actually has to block for, and
//! [`SlotEvent::Acquired`] right after that same resource is granted --
//! never for a slot that was immediately free. [`Limiter::acquire`] is
//! the same acquisition with no reporting; [`Limiter::try_acquire`] is
//! the non-blocking twin of that: `Ok(Some(_))` on an immediate grant,
//! `Ok(None)` on a miss (with nothing held -- any resource it did manage
//! to grab before missing the other one is released before it returns).
//! This crate never writes the store itself -- a caller that wants
//! `meta.waiting_for` writes it from inside its own `report` callback
//! (`exec::dynamic`/`exec::tool`, lane B/C); cancellation is the same
//! caller's own `select!` against `token.cancelled()` racing the
//! `acquire`/`acquire_reporting` future, not something this crate
//! polls for on its own (`core/concurrency.py::RunConcurrencyLimiter.
//! _acquire_one`'s own cancellation-token poll loop exists only because
//! Python's blocking `threading.Semaphore.acquire` has no cancellable
//! async equivalent to `select!` against).

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

/// The `on_wait`/`on_acquired` label for the run-wide cap, as opposed to
/// a named group's own name (`core/concurrency.py::
/// GLOBAL_RESOURCE_LABEL`).
pub const GLOBAL_RESOURCE_LABEL: &str = "global";

/// One event [`Limiter::acquire_reporting`]'s own callback can see --
/// fired only while a resource is genuinely blocking, in acquisition
/// order (the group, if any, then the global cap).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotEvent<'a> {
    /// *label* (a group's own name, or [`GLOBAL_RESOURCE_LABEL`]) has no
    /// free slot right now, so this call is about to block on it.
    Waiting(&'a str),
    /// The resource [`SlotEvent::Waiting`] most recently named has just
    /// been granted.
    Acquired,
}

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

    /// A limiter with a global cap of *max_concurrency* permits and one
    /// named semaphore per entry in *groups* (`runtime.
    /// concurrency_groups`). Both are taken as already-validated permit
    /// counts -- parsing `runtime.max_concurrency`/`runtime.
    /// concurrency_groups` out of a document's own JSON/YAML value, with
    /// Python's own `"Invalid runtime concurrency configuration:\n  -
    /// ..."` error text, is `electricity-config`'s job (issue #431's
    /// Lane D section), not this crate's.
    ///
    /// *max_concurrency* of `None` **or** `Some(0)` both mean no global
    /// cap -- Python's own `Semaphore(max_concurrency) if max_concurrency
    /// else None` (`concurrency.py:118-120`) is falsy for `0` exactly
    /// like it is for `None`, so a validated-but-zero ceiling must mean
    /// the same "unconfigured" thing here, not a semaphore with zero
    /// permits that can never be acquired. A *group*'s own limit has no
    /// such rule -- Python always builds `Semaphore(limit)` for a
    /// configured group unconditionally, and `parse_concurrency_groups`
    /// already rejects a non-positive one before this constructor ever
    /// sees it -- so `0` there is passed straight through to
    /// `Semaphore::new`, matching Python's own (non-)handling rather than
    /// inventing a "no cap" meaning Python's own group semaphores never
    /// have.
    ///
    /// Either count above [`Semaphore::MAX_PERMITS`] is clamped down to
    /// it -- `Semaphore::new` panics past that ceiling, which a
    /// sufficiently large (but validly parsed) `runtime.max_concurrency`/
    /// `concurrency_groups` value could otherwise reach.
    pub fn with_limits(
        max_concurrency: Option<usize>,
        groups: impl IntoIterator<Item = (String, usize)>,
    ) -> Self {
        let global = match max_concurrency {
            None | Some(0) => None,
            Some(n) => Some(Arc::new(Semaphore::new(clamp_to_max_permits(n)))),
        };
        Limiter {
            global,
            groups: groups
                .into_iter()
                .map(|(name, limit)| (name, Arc::new(Semaphore::new(clamp_to_max_permits(limit)))))
                .collect(),
        }
    }

    /// Acquires a slot for *group* (`None` for the global-only limit),
    /// blocking until one is free in both the named group's own
    /// semaphore (first) and the global one (second), reporting nothing.
    /// The same as [`Limiter::acquire_reporting`] with a `report` that
    /// does nothing -- a caller that wants `waiting_for` reported calls
    /// that method directly instead.
    ///
    /// `Err` only for an unknown *group* name (`core/concurrency.py::
    /// UnknownConcurrencyGroupError`'s own text, word for word).
    pub async fn acquire(&self, group: Option<&str>) -> Result<SlotGuard, LimiterError> {
        self.acquire_reporting(group, |_| {}).await
    }

    /// [`Limiter::acquire`], reporting every [`SlotEvent`] to *report* as
    /// it happens -- Python's own `on_wait`/`on_acquired` pair
    /// (`core/concurrency.py::RunConcurrencyLimiter.acquire`'s own
    /// `_acquire_one`), folded into one callback so a caller captures its
    /// own `meta.waiting_for` write once rather than writing two
    /// separate closures. *report* fires [`SlotEvent::Waiting`] for a
    /// resource (the group, if any, then the global cap, in that order)
    /// only the moment this call actually has to block for it -- never
    /// for a slot that was immediately free -- and [`SlotEvent::
    /// Acquired`] right after that same resource is granted; a caller
    /// writes `meta.waiting_for = Some(label)` on the first and `None` on
    /// the second, exactly mirroring `tool.py:711-720`/`prompt.
    /// py:658-667`'s own pair of writes around Python's `on_wait`/
    /// `on_acquired`.
    ///
    /// `Err` only for an unknown *group* name, raised before *report* is
    /// ever called -- the same as [`Limiter::acquire`].
    pub async fn acquire_reporting(
        &self,
        group: Option<&str>,
        mut report: impl FnMut(SlotEvent<'_>),
    ) -> Result<SlotGuard, LimiterError> {
        let group_sem = self.resolve_group(group)?;
        let group_permit = match (group, group_sem) {
            (Some(label), Some(sem)) => Some(acquire_one(&sem, label, &mut report).await),
            _ => None,
        };
        let global_permit = match &self.global {
            Some(sem) => Some(acquire_one(sem, GLOBAL_RESOURCE_LABEL, &mut report).await),
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
    /// caller to leak. This never tells a caller *which* resource missed
    /// (both are checked in the same non-blocking instant, so there is
    /// nothing to report `on_wait` for) -- a caller that needs that, to
    /// write `meta.waiting_for` per resource the way Python's `tool.
    /// py`/`prompt.py` do, wants [`Limiter::acquire_reporting`] instead.
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

/// Acquires *sem* for [`Limiter::acquire_reporting`]/[`Limiter::
/// acquire`]'s own *label* -- Python's own `_acquire_one`
/// (`core/concurrency.py:179-198`): a non-blocking try first, and only
/// if that misses, *report*'s own [`SlotEvent::Waiting`] before the real
/// (blocking) acquire, then [`SlotEvent::Acquired`] right after it
/// grants -- never reporting a resource that was free on the first try.
async fn acquire_one(
    sem: &Arc<Semaphore>,
    label: &str,
    report: &mut impl FnMut(SlotEvent<'_>),
) -> OwnedSemaphorePermit {
    match Arc::clone(sem).try_acquire_owned() {
        Ok(permit) => permit,
        Err(_) => {
            report(SlotEvent::Waiting(label));
            let permit = Arc::clone(sem)
                .acquire_owned()
                .await
                .expect("a Limiter's own semaphores are never closed");
            report(SlotEvent::Acquired);
            permit
        }
    }
}

/// Clamps *n* down to [`Semaphore::MAX_PERMITS`] -- `Semaphore::new`
/// panics past that ceiling, which a validly parsed but very large
/// `runtime.max_concurrency`/`concurrency_groups` count could reach.
fn clamp_to_max_permits(n: usize) -> usize {
    n.min(Semaphore::MAX_PERMITS)
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
        // A global cap that is *never* contended would pass this test
        // even if the code took the global slot before the group one --
        // capped at 3 (one legitimately held by the holder's own
        // acquire below, two spare for this test's own probes) so a
        // waiter that wrongly grabbed a global slot while still blocked
        // on the group would starve one of the probes.
        let limiter = Limiter::with_limits(Some(3), [("io".to_string(), 1)]);
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
        // The probe itself: with a global cap of 2, both slots are still
        // free as long as the waiter is only holding (or blocked on) the
        // group -- if it had instead acquired a global slot first, one
        // of these two `try_acquire`s would still succeed (2 - 1 held by
        // this probe's own first call), which wouldn't distinguish the
        // bug; acquiring *both* and finding them free pins the order.
        let probe_a = limiter.try_acquire(None).unwrap();
        assert!(probe_a.is_some(), "global slot 1 must still be free");
        let probe_b = limiter.try_acquire(None).unwrap();
        assert!(probe_b.is_some(), "global slot 2 must still be free");
        drop(probe_a);
        drop(probe_b);

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

    #[tokio::test]
    async fn with_limits_global_zero_means_no_cap() {
        let limiter = Limiter::with_limits(Some(0), []);
        let first = limiter.try_acquire(None).unwrap();
        assert!(first.is_some());
        // An actual zero-permit semaphore would make a second attempt
        // miss even with the first guard still held.
        let second = limiter.try_acquire(None).unwrap();
        assert!(second.is_some());
    }

    #[tokio::test]
    async fn with_limits_clamps_a_global_count_above_max_permits() {
        // `Semaphore::new` panics above `MAX_PERMITS`; this must not.
        let limiter = Limiter::with_limits(Some(usize::MAX), []);
        assert!(limiter.try_acquire(None).unwrap().is_some());
    }

    #[tokio::test]
    async fn with_limits_clamps_a_group_count_above_max_permits() {
        let limiter = Limiter::with_limits(None, [("io".to_string(), usize::MAX)]);
        assert!(limiter.try_acquire(Some("io")).unwrap().is_some());
    }

    #[tokio::test]
    async fn acquire_reporting_fires_nothing_when_both_are_free() {
        let limiter = Limiter::with_limits(Some(1), [("io".to_string(), 1)]);
        let mut events: Vec<SlotEvent<'static>> = Vec::new();
        let guard = limiter
            .acquire_reporting(Some("io"), |event| events.push(owned_event(event)))
            .await
            .unwrap();
        assert_eq!(events, Vec::new());
        drop(guard);
    }

    #[tokio::test]
    async fn acquire_reporting_errors_on_an_unknown_group_before_any_event() {
        let limiter = Limiter::with_limits(Some(1), []);
        let mut events: Vec<SlotEvent<'static>> = Vec::new();
        let err = limiter
            .acquire_reporting(Some("missing"), |event| events.push(owned_event(event)))
            .await
            .unwrap_err();
        assert!(err.0.contains("missing"));
        assert_eq!(events, Vec::new());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn acquire_reporting_on_a_group_miss_reports_only_the_group() {
        let limiter = Limiter::with_limits(None, [("io".to_string(), 1)]);
        let holder = limiter.acquire(Some("io")).await.unwrap();

        let mut events: Vec<SlotEvent<'static>> = Vec::new();
        {
            let waiting = async {
                limiter
                    .acquire_reporting(Some("io"), |event| events.push(owned_event(event)))
                    .await
                    .unwrap()
            };
            tokio::pin!(waiting);
            for _ in 0..4 {
                tokio::select! {
                    _ = &mut waiting => unreachable!("must not acquire while the group is held"),
                    _ = tokio::task::yield_now() => {}
                }
            }
            drop(holder);
            let _guard = waiting.await;
            // `waiting` (and the closure's mutable borrow of `events`)
            // is fully dropped at the end of this block.
        }
        assert_eq!(events, vec![SlotEvent::Waiting("io"), SlotEvent::Acquired]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn acquire_reporting_on_a_global_miss_reports_only_the_global_label() {
        let limiter = Limiter::with_limits(Some(1), []);
        let holder = limiter.acquire(None).await.unwrap();

        let mut events: Vec<SlotEvent<'static>> = Vec::new();
        {
            let waiting = async {
                limiter
                    .acquire_reporting(None, |event| events.push(owned_event(event)))
                    .await
                    .unwrap()
            };
            tokio::pin!(waiting);
            for _ in 0..4 {
                tokio::select! {
                    _ = &mut waiting => unreachable!("must not acquire while the global slot is held"),
                    _ = tokio::task::yield_now() => {}
                }
            }
            drop(holder);
            let _guard = waiting.await;
        }
        assert_eq!(
            events,
            vec![
                SlotEvent::Waiting(GLOBAL_RESOURCE_LABEL),
                SlotEvent::Acquired
            ]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn acquire_reporting_on_a_double_miss_reports_group_then_global_in_order() {
        let limiter = Limiter::with_limits(Some(1), [("io".to_string(), 1)]);
        // Held independently of each other (not through one `acquire`
        // call, which would always grab the group and the global slot
        // together) -- straight against each private semaphore, so the
        // waiter below is guaranteed to still find the global slot busy
        // even after the group one frees, rather than both freeing at
        // once the moment a single combined holder is dropped.
        let group_permit = limiter
            .groups
            .get("io")
            .unwrap()
            .clone()
            .try_acquire_owned()
            .unwrap();
        let global_permit = limiter.global.clone().unwrap().try_acquire_owned().unwrap();

        let mut events: Vec<SlotEvent<'static>> = Vec::new();
        {
            let waiting = async {
                limiter
                    .acquire_reporting(Some("io"), |event| events.push(owned_event(event)))
                    .await
                    .unwrap()
            };
            tokio::pin!(waiting);
            for _ in 0..4 {
                tokio::select! {
                    _ = &mut waiting => unreachable!("must not acquire while both are held"),
                    _ = tokio::task::yield_now() => {}
                }
            }
            drop(group_permit);
            for _ in 0..4 {
                tokio::select! {
                    _ = &mut waiting => unreachable!("must not acquire while the global slot is held"),
                    _ = tokio::task::yield_now() => {}
                }
            }
            drop(global_permit);
            let _guard = waiting.await;
        }
        assert_eq!(
            events,
            vec![
                SlotEvent::Waiting("io"),
                SlotEvent::Acquired,
                SlotEvent::Waiting(GLOBAL_RESOURCE_LABEL),
                SlotEvent::Acquired,
            ]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dropping_a_future_mid_wait_releases_a_group_permit_it_already_holds() {
        let limiter = Limiter::with_limits(Some(1), [("io".to_string(), 1)]);
        // Held alone (no group), so the waiter below is guaranteed to
        // have already fully acquired the group's own slot by the time
        // it blocks on this one.
        let global_guard = limiter.acquire(None).await.unwrap();

        {
            let waiting = async { limiter.acquire_reporting(Some("io"), |_| {}).await.unwrap() };
            tokio::pin!(waiting);
            for _ in 0..4 {
                tokio::select! {
                    _ = &mut waiting => unreachable!("must not acquire while the global slot is held"),
                    _ = tokio::task::yield_now() => {}
                }
            }
            // `waiting` (and the `OwnedSemaphorePermit` for the group it
            // already holds) is dropped at the end of this block, while
            // still suspended awaiting the global slot.
        }

        // Checked directly against the group's own semaphore, not
        // through `try_acquire` -- `global_guard` is still held at this
        // point (deliberately: it's what the dropped future was blocked
        // on), so a `try_acquire(Some("io"))` would itself still report
        // a miss on the *global* half, which would prove nothing about
        // whether the group permit specifically got released.
        assert_eq!(
            limiter.groups.get("io").unwrap().available_permits(),
            1,
            "the group's only permit must be free again"
        );
        drop(global_guard);
    }

    /// [`SlotEvent`] borrows its `Waiting` label from the call that fired
    /// it; this test module only ever reports [`GLOBAL_RESOURCE_LABEL`]
    /// or a `'static` group name literal, so re-expressing it as
    /// `'static` is always sound here and lets the recorded `Vec`
    /// outlive the `report` closure's own borrow of it.
    fn owned_event(event: SlotEvent<'_>) -> SlotEvent<'static> {
        match event {
            SlotEvent::Waiting("io") => SlotEvent::Waiting("io"),
            SlotEvent::Waiting(GLOBAL_RESOURCE_LABEL) => SlotEvent::Waiting(GLOBAL_RESOURCE_LABEL),
            SlotEvent::Waiting(other) => panic!("unexpected label {other:?} in this test module"),
            SlotEvent::Acquired => SlotEvent::Acquired,
        }
    }
}
