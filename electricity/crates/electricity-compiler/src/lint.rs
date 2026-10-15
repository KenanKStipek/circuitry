//! Issue #428: `lint_warnings`, porting `core/lint.py::lint_orchestration` in
//! full -- deprecated effect-type/flow aliases, an effect named after its
//! own type, a tool's `params.args` entries YAML turned into a non-string
//! scalar, `min_iterations` on an `each` loop, `threshold:` on a built-in
//! `if`, and the loop-body reference footguns (`iter_<N>`, `last`,
//! `prime.<loop>.<body_step>`). Advisory only: nothing here ever turns a
//! valid document invalid, and every message is Circuitry's own, word for
//! word.

use electricity_value::pycompat::py_strip;
use electricity_value::{Dict, Value};
use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;

/// `core/lint.py::CANONICAL_EFFECT_TYPES`.
const CANONICAL_EFFECT_TYPES: [&str; 8] = [
    "prompt",
    "dynamic",
    "if",
    "loop",
    "reflector",
    "tool",
    "use",
    "yield",
];

/// `core/lint.py::DEPRECATED_EFFECT_TYPE_ALIASES`.
const DEPRECATED_EFFECT_TYPE_ALIASES: [(&str, &str); 1] = [("conditional", "if")];

/// `core/lint.py::DEPRECATED_FLOW_ALIASES`.
const DEPRECATED_FLOW_ALIASES: [(&str, &str); 4] = [
    ("chain_of_thought", "chain"),
    ("cot", "chain"),
    ("tree_of_thought", "tree"),
    ("tot", "tree"),
];

/// `core/lint.py::TYPE_KEYWORDS`: `CANONICAL_EFFECT_TYPES` plus every
/// deprecated effect-type alias.
fn is_type_keyword(name: &str) -> bool {
    CANONICAL_EFFECT_TYPES.contains(&name)
        || DEPRECATED_EFFECT_TYPE_ALIASES
            .iter()
            .any(|(alias, _)| *alias == name)
}

/// `core/lint.py::_CHILD_KEYS` -- deliberately narrower than
/// `structural.rs`'s own `CHILD_KEYS`: a legacy top-level `steps:` is
/// handled once, by [`lint_effects_or_steps`], not walked as a nested
/// child key too.
const CHILD_KEYS: [&str; 5] = ["effects", "then", "else", "body", "finally"];

/// `core/lint.py::_ITER_SEGMENT`: a fixed-iteration path segment, e.g.
/// `iter_0`. `\d` (not `[0-9]`) to match Python's own Unicode digit
/// class -- `prime.outer.iter_\u0661` (an Arabic-Indic digit) warns in
/// cof too.
static ITER_SEGMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^iter_\d+$").expect("static regex"));

fn dict_get<'a>(dict: &'a Dict, key: &str) -> Option<&'a Value> {
    dict.get(&Value::Str(key.to_string()))
}

/// A loop whose iteration is in progress at the point being linted --
/// `core/lint.py::_LoopScope`.
#[derive(Clone)]
struct LoopScope {
    name: Option<String>,
    body_names: HashSet<String>,
}

/// `core/lint.py::lint_orchestration`.
pub fn lint_warnings(document: &Value) -> Vec<String> {
    let mut warnings = Vec::new();
    let Some(dict) = document.as_dict() else {
        return warnings;
    };

    check_flow(dict_get(dict, "flow"), "top level", &mut warnings);

    let effects = lint_effects_or_steps(dict);
    walk(effects, "effects", &mut warnings, &[]);
    walk(dict_get(dict, "finally"), "finally", &mut warnings, &[]);

    warnings
}

/// `orch.get("effects")`, falling back to `orch.get("steps")` only when
/// `effects` isn't itself a list -- `core/lint.py`'s own `if not
/// isinstance(effects, Sequence) or isinstance(effects, (str, bytes)):
/// effects = orch.get("steps")`. Unlike `structural.rs`'s
/// `effects_or_steps` (which falls back only on `None`/absent), this
/// also falls back when `effects:` is present but not a list at all
/// (a mapping, a string, a number) -- a real, deliberate divergence
/// between the two modules that this port preserves exactly.
fn lint_effects_or_steps(dict: &Dict) -> Option<&Value> {
    match dict_get(dict, "effects") {
        Some(value @ Value::List(_)) => Some(value),
        _ => dict_get(dict, "steps"),
    }
}

fn is_loop(effect: &Dict) -> bool {
    matches!(dict_get(effect, "type"), Some(Value::Str(s)) if py_strip(s).to_lowercase() == "loop")
}

/// `core/lint.py::_loop_scope`.
fn loop_scope(effect: &Dict) -> LoopScope {
    let mut body_names = HashSet::new();
    if let Some(Value::List(body)) = dict_get(effect, "body") {
        for child in body {
            if let Some(child_dict) = child.as_dict() {
                if let Some(Value::Str(name)) = dict_get(child_dict, "name") {
                    if !name.is_empty() {
                        body_names.insert(name.clone());
                    }
                }
            }
        }
    }
    let name = match dict_get(effect, "name") {
        Some(Value::Str(name)) if !name.is_empty() => Some(name.clone()),
        _ => None,
    };
    LoopScope { name, body_names }
}

fn walk(effects: Option<&Value>, path: &str, warnings: &mut Vec<String>, loops: &[LoopScope]) {
    let Some(Value::List(items)) = effects else {
        return;
    };
    for (index, effect) in items.iter().enumerate() {
        let Some(effect_dict) = effect.as_dict() else {
            continue;
        };
        let here = format!("{path}[{index}]");

        let inner_owned: Vec<LoopScope>;
        let inner: &[LoopScope] = if is_loop(effect_dict) {
            let mut scopes = loops.to_vec();
            scopes.push(loop_scope(effect_dict));
            inner_owned = scopes;
            &inner_owned
        } else {
            loops
        };

        check_effect(effect_dict, &here, warnings, inner);
        for key in CHILD_KEYS {
            walk(
                dict_get(effect_dict, key),
                &format!("{here}.{key}"),
                warnings,
                inner,
            );
        }
    }
}

/// `core/lint.py::_reference_strings`: every string an effect renders or
/// evaluates, minus its child effect lists.
fn reference_strings(effect: &Dict) -> Vec<String> {
    fn collect(value: &Value, found: &mut Vec<String>) {
        match value {
            Value::Str(s) => found.push(s.clone()),
            Value::Dict(d) => {
                for v in d.values() {
                    collect(v, found);
                }
            }
            Value::List(items) => {
                for v in items {
                    collect(v, found);
                }
            }
            _ => {}
        }
    }

    let mut found = Vec::new();
    for (key, value) in effect {
        if let Some(key_str) = key.as_str() {
            if CHILD_KEYS.contains(&key_str) {
                continue;
            }
        }
        collect(value, &mut found);
    }
    found
}

/// One `prime.<name>.<segment>` reference, found by a literal scan for
/// `prime.<name>.` with a hand-checked `\b` before it, then the longest
/// run of `electricity_pyre::is_py_word_char` (first character
/// restricted to `[A-Za-z_]`) after it -- `core/lint.py::
/// _check_loop_references`'s own pattern. A literal scan rather than a
/// regex compiled per loop name for two reasons: a loop name of a few
/// hundred KB can exceed the `regex` crate's default compiled-size
/// limit, and `is_py_word_char` is CPython 3.11's own `\w` membership
/// table, not Rust's wider Unicode word class (which also counts
/// combining marks). Non-overlapping, left to right, like
/// `re.finditer`: a successful match resumes the scan after it; a
/// failed one (no boundary, or no valid segment) resumes one character
/// later.
fn find_scope_references<'t>(text: &'t str, name: &str) -> Vec<&'t str> {
    let prefix = format!("prime.{name}.");
    let mut found = Vec::new();
    let mut search_from = 0usize;
    while let Some(offset) = text[search_from..].find(&prefix) {
        let start = search_from + offset;
        let boundary_ok = match text[..start].chars().next_back() {
            Some(prev) => !electricity_pyre::is_py_word_char(prev),
            None => true,
        };
        if !boundary_ok {
            search_from = start + 1;
            continue;
        }
        let after_prefix = start + prefix.len();
        let mut rest = text[after_prefix..].chars();
        let Some(first) = rest.next() else {
            search_from = start + 1;
            continue;
        };
        if !(first.is_ascii_alphabetic() || first == '_') {
            search_from = start + 1;
            continue;
        }
        let mut end = after_prefix + first.len_utf8();
        for ch in rest {
            if !electricity_pyre::is_py_word_char(ch) {
                break;
            }
            end += ch.len_utf8();
        }
        found.push(&text[after_prefix..end]);
        search_from = end;
    }
    found
}

/// `core/lint.py::_check_loop_references`.
fn check_loop_references(
    effect: &Dict,
    where_: &str,
    warnings: &mut Vec<String>,
    loops: &[LoopScope],
) {
    let named: Vec<&LoopScope> = loops.iter().filter(|scope| scope.name.is_some()).collect();
    if named.is_empty() {
        return;
    }

    let mut seen: HashSet<String> = HashSet::new();
    for text in reference_strings(effect) {
        for scope in &named {
            let name = scope.name.as_ref().expect("filtered to named scopes");
            let body_names = &scope.body_names;
            for segment in find_scope_references(&text, name) {
                let key = format!("{name}.{segment}");
                if seen.contains(&key) {
                    continue;
                }

                if ITER_SEGMENT.is_match(segment) {
                    seen.insert(key);
                    let pass = segment.strip_prefix("iter_").unwrap_or(segment);
                    warnings.push(format!(
                        "{where_}: 'prime.{name}.{segment}' names a fixed pass of the \
                         loop it sits inside. It does not fail — it resolves to pass \
                         {pass}'s output in every iteration, so every later pass reads \
                         stale data. Use '{{{{prime.<step>.value}}}}' for the current \
                         pass; iter_<N> is only meaningful outside the loop."
                    ));
                } else if segment == "last" {
                    seen.insert(key);
                    warnings.push(format!(
                        "{where_}: 'prime.{name}.last' inside the loop it names resolves \
                         to the previous pass at best — 'last' is written when the loop \
                         completes. Use '{{{{prime.<step>.value}}}}' for the current pass; \
                         'prime.{name}.last.<step>.value' is only meaningful after the loop."
                    ));
                } else if body_names.contains(segment) {
                    seen.insert(key);
                    warnings.push(format!(
                        "{where_}: 'prime.{name}.{segment}' never resolves — 'prime.{name}' \
                         is the loop's own node (iter_<N>, last, collected, meta), not a \
                         scope its body steps live in. Use '{{{{prime.{segment}.value}}}}' \
                         for the current pass, or 'prime.{name}.collected.value' after the \
                         loop."
                    ));
                }
            }
        }
    }
}

/// `core/lint.py::_check_tool_args`.
fn check_tool_args(params: Option<&Value>, where_: &str, warnings: &mut Vec<String>) {
    let Some(params_dict) = params.and_then(Value::as_dict) else {
        return;
    };
    let Some(Value::List(args)) = dict_get(params_dict, "args") else {
        return;
    };
    for (index, arg) in args.iter().enumerate() {
        if !matches!(
            arg,
            Value::None | Value::Bool(_) | Value::Int(_) | Value::Float(_)
        ) {
            continue;
        }
        let kind = if arg.is_none() {
            "null"
        } else {
            arg.type_name()
        };
        warnings.push(format!(
            "{where_}: params.args[{index}] is not a string — YAML read it as the {kind} \
             {}, so the tool receives the text '{}'. Quote it to pass exactly what you \
             wrote (unquoted, YAML turns 0x1 into 1, off into false and -0 into 0).",
            arg.py_repr(),
            arg.py_str()
        ));
    }
}

/// `core/lint.py::_check_flow`.
fn check_flow(flow: Option<&Value>, where_: &str, warnings: &mut Vec<String>) {
    let Some(Value::Str(flow)) = flow else {
        return;
    };
    let normalized = py_strip(flow).to_lowercase();
    if let Some((_, canonical)) = DEPRECATED_FLOW_ALIASES
        .iter()
        .find(|(alias, _)| *alias == normalized)
    {
        warnings.push(format!(
            "{where_}: flow '{flow}' is a deprecated alias — write 'flow: {canonical}'. \
             Both parse; only '{canonical}' is documented."
        ));
    }
}

/// `core/lint.py::_check_effect`.
fn check_effect(effect: &Dict, where_: &str, warnings: &mut Vec<String>, loops: &[LoopScope]) {
    let effect_type = match dict_get(effect, "type") {
        Some(Value::Str(s)) => py_strip(s).to_lowercase(),
        _ => String::new(),
    };

    if let Some((_, canonical)) = DEPRECATED_EFFECT_TYPE_ALIASES
        .iter()
        .find(|(alias, _)| *alias == effect_type)
    {
        warnings.push(format!(
            "{where_}: type '{effect_type}' is a deprecated alias — write 'type: \
             {canonical}'. Both parse; only '{canonical}' is documented."
        ));
    }

    check_flow(dict_get(effect, "flow"), where_, warnings);

    if let Some(Value::Str(name)) = dict_get(effect, "name") {
        if is_type_keyword(&py_strip(name).to_lowercase()) {
            warnings.push(format!(
                "{where_}: effect is named '{name}', which is an effect-type keyword. \
                 Name effects after the job they do (e.g. 'summarize_article'), not \
                 after their type — generic names collide when two of them end up \
                 siblings in the same scope."
            ));
        }
    }

    check_loop_references(effect, where_, warnings, loops);

    if effect_type == "tool" {
        check_tool_args(dict_get(effect, "params"), where_, warnings);
    }

    if effect_type == "loop"
        && effect.contains_key(&Value::Str("min_iterations".to_string()))
        && matches!(dict_get(effect, "each"), Some(Value::Dict(_)))
    {
        warnings.push(format!(
            "{where_}: 'min_iterations' has no effect on an 'each' loop; it only \
             forces extra passes before a 'while' loop's condition is checked. An \
             'each' loop always runs once per collection element."
        ));
    }

    if (effect_type == "if" || effect_type == "conditional")
        && effect.contains_key(&Value::Str("threshold".to_string()))
    {
        warnings.push(format!(
            "{where_}: 'threshold' has no effect — the built-in evaluator reads a \
             categorical yes/no with no calibrated confidence to cut, so it is \
             recorded on meta.threshold and never consulted. Deprecated: kept \
             schema-valid only so existing documents that set it keep passing; new \
             documents should not set it."
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc_with_effects(effects: Vec<Value>) -> Value {
        let mut dict = Dict::new();
        dict.insert(Value::Str("effects".to_string()), Value::List(effects));
        Value::Dict(dict)
    }

    fn effect(pairs: Vec<(&str, Value)>) -> Value {
        let mut dict = Dict::new();
        for (k, v) in pairs {
            dict.insert(Value::Str(k.to_string()), v);
        }
        Value::Dict(dict)
    }

    fn str_(s: &str) -> Value {
        Value::Str(s.to_string())
    }

    fn int(n: i64) -> Value {
        Value::from(n)
    }

    #[test]
    fn deprecated_effect_type_alias_is_warned() {
        let doc = doc_with_effects(vec![effect(vec![
            ("type", str_("conditional")),
            ("name", str_("c")),
        ])]);
        let warnings = lint_warnings(&doc);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("type 'conditional' is a deprecated alias"));
        assert!(warnings[0].contains("write 'type: if'"));
    }

    #[test]
    fn deprecated_flow_alias_at_top_level_is_warned() {
        let mut dict = Dict::new();
        dict.insert(Value::Str("flow".to_string()), str_("cot"));
        dict.insert(Value::Str("effects".to_string()), Value::List(vec![]));
        let warnings = lint_warnings(&Value::Dict(dict));
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].starts_with("top level: flow 'cot' is a deprecated alias"));
        assert!(warnings[0].contains("write 'flow: chain'"));
    }

    #[test]
    fn deprecated_flow_alias_on_an_effect_is_warned() {
        let doc = doc_with_effects(vec![effect(vec![
            ("type", str_("loop")),
            ("name", str_("poll")),
            ("flow", str_("tot")),
        ])]);
        let warnings = lint_warnings(&doc);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].starts_with("effects[0]: flow 'tot' is a deprecated alias"));
        assert!(warnings[0].contains("write 'flow: tree'"));
    }

    #[test]
    fn effect_named_after_its_own_type_is_warned() {
        let doc = doc_with_effects(vec![effect(vec![
            ("type", str_("tool")),
            ("name", str_("tool")),
        ])]);
        let warnings = lint_warnings(&doc);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("effect is named 'tool', which is an effect-type keyword"));
    }

    #[test]
    fn tool_args_yaml_scalar_gotchas_are_warned_with_what_the_tool_receives() {
        let mut params = Dict::new();
        params.insert(
            Value::Str("args".to_string()),
            Value::List(vec![int(1), Value::Bool(false), Value::None]),
        );
        let doc = doc_with_effects(vec![effect(vec![
            ("type", str_("tool")),
            ("name", str_("t")),
            ("params", Value::Dict(params)),
        ])]);
        let warnings = lint_warnings(&doc);
        assert_eq!(warnings.len(), 3);
        assert!(warnings[0].starts_with(
            "effects[0]: params.args[0] is not a string — YAML read it as the int 1, \
             so the tool receives the text '1'."
        ));
        assert!(warnings[1].contains("as the bool False, so the tool receives the text 'False'"));
        assert!(warnings[2].contains("as the null None, so the tool receives the text 'None'"));
    }

    #[test]
    fn string_tool_args_are_not_warned() {
        let mut params = Dict::new();
        params.insert(
            Value::Str("args".to_string()),
            Value::List(vec![str_("-p"), str_("0x1")]),
        );
        let doc = doc_with_effects(vec![effect(vec![
            ("type", str_("tool")),
            ("name", str_("t")),
            ("params", Value::Dict(params)),
        ])]);
        assert_eq!(lint_warnings(&doc), Vec::<String>::new());
    }

    #[test]
    fn min_iterations_on_each_loop_is_warned() {
        let mut each = Dict::new();
        each.insert(Value::Str("in".to_string()), str_("input.items"));
        let doc = doc_with_effects(vec![effect(vec![
            ("type", str_("loop")),
            ("name", str_("l")),
            ("each", Value::Dict(each)),
            ("min_iterations", int(5)),
        ])]);
        let warnings = lint_warnings(&doc);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("'min_iterations' has no effect on an 'each' loop"));
    }

    #[test]
    fn min_iterations_on_while_loop_is_not_warned() {
        let mut while_ = Dict::new();
        while_.insert(Value::Str("mode".to_string()), str_("cel"));
        while_.insert(Value::Str("expr".to_string()), str_("true"));
        let doc = doc_with_effects(vec![effect(vec![
            ("type", str_("loop")),
            ("name", str_("l")),
            ("while", Value::Dict(while_)),
            ("min_iterations", int(1)),
        ])]);
        assert_eq!(lint_warnings(&doc), Vec::<String>::new());
    }

    #[test]
    fn if_threshold_is_always_warned() {
        let mut if_ = Dict::new();
        if_.insert(Value::Str("mode".to_string()), str_("cel"));
        if_.insert(Value::Str("expr".to_string()), str_("true"));
        let doc = doc_with_effects(vec![effect(vec![
            ("type", str_("if")),
            ("name", str_("c")),
            ("if", Value::Dict(if_)),
            ("threshold", Value::Float(0.7)),
        ])]);
        let warnings = lint_warnings(&doc);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("'threshold' has no effect"));
    }

    #[test]
    fn if_without_threshold_is_not_warned() {
        let mut if_ = Dict::new();
        if_.insert(Value::Str("mode".to_string()), str_("cel"));
        if_.insert(Value::Str("expr".to_string()), str_("true"));
        let doc = doc_with_effects(vec![effect(vec![
            ("type", str_("if")),
            ("name", str_("c")),
            ("if", Value::Dict(if_)),
        ])]);
        assert_eq!(lint_warnings(&doc), Vec::<String>::new());
    }

    fn body_effect(name: &str, template: &str) -> Value {
        effect(vec![
            ("type", str_("prompt")),
            ("name", str_(name)),
            ("template", str_(template)),
        ])
    }

    #[test]
    fn fixed_iteration_inside_the_loop_it_names_is_warned() {
        let mut each = Dict::new();
        each.insert(Value::Str("in".to_string()), str_("input.items"));
        each.insert(Value::Str("as".to_string()), str_("item"));
        let doc = doc_with_effects(vec![effect(vec![
            ("type", str_("loop")),
            ("name", str_("outer")),
            ("each", Value::Dict(each)),
            (
                "body",
                Value::List(vec![
                    body_effect("first", "FIRST {{item}}"),
                    body_effect("second", "iter0=[{{prime.outer.iter_0.first.value}}]"),
                ]),
            ),
        ])]);
        let warnings = lint_warnings(&doc);
        let stale: Vec<&String> = warnings
            .iter()
            .filter(|w| w.contains("fixed pass"))
            .collect();
        assert_eq!(stale.len(), 1);
        assert!(stale[0].contains("prime.outer.iter_0"));
        assert!(stale[0].contains("{{prime.<step>.value}}"));
    }

    #[test]
    fn loop_qualified_sibling_path_never_resolves() {
        let mut each = Dict::new();
        each.insert(Value::Str("in".to_string()), str_("input.items"));
        each.insert(Value::Str("as".to_string()), str_("item"));
        let doc = doc_with_effects(vec![effect(vec![
            ("type", str_("loop")),
            ("name", str_("outer")),
            ("each", Value::Dict(each)),
            (
                "body",
                Value::List(vec![
                    body_effect("first", "FIRST {{item}}"),
                    body_effect("second", "loopname=[{{prime.outer.first.value}}]"),
                ]),
            ),
        ])]);
        let warnings = lint_warnings(&doc);
        let dead: Vec<&String> = warnings
            .iter()
            .filter(|w| w.contains("never resolves"))
            .collect();
        assert_eq!(dead.len(), 1);
        assert!(dead[0].contains("prime.outer.first"));
        assert!(dead[0].contains("{{prime.first.value}}"));
    }

    #[test]
    fn last_in_the_loop_body_is_warned() {
        let mut each = Dict::new();
        each.insert(Value::Str("in".to_string()), str_("input.items"));
        each.insert(Value::Str("as".to_string()), str_("item"));
        let doc = doc_with_effects(vec![effect(vec![
            ("type", str_("loop")),
            ("name", str_("review")),
            ("each", Value::Dict(each)),
            (
                "body",
                Value::List(vec![body_effect(
                    "refine",
                    "Base it on {{prime.review.last.refine.value}}",
                )]),
            ),
        ])]);
        let warnings = lint_warnings(&doc);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("'prime.review.last'"));
        assert!(warnings[0].contains("previous pass at best"));
    }

    #[test]
    fn canonical_within_iteration_form_is_quiet() {
        let mut each = Dict::new();
        each.insert(Value::Str("in".to_string()), str_("input.items"));
        each.insert(Value::Str("as".to_string()), str_("item"));
        let doc = doc_with_effects(vec![effect(vec![
            ("type", str_("loop")),
            ("name", str_("outer")),
            ("each", Value::Dict(each)),
            (
                "body",
                Value::List(vec![
                    body_effect("first", "FIRST {{item}}"),
                    body_effect("second", "{{prime.first.value}}"),
                ]),
            ),
        ])]);
        assert_eq!(lint_warnings(&doc), Vec::<String>::new());
    }

    #[test]
    fn another_loops_finished_fixed_iteration_is_not_warned() {
        let mut each = Dict::new();
        each.insert(Value::Str("in".to_string()), str_("input.items"));
        each.insert(Value::Str("as".to_string()), str_("item"));
        let doc = doc_with_effects(vec![
            effect(vec![
                ("type", str_("loop")),
                ("name", str_("earlier")),
                ("each", Value::Dict(each.clone())),
                ("body", Value::List(vec![body_effect("seed", "S")])),
            ]),
            effect(vec![
                ("type", str_("loop")),
                ("name", str_("later")),
                ("each", Value::Dict(each)),
                (
                    "body",
                    Value::List(vec![body_effect(
                        "use_seed",
                        "{{prime.earlier.iter_0.seed.value}}",
                    )]),
                ),
            ]),
        ]);
        assert_eq!(lint_warnings(&doc), Vec::<String>::new());
    }

    #[test]
    fn non_mapping_document_lints_quietly() {
        assert_eq!(lint_warnings(&Value::None), Vec::<String>::new());
    }

    #[test]
    fn non_list_effects_with_no_list_steps_lints_quietly() {
        let mut dict = Dict::new();
        dict.insert(Value::Str("effects".to_string()), str_("not a list"));
        assert_eq!(lint_warnings(&Value::Dict(dict)), Vec::<String>::new());
    }

    #[test]
    fn non_mapping_entries_in_effects_are_skipped() {
        let doc = doc_with_effects(vec![Value::None, str_("nope"), int(3)]);
        assert_eq!(lint_warnings(&doc), Vec::<String>::new());
    }

    #[test]
    fn pathologically_long_loop_name_does_not_panic() {
        let long_name = "a".repeat(400_000);
        let mut each = Dict::new();
        each.insert(Value::Str("in".to_string()), str_("input.items"));
        each.insert(Value::Str("as".to_string()), str_("item"));
        let doc = doc_with_effects(vec![effect(vec![
            ("type", str_("loop")),
            ("name", str_(&long_name)),
            ("each", Value::Dict(each)),
            (
                "body",
                Value::List(vec![body_effect(
                    "step",
                    &format!("{{{{prime.{long_name}.last.step.value}}}}"),
                )]),
            ),
        ])]);
        let warnings = lint_warnings(&doc);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("previous pass at best"));
    }
}
