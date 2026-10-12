//! A hand-rolled JSON-Schema violation collector, operating directly on
//! order-preserving `electricity_value::Value` trees rather than on
//! `serde_json::Value` (a `BTreeMap`-backed `Map` this workspace never turns
//! `preserve_order` on for -- see `electricity-vm/Cargo.toml`'s own
//! dev-dependency comment) -- issue #498 ruling 6, tightened twice over by
//! PR #502's review (finding 2) and the orchestrator's follow-up probes.
//!
//! The `jsonschema` Rust crate (0.26) is still the accept/reject authority
//! (`validate_prompt_schema` only ever calls into this module after the
//! crate has already said a document is invalid) and is also still used,
//! narrowly, to decide whether an `anyOf`/`oneOf` *branch* itself validates
//! -- but the crate's own `serde_json::Value`-based error objects are never
//! used for *text*: they lose the schema author's own key order (so a
//! `type: [string, null]` list, or a multi-property object's own repr,
//! would render alphabetized instead of as-written), and the crate exposes
//! no sub-error list for `anyOf`/`oneOf` to descend into the way Python's
//! own `jsonschema.exceptions.best_match` does.
//!
//! This module instead walks the *schema's own declared key order* at every
//! level (`electricity_value::Dict` is an `IndexMap`, so that order is
//! exactly the YAML/JSON author's own), mirroring Python's own
//! `jsonschema.validators.create`'s default `applicable_validators`
//! (`methodcaller("items")` -- the schema dict's own `.items()`, not a
//! fixed per-draft keyword order) and each keyword's own internal iteration
//! order (`required`'s own list, `properties`'/`patternProperties`' own
//! dict, `items`' own instance-index order) -- so the violations this
//! module collects come out in *exactly* the order Python's own
//! `iter_errors` would yield them. [`best_match`]'s own tie-break needs
//! that order for two reasons, not one: Python's plain `max()` keeps the
//! *first* of several violations whose full relevance key compares
//! equal, **and** `jsonschema.exceptions.by_relevance`'s key itself has a
//! real path-content component (`error.path`, compared element by
//! element, not just `len(path)`) that this module's own earlier revision
//! missed entirely -- every same-depth tie used to fall through to
//! insertion order alone, which only coincidentally agreed with Python
//! whenever the lexicographically greatest path segment also happened to
//! be the one `iter_errors` yielded last (an array's own ascending index
//! order makes that look true far more often than it actually holds).
//!
//! Deliberately narrow: only the keyword families below are understood.
//! Anything else (`allOf`, `not`, `if`/`then`/`else`, `dependentRequired`/
//! `dependentSchemas`, `contains`, `propertyNames`, `format`,
//! `contentEncoding`/`contentMediaType`, `$dynamicRef`, the Draft 7 tuple
//! form of `items`, `additionalProperties` as a subschema rather than a
//! bare `false`) is silently skipped -- [`evaluate`] simply finds nothing
//! for it, and `validate_prompt_schema`'s own caller falls back to the
//! legacy `jsonschema`-crate-`Display`-text path whenever this module's
//! top-level call finds no violations at all despite the crate saying the
//! document is invalid (DESIGN.md §13 names the resulting gap precisely).

use electricity_value::{Dict, Value};

fn key(name: &str) -> Value {
    Value::Str(name.to_string())
}

fn get<'a>(dict: &'a Dict, name: &str) -> Option<&'a Value> {
    dict.get(&key(name))
}

/// One violation, in the shape Python's own `ValidationError` carries just
/// enough of for [`relevance`]/[`render`] -- `local_schema`/`local_instance`
/// are the exact schema/instance nodes the failing keyword ran against, so
/// a keyword's own value (`type`, `minimum`, `enum`, ...) and `_matches_type`
/// are both read from them directly, in their own authored order, at
/// render/relevance time rather than copied eagerly into this struct.
pub(crate) struct Violation<'a> {
    pub(crate) kind: Kind<'a>,
    path: Vec<PathSegment>,
    local_schema: &'a Value,
    local_instance: &'a Value,
}

/// One `error.path`/`error.relative_path` segment -- a `properties`/
/// `patternProperties` descent appends a [`PathSegment::Key`], an `items`
/// descent a [`PathSegment::Index`]; `required`/`type`/`enum`/... and every
/// other leaf keyword that yields a bare `ValidationError` with no
/// `path=`/`schema_path=` of its own (`validators.create.descend` only
/// ever prepends one when its own caller passes one) leaves the path
/// exactly as the enclosing `evaluate` call received it. An `anyOf`/
/// `oneOf` branch descent also leaves it unchanged (`_keywords.anyOf`/
/// `oneOf`'s own `validator.descend(instance, subschema, schema_path=
/// index)` calls never pass `path=` either).
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum PathSegment {
    Key(String),
    Index(usize),
}

/// `error.path`/`error.relative_path`'s own comparison (a `deque`, compared
/// element-by-element the way Python tuples/sequences are) -- the second
/// component of [`jsonschema.exceptions.by_relevance`]'s relevance key,
/// which this module's own earlier revision missed entirely (every
/// same-depth tie silently fell through to insertion order instead, which
/// only coincidentally matched Python whenever the lexicographically
/// greatest path segment also happened to be the one `iter_errors` yielded
/// last). A `Key`-vs-`Index` comparison (heterogeneous path shapes at the
/// same depth) has no Python analogue at all -- comparing a `str` to an
/// `int` raises `TypeError` there -- so this picks an arbitrary, documented
/// order (`Index` before `Key`) rather than mirroring a crash.
fn compare_paths(a: &[PathSegment], b: &[PathSegment]) -> std::cmp::Ordering {
    for (x, y) in a.iter().zip(b.iter()) {
        let ordering = match (x, y) {
            (PathSegment::Key(ka), PathSegment::Key(kb)) => ka.cmp(kb),
            (PathSegment::Index(ia), PathSegment::Index(ib)) => ia.cmp(ib),
            (PathSegment::Index(_), PathSegment::Key(_)) => std::cmp::Ordering::Less,
            (PathSegment::Key(_), PathSegment::Index(_)) => std::cmp::Ordering::Greater,
        };
        if ordering != std::cmp::Ordering::Equal {
            return ordering;
        }
    }
    a.len().cmp(&b.len())
}

pub(crate) enum Kind<'a> {
    Type,
    Required {
        property: &'a str,
    },
    Enum,
    Const,
    AdditionalPropertiesPlain {
        unexpected: Vec<&'a str>,
    },
    AdditionalPropertiesPatterned {
        unexpected: Vec<&'a str>,
    },
    MinItems {
        limit: i64,
    },
    MaxItems {
        limit: i64,
    },
    MinLength {
        limit: i64,
    },
    MaxLength {
        limit: i64,
    },
    MinProperties {
        limit: i64,
    },
    MaxProperties {
        limit: i64,
    },
    Minimum,
    Maximum,
    ExclusiveMinimum,
    ExclusiveMaximum,
    MultipleOf,
    Pattern,
    UniqueItems,
    AnyOfNotValid {
        context: Vec<Violation<'a>>,
    },
    OneOfNotValid {
        context: Vec<Violation<'a>>,
    },
    OneOfMultipleValid {
        matching: Vec<&'a Value>,
    },
    FalseSchema,
    /// A failing `anyOf`/`oneOf` branch this module's own `evaluate` found
    /// nothing wrong with (an unsupported keyword is the real cause) --
    /// carries the legacy crate-`Display` single-message text for that
    /// branch instead of leaving `context` silently missing an entry
    /// Python's own `_keywords.anyOf`/`oneOf` would always have populated.
    Fallback {
        message: String,
    },
}

fn is_weak(kind: &Kind<'_>) -> bool {
    matches!(
        kind,
        Kind::AnyOfNotValid { .. } | Kind::OneOfNotValid { .. } | Kind::OneOfMultipleValid { .. }
    )
}

fn as_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Int(i) => Some(i.to_f64()),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

/// `TypeChecker.is_type` (Draft 2020-12): a `bool` is never `"integer"`/
/// `"number"` even though Python's own `bool` is an `int` subclass; an
/// `"integer"` instance is a JSON literal with no `.`/`e` in it (`Value::
/// Int`), never a whole-numbered `Value::Float` -- `json.loads("3.0")` is a
/// Python `float`, which `is_integer` rejects, so no fuzzy
/// `f.fract() == 0.0` check belongs here either.
fn is_type_match(instance: &Value, type_name: &str) -> bool {
    match type_name {
        "null" => matches!(instance, Value::None),
        "boolean" => matches!(instance, Value::Bool(_)),
        "object" => matches!(instance, Value::Dict(_)),
        "array" => matches!(instance, Value::List(_)),
        "string" => matches!(instance, Value::Str(_)),
        "integer" => matches!(instance, Value::Int(_)),
        "number" => matches!(instance, Value::Int(_) | Value::Float(_)),
        _ => false,
    }
}

/// `_Error._matches_type`: `False` when *local_schema* has no `"type"` key
/// at all (or it's malformed); otherwise whether *local_instance* matches
/// the declared type(s).
fn matches_type(local_schema: &Value, local_instance: &Value) -> bool {
    let Some(declared) = local_schema.as_dict().and_then(|d| get(d, "type")) else {
        return false;
    };
    match declared {
        Value::Str(t) => is_type_match(local_instance, t),
        Value::List(types) => types
            .iter()
            .filter_map(Value::as_str)
            .any(|t| is_type_match(local_instance, t)),
        _ => false,
    }
}

/// `jsonschema.exceptions.by_relevance()`'s own relevance key: `(-len(
/// path), path, validator not in weak, validator in strong, not
/// _matches_type())`, minus the constant-`False` `validator in
/// STRONG_MATCHES` component (`STRONG_MATCHES` is always empty in this
/// version, so that component can never change a comparison's outcome --
/// dropping it changes nothing observable). `path` itself -- compared
/// element-by-element, not just by length -- is a real component, not a
/// tie-break nicety: two same-depth sibling errors (two wrong array
/// elements, two wrong object properties) are routinely tied on every
/// *other* component, and Python's own `max()` then picks whichever has
/// the lexicographically *greatest* path (`compare_paths`'s own doc
/// comment).
fn relevance_cmp(a: &Violation<'_>, b: &Violation<'_>) -> std::cmp::Ordering {
    (-(a.path.len() as i64))
        .cmp(&-(b.path.len() as i64))
        .then_with(|| compare_paths(&a.path, &b.path))
        .then_with(|| (!is_weak(&a.kind)).cmp(&!is_weak(&b.kind)))
        .then_with(|| {
            (!matches_type(a.local_schema, a.local_instance))
                .cmp(&!matches_type(b.local_schema, b.local_instance))
        })
}

/// `jsonschema.exceptions.best_match`: `max(violations, key=relevance)`
/// (Python's plain `max` keeps the *first* maximal element whenever
/// [`relevance_cmp`] reports every candidate as equal to the current
/// best, which this loop mirrors by only replacing `best` on a strict
/// `Greater`), then the `anyOf`/`oneOf` context descent (`heapq.
/// nsmallest(2, ...)`, stopping -- ambiguous -- the moment the two
/// smallest contexts tie).
pub(crate) fn best_match<'a>(violations: &'a [Violation<'a>]) -> Option<&'a Violation<'a>> {
    let mut best: Option<&Violation<'a>> = None;
    for v in violations {
        let better = match best {
            None => true,
            Some(current) => relevance_cmp(v, current) == std::cmp::Ordering::Greater,
        };
        if better {
            best = Some(v);
        }
    }
    let mut best = best?;
    loop {
        let context = match &best.kind {
            Kind::AnyOfNotValid { context } | Kind::OneOfNotValid { context } => context,
            _ => return Some(best),
        };
        if context.is_empty() {
            return Some(best);
        }
        let mut by_relevance: Vec<&Violation<'a>> = context.iter().collect();
        by_relevance.sort_by(|a, b| relevance_cmp(a, b));
        if by_relevance.len() >= 2
            && relevance_cmp(by_relevance[0], by_relevance[1]) == std::cmp::Ordering::Equal
        {
            return Some(best);
        }
        best = by_relevance[0];
    }
}

/// Resolves a local `"#/a/b/c"` JSON Pointer against *root* -- the only
/// `$ref` shape this module understands (same-document; DESIGN.md already
/// documents Circuitry's own schemas as never using anything else, and a
/// document author's own prompt `schema:` resolving to a genuinely remote
/// `$ref` falls outside what `validate_prompt_schema` ever fetches anyway).
fn resolve_ref<'a>(root: &'a Value, pointer: &str) -> Option<&'a Value> {
    let rest = pointer
        .strip_prefix("#/")
        .or_else(|| pointer.strip_prefix('#'))?;
    let mut node = root;
    if rest.is_empty() {
        return Some(node);
    }
    for raw_segment in rest.split('/') {
        let segment = raw_segment.replace("~1", "/").replace("~0", "~");
        node = match node {
            Value::Dict(d) => get(d, &segment)?,
            Value::List(items) => items.get(segment.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(node)
}

/// Collects every violation *schema* has against *instance*, in Python's
/// own `iter_errors` order, appending to *out*. *path* is the instance
/// path already descended through from this call's own starting point (an
/// `anyOf`/`oneOf` branch descent never extends it --
/// `jsonschema.validators.create.descend`'s own `path=None` for those two
/// keywords, matching `_keywords.anyOf`/`oneOf`'s own `validator.descend(
/// instance, subschema, schema_path=index)` call, no `path=` at all).
pub(crate) fn evaluate<'a>(
    schema: &'a Value,
    instance: &'a Value,
    root: &'a Value,
    path: &[PathSegment],
    out: &mut Vec<Violation<'a>>,
) {
    if matches!(schema, Value::Bool(false)) {
        out.push(Violation {
            kind: Kind::FalseSchema,
            path: path.to_vec(),
            local_schema: schema,
            local_instance: instance,
        });
        return;
    }
    let Some(dict) = schema.as_dict() else {
        return;
    };
    for (raw_key, value) in dict.iter() {
        let Some(k) = raw_key.as_str() else { continue };
        match k {
            "$ref" => {
                if let Some(pointer) = value.as_str() {
                    if let Some(resolved) = resolve_ref(root, pointer) {
                        evaluate(resolved, instance, root, path, out);
                    }
                }
            }
            "type" => {
                if !matches_type(schema, instance) {
                    out.push(Violation {
                        kind: Kind::Type,
                        path: path.to_vec(),
                        local_schema: schema,
                        local_instance: instance,
                    });
                }
            }
            "enum" => {
                if let Value::List(options) = value {
                    if !options.contains(instance) {
                        out.push(Violation {
                            kind: Kind::Enum,
                            path: path.to_vec(),
                            local_schema: schema,
                            local_instance: instance,
                        });
                    }
                }
            }
            "const" => {
                if instance != value {
                    out.push(Violation {
                        kind: Kind::Const,
                        path: path.to_vec(),
                        local_schema: schema,
                        local_instance: instance,
                    });
                }
            }
            "required" => {
                let (Value::List(required), Value::Dict(obj)) = (value, instance) else {
                    continue;
                };
                for property in required {
                    let Some(name) = property.as_str() else {
                        continue;
                    };
                    if !obj.contains_key(&key(name)) {
                        out.push(Violation {
                            kind: Kind::Required { property: name },
                            path: path.to_vec(),
                            local_schema: schema,
                            local_instance: instance,
                        });
                    }
                }
            }
            "properties" => {
                let (Value::Dict(properties), Value::Dict(obj)) = (value, instance) else {
                    continue;
                };
                for (prop_key, subschema) in properties.iter() {
                    let Some(name) = prop_key.as_str() else {
                        continue;
                    };
                    if let Some(prop_instance) = obj.get(&key(name)) {
                        let mut child_path = path.to_vec();
                        child_path.push(PathSegment::Key(name.to_string()));
                        evaluate(subschema, prop_instance, root, &child_path, out);
                    }
                }
            }
            "patternProperties" => {
                let (Value::Dict(patterns), Value::Dict(obj)) = (value, instance) else {
                    continue;
                };
                for (pattern_key, subschema) in patterns.iter() {
                    let Some(pattern) = pattern_key.as_str() else {
                        continue;
                    };
                    let Some(re) = compile_pattern(pattern) else {
                        continue;
                    };
                    for (prop_key, prop_instance) in obj.iter() {
                        let Some(name) = prop_key.as_str() else {
                            continue;
                        };
                        if re.is_match(name).unwrap_or(false) {
                            let mut child_path = path.to_vec();
                            child_path.push(PathSegment::Key(name.to_string()));
                            evaluate(subschema, prop_instance, root, &child_path, out);
                        }
                    }
                }
            }
            "additionalProperties" => {
                let (Value::Bool(false), Value::Dict(obj)) = (value, instance) else {
                    continue;
                };
                let properties = schema
                    .as_dict()
                    .and_then(|d| get(d, "properties"))
                    .and_then(Value::as_dict);
                let pattern_keys: Vec<&str> = schema
                    .as_dict()
                    .and_then(|d| get(d, "patternProperties"))
                    .and_then(Value::as_dict)
                    .map(|d| d.keys().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                let patterns: Vec<_> = pattern_keys
                    .iter()
                    .filter_map(|p| compile_pattern(p))
                    .collect();
                let mut extras: Vec<&str> = Vec::new();
                for (prop_key, _) in obj.iter() {
                    let Some(name) = prop_key.as_str() else {
                        continue;
                    };
                    if properties.is_some_and(|p| p.contains_key(&key(name))) {
                        continue;
                    }
                    if patterns.iter().any(|re| re.is_match(name).unwrap_or(false)) {
                        continue;
                    }
                    extras.push(name);
                }
                if !extras.is_empty() {
                    extras.sort_unstable();
                    let kind = if !pattern_keys.is_empty() {
                        Kind::AdditionalPropertiesPatterned { unexpected: extras }
                    } else {
                        Kind::AdditionalPropertiesPlain { unexpected: extras }
                    };
                    out.push(Violation {
                        kind,
                        path: path.to_vec(),
                        local_schema: schema,
                        local_instance: instance,
                    });
                }
            }
            "items" => {
                // Only the Draft 2020-12 single-schema form; the Draft 7
                // tuple form (`items` itself a list) is left to the
                // legacy fallback.
                if matches!(value, Value::List(_)) {
                    continue;
                }
                if let Value::List(elements) = instance {
                    for (index, element) in elements.iter().enumerate() {
                        let mut child_path = path.to_vec();
                        child_path.push(PathSegment::Index(index));
                        evaluate(value, element, root, &child_path, out);
                    }
                }
            }
            "minItems" => {
                if let (Value::List(items), Some(limit)) = (instance, as_i64(value)) {
                    if (items.len() as i64) < limit {
                        out.push(Violation {
                            kind: Kind::MinItems { limit },
                            path: path.to_vec(),
                            local_schema: schema,
                            local_instance: instance,
                        });
                    }
                }
            }
            "maxItems" => {
                if let (Value::List(items), Some(limit)) = (instance, as_i64(value)) {
                    if (items.len() as i64) > limit {
                        out.push(Violation {
                            kind: Kind::MaxItems { limit },
                            path: path.to_vec(),
                            local_schema: schema,
                            local_instance: instance,
                        });
                    }
                }
            }
            "minLength" => {
                if let (Value::Str(s), Some(limit)) = (instance, as_i64(value)) {
                    if (s.chars().count() as i64) < limit {
                        out.push(Violation {
                            kind: Kind::MinLength { limit },
                            path: path.to_vec(),
                            local_schema: schema,
                            local_instance: instance,
                        });
                    }
                }
            }
            "maxLength" => {
                if let (Value::Str(s), Some(limit)) = (instance, as_i64(value)) {
                    if (s.chars().count() as i64) > limit {
                        out.push(Violation {
                            kind: Kind::MaxLength { limit },
                            path: path.to_vec(),
                            local_schema: schema,
                            local_instance: instance,
                        });
                    }
                }
            }
            "minProperties" => {
                if let (Value::Dict(obj), Some(limit)) = (instance, as_i64(value)) {
                    if (obj.len() as i64) < limit {
                        out.push(Violation {
                            kind: Kind::MinProperties { limit },
                            path: path.to_vec(),
                            local_schema: schema,
                            local_instance: instance,
                        });
                    }
                }
            }
            "maxProperties" => {
                if let (Value::Dict(obj), Some(limit)) = (instance, as_i64(value)) {
                    if (obj.len() as i64) > limit {
                        out.push(Violation {
                            kind: Kind::MaxProperties { limit },
                            path: path.to_vec(),
                            local_schema: schema,
                            local_instance: instance,
                        });
                    }
                }
            }
            "minimum" => {
                if let (Some(i), Some(limit)) = (as_f64(instance), as_f64(value)) {
                    if matches!(instance, Value::Int(_) | Value::Float(_)) && i < limit {
                        out.push(Violation {
                            kind: Kind::Minimum,
                            path: path.to_vec(),
                            local_schema: schema,
                            local_instance: instance,
                        });
                    }
                }
            }
            "maximum" => {
                if let (Some(i), Some(limit)) = (as_f64(instance), as_f64(value)) {
                    if matches!(instance, Value::Int(_) | Value::Float(_)) && i > limit {
                        out.push(Violation {
                            kind: Kind::Maximum,
                            path: path.to_vec(),
                            local_schema: schema,
                            local_instance: instance,
                        });
                    }
                }
            }
            "exclusiveMinimum" => {
                if let (Some(i), Some(limit)) = (as_f64(instance), as_f64(value)) {
                    if matches!(instance, Value::Int(_) | Value::Float(_)) && i <= limit {
                        out.push(Violation {
                            kind: Kind::ExclusiveMinimum,
                            path: path.to_vec(),
                            local_schema: schema,
                            local_instance: instance,
                        });
                    }
                }
            }
            "exclusiveMaximum" => {
                if let (Some(i), Some(limit)) = (as_f64(instance), as_f64(value)) {
                    if matches!(instance, Value::Int(_) | Value::Float(_)) && i >= limit {
                        out.push(Violation {
                            kind: Kind::ExclusiveMaximum,
                            path: path.to_vec(),
                            local_schema: schema,
                            local_instance: instance,
                        });
                    }
                }
            }
            "multipleOf" => {
                if let (Some(i), Some(divisor)) = (as_f64(instance), as_f64(value)) {
                    if matches!(instance, Value::Int(_) | Value::Float(_)) && divisor != 0.0 {
                        let quotient = i / divisor;
                        if quotient.trunc() != quotient {
                            out.push(Violation {
                                kind: Kind::MultipleOf,
                                path: path.to_vec(),
                                local_schema: schema,
                                local_instance: instance,
                            });
                        }
                    }
                }
            }
            "pattern" => {
                if let (Value::Str(s), Some(pattern)) = (instance, value.as_str()) {
                    if let Some(re) = compile_pattern(pattern) {
                        if !re.is_match(s).unwrap_or(true) {
                            out.push(Violation {
                                kind: Kind::Pattern,
                                path: path.to_vec(),
                                local_schema: schema,
                                local_instance: instance,
                            });
                        }
                    }
                }
            }
            "uniqueItems" => {
                if let (Value::Bool(true), Value::List(items)) = (value, instance) {
                    let mut seen: Vec<&Value> = Vec::new();
                    let mut duplicate = false;
                    for item in items {
                        if seen.contains(&item) {
                            duplicate = true;
                            break;
                        }
                        seen.push(item);
                    }
                    if duplicate {
                        out.push(Violation {
                            kind: Kind::UniqueItems,
                            path: path.to_vec(),
                            local_schema: schema,
                            local_instance: instance,
                        });
                    }
                }
            }
            "anyOf" => {
                let Value::List(branches) = value else {
                    continue;
                };
                let mut context = Vec::new();
                let mut all_valid = false;
                for branch in branches {
                    if schema_accepts(branch, instance, root) {
                        all_valid = true;
                        break;
                    }
                    collect_branch_violations(branch, instance, root, path, &mut context);
                }
                if !all_valid {
                    out.push(Violation {
                        kind: Kind::AnyOfNotValid { context },
                        path: path.to_vec(),
                        local_schema: schema,
                        local_instance: instance,
                    });
                }
            }
            "oneOf" => {
                // `_keywords.oneOf`'s own two-loop shape over one shared
                // `enumerate(oneOf)` iterator: the first loop breaks the
                // instant it finds a valid branch (`first_valid`),
                // collecting every *earlier* branch's own violations into
                // `context` as it goes; only then does a second loop walk
                // whatever's left (starting *after* `first_valid`'s own
                // index) checking each remaining branch's bare validity.
                // `more_valid.append(first_valid)` puts the first match
                // *last* in `OneOfMultipleValid`'s own repr list, not
                // first -- a real quirk, not a transcription slip.
                let Value::List(branches) = value else {
                    continue;
                };
                let mut context = Vec::new();
                let mut first_valid: Option<&Value> = None;
                let mut rest_start = branches.len();
                for (index, branch) in branches.iter().enumerate() {
                    if schema_accepts(branch, instance, root) {
                        first_valid = Some(branch);
                        rest_start = index + 1;
                        break;
                    }
                    collect_branch_violations(branch, instance, root, path, &mut context);
                }
                match first_valid {
                    None => {
                        out.push(Violation {
                            kind: Kind::OneOfNotValid { context },
                            path: path.to_vec(),
                            local_schema: schema,
                            local_instance: instance,
                        });
                    }
                    Some(first) => {
                        let mut more_valid: Vec<&Value> = branches[rest_start..]
                            .iter()
                            .filter(|branch| schema_accepts(branch, instance, root))
                            .collect();
                        if !more_valid.is_empty() {
                            more_valid.push(first);
                            out.push(Violation {
                                kind: Kind::OneOfMultipleValid {
                                    matching: more_valid,
                                },
                                path: path.to_vec(),
                                local_schema: schema,
                                local_instance: instance,
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn as_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Int(electricity_value::IntValue::Small(n)) => Some(*n),
        _ => None,
    }
}

fn compile_pattern(pattern: &str) -> Option<fancy_regex::Regex> {
    let normalized = crate::rewrite_digit_class(&crate::rewrite_trailing_dollar(pattern));
    fancy_regex::Regex::new(&normalized).ok()
}

/// Whether *instance* validates against *branch* at all -- delegates to the
/// `jsonschema` crate (via a `serde_json` round trip) rather than this
/// module's own `evaluate`, since a pure accept/reject check has none of
/// `evaluate`'s own text-rendering concerns and the crate's verdict is
/// already trusted everywhere else in `validate_prompt_schema`.
fn schema_accepts(branch: &Value, instance: &Value, root: &Value) -> bool {
    let resolved = resolve_schema_refs(branch, root);
    let branch_json = crate::value_to_json(&resolved);
    let instance_json = crate::value_to_json(instance);
    match jsonschema::validator_for(&branch_json) {
        Ok(validator) => validator.is_valid(&instance_json),
        Err(_) => false,
    }
}

/// `jsonschema`-crate-based validators are each built standalone (`validator_for`
/// on just *branch*), so a `$ref` inside an `anyOf`/`oneOf` branch pointing
/// back into the *root* document (never itself part of the standalone
/// branch) would otherwise fail to resolve -- this inlines one level of
/// local `$ref`s (`"#/..."` only, matching [`resolve_ref`]'s own scope)
/// before handing the branch to the crate.
fn resolve_schema_refs(branch: &Value, root: &Value) -> Value {
    if let Some(pointer) = branch
        .as_dict()
        .and_then(|d| get(d, "$ref"))
        .and_then(Value::as_str)
    {
        if let Some(resolved) = resolve_ref(root, pointer) {
            return resolved.clone();
        }
    }
    branch.clone()
}

/// Collects *branch*'s own violations against *instance* into *context*,
/// the same way Python's `_keywords.anyOf`/`oneOf` populate `all_errors`
/// (`.extend`, never grouped per branch) -- when this module's own
/// `evaluate` finds nothing for a branch the crate has already confirmed
/// is invalid (an unsupported keyword), a single [`Kind::Fallback`] entry
/// stands in so `context` is never silently missing an entry Python's own
/// `_keywords.anyOf`/`oneOf` would always have populated.
fn collect_branch_violations<'a>(
    branch: &'a Value,
    instance: &'a Value,
    root: &'a Value,
    path: &[PathSegment],
    context: &mut Vec<Violation<'a>>,
) {
    let before = context.len();
    evaluate(branch, instance, root, path, context);
    if context.len() == before {
        let resolved = resolve_schema_refs(branch, root);
        let branch_json = crate::value_to_json(&resolved);
        let instance_json = crate::value_to_json(instance);
        if let Ok(validator) = jsonschema::validator_for(&branch_json) {
            if let Err(err) = validator.validate(&instance_json) {
                let message = crate::substitute_value_prefix(&err.instance, err.to_string());
                context.push(Violation {
                    kind: Kind::Fallback { message },
                    path: path.to_vec(),
                    local_schema: branch,
                    local_instance: instance,
                });
            }
        }
    }
}

/// Renders *violation*'s own message text, exactly matching the Python
/// `jsonschema` template for the keyword it came from.
pub(crate) fn render(violation: &Violation<'_>) -> String {
    let instance = violation.local_instance;
    let schema = violation.local_schema;
    let schema_dict = schema.as_dict();
    match &violation.kind {
        Kind::Type => {
            let declared = schema_dict.and_then(|d| get(d, "type"));
            let reprs = match declared {
                Some(Value::Str(t)) => format!("'{t}'"),
                Some(Value::List(types)) => types
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|t| format!("'{t}'"))
                    .collect::<Vec<_>>()
                    .join(", "),
                _ => String::new(),
            };
            format!("{} is not of type {reprs}", instance.py_repr())
        }
        Kind::Required { property } => {
            format!(
                "{} is a required property",
                Value::Str(property.to_string()).py_repr()
            )
        }
        Kind::Enum => {
            let options = schema_dict
                .and_then(|d| get(d, "enum"))
                .map(Value::py_repr)
                .unwrap_or_default();
            format!("{} is not one of {options}", instance.py_repr())
        }
        Kind::Const => {
            let expected = schema_dict
                .and_then(|d| get(d, "const"))
                .map(Value::py_repr)
                .unwrap_or_default();
            format!("{expected} was expected")
        }
        Kind::AdditionalPropertiesPlain { unexpected } => {
            let (joined, verb) = extras_msg(unexpected);
            format!("Additional properties are not allowed ({joined} {verb} unexpected)")
        }
        Kind::AdditionalPropertiesPatterned { unexpected } => {
            let (joined, verb) = extras_msg(unexpected);
            let verb = if verb == "was" { "does" } else { "do" };
            let mut pattern_keys: Vec<&str> = schema_dict
                .and_then(|d| get(d, "patternProperties"))
                .and_then(Value::as_dict)
                .map(|d| d.keys().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            pattern_keys.sort_unstable();
            let patterns = pattern_keys
                .iter()
                .map(|p| Value::Str(p.to_string()).py_repr())
                .collect::<Vec<_>>()
                .join(", ");
            format!("{joined} {verb} not match any of the regexes: {patterns}")
        }
        Kind::MinItems { limit } => {
            let message = if *limit == 1 {
                "should be non-empty"
            } else {
                "is too short"
            };
            format!("{} {message}", instance.py_repr())
        }
        Kind::MaxItems { limit } => {
            let message = if *limit == 0 {
                "is expected to be empty"
            } else {
                "is too long"
            };
            format!("{} {message}", instance.py_repr())
        }
        Kind::MinLength { limit } => {
            let message = if *limit == 1 {
                "should be non-empty"
            } else {
                "is too short"
            };
            format!("{} {message}", instance.py_repr())
        }
        Kind::MaxLength { limit } => {
            let message = if *limit == 0 {
                "is expected to be empty"
            } else {
                "is too long"
            };
            format!("{} {message}", instance.py_repr())
        }
        Kind::MinProperties { limit } => {
            let message = if *limit == 1 {
                "should be non-empty"
            } else {
                "does not have enough properties"
            };
            format!("{} {message}", instance.py_repr())
        }
        Kind::MaxProperties { limit } => {
            let message = if *limit == 0 {
                "is expected to be empty"
            } else {
                "has too many properties"
            };
            format!("{} {message}", instance.py_repr())
        }
        Kind::Minimum => {
            let limit = schema_dict
                .and_then(|d| get(d, "minimum"))
                .map(Value::py_repr)
                .unwrap_or_default();
            format!("{} is less than the minimum of {limit}", instance.py_repr())
        }
        Kind::Maximum => {
            let limit = schema_dict
                .and_then(|d| get(d, "maximum"))
                .map(Value::py_repr)
                .unwrap_or_default();
            format!(
                "{} is greater than the maximum of {limit}",
                instance.py_repr()
            )
        }
        Kind::ExclusiveMinimum => {
            let limit = schema_dict
                .and_then(|d| get(d, "exclusiveMinimum"))
                .map(Value::py_repr)
                .unwrap_or_default();
            format!(
                "{} is less than or equal to the minimum of {limit}",
                instance.py_repr()
            )
        }
        Kind::ExclusiveMaximum => {
            let limit = schema_dict
                .and_then(|d| get(d, "exclusiveMaximum"))
                .map(Value::py_repr)
                .unwrap_or_default();
            format!(
                "{} is greater than or equal to the maximum of {limit}",
                instance.py_repr()
            )
        }
        Kind::MultipleOf => {
            let divisor = schema_dict
                .and_then(|d| get(d, "multipleOf"))
                .map(Value::py_repr)
                .unwrap_or_default();
            format!("{} is not a multiple of {divisor}", instance.py_repr())
        }
        Kind::Pattern => {
            let pattern = schema_dict
                .and_then(|d| get(d, "pattern"))
                .map(Value::py_repr)
                .unwrap_or_default();
            format!("{} does not match {pattern}", instance.py_repr())
        }
        Kind::UniqueItems => format!("{} has non-unique elements", instance.py_repr()),
        Kind::AnyOfNotValid { context } | Kind::OneOfNotValid { context } => {
            let _ = context;
            format!(
                "{} is not valid under any of the given schemas",
                instance.py_repr()
            )
        }
        Kind::OneOfMultipleValid { matching } => {
            let reprs = matching
                .iter()
                .map(|s| s.py_repr())
                .collect::<Vec<_>>()
                .join(", ");
            format!("{} is valid under each of {reprs}", instance.py_repr())
        }
        Kind::FalseSchema => format!("False schema does not allow {}", instance.py_repr()),
        Kind::Fallback { message } => message.clone(),
    }
}

/// `jsonschema._utils.extras_msg`: the comma-joined `repr()` of *extras*
/// (already sorted by the caller) plus the right `was`/`were` verb.
fn extras_msg(extras: &[&str]) -> (String, &'static str) {
    let joined = extras
        .iter()
        .map(|s| Value::Str(s.to_string()).py_repr())
        .collect::<Vec<_>>()
        .join(", ");
    let verb = if extras.len() == 1 { "was" } else { "were" };
    (joined, verb)
}
