//! `CancellationToken`: the first-signal-wins token tree DESIGN.md
//! §6.5/§6.9 describes -- a real, self-contained implementation (not a
//! lane B/C/D stub: no `Program`/`Store` dependency, so there is nothing
//! here lane A can't finish correctly itself).
//!
//! A root token has no parent; [`CancellationToken::child`] makes a new
//! token whose [`CancellationToken::is_set`]/[`CancellationToken::signum`]
//! also see the parent's own cancellation (so a run-wide SIGINT cancels
//! every tree branch's token too), while its own
//! [`CancellationToken::request`] only ever sets *its own* flag, never
//! the parent's or a sibling's -- the asymmetry a tree needs: cancelling
//! a branch must not cancel the run, but cancelling the run must cancel
//! every branch.
//!
//! `Send + Sync` (orchestrator ruling on PR #432's review): lane D's
//! signal handler cancels a run from a dedicated signal-handling task or
//! thread, not from inside the single VM task that's blocked on a tool
//! call or a `finally:` -- the two must be able to share a token across
//! that boundary. Backed by `Arc`/`AtomicI32` rather than `Rc`/`Cell`
//! (which an earlier version of this file used, single-threaded only)
//! so a `Clone` stays cheap while the token itself is safely shared.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use tokio::sync::Notify;

/// `0` means "unset" -- every real signal number (`SIGINT` 2, `SIGTERM`
/// 15, `SIGHUP` 1) is positive, so it can never collide with the sentinel.
const UNSET: i32 = 0;

#[derive(Debug)]
struct Inner {
    signum: AtomicI32,
    notify: Notify,
}

/// A cancellation flag, shareable across threads and tasks, with an
/// optional parent.
#[derive(Clone, Debug)]
pub struct CancellationToken {
    inner: Arc<Inner>,
    parent: Option<Arc<CancellationToken>>,
}

const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<CancellationToken>();
};

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancellationToken {
    /// A fresh, unset root token.
    pub fn new() -> Self {
        CancellationToken {
            inner: Arc::new(Inner {
                signum: AtomicI32::new(UNSET),
                notify: Notify::new(),
            }),
            parent: None,
        }
    }

    /// Requests cancellation with *signum*. Returns `true` exactly when
    /// this call was the one that set *this token's own* flag (DESIGN's
    /// "first signal wins" rule, issue #431's `request(signum) -> first?`)
    /// -- a second call, with the same or a different signum, returns
    /// `false` and leaves the original signum in place. Setting an
    /// ancestor's flag separately (a run-wide SIGINT after a branch was
    /// already cancelled on its own) is a distinct, independent "first"
    /// for that ancestor's own token. Safe to call from any thread,
    /// including one that doesn't hold a Tokio runtime at all (a signal
    /// handler, for example).
    pub fn request(&self, signum: i32) -> bool {
        let won = self
            .inner
            .signum
            .compare_exchange(UNSET, signum, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok();
        if won {
            self.inner.notify.notify_waiters();
        }
        won
    }

    /// `true` once this token or any ancestor has been [`request`]ed.
    pub fn is_set(&self) -> bool {
        self.inner.signum.load(Ordering::SeqCst) != UNSET
            || self.parent.as_ref().is_some_and(|parent| parent.is_set())
    }

    /// The first signal this token (or, failing that, the nearest
    /// cancelled ancestor) was [`request`]ed with.
    pub fn signum(&self) -> Option<i32> {
        let own = self.inner.signum.load(Ordering::SeqCst);
        if own != UNSET {
            return Some(own);
        }
        self.parent.as_ref().and_then(|parent| parent.signum())
    }

    /// A new, independently-cancellable child token whose own
    /// [`is_set`]/[`signum`] also see *self*'s cancellation.
    pub fn child(&self) -> CancellationToken {
        CancellationToken {
            inner: Arc::new(Inner {
                signum: AtomicI32::new(UNSET),
                notify: Notify::new(),
            }),
            parent: Some(Arc::new(self.clone())),
        }
    }

    /// Resolves once this token or any ancestor is [`request`]ed --
    /// never misses a `request()` that already happened before this
    /// call started (checked, then listened for, then re-checked: the
    /// standard `tokio::sync::Notify` idiom for a level-triggered flag,
    /// not an edge-triggered event a notification before `.await` could
    /// lose), including one from a different thread than the one
    /// awaiting this future. A VM loop selects on this future beside
    /// whatever it's actually waiting on (a tool call, a tree branch) to
    /// react to cancellation promptly instead of polling [`is_set`] in a
    /// loop.
    pub fn cancelled(&self) -> impl Future<Output = ()> + Send + '_ {
        self.cancelled_boxed()
    }

    fn cancelled_boxed(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(async move {
            match &self.parent {
                Some(parent) => {
                    tokio::select! {
                        () = self.wait_own() => {}
                        () = parent.cancelled_boxed() => {}
                    }
                }
                None => self.wait_own().await,
            }
        })
    }

    /// Resolves once *this token's own* flag (not an ancestor's) is set.
    async fn wait_own(&self) {
        loop {
            if self.inner.signum.load(Ordering::SeqCst) != UNSET {
                return;
            }
            // Registered *before* the re-check below: a `request()`
            // landing between the check above and this line still wakes
            // `notified`, since `Notify::notify_waiters` only has to
            // reach futures already created (not yet polled is fine) by
            // the time it runs.
            let notified = self.inner.notify.notified();
            if self.inner.signum.load(Ordering::SeqCst) != UNSET {
                return;
            }
            notified.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_fresh_token_is_not_set() {
        let token = CancellationToken::new();
        assert!(!token.is_set());
        assert_eq!(token.signum(), None);
    }

    #[test]
    fn the_first_request_wins() {
        let token = CancellationToken::new();
        assert!(token.request(2));
        assert!(!token.request(15));
        assert_eq!(token.signum(), Some(2));
        assert!(token.is_set());
    }

    #[test]
    fn a_childs_own_cancellation_does_not_reach_the_parent() {
        let parent = CancellationToken::new();
        let child = parent.child();
        assert!(child.request(2));
        assert!(child.is_set());
        assert!(!parent.is_set());
    }

    #[test]
    fn a_parents_cancellation_is_visible_to_every_child() {
        let parent = CancellationToken::new();
        let child = parent.child();
        let grandchild = child.child();
        assert!(parent.request(15));
        assert!(child.is_set());
        assert!(grandchild.is_set());
        assert_eq!(grandchild.signum(), Some(15));
    }

    #[test]
    fn a_token_cancelled_from_another_thread_is_seen_on_this_one() {
        let token = CancellationToken::new();
        let moved = token.clone();
        let handle = std::thread::spawn(move || {
            moved.request(2);
        });
        handle.join().unwrap();
        assert!(token.is_set());
        assert_eq!(token.signum(), Some(2));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancelled_does_not_miss_a_request_already_made_before_the_await_started() {
        let token = CancellationToken::new();
        token.request(2);
        // If this were edge-triggered (a plain `Notify::notified()` with
        // no "already set?" check), a request with no one yet awaiting
        // `cancelled()` would be lost and this would hang; the timeout
        // turns that hang into a failing test instead of a wedged suite.
        tokio::time::timeout(Duration::from_millis(200), token.cancelled())
            .await
            .expect("cancelled() must resolve immediately for an already-cancelled token");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cancelled_wakes_up_on_a_request_from_a_real_os_thread() {
        let token = CancellationToken::new();
        let waiter = token.clone();
        let awaited = tokio::spawn(async move { waiter.cancelled().await });

        let requester = token.clone();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            requester.request(2);
        });

        tokio::time::timeout(Duration::from_secs(2), awaited)
            .await
            .expect("cancelled() must wake once another thread calls request()")
            .unwrap();
        thread.join().unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_childs_cancelled_wakes_when_only_the_parent_is_requested() {
        let parent = CancellationToken::new();
        let child = parent.child();
        let waiter = child.clone();
        let awaited = tokio::spawn(async move { waiter.cancelled().await });

        let requester = parent.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            requester.request(15);
        });

        tokio::time::timeout(Duration::from_secs(2), awaited)
            .await
            .expect("a child's cancelled() must wake when its parent is requested")
            .unwrap();
    }
}
