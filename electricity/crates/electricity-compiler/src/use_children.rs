//! Lane I prerequisites (issue #448, item U4): every literal
//! `use:` child statically reachable from a document -- ported from
//! `core/cycle_check.py::iter_use_children`. Two walks, both built on
//! [`collect_use_paths`]/[`collect_use_inline`]:
//!
//! - [`iter_use_path_children`]: path/`orchestration:` children only
//!   (no `inline:`), the one both the up-front refusal walk and this
//!   module's own tests use when only a *file* child can matter --
//!   the refusal walk compiles each child the same way the parent was
//!   compiled, and an `inline:` child's final content doesn't exist
//!   until its own Mustache tags render, so there is nothing for it to
//!   check statically either way.
//! - [`iter_use_children`]: the full walk, `inline:` children
//!   included, for the allowlist check (`cli/allowlist.py::
//!   check_allowlist`'s own `iter_use_children` port) -- an inline
//!   child's provider/adapter *names* are already present in its
//!   unrendered text (only a Mustache *value* like `{{input.tool}}`
//!   isn't yet; [`electricity_config::orchestration_denials`]'s own
//!   `skip_templated` leaves exactly that case unjudged here, same as
//!   Circuitry).
//!
//! [`first_refused_child`] and [`allowlist_denials`] are the two
//! composed checks `electricity` (lib)'s own run wiring calls; wiring
//! either into a real run's own per-`use`-child runtime check (as
//! opposed to this up-front, literal-children-only prediction) stays
//! lane I's own job (`electricity/docs/spec/vm-lanes.md`'s own
//! `exec/use_.rs` row).
//!
//! A non-blank `ref:` on the same effect takes priority over `path:`/
//! `orchestration:`, exactly as `core/cycle_check.py::
//! collect_use_refs`'s own `elif` chain does -- but, unlike Circuitry,
//! contributes no child at all: a compiled document never carries
//! `ref:` (DESIGN.md §4 rejects it at compile time), and this walk
//! runs over *raw*, uncompiled documents, so resolving one here would
//! mean porting the library registry lookup `core/cycle_check.py::
//! resolve_reference` falls back to for `ref:` -- out of scope for a
//! static prediction this lane's own real per-child runtime check
//! (lane I) still has the final word over.
//!
//! **A fresh, self-contained walk, not a reuse of [`crate::cycles`]'s
//! own private helpers.** `crate::cycles::detect_cycles` already loads
//! every literal child for its own three-color DFS, but keeps every
//! helper that does so private to that file (an M0 file, predating the
//! M1 lane split entirely -- not any M1 lane's own), and M1 lanes
//! C/D1/H are running in parallel on this crate's other files right
//! now -- adding `pub(crate)` to `cycles.rs`'s own internals would be
//! an edit to a file this PR has no need to touch otherwise. The
//! read/parse behaviour below is intentionally identical: absolute
//! path, then the working directory, then the parent document's own
//! directory; an unreadable or unparseable child counts as an empty
//! document (`{}`), never a hard error -- the same divergences
//! `cycles.rs`'s own doc comment already notes (non-UTF-8 content
//! treated as unreadable rather than raising) apply here too.

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

/// Every `use`-typed effect dict anywhere in *document* -- the shared
/// first half of `core/cycle_check.py::collect_use_refs` and
/// `iter_use_children`'s own inline loop, which both walk the same
/// `effects`/`steps` + `finally` tree and then filter to `type: use`.
fn use_effect_dicts(document: &Value) -> Vec<&Dict> {
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
    effect_dicts
        .into_iter()
        .filter(|effect| {
            effect
                .get(&Value::Str("type".to_string()))
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_lowercase()
                == "use"
        })
        .collect()
}

fn trimmed_str_field<'a>(effect: &'a Dict, field: &str) -> Option<&'a str> {
    effect
        .get(&Value::Str(field.to_string()))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Every literal `path:`/`orchestration:` value named by a `use`
/// effect anywhere in *document* -- `core/cycle_check.py::
/// collect_use_refs`'s own `elif` chain: a non-blank `ref:` takes
/// priority over `path:`/`orchestration:` on the same effect and, since
/// this walk never resolves one (this module's own doc comment),
/// contributes no child at all rather than falling through to `path:`.
fn collect_use_paths(document: &Value) -> Vec<String> {
    use_effect_dicts(document)
        .into_iter()
        .filter_map(|effect| {
            if trimmed_str_field(effect, "ref").is_some() {
                return None;
            }
            trimmed_str_field(effect, "path")
                .or_else(|| trimmed_str_field(effect, "orchestration"))
                .map(str::to_string)
        })
        .collect()
}

/// `(effect name or "use", unrendered inline text)` for every `use`
/// effect anywhere in *document* carrying a non-blank `inline:` --
/// `core/cycle_check.py::iter_use_children`'s own inline loop, which
/// checks `inline:` independently of whatever `ref:`/`path:` the same
/// effect might also carry (Python's two loops over `collect_use_refs`
/// and `effect.get('inline')` are unconditional of each other).
fn collect_use_inline(document: &Value) -> Vec<(String, String)> {
    use_effect_dicts(document)
        .into_iter()
        .filter_map(|effect| {
            let text = effect
                .get(&Value::Str("inline".to_string()))
                .and_then(Value::as_str)?;
            let name = trimmed_str_field(effect, "name").unwrap_or("use");
            Some((name.to_string(), text.to_string()))
        })
        .collect()
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

/// `(label, child document)` for every `use` child statically
/// reachable from *document* -- the root excluded, each path visited
/// once and each distinct inline text visited once -- ported from
/// `core/cycle_check.py::iter_use_children` in full, *path:*/
/// `orchestration:` and `inline:` children alike. *label* is the
/// child's own resolved path (as [`iter_use_path_children`]), or
/// `inline '<name>'` for an inline child (`effect.get('name') or
/// 'use'`) -- the allowlist check's own per-child walk
/// (`cli/allowlist.py::check_allowlist`).
///
/// Per document, every path/`orchestration:` child is visited first
/// (depth-first, each fully explored before the next), then every
/// inline child -- Python's own `visit`'s two separate loops, in that
/// order. An inline child has no directory of its own: a nested
/// `path:`/`orchestration:` reference inside one resolves against the
/// *enclosing* document's own directory, the same *parent_dir* this
/// call already carries, never a directory of the inline child's own.
pub fn iter_use_children(document: &Value, entry_path: Option<&Path>) -> Vec<(String, Value)> {
    let mut seen: HashSet<String> = HashSet::new();
    if let Some(path) = entry_path {
        if let Ok(resolved) = path.canonicalize() {
            seen.insert(resolved.to_string_lossy().into_owned());
        }
    }
    let mut out = Vec::new();
    let root_parent_dir = entry_path.and_then(Path::parent).map(PathBuf::from);
    visit_with_inline(document, root_parent_dir.as_deref(), &mut seen, &mut out);
    out
}

fn visit_with_inline(
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
        visit_with_inline(&child, child_parent_dir.as_deref(), seen, out);
    }
    for (name, text) in collect_use_inline(document) {
        if seen.contains(&text) {
            continue;
        }
        seen.insert(text.clone());
        let Ok(child @ Value::Dict(_)) = electricity_yaml::load_yaml_last_key_wins(&text) else {
            continue;
        };
        out.push((format!("inline '{name}'"), child.clone()));
        // An inline child has no directory of its own -- a nested
        // path reference inside one resolves against the enclosing
        // document's own directory (*parent_dir*, unchanged).
        visit_with_inline(&child, parent_dir, seen, out);
    }
}

/// The up-front refusal walk's own composed check (M1 lane I
/// prerequisites, issue #448, item U4(a)): the first literal path
/// child, in walk order, whose own freshly-compiled `Program` is
/// itself unsupported against *supported* -- `None` if every child
/// compiles and passes, or if *document* has no literal path children
/// at all. A child's own directory is reconstructed from its resolved
/// label for [`crate::DocumentOrigin::File`] (the same shape
/// [`crate::compile::compile_document`] needs); a child that fails to
/// *compile* at all (a structural error, never a capability refusal)
/// is skipped here, not surfaced as a refusal -- lane I's own
/// per-child runtime check, which actually loads and runs the child,
/// is still the one that reports a real compile failure when a run
/// reaches it for real.
pub fn first_refused_child(
    document: &Value,
    entry_path: Option<&Path>,
    decomposition_enabled: bool,
    supported: &electricity_bytecode::Supported<'_>,
) -> Option<String> {
    for (label, child) in iter_use_path_children(document, entry_path) {
        let child_dir = Path::new(&label)
            .parent()
            .map(PathBuf::from)
            .unwrap_or_default();
        let origin = crate::DocumentOrigin::File {
            document_dir: child_dir.clone(),
            confinement_root: child_dir,
        };
        let Ok(program) = crate::compile::compile_document(&child, &origin) else {
            continue;
        };
        if let Some(refusal) =
            electricity_bytecode::first_unsupported(&program, decomposition_enabled, supported)
        {
            return Some(electricity_bytecode::format_refused_child(&label, &refusal));
        }
    }
    None
}

/// The allowlist check's own composed walk (M1 lane I prerequisites,
/// issue #448, item U4(b)): *document*'s own top-level denials
/// ([`electricity_config::orchestration_denials`], `(false, true)`,
/// unchanged M0 behaviour), followed by every reachable `use` child's
/// own denials (`skip_templated: true, include_document_adapter:
/// false`), each prefixed `"use child {label}: "` -- `cli/
/// allowlist.py::check_allowlist` in full, now that
/// `electricity-compiler` (unlike `electricity-config`, which cannot
/// depend back on this crate) can walk children itself. Children are
/// walked at all only when *enabled_adapters* or *enabled_tools* is
/// `Some` -- a wholly default-open config has nothing to find either
/// way, so the walk (and every child file read it implies) is skipped
/// outright, matching Python's own early return.
pub fn allowlist_denials(
    document: &Value,
    entry_path: Option<&Path>,
    enabled_adapters: Option<&[String]>,
    enabled_tools: Option<&[String]>,
) -> Vec<String> {
    let mut errors = electricity_config::orchestration_denials(
        document,
        enabled_adapters,
        enabled_tools,
        false,
        true,
    );
    if enabled_adapters.is_none() && enabled_tools.is_none() {
        return errors;
    }
    for (label, child) in iter_use_children(document, entry_path) {
        errors.extend(
            electricity_config::orchestration_denials(
                &child,
                enabled_adapters,
                enabled_tools,
                true,
                false,
            )
            .into_iter()
            .map(|denial| format!("use child {label}: {denial}")),
        );
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::{
        allowlist_denials, first_refused_child, iter_use_children, iter_use_path_children,
    };
    use electricity_bytecode::Supported;
    use electricity_value::{Dict, Value};
    use std::fs;
    use std::path::PathBuf;

    fn supported_with_use_only() -> Supported<'static> {
        Supported {
            use_: true,
            ..Supported::m0(&["json"])
        }
    }

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

    fn use_effect_with_ref(name: &str, ref_value: &str, path: &str) -> Value {
        let mut dict = Dict::new();
        dict.insert(
            Value::Str("type".to_string()),
            Value::Str("use".to_string()),
        );
        dict.insert(Value::Str("name".to_string()), Value::Str(name.to_string()));
        dict.insert(
            Value::Str("ref".to_string()),
            Value::Str(ref_value.to_string()),
        );
        dict.insert(Value::Str("path".to_string()), Value::Str(path.to_string()));
        Value::Dict(dict)
    }

    fn inline_use_effect(name: &str, inline: &str) -> Value {
        let mut dict = Dict::new();
        dict.insert(
            Value::Str("type".to_string()),
            Value::Str("use".to_string()),
        );
        dict.insert(Value::Str("name".to_string()), Value::Str(name.to_string()));
        dict.insert(
            Value::Str("inline".to_string()),
            Value::Str(inline.to_string()),
        );
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

    #[test]
    fn a_non_blank_ref_contributes_no_child_even_with_a_path_also_set() {
        // `core/cycle_check.py::collect_use_refs`'s own `elif` chain: a
        // non-blank `ref:` takes priority over `path:` on the same
        // effect -- and, since this walk never resolves a `ref:` at all
        // (this module's own doc comment), the effect contributes no
        // child, not the `path:` child Python's own `elif` would also
        // never reach.
        let dir = tmp_dir("ref-priority");
        fs::write(dir.join("a.yaml"), "effects: []\n").unwrap();
        let entry = dir.join("root.yaml");
        let document =
            doc_with_effects(vec![use_effect_with_ref("sub", "library:thing", "a.yaml")]);

        assert_eq!(iter_use_path_children(&document, Some(&entry)), vec![]);
        assert_eq!(iter_use_children(&document, Some(&entry)), vec![]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn iter_use_children_includes_an_inline_child_parsing_as_a_mapping() {
        let document = doc_with_effects(vec![inline_use_effect("sub", "effects: []")]);
        let children = iter_use_children(&document, None);
        assert_eq!(children.len(), 1);
        let (label, child) = &children[0];
        assert_eq!(label, "inline 'sub'");
        assert_eq!(
            child,
            &Value::Dict({
                let mut d = Dict::new();
                d.insert(Value::Str("effects".to_string()), Value::List(vec![]));
                d
            })
        );
    }

    #[test]
    fn iter_use_children_skips_an_inline_child_whose_text_is_not_a_mapping() {
        let document = doc_with_effects(vec![inline_use_effect("sub", "- 1\n- 2\n")]);
        assert_eq!(iter_use_children(&document, None), vec![]);
    }

    #[test]
    fn iter_use_children_names_an_unnamed_inline_child_use() {
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
        let children = iter_use_children(&document, None);
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].0, "inline 'use'");
    }

    #[test]
    fn iter_use_children_dedupes_two_inline_children_with_identical_text() {
        let document = doc_with_effects(vec![
            inline_use_effect("a", "effects: []"),
            inline_use_effect("b", "effects: []"),
        ]);
        let children = iter_use_children(&document, None);
        assert_eq!(children.len(), 1, "identical inline text is visited once");
    }

    #[test]
    fn a_nested_path_reference_inside_an_inline_child_resolves_against_the_enclosing_directory() {
        // An inline child has no directory of its own -- a `path:`
        // reference nested inside one resolves against the *enclosing*
        // document's own directory (`core/cycle_check.py::
        // iter_use_children`'s own `visit(child, parent_dir)`, never a
        // directory of the inline child's own).
        let dir = tmp_dir("inline-nested-path");
        fs::write(dir.join("nested.yaml"), "effects: []\n").unwrap();
        let entry = dir.join("root.yaml");
        let document = doc_with_effects(vec![inline_use_effect(
            "sub",
            "effects:\n  - type: use\n    name: inner\n    path: nested.yaml\n",
        )]);

        let children = iter_use_children(&document, Some(&entry));
        assert_eq!(children.len(), 2);
        assert_eq!(children[0].0, "inline 'sub'");
        assert!(children[1].0.ends_with("nested.yaml"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn path_children_are_walked_depth_first_before_any_inline_child() {
        let dir = tmp_dir("order");
        fs::write(dir.join("a.yaml"), "effects: []\n").unwrap();
        let entry = dir.join("root.yaml");
        let document = doc_with_effects(vec![
            use_effect("path_child", "a.yaml"),
            inline_use_effect("inline_child", "effects: []"),
        ]);

        let children = iter_use_children(&document, Some(&entry));
        assert_eq!(children.len(), 2);
        assert!(children[0].0.ends_with("a.yaml"));
        assert_eq!(children[1].0, "inline 'inline_child'");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn allowlist_denials_skips_the_child_walk_entirely_when_both_lists_are_default_open() {
        let document = doc_with_effects(vec![inline_use_effect(
            "sub",
            "effects:\n  - type: tool\n    name: run_it\n    provider: shell\n",
        )]);
        assert_eq!(
            allowlist_denials(&document, None, None, None),
            Vec::<String>::new()
        );
    }

    #[test]
    fn allowlist_denials_finds_an_inline_childs_own_disallowed_tool() {
        let document = doc_with_effects(vec![inline_use_effect(
            "sub",
            "effects:\n  - type: tool\n    name: run_it\n    provider: shell\n",
        )]);
        let enabled_tools = vec!["json".to_string()];
        let denials = allowlist_denials(&document, None, None, Some(&enabled_tools));
        assert_eq!(denials.len(), 1);
        assert!(denials[0].starts_with("use child inline 'sub': "));
        assert!(denials[0].contains("tool 'shell'"));
    }

    #[test]
    fn first_refused_child_names_the_child_the_walk_finds() {
        let dir = tmp_dir("first-refused");
        fs::write(
            dir.join("child.yaml"),
            "effects:\n  - type: tool\n    name: run_it\n    provider: shell\n    params: {command: 'echo hi'}\n",
        )
        .unwrap();
        let entry = dir.join("root.yaml");
        let document = doc_with_effects(vec![use_effect("child", "child.yaml")]);

        let refusal =
            first_refused_child(&document, Some(&entry), false, &supported_with_use_only())
                .expect("the child's own shell tool must be refused");
        assert!(refusal.contains("child.yaml"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn first_refused_child_is_none_when_every_child_is_supported() {
        let dir = tmp_dir("no-refusal");
        fs::write(dir.join("child.yaml"), "effects: []\n").unwrap();
        let entry = dir.join("root.yaml");
        let document = doc_with_effects(vec![use_effect("child", "child.yaml")]);

        assert_eq!(
            first_refused_child(&document, Some(&entry), false, &supported_with_use_only()),
            None
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
