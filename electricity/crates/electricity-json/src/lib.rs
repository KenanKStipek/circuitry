//! `electricity-json`: a `Value` ⇄ text codec matching Python's `json`
//! module exactly (DESIGN.md §3.4, §3.4.1), built on `electricity-value`'s
//! `Value` type and its `py_str`/`py_repr`/float-repr machinery (never a
//! second implementation of CPython float formatting).
//!
//! The writer ([`dumps`]/[`dumps_default_str`]) and the reader
//! ([`loads`]/[`load_json`]) are independent: the writer never appends the
//! trailing `"\n"` that `cli/app.py`'s `_write_state_json` adds — that
//! belongs to the caller, not this crate (runtime-semantics.md §8.3).
//! [`loads`] is plain `json.loads` (a repeated object key silently keeps
//! only the last value); [`load_json`] is `core/json_load.load_json`,
//! which every `.json` orchestration document goes through instead, and
//! raises on a repeated key naming its dotted path (runtime-semantics.md
//! §1.2).

mod reader;
mod writer;

pub use reader::{ReadError, load_json, loads};
pub use writer::{WriteError, WriteMode, dumps, dumps_default_str};
