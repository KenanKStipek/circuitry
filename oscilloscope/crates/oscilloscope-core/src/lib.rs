//! oscilloscope-core: the engine- and terminal-independent half of `osp`
//! (see `../../DESIGN.md`, especially §6.1's module map). It holds no
//! terminal code, so a GUI front end can reuse it unchanged once one
//! exists.
//!
//! Milestone O-0 (issue #420) only lays out the module skeleton the
//! design names; each module's real contents land in O-1/O-2.

pub mod diff;
pub mod engine;
pub mod model;
pub mod observe;
pub mod plan;
pub mod supervise;
