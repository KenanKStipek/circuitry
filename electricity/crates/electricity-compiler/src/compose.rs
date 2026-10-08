//! Lane D: ports `core/prompt_compose.py`'s composition checks — each
//! fragment's syntax, set-delimiter tags refused, unknown names
//! resolved scope-aware, collisions between declared prompts and
//! effects, references to non-text effects, cycles among declared
//! prompts. The `type: yield` effect's own compile step is lane C's;
//! this module only handles its role in composition (a `yield` is
//! text-producing, so it is a valid bare/dotted `{{> name}}` target).
//!
//! Called from `compile::compile_document` (lane C) right after
//! [`crate::prompt_files::compile_declared_prompts`], matching
//! `core/compiler.py::compile_orchestration`'s order: the composition
//! checks run before any effect compiles.

use crate::prompt_files::resolve_text_or_file;
use crate::{CompileError, DocumentOrigin};
use electricity_value::Value;
use indexmap::IndexMap;
use std::collections::BTreeSet;

/// A `{{> name}}` name: a declared prompt or effect name, optionally
/// dotted (`pipeline.outline`) to reach a nested/composed effect's
/// state -- `core/prompt_compose.py`'s `_NAME_SHAPE`.
fn name_shape_matches(name: &str) -> bool {
    name.split('.').all(is_plain_identifier) && !name.is_empty()
}

fn is_plain_identifier(segment: &str) -> bool {
    let mut chars = segment.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

/// Every distinct name a `{{> name}}` tag in *text* names -- `core/
/// prompt_compose.py`'s `_PARTIAL_TAG`/`partial_references`, which
/// returns a `set[str]` (a name repeated in the same text, e.g. `'{{>
/// nope}} {{> nope}}'`, is reported once, not once per occurrence): the
/// sigil must immediately follow `{{` with no leading whitespace, and
/// `{{{>x}}}` (a "no escape" tag, not a partial) is excluded.
fn partial_references(text: &str) -> BTreeSet<String> {
    let bytes = text.as_bytes();
    let mut names = BTreeSet::new();
    let mut i = 0;
    while let Some(offset) = text[i..].find("{{>") {
        let start = i + offset;
        // Exclude `{{{>` -- that opening brace makes it a `{{{ ... }}}`
        // no-escape tag, not a partial (mirrors the regex's negative
        // lookbehind `(?<!\{)`).
        if start > 0 && bytes[start - 1] == b'{' {
            i = start + 3;
            continue;
        }
        let after = start + 3;
        match text[after..].find("}}") {
            Some(end_offset) => {
                let name = text[after..after + end_offset].trim().to_string();
                names.insert(name);
                i = after + end_offset + 2;
            }
            None => break,
        }
    }
    names
}

/// Container fields whose values are lists of child effect dicts,
/// walked when collecting every effect name anywhere in the document
/// (not into a `use` effect's own child document -- that compiles
/// separately) -- `core/prompt_compose.py`'s `_CHILD_LISTS`.
pub(crate) const CHILD_LISTS: [&str; 6] = ["effects", "steps", "body", "then", "else", "finally"];

/// Effect types whose value is prose text a declared prompt/`{{> name}}`
/// may splice in -- `core/prompt_compose.py`'s `_TEXT_PRODUCING_TYPES`,
/// folded into [`effect_is_text_producing`] since a `prompt`'s own
/// qualification depends on its `prompt_type`.
fn effect_is_text_producing(effect_type: &str, effect: &Value) -> bool {
    if effect_type == "yield" {
        return true;
    }
    if effect_type != "prompt" {
        return false;
    }
    match dict_get(effect, "prompt_type") {
        None | Some(Value::None) => true,
        Some(Value::Str(s)) => s == "text",
        _ => false,
    }
}

/// Effect types whose named children nest under the container's own
/// name in real state (`prime.<name>.<child>.value`), so a bare `{{>
/// child}}` never resolves to one -- only the dotted form (`{{>
/// name.child}}`) does, from anywhere in the document --
/// `core/prompt_compose.py`'s `_SCOPE_INTRODUCING_TYPES`.
fn is_scope_introducing(effect_type: &str) -> bool {
    effect_type == "if" || effect_type == "dynamic"
}

pub(crate) fn dict_get<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.as_dict()?.get(&Value::Str(key.to_string()))
}

fn effect_type_of(effect: &Value) -> String {
    match dict_get(effect, "type") {
        Some(Value::Str(s)) => s.trim().to_lowercase(),
        _ => String::new(),
    }
}

fn effect_name_of(effect: &Value) -> Option<&str> {
    match dict_get(effect, "name") {
        Some(Value::Str(s)) if !s.is_empty() => Some(s.as_str()),
        _ => None,
    }
}

/// Python truthiness for the `effects or steps` fallback: a missing key,
/// `None`, `False`, a numeric zero, and any empty `str`/`bytes`/`list`/
/// `dict` are falsy; everything else is truthy.
fn is_truthy(value: Option<&Value>) -> bool {
    match value {
        None => false,
        Some(Value::None) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Int(i)) => !i.is_zero(),
        Some(Value::Float(f)) => *f != 0.0,
        Some(Value::Str(s)) => !s.is_empty(),
        Some(Value::Bytes(b)) => !b.is_empty(),
        Some(Value::List(items)) => !items.is_empty(),
        Some(Value::Dict(d)) => !d.is_empty(),
        Some(Value::Date(_)) | Some(Value::DateTime(..)) => true,
    }
}

/// The top-level `effects` list, falling back to `steps` by
/// truthiness -- `orch.get("effects") or orch.get("steps") or []`.
pub(crate) fn effects_or_steps(document: &Value) -> &[Value] {
    let effects = dict_get(document, "effects");
    let chosen = if is_truthy(effects) {
        effects
    } else {
        dict_get(document, "steps")
    };
    match chosen {
        Some(Value::List(items)) => items,
        _ => &[],
    }
}

pub(crate) fn finally_list(document: &Value) -> &[Value] {
    match dict_get(document, "finally") {
        Some(Value::List(items)) => items,
        _ => &[],
    }
}

/// Every effect name anywhere in *document*, flattened regardless of
/// nesting -- `core/prompt_compose.py`'s `all_effect_names`. Used for
/// the declared-prompt/effect-name collision check and -- once lane C
/// wires it in -- `Program.effect_names`.
///
/// Walks with an explicit stack rather than recursion, so a document as
/// deep as [`electricity_value::MAX_DEPTH`] allows can't overflow the
/// stack here either (`electricity-value`'s own [`Drop`](electricity_value::Value)
/// impl uses the same shape, for the same reason).
pub(crate) fn all_effect_names(document: &Value) -> BTreeSet<String> {
    let mut names = BTreeSet::new();

    let mut pending: Vec<&Value> = Vec::new();
    pending.extend(effects_or_steps(document).iter());
    pending.extend(finally_list(document).iter());

    while let Some(effect) = pending.pop() {
        let Some(dict) = effect.as_dict() else {
            continue;
        };
        if let Some(Value::Str(name)) = dict.get(&Value::Str("name".to_string())) {
            if !name.is_empty() {
                names.insert(name.clone());
            }
        }
        for field in CHILD_LISTS {
            if let Some(Value::List(items)) = dict.get(&Value::Str(field.to_string())) {
                pending.extend(items.iter());
            }
        }
    }

    names
}

/// One node of the document's own effect tree, as
/// [`check_prompt_composition`] builds it -- an arena entry rather than
/// a `Value`-shaped recursive struct, so the tree can be built and
/// walked by plain `usize` index without the borrow-checker conflicts
/// real shared mutable references (Python's own dict-of-dicts) would
/// otherwise force.
struct EffectNode {
    effect_type: String,
    text_producing: bool,
    children: IndexMap<String, usize>,
}

/// The document's effect tree: index `0` is always the root (not itself
/// a real effect -- `core/prompt_compose.py`'s top-level `root` dict).
struct EffectTree {
    arena: Vec<EffectNode>,
}

impl EffectTree {
    fn new() -> (Self, usize) {
        let arena = vec![EffectNode {
            effect_type: String::new(),
            text_producing: false,
            children: IndexMap::new(),
        }];
        (EffectTree { arena }, 0)
    }

    /// Registers *name* as a child of *parent*, overwriting any earlier
    /// child of the same name with a *fresh*, empty-children node --
    /// `out[name] = {...}` in Python, which rebinds the dict key to a
    /// brand new dict object. A duplicate effect name within one scope
    /// is itself a later compile error (`core/compiler.py`'s own
    /// `_validate_name`/`_compile_effects_in_scope`, not run yet at this
    /// point in `compile_orchestration`'s own order), so this is
    /// reachable input, not just defensive code.
    fn register(
        &mut self,
        parent: usize,
        name: String,
        effect_type: String,
        text_producing: bool,
    ) -> usize {
        let idx = self.arena.len();
        self.arena.push(EffectNode {
            effect_type,
            text_producing,
            children: IndexMap::new(),
        });
        self.arena[parent].children.insert(name, idx);
        idx
    }

    fn child(&self, parent: usize, name: &str) -> Option<usize> {
        self.arena[parent].children.get(name).copied()
    }
}

/// The compile-time half of #406's composition checks against
/// *document* and its already-read *declared_prompts* -- `core/
/// prompt_compose.py::check_prompt_composition`.
///
/// *origin* resolves a `{file: ...}`-sourced `template`/`messages[].
/// content` the same best-effort way `_iter_composable_strings` does
/// (swallowing a [`CompileError`] rather than raising it -- a genuine
/// `file:` violation is [`crate::prompt_files::compile_declared_prompts`]/
/// the effect's own compile step's to raise, with the field name this
/// scan has lost).
pub(crate) fn check_prompt_composition(
    document: &Value,
    declared_prompts: &IndexMap<String, String>,
    origin: &DocumentOrigin,
) -> Result<(), CompileError> {
    let mut errors = Vec::new();

    errors.extend(declared_prompt_syntax_errors(declared_prompts));

    let all_names = all_effect_names(document);
    let mut overlap: Vec<&String> = declared_prompts
        .keys()
        .filter(|name| all_names.contains(name.as_str()))
        .collect();
    overlap.sort();
    for name in overlap {
        errors.push(format!(
            "'{name}' is both a declared prompt and an effect name — \
             '{{{{> {name}}}}}' would be ambiguous."
        ));
    }

    let (mut tree, root) = EffectTree::new();
    let ctx = WalkCtx {
        declared: declared_prompts,
        origin,
        root,
    };
    walk_effects(
        effects_or_steps(document),
        "effects",
        &mut tree,
        root,
        &[root],
        &ctx,
        &mut errors,
    );
    walk_effects(
        finally_list(document),
        "finally",
        &mut tree,
        root,
        &[root],
        &ctx,
        &mut errors,
    );

    for (prompt_name, text) in declared_prompts {
        for name in partial_references(text) {
            let where_ = format!("prompts.{prompt_name}");
            if !name_shape_matches(&name) {
                errors.push(format!("{where_}: '{{{{> {name}}}}}' is not a valid name."));
                continue;
            }
            check_name(
                &name,
                &where_,
                &tree,
                root,
                &[root],
                declared_prompts,
                &mut errors,
            );
        }
    }

    errors.extend(declared_prompt_cycles(declared_prompts));

    if errors.is_empty() {
        Ok(())
    } else {
        let body: String = errors.iter().map(|e| format!("\n  - {e}")).collect();
        Err(CompileError(format!("Prompt composition errors:{body}")))
    }
}

/// Each declared prompt validated on its own, before it is ever spliced
/// anywhere -- `core/prompt_compose.py::_declared_prompt_syntax_errors`.
fn declared_prompt_syntax_errors(declared: &IndexMap<String, String>) -> Vec<String> {
    let mut errors = Vec::new();
    for (name, text) in declared {
        match electricity_template::tokens(text) {
            Err(reason) => {
                errors.push(format!(
                    "prompts.{name}: malformed Mustache template: {reason}"
                ));
            }
            Ok(tokens) => {
                if tokens
                    .iter()
                    .any(|tag| matches!(tag, electricity_template::Tag::SetDelimiter(_)))
                {
                    errors.push(format!(
                        "prompts.{name}: '{{{{=...=}}}}' (set-delimiter) is not \
                         allowed inside a declared prompt — it would change the \
                         delimiters of the template that includes it."
                    ));
                }
            }
        }
    }
    errors
}

/// The exact scalar fields `{{> name}}` composition is checked in,
/// besides `messages[].content` (handled separately below) -- `core/
/// prompt_compose.py`'s `_COMPOSABLE_SCALAR_FIELDS`. Only `template`
/// may be `{file: ...}`-shaped among these (#396 §3); `prompt`/
/// `inline`/`params_json` are always plain strings, so a non-string
/// value there is simply skipped, exactly as Python's own
/// `elif field == "template":` guard does.
const COMPOSABLE_SCALAR_FIELDS: [&str; 4] = ["template", "prompt", "inline", "params_json"];

/// Every string in *effect* that `{{> name}}` composition actually
/// sees -- `core/prompt_compose.py::_iter_composable_strings`: the
/// scalar fields above, every message's `content`, every string
/// anywhere inside a tool's `params`, and -- only for a `use` effect --
/// every string anywhere inside `inputs`.
fn composable_strings(effect: &Value, origin: &DocumentOrigin) -> Vec<String> {
    let mut found = Vec::new();
    for field in COMPOSABLE_SCALAR_FIELDS {
        match dict_get(effect, field) {
            Some(Value::Str(text)) => found.push(text.clone()),
            Some(value) if field == "template" => {
                if let Ok(text) = resolve_text_or_file(value, "template", origin) {
                    found.push(text);
                }
            }
            _ => {}
        }
    }
    if let Some(Value::List(messages)) = dict_get(effect, "messages") {
        for message in messages {
            match dict_get(message, "content") {
                Some(Value::Str(text)) => found.push(text.clone()),
                Some(value) => {
                    if let Ok(text) = resolve_text_or_file(value, "messages[].content", origin) {
                        found.push(text);
                    }
                }
                None => {}
            }
        }
    }
    if let Some(params) = dict_get(effect, "params") {
        walk_strings(params, &mut found);
    }
    if effect_type_of(effect) == "use" {
        if let Some(inputs) = dict_get(effect, "inputs") {
            walk_strings(inputs, &mut found);
        }
    }
    found
}

/// Every string anywhere inside *value*, recursing through any nesting
/// of dicts/lists -- `core/prompt_compose.py::_walk_strings`.
fn walk_strings(value: &Value, found: &mut Vec<String>) {
    match value {
        Value::Str(s) => found.push(s.clone()),
        Value::Dict(dict) => {
            for v in dict.values() {
                walk_strings(v, found);
            }
        }
        Value::List(items) => {
            for v in items {
                walk_strings(v, found);
            }
        }
        _ => {}
    }
}

/// Checks one `{{> name}}` reference, recording any error into *errors*
/// -- `core/prompt_compose.py::check_name`. *bare_chain* is innermost
/// scope first: a bare lookup checks it from the end backward, then
/// falls back to *root* (document-wide bare visibility); a dotted
/// lookup always resolves from *root*, regardless of where the
/// reference itself sits.
fn check_name(
    name: &str,
    where_: &str,
    tree: &EffectTree,
    root: usize,
    bare_chain: &[usize],
    declared: &IndexMap<String, String>,
    errors: &mut Vec<String>,
) {
    let head = name.split('.').next().unwrap_or(name);
    if declared.contains_key(head) {
        if name.contains('.') {
            errors.push(format!(
                "{where_}: '{{{{> {name}}}}}' names declared prompt '{head}', \
                 which has no nested state — a declared prompt is plain text, \
                 never dotted."
            ));
        }
        return;
    }
    if !name.contains('.') {
        for scope in bare_chain.iter().rev() {
            if let Some(idx) = tree.child(*scope, head) {
                let node = &tree.arena[idx];
                if !node.text_producing {
                    errors.push(format!(
                        "{where_}: '{{{{> {name}}}}}' names effect '{head}' (type \
                         '{}'), which is neither a 'yield' nor a text 'prompt'.",
                        node.effect_type
                    ));
                }
                return;
            }
        }
        errors.push(format!(
            "{where_}: '{{{{> {name}}}}}' does not name a declared prompt or effect."
        ));
        return;
    }
    let Some(mut idx) = tree.child(root, head) else {
        errors.push(format!(
            "{where_}: '{{{{> {name}}}}}' does not name a declared prompt or effect."
        ));
        return;
    };
    let segments: Vec<&str> = name.split('.').collect();
    for seg in &segments[1..] {
        match tree.child(idx, seg) {
            Some(next) => idx = next,
            None => {
                if is_scope_introducing(&tree.arena[idx].effect_type) {
                    errors.push(format!(
                        "{where_}: '{{{{> {name}}}}}' does not name a declared \
                         prompt or effect."
                    ));
                }
                return;
            }
        }
    }
    let node = &tree.arena[idx];
    if !node.text_producing {
        errors.push(format!(
            "{where_}: '{{{{> {name}}}}}' names effect '{}' (type '{}'), which is \
             neither a 'yield' nor a text 'prompt'.",
            segments[segments.len() - 1],
            node.effect_type
        ));
    }
}

/// Bundles the values that stay constant across every recursive
/// [`walk_effects`] call, to keep its own argument count under
/// clippy's `too_many_arguments` limit. *root* is the tree's own root
/// index (always `0`, see [`EffectTree::new`]) -- carried here rather
/// than derived from `out` at each call site, since a dotted lookup
/// always resolves from the document root, never from the current
/// container (`core/prompt_compose.py::check_name`'s own "Dotted lookup
/// always resolves from `root`, regardless of where the reference
/// itself sits").
struct WalkCtx<'a> {
    declared: &'a IndexMap<String, String>,
    origin: &'a DocumentOrigin,
    root: usize,
}

/// `core/prompt_compose.py::check_prompt_composition`'s nested `walk`.
fn walk_effects(
    effects: &[Value],
    container_path: &str,
    tree: &mut EffectTree,
    out: usize,
    bare_chain: &[usize],
    ctx: &WalkCtx<'_>,
    errors: &mut Vec<String>,
) {
    // First pass: every sibling's own name is registered before any
    // reference at this level is checked, so a forward reference to a
    // later sibling resolves (Python's own two-pass `walk`).
    for effect in effects {
        if let Some(name) = effect_name_of(effect) {
            let effect_type = effect_type_of(effect);
            let text_producing = effect_is_text_producing(&effect_type, effect);
            tree.register(out, name.to_string(), effect_type, text_producing);
        }
    }

    for (idx, effect) in effects.iter().enumerate() {
        let effect_path = format!("{container_path}[{idx}]");
        for text in composable_strings(effect, ctx.origin) {
            for name in partial_references(&text) {
                if !name_shape_matches(&name) {
                    errors.push(format!(
                        "{effect_path}: '{{{{> {name}}}}}' is not a valid name."
                    ));
                    continue;
                }
                check_name(
                    &name,
                    &effect_path,
                    tree,
                    ctx.root,
                    bare_chain,
                    ctx.declared,
                    errors,
                );
            }
        }

        let name = effect_name_of(effect);
        let effect_type = effect_type_of(effect);
        let mut next_out = out;
        let mut next_bare_chain = bare_chain.to_vec();
        if let Some(name) = name {
            if is_scope_introducing(&effect_type) {
                let child_idx = tree.child(out, name).expect("registered above");
                next_out = child_idx;
                if effect_type == "if" {
                    next_bare_chain.push(child_idx);
                }
            }
        }
        for field in CHILD_LISTS {
            if let Some(Value::List(items)) = dict_get(effect, field) {
                walk_effects(
                    items,
                    &format!("{effect_path}.{field}"),
                    tree,
                    next_out,
                    &next_bare_chain,
                    ctx,
                    errors,
                );
            }
        }
    }
}

/// One error per cycle found among declared prompts' own `{{> name}}`
/// refs -- `core/prompt_compose.py::_declared_prompt_cycles`.
fn declared_prompt_cycles(declared: &IndexMap<String, String>) -> Vec<String> {
    let graph: IndexMap<&str, BTreeSet<String>> = declared
        .iter()
        .map(|(name, text)| {
            let refs: BTreeSet<String> = partial_references(text)
                .into_iter()
                .filter(|r| declared.contains_key(r.as_str()))
                .collect();
            (name.as_str(), refs)
        })
        .collect();

    let mut errors = Vec::new();
    let mut visited: BTreeSet<&str> = BTreeSet::new();

    let mut starts: Vec<&str> = graph.keys().copied().collect();
    starts.sort();
    for start in starts {
        if visited.contains(start) {
            continue;
        }
        let mut on_path: BTreeSet<&str> = BTreeSet::new();
        let mut path: Vec<&str> = Vec::new();
        if let Some(cycle_start) =
            dfs_find_cycle(start, &graph, &mut visited, &mut on_path, &mut path)
        {
            let idx = path.iter().position(|n| *n == cycle_start).unwrap_or(0);
            let mut cycle: Vec<&str> = path[idx..].to_vec();
            cycle.push(cycle_start);
            errors.push(format!(
                "cycle among declared prompts: {}",
                cycle.join(" -> ")
            ));
        }
    }
    errors
}

fn dfs_find_cycle<'a>(
    node: &'a str,
    graph: &IndexMap<&'a str, BTreeSet<String>>,
    visited: &mut BTreeSet<&'a str>,
    on_path: &mut BTreeSet<&'a str>,
    path: &mut Vec<&'a str>,
) -> Option<&'a str> {
    if on_path.contains(node) {
        return Some(node);
    }
    if visited.contains(node) {
        return None;
    }
    visited.insert(node);
    on_path.insert(node);
    path.push(node);
    if let Some(neighbors) = graph.get(node) {
        let mut sorted_neighbors: Vec<&str> = neighbors.iter().map(String::as_str).collect();
        sorted_neighbors.sort();
        for neighbor_text in sorted_neighbors {
            // `neighbor_text` borrows from `graph`'s `BTreeSet<String>`,
            // but every name in it is also a key of `declared`/`graph`
            // itself (filtered when the graph was built) -- look the
            // matching `&'a str` key back up so the recursive call's
            // lifetime is `'a`, not tied to this borrow of `graph`.
            let neighbor = *graph.keys().find(|k| **k == neighbor_text).unwrap();
            if let Some(cycle_at) = dfs_find_cycle(neighbor, graph, visited, on_path, path) {
                return Some(cycle_at);
            }
        }
    }
    path.pop();
    on_path.remove(node);
    None
}

#[cfg(test)]
mod tests {
    use super::{all_effect_names, check_prompt_composition};
    use crate::DocumentOrigin;
    use electricity_value::{Dict, Value};
    use indexmap::IndexMap;
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    fn origin() -> DocumentOrigin {
        DocumentOrigin::File {
            document_dir: PathBuf::from("/doc"),
            confinement_root: PathBuf::from("/doc"),
        }
    }

    fn names(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn effect_with(name: Value, children: &[(&str, Vec<Value>)]) -> Value {
        let mut dict = Dict::new();
        dict.insert(Value::Str("name".to_string()), name);
        for (field, items) in children {
            dict.insert(Value::Str((*field).to_string()), Value::List(items.clone()));
        }
        Value::Dict(dict)
    }

    fn effect(name: &str, etype: &str, extra: Vec<(&str, Value)>) -> Value {
        let mut dict = Dict::new();
        dict.insert(Value::Str("name".to_string()), Value::Str(name.to_string()));
        dict.insert(
            Value::Str("type".to_string()),
            Value::Str(etype.to_string()),
        );
        for (k, v) in extra {
            dict.insert(Value::Str(k.to_string()), v);
        }
        Value::Dict(dict)
    }

    fn document(effects: Vec<Value>) -> Value {
        let mut dict = Dict::new();
        dict.insert(Value::Str("effects".to_string()), Value::List(effects));
        Value::Dict(dict)
    }

    fn template_effect(name: &str, etype: &str, template: &str) -> Value {
        effect(
            name,
            etype,
            vec![("template", Value::Str(template.to_string()))],
        )
    }

    fn declared(pairs: &[(&str, &str)]) -> IndexMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn document_without_prompts_or_partials_passes_through() {
        let doc = document(vec![template_effect("a", "yield", "plain text")]);
        assert_eq!(
            check_prompt_composition(&doc, &IndexMap::new(), &origin()),
            Ok(())
        );
    }

    #[test]
    fn declared_prompt_referencing_an_unknown_name_fails() {
        let doc = document(vec![]);
        let decl = declared(&[("greeting", "hi {{> nope}}")]);
        let err = check_prompt_composition(&doc, &decl, &origin()).unwrap_err();
        assert_eq!(
            err.0,
            "Prompt composition errors:\n  - prompts.greeting: '{{> nope}}' does \
             not name a declared prompt or effect."
        );
    }

    #[test]
    fn yield_effect_is_a_valid_bare_reference() {
        let doc = document(vec![
            template_effect("hook", "yield", "hooked"),
            template_effect("body", "yield", "{{> hook}}"),
        ]);
        assert_eq!(
            check_prompt_composition(&doc, &IndexMap::new(), &origin()),
            Ok(())
        );
    }

    #[test]
    fn text_prompt_effect_is_a_valid_bare_reference() {
        let doc = document(vec![
            template_effect("hook", "prompt", "hooked"),
            template_effect("body", "yield", "{{> hook}}"),
        ]);
        assert_eq!(
            check_prompt_composition(&doc, &IndexMap::new(), &origin()),
            Ok(())
        );
    }

    #[test]
    fn non_text_effect_reference_fails() {
        let doc = document(vec![
            effect("t1", "tool", vec![]),
            template_effect("body", "yield", "{{> t1}}"),
        ]);
        let err = check_prompt_composition(&doc, &IndexMap::new(), &origin()).unwrap_err();
        assert_eq!(
            err.0,
            "Prompt composition errors:\n  - effects[1]: '{{> t1}}' names effect \
             't1' (type 'tool'), which is neither a 'yield' nor a text 'prompt'."
        );
    }

    #[test]
    fn json_prompt_is_not_text_producing() {
        let mut t1 = effect(
            "t1",
            "prompt",
            vec![
                ("template", Value::Str("x".to_string())),
                ("prompt_type", Value::Str("json".to_string())),
            ],
        );
        let _ = &mut t1;
        let doc = document(vec![t1, template_effect("body", "yield", "{{> t1}}")]);
        let err = check_prompt_composition(&doc, &IndexMap::new(), &origin()).unwrap_err();
        assert!(err.0.contains("neither a 'yield' nor a text 'prompt'"));
    }

    #[test]
    fn declared_prompt_and_effect_name_collide() {
        let doc = document(vec![template_effect("greeting", "yield", "x")]);
        let decl = declared(&[("greeting", "hi")]);
        let err = check_prompt_composition(&doc, &decl, &origin()).unwrap_err();
        assert_eq!(
            err.0,
            "Prompt composition errors:\n  - 'greeting' is both a declared prompt \
             and an effect name — '{{> greeting}}' would be ambiguous."
        );
    }

    #[test]
    fn cycle_among_declared_prompts_is_reported() {
        let decl = declared(&[("a", "{{> b}}"), ("b", "{{> a}}")]);
        let doc = document(vec![]);
        let err = check_prompt_composition(&doc, &decl, &origin()).unwrap_err();
        assert_eq!(
            err.0,
            "Prompt composition errors:\n  - cycle among declared prompts: a -> b -> a"
        );
    }

    #[test]
    fn invalid_partial_name_is_rejected() {
        let doc = document(vec![template_effect("a", "yield", "{{> 1bad}}")]);
        let err = check_prompt_composition(&doc, &IndexMap::new(), &origin()).unwrap_err();
        assert_eq!(
            err.0,
            "Prompt composition errors:\n  - effects[0]: '{{> 1bad}}' is not a valid name."
        );
    }

    #[test]
    fn set_delimiter_inside_a_declared_prompt_is_rejected() {
        let decl = declared(&[("a", "{{=<% %>=}}")]);
        let doc = document(vec![]);
        let err = check_prompt_composition(&doc, &decl, &origin()).unwrap_err();
        assert_eq!(
            err.0,
            "Prompt composition errors:\n  - prompts.a: '{{=...=}}' (set-delimiter) \
             is not allowed inside a declared prompt — it would change the \
             delimiters of the template that includes it."
        );
    }

    #[test]
    fn malformed_declared_prompt_is_reported_as_its_own_error() {
        let decl = declared(&[("a", "{{#open}}")]);
        let doc = document(vec![]);
        let err = check_prompt_composition(&doc, &decl, &origin()).unwrap_err();
        assert!(
            err.0.contains("prompts.a: malformed Mustache template:"),
            "unexpected error: {}",
            err.0
        );
    }

    #[test]
    fn bare_reference_does_not_cross_a_named_if_boundary() {
        let then_branch = vec![template_effect("inner", "yield", "hi")];
        let mut branch = effect("branch", "if", vec![]);
        branch
            .as_dict_mut()
            .unwrap()
            .insert(Value::Str("then".to_string()), Value::List(then_branch));
        let doc = document(vec![
            branch,
            template_effect("outer", "yield", "{{> inner}}"),
        ]);
        let err = check_prompt_composition(&doc, &IndexMap::new(), &origin()).unwrap_err();
        assert_eq!(
            err.0,
            "Prompt composition errors:\n  - effects[1]: '{{> inner}}' does not \
             name a declared prompt or effect."
        );
    }

    #[test]
    fn dotted_reference_reaches_into_a_named_if_from_anywhere() {
        let then_branch = vec![template_effect("inner", "yield", "hi")];
        let mut branch = effect("branch", "if", vec![]);
        branch
            .as_dict_mut()
            .unwrap()
            .insert(Value::Str("then".to_string()), Value::List(then_branch));
        let doc = document(vec![
            branch,
            template_effect("outer", "yield", "{{> branch.inner}}"),
        ]);
        assert_eq!(
            check_prompt_composition(&doc, &IndexMap::new(), &origin()),
            Ok(())
        );
    }

    #[test]
    fn a_sibling_inside_the_same_named_if_branch_can_bare_reference_another() {
        let then_branch = vec![
            template_effect("inner", "yield", "hi"),
            template_effect("inner2", "yield", "{{> inner}}"),
        ];
        let mut branch = effect("branch", "if", vec![]);
        branch
            .as_dict_mut()
            .unwrap()
            .insert(Value::Str("then".to_string()), Value::List(then_branch));
        let doc = document(vec![branch]);
        assert_eq!(
            check_prompt_composition(&doc, &IndexMap::new(), &origin()),
            Ok(())
        );
    }

    #[test]
    fn a_named_loops_body_stays_bare_visible_outside_it() {
        let body = vec![template_effect("inner", "yield", "hi")];
        let mut loop_effect = effect("myloop", "loop", vec![]);
        loop_effect
            .as_dict_mut()
            .unwrap()
            .insert(Value::Str("body".to_string()), Value::List(body));
        let doc = document(vec![
            loop_effect,
            template_effect("outer", "yield", "{{> inner}}"),
        ]);
        assert_eq!(
            check_prompt_composition(&doc, &IndexMap::new(), &origin()),
            Ok(())
        );
    }

    #[test]
    fn a_missing_dotted_segment_under_a_named_if_is_an_unknown_name() {
        let then_branch = vec![template_effect("inner", "yield", "hi")];
        let mut branch = effect("branch", "if", vec![]);
        branch
            .as_dict_mut()
            .unwrap()
            .insert(Value::Str("then".to_string()), Value::List(then_branch));
        let doc = document(vec![
            branch,
            template_effect("outer", "yield", "{{> branch.missing}}"),
        ]);
        let err = check_prompt_composition(&doc, &IndexMap::new(), &origin()).unwrap_err();
        assert_eq!(
            err.0,
            "Prompt composition errors:\n  - effects[1]: '{{> branch.missing}}' \
             does not name a declared prompt or effect."
        );
    }

    #[test]
    fn a_dotted_reference_past_an_untracked_container_is_trusted() {
        let body = vec![template_effect("inner", "yield", "hi")];
        let mut loop_effect = effect("myloop", "loop", vec![]);
        loop_effect
            .as_dict_mut()
            .unwrap()
            .insert(Value::Str("body".to_string()), Value::List(body));
        let doc = document(vec![
            loop_effect,
            template_effect("outer", "yield", "{{> myloop.whatever}}"),
        ]);
        assert_eq!(
            check_prompt_composition(&doc, &IndexMap::new(), &origin()),
            Ok(())
        );
    }

    #[test]
    fn forward_reference_to_a_later_sibling_resolves() {
        let doc = document(vec![
            template_effect("first", "yield", "{{> second}}"),
            template_effect("second", "yield", "hi"),
        ]);
        assert_eq!(
            check_prompt_composition(&doc, &IndexMap::new(), &origin()),
            Ok(())
        );
    }

    #[test]
    fn declared_prompt_wins_over_a_same_named_effect_without_double_reporting() {
        // The collision is already reported once by the overlap check;
        // `check_name` must not also report a "does not name" error for
        // the same reference.
        let doc = document(vec![template_effect("greeting", "yield", "x")]);
        let decl = declared(&[("greeting", "hi")]);
        let referencing = document(vec![template_effect("ref", "yield", "{{> greeting}}")]);
        let mut combined_effects = match &referencing {
            Value::Dict(d) => match d.get(&Value::Str("effects".to_string())).unwrap() {
                Value::List(items) => items.clone(),
                _ => unreachable!(),
            },
            _ => unreachable!(),
        };
        combined_effects.extend(match &doc {
            Value::Dict(d) => match d.get(&Value::Str("effects".to_string())).unwrap() {
                Value::List(items) => items.clone(),
                _ => unreachable!(),
            },
            _ => unreachable!(),
        });
        let combined = document(combined_effects);
        let err = check_prompt_composition(&combined, &decl, &origin()).unwrap_err();
        assert_eq!(
            err.0,
            "Prompt composition errors:\n  - 'greeting' is both a declared prompt \
             and an effect name — '{{> greeting}}' would be ambiguous."
        );
    }

    #[test]
    fn dotted_reference_to_a_declared_prompt_is_rejected() {
        let doc = document(vec![template_effect("ref", "yield", "{{> greeting.x}}")]);
        let decl = declared(&[("greeting", "hi")]);
        let err = check_prompt_composition(&doc, &decl, &origin()).unwrap_err();
        assert_eq!(
            err.0,
            "Prompt composition errors:\n  - effects[0]: '{{> greeting.x}}' names \
             declared prompt 'greeting', which has no nested state — a declared \
             prompt is plain text, never dotted."
        );
    }

    #[test]
    fn collects_names_nested_under_if_loop_dynamic_and_finally() {
        let a = effect_with(
            Value::Str("a".to_string()),
            &[
                ("then", vec![effect_with(Value::Str("b".to_string()), &[])]),
                ("else", vec![effect_with(Value::Str("c".to_string()), &[])]),
                ("body", vec![effect_with(Value::Str("d".to_string()), &[])]),
                (
                    "finally",
                    vec![effect_with(Value::Str("e".to_string()), &[])],
                ),
            ],
        );
        let mut doc = Dict::new();
        doc.insert(Value::Str("effects".to_string()), Value::List(vec![a]));
        doc.insert(
            Value::Str("finally".to_string()),
            Value::List(vec![effect_with(Value::Str("f".to_string()), &[])]),
        );

        let result = all_effect_names(&Value::Dict(doc));

        assert_eq!(result, names(&["a", "b", "c", "d", "e", "f"]));
    }

    #[test]
    fn non_string_and_empty_names_are_skipped() {
        let mut doc = Dict::new();
        doc.insert(
            Value::Str("effects".to_string()),
            Value::List(vec![
                effect_with(Value::Int(123.into()), &[]),
                effect_with(Value::Str(String::new()), &[]),
                effect_with(Value::Str("kept".to_string()), &[]),
            ]),
        );

        let result = all_effect_names(&Value::Dict(doc));

        assert_eq!(result, names(&["kept"]));
    }

    #[test]
    fn an_empty_effects_list_falls_back_to_steps() {
        let mut doc = Dict::new();
        doc.insert(Value::Str("effects".to_string()), Value::List(Vec::new()));
        doc.insert(
            Value::Str("steps".to_string()),
            Value::List(vec![effect_with(Value::Str("s".to_string()), &[])]),
        );

        let result = all_effect_names(&Value::Dict(doc));

        assert_eq!(result, names(&["s"]));
    }

    #[test]
    fn dotted_self_reference_from_inside_the_same_named_if_resolves() {
        // Regression for the dotted-lookup bug: `check_name`'s dotted
        // branch must resolve from the document root, not from the
        // current container -- otherwise `branch.inner`, referenced
        // from a sibling inside `branch` itself, wrongly reports
        // "does not name a declared prompt or effect".
        let then_branch = vec![
            template_effect("inner", "yield", "hi"),
            template_effect("inner2", "yield", "{{> branch.inner}}"),
        ];
        let mut branch = effect("branch", "if", vec![]);
        branch
            .as_dict_mut()
            .unwrap()
            .insert(Value::Str("then".to_string()), Value::List(then_branch));
        let doc = document(vec![branch]);
        assert_eq!(
            check_prompt_composition(&doc, &IndexMap::new(), &origin()),
            Ok(())
        );
    }

    #[test]
    fn dotted_self_reference_from_inside_a_named_dynamic_resolves() {
        let body = vec![
            template_effect("inner", "yield", "hi"),
            template_effect("inner2", "yield", "{{> loop_ns.inner}}"),
        ];
        let mut dynamic_effect = effect("loop_ns", "dynamic", vec![]);
        dynamic_effect
            .as_dict_mut()
            .unwrap()
            .insert(Value::Str("effects".to_string()), Value::List(body));
        let doc = document(vec![dynamic_effect]);
        assert_eq!(
            check_prompt_composition(&doc, &IndexMap::new(), &origin()),
            Ok(())
        );
    }

    #[test]
    fn dotted_reference_inside_a_named_if_to_a_non_root_name_is_rejected() {
        // The reverse direction of the same bug: a dotted name must
        // resolve from the root even when it happens to match a name
        // nested only inside the current container -- `inner` here is
        // `branch`'s own child, not a document-root name, so
        // `{{> inner.x}}` must be rejected even from right beside it.
        let then_branch = vec![
            template_effect("inner", "yield", "hi"),
            template_effect("inner2", "yield", "{{> inner.x}}"),
        ];
        let mut branch = effect("branch", "if", vec![]);
        branch
            .as_dict_mut()
            .unwrap()
            .insert(Value::Str("then".to_string()), Value::List(then_branch));
        let doc = document(vec![branch]);
        let err = check_prompt_composition(&doc, &IndexMap::new(), &origin()).unwrap_err();
        assert_eq!(
            err.0,
            "Prompt composition errors:\n  - effects[0].then[1]: '{{> inner.x}}' \
             does not name a declared prompt or effect."
        );
    }

    #[test]
    fn tool_params_partial_reference_is_checked() {
        let mut t1 = effect("t1", "tool", vec![]);
        let mut params = Dict::new();
        params.insert(
            Value::Str("command".to_string()),
            Value::Str("echo {{> nope}}".to_string()),
        );
        t1.as_dict_mut()
            .unwrap()
            .insert(Value::Str("params".to_string()), Value::Dict(params));
        let doc = document(vec![t1]);
        let err = check_prompt_composition(&doc, &IndexMap::new(), &origin()).unwrap_err();
        assert_eq!(
            err.0,
            "Prompt composition errors:\n  - effects[0]: '{{> nope}}' does not \
             name a declared prompt or effect."
        );
    }

    #[test]
    fn use_inputs_partial_reference_is_checked() {
        let mut u1 = effect("u1", "use", vec![]);
        let mut inputs = Dict::new();
        inputs.insert(
            Value::Str("x".to_string()),
            Value::Str("{{> nope}}".to_string()),
        );
        u1.as_dict_mut()
            .unwrap()
            .insert(Value::Str("inputs".to_string()), Value::Dict(inputs));
        let doc = document(vec![u1]);
        let err = check_prompt_composition(&doc, &IndexMap::new(), &origin()).unwrap_err();
        assert_eq!(
            err.0,
            "Prompt composition errors:\n  - effects[0]: '{{> nope}}' does not \
             name a declared prompt or effect."
        );
    }

    #[test]
    fn non_use_effects_inputs_field_is_not_composable() {
        // `inputs` on a prompt/yield effect is a prompt-local value
        // merged into context as-is, never a template -- only a `use`
        // effect's own `inputs` are rendered/scanned.
        let mut p1 = template_effect("p1", "yield", "hi");
        let mut inputs = Dict::new();
        inputs.insert(
            Value::Str("x".to_string()),
            Value::Str("{{> nope}}".to_string()),
        );
        p1.as_dict_mut()
            .unwrap()
            .insert(Value::Str("inputs".to_string()), Value::Dict(inputs));
        let doc = document(vec![p1]);
        assert_eq!(
            check_prompt_composition(&doc, &IndexMap::new(), &origin()),
            Ok(())
        );
    }

    #[test]
    fn prompt_scalar_field_partial_reference_is_checked() {
        let mut p1 = effect("p1", "reflector", vec![]);
        p1.as_dict_mut().unwrap().insert(
            Value::Str("prompt".to_string()),
            Value::Str("hi {{> nope}}".to_string()),
        );
        let doc = document(vec![p1]);
        let err = check_prompt_composition(&doc, &IndexMap::new(), &origin()).unwrap_err();
        assert_eq!(
            err.0,
            "Prompt composition errors:\n  - effects[0]: '{{> nope}}' does not \
             name a declared prompt or effect."
        );
    }

    #[test]
    fn a_repeated_partial_reference_in_one_text_is_reported_once() {
        let doc = document(vec![template_effect("a", "yield", "{{> nope}} {{> nope}}")]);
        let err = check_prompt_composition(&doc, &IndexMap::new(), &origin()).unwrap_err();
        assert_eq!(
            err.0,
            "Prompt composition errors:\n  - effects[0]: '{{> nope}}' does not \
             name a declared prompt or effect."
        );
    }
}
