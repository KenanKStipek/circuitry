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
    Not {
        not_schema: &'a Value,
    },
    DependentRequired {
        each: &'a str,
        property: &'a str,
    },
    ContainsNone,
    MinContains {
        min_contains: i64,
        matches: i64,
    },
    MaxContains {
        max_contains: i64,
    },
    ItemsFalse {
        prefix: usize,
        extra: usize,
        item_word: &'static str,
        rest_repr: String,
    },
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

/// `jsonschema._utils.equal`: evades Python's own type hierarchy (`bool`
/// is an `int` subclass) by special-casing it, and never equates a `str`
/// with anything but another `str` (unlike `_sequence_equal`, which would
/// otherwise iterate a string's own characters as if it were a plain
/// sequence). `None`/numeric comparison falls through to plain `==`.
/// Numeric cross-type comparison (`Int` vs `Float`) goes through `f64`,
/// not exact arbitrary-precision arithmetic -- Python's own `int`-`float`
/// `==` is exact at any magnitude; this is a narrow, documented residual
/// gap for an enum/const value large enough for that to matter (`DESIGN.md`
/// §13), accepted the same way [`as_f64`]'s other callers already are.
pub(crate) fn py_equal(one: &Value, two: &Value) -> bool {
    match (one, two) {
        (Value::Str(a), Value::Str(b)) => a == b,
        (Value::Str(_), _) | (_, Value::Str(_)) => false,
        (Value::List(a), Value::List(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| py_equal(x, y))
        }
        (Value::Dict(a), Value::Dict(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(k, v)| b.get(k).is_some_and(|v2| py_equal(v, v2)))
        }
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Bool(_), _) | (_, Value::Bool(_)) => false,
        (Value::None, Value::None) => true,
        (Value::None, _) | (_, Value::None) => false,
        (Value::Int(a), Value::Int(b)) => a.to_bigint() == b.to_bigint(),
        _ => match (as_f64(one), as_f64(two)) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        },
    }
}

/// A hashable stand-in for [`py_equal`]'s own equivalence classes --
/// `Hash`/`Eq` agree with it exactly (unlike `electricity_value::Value`'s
/// own derived ones, which deliberately let `True`/`1`/`1.0` collide as
/// Python dict keys do -- the opposite of what `jsonschema._utils.equal`
/// wants here). `Int`/`Float` share one numeric bucket so `1` and `1.0`
/// still collide (Python's own `hash(1) == hash(1.0)` invariant); a
/// `Float` outside `i64`'s range, or carrying a fractional part, falls
/// back to its own bit pattern; a `NaN` has no key at all ([`uniq_key`]
/// gives `None`), since it is never equal to anything under `py_equal`
/// -- not even another `NaN`, not even itself -- and a hash key would
/// otherwise wrongly treat two same-bit-pattern `NaN`s as a duplicate
/// pair.
#[derive(PartialEq, Eq, Hash)]
enum UniqKey {
    Str(String),
    Bool(bool),
    None,
    Int(i64),
    FloatBits(u64),
}

fn uniq_key(value: &Value) -> Option<UniqKey> {
    match value {
        Value::Str(s) => Some(UniqKey::Str(s.clone())),
        Value::Bool(b) => Some(UniqKey::Bool(*b)),
        Value::None => Some(UniqKey::None),
        Value::Int(electricity_value::IntValue::Small(n)) => Some(UniqKey::Int(*n)),
        Value::Float(f) if f.is_nan() => None,
        Value::Float(f)
            if f.is_finite()
                && f.fract() == 0.0
                && *f >= i64::MIN as f64
                && *f <= i64::MAX as f64 =>
        {
            Some(UniqKey::Int(*f as i64))
        }
        Value::Float(f) => Some(UniqKey::FloatBits(f.to_bits())),
        // `IntValue::Big` and every container type fall back to
        // `py_uniq`'s own brute-force bucket below -- a `BigInt` could
        // in principle get its own exact key too, but no required
        // case needs `uniqueItems` over instances that large.
        _ => None,
    }
}

/// `jsonschema._utils.uniq`: Python's own *sorted* fast path isn't
/// replicated (it needs every element mutually orderable, a `TypeError`
/// in Python for anything heterogeneous, caught and handled by the same
/// brute-force fallback this always uses for a `List`/`Dict` element --
/// container equality is never by identity/hash here, so there's no fast
/// path for it to take regardless). Every hashable scalar (everything
/// but a `List`, a `Dict`, or a `BigInt`) instead goes through
/// [`uniq_key`]'s own `O(n)` duplicate-count pass -- the one that
/// actually matters for a large array, per issue #498's own orchestrator
/// follow-up, finding N11 (`O(n^2)` over 50k items took several seconds
/// in a debug build; this brings it back to milliseconds).
fn py_uniq(items: &[Value]) -> bool {
    let mut seen: std::collections::HashMap<UniqKey, usize> = std::collections::HashMap::new();
    let mut unhashable: Vec<&Value> = Vec::new();
    for item in items {
        match uniq_key(item) {
            Some(key) => {
                let count = seen.entry(key).or_insert(0);
                *count += 1;
                if *count > 1 {
                    return false;
                }
            }
            None => unhashable.push(item),
        }
    }
    for (i, item) in unhashable.iter().enumerate() {
        for earlier in &unhashable[..i] {
            if py_equal(earlier, item) {
                return false;
            }
        }
    }
    true
}

/// `TypeChecker.is_type`: a `bool` is never `"integer"`/`"number"` even
/// though Python's own `bool` is an `int` subclass. `"integer"` is wider
/// than a bare JSON-literal integer from Draft 6 onward (every draft this
/// engine targets): `draft6_type_checker.redefine("integer", ...)` also
/// accepts a whole-numbered, *finite* `float` (`instance.is_integer()`),
/// so `3.0` passes `type: integer` even though `json.loads("3.0")` is a
/// Python `float`, not an `int` -- `NaN`/`Infinity` never do (`float.
/// is_integer()` is `False` for both). `"number"` already includes every
/// `Value::Float`, finite or not, matching Python's own `isinstance(x,
/// numbers.Number)` (`NaN`/`Infinity` are real Python floats).
fn is_type_match(instance: &Value, type_name: &str) -> bool {
    match type_name {
        "null" => matches!(instance, Value::None),
        "boolean" => matches!(instance, Value::Bool(_)),
        "object" => matches!(instance, Value::Dict(_)),
        "array" => matches!(instance, Value::List(_)),
        "string" => matches!(instance, Value::Str(_)),
        "integer" => match instance {
            Value::Int(_) => true,
            Value::Float(f) => f.is_finite() && f.fract() == 0.0,
            _ => false,
        },
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

thread_local! {
    /// A global (thread-local, since `evaluate` never actually runs
    /// concurrently with itself on one thread -- it's a plain
    /// synchronous call tree with no `.await` inside it) recursion
    /// counter, incremented on every `evaluate` call regardless of
    /// entry point (a top-level `evaluate`, or one reached through
    /// `is_valid`'s own `not`/`if`/`contains`/`anyOf`/`oneOf` checks) --
    /// a self-referencing `$ref` (`$defs: {loop: {$ref: '#/$defs/loop'}}`)
    /// recurses through this exact function forever otherwise, and
    /// Rust has no `RecursionError` to catch: it's a stack overflow, an
    /// immediate abort with no state at all (issue #498's own
    /// orchestrator follow-up, finding N6).
    static RECURSION_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    /// Set once the depth budget below is exceeded during a call;
    /// [`crate::validate_prompt_schema`] checks and clears it after
    /// calling [`evaluate`]/[`best_match`], the same way Python's own
    /// uncaught `RecursionError` would reach `core/prompt.py`'s own
    /// generic `except Exception` (never `except jsonschema.
    /// ValidationError`) and record a `"failed"`, non-retryable attempt
    /// with that exception's own text -- not `schema_invalid`.
    pub(crate) static RECURSION_EXCEEDED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Comfortably past any legitimate schema's own nesting depth (Circuitry
/// documents never approach even double digits here), comfortably short
/// of where Rust's own real call-stack limit would bite first.
const MAX_RECURSION_DEPTH: u32 = 500;

struct DepthGuard;

impl Drop for DepthGuard {
    fn drop(&mut self) {
        RECURSION_DEPTH.with(|d| d.set(d.get() - 1));
    }
}

fn enter_depth_guard() -> (DepthGuard, bool) {
    let depth = RECURSION_DEPTH.with(|d| {
        let v = d.get() + 1;
        d.set(v);
        v
    });
    let exceeded = depth > MAX_RECURSION_DEPTH;
    if exceeded {
        RECURSION_EXCEEDED.with(|e| e.set(true));
    }
    (DepthGuard, exceeded)
}

/// Clears [`RECURSION_EXCEEDED`] and returns its own prior value --
/// [`crate::validate_prompt_schema`]'s own first step, since the flag is
/// thread-local *state*, not a return value, and must never leak from
/// one validation attempt into the next one on the same thread.
pub(crate) fn take_recursion_exceeded() -> bool {
    RECURSION_EXCEEDED.with(|e| e.replace(false))
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
    let (_depth_guard, exceeded) = enter_depth_guard();
    if exceeded {
        return;
    }
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
                    if !options.iter().any(|option| py_equal(option, instance)) {
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
                if !py_equal(instance, value) {
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
                let Value::Dict(obj) = instance else { continue };
                // `"patternProperties" in schema` (mere key *presence*,
                // even an empty `{}`) -- not whether any pattern inside
                // it actually compiled, matching `_keywords.
                // additionalProperties`'s own `if "patternProperties" in
                // schema:` check exactly (review of PR #502's own
                // orchestrator follow-up, finding N8).
                let has_pattern_properties = schema
                    .as_dict()
                    .is_some_and(|d| d.contains_key(&key("patternProperties")));
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
                if extras.is_empty() {
                    continue;
                }
                match value {
                    // A subschema: `for extra in extras: yield from
                    // validator.descend(instance[extra], aP, path=extra)`
                    // -- `extras` is a Python `set` there, so real `cof`
                    // output has no guaranteed order for 2+ extra keys;
                    // this keeps the instance's own key order instead, a
                    // documented, narrower residual gap than leaving the
                    // keyword unsupported entirely (`DESIGN.md` §13).
                    Value::Dict(_) => {
                        for name in &extras {
                            let prop_instance = obj.get(&key(name)).expect("just found above");
                            let mut child_path = path.to_vec();
                            child_path.push(PathSegment::Key(name.to_string()));
                            evaluate(value, prop_instance, root, &child_path, out);
                        }
                    }
                    Value::Bool(false) => {
                        extras.sort_unstable();
                        let kind = if has_pattern_properties {
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
                    // `true` (or any other bare value no real schema
                    // ever uses here): every property is allowed.
                    _ => {}
                }
            }
            "items" => {
                // Draft 7's tuple form (`items` itself a list) is left to
                // the legacy fallback -- 2020-12's `items` only ever
                // applies past `prefixItems`' own length.
                if matches!(value, Value::List(_)) {
                    continue;
                }
                let Value::List(elements) = instance else {
                    continue;
                };
                let prefix = schema
                    .as_dict()
                    .and_then(|d| get(d, "prefixItems"))
                    .and_then(Value::as_list)
                    .map(<[Value]>::len)
                    .unwrap_or(0);
                let total = elements.len();
                if total <= prefix {
                    continue;
                }
                if matches!(value, Value::Bool(false)) {
                    let extra = total - prefix;
                    let rest: Vec<Value> = elements[prefix..].to_vec();
                    let rest_repr = if extra == 1 {
                        rest[0].py_repr()
                    } else {
                        Value::List(rest).py_repr()
                    };
                    let item_word = if prefix == 1 { "item" } else { "items" };
                    out.push(Violation {
                        kind: Kind::ItemsFalse {
                            prefix,
                            extra,
                            item_word,
                            rest_repr,
                        },
                        path: path.to_vec(),
                        local_schema: schema,
                        local_instance: instance,
                    });
                } else {
                    for (index, element) in elements.iter().enumerate().skip(prefix) {
                        let mut child_path = path.to_vec();
                        child_path.push(PathSegment::Index(index));
                        evaluate(value, element, root, &child_path, out);
                    }
                }
            }
            "prefixItems" => {
                let (Value::List(prefix_schemas), Value::List(elements)) = (value, instance) else {
                    continue;
                };
                for (index, (subschema, element)) in
                    prefix_schemas.iter().zip(elements.iter()).enumerate()
                {
                    let mut child_path = path.to_vec();
                    child_path.push(PathSegment::Index(index));
                    evaluate(subschema, element, root, &child_path, out);
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
                if matches!(instance, Value::Int(_) | Value::Float(_))
                    && numeric_cmp(instance, value) == Some(std::cmp::Ordering::Less)
                {
                    out.push(Violation {
                        kind: Kind::Minimum,
                        path: path.to_vec(),
                        local_schema: schema,
                        local_instance: instance,
                    });
                }
            }
            "maximum" => {
                if matches!(instance, Value::Int(_) | Value::Float(_))
                    && numeric_cmp(instance, value) == Some(std::cmp::Ordering::Greater)
                {
                    out.push(Violation {
                        kind: Kind::Maximum,
                        path: path.to_vec(),
                        local_schema: schema,
                        local_instance: instance,
                    });
                }
            }
            "exclusiveMinimum" => {
                if matches!(instance, Value::Int(_) | Value::Float(_))
                    && matches!(
                        numeric_cmp(instance, value),
                        Some(std::cmp::Ordering::Less | std::cmp::Ordering::Equal)
                    )
                {
                    out.push(Violation {
                        kind: Kind::ExclusiveMinimum,
                        path: path.to_vec(),
                        local_schema: schema,
                        local_instance: instance,
                    });
                }
            }
            "exclusiveMaximum" => {
                if matches!(instance, Value::Int(_) | Value::Float(_))
                    && matches!(
                        numeric_cmp(instance, value),
                        Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)
                    )
                {
                    out.push(Violation {
                        kind: Kind::ExclusiveMaximum,
                        path: path.to_vec(),
                        local_schema: schema,
                        local_instance: instance,
                    });
                }
            }
            "multipleOf" => {
                if multiple_of_failed(instance, value) {
                    out.push(Violation {
                        kind: Kind::MultipleOf,
                        path: path.to_vec(),
                        local_schema: schema,
                        local_instance: instance,
                    });
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
                    if !py_uniq(items) {
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
            "allOf" => {
                // `for index, subschema in enumerate(allOf): yield from
                // validator.descend(instance, subschema, schema_path=
                // index)` -- no `path=`, so every subschema's own
                // violations flow straight into `out`, never wrapped.
                if let Value::List(subschemas) = value {
                    for subschema in subschemas {
                        evaluate(subschema, instance, root, path, out);
                    }
                }
            }
            "not" => {
                if is_valid(value, instance, root) {
                    out.push(Violation {
                        kind: Kind::Not { not_schema: value },
                        path: path.to_vec(),
                        local_schema: schema,
                        local_instance: instance,
                    });
                }
            }
            "if" => {
                if is_valid(value, instance, root) {
                    if let Some(then) = schema.as_dict().and_then(|d| get(d, "then")) {
                        evaluate(then, instance, root, path, out);
                    }
                } else if let Some(else_) = schema.as_dict().and_then(|d| get(d, "else")) {
                    evaluate(else_, instance, root, path, out);
                }
            }
            "dependentRequired" => {
                let (Value::Dict(dependent), Value::Dict(obj)) = (value, instance) else {
                    continue;
                };
                for (prop_key, dependency) in dependent.iter() {
                    let Some(property) = prop_key.as_str() else {
                        continue;
                    };
                    if !obj.contains_key(&key(property)) {
                        continue;
                    }
                    let Value::List(each_list) = dependency else {
                        continue;
                    };
                    for each in each_list {
                        let Some(each) = each.as_str() else {
                            continue;
                        };
                        if !obj.contains_key(&key(each)) {
                            out.push(Violation {
                                kind: Kind::DependentRequired { each, property },
                                path: path.to_vec(),
                                local_schema: schema,
                                local_instance: instance,
                            });
                        }
                    }
                }
            }
            "dependentSchemas" => {
                let (Value::Dict(dependent), Value::Dict(obj)) = (value, instance) else {
                    continue;
                };
                for (prop_key, dependency) in dependent.iter() {
                    let Some(property) = prop_key.as_str() else {
                        continue;
                    };
                    if obj.contains_key(&key(property)) {
                        evaluate(dependency, instance, root, path, out);
                    }
                }
            }
            "contains" => {
                let Value::List(elements) = instance else {
                    continue;
                };
                let min_contains = schema
                    .as_dict()
                    .and_then(|d| get(d, "minContains"))
                    .and_then(as_i64)
                    .unwrap_or(1);
                let max_contains = schema
                    .as_dict()
                    .and_then(|d| get(d, "maxContains"))
                    .and_then(as_i64)
                    .unwrap_or(elements.len() as i64);
                let mut matches: i64 = 0;
                let mut too_many = false;
                for element in elements {
                    if is_valid(value, element, root) {
                        matches += 1;
                        if matches > max_contains {
                            too_many = true;
                            break;
                        }
                    }
                }
                if too_many {
                    out.push(Violation {
                        kind: Kind::MaxContains { max_contains },
                        path: path.to_vec(),
                        local_schema: schema,
                        local_instance: instance,
                    });
                } else if matches < min_contains {
                    let kind = if matches == 0 {
                        Kind::ContainsNone
                    } else {
                        Kind::MinContains {
                            min_contains,
                            matches,
                        }
                    };
                    out.push(Violation {
                        kind,
                        path: path.to_vec(),
                        local_schema: schema,
                        local_instance: instance,
                    });
                }
            }
            "propertyNames" => {
                // `for property in instance: yield from validator.descend(
                // instance=property, schema=propertyNames)` -- no `path=`,
                // same as `allOf`/`not`'s own descends.
                let Value::Dict(obj) = instance else {
                    continue;
                };
                for prop_key in obj.keys() {
                    if prop_key.as_str().is_some() {
                        evaluate(value, prop_key, root, path, out);
                    }
                }
            }
            _ => {}
        }
    }
}

/// `Validator.is_valid`: *schema* has no violations at all against
/// *instance* -- used by `not`/`if`/`contains` exactly the way Python's
/// own keyword functions call `validator.evolve(schema=...).is_valid(...)`
/// rather than collecting (and discarding) a full violation list.
pub(crate) fn is_valid(schema: &Value, instance: &Value, root: &Value) -> bool {
    let mut violations = Vec::new();
    evaluate(schema, instance, root, &[], &mut violations);
    violations.is_empty()
}

/// `instance < limit`/`instance > limit`/... as Python's own numeric
/// tower compares them: exact for an `Int`-vs-`Int` pair (never losing
/// precision for a big instance, the same reason [`multiple_of_failed`]
/// does its own `int`/`int` arm in `BigInt`), `f64` otherwise (a `NaN` on
/// either side gives `None`, matching Python's own "every comparison
/// with `NaN` is `False`" rule -- callers that only check for one
/// specific `Ordering` already get that for free; `None` compares equal
/// to nothing).
fn numeric_cmp(instance: &Value, limit: &Value) -> Option<std::cmp::Ordering> {
    if let (Value::Int(a), Value::Int(b)) = (instance, limit) {
        return Some(a.to_bigint().cmp(&b.to_bigint()));
    }
    let a = as_f64(instance)?;
    let b = as_f64(limit)?;
    a.partial_cmp(&b)
}

/// `_keywords.multipleOf`, ported exactly: an `int`-instance/`int`-divisor
/// pair uses exact arbitrary-precision modulo (never `f64`, so issue
/// #498's own `2**70`-sized instance ported correctly rather than
/// drifting on the way to a float); anything else (either side a
/// `float`, including `NaN`/`Infinity`) follows `instance / dB`, checking
/// the quotient has no fractional part. Python's own `Fraction`-exact
/// fallback for a quotient that overflows to infinity is not replicated
/// (an `f64` division overflowing while every value involved is still a
/// real, finite number) -- a narrow, documented residual gap (`DESIGN.md`
/// §13); an *already*-non-finite quotient (a `NaN` instance, or a zero
/// divisor) is treated as a violation, matching Python's own `bool(nan)
/// == True` by the same coincidence of IEEE-754 comparison rules `bool`
/// never needs to intervene for.
fn multiple_of_failed(instance: &Value, divisor: &Value) -> bool {
    if !matches!(instance, Value::Int(_) | Value::Float(_)) {
        return false;
    }
    if let (Value::Int(n), Value::Int(d)) = (instance, divisor) {
        let d = d.to_bigint();
        if d == num_bigint::BigInt::from(0) {
            return false;
        }
        return n.to_bigint() % d != num_bigint::BigInt::from(0);
    }
    let Some(i) = as_f64(instance) else {
        return false;
    };
    let Some(d) = as_f64(divisor) else {
        return false;
    };
    if matches!(divisor, Value::Int(_)) {
        // A `float` instance against an `int` divisor: Python's `%`
        // follows floor division (the result's own sign matches the
        // *divisor*'s), unlike Rust's `%` (truncated division, the
        // dividend's own sign) -- `py_fmod` corrects that.
        let remainder = py_fmod(i, d);
        return remainder != 0.0;
    }
    let quotient = i / d;
    if !quotient.is_finite() {
        return true;
    }
    quotient.trunc() != quotient
}

/// Python's own float `%`: floor division, sign of the result matches
/// the divisor's (`-1.0 % 3.0 == 2.0` in Python; Rust's own `%` gives
/// `-1.0`).
fn py_fmod(a: f64, b: f64) -> f64 {
    let r = a % b;
    if r != 0.0 && (r < 0.0) != (b < 0.0) {
        r + b
    } else {
        r
    }
}

fn as_i64(value: &Value) -> Option<i64> {
    match value {
        Value::Int(electricity_value::IntValue::Small(n)) => Some(*n),
        _ => None,
    }
}

fn compile_pattern(pattern: &str) -> Option<fancy_regex::Regex> {
    let normalized = rewrite_word_and_space_classes(&crate::rewrite_digit_class(
        &crate::rewrite_trailing_dollar(pattern),
    ));
    fancy_regex::Regex::new(&normalized).ok()
}

/// Every unescaped `\w`/`\W`/`\s`/`\S` token, narrowed/widened to match
/// Python `re`'s own Unicode tables exactly (issue #498's own orchestrator
/// follow-up, finding N3) -- `fancy-regex`'s own default `\w` also
/// matches a combining mark (`\p{M}`), which Python's own `str.isalnum()`
/// never does (`'e\u0301'` -- `e` plus a free-standing acute accent --
/// fails `^\w+$` in Python, passes it here otherwise); its own `\s`, in
/// turn, is missing the four ASCII separator controls (`\x1c`-`\x1f`)
/// CPython's own Unicode tables still class as whitespace for historical
/// reasons. `\b`/`\B` are left alone (`fancy-regex`'s own word-boundary
/// test does not consult a schema-provided override the way this
/// rewrite's `\w`/`\W` do, so it keeps whatever divergence its *own*
/// default has -- `DESIGN.md` §13).
fn rewrite_word_and_space_classes(pattern: &str) -> String {
    const WORD: &str = r"\p{L}\p{Nd}\p{Nl}\p{No}_";
    const SPACE: &str = r"\s\x1c-\x1f";
    let chars: Vec<char> = pattern.chars().collect();
    let mut out = String::with_capacity(pattern.len());
    let mut in_class = false;
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '\\' && i + 1 < chars.len() {
            // Only rewritten outside a character class: `[\w-]` needs
            // the class's own *members* spliced in unbracketed
            // (`[\p{L}...-]`), never a second, nested `[...]`, and
            // `\W`/`\S` can't be inlined that way inside one at all
            // (a negated class has no complement *within* another
            // class) -- left as `fancy-regex`'s own default there, a
            // narrower, documented residual gap than emitting invalid
            // syntax (`DESIGN.md` §13); no pattern in this engine's own
            // required corpus uses either inside a class.
            match (in_class, chars[i + 1]) {
                (false, 'w') => out.push_str(&format!("[{WORD}]")),
                (false, 'W') => out.push_str(&format!("[^{WORD}]")),
                (false, 's') => out.push_str(&format!("[{SPACE}]")),
                (false, 'S') => out.push_str(&format!("[^{SPACE}]")),
                (true, 'w') => out.push_str(WORD),
                (true, 's') => out.push_str(SPACE),
                (_, other) => {
                    out.push('\\');
                    out.push(other);
                }
            }
            i += 2;
            continue;
        }
        if chars[i] == '[' && !in_class {
            in_class = true;
        } else if chars[i] == ']' && in_class {
            in_class = false;
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Every keyword [`evaluate`] understands on its own, needing no
/// subschema recursion to confirm support for -- the leaf keywords plus
/// the pure-annotation no-ops Draft 2020-12 itself defines (`$defs`,
/// `$id`, `$anchor`, `$comment`, `title`, `description`, `default`,
/// `examples`, `deprecated`, `readOnly`, `writeOnly`, and every
/// `content*`/`format` keyword -- Python's own reference implementation
/// never validates `format` without an explicit `format_checker`, so it
/// is an annotation here too, issue #498 ruling 6's own module docs).
/// `minContains`/`maxContains` ride along with `contains`, never
/// meaningful alone, so they're listed as leaves too.
const SUPPORTED_LEAF_KEYWORDS: &[&str] = &[
    "type",
    "enum",
    "const",
    "required",
    "minItems",
    "maxItems",
    "minLength",
    "maxLength",
    "minProperties",
    "maxProperties",
    "minimum",
    "maximum",
    "exclusiveMinimum",
    "exclusiveMaximum",
    "multipleOf",
    "pattern",
    "uniqueItems",
    "minContains",
    "maxContains",
    "dependentRequired",
    "$defs",
    "definitions",
    "$id",
    "$anchor",
    "$comment",
    "title",
    "description",
    "default",
    "examples",
    "deprecated",
    "readOnly",
    "writeOnly",
    "format",
    "contentEncoding",
    "contentMediaType",
    "contentSchema",
];

/// Whether *schema* (and, recursively, every subschema it can reach --
/// `properties`/`patternProperties`/`additionalProperties`/`items`/
/// `prefixItems`/`allOf`/`anyOf`/`oneOf`/`not`/`if`/`then`/`else`/
/// `dependentSchemas`/`contains`/`propertyNames`, and whatever a `$ref`
/// resolves to) uses *only* keywords [`evaluate`] implements -- the
/// authority test issue #498's own orchestrator follow-up (ruling R6)
/// requires: this engine decides accept/reject itself only when this
/// returns `true`, never the `jsonschema` crate (whose own lossy
/// `serde_json`/ECMA-regex/format-by-default behavior is exactly what
/// the follow-up's own probes caught diverging from Python). `depth`
/// guards against a self-referencing `$ref` the same way [`evaluate`]'s
/// own [`RECURSION_DEPTH`] does, independently (this function never
/// calls `evaluate`, so it needs its own budget) -- a schema that deep
/// or cyclic is simply judged unsupported, which safely routes it to
/// the crate-authority path (crate-vs-Python divergence on a document
/// this pathological is the least of its problems).
pub(crate) fn schema_is_fully_supported(schema: &Value, root: &Value, depth: u32) -> bool {
    if depth > MAX_RECURSION_DEPTH {
        // The same signal `evaluate`'s own `DepthGuard` raises: a
        // schema this deep is, for every real document, a
        // self-referencing `$ref` -- and the crate itself (unlike
        // Python) can resolve one gracefully without erroring, which
        // would otherwise mean `validate_prompt_schema`'s own
        // crate-authority fallback silently gives a *different*,
        // non-crashing-but-wrong verdict instead of matching Python's
        // own uncaught `RecursionError` here (issue #498's own
        // orchestrator follow-up, finding N6).
        RECURSION_EXCEEDED.with(|e| e.set(true));
        return false;
    }
    match schema {
        Value::Bool(_) => true,
        Value::Dict(dict) => dict.iter().all(|(k, v)| {
            let Some(key_str) = k.as_str() else {
                return false;
            };
            if SUPPORTED_LEAF_KEYWORDS.contains(&key_str) {
                return true;
            }
            match key_str {
                "$ref" => v
                    .as_str()
                    .and_then(|pointer| resolve_ref(root, pointer))
                    .is_some_and(|resolved| schema_is_fully_supported(resolved, root, depth + 1)),
                "properties" | "patternProperties" | "dependentSchemas" => {
                    v.as_dict().is_some_and(|d| {
                        d.values()
                            .all(|sub| schema_is_fully_supported(sub, root, depth + 1))
                    })
                }
                "additionalProperties" | "items" | "not" | "propertyNames" | "contains" => {
                    schema_is_fully_supported(v, root, depth + 1)
                }
                "if" => {
                    schema_is_fully_supported(v, root, depth + 1)
                        && dict
                            .get(&key("then"))
                            .is_none_or(|t| schema_is_fully_supported(t, root, depth + 1))
                        && dict
                            .get(&key("else"))
                            .is_none_or(|e| schema_is_fully_supported(e, root, depth + 1))
                }
                "then" | "else" => true, // handled alongside "if" above
                "prefixItems" | "allOf" | "anyOf" | "oneOf" => v.as_list().is_some_and(|items| {
                    items
                        .iter()
                        .all(|sub| schema_is_fully_supported(sub, root, depth + 1))
                }),
                _ => false,
            }
        }),
        _ => false,
    }
}

/// Whether *instance* validates against *branch* at all. Prefers this
/// module's own [`is_valid`] directly on the *unresolved* branch (whose
/// own `evaluate` already understands `$ref`, siblings included, exactly
/// as Python's own `_validate_reference` applicator does) whenever
/// [`schema_is_fully_supported`] judges the branch trustworthy; only
/// then falls back to the `jsonschema` crate (format checking off,
/// matching Python never validating `format` without an explicit
/// checker) -- never the other way around, and never by compiling the
/// branch as its own standalone document (issue #498's own orchestrator
/// follow-up, finding N1: a `$ref` nested inside a branch, or a `$ref`
/// with sibling keywords, used to be judged by a *freshly compiled*,
/// branch-only validator that either couldn't resolve the ref at all or
/// silently dropped the siblings).
fn schema_accepts(branch: &Value, instance: &Value, root: &Value) -> bool {
    if schema_is_fully_supported(branch, root, 0) {
        return is_valid(branch, instance, root);
    }
    let resolved = resolve_schema_refs(branch, root);
    let branch_json = crate::value_to_json(&resolved);
    let instance_json = crate::value_to_json(instance);
    match jsonschema::options()
        .should_validate_formats(false)
        .build(&branch_json)
    {
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
        if let Ok(validator) = jsonschema::options()
            .should_validate_formats(false)
            .build(&branch_json)
        {
            if let Err(err) = validator.validate(&instance_json) {
                // Not `crate::substitute_value_prefix` -- that's
                // `core.document_check`'s own compile-time convention,
                // never `core/prompt.py`'s own raw `e.message` (finding
                // N10, same as `validate_prompt_schema`'s own fallback).
                let message = err.to_string();
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
        Kind::Not { not_schema } => {
            format!(
                "{} should not be valid under {}",
                instance.py_repr(),
                not_schema.py_repr()
            )
        }
        Kind::DependentRequired { each, property } => format!(
            "{} is a dependency of {}",
            Value::Str(each.to_string()).py_repr(),
            Value::Str(property.to_string()).py_repr()
        ),
        Kind::ContainsNone => format!(
            "{} does not contain items matching the given schema",
            instance.py_repr()
        ),
        Kind::MinContains {
            min_contains,
            matches,
        } => format!(
            "Too few items match the given schema (expected at least {min_contains} but only \
             {matches} matched)"
        ),
        Kind::MaxContains { max_contains } => {
            format!("Too many items match the given schema (expected at most {max_contains})")
        }
        Kind::ItemsFalse {
            prefix,
            extra,
            item_word,
            rest_repr,
        } => format!("Expected at most {prefix} {item_word} but found {extra} extra: {rest_repr}"),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Issue #498's own orchestrator follow-up, finding N11: the
    /// brute-force `O(n^2)` `uniqueItems` check this module used to run
    /// took several seconds on a 20,000-item array in a debug build --
    /// unusable on anything realistic. [`py_uniq`]'s own hash-bucketed
    /// fast path must stay well under a second even at a much larger
    /// size.
    #[test]
    fn uniq_items_stays_fast_at_scale() {
        let items: Vec<Value> = (0..50_000).map(|i| Value::from(i as i64)).collect();
        let start = std::time::Instant::now();
        assert!(py_uniq(&items));
        assert!(
            start.elapsed().as_secs() < 2,
            "py_uniq(50_000 unique items) took {:?}",
            start.elapsed()
        );

        let mut with_duplicate = items.clone();
        with_duplicate.push(Value::from(1i64));
        assert!(!py_uniq(&with_duplicate));
    }

    /// A `NaN` is never a duplicate of anything, including a `NaN` with
    /// the exact same bit pattern (`py_equal`'s own rule, which a hash
    /// key would otherwise violate -- [`uniq_key`]'s own doc comment).
    #[test]
    fn uniq_items_never_dedupes_nan() {
        let items = vec![Value::Float(f64::NAN), Value::Float(f64::NAN)];
        assert!(py_uniq(&items));
    }

    /// Issue #498's own orchestrator follow-up, finding N11: `anyOf`
    /// branch validity used to compile a fresh `jsonschema`-crate
    /// validator per branch per instance node -- `schema_accepts`'s own
    /// engine-authority fast path (no crate compile at all for a fully
    /// supported branch) must stay well under a second even with many
    /// branches.
    #[test]
    fn any_of_with_many_branches_stays_fast() {
        let mut branches = Vec::new();
        for i in 0..2000 {
            let mut schema = Dict::new();
            schema.insert(key("const"), Value::from(i as i64));
            branches.push(Value::Dict(schema));
        }
        let mut schema = Dict::new();
        schema.insert(key("anyOf"), Value::List(branches));
        let schema = Value::Dict(schema);
        let instance = Value::from(1999i64);

        let start = std::time::Instant::now();
        let mut violations = Vec::new();
        evaluate(&schema, &instance, &schema, &[], &mut violations);
        assert!(violations.is_empty(), "the last branch should have matched");
        assert!(
            start.elapsed().as_secs() < 2,
            "anyOf over 2000 branches took {:?}",
            start.elapsed()
        );
    }
}
