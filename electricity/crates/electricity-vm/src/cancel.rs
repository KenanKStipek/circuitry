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

use std::cell::Cell;
use std::rc::Rc;

/// A single-threaded (`Rc`/`Cell`, matching the VM's own single-threaded
/// `tokio::LocalSet` -- DESIGN.md §6.1-6.2) cancellation flag, with an
/// optional parent.
#[derive(Clone)]
pub struct CancellationToken {
    signum: Rc<Cell<Option<i32>>>,
    parent: Option<Box<CancellationToken>>,
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancellationToken {
    /// A fresh, unset root token.
    pub fn new() -> Self {
        CancellationToken {
            signum: Rc::new(Cell::new(None)),
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
    /// for that ancestor's own token.
    pub fn request(&self, signum: i32) -> bool {
        if self.signum.get().is_none() {
            self.signum.set(Some(signum));
            true
        } else {
            false
        }
    }

    /// `true` once this token or any ancestor has been [`request`]ed.
    pub fn is_set(&self) -> bool {
        self.signum.get().is_some() || self.parent.as_ref().is_some_and(|parent| parent.is_set())
    }

    /// The first signal this token (or, failing that, the nearest
    /// cancelled ancestor) was [`request`]ed with.
    pub fn signum(&self) -> Option<i32> {
        self.signum
            .get()
            .or_else(|| self.parent.as_ref().and_then(|parent| parent.signum()))
    }

    /// A new, independently-cancellable child token whose own
    /// [`is_set`]/[`signum`] also see *self*'s cancellation.
    pub fn child(&self) -> CancellationToken {
        CancellationToken {
            signum: Rc::new(Cell::new(None)),
            parent: Some(Box::new(self.clone())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
