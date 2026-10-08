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

pub use compose::{load_yaml, load_yaml_last_key_wins};
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
/// This constant is a documented divergence from Circuitry's own
/// composer in **both** directions, neither of them a fixed boundary:
///
/// - **electricity is more permissive for deep block/mapping nesting.**
///   Circuitry's own composer (`core/yaml_load.py`, on top of PyYAML's
///   recursive-descent one) spends about two Python stack frames per
///   nesting level (`Composer.compose_node` calling
///   `compose_scalar_node`/`compose_sequence_node`/`compose_mapping_node`,
///   each of which recurses back into `compose_node`), so it raises
///   Python's own `RecursionError` -- not a `YAMLError`, so
///   `core/yaml_load.py`'s callers can't catch it the way they catch
///   every other load failure -- at roughly `(sys.getrecursionlimit() -
///   (the caller's own already-used frames) - a small constant) / 2`
///   levels: about 490 measured with no caller frames at Python's
///   default recursion limit of 1000, but *not* a constant -- measured
///   against the real loader, a document that loads fine with no extra
///   caller frames already raises `RecursionError` with as few as ~50
///   extra frames already on the stack (e.g. called from deeper inside
///   `cof run`), and every environment's own recursion limit shifts it
///   further still. electricity is therefore more permissive than
///   Circuitry for a document between Circuitry's own
///   (caller-dependent) boundary and this constant, which is fixed.
/// - **electricity is less permissive for deep *flow* nesting**
///   (`[...]`/`{...}`) **specifically.** `saphyr-parser` 0.1.0 counts
///   flow-collection nesting in its own `u8` (`Scanner`'s `flow_level`),
///   which overflows -- with its own, pre-existing "recursion limit
///   exceeded" `ScanError`, not `NestingTooDeep` -- at 256 levels,
///   *below* `MAX_DEPTH`; Circuitry's own loader accepts flow nesting
///   up to its own, caller-dependent boundary above (roughly 490, same
///   as block nesting -- PyYAML's composer doesn't distinguish flow
///   from block style). A flow-nested document between 256 and
///   Circuitry's own boundary therefore fails here but not there --
///   checked by `tests/nesting_limit.rs`'s
///   `one_hundred_thousand_nested_flow_sequences_does_not_crash` (which
///   only asserts no crash, since *which* of the two layers' errors
///   comes back for a given depth isn't itself part of the contract).
///
/// Both are documented, narrow divergences (this crate's "Known
/// divergences" section below). A YAML document deeply nested enough
/// for either direction to matter can only be model-generated (a
/// reflector/decomposition plan, or a rendered `use: inline` child) --
/// untrusted input that must fail with a distinct, catchable error here
/// rather than exhausting the stack or hitting an uncatchable
/// `RecursionError`.
///
/// Re-exported from [`electricity_value::MAX_DEPTH`] -- the limit every
/// crate in this workspace that reads, writes or evaluates nested data
/// shares (#394), not a number this crate happens to have picked to
/// match independently.
pub const MAX_DEPTH: usize = electricity_value::MAX_DEPTH;

/// The most `Node`s expanding every `*alias` in a document may clone, in
/// total, before loading fails with [`YamlError::AliasExpansionTooLarge`]
/// -- `compose.rs`'s `Composer::alias_node`, checked against the
/// anchored node's own, already-known `Node::size` *before* the clone
/// that would exceed it is ever allocated.
///
/// `electricity_value::Value` is an owned tree: unlike PyYAML, which
/// shares one Python object per anchor (`composer.py`'s
/// `Composer.compose_node` returns `self.anchors[anchor]` directly,
/// never a copy), every `*alias` here clones the whole subtree behind
/// it. A chain of aliases each referencing the previous one -- 10
/// aliases of 10 aliases, 9 levels deep -- clones about 10^9 nodes with
/// no limit at all, and even a single anchor of linear size (say,
/// 10,000 items) referenced 10,000 times clones about 10^8 nodes --
/// both cheap, small documents under PyYAML, both a memory-exhaustion
/// denial-of-service here. Circuitry parses model-generated YAML with
/// this loader (a reflector or decomposition plan, or a rendered `use:
/// inline` child), so an adversarial or malformed anchor chain is not a
/// hypothetical: it is exactly the kind of document untrusted input can
/// produce.
///
/// 1,000,000 is comfortably above any legitimate document's node count
/// (even a large plan is several orders of magnitude smaller) and
/// comfortably below what exhausts memory outright, so a legitimate
/// document never trips it while an exponential or combinatorial alias
/// chain fails fast, before allocating the blow-up, rather than slowly
/// running out of memory.
///
/// Circuitry's own loader has no such budget: `core/yaml_load.py`'s
/// `load_yaml` on top of `yaml.safe_load` shares one object per anchor,
/// so it loads a document like this cheaply -- and then, if that shared
/// structure is later serialized (`json.dumps` for a plan's `--out`) or
/// rendered through a template, Python re-walks and re-expands every
/// alias as if it were a real copy, which is exactly where a document
/// that loaded cheaply can still blow up later. This is a documented,
/// narrow divergence (this crate's "Known divergences" section below):
/// electricity-yaml fails fast, at load time, with a catchable error;
/// Circuitry's own loader defers the same failure to whatever uses the
/// value afterward.
pub const MAX_NODES: usize = 1_000_000;

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
// - **Unicode line-break characters inside a scalar** (finding 3, PR
//   #388's third review). PyYAML's `Reader.forward` (`reader.py`)
//   counts U+2028 (LINE SEPARATOR), U+2029 (PARAGRAPH SEPARATOR) and
//   U+0085 (NEL) as line breaks, exactly like `\n`, even inside a
//   scalar; `saphyr-parser` 0.1.0 never does. Pinned in the golden
//   corpus where both sides still fail (just one line apart --
//   `known_divergence_cases`' own U+2028/U+2029/NEL *comment* cases);
//   can't be pinned at all for a line-break character *inside a plain
//   scalar* specifically (`a: x<U+2028>y`), where it instead changes
//   *which side fails* -- a scanner error for Circuitry (a plain
//   scalar can't contain a line break without folding) but an
//   ordinary string containing that character here (electricity-yaml
//   has no divergence-case shape for "Circuitry fails, electricity-yaml
//   loads a value" -- D4's own gap, below) -- so that one shape is
//   pinned directly against the behaviour instead, in
//   `tests/reader_divergences.rs`.
//
//   A lone `\r` (one *not* immediately followed by `\n`) is *not* one
//   of these: both PyYAML and `saphyr-parser` 0.1.0 already count it as
//   its own line break (`char_traits.rs`'s `is_break`,
//   `Scanner::skip_linebreak`/`skip_nl`), so an anchor reused across one
//   (`a: &x 1\rb: &x 2\r`) is Circuitry's own duplicate-anchor error in
//   both loaders -- an ordinary parity case (`generate_yaml_corpus.py`'s
//   `anchor_cases`), not a divergence. A fourth-review bug in this
//   crate's own byte-offset bookkeeping (`compose.rs`'s `line_starts`
//   and `advance_marker`, which previously only recognized `\n`) used
//   to make it look like one by silently dropping every anchor/tag
//   prefix and duplicate-key position on the far side of a lone `\r`;
//   fixed by counting a lone `\r` as a line break there too.
// - **Unicode decimal digits under an explicit `!!int`/`!!float` tag**
//   (finding 3, PR #388's third review): Python's `int()`/`float()`
//   accept any Unicode decimal digit (Arabic-Indic, fullwidth, ...),
//   not just ASCII `0`-`9`; `num_bigint::BigInt::from_str_radix` and
//   Rust's `f64::from_str` don't. Pinned in the golden corpus (an
//   `!!int "\u0661\u0662"\n"`/`!!float "\uff11.\uff15"` case each),
//   since here Circuitry succeeds and electricity-yaml fails cleanly
//   with `InvalidScalar` -- the direction this crate's existing
//   `known_divergence` kind *can* express.
// - **NEL (U+0085) inside a quoted or block scalar** (PR #388's fourth
//   review). PyYAML's `Reader.scan_line_break` (`scanner.py`) folds
//   every line break it recognizes -- `\r`, `\r\n`, and also NEL,
//   U+2028 and U+2029 -- to a plain `\n` as it scans, so a NEL inside a
//   double-quoted scalar's text becomes a space (quoted-scalar folding:
//   `a: "x\x85y"\n` loads as `{'a': 'x y'}`), and the same folding
//   applies inside a literal/folded block scalar's content. This
//   crate's own resolver never performs that substitution at all --
//   `\x85`, like any other character, is only ever consumed as scanner
//   input, never rewritten -- so the same document here loads as
//   `{'a': 'x\x85y'}`, keeping the raw NEL in the string. Not yet
//   pinned in the golden corpus or fixed; left as a follow-up.
// - **U+FEFF (BOM) mid-stream.** `Self::strip_leading_bom` (`compose.rs`)
//   only ever strips a BOM at index 0, matching PyYAML's own
//   `scan_to_next_token`; PyYAML additionally gives a BOM *anywhere*
//   else in the stream no column at all (`reader.py`'s `forward`: `elif
//   ch != '\uFEFF': self.column += 1`), while saphyr-parser 0.1.0 counts
//   it like any other character. `{x: "\ufeff", x: 1}\n`'s duplicate-key
//   error is at column 9 under Circuitry, column 10 here. Not yet
//   pinned in the golden corpus or fixed; left as a follow-up.
// - **A `=` key retagged through one merge, but not through a
//   sibling's direct (non-merge) reference to the same shared anchor**
//   (finding 8, PR #388's third review): `SafeConstructor
//   .flatten_mapping` only retags a bare `=` key to `str` when
//   reached *through* `<<:`; PyYAML's `MappingNode`s are shared and
//   mutable, so once one mapping's merge flattens an anchored source
//   and retags its `=` key in place, any other mapping that reaches
//   that same node -- including directly, never through a merge of
//   its own -- sees the already-retagged key too. electricity-yaml
//   clones a shared anchor per alias instead (`MAX_NODES`'s doc
//   comment above), so the merge's retag never reaches the original,
//   separately-held copy a direct reference sees -- which still has
//   no constructor for a bare `=`, so loading that copy fails where
//   Circuitry's loader, having already mutated the one shared node,
//   succeeds. Pinned in the golden corpus.
//
// Every case above that *can* be pinned mechanically -- both sides
// fail differently, or Circuitry succeeds and electricity-yaml fails
// in a specific, checkable way -- is pinned in the golden corpus
// (`tests/golden/corpus.json`, generated by
// `scripts/generate_yaml_corpus.py`'s `known_divergence_cases`), each
// one labelled as a divergence rather than compared for equality
// against Circuitry's own loader. A few above are not: the anchor-name
// character-set case, both depth-limit directions (`MAX_DEPTH`'s own
// doc comment), and the "Circuitry fails, electricity-yaml loads a
// value" shape just above (a line-break character inside a plain
// scalar) all have Circuitry on the *failing* side, which this crate's
// `known_divergence` corpus kind has no way to express (D4, PR #388's
// third review) -- documented here in prose instead, and pinned
// directly against the behaviour in `tests/reader_divergences.rs`.
