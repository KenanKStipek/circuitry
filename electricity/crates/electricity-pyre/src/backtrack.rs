//! `fancy-regex`'s own default backtrack limit (1,000,000) counts
//! backtrack attempts across *every* start position of one search,
//! not just the eventual match -- so an otherwise-linear pattern a
//! Python-`re`-to-`fancy-regex` translation turns into a lookaround
//! (non-MULTILINE `$`, `\b`/`\B`, any lookbehind) can exhaust the
//! crate's own default on an ordinary multi-hundred-KB input even
//! though the real per-character cost is tiny (PR #480 review R8:
//! `\bzebra\b` findall with no match, 20,000 lines of `item N value
//! M`, `\s+$` on 1.2 MB -- all fail against `fancy-regex`'s default
//! even though `cof` succeeds instantly). Shared by
//! `electricity-tools::regex` (moved out from under it, PR #502's
//! round-5 fix) and `electricity-schema::engine` (`pattern`/
//! `patternProperties` -- issue #498's own follow-up,
//! finding V4), the two places either crate needs a backtrack-limit
//! sized to the text actually being matched rather than the crate's
//! own flat default.
//!
//! Measured directly in a release build (`cargo build --release`;
//! debug assertions change the constant meaningfully) rather than
//! guessed:
//! - **`PER_CHAR` = 100.** Every lookaround form this translation
//!   emits (`\bzebra\b`, `\d+$`, `\s+$`, a fixed-width lookbehind) was
//!   run as a no-match search over inputs from 10 KB to 2 MB; the
//!   worst observed cost was ~7 backtracks per input byte (`\bzebra\b`
//!   against plain words). 100 is comfortably past 10x that.
//!   `(PER_CHAR * (input_len + 1))` is this limit's own scaling term.
//! - **`FLOOR` = 10,000,000.** CPython's own baseline for a
//!   catastrophic pattern on a *small* input --
//!   `(a+)+(?=b)` against 18-20 `a`s, which the review's own R8 names
//!   -- needs 1,048,576-4,194,304 backtracks to fail in `fancy-regex`;
//!   10,000,000 clears that with more than 2x headroom (measured: 18
//!   `a`s at the exact limit this constant would compute, ~22ms;
//!   CPython itself returns `[]` in low-single-digit milliseconds, so
//!   this is still "like CPython", not just "eventually"). At the same
//!   time, a genuinely catastrophic pattern on a small input --
//!   `(a+)+` against 30 `a`s with no match -- still fails in ~223ms at
//!   this exact FLOOR, nowhere near the ~2s budget R8 sets.
//!
//! *input_len* is the input text's own byte length, never smaller
//! than its char count, so this never under-provisions for multi-byte
//! text.
const REGEX_BACKTRACK_PER_CHAR: usize = 100;
const REGEX_BACKTRACK_FLOOR: usize = 10_000_000;

pub fn backtrack_limit_for(input_len: usize) -> usize {
    REGEX_BACKTRACK_FLOOR.max(REGEX_BACKTRACK_PER_CHAR.saturating_mul(input_len.saturating_add(1)))
}
