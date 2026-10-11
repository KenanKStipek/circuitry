//! Replays `electricity/scripts/generate_process_tools_corpus.py`'s own
//! differential corpus -- CPython's real `shlex.split`/`shlex.quote` --
//! against [`electricity_value::pycompat::shlex_split`]/
//! [`electricity_value::pycompat::shlex_quote`].
//!
//! The corpus is checked in (`tests/golden/shlex_corpus.json`); CI
//! separately regenerates it with Python 3.11 and fails the build if
//! it differs (`.github/workflows/electricity-generated.yml`), so this
//! test only needs to trust the committed file.

use electricity_value::pycompat::{shlex_quote, shlex_split};
use serde::Deserialize;

#[derive(Deserialize)]
struct SplitExpected {
    ok: Option<Vec<String>>,
    err: Option<String>,
}

#[derive(Deserialize)]
struct SplitCase {
    input: String,
    expected: SplitExpected,
}

#[derive(Deserialize)]
struct QuoteCase {
    input: String,
    expected: String,
}

#[derive(Deserialize)]
struct Corpus {
    shlex_split_cases: Vec<SplitCase>,
    shlex_quote_cases: Vec<QuoteCase>,
}

#[test]
fn golden_corpus_shlex() {
    let text = include_str!("golden/shlex_corpus.json");
    let corpus: Corpus =
        serde_json::from_str(text).expect("golden/shlex_corpus.json is valid JSON");
    assert!(!corpus.shlex_split_cases.is_empty());
    assert!(!corpus.shlex_quote_cases.is_empty());

    let mut failures = Vec::new();
    for case in &corpus.shlex_split_cases {
        let got = shlex_split(&case.input);
        match (&case.expected.ok, &case.expected.err, got) {
            (Some(expected), None, Ok(got)) if *expected == got => {}
            (None, Some(expected), Err(got)) if *expected == got => {}
            (_, _, got) => failures.push(format!("input={:?}: got {:?}", case.input, got)),
        }
    }
    for case in &corpus.shlex_quote_cases {
        let got = shlex_quote(&case.input).expect("shlex_quote never fails");
        if got != case.expected {
            failures.push(format!(
                "quote input={:?}: expected {:?}, got {:?}",
                case.input, case.expected, got
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
