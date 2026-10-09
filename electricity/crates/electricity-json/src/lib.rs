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
//!
//! Known differences from CPython 3.11:
//!
//! - An integer literal (or a big-integer `Value`, for the writer) longer
//!   than 4300 decimal digits parses or writes its full digits here,
//!   where CPython's `int(...)`/`json.dumps(...)` raises `ValueError`
//!   (`sys.set_int_max_str_digits`) — inherited from `electricity-value`.
//! - Nesting depth is capped at a fixed [`MAX_DEPTH`], returning
//!   [`ReadError::Depth`]/[`WriteError::Depth`] rather than recursing
//!   further. CPython's own limit instead comes from
//!   `sys.getrecursionlimit()` (1000 call frames by default) *minus*
//!   however many frames of Python call stack already exist when
//!   `json.loads`/`json.dumps` is entered — not a fixed number, and not
//!   reproducible here. `json.loads` raises `RecursionError` at that
//!   point, a *different* exception from `json.JSONDecodeError`; several
//!   Circuitry call sites (`core/prompt.py`'s model-reply parsing,
//!   `core/tool.py`, `plugins/json.py`) catch only `JSONDecodeError` and
//!   fall back or report a parse error, letting `RecursionError` pass
//!   through uncaught instead. [`ReadError::Depth`]/[`WriteError::Depth`]
//!   are likewise distinct from [`ReadError::Syntax`], for the same
//!   reason: a caller that maps `Syntax` to "not JSON, try a fallback"
//!   must not also catch a depth-limit error that way.
//! - [`load_json`]/[`loads`] decode a lone (unpaired) UTF-16 surrogate
//!   escape (`"\ud800"`, or a high surrogate whose following `\uXXXX`
//!   isn't a matching low surrogate) to U+FFFD, since a Rust `String`
//!   can't hold one (it isn't a Unicode scalar value). CPython's `str`
//!   has no such restriction, and keeps the lone surrogate — with
//!   consequences on every write path Circuitry uses that this crate
//!   doesn't reproduce:
//!   - `ensure_ascii=True` (both `--out` modes, via `cli/app.py` and the
//!     state stores): CPython writes the escape back out verbatim
//!     (`"\ud800"`); this crate writes `"\ufffd"` instead. Neither side
//!     errors; the bytes differ silently.
//!   - `ensure_ascii=False` followed by a UTF-8 encode (`core/tool.py`'s
//!     `.encode("utf-8")`): CPython raises `UnicodeEncodeError` (a
//!     `ValueError`, caught and handled by that call site); encoding this
//!     crate's `"\ufffd"` output always succeeds.
//!   - `ensure_ascii=False` followed by a file or subprocess write
//!     (`plugins/json.py`): the same — fails in Python, succeeds here.
//!   - Two dict keys that differ only by a lone surrogate (or a lone
//!     surrogate next to a real U+FFFD) are two distinct Python `str`
//!     keys, but collapse to one here: [`loads`] silently keeps only one;
//!     [`load_json`] raises [`ReadError::DuplicateKey`], which CPython
//!     never raises for this input.
//! - `sort_keys=True` sorts a dict whose keys include a `NaN` float and has
//!   64 or more keys with a stable sort that puts every `NaN` key after
//!   every non-`NaN` key (ties among multiple `NaN` keys keep their
//!   insertion order), rather than CPython's own algorithm (`count_run`
//!   then `binarysort`, for a list under 64 elements; `binarysort`
//!   within a full timsort merge otherwise) — reproducing the exact key
//!   order CPython's sort produces for a `NaN` key is only done here for
//!   dicts with fewer than 64 keys (`sorted_items`, in `writer.rs`).

mod reader;
mod writer;

pub use reader::{ReadError, load_json, loads};
pub use writer::{WriteError, WriteMode, dumps, dumps_default_str, stringify_key};

/// Maximum nesting depth (containment depth of nested `{}`/`[]`) that
/// [`loads`]/[`load_json`] will parse, or [`dumps`]/[`dumps_default_str`]
/// will write, before returning [`ReadError::Depth`]/[`WriteError::Depth`]
/// instead of recursing further — a fixed number, unlike CPython's own
/// stack-depth-dependent limit (see the module docs above).
///
/// Re-exported from [`electricity_value::MAX_DEPTH`] — the same limit
/// `electricity-yaml` enforces, and the one every crate in this workspace
/// that reads, writes or evaluates nested data shares (#394), so this is
/// not a second, independently-chosen number that happens to currently
/// match.
pub const MAX_DEPTH: usize = electricity_value::MAX_DEPTH;
