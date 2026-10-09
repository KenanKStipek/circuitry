//! oscilloscope-core: the engine- and terminal-independent half of `osp`
//! (see `../../DESIGN.md`, especially §6.1's module map). It holds no
//! terminal code, so a GUI front end can reuse it unchanged once one
//! exists.
//!
//! Milestone O-1 (issue #424) fills in the core itself: the plan join,
//! the live-state/events observers, status inference, log-line
//! diffing, engine commands and process supervision. The terminal UI
//! (`ratatui`) lands in O-2, in the `oscilloscope` binary crate only.

pub mod diff;
pub mod engine;
pub mod model;
pub mod observe;
pub mod plan;
pub mod render;
pub mod supervise;
