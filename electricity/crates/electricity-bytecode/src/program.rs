//! Lane A: `Program`, `DocumentInfo` (issue #408's Scope section).

use crate::op::Op;
use electricity_value::Value;
use indexmap::IndexMap;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::PathBuf;

/// Where a document came from — a real file, or generated at run time
/// (a reflector/decompose plan, a `use: inline` child), which changes
/// whether a `{file: ...}` prompt source is even legal (DESIGN.md §4
/// step 1; `core/compiler.py`'s `compile_orchestration`
/// `document_dir`/`confinement_root`).
#[derive(Debug, Clone, Serialize)]
pub struct DocumentInfo {
    /// The path exactly as given on the command line / by the caller.
    pub path_as_given: String,
    /// The document's resolved directory, used to find a `{file: ...}`
    /// prompt source and the nearest `circuitry.config.json`.
    pub resolved_directory: PathBuf,
    /// The confinement root a `{file: ...}` prompt path must stay inside
    /// (DESIGN.md §4 step 1): the nearest `circuitry.config.json`/
    /// `config.json` at or above `resolved_directory`, else
    /// `resolved_directory` itself.
    pub confinement_root: PathBuf,
    /// `document_content_digest` (`core/prompt_compose.py`) — the
    /// document bytes plus every referenced prompt file's relative-path
    /// label, 8-byte length, and bytes. `None` exactly when Circuitry's
    /// own `document_hash` is `None`: an `OSError` computing it (a race
    /// where the file vanished between the document check and this
    /// point) -- `cli/runtime_shim.py::run`, ~:678-683, catches `OSError`
    /// around this call and sets `document_hash = None` rather than
    /// failing the run over a hash that only matters for a future
    /// `--resume`. [`crate::digest::document_content_digest`]'s own
    /// `Err` is this field's one `None` source here too.
    pub digest: Option<String>,
}

/// A fully compiled orchestration: the root op plus everything the VM
/// (M0-H) needs that isn't reachable by walking `root` alone.
#[derive(Debug, Clone, Serialize)]
pub struct Program {
    /// The document root, always a `dynamic` named `"prime"` with no
    /// scope-overlay semantics (runtime-semantics §2.4).
    pub root: Op,
    /// The top-level `prompts:` map's declared prompts, raw text, with
    /// prompt files already read (`DynamicDefinition.prompts`, root
    /// only — a non-root `dynamic`'s own `prompts` field is always
    /// empty, folded into this field instead of carried per-node).
    pub prompts: IndexMap<String, String>,
    /// `all_effect_names` (`core/prompt_compose.py`) — every effect name
    /// in the document, used by the compile-time half of #406's
    /// collision checks (`DynamicDefinition.effect_names`, root only,
    /// same folding as `prompts` above).
    pub effect_names: BTreeSet<String>,
    /// `None` from `electricity_compiler::compile_document` itself: its
    /// `DocumentOrigin::File` only carries `document_dir`/
    /// `confinement_root`, and a `Generated` origin has no directory or
    /// confinement root at all, so `compile_document` (which sees
    /// neither the path as given nor the document's raw bytes the
    /// digest needs) cannot build one. `check_report`/`check_for_run`
    /// (lane B's `pipeline` module, which read both before calling
    /// `compile_document`) fill this in for a real file; it stays
    /// `None` for a document with no file of its own.
    pub document: Option<DocumentInfo>,
    /// The document's own `runtime:` block, merged over the config
    /// file's `runtime:` block key by key at run time (#408's CLI
    /// section) — carried here unmerged, as the document wrote it.
    pub runtime_block: Option<Value>,
    /// The document's own `interface:` block (inputs/outputs schema).
    pub interface: Option<Value>,
    pub adapter: Option<String>,
    pub model: Option<String>,
}
