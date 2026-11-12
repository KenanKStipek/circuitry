//! A short, memorable name for a run (issue #717): `amber-falcon`, derived purely from the run id both
//! engines write (`run_start`'s own `run_id`, carried from there into [`crate::render::Header::run_id`],
//! [`crate::discovery::DiscoveredRun::run_id`] and [`crate::history::HistoryRecord::run_id`]) -- so Select,
//! All, the Combined Signal, a run's own header, `--log` and the exit summary all show the same name for the
//! same run, and Recent Runs keeps it once the run has ended, with no extra field needed since history
//! already carries the run id. [`run_name`] hashes the id with FNV-1a 64 and picks one adjective and one
//! noun from the two word lists in `names/adjectives.txt`/`names/nouns.txt` -- about 200 plain, inoffensive
//! English words each, at most seven letters, `include_str!`'d so a name never depends on an installed data
//! file. A run has no name until its `run_id` is known, so every caller here takes `Option<&str>`, never an
//! empty string standing in for "none".

const ADJECTIVES_TEXT: &str = include_str!("names/adjectives.txt");
const NOUNS_TEXT: &str = include_str!("names/nouns.txt");

fn word_list(text: &str) -> Vec<&str> {
    text.lines().filter(|line| !line.is_empty()).collect()
}

/// FNV-1a 64: the simplest hash that gives [`run_name`] good spread across both word lists from a short id,
/// with no dependency pulled in just for this.
fn fnv1a64(bytes: &[u8]) -> u64 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = OFFSET_BASIS;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

/// `amber-falcon`: `run_id`'s own FNV-1a 64 hash picks one adjective (the low 32 bits) and one noun (the
/// high 32 bits) from the two fixed word lists -- pure and stable, so the same run id always gets the same
/// name, in every `osi`, on every screen.
pub fn run_name(run_id: &str) -> String {
    let hash = fnv1a64(run_id.as_bytes());
    let adjectives = word_list(ADJECTIVES_TEXT);
    let nouns = word_list(NOUNS_TEXT);
    let adjective = adjectives[(hash as u32) as usize % adjectives.len()];
    let noun = nouns[(hash >> 32) as u32 as usize % nouns.len()];
    format!("{adjective}-{noun}")
}

/// The first four hex digits of `run_id` (issue #717's own collision suffix, `amber-falcon·0299`) -- `run_id`
/// is an opaque id (a UUID from both engines today), never parsed as a number, so this just reads its own
/// first four hex-digit characters as given, skipping any separator in between.
fn id_suffix(run_id: &str) -> String {
    run_id
        .chars()
        .filter(char::is_ascii_hexdigit)
        .take(4)
        .collect()
}

/// Every one of `run_ids`' own [`run_name`], in the same order, with the first four hex digits of its own
/// run id appended wherever two or more positions share the same bare name (issue #717: "if two runs on the
/// same screen get the same name, both add" the suffix) -- `None` passes straight through unchanged, a run
/// whose `run_start` hasn't arrived yet showing no name at all rather than colliding with anything.
pub fn disambiguated_names(run_ids: &[Option<&str>]) -> Vec<Option<String>> {
    let bare: Vec<Option<String>> = run_ids.iter().map(|id| id.map(run_name)).collect();
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for name in bare.iter().flatten() {
        *counts.entry(name.as_str()).or_insert(0) += 1;
    }
    run_ids
        .iter()
        .zip(bare.iter())
        .map(|(id, name)| {
            let name = name.as_ref()?;
            let collides = counts.get(name.as_str()).copied().unwrap_or(0) > 1;
            if collides {
                Some(format!(
                    "{name}\u{b7}{}",
                    id_suffix(id.expect("name implies id"))
                ))
            } else {
                Some(name.clone())
            }
        })
        .collect()
}

/// The NAME column's own width wherever one is drawn (issue #717's own narrow-width rule): as wide as the
/// longest name actually on this screen, plus 2 for a column break before the next field -- never a fixed
/// width, so a screen whose names are all short (or none named yet) never reserves more room than it needs,
/// leaving the rest for whichever field would otherwise be clipped. `names` is whatever a draw call is about
/// to show, already [`disambiguated_names`]'d; a screen with no named row at all still reserves the minimum
/// two columns, so a row that later gains a name doesn't reflow every other row around it.
pub fn name_column_width(names: &[Option<&str>]) -> usize {
    names
        .iter()
        .flatten()
        .map(|n| n.chars().count())
        .max()
        .map_or(2, |longest| longest + 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_name_is_pinned_on_fixed_ids() {
        assert_eq!(
            run_name("936ff268-5f52-479a-887f-fda1a47dae63"),
            run_name("936ff268-5f52-479a-887f-fda1a47dae63")
        );
        // Pinned literal names, so a word-list edit is a deliberate,
        // visible change to this test, not a silent drift.
        assert_eq!(
            run_name("936ff268-5f52-479a-887f-fda1a47dae63"),
            "square-jay"
        );
        assert_eq!(
            run_name("d2c8267e-2864-4e04-89d6-4f174bc64516"),
            "ochre-desert"
        );
        assert_eq!(
            run_name("0299262e-51a9-4956-bed4-743a78822a9e"),
            "icy-geyser"
        );
    }

    #[test]
    fn run_name_is_lowercase_and_hyphenated() {
        let name = run_name("some-run-id");
        assert!(name.chars().all(|c| c.is_ascii_lowercase() || c == '-'));
        assert_eq!(name.matches('-').count(), 1);
    }

    #[test]
    fn run_name_differs_for_different_ids() {
        assert_ne!(run_name("run-a"), run_name("run-b"));
    }

    #[test]
    fn word_lists_are_about_two_hundred_plain_short_words_each() {
        let adjectives = word_list(ADJECTIVES_TEXT);
        let nouns = word_list(NOUNS_TEXT);
        for words in [&adjectives, &nouns] {
            assert!(
                words.len() >= 180 && words.len() <= 220,
                "expected about 200 words, got {}",
                words.len()
            );
            let unique: std::collections::HashSet<&str> = words.iter().copied().collect();
            assert_eq!(unique.len(), words.len(), "word list has a duplicate");
            for word in words.iter() {
                assert!(
                    word.len() <= 7 && !word.is_empty(),
                    "{word:?} is not 1-7 characters"
                );
                assert!(
                    word.chars().all(|c| c.is_ascii_lowercase()),
                    "{word:?} is not plain lowercase ascii"
                );
            }
        }
    }

    #[test]
    fn id_suffix_reads_the_first_four_hex_characters() {
        assert_eq!(id_suffix("0299262e-51a9-4956-bed4-743a78822a9e"), "0299");
    }

    #[test]
    fn disambiguated_names_passes_through_a_single_run_unchanged() {
        let names = disambiguated_names(&[Some("936ff268-5f52-479a-887f-fda1a47dae63")]);
        assert_eq!(names, vec![Some("square-jay".to_string())]);
    }

    #[test]
    fn disambiguated_names_appends_the_suffix_only_on_a_real_collision() {
        // Two distinct ids verified ahead of time to hash to the same
        // bare name (`wary-tulip`) with different id suffixes, standing
        // in for the rare real collision the suffix exists for.
        let a = "00000016-aaaa-bbbb-cccc-dddddddddddd";
        let b = "0000007f-aaaa-bbbb-cccc-dddddddddddd";
        assert_eq!(run_name(a), run_name(b), "fixture pair must collide");
        let names = disambiguated_names(&[Some(a), Some(b)]);
        assert_eq!(
            names,
            vec![
                Some(format!("{}\u{b7}{}", run_name(a), id_suffix(a))),
                Some(format!("{}\u{b7}{}", run_name(b), id_suffix(b))),
            ]
        );
    }

    #[test]
    fn disambiguated_names_leaves_a_run_with_no_run_id_unnamed() {
        let names = disambiguated_names(&[Some("936ff268-5f52-479a-887f-fda1a47dae63"), None]);
        assert_eq!(names[1], None);
    }

    #[test]
    fn disambiguated_names_does_not_collide_a_pair_that_differs() {
        let names = disambiguated_names(&[Some("run-a"), Some("run-b")]);
        assert_ne!(names[0], None);
        assert!(!names[0].as_ref().unwrap().contains('\u{b7}'));
    }

    #[test]
    fn name_column_width_fits_the_longest_name_plus_two() {
        assert_eq!(name_column_width(&[Some("ab"), Some("abcde")]), 7);
    }

    #[test]
    fn name_column_width_has_a_minimum_with_no_name_at_all() {
        assert_eq!(name_column_width(&[None, None]), 2);
    }

    #[test]
    fn name_column_width_ignores_an_unnamed_row_among_named_ones() {
        assert_eq!(name_column_width(&[Some("abcd"), None]), 6);
    }
}
