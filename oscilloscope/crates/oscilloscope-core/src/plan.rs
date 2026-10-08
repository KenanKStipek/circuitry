//! The plan side: compiling a document into the tree `osp` displays
//! (DESIGN.md §5, `PlanTree`). O-1 fills in the tree itself — the path
//! pattern matching against state and event paths, `use` grafting, and
//! the fallback for a document that fails to compile. O-0 only proves
//! the path dependency on `electricity-compiler`/`electricity-bytecode`
//! builds, with the one call osp will always make first.

use std::path::Path;

use electricity_bytecode::Program;
use electricity_compiler::{CheckOptions, RunCheckError, check_for_run};

/// Compiles `path` with the same options `cof`'s `runtime_shim.run`
/// passes (DESIGN.md §5): calling code has already decided to run the
/// document, so preflight and trust checks are skipped here, not
/// reimplemented.
///
/// `CheckOptions` is built from `Default` plus field assignment, not a
/// struct literal: its field list is still growing as electricity's
/// compiler lanes land, and a literal would need an update — silently
/// defaulting the rest is correct here — every time one does.
#[allow(clippy::field_reassign_with_default)]
pub fn compile(path: &Path) -> Result<Program, RunCheckError> {
    let mut options = CheckOptions::default();
    options.skip_preflight = true;
    options.trust_document = true;
    check_for_run(path, &options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_a_missing_file_is_an_error() {
        let err = compile(Path::new("/nonexistent-osp-path/does-not-exist.yml"));
        assert!(err.is_err());
    }
}
