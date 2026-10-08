//! A PyYAML-compatible YAML 1.1 loader for electricity: Circuitry's own
//! composer and resolver on top of `saphyr-parser` (DESIGN.md §3.2,
//! runtime-semantics §1.1).
//!
//! No Rust YAML library implements YAML 1.1's implicit-scalar resolution
//! (every maintained one targets 1.2), so this crate owns that layer
//! itself: `saphyr-parser` for tokenizing/parsing (events with scalar
//! style, anchors, tags, and spans), and a from-scratch resolver and
//! constructor on top that applies PyYAML's own rules to every plain
//! scalar, plus explicit tags, anchors/aliases, `<<:` merge keys, and
//! Circuitry's own duplicate-key check (`core/yaml_load.py`).

mod compose;
mod error;
mod scalar;

pub use compose::load_yaml;
pub use error::{Mark, YamlError};
