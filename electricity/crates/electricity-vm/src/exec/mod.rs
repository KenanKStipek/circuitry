//! Lane B/C: the per-container execution modules -- `dynamic` (chain/
//! tree), `conditional` (CEL `if`), and this module's own [`tool`]
//! (lane A's real `run_tool` dispatch seam).

pub mod tool;
