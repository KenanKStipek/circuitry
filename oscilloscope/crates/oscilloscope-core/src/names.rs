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

/// A run's own display name (issue #752): its own cleaned label
/// when it has one, else [`run_name`]'s own generated one -- `None`
/// only when there is neither (no label, and `run_id` itself not
/// known yet). `label` is always already [`clean_label`]'d by the
/// one caller that reads it off a file (`render::build_header`,
/// `discovery::discover_runs`, `history::build_record`); this never
/// cleans it again, so a caller that hands in raw, unclean text
/// gets it back unclean.
pub fn display_name(label: Option<&str>, run_id: Option<&str>) -> Option<String> {
    match label {
        Some(label) => Some(label.to_string()),
        None => run_id.map(run_name),
    }
}

/// Every one of `runs`' own `(label, run_id)` pair turned into its own [`display_name`], in the same
/// order, disambiguated against every other position on the same screen (issue #752, extending #717's own
/// rule): two positions whose bare display name collides each add their own tell-apart suffix -- two shared
/// labels each add their own [`run_name`] (`ticket-42 \u{b7} amber-falcon`), two shared generated names (no
/// label on either) each add the first four hex digits of their own run id instead (`amber-falcon\u{b7}0299`,
/// #717's own original rule), exactly as before when no label is in play at all. A label whose own run id
/// isn't known yet (a file written by something other than `cof`/`electricity` can carry a `label` with no
/// `run_id` at all, see [`clean_label`]'s own doc comment on why osi never trusts a file to be well-formed)
/// gets no suffix to derive one from either: a colliding one instead gets a plain positional count, `\u{b7}2`,
/// `\u{b7}3`, ... in the order `runs` lists them, its very first occurrence keeping the bare name unsuffixed --
/// never a panic. A label alone is capped at [`MAX_NAME_DISPLAY_CELLS`] display cells, cut with `\u{22ef}`,
/// before collision counting -- the "longest + 2" column-width rule ([`name_column_width`]) a caller derives
/// from this result's own strings never has to reserve more than that for one run's own name, however long a
/// label actually is; the tell-apart suffix itself is never further cut, since it alone still has to tell the
/// two runs apart. `None` passes straight through unchanged, a run whose `run_start` hasn't arrived yet (no
/// label, no run id) showing no name at all rather than colliding with anything.
pub fn disambiguated_names(runs: &[(Option<&str>, Option<&str>)]) -> Vec<Option<String>> {
    let bare: Vec<Option<String>> = runs
        .iter()
        .map(|(label, id)| {
            display_name(*label, *id)
                .map(|name| truncate_display_width(&name, MAX_NAME_DISPLAY_CELLS))
        })
        .collect();
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for name in bare.iter().flatten() {
        *counts.entry(name.as_str()).or_insert(0) += 1;
    }
    let suffixed: Vec<Option<String>> = runs
        .iter()
        .zip(bare.iter())
        .map(|((label, id), name)| {
            let name = name.as_ref()?;
            let collides = counts.get(name.as_str()).copied().unwrap_or(0) > 1;
            if !collides {
                return Some(name.clone());
            }
            let Some(id) = id else {
                // No run id to tell this one apart with at all --
                // left for the positional pass below, which also
                // needs every other colliding bare name's own count
                // decided first.
                return None;
            };
            Some(match label {
                Some(_) => format!("{name} \u{b7} {}", run_name(id)),
                None => format!("{name}\u{b7}{}", id_suffix(id)),
            })
        })
        .collect();
    // Finding on #752's own collision rule: a shared label's own
    // generated-name suffix (`run_name(id)`, same ~40,000-way spread
    // as the bare generated name itself) can collide too, for two
    // different ids -- counted only among `suffixed`'s own `Some`s,
    // so a `None` left for the positional pass just below never
    // joins this count.
    let mut suffixed_counts: std::collections::HashMap<&str, usize> =
        std::collections::HashMap::new();
    for name in suffixed.iter().flatten() {
        *suffixed_counts.entry(name.as_str()).or_insert(0) += 1;
    }
    let mut no_id_seen: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    runs.iter()
        .zip(bare.iter())
        .zip(suffixed.iter())
        .map(|(((label, id), name), suffixed_name)| {
            let name = name.as_ref()?;
            match suffixed_name {
                Some(suffixed_name) => {
                    // The extra append only ever applies to a shared
                    // label's own `run_name(id)` suffix (the one with
                    // a ~40,000-way spread that can itself collide
                    // for two different ids) -- the no-label branch's
                    // `id_suffix(id)` keeps #717's own original rule
                    // unchanged, exactly as before #752.
                    if label.is_some()
                        && suffixed_counts
                            .get(suffixed_name.as_str())
                            .copied()
                            .unwrap_or(0)
                            > 1
                    {
                        let id = id.expect("a suffixed name always came from a known run id");
                        Some(format!("{suffixed_name}\u{b7}{}", id_suffix(id)))
                    } else {
                        Some(suffixed_name.clone())
                    }
                }
                None => {
                    let seen = no_id_seen.entry(name.as_str()).or_insert(0);
                    *seen += 1;
                    if *seen == 1 {
                        Some(name.clone())
                    } else {
                        Some(format!("{name}\u{b7}{seen}"))
                    }
                }
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
        .map(|n| display_width(n))
        .max()
        .map_or(2, |longest| longest + 2)
}

/// A label's own cap in a column (issue #752's §3: "a label counts
/// at most 32 display cells, cut with `⋯`") -- applied once, inside
/// [`disambiguated_names`], to every bare display name before
/// collision counting; never applied to the header's own name,
/// which shows a label in full.
pub const MAX_NAME_DISPLAY_CELLS: usize = 32;

/// `text`'s own width in terminal cells (issue #752's §3: "use
/// display width, CJK and emoji count 2, never byte or char
/// counts") -- `unicode_width::UnicodeWidthChar`'s own per-`char`
/// answer, summed; a control character (none of which ever survive
/// [`clean_label`]) would count 0, same as `unicode-width` itself
/// answers for one.
pub fn display_width(text: &str) -> usize {
    text.chars()
        .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(0))
        .sum()
}

/// `text`, cut to at most `max_cells` display cells, with a
/// trailing `⋯` (itself one cell) once it doesn't fit whole --
/// [`truncate_text`]'s own shape in `oscilloscope`'s `tui.rs`, but
/// by display width rather than by `char` count, so a label holding
/// wide characters is never cut shorter, or let run longer, than
/// its own cap actually allows.
pub fn truncate_display_width(text: &str, max_cells: usize) -> String {
    if display_width(text) <= max_cells {
        return text.to_string();
    }
    if max_cells == 0 {
        return String::new();
    }
    let budget = max_cells - 1;
    let mut cut = String::new();
    let mut width = 0usize;
    for ch in text.chars() {
        let w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + w > budget {
            break;
        }
        cut.push(ch);
        width += w;
    }
    cut.push('⋯');
    cut
}

/// `text`, right-padded with plain spaces to `width` display cells
/// (issue #752's §3: "never byte or char counts") -- the column-
/// padding equivalent of [`truncate_display_width`]: `format!("{:<
/// width$}", text)` pads by `char` count, which overshoots for any
/// text narrower, cell for cell, than its own `char` count (a label
/// holding a CJK character or an emoji) and leaves every later
/// column misaligned. `text` already wider than `width` is returned
/// unchanged, same as `format!`'s own `<width$>` for a `str` that
/// overruns its column.
pub fn pad_display_width(text: &str, width: usize) -> String {
    let pad = width.saturating_sub(display_width(text));
    let mut padded = text.to_string();
    padded.push_str(&" ".repeat(pad));
    padded
}

/// A run's own label (issue #752), cleaned exactly the way both
/// engines already clean it before ever writing it -- osi cleans it
/// again on every read, since a `--events`/state file can be
/// written by anything else, not only a `cof`/`electricity` that
/// cleaned it first: ANSI/CSI escape sequences stripped first (so a
/// lone `\x1b` byte they leave behind is never mistaken for a
/// control character of its own), then every run of whitespace
/// (line breaks included) collapsed to one plain space, every
/// remaining control character dropped outright, the result trimmed
/// and cut to at most 64 characters. `None` for anything that
/// cleans away to nothing at all -- an empty or all-control/
/// whitespace label is the same as no label.
pub fn clean_label(raw: &str) -> Option<String> {
    let stripped = crate::engine::strip_ansi_escapes(raw);
    let mut cleaned = String::new();
    let mut in_space_run = false;
    for ch in stripped.chars() {
        if ch.is_whitespace() {
            if !in_space_run {
                cleaned.push(' ');
                in_space_run = true;
            }
        } else if ch.is_control() {
            continue;
        } else {
            cleaned.push(ch);
            in_space_run = false;
        }
    }
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return None;
    }
    let cut: String = trimmed.chars().take(64).collect();
    let cut = cut.trim_end();
    if cut.is_empty() {
        None
    } else {
        Some(cut.to_string())
    }
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
    fn display_name_prefers_a_label_over_the_generated_name() {
        assert_eq!(
            display_name(
                Some("ticket 42"),
                Some("936ff268-5f52-479a-887f-fda1a47dae63")
            ),
            Some("ticket 42".to_string())
        );
    }

    #[test]
    fn display_name_falls_back_to_the_generated_name_with_no_label() {
        assert_eq!(
            display_name(None, Some("936ff268-5f52-479a-887f-fda1a47dae63")),
            Some("square-jay".to_string())
        );
    }

    #[test]
    fn display_name_is_none_with_neither_a_label_nor_a_run_id() {
        assert_eq!(display_name(None, None), None);
    }

    #[test]
    fn disambiguated_names_passes_through_a_single_run_unchanged() {
        let names = disambiguated_names(&[(None, Some("936ff268-5f52-479a-887f-fda1a47dae63"))]);
        assert_eq!(names, vec![Some("square-jay".to_string())]);
    }

    #[test]
    fn disambiguated_names_appends_the_suffix_only_on_a_real_collision() {
        // Two distinct ids verified ahead of time to hash to the same
        // bare name with different id suffixes, standing in for the
        // rare real collision the suffix exists for.
        let a = "00000016-aaaa-bbbb-cccc-dddddddddddd";
        let b = "0000007f-aaaa-bbbb-cccc-dddddddddddd";
        assert_eq!(run_name(a), run_name(b), "fixture pair must collide");
        let names = disambiguated_names(&[(None, Some(a)), (None, Some(b))]);
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
        let names = disambiguated_names(&[
            (None, Some("936ff268-5f52-479a-887f-fda1a47dae63")),
            (None, None),
        ]);
        assert_eq!(names[1], None);
    }

    #[test]
    fn disambiguated_names_does_not_collide_a_pair_that_differs() {
        let names = disambiguated_names(&[(None, Some("run-a")), (None, Some("run-b"))]);
        assert_ne!(names[0], None);
        assert!(!names[0].as_ref().unwrap().contains('\u{b7}'));
    }

    #[test]
    fn disambiguated_names_adds_the_generated_name_only_on_a_real_label_collision() {
        let names = disambiguated_names(&[
            (
                Some("ticket 42"),
                Some("936ff268-5f52-479a-887f-fda1a47dae63"),
            ),
            (
                Some("ticket 42"),
                Some("d2c8267e-2864-4e04-89d6-4f174bc64516"),
            ),
        ]);
        assert_eq!(
            names,
            vec![
                Some("ticket 42 \u{b7} square-jay".to_string()),
                Some("ticket 42 \u{b7} ochre-desert".to_string()),
            ]
        );
    }

    #[test]
    fn disambiguated_names_does_not_collide_two_distinct_labels() {
        let names = disambiguated_names(&[
            (
                Some("ticket 42"),
                Some("936ff268-5f52-479a-887f-fda1a47dae63"),
            ),
            (
                Some("ticket 43"),
                Some("d2c8267e-2864-4e04-89d6-4f174bc64516"),
            ),
        ]);
        assert_eq!(
            names,
            vec![Some("ticket 42".to_string()), Some("ticket 43".to_string()),]
        );
    }

    #[test]
    fn disambiguated_names_never_panics_on_a_shared_label_with_no_run_id_at_all() {
        // A file written by something other than cof/electricity can
        // carry `runtime.last_run.label` with no `_run_id` at all --
        // two such runs sharing a label must never panic, and each
        // gets told apart by plain position instead of a suffix
        // derived from an id neither one has.
        let names = disambiguated_names(&[(Some("ticket 42"), None), (Some("ticket 42"), None)]);
        assert_eq!(
            names,
            vec![
                Some("ticket 42".to_string()),
                Some("ticket 42\u{b7}2".to_string()),
            ]
        );
    }

    #[test]
    fn disambiguated_names_numbers_a_third_shared_label_with_no_run_id() {
        let names = disambiguated_names(&[
            (Some("ticket 42"), None),
            (Some("ticket 42"), None),
            (Some("ticket 42"), None),
        ]);
        assert_eq!(
            names,
            vec![
                Some("ticket 42".to_string()),
                Some("ticket 42\u{b7}2".to_string()),
                Some("ticket 42\u{b7}3".to_string()),
            ]
        );
    }

    #[test]
    fn disambiguated_names_tells_apart_two_shared_labels_whose_generated_names_also_collide() {
        // Two distinct ids whose bare generated name collides (the
        // same ~40,000-way spread a shared label's own tell-apart
        // suffix, `run_name(id)`, draws from) but whose `id_suffix`
        // doesn't -- found by search, since `run_name`'s hash gives
        // no simple way to construct one by hand.
        let mut by_name: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        let (a, b) = (0..100_000)
            .map(|i| format!("id-{i}"))
            .find_map(|id| {
                let name = run_name(&id);
                if let Some(other) = by_name.get(&name) {
                    if id_suffix(other) != id_suffix(&id) {
                        return Some((other.clone(), id));
                    }
                }
                by_name.insert(name, id);
                None
            })
            .expect("some collision within 100k ids");
        let names = disambiguated_names(&[
            (Some("ticket 42"), Some(a.as_str())),
            (Some("ticket 42"), Some(b.as_str())),
        ]);
        assert_ne!(names[0], names[1], "{names:?}");
        for name in names.iter().flatten() {
            assert!(name.starts_with("ticket 42 \u{b7} "), "{name:?}");
        }
    }

    #[test]
    fn disambiguated_names_caps_a_long_label_at_32_display_cells() {
        let long = "x".repeat(80);
        let names = disambiguated_names(&[(Some(long.as_str()), Some("run-a"))]);
        let name = names[0].as_ref().unwrap();
        assert_eq!(display_width(name), 32);
        assert!(name.ends_with('\u{22ef}'));
    }

    #[test]
    fn clean_label_collapses_whitespace_runs_and_drops_control_characters() {
        assert_eq!(
            clean_label("ticket\t\n  42\x07done"),
            Some("ticket 42done".to_string())
        );
    }

    #[test]
    fn clean_label_strips_ansi_escapes() {
        assert_eq!(
            clean_label("\x1b[1;36mticket 42\x1b[0m"),
            Some("ticket 42".to_string())
        );
    }

    #[test]
    fn clean_label_drops_an_osc_sequences_own_body_terminated_by_bel() {
        // A terminal title set (`ESC ] 0 ; title BEL`) drops the
        // title text itself, not only the two control bytes around
        // it (issue #752's own cleaning rule: "escape sequences are
        // dropped", not just their control bytes).
        assert_eq!(
            clean_label("ticket 42\x1b]0;evil title\x07done"),
            Some("ticket 42done".to_string())
        );
    }

    #[test]
    fn clean_label_drops_an_osc_sequences_own_body_terminated_by_st() {
        assert_eq!(
            clean_label("ticket 42\x1b]0;evil title\x1b\\done"),
            Some("ticket 42done".to_string())
        );
    }

    #[test]
    fn clean_label_drops_a_two_byte_escape_sequence() {
        // `ESC c` (a full terminal reset) is neither CSI nor OSC --
        // just the one byte after `ESC` itself, dropped the same way.
        assert_eq!(clean_label("ticket\x1bc 42"), Some("ticket 42".to_string()));
    }

    #[test]
    fn clean_label_trims_and_cuts_at_64_characters() {
        let raw = format!("  {}  ", "a".repeat(100));
        assert_eq!(clean_label(&raw), Some("a".repeat(64)));
    }

    #[test]
    fn clean_label_is_none_for_text_that_cleans_away_to_nothing() {
        assert_eq!(clean_label("  \x01\x02  "), None);
    }

    #[test]
    fn display_width_counts_wide_characters_twice() {
        assert_eq!(display_width("ab"), 2);
        assert_eq!(display_width("\u{4e2d}\u{6587}"), 4);
    }

    #[test]
    fn truncate_display_width_cuts_a_wide_character_string_by_cells_not_chars() {
        // Five two-cell characters: 10 cells, cut to 6 keeps only two
        // whole characters plus the ellipsis (2+2+1 = 5, the next
        // character's own 2 cells would make 7).
        let text = "\u{4e2d}\u{6587}\u{4e2d}\u{6587}\u{4e2d}";
        let cut = truncate_display_width(text, 6);
        assert_eq!(display_width(&cut), 5);
        assert!(cut.ends_with('\u{22ef}'));
    }

    #[test]
    fn pad_display_width_pads_a_wide_character_string_by_cells_not_chars() {
        // One CJK character is 2 cells but 1 char: padding to 5
        // cells needs 3 spaces, not 4.
        assert_eq!(pad_display_width("注", 5), "注   ");
    }

    #[test]
    fn pad_display_width_leaves_text_already_at_width_unchanged() {
        assert_eq!(pad_display_width("abc", 3), "abc");
    }

    #[test]
    fn truncate_display_width_leaves_text_that_already_fits_unchanged() {
        assert_eq!(truncate_display_width("amber-falcon", 32), "amber-falcon");
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
