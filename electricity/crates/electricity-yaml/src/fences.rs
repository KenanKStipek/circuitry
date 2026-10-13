//! `core/use.py::_clean_yaml_fences` -- moved here from
//! `electricity-vm::exec::use_` (issue #521, §2): `electricity-tools`'s
//! own `validate_yaml` tool (issue #448's decision 3) needs this
//! function too, and `electricity-tools` cannot depend on
//! `electricity-vm`, where it used to live. `exec::use_::
//! clean_yaml_fences` is now `pub use electricity_yaml::
//! clean_yaml_fences;`, so every existing caller is unaffected.

/// `text.splitlines()`/`line.strip()`, never Rust's own `str::lines()`/
/// `str::trim()` (issue #506's own "Hazards" section: model output --
/// the one place this function ever runs against untrusted text, a
/// reflector's generated plan or a decomposition plan included -- can
/// carry `\x0c`, a lone `\r`, or a Unicode line/paragraph separator,
/// which Python's `splitlines()` treats as a line boundary and Rust's
/// `lines()` does not).
pub fn clean_yaml_fences(text: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in electricity_value::pycompat::py_splitlines(text) {
        let stripped = electricity_value::pycompat::py_strip(&line);
        if stripped.starts_with("```") || stripped == "---" {
            continue;
        }
        lines.push(line);
    }
    electricity_value::pycompat::py_strip(&lines.join("\n")).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_yaml_fences_drops_code_fences_and_document_separators() {
        let text = "```yaml\neffects: []\n---\n```";
        assert_eq!(clean_yaml_fences(text), "effects: []");
    }

    #[test]
    fn clean_yaml_fences_keeps_original_indentation() {
        let text = "effects:\n  - type: tool\n```\n";
        assert_eq!(clean_yaml_fences(text), "effects:\n  - type: tool");
    }

    #[test]
    fn clean_yaml_fences_splits_on_every_python_splitlines_separator() {
        // Issue #506's own "Hazards" section: `str::lines()` only
        // splits on `\n`/`\r\n` -- model output reaches this function
        // first, so it has to split on every boundary Python's
        // `str.splitlines()` does, not just those two.
        assert_eq!(clean_yaml_fences("a\x0cb"), "a\nb");
        assert_eq!(clean_yaml_fences("a\rb"), "a\nb");
        assert_eq!(clean_yaml_fences("a\u{2028}b"), "a\nb");
        assert_eq!(clean_yaml_fences("a\u{85}b"), "a\nb");
        assert_eq!(
            clean_yaml_fences("```\x0ceffects: []\x0c---\x0c```"),
            "effects: []"
        );
    }
}
