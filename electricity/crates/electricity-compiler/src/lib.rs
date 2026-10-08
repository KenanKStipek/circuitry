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
//! # Lanes (issue #408)
//!
//! Lanes A (this crate's stable public API), B, C and D each filled in
//! disjoint files and have all landed — see each module's own header
//! for which lane owns it and which Circuitry source it ports:
//!
//! | Module | Lane | Circuitry source |
//! |---|---|---|
//! | [`load`] | B | `cli/orchestration_loader.py` |
//! | [`structural`] | B | `core/document_check.py` (orchestrates [`difflib`]/[`schema_instance`]) |
//! | [`difflib`] | B | `difflib.get_close_matches` (Python stdlib) |
//! | [`schema_instance`] | B | the `Value` -> schema-instance conversion |
//! | [`pipeline`] | B | `cli/runtime_shim.py` (`validate`/`run`, and `core/concurrency.py`'s config-parsing errors) |
//! | [`project_root`] | B | `core/prompt_files.py::default_project_root` |
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
//! - [`pipeline::check_report`]'s `warnings` does not reproduce
//!   Circuitry's own advisory lint (`core/lint.py::lint_orchestration`
//!   -- deprecated aliases, loop-body reference footguns, `threshold:`
//!   on a built-in `if`, `min_iterations` on an `each` loop, and more):
//!   only `unknown_key_warnings` and the host-settings notice are
//!   ported. Lint parity was never assigned to any of issue #408's
//!   lanes; tracked as issue #428.
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
//! - [`schema_instance`]'s conversion of a `Date`/`DateTime`/`Bytes`/
//!   `NaN`/`Infinity` value fails every `"type"` keyword exactly the way
//!   Python's own `isinstance` verdict would (including `"object"` --
//!   `electricity-schema`'s own overridden `"type"` keyword, not this
//!   crate's marker representation alone -- see that module's own doc
//!   comment), except one narrow, live case: `minimum`/`maximum` against
//!   an `Infinity`/`-Infinity` value only vacuously pass rather than
//!   enforcing a finite bound (e.g. `threshold: .inf` passes here, a
//!   `maximum` error in Python) -- a deliberate, documented trade-off
//!   against a panic risk in the `jsonschema` crate itself (same doc
//!   comment), not an inert gap.
//! - [`electricity_schema::json_path_from_pointer`]'s non-string-key
//!   location: decoded back exactly for an `int`/`bool` key (`1:` ->
//!   `[1]`, `yes:` -> `[True]`). For any other hashable non-string key
//!   (a float, `None`, a date/datetime, bytes), Python's own `json_path`
//!   raises `TypeError` instead, collapsing the whole structural check
//!   into one error rather than this location's own -- not reproduced
//!   here (`schema_instance`'s own doc comment has the full rationale);
//!   the location instead falls back to a non-empty, best-effort quoted
//!   rendering of the key's own `repr()` text.
//! - [`pipeline::check_for_run`] omits two of `cli/runtime_shim.py::run`'s
//!   own pre-structural checks, both ported nowhere in electricity today:
//!   `resolve_complexity_settings`'s validation of a malformed
//!   `runtime.complexity` block (`cli/complexity_config.py`, raising
//!   `ComplexityConfigError` from inside `resolve_effective_settings`,
//!   before the concurrency limiter this function does check), and
//!   `build_persistence_backend`'s validation of a malformed
//!   `runtime.persistence` block (`core/store/persistence.py`, raised
//!   after the concurrency limiter but before `check_interface_inputs`).
//!   A document with an otherwise-valid structure but a malformed
//!   `runtime.complexity`/`runtime.persistence` block passes
//!   `check_for_run` here where `cof run` would fail -- complexity
//!   routing/decomposition and persistence backends are whole subsystems
//!   with no IR representation in this crate at all (out of scope for
//!   issue #408's lane B), so this is left a documented gap rather than
//!   a partial port of either module.
//! - [`structural::schema_errors`] sorts multiple simultaneous schema
//!   violations by `(location, message)`, not Circuitry's own `str(err)`
//!   order (third-party `jsonschema` text from a different
//!   implementation) -- see that function's own doc comment.
//! - [`compile::containers`]'s [`compile::containers::compile_effects_in_scope`]
//!   rejects more than `MAX_COMPILE_DEPTH` (128) nested effect
//!   containers with its own distinct error. Circuitry has no fixed
//!   limit of its own: its YAML loader and its schema-instance check
//!   each raise Python's `RecursionError` at a caller-dependent depth,
//!   well before `compile_orchestration`'s own stack frames would
//!   (`compile::containers`'s own module docs have the measured
//!   numbers).
//! - [`cycles`]'s own non-UTF-8-content gap: a `use` child that isn't
//!   valid UTF-8 is treated as unreadable (an empty document, never a
//!   cycle edge through it), where Circuitry's own `load_orch` raises
//!   an uncaught `UnicodeDecodeError` ([`cycles`]'s own module docs).
//! - [`compile::coerce::py_int`]/[`compile::coerce::py_float`]: a value
//!   of the wrong Python type (one schema validation -- lane B -- would
//!   already have rejected) coerces to `0`/`0.0` rather than raising,
//!   so this compiler never panics on it ([`compile::coerce`]'s own
//!   module docs).
//! - A handful of narrower whitespace/Unicode divergences, declined as
//!   out of scope for this lane (PR #415's review, finding 19): the
//!   `\x1c`-`\x1f` control characters Python's `str.strip()`/`.isspace()`
//!   treat as whitespace but Rust's `char::is_whitespace` does not
//!   ([`compose`]'s own module docs); [`compile::names`]'s own
//!   `iter_<n>`-reserved-pattern check is ASCII-digit-only, where
//!   Circuitry's `re.fullmatch(r"iter_\d+", name)` matches any Unicode
//!   decimal digit; [`state_ns`]'s own `iter_template_strings` silently
//!   drops a non-string leaf under a checked field rather than
//!   reporting it, matching `core/state_ns.py::_iter_template_strings`'s
//!   own `isinstance(str)` guard exactly, but still a narrower overall
//!   behavior than Circuitry's, since nothing later reports that leaf
//!   either; a declared `use.inputs: {}` and an absent `inputs` compile
//!   identically here, where Circuitry's schema instance (lane B)
//!   distinguishes the two; and an unknown chat-message `role` value
//!   (unreachable today -- the schema's `role` is a closed enum).
//! - [`prompt_files`]'s confinement-root resolve: `core/prompt_files.py::
//!   resolve_prompt_file_path` lets the confinement root's own `Path.
//!   resolve(strict=False)` raise an *uncaught* `RuntimeError` (only the
//!   candidate path's own resolve is wrapped in a `PromptFileError`) --
//!   a latent bug, reachable only by a confinement root itself behind a
//!   symlink *loop* (on CPython 3.11, `Path.resolve(strict=False)` only
//!   raises there; a merely dangling/broken chain with no cycle resolves
//!   fine, leaving the missing target as a literal trailing path
//!   segment). This port wraps both resolves the same way, as a
//!   [`CompileError`] rather than an uncaught panic (Rust has no
//!   equivalent of letting an arbitrary exception type propagate out of
//!   a `Result`-returning function); not pinned by a corpus case, since
//!   the exact OS error text this produces is errno-message dependent
//!   even in CPython itself.
//! - [`prompt_files::resolve_non_strict`]'s own symlink-loop handling:
//!   unlike CPython's `Path.resolve(strict=False)` (which detects a
//!   cycle structurally and raises `RuntimeError` immediately), this
//!   port caps any symlink chain -- looping or not -- at 40 hops and
//!   then fails with an `other`-kind [`std::io::Error`] ("too many
//!   levels of symbolic links"), an approximation of the OS's own
//!   `ELOOP`, not a byte-for-byte port of CPython's own loop detection
//!   or error text.
//! - A prompt file that exists but cannot be read (permission denied,
//!   say) reports the OS error text verbatim after "could not be
//!   read: " -- `std::io::Error`'s own `Display`, which never matches
//!   Python's `OSError.__str__` word for word (e.g. Rust's `Permission
//!   denied (os error 13)` vs. Python's `[Errno 13] Permission denied:
//!   '<path>'`). Only the Circuitry-owned prefix in front of it is
//!   pinned exactly by a corpus case; the OS-specific suffix is
//!   compared by location only (DESIGN.md §1, §12).

pub mod compile;
pub mod compose;
pub mod cycles;
pub mod difflib;
pub mod digest;
pub mod groups;
pub mod load;
pub mod pipeline;
pub mod project_root;
pub mod prompt_files;
pub mod reflector_prime;
pub mod schema_instance;
pub mod state_ns;
pub mod structural;

pub use compile::compile_document;
pub use digest::document_content_digest;
pub use load::load_document;
pub use pipeline::{check_for_run, check_report};
pub use structural::{structural_errors, unknown_key_warnings};

use electricity_value::Value;
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
/// `validate`/`run` both take, plus what the pipeline needs from the
/// *config file* itself (issue #408's lane B section): its own
/// `runtime:` block, merged under the document's own (document wins
/// key by key — electricity trusts every document, DESIGN.md §11).
/// `None` — the default, and what every golden corpus case runs
/// with, since Circuitry's own ground truth always calls
/// `validate`/`run` with `config=None` — behaves exactly like an
/// empty config: the merged runtime is just the document's own
/// `runtime:` block, unchanged. Populated by the `electricity` CLI
/// alone, from the `config.json` positional argument.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckOptions {
    pub skip_preflight: bool,
    pub trust_document: bool,
    pub config_runtime: Option<Value>,
    /// The CLI's `-e key=value` pairs, as given, text — in command-line
    /// order, with a later occurrence of a key replacing an earlier
    /// one's *value* but not its position (`IndexMap::insert`'s own
    /// behavior on a repeated key, matching Python's `dict.__setitem__`
    /// exactly: `cli/app.py::_parse_env_vars`'s `result[key] = parsed`
    /// keeps a repeated key at its first position, last value wins).
    /// Empty by default, so every existing caller and golden corpus
    /// case behaves as it did before this field existed —
    /// [`pipeline::check_interface_inputs_error`] reduces to its old,
    /// always-empty-input-namespace behavior when this is empty.
    pub inputs: indexmap::IndexMap<String, String>,
}

/// Matches `runtime_shim.validate(...)`'s return shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckReport {
    pub ok: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}
