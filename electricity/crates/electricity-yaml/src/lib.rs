//! A PyYAML-compatible YAML 1.1 loader for electricity: Circuitry's own
//! composer and resolver on top of `saphyr-parser` (DESIGN.md §3.2,
//! runtime-semantics §1.1).
//!
//! No Rust YAML library implements YAML 1.1's implicit-scalar resolution
//! (every maintained one targets 1.2), so this crate owns that layer
//! itself: `saphyr-parser` for tokenizing/parsing (events with scalar
//! style, anchors, tags, and spans), and a from-scratch resolver and
//! constructor on top that applies PyYAML's own rules to every plain
//! scalar, plus explicit tags, anchors/aliases, `<<:` merge keys, and
//! Circuitry's own duplicate-key check (`core/yaml_load.py`).

mod compose;
mod error;
mod scalar;

pub use compose::load_yaml;
pub use error::{Mark, YamlError};

/// The deepest a document's containers (sequences/mappings) may nest,
/// counting the root as depth 1 -- checked both as the text is composed
/// (so a document nested arbitrarily deep in the text itself never
/// recurses the composer more than `MAX_DEPTH` frames deep) and again
/// once an alias is expanded (so a *chain* of aliases, each one cloning
/// an already near-the-limit subtree into a new, shallow-looking
/// container, can't compound past it either): `compose.rs`'s
/// `Composer::compose_from_event` and `node_depth_of`/`node_depth_of_pairs`.
///
/// Circuitry's own composer (`core/yaml_load.py`, on top of PyYAML's
/// recursive-descent one) raises Python's `RecursionError` at roughly
/// 490 levels -- below this constant -- rather than erroring with its
/// own distinct message; electricity is therefore *more* permissive
/// than Circuitry for a document between roughly 490 and 512 levels
/// deep, a documented, narrow divergence (this crate's "Known
/// divergences" section below). A YAML document this deeply nested can
/// only be model-generated (a reflector/decomposition plan, or a
/// rendered `use: inline` child) -- untrusted input that must fail with
/// a distinct, catchable error here rather than exhausting the stack.
pub const MAX_DEPTH: u32 = 512;

// ---------------------------------------------------------------------
// Known divergences from Circuitry's own loader (`core/yaml_load.py`)
// ---------------------------------------------------------------------
//
// - **Merge-source mutation (circuitry#390).** Circuitry's own
//   `_UniqueKeyLoader.construct_mapping` runs its duplicate-key pre-pass
//   over a `MappingNode`'s `.value` list in place, but PyYAML's
//   `SafeConstructor.flatten_mapping` *also* mutates that same list (in
//   place) when expanding a `<<:` merge key -- so a merge source shared
//   by two mappings (an anchor merged into one mapping that is itself
//   anchored and merged into a third) can have *already* been expanded
//   once by the time Circuitry's own pre-pass walks the second
//   mapping's keys, producing a `DuplicateKeyError` that plain
//   `yaml.safe_load` does not raise on the same document. This is a bug
//   in Circuitry's own loader, not in the YAML it's loading -- filed as
//   circuitry#390 -- and electricity-yaml must not reproduce it: it
//   matches `yaml.safe_load`'s result instead, pinned by the golden
//   corpus's `known_divergence_cases` (`generate_yaml_corpus.py`) with
//   the expected value taken from `yaml.safe_load`, not from
//   Circuitry's own `load_yaml`.
// - **Unrepresentable tags** (`!!set`, `!!omap`, `!!pairs`, any unknown
//   tag): `Value` has no set or tuple-list variant to hold what these
//   construct in Python (`DESIGN.md` §3.1), so electricity-yaml refuses
//   them with an `UnresolvableTag` error instead of lossily
//   approximating them as a plain list/dict.
// - **A genuinely self-referential alias** (`a: &a [*a]`): PyYAML
//   registers an anchor before composing its own children, so it builds
//   a real recursive structure; electricity-yaml's `Node` tree has no
//   way to represent a cycle, so this is a load error instead
//   (`compose.rs`'s `Event::Alias` arm).
// - **A bare alias directly against `:`** (`*k: 1`, no space): accepted
//   by PyYAML's YAML-1.1 scanner, rejected by saphyr-parser 0.1.0's
//   YAML-1.2 one -- a scanner-level difference this crate's composer
//   doesn't control.
// - **An anchor name outside `[0-9A-Za-z_-]`** (e.g. `&x.y`): PyYAML's
//   scanner restricts anchor names to that set; saphyr-parser 0.1.0 is
//   more permissive.
// - **A `.nan` key's identity.** PyYAML's `construct_yaml_float` returns
//   one shared `nan_value` object only for the exact (sign-optional)
//   spelling `.nan`/`.NaN`/`.NAN` -- the path both implicit resolution
//   and an explicit `!!float .nan` take; an explicit `!!float nan`
//   (*without* the leading dot) instead falls to that function's plain
//   `float(value)` branch, which builds a *fresh* NaN object every call.
//   Two keys that are both the shared singleton collide in a real
//   Python `dict` (identity precedes `__eq__`); two freshly built ones
//   don't (`NaN != NaN`, and they're not the same object either). This
//   crate's `Value` has no such identity to track -- every NaN float is
//   just `f64::NAN` -- so its duplicate-key check (`compose.rs`'s
//   `SeenKeys`) treats every NaN key as one, which is both this crate's
//   *and* the resolver's final `Dict::insert`'s approximation of "the
//   same key": right for the `.nan`-spelled, shared-singleton case,
//   wrong (reporting a duplicate, or collapsing to one entry, where
//   Circuitry's loader keeps two) for the `nan`-without-a-dot,
//   explicit-tag spelling specifically.
// - **A block-scalar mapping key's position.** saphyr-parser 0.1.0's
//   scanner starts a block (literal/folded) scalar's span at its first
//   *content* line, not at the `|`/`>` indicator itself (`scanner.rs`);
//   PyYAML's own mark is the indicator's position. A duplicate-key
//   error naming such a key is therefore at the wrong line here -- a
//   scanner-level position this crate's composer has no way to recover
//   (unlike an anchor/tag prefix, `node_prefix`'s gap-scanning has
//   nothing to look for: no punctuation marks a block scalar's start
//   other than the indicator consumed into the *previous* event's own
//   span).
//
// Every case above is pinned in the golden corpus
// (`tests/golden/corpus.json`, generated by
// `scripts/generate_yaml_corpus.py`'s `known_divergence_cases`), each
// one labelled as a divergence rather than compared for equality
// against Circuitry's own loader.
