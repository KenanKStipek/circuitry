//! A run's label: the document's own top-level `label:` template, or
//! `--label` (#752). Ports `circuitry.core.label` byte for byte: the same
//! cleaning rules, the same render-failure-warns-and-continues contract.
//!
//! `label:`'s own syntax (does it parse as Mustache) is checked only by
//! `cof check` (`core/document_check.py::label_syntax_error`); electricity
//! has no `check` subcommand of its own. A malformed template that reaches
//! a run anyway -- an unchecked file, or `electricity run` itself -- is a
//! [`render_document_label`] warning, never a failure of
//! [`crate::run::run_orchestration`]'s own hard-fail gate.

use electricity_template::{PlainCtx, render_template_with_escape};
use electricity_value::{Dict, Value};

/// Cut point, in Unicode scalar values (`char`s) -- not grapheme
/// clusters, matching `core/label.py::MAX_LABEL_LENGTH`.
pub const MAX_LABEL_LENGTH: usize = 64;

/// Whether *c* is one of the ASCII whitespace bytes this module treats
/// as collapsible (`core/label.py::_WHITESPACE_RE`'s own narrower-than-
/// `\s` definition, chosen so both engines agree without depending on
/// either language's Unicode-whitespace tables): space, tab, newline,
/// carriage return, form feed, vertical tab.
fn is_collapsible_whitespace(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c' | '\x0b')
}

/// Whether *c* is a C0 control character (0x00-0x1F), DEL (0x7F), or a
/// C1 control character (0x80-0x9F) -- `core/label.py::_CONTROL_RE`'s
/// own range, applied *after* whitespace collapsing so the ASCII
/// whitespace bytes above (themselves C0 controls) have already become
/// a literal space and never reach this check.
fn is_control(c: char) -> bool {
    let code = c as u32;
    (0x00..=0x1f).contains(&code) || code == 0x7f || (0x80..=0x9f).contains(&code)
}

/// *text*, cleaned for display as a run's label -- `core/label.py::
/// clean_label`, ported byte for byte:
///
/// 1. every ESC-initiated sequence (an OSC terminated by BEL/ST, a CSI
///    ending in a final byte, or ESC plus one other character) is
///    dropped;
/// 2. every run of [`is_collapsible_whitespace`] characters becomes one
///    ASCII space;
/// 3. every remaining [`is_control`] character is dropped;
/// 4. the result is trimmed and cut at [`MAX_LABEL_LENGTH`] `char`s, with
///    nothing appended.
///
/// An empty result means no label -- callers check for that themselves;
/// this always returns a `String`, never an `Option`.
pub fn clean_label(text: &str) -> String {
    let without_escapes = strip_escape_sequences(text);

    let mut collapsed = String::with_capacity(without_escapes.len());
    let mut in_whitespace_run = false;
    for c in without_escapes.chars() {
        if is_collapsible_whitespace(c) {
            if !in_whitespace_run {
                collapsed.push(' ');
                in_whitespace_run = true;
            }
        } else {
            collapsed.push(c);
            in_whitespace_run = false;
        }
    }

    let without_controls: String = collapsed.chars().filter(|c| !is_control(*c)).collect();
    let trimmed = without_controls.trim();
    let cut: String = trimmed.chars().take(MAX_LABEL_LENGTH).collect();
    // The cut can land right after a whitespace run's collapsed single
    // space, leaving a trailing space the earlier `trim()` never saw.
    cut.trim_end().to_string()
}

/// Drops every ESC-initiated sequence in *text*: an OSC (`ESC ]` ...
/// terminated by BEL or `ESC \`), a CSI (`ESC [` then parameter/
/// intermediate characters then one final character in `@`-`~`), or --
/// for any other character following ESC -- that one character, as a
/// simple two-character escape. A lone trailing ESC with nothing after
/// it, or an ESC-initiated sequence that never finds its own terminator
/// (an unclosed OSC, a CSI with no final byte), matches none of these
/// and only the leading `ESC` plus its very next character (if any) is
/// consumed, exactly as `core/label.py::_ESCAPE_RE`'s regex backtracks
/// to its own trailing `.` alternative in the same case -- walked here
/// by hand, index by index, since this crate's dependency set has no
/// regex engine, but equivalent to that regex: parameter/intermediate/
/// final-byte ranges never overlap, so a maximal greedy scan that then
/// checks for exactly one terminator needs no real backtracking.
fn strip_escape_sequences(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c != '\x1b' {
            out.push(c);
            i += 1;
            continue;
        }
        if i + 1 >= chars.len() {
            // Lone trailing ESC -- left in place for the control-strip
            // pass to drop.
            out.push(c);
            i += 1;
            continue;
        }
        match chars[i + 1] {
            ']' => {
                // OSC: scan from just past "ESC ]" for BEL or ESC+'\\'.
                let mut j = i + 2;
                let mut terminator_end = None;
                while j < chars.len() {
                    if chars[j] == '\x07' {
                        terminator_end = Some(j + 1);
                        break;
                    }
                    if chars[j] == '\x1b' {
                        if j + 1 < chars.len() && chars[j + 1] == '\\' {
                            terminator_end = Some(j + 2);
                        }
                        break;
                    }
                    j += 1;
                }
                i = terminator_end.unwrap_or(i + 2);
            }
            '[' => {
                // CSI: parameter bytes (0x30-0x3F)*, intermediate bytes
                // (0x20-0x2F)*, one final byte (0x40-0x7E).
                let mut j = i + 2;
                while j < chars.len() && (0x30..=0x3f).contains(&(chars[j] as u32)) {
                    j += 1;
                }
                while j < chars.len() && (0x20..=0x2f).contains(&(chars[j] as u32)) {
                    j += 1;
                }
                if j < chars.len() && (0x40..=0x7e).contains(&(chars[j] as u32)) {
                    i = j + 1;
                } else {
                    i += 2;
                }
            }
            _ => {
                i += 2;
            }
        }
    }
    out
}

/// Render the document's own top-level `label:` against a context
/// holding only this run's resolved `input.*` -- nothing else has run
/// yet (#752). Returns `(label, warning)`: *label* is `None` when
/// *label_template* isn't a non-empty string, or the rendered+cleaned
/// text was empty; *warning* is set, with *label* `None`, when
/// rendering raised (malformed template or any other render failure) --
/// `core/label.py::render_document_label`, ported.
pub fn render_document_label(
    label_template: Option<&str>,
    input_namespace: Value,
) -> (Option<String>, Option<String>) {
    let Some(template) = label_template.filter(|t| !t.is_empty()) else {
        return (None, None);
    };
    let mut root_dict = Dict::new();
    root_dict.insert(Value::Str("input".to_string()), input_namespace);
    let root = Value::Dict(root_dict);
    // escape: false -- a label is display text in a terminal or a log,
    // not HTML, the same reasoning that turned escaping off for prompt
    // text (#397).
    match render_template_with_escape(template, &root, &PlainCtx, "label", false) {
        Ok(rendered) => {
            let cleaned = clean_label(&rendered);
            if cleaned.is_empty() {
                (None, None)
            } else {
                (Some(cleaned), None)
            }
        }
        Err(err) => (
            None,
            Some(format!("{err}; the run continues without a label.")),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_unchanged() {
        assert_eq!(clean_label("hello world"), "hello world");
    }

    #[test]
    fn newlines_collapse_to_one_space() {
        assert_eq!(clean_label("a\n\nb"), "a b");
    }

    #[test]
    fn tabs_collapse_to_one_space() {
        assert_eq!(clean_label("a\t\tb"), "a b");
    }

    #[test]
    fn ansi_colour_codes_are_dropped() {
        assert_eq!(clean_label("a\x1b[31mred\x1b[0mb"), "aredb");
    }

    #[test]
    fn osc_title_sequence_bel_terminated_is_dropped() {
        assert_eq!(clean_label("a\x1b]0;title\x07b"), "ab");
    }

    #[test]
    fn osc_title_sequence_st_terminated_is_dropped() {
        assert_eq!(clean_label("a\x1b]0;title\x1b\\b"), "ab");
    }

    #[test]
    fn bare_bel_is_dropped() {
        assert_eq!(clean_label("a\x07b"), "ab");
    }

    #[test]
    fn c1_control_is_dropped() {
        assert_eq!(clean_label("a\u{85}b"), "ab");
    }

    #[test]
    fn combining_marks_are_preserved() {
        let text = "e\u{301}clair";
        assert_eq!(clean_label(text), text);
    }

    #[test]
    fn only_whitespace_is_empty() {
        assert_eq!(clean_label("   \t\n  "), "");
    }

    #[test]
    fn leading_and_trailing_whitespace_trimmed() {
        assert_eq!(clean_label("  hi  "), "hi");
    }

    #[test]
    fn exactly_64_is_unchanged() {
        let text = "x".repeat(64);
        let cleaned = clean_label(&text);
        assert_eq!(cleaned, text);
        assert_eq!(cleaned.chars().count(), 64);
    }

    #[test]
    fn sixty_five_is_cut_to_64() {
        let text = "x".repeat(65);
        let cleaned = clean_label(&text);
        assert_eq!(cleaned, "x".repeat(64));
        assert_eq!(cleaned.chars().count(), 64);
    }

    #[test]
    fn cut_lands_on_a_collapsed_space() {
        let text = format!("{} y", "x".repeat(63));
        assert_eq!(clean_label(&text), "x".repeat(63));
    }

    #[test]
    fn emoji_at_the_cut_is_not_split() {
        let text = format!("{}🎉y", "x".repeat(63));
        let cleaned = clean_label(&text);
        assert_eq!(cleaned, format!("{}🎉", "x".repeat(63)));
        assert_eq!(cleaned.chars().count(), 64);
    }

    #[test]
    fn render_document_label_renders_against_input_only() {
        let mut input = Dict::new();
        input.insert(
            Value::Str("ticket".to_string()),
            Value::Str("42".to_string()),
        );
        let (label, warning) =
            render_document_label(Some("ticket {{input.ticket}}"), Value::Dict(input));
        assert_eq!(label, Some("ticket 42".to_string()));
        assert_eq!(warning, None);
    }

    #[test]
    fn render_document_label_missing_input_renders_empty_no_label() {
        let (label, warning) =
            render_document_label(Some("{{input.ticket}}"), Value::Dict(Dict::new()));
        assert_eq!(label, None);
        assert_eq!(warning, None);
    }

    #[test]
    fn render_document_label_none_or_empty_is_no_label_no_warning() {
        let (label, warning) = render_document_label(None, Value::Dict(Dict::new()));
        assert_eq!(label, None);
        assert_eq!(warning, None);

        let (label, warning) = render_document_label(Some(""), Value::Dict(Dict::new()));
        assert_eq!(label, None);
        assert_eq!(warning, None);
    }

    #[test]
    fn render_document_label_malformed_template_warns_once_no_label() {
        let (label, warning) = render_document_label(Some("{{#open"), Value::Dict(Dict::new()));
        assert_eq!(label, None);
        let warning = warning.expect("a malformed template warns");
        assert!(warning.contains("label"));
        assert!(warning.contains("malformed Mustache template"));
        assert!(warning.ends_with("the run continues without a label."));
    }

    #[test]
    fn render_document_label_does_not_html_escape() {
        let mut input = Dict::new();
        input.insert(
            Value::Str("raw".to_string()),
            Value::Str("R&D \"fix\" <x> 'y'".to_string()),
        );
        let (label, warning) = render_document_label(Some("{{input.raw}}"), Value::Dict(input));
        assert_eq!(label, Some("R&D \"fix\" <x> 'y'".to_string()));
        assert_eq!(warning, None);
    }

    #[test]
    fn render_document_label_cleans_the_result() {
        let mut input = Dict::new();
        input.insert(Value::Str("a".to_string()), Value::Str("x".to_string()));
        input.insert(Value::Str("b".to_string()), Value::Str("y".to_string()));
        let (label, warning) =
            render_document_label(Some("  {{input.a}}\t{{input.b}}  "), Value::Dict(input));
        assert_eq!(label, Some("x y".to_string()));
        assert_eq!(warning, None);
    }
}
