//! Lane C: ports `core/cycle_check.py` — `path:`/`orchestration:`
//! children read the way it reads them (absolute path, then the
//! working directory, then the parent document's directory, no
//! duplicate-key check, an unreadable child treated as empty), with no
//! library lookup, reporting `Cycle: a → b → a`.
