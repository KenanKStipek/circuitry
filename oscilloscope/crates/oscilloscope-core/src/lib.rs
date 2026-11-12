//! oscilloscope-core: the engine- and terminal-independent half of `osi`
//! (see `../../DESIGN.md`, especially §6.1's module map). It holds no
//! terminal code, so a GUI front end can reuse it unchanged once one
//! exists.
//!
//! Milestone O-1 (issue #424) fills in the core itself: the plan join,
//! the live-state/events observers, status inference, log-line
//! diffing, engine commands and process supervision. The terminal UI
//! (`ratatui`) lands in O-2, in the `oscilloscope` binary crate only.

pub mod changes;
pub mod combined;
pub mod config_discovery;
pub mod diff;
pub mod discovery;
pub mod engine;
pub mod flow;
pub mod fold;
pub mod format;
pub mod history;
pub mod icon;
pub mod layout;
pub mod model;
pub mod multi;
pub mod names;
pub mod observe;
pub mod plan;
pub mod rail;
pub mod registry;
pub mod render;
pub mod screens;
pub mod select;
pub mod store;
pub mod summary;
pub mod supervise;
pub mod transcript;
