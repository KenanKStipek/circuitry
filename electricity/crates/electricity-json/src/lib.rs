//! `electricity-json`: a `Value` ⇄ text codec matching Python's `json`
//! module exactly (DESIGN.md §3.4, §3.4.1), built on `electricity-value`'s
//! `Value` type and its `py_str`/`py_repr`/float-repr machinery (never a
//! second implementation of CPython float formatting).
//!
//! The writer ([`dumps`]/[`dumps_default_str`]) and the reader ([`loads`])
//! are independent: the writer never appends the trailing `"\n"` that
//! `cli/app.py`'s `_write_state_json` adds — that belongs to the caller,
//! not this crate (runtime-semantics.md §8.3).

mod reader;
mod writer;

pub use reader::{ReadError, loads};
pub use writer::{WriteError, WriteMode, dumps, dumps_default_str};
