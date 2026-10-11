//! Lane I prerequisites (issue #448, item U4): every literal
//! `use: {path:}`/`orchestration:` child statically reachable from a
//! document -- ported from `core/cycle_check.py::iter_use_children`,
//! narrowed to its own path/orchestration half (an inline child has no
//! static file content to check before its own Mustache tags render,
//! and `ref:` never appears in a compiled document at all: DESIGN.md
//! §4 rejects it at compile time).
//!
//! [`iter_use_path_children`] is the one seam two different checks
//! need run against every loaded child, before a real run ever reaches
//! it (`electricity/docs/spec/vm-lanes.md`'s own `exec/use_.rs` row):
//!
//! - the refusal walker (`electricity_bytecode::refusal::
//!   first_unsupported`) against each child's own freshly-compiled
//!   `Program`, the same `Supported` value the parent document was
//!   already checked against;
//! - the allowlist check (`electricity_config::orchestration_denials`,
//!   `skip_templated: true,
//!   include_document_adapter: false` -- the same two options
//!   `cli/allowlist.py::check_allowlist`'s own per-child loop passes)
//!   against each child's own raw document.
//!
//! Wiring either check into a real run is lane I's own job (this
//! module only exposes the walk both need); M0 behaviour is unchanged
//! either way, since `use` stays refused outright
//! (`RefusalReason::Use`) while `Supported::use_` is `false`.
//!
//! **A fresh, self-contained walk, not a reuse of [`crate::cycles`]'s
//! own private helpers.** `crate::cycles::detect_cycles` already loads
//! every literal child for its own three-color DFS, but keeps every
//! helper that does so private to that file, and M1 lanes C/D1/H are
//! running in parallel on this crate's other files right now -- adding
//! `pub(crate)` to `cycles.rs`'s own internals would be an edit to a
//! file this PR has no need to touch otherwise. The read/parse
//! behaviour below is intentionally identical: absolute path, then the
//! working directory, then the parent document's own directory; an
//! unreadable or unparseable child counts as an empty document (`{}`),
//! never a hard error -- the same divergences `cycles.rs`'s own doc
//! comment already notes (non-UTF-8 content treated as unreadable
//! rather than raising) apply here too.

use electricity_value::{Dict, Value};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

/// `core/cycle_check.py::_walk_effects`'s own child-container keys.
const CHILD_LISTS: [&str; 5] = ["effects", "then", "else", "body", "finally"];

fn dict_get<'a>(document: &'a Value, key: &str) -> Option<&'a Value> {
    document.as_dict()?.get(&Value::Str(key.to_string()))
}

fn is_truthy(value: &Value) -> bool {
    match value {
        Value::None => false,
        Value::Bool(b) => *b,
        Value::Int(i) => !i.is_zero(),
        Value::Float(f) => *f != 0.0,
        Value::Str(s) => !s.is_empty(),
        Value::Bytes(b) => !b.is_empty(),
        Value::List(items) => !items.is_empty(),
        Value::Dict(d) => !d.is_empty(),
        Value::Date(_) | Value::DateTime(..) => true,
    }
}

/// Every effect dict anywhere under *effects*, ported from
/// `core/cycle_check.py::_walk_effects`.
fn walk_effects<'a>(effects: &'a Value, out: &mut Vec<&'a Dict>) {
    let Value::List(items) = effects else {
        return;
    };
    for effect in items {
        let Some(dict) = effect.as_dict() else {
            continue;
        };
        out.push(dict);
        for field in CHILD_LISTS {
            if let Some(child @ Value::List(_)) = dict.get(&Value::Str(field.to_string())) {
                walk_effects(child, out);
            }
        }
    }
}

/// Every literal `path:`/`orchestration:` value named by a `use`
/// effect anywhere in *document* -- `core/cycle_check.py::
/// collect_use_refs`'s own path/orchestration half (`ref:` is skipped
/// defensively, the same as `cycles.rs`, though a compiled document
/// never carries one; `inline:` has no path to collect).
fn collect_use_paths(document: &Value) -> Vec<String> {
    let mut effect_dicts = Vec::new();
    let top_effects = dict_get(document, "effects");
    let start = if top_effects.is_some_and(is_truthy) {
        top_effects
    } else {
        dict_get(document, "steps")
    };
    if let Some(effects) = start {
        walk_effects(effects, &mut effect_dicts);
    }
    if let Some(finally) = dict_get(document, "finally") {
        walk_effects(finally, &mut effect_dicts);
    }

    let mut out = Vec::new();
    for effect in effect_dicts {
        let effect_type = effect
            .get(&Value::Str("type".to_string()))
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_lowercase();
        if effect_type != "use" {
            continue;
        }
        let path_field = effect
            .get(&Value::Str("path".to_string()))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let legacy = effect
            .get(&Value::Str("orchestration".to_string()))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty());
        if let Some(path) = path_field.or(legacy) {
            out.push(path.to_string());
        }
    }
    out
}

/// Resolves a `path:`/`orchestration:` value: absolute path, then the
/// working directory, then the parent document's own directory --
/// `core/cycle_check.py::resolve_reference`'s own path-kind branch,
/// minus the final library-lookup fallback (DESIGN.md §4: electricity
/// never resolves a library reference).
fn resolve_reference(value: &str, parent_dir: Option<&Path>) -> Option<PathBuf> {
    let candidate = Path::new(value);
    if candidate.is_file() {
        return candidate.canonicalize().ok();
    }
    if let Some(parent_dir) = parent_dir {
        let relative = parent_dir.join(value);
        if relative.is_file() {
            return relative.canonicalize().ok();
        }
    }
    None
}

/// Universal-newline text read, `None` for any IO/UTF-8 failure --
/// both "unreadable" to [`load_child`].
fn read_text_universal_newlines(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    Some(text.replace("\r\n", "\n").replace('\r', "\n"))
}

/// Parses *path* as YAML, treating any read/parse failure or a
/// non-mapping root as an empty document -- `core/cycle_check.py::
/// load_orch`.
fn load_child(path: &Path) -> Value {
    let empty = || Value::Dict(Dict::new());
    let Some(text) = read_text_universal_newlines(path) else {
        return empty();
    };
    match electricity_yaml::load_yaml_last_key_wins(&text) {
        Ok(value @ Value::Dict(_)) => value,
        _ => empty(),
    }
}

/// `(label, child document)` for every literal `path:`/`orchestration:`
/// `use` child statically reachable from *document* -- the root
/// excluded, each resolved path visited once -- ported from
/// `core/cycle_check.py::iter_use_children`'s own path/orchestration
/// half. *label* is the child's own resolved path, as a string (the
/// same node identity [`crate::cycles::detect_cycles`] itself tracks).
///
/// *entry_path* is the root document's own path as given (`None` for a
/// document with no path of its own); a `path:`/`orchestration:` child
/// of the root resolves against *entry_path*'s own (unresolved) parent
/// directory, same as [`crate::cycles::detect_cycles`]'s own root
/// handling and Python's `root_path.parent`.
pub fn iter_use_path_children(document: &Value, entry_path: Option<&Path>) -> Vec<(String, Value)> {
    let mut seen: HashSet<String> = HashSet::new();
    if let Some(path) = entry_path {
        if let Ok(resolved) = path.canonicalize() {
            seen.insert(resolved.to_string_lossy().into_owned());
        }
    }
    let mut out = Vec::new();
    let root_parent_dir = entry_path.and_then(Path::parent).map(PathBuf::from);
    visit(document, root_parent_dir.as_deref(), &mut seen, &mut out);
    out
}

fn visit(
    document: &Value,
    parent_dir: Option<&Path>,
    seen: &mut HashSet<String>,
    out: &mut Vec<(String, Value)>,
) {
    for value in collect_use_paths(document) {
        let Some(resolved) = resolve_reference(&value, parent_dir) else {
            continue;
        };
        let identity = resolved.to_string_lossy().into_owned();
        if !seen.insert(identity.clone()) {
            continue;
        }
        let child = load_child(&resolved);
        let child_parent_dir = resolved.parent().map(PathBuf::from);
        out.push((identity, child.clone()));
        visit(&child, child_parent_dir.as_deref(), seen, out);
    }
}

#[cfg(test)]
mod tests {
    use super::iter_use_path_children;
    use electricity_value::{Dict, Value};
    use std::fs;
    use std::path::PathBuf;

    fn use_effect(name: &str, path: &str) -> Value {
        let mut dict = Dict::new();
        dict.insert(
            Value::Str("type".to_string()),
            Value::Str("use".to_string()),
        );
        dict.insert(Value::Str("name".to_string()), Value::Str(name.to_string()));
        dict.insert(Value::Str("path".to_string()), Value::Str(path.to_string()));
        Value::Dict(dict)
    }

    fn doc_with_effects(effects: Vec<Value>) -> Value {
        let mut dict = Dict::new();
        dict.insert(Value::Str("effects".to_string()), Value::List(effects));
        Value::Dict(dict)
    }

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "electricity-use-children-test-{tag}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn no_use_effects_finds_nothing() {
        let document = doc_with_effects(vec![]);
        assert_eq!(iter_use_path_children(&document, None), vec![]);
    }

    #[test]
    fn a_single_literal_path_child_is_found_and_loaded() {
        let dir = tmp_dir("single");
        fs::write(dir.join("a.yaml"), "effects: []\n").unwrap();
        let entry = dir.join("root.yaml");
        let document = doc_with_effects(vec![use_effect("sub", "a.yaml")]);

        let children = iter_use_path_children(&document, Some(&entry));
        assert_eq!(children.len(), 1);
        let (label, child) = &children[0];
        assert!(label.ends_with("a.yaml"));
        assert_eq!(
            child,
            &Value::Dict({
                let mut d = Dict::new();
                d.insert(Value::Str("effects".to_string()), Value::List(vec![]));
                d
            })
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_chain_of_literal_children_is_followed_to_every_leaf() {
        let dir = tmp_dir("chain");
        fs::write(
            dir.join("a.yaml"),
            "effects:\n  - type: use\n    name: s\n    path: b.yaml\n",
        )
        .unwrap();
        fs::write(dir.join("b.yaml"), "effects: []\n").unwrap();
        let entry = dir.join("root.yaml");
        let document = doc_with_effects(vec![use_effect("sub", "a.yaml")]);

        let children = iter_use_path_children(&document, Some(&entry));
        assert_eq!(children.len(), 2);
        assert!(children[0].0.ends_with("a.yaml"));
        assert!(children[1].0.ends_with("b.yaml"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_inline_use_contributes_no_literal_path_child() {
        let mut inline_effect = Dict::new();
        inline_effect.insert(
            Value::Str("type".to_string()),
            Value::Str("use".to_string()),
        );
        inline_effect.insert(
            Value::Str("inline".to_string()),
            Value::Str("effects: []".to_string()),
        );
        let document = doc_with_effects(vec![Value::Dict(inline_effect)]);
        assert_eq!(iter_use_path_children(&document, None), vec![]);
    }

    #[test]
    fn a_cycle_through_literal_children_terminates_instead_of_looping() {
        let dir = tmp_dir("cycle");
        fs::write(
            dir.join("a.yaml"),
            "effects:\n  - type: use\n    name: s\n    path: b.yaml\n",
        )
        .unwrap();
        fs::write(
            dir.join("b.yaml"),
            "effects:\n  - type: use\n    name: s\n    path: a.yaml\n",
        )
        .unwrap();
        let entry = dir.join("root.yaml");
        let document = doc_with_effects(vec![use_effect("sub", "a.yaml")]);

        let children = iter_use_path_children(&document, Some(&entry));
        assert_eq!(children.len(), 2, "a.yaml and b.yaml, each exactly once");
        let _ = fs::remove_dir_all(&dir);
    }
}
