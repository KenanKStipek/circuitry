//! Lane C: ports `core/cycle_check.py`'s `detect_cycles` -- a static
//! `use` cycle walk over `path:`/`orchestration:` references (`ref:` is
//! rejected at compile time, DESIGN.md §4, so there is no library
//! lookup and no `ref:` edge to walk; `inline:` uses are skipped, the
//! same way Python's own `collect_use_refs` skips them: an inline
//! document's content only exists after its Mustache tags render, which
//! a static walk cannot see).
//!
//! A child is read the way `core/cycle_check.py::load_orch` reads it:
//! absolute path, then the working directory (the same filesystem
//! check -- a relative path that exists is found either way), then the
//! parent document's directory; an unreadable or unparseable child
//! counts as an empty document (`{}`), never a hard error.
//!
//! Called from `pipeline.rs` (lane B), after a document compiles, in
//! both `check_for_run` and `check_report` -- mirroring
//! `cli/runtime_shim.py`'s own two calls to `core/cycle_check.py`'s
//! `detect_cycles`, right after the concurrency-group check
//! ([`crate::groups::unknown_group_errors`]).
//!
//! # Known divergences
//!
//! - **No duplicate-key check.** `core/cycle_check.py::load_orch` reads
//!   a child with plain `yaml.safe_load` (last key wins silently),
//!   unlike Circuitry's own main loader. electricity-yaml (a merged
//!   crate, additive-only in this lane) exposes only its own
//!   duplicate-key-rejecting [`electricity_yaml::load_yaml`]; a child
//!   with a duplicate key is treated as unreadable (empty) here instead
//!   of being read with the last key winning. Unreachable by an
//!   ordinary orchestration (a hand-written document has no reason to
//!   repeat a key), and the parent error this would otherwise mask
//!   (`c2-duplicate-key`) is already reported earlier, against the
//!   *entry* document, by the structural check (lane B) -- never
//!   against a `use` child.
//! - **Non-UTF-8 content.** Python's `path.read_text(encoding="utf-8")`
//!   raises `UnicodeDecodeError` uncaught here (not a `yaml.YAMLError`,
//!   so `load_orch`'s own `except (OSError, yaml.YAMLError)` doesn't
//!   catch it) -- an existing Circuitry edge case this port does not
//!   reproduce; a non-UTF-8 child is treated as unreadable (empty)
//!   rather than propagating a hard error.
//! - **No root self-identity.** Python's `detect_cycles` seeds its DFS
//!   with the *root* document's own resolved path
//!   (`root_path.resolve()`), so a child whose `path:`/`orchestration:`
//!   resolves back to the entry document itself is caught as a cycle.
//!   [`DocumentOrigin::File`] carries the document's *directory* only,
//!   not its filename, so this port cannot recover that identity and
//!   seeds the root under a sentinel (`"<root>"`) that no real resolved
//!   child path can ever equal. A cycle entirely among `use` children
//!   (`B -> C -> B`, reachable from the root but not including it) is
//!   still detected correctly; one that loops back through the root
//!   document itself is not. Flagged for the orchestrator -- fixing it
//!   needs either `DocumentOrigin` or `pipeline.rs` (lane B) to carry
//!   the entry path through to this call, neither of which this lane
//!   owns.

use crate::{CompileError, DocumentOrigin};
use electricity_value::{Dict, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// `core/cycle_check.py::_walk_effects`'s own child-container keys --
/// deliberately not [`crate::compose`]'s `CHILD_LISTS`: a *nested*
/// effect's own `effects:` is walked, but not a nested `steps:` legacy
/// alias (only the top-level list applies that fallback, matching
/// Python's `collect_use_refs` computing `effects = orch.get("effects")
/// or orch.get("steps") or []` once, before `_walk_effects` ever
/// recurses).
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

/// Every effect dict anywhere under *effects* (a top-level or nested
/// effects/then/else/body/finally list), ported from
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

/// `(kind, value)` for every `use` effect in *document* referencing a
/// file -- `"path"` for both `path:` and the deprecated `orchestration:`
/// alias; inline-mode uses are skipped (no static content to walk), and
/// `ref:` never appears here in practice (compiling already rejects it,
/// DESIGN.md §4) -- but is skipped defensively, not treated as a file
/// reference, if it somehow did. Ports
/// `core/cycle_check.py::collect_use_refs`.
fn collect_use_refs(document: &Value) -> Vec<String> {
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

/// Resolves a `path:`/`orchestration:` value the way
/// `core/cycle_check.py::resolve_reference` does for a `path`-kind
/// reference, minus the final library-lookup fallback (DESIGN.md §4):
/// absolute path, then the working directory (one filesystem check:
/// `Path::is_file` on a relative path checks it against the process's
/// current directory, exactly like Python's `Path(value).exists()`),
/// then the parent document's directory.
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

/// Universal-newline text read (`\r\n`/lone `\r` -> `\n`), matching
/// Python's default text-mode `read_text`. `None` for any IO or UTF-8
/// decode failure -- both treated as "unreadable" by [`load_child`].
fn read_text_universal_newlines(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    Some(text.replace("\r\n", "\n").replace('\r', "\n"))
}

/// Parses *path* as YAML, treating any read/parse failure or a
/// non-mapping root as an empty document -- `core/cycle_check.py::load_orch`.
fn load_child(path: &Path) -> Value {
    let empty = || Value::Dict(Dict::new());
    let Some(text) = read_text_universal_newlines(path) else {
        return empty();
    };
    match electricity_yaml::load_yaml(&text) {
        Ok(value @ Value::Dict(_)) => value,
        _ => empty(),
    }
}

/// Three-color DFS cycle search starting from *document*, ported from
/// `core/cycle_check.py::detect_cycles`'s own `visit` closure.
fn visit(
    document: &Value,
    identity: &str,
    parent_dir: Option<&Path>,
    color: &mut HashMap<String, u8>,
    parent_chain: &mut Vec<String>,
    cache: &mut HashMap<String, Value>,
) -> Option<Vec<String>> {
    match color.get(identity).copied().unwrap_or(0) {
        1 => {
            let idx = parent_chain.iter().position(|p| p == identity).unwrap_or(0);
            let mut cycle: Vec<String> = parent_chain[idx..].to_vec();
            cycle.push(identity.to_string());
            return Some(cycle);
        }
        2 => return None,
        _ => {}
    }

    color.insert(identity.to_string(), 1);
    parent_chain.push(identity.to_string());

    let result = (|| {
        for value in collect_use_refs(document) {
            let Some(resolved) = resolve_reference(&value, parent_dir) else {
                continue;
            };
            let child_identity = resolved.to_string_lossy().to_string();
            if !cache.contains_key(&child_identity) {
                let child = load_child(&resolved);
                cache.insert(child_identity.clone(), child);
            }
            let child_document = cache.get(&child_identity).expect("just inserted").clone();
            let child_parent_dir = resolved.parent().map(PathBuf::from);
            if let Some(cycle) = visit(
                &child_document,
                &child_identity,
                child_parent_dir.as_deref(),
                color,
                parent_chain,
                cache,
            ) {
                return Some(cycle);
            }
        }
        None
    })();

    parent_chain.pop();
    color.insert(identity.to_string(), 2);
    result
}

/// A `Cycle: a → b → a` error among *document*'s `use` effects, if any
/// -- ported from `core/cycle_check.py::detect_cycles` (see this
/// module's own docs for the divergences from the Python reference).
pub(crate) fn detect_cycles(document: &Value, origin: &DocumentOrigin) -> Result<(), CompileError> {
    let parent_dir = match origin {
        DocumentOrigin::File { document_dir, .. } => Some(document_dir.clone()),
        DocumentOrigin::Generated => None,
    };
    let mut color = HashMap::new();
    let mut parent_chain = Vec::new();
    let mut cache = HashMap::new();
    if let Some(cycle) = visit(
        document,
        "<root>",
        parent_dir.as_deref(),
        &mut color,
        &mut parent_chain,
        &mut cache,
    ) {
        return Err(CompileError(format!("Cycle: {}", cycle.join(" → "))));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::detect_cycles;
    use crate::DocumentOrigin;
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

    fn origin_for(dir: &std::path::Path) -> DocumentOrigin {
        DocumentOrigin::File {
            document_dir: dir.to_path_buf(),
            confinement_root: dir.to_path_buf(),
        }
    }

    fn tmp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "electricity-cycles-test-{tag}-{}-{}",
            std::process::id(),
            tag
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn no_use_effects_is_fine() {
        let document = doc_with_effects(vec![]);
        let dir = tmp_dir("none");
        assert!(detect_cycles(&document, &origin_for(&dir)).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unresolvable_reference_is_not_a_cycle() {
        let document = doc_with_effects(vec![use_effect("sub", "does-not-exist.yaml")]);
        let dir = tmp_dir("unresolvable");
        assert!(detect_cycles(&document, &origin_for(&dir)).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_two_hop_cycle_among_children_is_detected() {
        let dir = tmp_dir("two-hop");
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

        let document = doc_with_effects(vec![use_effect("sub", "a.yaml")]);
        let err = detect_cycles(&document, &origin_for(&dir)).unwrap_err();
        assert!(err.0.starts_with("Cycle: "));
        assert!(err.0.contains(" → "));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_acyclic_chain_is_fine() {
        let dir = tmp_dir("chain");
        fs::write(
            dir.join("a.yaml"),
            "effects:\n  - type: use\n    name: s\n    path: b.yaml\n",
        )
        .unwrap();
        fs::write(dir.join("b.yaml"), "effects: []\n").unwrap();

        let document = doc_with_effects(vec![use_effect("sub", "a.yaml")]);
        assert!(detect_cycles(&document, &origin_for(&dir)).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unreadable_child_counts_as_empty_not_an_error() {
        let dir = tmp_dir("unreadable");
        fs::write(dir.join("a.yaml"), "not: [valid: yaml:\n").unwrap();

        let document = doc_with_effects(vec![use_effect("sub", "a.yaml")]);
        assert!(detect_cycles(&document, &origin_for(&dir)).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn inline_uses_are_not_followed() {
        let mut inline_effect = Dict::new();
        inline_effect.insert(
            Value::Str("type".to_string()),
            Value::Str("use".to_string()),
        );
        inline_effect.insert(Value::Str("name".to_string()), Value::Str("s".to_string()));
        inline_effect.insert(
            Value::Str("inline".to_string()),
            Value::Str("effects: []".to_string()),
        );
        let document = doc_with_effects(vec![Value::Dict(inline_effect)]);
        let dir = tmp_dir("inline");
        assert!(detect_cycles(&document, &origin_for(&dir)).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }
}
