//! electricity-compiler: the load-and-check pipeline and compiler for
//! electricity orchestrations (issue #408's Scope section).
//!
//! This crate ports, in order: loading a document ([`load`]), its
//! structural checks ([`structural`], [`difflib`], [`schema_instance`]),
//! compilation to [`electricity_bytecode::Program`] ([`compile`],
//! [`state_ns`], [`groups`], [`cycles`]), the compile-time half of #406
//! ([`compose`], [`prompt_files`], [`digest`]), and the two entry points
//! that reproduce Circuitry's two surfaces ([`pipeline`]).
//!
//! # Lane A (this lane): the stable public API
//!
//! Every function and type below exists with its final signature in
//! this PR; every body is a stub that returns a clear "not implemented
//! in lane A" error (never a `todo!()`/panic) until the lane named in
//! its module's header fills it in. Lanes B, C and D fill in disjoint
//! files — see each module's own header for which lane owns it and
//! which Circuitry source it ports:
//!
//! | Module | Lane | Circuitry source |
//! |---|---|---|
//! | [`load`] | B | `cli/orchestration_loader.py` |
//! | [`structural`] | B | `core/document_check.py` (orchestrates [`difflib`]/[`schema_instance`]) |
//! | [`difflib`] | B | `difflib.get_close_matches` (Python stdlib) |
//! | [`schema_instance`] | B | the `Value` -> schema-instance conversion |
//! | [`pipeline`] | B | `cli/runtime_shim.py` (`validate`/`run`, and `core/concurrency.py`'s config-parsing errors) |
//! | [`compile`] (`containers`/`leaves`) | C | `core/compiler.py` |
//! | [`state_ns`] | C | `core/state_ns.py` |
//! | [`groups`] | C | `core/compiler.py`'s `unknown_concurrency_group_errors` (an effect's `group:` reference; the `max_concurrency`/`concurrency_groups` config-parsing errors are [`pipeline`]'s, lane B) |
//! | [`cycles`] | C | `core/cycle_check.py` |
//! | [`compose`] | D | `core/prompt_compose.py` (composition checks) |
//! | [`prompt_files`] | D | `core/prompt_files.py` |
//! | [`digest`] | D | `core/prompt_compose.py::document_content_digest` |
//!
//! # Known divergences
//!
//! - `.toon` documents are refused outright (an electricity-specific
//!   message: convert to YAML or JSON) — Circuitry's own TOON support
//!   has no Rust-side port.
//! - The `jsonschema` crate's `oneOf` failures don't append Circuitry's
//!   own "best match" suffix.
//! - **Library-cache confinement**: electricity runs documents by path
//!   and always uses the nearest-config-file rule, never a library
//!   source's cache-root override.
//! - `ref:` is rejected at compile time: there is no library-name/
//!   remote-library resolution (DESIGN.md §4).
//! - [`prompt_files`]'s confinement-root resolve: `core/prompt_files.py::
//!   resolve_prompt_file_path` lets the confinement root's own `Path.
//!   resolve(strict=False)` raise an *uncaught* `OSError` (only the
//!   candidate path's own resolve is wrapped in a `PromptFileError`) --
//!   a latent bug, reachable only by a confinement root itself behind a
//!   broken symlink chain. This port wraps both resolves the same way,
//!   as a [`CompileError`] rather than an uncaught panic (Rust has no
//!   equivalent of letting an arbitrary exception type propagate out of
//!   a `Result`-returning function); not pinned by a corpus case, since
//!   the exact OS error text this produces is errno-message dependent
//!   even in CPython itself.

pub mod compile;
pub mod compose;
pub mod cycles;
pub mod difflib;
pub mod digest;
pub mod groups;
pub mod load;
pub mod pipeline;
pub mod prompt_files;
pub mod reflector_prime;
pub mod schema_instance;
pub mod state_ns;
pub mod structural;

pub use compile::compile_document;
pub use load::load_document;
pub use pipeline::{check_for_run, check_report};
pub use structural::{structural_errors, unknown_key_warnings};

use std::fmt;
use std::path::PathBuf;

/// Where a document came from (`core/compiler.py`'s
/// `document_dir`/`confinement_root` parameters to `compile_orchestration`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentOrigin {
    /// A document with a real file of its own: its directory, and the
    /// confinement root a `{file: ...}` prompt source must stay inside.
    File {
        document_dir: PathBuf,
        confinement_root: PathBuf,
    },
    /// Generated at run time (a reflector/decompose plan, a `use:
    /// inline` child) or with no path at all — `file:` is a compile
    /// error against this origin.
    Generated,
}

/// A compile-time error from [`compile_document`] — Circuitry's own
/// message, word for word (issue #408's Goal).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileError(pub String);

impl fmt::Display for CompileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for CompileError {}

/// The exact error text a `cof run` of a document reports — the
/// [`check_for_run`] half of issue #408's two entry points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunCheckError {
    /// Structural or concurrency-group errors: formatted as
    /// `Orchestration validation failed:\n  - ...`.
    Structural(Vec<String>),
    /// A `compile_document` error's raw message, unprefixed.
    Compile(String),
    /// Prompt composition errors (compile-time half of #406): formatted
    /// as `Prompt composition errors:\n  - ...`.
    Composition(Vec<String>),
    /// A `use` cycle: `Cycle: a → b → a`, already formatted.
    Cycle(String),
    /// Lane A stub: the pipeline step that would have produced one of
    /// the variants above isn't implemented yet.
    NotImplemented(String),
}

impl fmt::Display for RunCheckError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunCheckError::Structural(errors) => {
                write!(f, "Orchestration validation failed:")?;
                for error in errors {
                    write!(f, "\n  - {error}")?;
                }
                Ok(())
            }
            RunCheckError::Compile(message) => write!(f, "{message}"),
            RunCheckError::Composition(errors) => {
                write!(f, "Prompt composition errors:")?;
                for error in errors {
                    write!(f, "\n  - {error}")?;
                }
                Ok(())
            }
            RunCheckError::Cycle(message) => write!(f, "{message}"),
            RunCheckError::NotImplemented(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for RunCheckError {}

/// Options shared by [`check_report`] and [`check_for_run`] — the
/// `skip_preflight`/`trust_document` flags `cli/runtime_shim.py`'s
/// `validate`/`run` both take.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckOptions {
    pub skip_preflight: bool,
    pub trust_document: bool,
}

/// Matches `runtime_shim.validate(...)`'s return shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckReport {
    pub ok: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

/// `program` reused by every stub so the exact wording stays
/// consistent, and so finding it (ripgrep this crate for the string
/// below) always lands on the right lane/issue.
pub(crate) fn not_implemented(function: &str, lane: &str) -> String {
    format!(
        "electricity-compiler: `{function}` is not implemented in lane A \
         (see issue #408, lane {lane})"
    )
}
