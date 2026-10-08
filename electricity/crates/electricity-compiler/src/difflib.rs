//! Lane B: an exact port of Python's `difflib.get_close_matches` (stdlib,
//! `cutoff=0.8`) plus the mistaken-for table `structural.rs`'s near-miss
//! unknown-key check uses -- covering `finally:` children and the label
//! for non-string keys.
//!
//! Only the single piece `structural.rs` actually calls --
//! `get_close_matches(word, sorted(known), n=1, cutoff=0.8)[0]` -- is
//! ported: a word, a list of possibilities already sorted ascending, and
//! the single best match at or above a cutoff, or none. `difflib.
//! SequenceMatcher`'s `quick_ratio`/`real_quick_ratio` short-circuits are
//! not reproduced: both are proven upper bounds on `ratio()` (the
//! stdlib's own module docs), so `ratio() >= cutoff` alone decides
//! exactly the same membership `get_close_matches` would with all three
//! checks -- the other two only skip the expensive exact computation for
//! a possibility that was always going to fail anyway, never change
//! which possibilities pass.
//!
//! `SequenceMatcher(None, ...)` -- the one constructor
//! `get_close_matches` ever builds -- has no junk function, and
//! `autojunk`'s popular-element culling only triggers for a sequence of
//! 200 or more elements (`SequenceMatcher.__chain_b`): every "word" this
//! crate ever matches is a document key name, always far shorter, so
//! `bjunk` is always empty and every `isbjunk` check in the reference
//! algorithm is always `False` -- omitted below accordingly.

use std::collections::HashMap;

/// `SequenceMatcher(None, a, b).find_longest_match(alo, ahi, blo, bhi)`:
/// the longest run common to `a[alo..ahi]` and `b[blo..bhi]`, as
/// `(besti, bestj, bestsize)` -- `a[besti..besti+bestsize] ==
/// b[bestj..bestj+bestsize]`. `b2j` maps each character of `b` to the
/// sorted list of indices (within the *whole* `b`, not just
/// `blo..bhi`) where it occurs -- built once per `b` by the caller, not
/// per call, exactly mirroring `SequenceMatcher`'s own `self.b2j`.
fn find_longest_match(
    a: &[char],
    alo: usize,
    ahi: usize,
    b: &[char],
    blo: usize,
    bhi: usize,
    b2j: &HashMap<char, Vec<usize>>,
) -> (usize, usize, usize) {
    let mut besti = alo;
    let mut bestj = blo;
    let mut bestsize = 0usize;
    let mut j2len: HashMap<usize, usize> = HashMap::new();
    for (i, &ch) in a.iter().enumerate().take(ahi).skip(alo) {
        let mut newj2len: HashMap<usize, usize> = HashMap::new();
        if let Some(js) = b2j.get(&ch) {
            for &j in js {
                if j < blo {
                    continue;
                }
                if j >= bhi {
                    break;
                }
                let prev = if j == 0 {
                    0
                } else {
                    *j2len.get(&(j - 1)).unwrap_or(&0)
                };
                let k = prev + 1;
                newj2len.insert(j, k);
                if k > bestsize {
                    besti = i + 1 - k;
                    bestj = j + 1 - k;
                    bestsize = k;
                }
            }
        }
        j2len = newj2len;
    }
    // Extend the best match by identical elements on each side -- no junk
    // to prefer extending through first, unlike the general algorithm.
    while besti > alo && bestj > blo && a[besti - 1] == b[bestj - 1] {
        besti -= 1;
        bestj -= 1;
        bestsize += 1;
    }
    while besti + bestsize < ahi
        && bestj + bestsize < bhi
        && a[besti + bestsize] == b[bestj + bestsize]
    {
        bestsize += 1;
    }
    (besti, bestj, bestsize)
}

/// `SequenceMatcher(None, a, b).ratio()`'s numerator: `sum(triple[-1] for
/// triple in self.get_matching_blocks())`. The matching blocks'
/// post-processing (sorting, collapsing adjacent blocks, the trailing
/// dummy triple) only matters to callers that inspect the blocks
/// themselves; summed, it changes nothing, so this returns the sum
/// directly from the same recursive divide-and-conquer
/// `get_matching_blocks` uses, without materializing or collapsing the
/// block list.
fn matching_block_total(a: &[char], b: &[char], b2j: &HashMap<char, Vec<usize>>) -> usize {
    let mut queue = vec![(0usize, a.len(), 0usize, b.len())];
    let mut total = 0usize;
    while let Some((alo, ahi, blo, bhi)) = queue.pop() {
        let (i, j, k) = find_longest_match(a, alo, ahi, b, blo, bhi, b2j);
        if k > 0 {
            total += k;
            if alo < i && blo < j {
                queue.push((alo, i, blo, j));
            }
            if i + k < ahi && j + k < bhi {
                queue.push((i + k, ahi, j + k, bhi));
            }
        }
    }
    total
}

fn build_b2j(b: &[char]) -> HashMap<char, Vec<usize>> {
    let mut b2j: HashMap<char, Vec<usize>> = HashMap::new();
    for (j, &ch) in b.iter().enumerate() {
        b2j.entry(ch).or_default().push(j);
    }
    b2j
}

/// `SequenceMatcher(None, a, b).ratio()`: `2.0 * M / T`, where `T =
/// len(a) + len(b)` and `M` is the total length of every matching
/// block.
fn ratio(a: &[char], b: &[char]) -> f64 {
    let total = a.len() + b.len();
    if total == 0 {
        return 1.0;
    }
    let b2j = build_b2j(b);
    let matches = matching_block_total(a, b, &b2j);
    2.0 * matches as f64 / total as f64
}

/// `difflib.get_close_matches(word, possibilities, n=1, cutoff=cutoff)[0]`
/// (or `None` for an empty result) -- the only call shape
/// `structural.rs` ever makes. *possibilities* must already be sorted
/// ascending (as `core/document_check.py` always passes `sorted(known)`)
/// since a tie is broken by `heapq.nlargest(1, ...)`'s own tuple-
/// ordering rule: among equally-scored candidates, the
/// lexicographically *greatest* possibility wins, regardless of which
/// one `possibilities` lists first.
pub fn get_close_match<'a>(
    word: &str,
    possibilities: &'a [String],
    cutoff: f64,
) -> Option<&'a str> {
    let b: Vec<char> = word.chars().collect();
    let mut best: Option<(f64, &'a str)> = None;
    for possibility in possibilities {
        let a: Vec<char> = possibility.chars().collect();
        let score = ratio(&a, &b);
        if score < cutoff {
            continue;
        }
        best = Some(match best {
            Some((best_score, best_name))
                if score < best_score
                    || (score == best_score && possibility.as_str() < best_name) =>
            {
                (best_score, best_name)
            }
            _ => (score, possibility.as_str()),
        });
    }
    best.map(|(_, name)| name)
}

#[cfg(test)]
mod tests {
    use super::get_close_match;

    /// Recorded from CPython directly:
    /// `difflib.get_close_matches("whlie", ["while", "yield", "loop"], n=1, cutoff=0.8)`
    /// -> `['while']` (also checked end to end against the real corpus
    /// in `golden_load.rs`'s `difflib_matches_cpython_recorded_values`).
    #[test]
    fn near_miss_within_cutoff_matches_cpython() {
        let known = vec!["while".to_string(), "yield".to_string(), "loop".to_string()];
        assert_eq!(get_close_match("whlie", &known, 0.8), Some("while"));
    }

    #[test]
    fn below_cutoff_is_no_match() {
        let known = vec!["template".to_string(), "params".to_string()];
        assert_eq!(get_close_match("owner", &known, 0.8), None);
    }

    #[test]
    fn exact_match_has_ratio_one() {
        let known = vec!["params".to_string()];
        assert_eq!(get_close_match("params", &known, 0.8), Some("params"));
    }

    #[test]
    fn empty_word_against_empty_possibilities_is_no_match() {
        let known: Vec<String> = vec![];
        assert_eq!(get_close_match("x", &known, 0.8), None);
    }

    /// Tie-break rule: `heapq.nlargest(1, [(ratio, x), ...])` compares the
    /// full `(ratio, x)` tuple, so an equal ratio picks the
    /// lexicographically greater `x` -- recorded directly:
    /// `difflib.get_close_matches("ab", ["ac", "ad"], n=1, cutoff=0.0)` ->
    /// `['ad']` (both score 0.5; `"ad" > "ac"`).
    #[test]
    fn tie_breaks_toward_the_lexicographically_greater_possibility() {
        let known = vec!["ac".to_string(), "ad".to_string()];
        assert_eq!(get_close_match("ab", &known, 0.0), Some("ad"));
    }
}
