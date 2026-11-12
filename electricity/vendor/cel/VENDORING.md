# Vendoring notes

This is a vendored copy of [`cel` 0.15.0](https://crates.io/crates/cel)
(upstream: https://github.com/cel-rust/cel-rust), MIT licensed (`LICENSE`).
electricity's workspace `Cargo.toml` resolves ordinary `cel = "0.15"`
dependencies to this copy via `[patch.crates-io]`; nothing outside this
directory needs to know it isn't the published crate.

## What changed and why

`cel::objects::Map` (the public `Value::Map` variant's own storage) and the
evaluator's own internal map representation,
`common::types::map::DefaultMap`, were both backed by a plain
`std::collections::HashMap`. A CEL map's own keys therefore had no
authored order on electricity, only on `cof run` (Python's `dict` is
insertion-ordered) — issue #723.

Both are now backed by `indexmap::IndexMap` instead, so a map literal
keeps its source order, a map read from `state` keeps `state`'s own
order (nested inside a list/map, or chosen by a ternary, included), all
the way through a round trip via `Context::add_variable_from_value` and
back out through `Program::execute`. `IndexMap`'s own `PartialEq` stays
order-insensitive (two maps compare equal regardless of order, the same
contract `HashMap` had), so CEL's `==`/`in` are unchanged.

Touched: `src/objects.rs`, `src/common/types/map.rs`, `Cargo.toml` (added
an `indexmap` dependency). Nothing else in this copy differs from the
published crate.

## Keeping this in sync

If `cel` publishes a new version electricity wants, re-vendor by copying
the new version's source over this directory (preserving this file,
`LICENSE`, and the `indexmap` dependency) and re-applying the same two
source changes against the new version's `objects.rs`/
`common/types/map.rs`.
