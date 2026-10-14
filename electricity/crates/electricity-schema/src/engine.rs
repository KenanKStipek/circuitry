//! A hand-rolled JSON-Schema violation collector, operating directly on
//! order-preserving `electricity_value::Value` trees rather than on
//! `serde_json::Value` (a `BTreeMap`-backed `Map` this workspace never turns
//! `preserve_order` on for -- see `electricity-vm/Cargo.toml`'s own
//! dev-dependency comment) -- issue #498 ruling 6, tightened twice over by
//! PR #502's review (finding 2) and follow-up probes.
//!
//! This module (via [`schema_is_fully_supported`]) is the accept/reject
//! *authority* whenever a schema uses only the keywords it implements and
//! the draft is 2020-12 or unstated -- `validate_prompt_schema` never
//! builds or even consults the `jsonschema` Rust crate (0.26) on that path
//! at all (PR #502's round-4 fix, finding F5: the crate's own build step
//! resolves a `$ref` eagerly where Python resolves lazily, and can't
//! represent a schema-literal `NaN`/`Infinity`, so either used to produce
//! a false `SchemaError` or a wrong verdict for a schema this module
//! understands perfectly well). The crate decides only when that gate is
//! false (an unsupported keyword, or an older draft) -- including every
//! `anyOf`/`oneOf` *branch* on that path, since this module itself never
//! finished the top-level walk that would let it trust its own verdict.
//! Once the gate holds, [`schema_accepts`]/[`evaluate_branch`] decide
//! every branch too (PR #502's round-6 fix): the crate's own
//! `serde_json::Value`-based error objects were never a good *text*
//! source anyway -- they lose the schema author's own key order (so a
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
//! Deliberately narrow, but wider than it looks: every ordinary 2020-12
//! validation keyword is understood, `allOf`/`not`/`if`/`then`/`else`/
//! `dependentRequired`/`dependentSchemas`/`contains`/`propertyNames`/
//! `additionalProperties`-as-a-subschema included -- [`schema_is_fully_
//! supported`]'s own module docs name exactly what isn't (`$dynamicRef`/
//! `$dynamicAnchor`/`unevaluatedProperties`/`unevaluatedItems`, a nested
//! non-root `$id`, the Draft 7 tuple form of `items`). A keyword Python's
//! own `Draft202012Validator.VALIDATORS` dict doesn't implement at all
//! (`dependencies`, a stray typo, ...) is a plain annotation here too,
//! never a reason to fall back to the crate (PR #502's round-4 fix,
//! finding F4) -- [`evaluate`] simply finds nothing for it, the same way
//! Python's own validator never would have either.

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
        is_one: bool,
    },
    MaxItems {
        is_zero: bool,
    },
    MinLength {
        is_one: bool,
    },
    MaxLength {
        is_zero: bool,
    },
    MinProperties {
        is_one: bool,
    },
    MaxProperties {
        is_zero: bool,
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
        min_contains: &'a Value,
        matches: i64,
    },
    MaxContains {
        max_contains: &'a Value,
    },
    ItemsFalse {
        prefix: usize,
        extra: usize,
        item_word: &'static str,
        rest_repr: String,
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

/// A finite `f64`'s own exact value as a reduced `(numerator,
/// denominator)` pair (`denominator` always a power of two, `1` whenever
/// *f* happens to be a whole number) -- every finite `f64` is exactly
/// representable this way (`mantissa * 2^exponent`, IEEE-754's own
/// definition), so this never loses precision the way routing a `BigInt`
/// through `f64` (or vice versa) would. `None` for `NaN`/`Infinity`
/// (callers compare those by case, never through a ratio). Used by
/// [`cmp_bigint_f64`] and [`uniq_key`] so an `Int`-`Float` comparison or
/// hash bucket is exact at any magnitude (issue #498's own
/// follow-up, finding F9: `9007199254740993 > 9007199254740992.0` must
/// stay `Greater`, not silently round to `Equal`).
fn float_exact_ratio(f: f64) -> Option<(num_bigint::BigInt, num_bigint::BigInt)> {
    if !f.is_finite() {
        return None;
    }
    if f == 0.0 {
        return Some((num_bigint::BigInt::from(0), num_bigint::BigInt::from(1)));
    }
    let bits = f.to_bits();
    let sign: i64 = if (bits >> 63) & 1 == 1 { -1 } else { 1 };
    let biased_exp = ((bits >> 52) & 0x7ff) as i64;
    let mantissa_bits = bits & 0x000f_ffff_ffff_ffff;
    let (mut mantissa, mut exp2): (u64, i64) = if biased_exp == 0 {
        (mantissa_bits, -1074)
    } else {
        (mantissa_bits | (1u64 << 52), biased_exp - 1075)
    };
    while mantissa & 1 == 0 {
        mantissa >>= 1;
        exp2 += 1;
    }
    let mantissa_bi = num_bigint::BigInt::from(mantissa) * sign;
    if exp2 >= 0 {
        Some((mantissa_bi << exp2 as u32, num_bigint::BigInt::from(1)))
    } else {
        Some((mantissa_bi, num_bigint::BigInt::from(1) << (-exp2) as u32))
    }
}

/// `n.cmp(f)` exactly, at any magnitude: `+Infinity`/`-Infinity` compare
/// greater/less than every finite `n`; `NaN` compares to nothing (`None`,
/// matching every other `NaN` comparison in this module). A finite *f*
/// goes through [`float_exact_ratio`] and a cross-multiplication, never
/// an `f64` round trip.
fn cmp_bigint_f64(n: &num_bigint::BigInt, f: f64) -> Option<std::cmp::Ordering> {
    if f.is_nan() {
        return None;
    }
    if f.is_infinite() {
        return Some(if f > 0.0 {
            std::cmp::Ordering::Less
        } else {
            std::cmp::Ordering::Greater
        });
    }
    let (num, den) = float_exact_ratio(f).expect("finite f64 always has an exact ratio");
    Some((n * &den).cmp(&num))
}

/// `jsonschema._utils.equal`: evades Python's own type hierarchy (`bool`
/// is an `int` subclass) by special-casing it, and never equates a `str`
/// with anything but another `str` (unlike `_sequence_equal`, which would
/// otherwise iterate a string's own characters as if it were a plain
/// sequence). `None`/numeric comparison falls through to plain `==`.
/// Numeric cross-type comparison (`Int` vs `Float`) is exact at any
/// magnitude via [`cmp_bigint_f64`] -- Python's own `int`-`float` `==` is
/// exact too, the same reason [`numeric_cmp`] needs it (issue #498's own
/// follow-up, finding F9).
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
        (Value::Int(a), Value::Float(b)) | (Value::Float(b), Value::Int(a)) => {
            cmp_bigint_f64(&a.to_bigint(), *b) == Some(std::cmp::Ordering::Equal)
        }
        (Value::Float(a), Value::Float(b)) => a == b,
        _ => false,
    }
}

/// A hashable stand-in for [`py_equal`]'s own equivalence classes --
/// `Hash`/`Eq` agree with it exactly (unlike `electricity_value::Value`'s
/// own derived ones, which deliberately let `True`/`1`/`1.0` collide as
/// Python dict keys do -- the opposite of what `jsonschema._utils.equal`
/// wants here). `Number` is an exact, fully-reduced `(numerator,
/// denominator)` ratio ([`float_exact_ratio`]) so `1`/`1.0`/`10**19`/
/// `1e19` all collide exactly, at any magnitude, the same way Python's
/// own `hash(1) == hash(1.0) == hash(10**19) == hash(1e19)` invariant
/// does (issue #498's own follow-up, finding F8).
///
/// `Nan`/`Inf` are their own buckets, deliberately *not* folded into
/// `Number`: CPython's own `json` C scanner hands back one shared `NaN`/
/// `Infinity`/`-Infinity` singleton object for every occurrence of that
/// literal reached by *one* `json.loads` call (confirmed directly), so
/// `_utils.equal`'s `one is two` short-circuit makes every `NaN` in one
/// decoded instance a duplicate of every other `NaN` there (`uniqueItems`
/// specifically -- never `py_equal`'s own general-purpose comparison,
/// e.g. a schema's own `const`/`enum` literal parsed from a wholly
/// separate YAML document is never `is`-identical to anything an
/// instance decodes, so that path stays `NaN != NaN` as plain `f64`
/// equality already gives).
#[derive(PartialEq, Eq, Hash)]
enum UniqKey {
    Str(String),
    Bool(bool),
    None,
    Nan,
    Inf(bool),
    Number(num_bigint::BigInt, num_bigint::BigInt),
}

fn uniq_key(value: &Value) -> Option<UniqKey> {
    match value {
        Value::Str(s) => Some(UniqKey::Str(s.clone())),
        Value::Bool(b) => Some(UniqKey::Bool(*b)),
        Value::None => Some(UniqKey::None),
        Value::Int(i) => Some(UniqKey::Number(i.to_bigint(), num_bigint::BigInt::from(1))),
        Value::Float(f) if f.is_nan() => Some(UniqKey::Nan),
        Value::Float(f) if f.is_infinite() => Some(UniqKey::Inf(*f < 0.0)),
        Value::Float(f) => float_exact_ratio(*f).map(|(n, d)| UniqKey::Number(n, d)),
        // Every container type falls back to `py_uniq`'s own
        // brute-force bucket below -- container equality is never by
        // hash here.
        _ => None,
    }
}

/// `jsonschema._utils.uniq`: every hashable scalar (everything but a
/// `List`/`Dict`) goes through [`uniq_key`]'s own `O(n)` duplicate-count
/// pass -- the one that actually matters for a large array, per issue
/// #498's own follow-up, finding N11 (`O(n^2)` over 50k
/// items took several seconds in a debug build; this brings it back to
/// milliseconds). A `List`/`Dict` element falls to
/// [`containers_have_a_duplicate`], which -- unlike a naive all-pairs
/// `equal()` loop -- replicates Python's own two-phase algorithm
/// exactly, including its one real quirk (issue #498's own
/// follow-up, finding F8).
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
    !containers_have_a_duplicate(&unhashable)
}

/// Python's own plain `<`/`==` ordering -- *never* [`py_equal`]'s own
/// `bool`-vs-`int` distinction (`_utils.unbool`'s own sentinel trick
/// only ever intercepts a *bare* `True`/`False` at the container's own
/// top level, never one buried inside a nested list, so list comparison
/// itself stays plain: `[1] == [True]` compares equal here, the same as
/// Python's own `list.__eq__`). `None` wherever Python's own `<` would
/// raise `TypeError`: comparing anything against a `dict` (genuinely
/// unorderable), or two values from incompatible type families (a
/// `str` against a number, a `list` against anything but another
/// `list`, ...). The numeric tower (`bool`/`int`/`float`) is exact at
/// any magnitude, the same reason [`numeric_cmp`] is.
fn py_sort_cmp(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Value::Dict(_), _) | (_, Value::Dict(_)) => None,
        (Value::None, Value::None) => Some(std::cmp::Ordering::Equal),
        (Value::None, _) | (_, Value::None) => None,
        (Value::Str(x), Value::Str(y)) => Some(x.cmp(y)),
        (Value::Str(_), _) | (_, Value::Str(_)) => None,
        (Value::List(x), Value::List(y)) => {
            for (xi, yi) in x.iter().zip(y.iter()) {
                match py_sort_cmp(xi, yi)? {
                    std::cmp::Ordering::Equal => continue,
                    other => return Some(other),
                }
            }
            Some(x.len().cmp(&y.len()))
        }
        (Value::List(_), _) | (_, Value::List(_)) => None,
        (
            Value::Bool(_) | Value::Int(_) | Value::Float(_),
            Value::Bool(_) | Value::Int(_) | Value::Float(_),
        ) => numeric_cmp(&as_numeric(a), &as_numeric(b)),
        (Value::Bytes(_) | Value::Date(_) | Value::DateTime(_, _), _)
        | (_, Value::Bytes(_) | Value::Date(_) | Value::DateTime(_, _)) => None,
    }
}

/// `bool` participates in Python's own numeric tower for ordering
/// purposes (`True < 2`, `False == 0`) -- unlike [`py_equal`]'s own
/// deliberately bool-distinguishing rule, matched here by converting it
/// to a plain `0`/`1` `Int` first.
fn as_numeric(value: &Value) -> Value {
    match value {
        Value::Bool(b) => Value::Int(electricity_value::IntValue::Small(*b as i64)),
        other => other.clone(),
    }
}

/// [`py_sort_cmp`], except a `NaN` on either side is a tie (`Some(
/// Equal)`), never incomparable -- Python's own `float.__lt__` never
/// raises for `NaN` (every comparison with it is simply `False`,
/// unlike a genuine `TypeError` from e.g. a `dict`), so CPython's own
/// sort never aborts over one; only [`uniq_equal`]'s own adjacency
/// check gets to decide whether two `NaN`s collide (issue #498's own
/// follow-up, NaN uniqueness edges).
fn uniq_sort_cmp(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    if let (Value::Float(x), Value::Float(y)) = (a, b) {
        if x.is_nan() || y.is_nan() {
            return Some(std::cmp::Ordering::Equal);
        }
    }
    if let (Value::List(x), Value::List(y)) = (a, b) {
        for (xi, yi) in x.iter().zip(y.iter()) {
            match uniq_sort_cmp(xi, yi)? {
                std::cmp::Ordering::Equal => continue,
                other => return Some(other),
            }
        }
        return Some(x.len().cmp(&y.len()));
    }
    py_sort_cmp(a, b)
}

/// [`py_equal`], except two `NaN`s are always equal -- within *one*
/// decoded instance, every `NaN` literal is CPython's own `json`
/// scanner's shared singleton object ([`UniqKey::Nan`]'s own doc
/// comment), so `_utils.equal`'s `one is two` identity check (checked
/// *before* any type-specific comparison, at every recursion level --
/// `_sequence_equal`/`_mapping_equal` both recurse through `equal`
/// itself, never a bare `==`) makes two `NaN`s nested at any depth
/// collide too, e.g. `[[nan], [nan]]` (confirmed directly against
/// Python). [`py_equal`] itself must stay narrower: a schema's own
/// `const`/`enum` literal (parsed from a wholly separate document) is
/// never identity-equal to anything an instance decodes, so that
/// comparison correctly keeps `NaN != NaN` as plain `f64` equality
/// already gives -- only *this* uniqueness-only path gets the identity
/// treatment.
fn uniq_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Float(x), Value::Float(y)) if x.is_nan() && y.is_nan() => true,
        (Value::List(x), Value::List(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| uniq_equal(p, q))
        }
        (Value::Dict(x), Value::Dict(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|v2| uniq_equal(v, v2)))
        }
        _ => py_equal(a, b),
    }
}

/// Simulates CPython `listsort`'s own `count_run` + `binarysort` --
/// together, the *entire* algorithm it uses for a list shorter than
/// `merge_compute_minrun`'s own threshold (64; `minrun` is the list's
/// own length below that, so there is only ever one "run" to begin
/// with) -- over *order* (indices into *items*), comparing with
/// [`uniq_sort_cmp`]. `count_run` first finds *order*'s own natural
/// ascending (ties continue it) or strictly-descending (ties break it)
/// prefix, reversing it in place if descending; `binarysort` then
/// inserts every remaining element with a bisection search exactly like
/// `bisect_right` (ties move *right*, preserving stability) and an
/// in-place rotation. Returns `false` -- *order* left in an unspecified
/// state, matching `_utils.uniq`'s own fallback-restarts-from-the-
/// original-container semantics, so no caller may look at a partial
/// sort -- the instant any comparison is incomparable (`None`), CPython's
/// own uncaught `TypeError` unwinding the whole `sorted()` call.
///
/// For a list of 64 or more, real timsort instead finds and merges
/// several shorter runs with a galloping merge; this keeps the same
/// single-run algorithm there too -- correct (any genuine total order
/// still ends up fully, stably sorted), just not bit-for-bit identical
/// to CPython's own comparison *sequence* that large (a documented,
/// narrower divergence, `DESIGN.md` §13) -- rather than ever risk the
/// transitivity-violation panic `[T]::sort_by` itself can raise when a
/// comparator isn't a genuine total order, the way mapping
/// "incomparable" to `Equal` used to (issue #498's own
/// follow-up, finding C1).
fn cpython_list_sort(order: &mut [usize], items: &[&Value]) -> bool {
    let n = order.len();
    if n < 2 {
        return true;
    }
    let descending = match uniq_sort_cmp(items[order[1]], items[order[0]]) {
        None => return false,
        Some(std::cmp::Ordering::Less) => true,
        _ => false,
    };
    let mut run_len = 2;
    while run_len < n {
        let cmp = match uniq_sort_cmp(items[order[run_len]], items[order[run_len - 1]]) {
            None => return false,
            Some(c) => c,
        };
        let continues = if descending {
            cmp == std::cmp::Ordering::Less
        } else {
            cmp != std::cmp::Ordering::Less
        };
        if !continues {
            break;
        }
        run_len += 1;
    }
    if descending {
        order[..run_len].reverse();
    }
    for i in run_len..n {
        let pivot = order[i];
        let mut lo = 0usize;
        let mut hi = i;
        while lo < hi {
            let mid = (lo + hi) / 2;
            match uniq_sort_cmp(items[pivot], items[order[mid]]) {
                None => return false,
                Some(std::cmp::Ordering::Less) => hi = mid,
                _ => lo = mid + 1,
            }
        }
        order[lo..=i].rotate_right(1);
    }
    true
}

fn containers_have_a_duplicate(items: &[&Value]) -> bool {
    let mut order: Vec<usize> = (0..items.len()).collect();
    if cpython_list_sort(&mut order, items) {
        return order
            .windows(2)
            .any(|w| uniq_equal(items[w[0]], items[w[1]]));
    }
    for i in 0..items.len() {
        for j in 0..i {
            if uniq_equal(items[j], items[i]) {
                return true;
            }
        }
    }
    false
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
///
/// `referencing`'s own `Resource.pointer` unquotes the *whole* remaining
/// fragment (everything after the pointer's own leading `/`) in one pass
/// first, and only *then* splits on `/` and unescapes each segment's own
/// `~1`/`~0` -- never per-segment, which would wrongly treat a literal
/// `%2F` (an escaped `/`) inside one logical segment as a fresh path
/// separator (issue #498's own follow-up, finding C2).
fn resolve_ref<'a>(root: &'a Value, pointer: &str) -> Option<&'a Value> {
    let fragment = pointer.strip_prefix('#')?;
    if fragment.is_empty() {
        return Some(root);
    }
    let rest = fragment.strip_prefix('/')?;
    let decoded = percent_decode(rest);
    let mut node = root;
    for raw_segment in decoded.split('/') {
        let segment = raw_segment.replace("~1", "/").replace("~0", "~");
        node = match node {
            Value::Dict(d) => get(d, &segment)?,
            Value::List(items) => items.get(segment.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(node)
}

/// A `$ref` the walk above actually reaches (lazily, [`evaluate`]'s own
/// "$ref" arm) but [`resolve_ref`] can't resolve is Python's own
/// `referencing.exceptions.PointerToNowhere`, wrapped by `jsonschema`'s
/// own `_WrappedReferencingError.__str__` into `f"{type(cause).__name__}:
/// {cause}"` -- never caught by `core/prompt.py::_validate_schema`'s own
/// `except jsonschema.ValidationError` (a `referencing` exception isn't
/// one), so it reaches the attempt loop's generic `except Exception` as a
/// non-retryable, generic failure, the same shape
/// [`crate::take_python_exception`] already gives a `RecursionError`
/// (issue #498's own follow-up, finding V1). *pointer* only
/// ever reaches here already filtered to [`is_same_document_pointer_syntax`]
/// by every caller, so `ref` -- `pointer` with its leading `#` stripped,
/// keeping the leading `/` -- is always `referencing`'s own `pointer`
/// argument exactly (confirmed directly: for `"#"` itself `resolve_ref`
/// always returns `Some`, so this is never reached with an empty `ref`).
fn resolve_ref_or_raise<'a>(root: &'a Value, pointer: &str) -> Option<&'a Value> {
    let resolved = resolve_ref(root, pointer);
    if resolved.is_none() {
        let fragment = pointer.strip_prefix('#').unwrap_or(pointer);
        raise_python_exception(format!(
            "PointerToNowhere: {} does not exist within {}",
            Value::Str(fragment.to_string()).py_repr(),
            root.py_repr(),
        ));
    }
    resolved
}

/// A minimal `%XX`-hex URI-percent-decoder -- every byte that isn't a
/// well-formed `%` escape passes through unchanged (a real JSON Pointer
/// fragment is never full of literal, un-encoded `%` signs, so this
/// never needs to be stricter than that). Decodes raw bytes throughout,
/// never slicing the original `&str` by a byte offset that could land
/// inside a multi-byte UTF-8 character (issue #498's own
/// follow-up, finding C2: a `%` immediately followed by one was exactly
/// `#/$defs/%1\u{e9}` -- `\u{e9}`'s own second byte sat right where the
/// old `&segment[i + 1..i + 3]` slice end fell, panicking).
fn percent_decode(segment: &str) -> String {
    fn hex_val(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    let bytes = segment.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push(hi * 16 + lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| segment.to_string())
}

thread_local! {
    /// A global (thread-local, since `evaluate` never actually runs
    /// concurrently with itself on one thread -- it's a plain
    /// synchronous call tree with no `.await` inside it) stack of every
    /// *active* `evaluate` frame's own `(schema, instance)` node-identity
    /// pair, pushed on entry and popped on return -- real cycle
    /// detection, not a plain depth count. A finite, growing instance
    /// (a tree, a linked list, ... each `$ref`-recursion step landing on
    /// a strictly deeper instance node) never repeats a pair here no
    /// matter how deep it goes; a true cycle (`$defs: {loop: {$ref:
    /// '#/$defs/loop'}}` chasing the *same* instance node every step, or
    /// the degenerate `{"$ref": "#"}` landing on literally the same
    /// schema node too) revisits one almost immediately. Rust has no
    /// `RecursionError` to catch a cycle like that -- left unchecked it's
    /// a stack overflow, an immediate process abort with no state at all
    /// (issue #498's own follow-up, finding N6; PR #502's
    /// own round-4 fix, finding F1).
    static EVAL_STACK: std::cell::RefCell<std::collections::HashSet<(usize, usize)>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
    /// Set once a cycle is detected, or the safety cap below is hit,
    /// during a call; [`crate::validate_prompt_schema`] checks and
    /// clears it after calling [`evaluate`]/[`best_match`], the same way
    /// Python's own uncaught `RecursionError` would reach `core/
    /// prompt.py`'s own generic `except Exception` (never `except
    /// jsonschema.ValidationError`) and record a `"failed"`,
    /// non-retryable attempt with that exception's own text -- not
    /// `schema_invalid`.
    pub(crate) static RECURSION_EXCEEDED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// A safety net for instance nesting *without* a cycle (so [`EVAL_STACK`]
/// alone never bounds it): comfortably past `electricity_value::MAX_DEPTH`
/// (512, the JSON decoder's own ceiling, so no real document's instance can
/// ever nest deeper than that) times the handful of `evaluate` frames one
/// level of a recursive-schema shape like a tree or linked list needs,
/// comfortably short of where Rust's own real call-stack limit would bite
/// first (`electricity-schema` builds at `opt-level = 2` even in the dev
/// profile -- workspace `Cargo.toml` -- specifically so this margin holds
/// in an unoptimized/debug test build too, not just `--release`).
const MAX_RECURSION_DEPTH: usize = 4096;

struct EvalGuard(Option<(usize, usize)>);

impl Drop for EvalGuard {
    fn drop(&mut self) {
        if let Some(key) = self.0 {
            EVAL_STACK.with(|s| {
                s.borrow_mut().remove(&key);
            });
        }
    }
}

/// A `HashSet`, not the `Vec` this used to be (PR #502's round-4 fix,
/// finding F12): a per-level linear scan over an already-O(depth)-sized
/// stack made the *cycle check itself* O(depth) on every single
/// `evaluate` call, needlessly squaring the cost of an otherwise-linear
/// deep recursion.
fn enter_eval_guard(schema: &Value, instance: &Value) -> (EvalGuard, bool) {
    let key = (
        schema as *const Value as usize,
        instance as *const Value as usize,
    );
    let (cycle, depth) = EVAL_STACK.with(|s| {
        let stack = s.borrow();
        (stack.contains(&key), stack.len())
    });
    if cycle || depth >= MAX_RECURSION_DEPTH || stack_budget_exceeded() {
        RECURSION_EXCEEDED.with(|e| e.set(true));
        return (EvalGuard(None), true);
    }
    EVAL_STACK.with(|s| s.borrow_mut().insert(key));
    (EvalGuard(Some(key)), false)
}

/// Clears [`RECURSION_EXCEEDED`] and returns its own prior value --
/// [`crate::validate_prompt_schema`]'s own first step, since the flag is
/// thread-local *state*, not a return value, and must never leak from
/// one validation attempt into the next one on the same thread.
pub(crate) fn take_recursion_exceeded() -> bool {
    RECURSION_EXCEEDED.with(|e| e.replace(false))
}

/// Reads [`RECURSION_EXCEEDED`] without clearing it -- unlike
/// [`take_recursion_exceeded`], safe to call mid-walk:
/// [`crate::validate_prompt_schema`] uses this right after its own
/// [`schema_is_fully_supported`] call to tell a genuine hop-cap trip
/// (the schema's own `$ref` graph is already too deep for this engine to
/// finish walking) apart from an ordinary "not fully supported" `false`
/// (an unimplemented keyword) *before* deciding whether to fall back to
/// the `jsonschema` crate at all -- the crate's own build step resolves
/// a `$ref` eagerly, so handing it a schema already known to be this deep
/// would just move the same stack-overflow risk into code this module
/// can't put a guard in.
pub(crate) fn recursion_exceeded_peek() -> bool {
    RECURSION_EXCEEDED.with(|e| e.get())
}

thread_local! {
    /// The address of a stack-local probe taken once per validation
    /// call, by [`set_stack_guard`], at the top of the frame the
    /// dedicated validation thread (or, on a `spawn_scoped` failure, the
    /// fallback inline path) starts running `evaluate`/
    /// `schema_is_fully_supported_inner` from -- `0` means no guard is
    /// configured (a unit test calling either function directly, never
    /// through [`crate::validate_prompt_schema`]), in which case
    /// [`stack_budget_exceeded`] never trips.
    static STACK_GUARD_BASE: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// How many bytes below [`STACK_GUARD_BASE`] this call is allowed to
    /// use before [`stack_budget_exceeded`] trips -- set alongside it by
    /// [`set_stack_guard`], sized to the *actual* stack the current
    /// thread was given (generous on the dedicated validation thread,
    /// conservative on the fallback inline path), not to
    /// [`MAX_RECURSION_DEPTH`]'s own hop count. Defense in depth: a
    /// per-hop frame shape costing more stack than the hop budget ever
    /// measured against trips this before it could overflow the thread's
    /// real stack, regardless of which recursive path (or how many of
    /// them nested together) got there.
    static STACK_GUARD_BUDGET: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// [`crate::validate_prompt_schema`]'s own first step (after the
/// thread-local clears above), run on whichever thread is about to call
/// into [`evaluate`]/[`schema_is_fully_supported`] -- a stack-probe
/// address taken *here*, not by the caller, since it must be a local in
/// the same frame this validation call's own recursion actually grows
/// down from.
pub(crate) fn set_stack_guard(budget_bytes: usize) {
    let probe = 0u8;
    STACK_GUARD_BASE.with(|b| b.set(std::ptr::addr_of!(probe) as usize));
    STACK_GUARD_BUDGET.with(|b| b.set(budget_bytes));
}

/// Whether the current thread has used up its [`STACK_GUARD_BUDGET`]
/// since [`set_stack_guard`] was last called -- stack grows down on
/// every target this workspace builds for, so a *smaller* current probe
/// address than [`STACK_GUARD_BASE`] means more bytes used, never fewer.
fn stack_budget_exceeded() -> bool {
    let base = STACK_GUARD_BASE.with(|b| b.get());
    if base == 0 {
        return false;
    }
    let probe = 0u8;
    let current = std::ptr::addr_of!(probe) as usize;
    let used = base.saturating_sub(current);
    STACK_GUARD_BUDGET.with(|b| used >= b.get())
}

thread_local! {
    /// A pending, uncaught Python exception's own text -- set from deep
    /// inside [`evaluate`] (so far, only [`multiple_of_failed`]'s own
    /// `NaN`/`Infinity`-instance-against-a-`float`-divisor case) for a
    /// condition Python itself never reaches as an ordinary
    /// `ValidationError` at all. Keeps the *first* one set, matching
    /// Python's own single-exception-then-stop semantics for a
    /// depth-first walk this engine's own traversal doesn't abort
    /// early for (harmless: whatever [`evaluate`] goes on to collect
    /// afterward is discarded the same way [`RECURSION_EXCEEDED`]'s own
    /// already is, once this is set at all).
    static PENDING_PY_EXCEPTION: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

fn raise_python_exception(message: impl Into<String>) {
    PENDING_PY_EXCEPTION.with(|e| {
        let mut slot = e.borrow_mut();
        if slot.is_none() {
            *slot = Some(message.into());
        }
    });
}

/// Clears [`PENDING_PY_EXCEPTION`] and returns its own prior value --
/// [`crate::validate_prompt_schema`]'s own first step, the same reason
/// [`take_recursion_exceeded`] does.
pub(crate) fn take_python_exception() -> Option<String> {
    PENDING_PY_EXCEPTION.with(|e| e.borrow_mut().take())
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
    let (_eval_guard, exceeded) = enter_eval_guard(schema, instance);
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
                    if let Some(resolved) = resolve_ref_or_raise(root, pointer) {
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
                    for (prop_key, prop_instance) in obj.iter() {
                        let Some(name) = prop_key.as_str() else {
                            continue;
                        };
                        if pattern_is_match(pattern, name) {
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
                // follow-up, finding N8).
                let has_pattern_properties = schema
                    .as_dict()
                    .is_some_and(|d| d.contains_key(&key("patternProperties")));
                let properties = schema
                    .as_dict()
                    .and_then(|d| get(d, "properties"))
                    .and_then(Value::as_dict);
                // `_utils.find_additional_properties`: `"|".join(
                // schema.get("patternProperties", {}))` -- one *joined*
                // pattern, compiled (and `re.search`ed) once, never each
                // key's own pattern checked separately the way
                // `patternProperties`'s own keyword function does (issue
                // #498's own follow-up, finding F7). A
                // leading inline flag (`(?i)`) scopes over the *whole*
                // join this way, exactly as it would over a hand-written
                // alternation; an empty join (no `patternProperties` keys
                // at all, or every key the empty string) is falsy in
                // Python (`if not patterns or ...`), so it never matches
                // *any* property, not even one the empty-string pattern
                // would otherwise match trivially (probe-schema7 c7).
                let joined_pattern: String = schema
                    .as_dict()
                    .and_then(|d| get(d, "patternProperties"))
                    .and_then(Value::as_dict)
                    .map(|d| {
                        d.keys()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join("|")
                    })
                    .unwrap_or_default();
                // Unlike `pattern`/`patternProperties`'s own keys,
                // `schema_is_fully_supported` never checks the *joined*
                // alternation itself, only each `patternProperties` key
                // on its own -- `(?i)^x-` and `(?i)^y-` each translate
                // fine alone, but joining them with `"|"` is genuinely
                // Python-invalid (a global inline flag must lead the
                // *whole* expression, not reappear after the first
                // alternative), so this is the one place in the engine
                // that can still reach a translation `Err` on a schema
                // it otherwise accepts (issue #498's own
                // follow-up, finding V6, probe-schema9 c7).
                let has_joined_pattern = !joined_pattern.is_empty();
                let joined_translation =
                    has_joined_pattern.then(|| translated_pattern(&joined_pattern));
                let mut extras: Vec<&str> = Vec::new();
                for (prop_key, _) in obj.iter() {
                    let Some(name) = prop_key.as_str() else {
                        continue;
                    };
                    if properties.is_some_and(|p| p.contains_key(&key(name))) {
                        continue;
                    }
                    if let Some(Err(translate_error)) = &joined_translation {
                        // `_utils.find_additional_properties` calls
                        // `re.search(patterns, property)` for the
                        // *first* candidate property (one not already
                        // covered by `properties`) and never gets any
                        // further once that raises -- matching Python's
                        // own generator-stops-at-the-first-exception
                        // shape exactly, never a wrong `schema_invalid`
                        // verdict treating the failure as "no match".
                        raise_python_exception(translate_error.clone());
                        return;
                    }
                    if has_joined_pattern && pattern_is_match(&joined_pattern, name) {
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
                if let Value::List(items) = instance {
                    if length_cmp(items.len(), value) == Some(std::cmp::Ordering::Less) {
                        out.push(Violation {
                            kind: Kind::MinItems {
                                is_one: limit_equals(value, 1),
                            },
                            path: path.to_vec(),
                            local_schema: schema,
                            local_instance: instance,
                        });
                    }
                }
            }
            "maxItems" => {
                if let Value::List(items) = instance {
                    if length_cmp(items.len(), value) == Some(std::cmp::Ordering::Greater) {
                        out.push(Violation {
                            kind: Kind::MaxItems {
                                is_zero: limit_equals(value, 0),
                            },
                            path: path.to_vec(),
                            local_schema: schema,
                            local_instance: instance,
                        });
                    }
                }
            }
            "minLength" => {
                if let Value::Str(s) = instance {
                    if length_cmp(s.chars().count(), value) == Some(std::cmp::Ordering::Less) {
                        out.push(Violation {
                            kind: Kind::MinLength {
                                is_one: limit_equals(value, 1),
                            },
                            path: path.to_vec(),
                            local_schema: schema,
                            local_instance: instance,
                        });
                    }
                }
            }
            "maxLength" => {
                if let Value::Str(s) = instance {
                    if length_cmp(s.chars().count(), value) == Some(std::cmp::Ordering::Greater) {
                        out.push(Violation {
                            kind: Kind::MaxLength {
                                is_zero: limit_equals(value, 0),
                            },
                            path: path.to_vec(),
                            local_schema: schema,
                            local_instance: instance,
                        });
                    }
                }
            }
            "minProperties" => {
                if let Value::Dict(obj) = instance {
                    if length_cmp(obj.len(), value) == Some(std::cmp::Ordering::Less) {
                        out.push(Violation {
                            kind: Kind::MinProperties {
                                is_one: limit_equals(value, 1),
                            },
                            path: path.to_vec(),
                            local_schema: schema,
                            local_instance: instance,
                        });
                    }
                }
            }
            "maxProperties" => {
                if let Value::Dict(obj) = instance {
                    if length_cmp(obj.len(), value) == Some(std::cmp::Ordering::Greater) {
                        out.push(Violation {
                            kind: Kind::MaxProperties {
                                is_zero: limit_equals(value, 0),
                            },
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
                    if !pattern_is_match(pattern, s) {
                        out.push(Violation {
                            kind: Kind::Pattern,
                            path: path.to_vec(),
                            local_schema: schema,
                            local_instance: instance,
                        });
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
                    if evaluate_branch(branch, instance, root, path, &mut context) {
                        all_valid = true;
                        break;
                    }
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
                    if evaluate_branch(branch, instance, root, path, &mut context) {
                        first_valid = Some(branch);
                        rest_start = index + 1;
                        break;
                    }
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
                let min_contains_value = schema.as_dict().and_then(|d| get(d, "minContains"));
                let max_contains_value = schema.as_dict().and_then(|d| get(d, "maxContains"));
                let mut matches: i64 = 0;
                let mut too_many = false;
                for element in elements {
                    if is_valid(value, element, root) {
                        matches += 1;
                        let exceeds_max = max_contains_value.is_some_and(|max_v| {
                            numeric_cmp(&Value::from(matches), max_v)
                                == Some(std::cmp::Ordering::Greater)
                        });
                        if exceeds_max {
                            too_many = true;
                            break;
                        }
                    }
                }
                let below_min = match min_contains_value {
                    Some(min_v) => {
                        numeric_cmp(&Value::from(matches), min_v) == Some(std::cmp::Ordering::Less)
                    }
                    None => matches < 1,
                };
                if too_many {
                    out.push(Violation {
                        kind: Kind::MaxContains {
                            max_contains: max_contains_value
                                .expect("set whenever too_many is true"),
                        },
                        path: path.to_vec(),
                        local_schema: schema,
                        local_instance: instance,
                    });
                } else if below_min {
                    let kind = if matches == 0 {
                        Kind::ContainsNone
                    } else {
                        Kind::MinContains {
                            min_contains: min_contains_value.expect(
                                "a default minContains of 1 can't be below_min once matches >= 1",
                            ),
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
        // `next(iter_errors(instance), None) is None` (Python's own
        // `Validator.is_valid`) only ever pulls *one* value total from
        // the entire nested `iter_errors` generator tree -- every
        // validator function it hasn't reached yet (including this
        // schema's own remaining top-level keywords) simply never runs
        // at all. [`LAZY_MODE`] mirrors that at this one granularity
        // (between top-level keywords of one schema node; a keyword's
        // own internal loop over several properties/elements/
        // subschemas is a documented, narrower approximation,
        // `DESIGN.md` §13) -- stopping here, not just at the end of
        // [`is_valid`] itself, is what keeps a keyword `not`/`if`/
        // `contains` never actually reaches (e.g. a `$ref` cycle sitting
        // behind a sibling `type` that already failed) from ever
        // running its own expensive or divergent work at all (issue
        // #498's own follow-up, finding V7: `not: {type:
        // string, $ref: '#'}` against `5` is `Valid` in Python, since
        // `type`'s own violation is the first and only one `next()`
        // ever sees -- `$ref` is never reached, so no recursion ever
        // happens, where this engine's own formerly-eager `is_valid`
        // walked it anyway and hit the real cycle-detection fallback).
        if lazy_mode_active() && !out.is_empty() {
            return;
        }
    }
}

thread_local! {
    /// Set for the duration of an [`is_valid`] call (restored to its
    /// prior value on return, via [`LazyModeGuard`], so a nested
    /// `is_valid`-within-`is_valid` -- e.g. `not` inside `not` --
    /// composes correctly) -- [`evaluate`]'s own per-keyword check
    /// reads this, never a parameter threaded through every recursive
    /// call, since the ordinary (non-lazy) top-level `evaluate` call
    /// from [`crate::validate_prompt_schema`] must never see this flag
    /// on just because it happens to run while *nested inside* an
    /// enclosing `is_valid` call. [`evaluate_branch`]'s own branch
    /// descent must be eager too (Python's own `_keywords.anyOf`/
    /// `oneOf` call `list(...)` on each branch, never `next()`), but it
    /// can run nested inside `is_valid` (a `not`/`if`/`contains`
    /// wrapping an `anyOf`) with this flag already on, so it clears it
    /// for its own call via [`suspend_lazy_mode`] rather than relying on
    /// nesting alone -- a *shared* violations `Vec` across branches
    /// means one branch's own early failure would otherwise make
    /// [`evaluate`]'s per-keyword lazy check fire on an *unrelated*
    /// branch's very first keyword, from a violation that branch never
    /// itself raised.
    static LAZY_MODE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn lazy_mode_active() -> bool {
    LAZY_MODE.with(|m| m.get())
}

struct LazyModeGuard(bool);

impl Drop for LazyModeGuard {
    fn drop(&mut self) {
        LAZY_MODE.with(|m| m.set(self.0));
    }
}

fn enter_lazy_mode() -> LazyModeGuard {
    LazyModeGuard(LAZY_MODE.with(|m| m.replace(true)))
}

/// Clears [`LAZY_MODE`] for the duration of one [`evaluate_branch`]
/// call (restored to its prior value on return, via [`LazyModeGuard`]),
/// so each `anyOf`/`oneOf` branch is evaluated to completion into its
/// own slice of the shared violations `Vec` -- regardless of whether an
/// enclosing `not`/`if`/`contains` already turned lazy mode on, and
/// regardless of what an earlier *sibling* branch already pushed.
fn suspend_lazy_mode() -> LazyModeGuard {
    LazyModeGuard(LAZY_MODE.with(|m| m.replace(false)))
}

/// `Validator.is_valid`: *schema* has no violations at all against
/// *instance* -- used by `not`/`if`/`contains` exactly the way Python's
/// own keyword functions call `validator.evolve(schema=...).is_valid(...)`
/// rather than collecting (and discarding) a full violation list; lazy
/// the same way Python's own `next(iter_errors(instance), None) is None`
/// is -- see [`LAZY_MODE`]'s own doc comment.
pub(crate) fn is_valid(schema: &Value, instance: &Value, root: &Value) -> bool {
    let _lazy_guard = enter_lazy_mode();
    let mut violations = Vec::new();
    evaluate(schema, instance, root, &[], &mut violations);
    violations.is_empty()
}

/// `instance < limit`/`instance > limit`/... as Python's own numeric
/// tower compares them: exact for an `Int`-vs-`Int` pair (never losing
/// precision for a big instance, the same reason [`multiple_of_failed`]
/// does its own `int`/`int` arm in `BigInt`) and exact for an `Int`-vs-
/// `Float` pair too, at any magnitude, via [`cmp_bigint_f64`] rather than
/// routing the `Int` side through `f64` first (issue #498's own
/// follow-up, finding F9: `9007199254740993 > 9007199254740992.0`
/// must stay `Greater`, never silently round to `Equal`). `f64`-vs-`f64`
/// otherwise (a `NaN` on either side gives `None`, matching Python's own
/// "every comparison with `NaN` is `False`" rule -- callers that only
/// check for one specific `Ordering` already get that for free; `None`
/// compares equal to nothing).
fn numeric_cmp(instance: &Value, limit: &Value) -> Option<std::cmp::Ordering> {
    match (instance, limit) {
        (Value::Int(a), Value::Int(b)) => Some(a.to_bigint().cmp(&b.to_bigint())),
        (Value::Int(a), Value::Float(b)) => cmp_bigint_f64(&a.to_bigint(), *b),
        (Value::Float(a), Value::Int(b)) => {
            cmp_bigint_f64(&b.to_bigint(), *a).map(std::cmp::Ordering::reverse)
        }
        (Value::Float(a), Value::Float(b)) => a.partial_cmp(b),
        _ => None,
    }
}

/// `length_cmp(len, limit)` is `numeric_cmp` with *len* (an array/string/
/// object's own length, never negative) as the instance side -- shared by
/// `minItems`/`maxItems`/`minLength`/`maxLength`/`minProperties`/
/// `maxProperties`, every one of which Python accepts an integral float
/// or big-int limit for exactly as any other numeric keyword (issue #498's
/// own follow-up, finding F6: `minItems: 2.0` must still
/// reject a shorter array, not silently skip the check the way requiring
/// a plain `i64` schema value used to).
fn length_cmp(len: usize, limit: &Value) -> Option<std::cmp::Ordering> {
    numeric_cmp(&Value::from(len as i64), limit)
}

/// Whether *limit* (a schema's own numeric value, `Int` or `Float`)
/// equals the small integer *n* exactly -- `minItems`/`maxItems`'s own
/// message selects different wording for a limit of exactly `1`/`0`
/// (`render`'s own `MinItems`/`MaxItems` arms), which must still trigger
/// for an integral-float limit (`1.0`), not just a bare JSON integer.
fn limit_equals(limit: &Value, n: i64) -> bool {
    numeric_cmp(&Value::from(n), limit) == Some(std::cmp::Ordering::Equal)
}

/// `_keywords.multipleOf`, ported exactly: an `int`-instance/`int`-divisor
/// pair uses exact arbitrary-precision modulo (never `f64`, so issue
/// #498's own `2**70`-sized instance ported correctly rather than
/// drifting on the way to a float); an `int`/`float`-instance against an
/// `int` divisor follows Python's own `%` (floor division, via
/// [`py_fmod`]), which never raises for a `NaN`/`Infinity` instance
/// (`float('inf') % 3` is itself `nan` in Python, a plain truthy
/// *result*, not an exception) -- an *already*-non-finite remainder is
/// simply a violation, matching Python's own `bool(nan) == True`.
///
/// A zero divisor (`int` or `float`, against either an `int` or a
/// `float` instance) raises `ZeroDivisionError` directly in Python,
/// *before* any of the above -- `integer modulo by zero` for an `int`
/// instance against an `int` divisor of `0`, `float modulo` for a
/// `float` instance against an `int` divisor of `0`, and `float
/// division by zero` for any instance against a `float` divisor of
/// `0.0` (confirmed directly against CPython, every combination).
///
/// A **`float` divisor**, though, is otherwise Python's own
/// `int(instance / dB)` path. The division itself never raises (a
/// `NaN` on either side, or an `Infinity` instance against a finite
/// divisor, simply gives a `NaN`/`Infinity` *quotient* rather than
/// raising) -- `int(quotient)` is what can: a `NaN` quotient (either
/// operand `NaN`, or an `Infinity` instance divided by an `Infinity`
/// divisor, which is itself `NaN`) is an uncaught `ValueError`
/// ("cannot convert float NaN to integer"); an *Infinity* quotient
/// raises `OverflowError`, caught by `_keywords.multipleOf` only to
/// retry via `Fraction(instance) / Fraction(dB)` -- which itself raises
/// its *own*, still-uncaught `OverflowError` ("cannot convert Infinity
/// to integer ratio") if *instance* is the infinite operand (message
/// text sign-independent, `-Infinity` included), but otherwise
/// succeeds: an infinite quotient from two genuinely *finite* operands
/// (`1e308` against `multipleOf: 0.5`, issue #498's own
/// follow-up, finding V5, probe-schema9 c6) is `f64`'s own range
/// overflowing computing `i / d`, not either operand itself being
/// non-finite, so [`fraction_multiple_of_failed`] gets a clean run.
/// `core/prompt.py`'s own generic `except Exception` (not `except
/// jsonschema.ValidationError`) turns any of the above into a
/// non-retryable, generic failure, never `schema_invalid` --
/// [`raise_python_exception`] reproduces that outcome exactly (issue
/// #498's own follow-up, finding F11); the caller must
/// treat this function's own `false` as "no ordinary violation", not
/// "valid", whenever that flag ends up set.
fn multiple_of_failed(instance: &Value, divisor: &Value) -> bool {
    if !matches!(instance, Value::Int(_) | Value::Float(_)) {
        return false;
    }
    if let (Value::Int(n), Value::Int(d)) = (instance, divisor) {
        let d = d.to_bigint();
        if d == num_bigint::BigInt::from(0) {
            raise_python_exception("integer modulo by zero");
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
        // dividend's own sign) -- `py_fmod` corrects that. A zero
        // divisor raises before `%` ever runs, regardless of the
        // instance's own `NaN`/`Infinity` status.
        if d == 0.0 {
            raise_python_exception("float modulo");
            return false;
        }
        let remainder = py_fmod(i, d);
        return remainder != 0.0;
    }
    // A zero `float` divisor raises before the division itself runs,
    // regardless of the instance's own `NaN`/`Infinity` status.
    if d == 0.0 {
        raise_python_exception("float division by zero");
        return false;
    }
    let quotient = i / d;
    if quotient.is_nan() {
        raise_python_exception("cannot convert float NaN to integer");
        return false;
    }
    if quotient.is_infinite() {
        if i.is_infinite() {
            // `instance` itself is the infinite operand: `Fraction(instance)`
            // raises this, sign-independent, before `divisor` is ever
            // considered (confirmed directly against CPython).
            raise_python_exception("cannot convert Infinity to integer ratio");
            return false;
        }
        // `instance` and `divisor` are both finite here -- the quotient
        // itself simply overflowed `f64`'s own range computing `i / d`;
        // `Fraction(instance)`/`Fraction(dB)` succeed on two finite
        // operands, so [`fraction_multiple_of_failed`] gets a clean run.
        return fraction_multiple_of_failed(i, d);
    }
    quotient.trunc() != quotient
}

/// `(Fraction(instance) / Fraction(dB)).denominator != 1` -- exact,
/// never losing precision the way routing either side through `f64`
/// division would (the same reason [`float_exact_ratio`] exists at
/// all): each finite `f64` has its own exact `(numerator, denominator)`
/// ratio already; dividing two fractions and reducing the result by
/// their `BigInt` `gcd` decides whether the quotient is a whole number
/// without ever computing it as a `f64`. [`multiple_of_failed`]'s own
/// caller only ever reaches this with two already-finite operands, but
/// a `None` here (impossible today) falls back to "no violation" --
/// never a panic -- rather than trust that invariant with an `expect`.
fn fraction_multiple_of_failed(instance: f64, divisor: f64) -> bool {
    let Some((in_, id)) = float_exact_ratio(instance) else {
        return false;
    };
    let Some((dn, dd)) = float_exact_ratio(divisor) else {
        return false;
    };
    let num = in_ * dd;
    let den = id * dn;
    if num == num_bigint::BigInt::from(0) {
        return false;
    }
    let reduced_den = bigint_abs(&den) / bigint_gcd(&num, &den);
    reduced_den != num_bigint::BigInt::from(1)
}

/// `num-bigint` has no inherent `abs` without also depending on
/// `num-traits` (not otherwise needed here) -- [`bigint_gcd`] and
/// [`fraction_multiple_of_failed`]'s own reduced denominator both need
/// a magnitude, never a sign.
fn bigint_abs(x: &num_bigint::BigInt) -> num_bigint::BigInt {
    if *x < num_bigint::BigInt::from(0) {
        -x
    } else {
        x.clone()
    }
}

/// The Euclidean algorithm over `BigInt` magnitudes -- `num-bigint`
/// itself has no `gcd` (that's `num-integer`, not a dependency this
/// crate otherwise needs), and [`fraction_multiple_of_failed`]'s own
/// fraction reduction is the only place one is needed.
fn bigint_gcd(a: &num_bigint::BigInt, b: &num_bigint::BigInt) -> num_bigint::BigInt {
    let mut a = bigint_abs(a);
    let mut b = bigint_abs(b);
    while b != num_bigint::BigInt::from(0) {
        let r = &a % &b;
        a = b;
        b = r;
    }
    a
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

thread_local! {
    /// A per-pattern-*text* translation cache -- `pattern`/
    /// `patternProperties` (each key) is translated once no matter how
    /// many instance nodes end up checked against it (every array
    /// element, every object property), never once per visit (issue
    /// #498's own follow-up, finding F2's own "compiling"
    /// bullet). Caches the *translated pattern text*, not a compiled
    /// `fancy_regex::Regex` -- [`pattern_is_match`]'s own backtrack
    /// limit is sized per *instance*, so the compiled object itself can
    /// never be reused across two differently-sized instances anyway
    /// (issue #498's own follow-up, finding V4). Cleared at
    /// the start of every [`crate::validate_prompt_schema`] call
    /// ([`clear_pattern_cache`]) -- purely a memory bound across
    /// unrelated schemas on the same thread, never a correctness one
    /// (translation never varies by anything but the pattern text
    /// itself).
    static TRANSLATED_PATTERN_CACHE: std::cell::RefCell<std::collections::HashMap<String, Result<std::rc::Rc<str>, String>>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

/// Clears [`TRANSLATED_PATTERN_CACHE`] -- [`crate::validate_prompt_schema`]'s
/// own first step, the same reason [`take_recursion_exceeded`] is.
pub(crate) fn clear_pattern_cache() {
    TRANSLATED_PATTERN_CACHE.with(|c| c.borrow_mut().clear());
}

/// Whether *pattern* is a construct Python's own `re.compile` accepts --
/// [`schema_is_fully_supported`]'s own "pattern"/"patternProperties"
/// gate, checked through the exact same translator [`compile_pattern`]
/// itself compiles through (never the `jsonschema` crate's own ECMA-262
/// regex check, which rejects Python-valid syntax `fancy-regex` itself
/// accepts, e.g. `\Z` -- issue #498's own follow-up, finding
/// F2).
fn python_regex_is_valid(pattern: &str) -> bool {
    electricity_pyre::translate::translate(
        pattern,
        electricity_pyre::translate::ResolvedFlags::default(),
    )
    .is_ok()
}

/// `jsonschema.validate`'s own `check_schema` step validates *schema*
/// against its draft's metaschema -- which, for `pattern`'s own value
/// and every `patternProperties` key, means `format: "regex"`
/// (`re.compile`), checked unconditionally and *before* any instance is
/// ever validated, regardless of which draft, regardless of whether
/// anything else about the schema is otherwise supported or even
/// reachable (issue #498's own follow-up, finding V2,
/// probe-schema9 c1/c2: confirmed directly that `\p{L}+` and
/// `(?<year>\d{4})` -- each genuinely Python-`re`-invalid -- raise
/// `jsonschema.SchemaError` from *every* schema position the metaschema
/// recurses into as a subschema, `$defs`/`definitions`/`dependencies`
/// included, even though none of those three is itself a real
/// `Draft202012Validator` keyword). [`crate::validate_prompt_schema`]
/// calls this *unconditionally*, before deciding which authority
/// (engine or crate) to use at all -- the `jsonschema` Rust crate's own
/// regex engine is ECMA-262-based and silently *accepts* both of those
/// patterns (confirmed directly), so routing to crate authority instead
/// of catching this first would misreport Python's own `SchemaError` as
/// a plain accept. Walks every position Python's own 2020-12 metaschema
/// validates as a schema -- `properties`/`patternProperties`/`$defs`/
/// `definitions`'s own *values*, a dict-shaped `dependencies` value,
/// `additionalProperties`/`items`/`not`/`propertyNames`/`contains`/
/// `if`/`then`/`else`, and every `allOf`/`anyOf`/`oneOf`/`prefixItems`
/// element -- never through `$ref` (a `$ref`'s own *target* is checked
/// directly wherever it lives in the document; the metaschema's own
/// `$ref` keyword definition only constrains the *pointer string*'s own
/// type, never double-checks whatever it points to). Returns Python's
/// own `str(SchemaError)` for the common, direct case (`'<pattern>' is
/// not a 'regex'`) -- the real exception's full multi-line metaschema
/// diagnostic is a documented, narrower gap (`DESIGN.md` §13); matching
/// the verdict itself (a failed, non-retryable attempt) is what's
/// required, not the exact text.
pub(crate) fn find_invalid_pattern(schema: &Value) -> Option<String> {
    let dict = schema.as_dict()?;
    for (raw_key, v) in dict.iter() {
        let Some(key_str) = raw_key.as_str() else {
            continue;
        };
        match key_str {
            "pattern" => {
                if let Some(p) = v.as_str() {
                    if !python_regex_is_valid(p) {
                        return Some(format!(
                            "{} is not a 'regex'",
                            Value::Str(p.to_string()).py_repr()
                        ));
                    }
                }
            }
            "patternProperties" => {
                if let Some(d) = v.as_dict() {
                    for (pk, sub) in d.iter() {
                        if let Some(p) = pk.as_str() {
                            if !python_regex_is_valid(p) {
                                return Some(format!(
                                    "{} is not a 'regex'",
                                    Value::Str(p.to_string()).py_repr()
                                ));
                            }
                        }
                        if let Some(found) = find_invalid_pattern(sub) {
                            return Some(found);
                        }
                    }
                }
            }
            "properties" | "dependentSchemas" | "$defs" | "definitions" => {
                if let Some(d) = v.as_dict() {
                    for sub in d.values() {
                        if let Some(found) = find_invalid_pattern(sub) {
                            return Some(found);
                        }
                    }
                }
            }
            "dependencies" => {
                if let Some(d) = v.as_dict() {
                    for sub in d.values() {
                        if sub.as_dict().is_some() {
                            if let Some(found) = find_invalid_pattern(sub) {
                                return Some(found);
                            }
                        }
                    }
                }
            }
            "additionalProperties"
            | "items"
            | "not"
            | "propertyNames"
            | "contains"
            | "if"
            | "then"
            | "else"
            | "unevaluatedProperties"
            | "unevaluatedItems" => {
                if let Some(found) = find_invalid_pattern(v) {
                    return Some(found);
                }
            }
            "allOf" | "anyOf" | "oneOf" | "prefixItems" => {
                if let Some(items) = v.as_list() {
                    for sub in items {
                        if let Some(found) = find_invalid_pattern(sub) {
                            return Some(found);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// Translates *pattern* through [`electricity_pyre::translate::translate`]
/// (Python-`re`-exact `\w`/`\s`/`\d`/`\b`/`\B`/non-MULTILINE `$`/`\Z`/...
/// -- issue #498's own follow-up, finding F2, replacing this
/// module's own narrower ad hoc `\w`/`\s`/`$`/`\d` rewrites), memoized by
/// pattern text in [`TRANSLATED_PATTERN_CACHE`]. `Err` for anything
/// Python's own `re.compile` would also reject -- on the `pattern`/
/// `patternProperties` keyword path this is never reached at all, since
/// [`schema_is_fully_supported`] (via [`python_regex_is_valid`]) already
/// vetoes a schema with an invalid pattern anywhere in it before
/// [`evaluate`] ever runs; the `additionalProperties`+`patternProperties`
/// *joined* pattern is the one caller that genuinely reaches `Err` on a
/// schema this engine still accepts (issue #498's own
/// follow-up, finding V6: each `patternProperties` key alone translates
/// fine, but `"|"`-joining two that each open with their own inline
/// flag group, e.g. `(?i)^x-` and `(?i)^y-`, is -- correctly -- Python-
/// invalid, since a global flag must lead the *whole* expression).
fn translated_pattern(pattern: &str) -> Result<std::rc::Rc<str>, String> {
    TRANSLATED_PATTERN_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(cached) = cache.get(pattern) {
            return cached.clone();
        }
        let result = electricity_pyre::translate::translate(
            pattern,
            electricity_pyre::translate::ResolvedFlags::default(),
        )
        .map(|t| std::rc::Rc::from(t.pattern.as_str()));
        cache.insert(pattern.to_string(), result.clone());
        result
    })
}

/// Whether *text* matches Python pattern *pattern*, through
/// [`translated_pattern`] then a fresh `fancy-regex` build sized with a
/// backtrack limit for *text*'s own length
/// ([`electricity_pyre::backtrack_limit_for`], the same formula
/// `electricity-tools::regex` already sizes its own limit with) --
/// never fancy-regex's flat 1,000,000 default, which an otherwise-
/// ordinary-length instance can exhaust through a lookaround this
/// translation emits (non-MULTILINE `$`, `\b`/`\B`, a lookbehind) even
/// though Python's own `re` (no such limit at all) matches it instantly
/// (issue #498's own follow-up, finding V4, probe-schema9
/// c5). Never silently treats a *build* failure (fancy-regex rejecting
/// syntax Python's own `re` accepts, e.g. a backreference inside a
/// lookbehind -- `electricity-tools::regex`'s own module docs, finding
/// V3, probe-schema9 c4) or this *match* call's own backtrack-limit
/// error as a match: either one calls [`raise_python_exception`]
/// instead, a documented, narrower divergence (`DESIGN.md` §13)
/// `validate_prompt_schema` reports as a generic, non-retryable failure
/// rather than a wrong `schema_invalid`/`Valid` verdict.
fn pattern_is_match(pattern: &str, text: &str) -> bool {
    let Ok(translated) = translated_pattern(pattern) else {
        // Only reachable from the `additionalProperties` joined-pattern
        // path, which checks `translated_pattern` itself first and
        // never calls this at all once it has -- every other caller is
        // already gated by `schema_is_fully_supported`.
        return false;
    };
    let limit = electricity_pyre::backtrack_limit_for(text.len());
    let compiled = match fancy_regex::RegexBuilder::new(&translated)
        .backtrack_limit(limit)
        .build()
    {
        Ok(re) => re,
        Err(e) => {
            raise_python_exception(format!(
                "electricity's regex engine cannot compile the pattern {} that Python's `re` \
                 accepts: {e}",
                Value::Str(pattern.to_string()).py_repr(),
            ));
            return false;
        }
    };
    match compiled.is_match(text) {
        Ok(matched) => matched,
        Err(e) => {
            raise_python_exception(format!(
                "electricity's regex engine gave up matching the pattern {} (Python's `re` has \
                 no backtracking limit at all): {e}",
                Value::Str(pattern.to_string()).py_repr(),
            ));
            false
        }
    }
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
    "uniqueItems",
    "minContains",
    "maxContains",
    "dependentRequired",
    "$defs",
    "definitions",
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

thread_local! {
    /// Memoizes [`schema_is_fully_supported`] by schema-node identity --
    /// today, called at most once per [`crate::validate_prompt_schema`]
    /// call (the top-level accept-authority gate; [`evaluate_branch`]/
    /// [`schema_accepts`] no longer re-run it per branch, PR #502's
    /// round-6 fix), but a node this pass already resolved gets the
    /// answer back without a second walk rather than relying on every
    /// caller to know that invariant holds. Cleared at the start of
    /// every [`crate::validate_prompt_schema`] call ([`clear_pattern_cache`]
    /// does this too; the same reason).
    static FULLY_SUPPORTED_CACHE: std::cell::RefCell<std::collections::HashMap<*const Value, bool>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

pub(crate) fn clear_fully_supported_cache() {
    FULLY_SUPPORTED_CACHE.with(|c| c.borrow_mut().clear());
}

/// Whether *schema* (and, recursively, every subschema it can reach --
/// `properties`/`patternProperties`/`additionalProperties`/`items`/
/// `prefixItems`/`allOf`/`anyOf`/`oneOf`/`not`/`if`/`then`/`else`/
/// `dependentSchemas`/`contains`/`propertyNames`, and whatever a `$ref`
/// resolves to) uses *only* keywords [`evaluate`] implements -- the
/// authority test issue #498's own follow-up (ruling R6)
/// requires: this engine decides accept/reject itself only when this
/// returns `true`, never the `jsonschema` crate (whose own lossy
/// `serde_json`/ECMA-regex/format-by-default behavior is exactly what
/// the follow-up's own probes caught diverging from Python). Memoized
/// by schema-node identity ([`FULLY_SUPPORTED_CACHE`]).
///
/// A `$ref` target already seen *anywhere* in this call's own walk
/// (tracked by node identity in `seen`, inserted before descending into
/// it and never removed) counts as supported without recursing into it
/// again -- the overall answer is a single AND over every *distinct*
/// node the schema can reach, so once one occurrence of a node has
/// contributed its own share of that AND, a second occurrence (whether
/// a genuine cycle back to an ancestor still being walked, or simply
/// another path converging on the same node later) has nothing left to
/// add: if that node were going to make the overall answer `false`, the
/// first visit's own `false` would already have short-circuited every
/// `all`/`&&` between it and the top of this call before a second visit
/// could ever run (every aggregation point here is exactly that kind of
/// short-circuiting boolean -- `Dict::iter().all`, `d.values().all`,
/// plain `&&`), so treating a repeat as supported is never a wrong
/// answer, only a skipped, already-redundant walk.
///
/// This also makes a schema cyclic *only* via `$ref` safe the same way
/// it always was (`$defs: {loop: {$ref: '#/$defs/loop'}}`, or the
/// degenerate `{"$ref": "#"}`) -- the recursive shape a real document's
/// `schema:` legitimately uses to describe a tree or a linked list.
/// Judging a cycle like that "unsupported" (PR #502's round-4 fix,
/// finding F1) used to fail every instance of every recursive schema,
/// even a depth-1 one, before [`evaluate`] ever ran: `MAX_RECURSION_
/// DEPTH`-many `$ref` hops through the same handful of cyclic nodes
/// always eventually tripped the old plain depth budget and reported
/// Python's own uncaught `RecursionError` text regardless of the
/// instance at hand. Never popping `seen` means it can no longer double
/// as that depth budget (PR #502's round-6 fix, finding F14: without
/// this, a schema with many mutually-referencing `$ref` targets re-
/// walked the identical subtree once per path between them, genuinely
/// factorial in the number of targets -- ten of them, each referencing
/// every other, took several seconds), so a separate `depth` parameter
/// -- incremented only for a newly-seen `$ref` hop, exactly mirroring
/// how deep `visited` itself used to grow along one path -- takes over
/// that job. [`evaluate`]'s own [`enter_eval_guard`] is the real
/// recursion-safety net regardless (this function never calls
/// `evaluate`, so a non-cyclic, pathologically deep schema can't
/// overflow the stack through this path either).
pub(crate) fn schema_is_fully_supported(schema: &Value, root: &Value) -> bool {
    let ptr: *const Value = schema;
    if let Some(cached) = FULLY_SUPPORTED_CACHE.with(|c| c.borrow().get(&ptr).copied()) {
        return cached;
    }
    let mut seen = std::collections::HashSet::new();
    let result = schema_is_fully_supported_inner(schema, root, &mut seen, 0, true);
    FULLY_SUPPORTED_CACHE.with(|c| {
        c.borrow_mut().insert(ptr, result);
    });
    result
}

/// A `$ref`'s own value is only ever this engine's own [`resolve_ref`]
/// shape -- a bare `"#"` (the whole document) or a `"#/..."` JSON
/// Pointer -- never an `$anchor` name (`"#foo"`), an `$id`-relative
/// form, or a remote URI; [`resolve_ref`] returns `None` for all of
/// these indiscriminately, so this is what tells a genuinely dangling
/// *pointer* (lazily fine, Python resolves it lazily too -- finding F5)
/// apart from a `$ref` shape this engine never understood in the first
/// place (genuinely unsupported, routing to the crate -- finding F10,
/// `DESIGN.md` §13's own existing "any `$ref` anchor" row).
fn is_same_document_pointer_syntax(pointer: &str) -> bool {
    pointer == "#" || pointer.starts_with("#/")
}

fn schema_is_fully_supported_inner(
    schema: &Value,
    root: &Value,
    seen: &mut std::collections::HashSet<*const Value>,
    depth: usize,
    is_root: bool,
) -> bool {
    if depth >= MAX_RECURSION_DEPTH || stack_budget_exceeded() {
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
            // Keywords Python's Draft202012Validator own `VALIDATORS`
            // dict *does* implement that this engine does not -- the
            // only names that must stay "unsupported" (routing to the
            // crate) rather than falling through to the generic
            // annotation case below (issue #498's own
            // follow-up, finding F4).
            if matches!(
                key_str,
                "$dynamicRef" | "$dynamicAnchor" | "unevaluatedProperties" | "unevaluatedItems"
            ) {
                return false;
            }
            match key_str {
                // A nested (non-root) `$id` changes the base URI every
                // `$ref` underneath it resolves against -- this engine
                // ([`resolve_ref`]) only ever resolves a same-document
                // pointer against the *document* root, never a `$id`
                // subtree's own base, so a schema using one is routed
                // to the crate instead, which does resolve it correctly
                // (confirmed directly) -- never a silent, wrong verdict
                // (issue #498's own follow-up, finding
                // F10, probe-schema7 c14). The document's own top-level
                // `$id` (`is_root`) changes nothing a same-document
                // `"#/..."` pointer's own resolution needs, so it stays
                // a plain annotation.
                // A nested (non-root) `$schema` switches `jsonschema`'s
                // own validator -- and therefore draft -- for that
                // subschema and everything under it (`referencing`'s
                // own per-resource specification detection); this
                // engine always validates the *whole* document under
                // one draft (`validate_prompt_schema`'s own top-level
                // `is_2020_12_or_unset` check), so a nested `$schema`
                // routes to the crate instead rather than silently
                // validating that subtree under the wrong draft (issue
                // #498's own follow-up, P2 "nested
                // `$schema`"). The document's own top-level `$schema`
                // is already how `is_2020_12_or_unset` picked a draft
                // in the first place, so it stays a plain annotation
                // here, the same as `"$id"`.
                "$schema" => is_root,
                "$id" => is_root,
                // A `$ref` that doesn't resolve is not, by itself, a
                // reason to fall back to the crate: Python resolves a
                // `$ref` lazily, only once the validator actually
                // descends into it, so a dangling pointer an instance
                // never reaches is never an error at all (issue #498's
                // own follow-up, finding F5, probe-schema7
                // c15) -- [`evaluate`]'s own "$ref" arm is equally lazy
                // (silently does nothing when `resolve_ref` fails). An
                // `$anchor`/`$id`-relative/remote `$ref` shape
                // ([`is_same_document_pointer_syntax`]) stays
                // unsupported outright, the same as before this fix.
                "$ref" => v.as_str().is_some_and(|pointer| {
                    if !is_same_document_pointer_syntax(pointer) {
                        return false;
                    }
                    match resolve_ref(root, pointer) {
                        None => true,
                        Some(resolved) => {
                            let ptr: *const Value = resolved;
                            if !seen.insert(ptr) {
                                return true;
                            }
                            schema_is_fully_supported_inner(resolved, root, seen, depth + 1, false)
                        }
                    }
                }),
                // `pattern`'s own value, and every key of
                // `patternProperties`, must be a pattern Python's own
                // `re.compile` accepts -- checked through the same
                // translator [`compile_pattern`] itself compiles
                // through, never the `jsonschema` crate's own narrower
                // ECMA-262 regex check (issue #498's own
                // follow-up, finding F2: a Python-valid-but-ECMA-
                // incompatible pattern, e.g. `^a\Z`, must never route a
                // schema to crate authority at all -- that authority
                // check is also, by construction, the only place this
                // engine needs to veto an invalid pattern, since
                // [`evaluate`]'s own "pattern"/"patternProperties"
                // handling only ever runs on a schema this function has
                // already confirmed every pattern in compiles, issue
                // #498's own follow-up, finding F13).
                "pattern" => v.as_str().is_some_and(python_regex_is_valid),
                "properties" | "dependentSchemas" => v.as_dict().is_some_and(|d| {
                    d.values()
                        .all(|sub| schema_is_fully_supported_inner(sub, root, seen, depth, false))
                }),
                "patternProperties" => v.as_dict().is_some_and(|d| {
                    d.iter().all(|(pattern_key, sub)| {
                        pattern_key.as_str().is_some_and(python_regex_is_valid)
                            && schema_is_fully_supported_inner(sub, root, seen, depth, false)
                    })
                }),
                "additionalProperties" | "items" | "not" | "propertyNames" | "contains" => {
                    schema_is_fully_supported_inner(v, root, seen, depth, false)
                }
                "if" => {
                    schema_is_fully_supported_inner(v, root, seen, depth, false)
                        && dict.get(&key("then")).is_none_or(|t| {
                            schema_is_fully_supported_inner(t, root, seen, depth, false)
                        })
                        && dict.get(&key("else")).is_none_or(|e| {
                            schema_is_fully_supported_inner(e, root, seen, depth, false)
                        })
                }
                "then" | "else" => true, // handled alongside "if" above
                "prefixItems" | "allOf" | "anyOf" | "oneOf" => v.as_list().is_some_and(|items| {
                    items
                        .iter()
                        .all(|sub| schema_is_fully_supported_inner(sub, root, seen, depth, false))
                }),
                // Any other key -- a keyword Python's own Draft202012
                // `VALIDATORS` dict never implements at all (e.g.
                // `dependencies`, a legacy Draft 7 keyword Python
                // ignores as a pure annotation on 2020-12) or a plain
                // typo (`descripton`) -- is never walked into and never
                // affects validity either way, matching Python's own
                // unknown-keyword-is-an-annotation rule exactly (issue
                // #498's own follow-up, finding F4;
                // probe-schema7 c3/c4).
                _ => true,
            }
        }),
        _ => false,
    }
}

/// Whether *instance* validates against *branch* at all, via this
/// module's own [`is_valid`] directly on the *unresolved* branch (whose
/// own `evaluate` already understands `$ref`, siblings included, exactly
/// as Python's own `_validate_reference` applicator does) -- never the
/// `jsonschema` crate, and never by compiling *branch* as its own
/// standalone document (issue #498's own follow-up,
/// finding N1: a `$ref` nested inside a branch, or a `$ref` with sibling
/// keywords, used to be judged by a *freshly compiled*, branch-only
/// validator that either couldn't resolve the ref at all or silently
/// dropped the siblings). *branch* needs no [`schema_is_fully_supported`]
/// check of its own: every caller of [`evaluate`] (the only way this
/// function, or [`evaluate_branch`], is ever reached) already ran under
/// [`crate::validate_prompt_schema`]'s own top-level gate on the *same*
/// *root* -- a single AND over every node that `root` can reach at all,
/// `anyOf`/`oneOf` branches included -- so a branch *this* function
/// sees was already confirmed supported before `evaluate` ever started
/// (PR #502's round-6 fix: re-running the check per branch, on a schema
/// with many mutually-referencing `$ref` targets, revisited the same
/// nodes often enough to turn a sub-second check into several seconds).
fn schema_accepts(branch: &Value, instance: &Value, root: &Value) -> bool {
    is_valid(branch, instance, root)
}

/// Evaluates *branch* against *instance* exactly once, returning whether
/// it's valid and -- unlike [`schema_accepts`] -- appending its own
/// violations to *context* if it isn't, the same way Python's
/// `_keywords.anyOf`/`oneOf` populate `all_errors` (`.extend`, never
/// grouped per branch). No [`schema_is_fully_supported`] check of its
/// own, for the same reason [`schema_accepts`] no longer has one.
///
/// Replaces this module's own former pairing of `schema_accepts` (a
/// bare bool) immediately followed by a *second*, separate call that
/// re-evaluated the identical branch/instance pair just to collect its
/// violations -- doubling every failing anyOf/oneOf branch's own cost,
/// recursively, for a `$ref` branch nested inside another anyOf/oneOf:
/// genuinely exponential in nesting depth once a document goes deep
/// enough to fail at every level (issue #498's own
/// follow-up, finding F12, probe-schema7 c18/c19 -- `tests/
/// nested_any_of_depth.rs` pins 30 levels staying well under a second).
fn evaluate_branch<'a>(
    branch: &'a Value,
    instance: &'a Value,
    root: &'a Value,
    path: &[PathSegment],
    context: &mut Vec<Violation<'a>>,
) -> bool {
    let _lazy_guard = suspend_lazy_mode();
    let before = context.len();
    evaluate(branch, instance, root, path, context);
    context.len() == before
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
        Kind::MinItems { is_one } => {
            let message = if *is_one {
                "should be non-empty"
            } else {
                "is too short"
            };
            format!("{} {message}", instance.py_repr())
        }
        Kind::MaxItems { is_zero } => {
            let message = if *is_zero {
                "is expected to be empty"
            } else {
                "is too long"
            };
            format!("{} {message}", instance.py_repr())
        }
        Kind::MinLength { is_one } => {
            let message = if *is_one {
                "should be non-empty"
            } else {
                "is too short"
            };
            format!("{} {message}", instance.py_repr())
        }
        Kind::MaxLength { is_zero } => {
            let message = if *is_zero {
                "is expected to be empty"
            } else {
                "is too long"
            };
            format!("{} {message}", instance.py_repr())
        }
        Kind::MinProperties { is_one } => {
            let message = if *is_one {
                "should be non-empty"
            } else {
                "does not have enough properties"
            };
            format!("{} {message}", instance.py_repr())
        }
        Kind::MaxProperties { is_zero } => {
            let message = if *is_zero {
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
        } => {
            let min_contains = min_contains.py_repr();
            format!(
                "Too few items match the given schema (expected at least {min_contains} but only \
                 {matches} matched)"
            )
        }
        Kind::MaxContains { max_contains } => {
            let max_contains = max_contains.py_repr();
            format!("Too many items match the given schema (expected at most {max_contains})")
        }
        Kind::ItemsFalse {
            prefix,
            extra,
            item_word,
            rest_repr,
        } => format!("Expected at most {prefix} {item_word} but found {extra} extra: {rest_repr}"),
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

    /// Issue #498's own follow-up, finding N11: the
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

    /// Two `NaN`s in one decoded instance ARE a duplicate pair --
    /// CPython's own `json` scanner hands back one shared `NaN` object
    /// for every occurrence (`UniqKey::Nan`'s own doc comment; issue
    /// #498's own follow-up, finding F8).
    #[test]
    fn uniq_items_dedupes_two_nans() {
        let items = vec![Value::Float(f64::NAN), Value::Float(f64::NAN)];
        assert!(!py_uniq(&items));
    }

    /// `+Infinity` and `-Infinity` are each a duplicate of themselves but
    /// never of each other (same CPython singleton-per-sign rule).
    #[test]
    fn uniq_items_dedupes_same_signed_infinity_but_not_the_other_sign() {
        assert!(!py_uniq(&[
            Value::Float(f64::INFINITY),
            Value::Float(f64::INFINITY)
        ]));
        assert!(py_uniq(&[
            Value::Float(f64::INFINITY),
            Value::Float(f64::NEG_INFINITY)
        ]));
    }

    /// A big int and the float it is exactly equal to collide even past
    /// `i64`'s own range (issue #498's own follow-up,
    /// finding F8, c11/c12).
    #[test]
    fn uniq_items_dedupes_a_big_int_against_its_exact_float_equivalent() {
        let big: num_bigint::BigInt = "10000000000000000000".parse().unwrap();
        let items = vec![
            Value::Int(electricity_value::IntValue::Big(big)),
            Value::Float(1e19),
        ];
        assert!(!py_uniq(&items));
    }

    /// `2**63 - 1` (an odd `i64::MAX`) is genuinely distinct from the
    /// float `2**63` next to it -- no spurious collision from rounding
    /// either value through the other's own representation.
    #[test]
    fn uniq_items_keeps_i64_max_distinct_from_its_float_neighbor() {
        let items = vec![Value::from(i64::MAX), Value::Float(9223372036854775808.0)];
        assert!(py_uniq(&items));
    }

    /// Confirmed directly against Python's own `jsonschema._utils.uniq`
    /// (issue #498's own follow-up, finding F8,
    /// probe-schema7 c10): a real, non-adjacent duplicate (`[1]` at
    /// index 0 and 2) can survive `uniq` entirely when a third element
    /// (`[True]`) sorts as *tied* with both under Python's own plain
    /// `==` (`1 == True`) without being `equal()`-equal to either --
    /// the adjacency check (`[1]` vs `[True]`, then `[True]` vs `[1]`)
    /// never actually compares the two genuinely-equal `[1]`s to each
    /// other at all. Not a bug to "fix" -- replicating it exactly is
    /// the correct behavior here.
    #[test]
    fn uniq_items_misses_a_duplicate_a_tied_but_unequal_element_separates() {
        let items = vec![
            Value::List(vec![Value::from(1i64)]),
            Value::List(vec![Value::Bool(true)]),
            Value::List(vec![Value::from(1i64)]),
        ];
        assert!(py_uniq(&items));
    }

    /// The genuine `TypeError` fallback: a `dict` is unorderable, so a
    /// real duplicate among otherwise-incomparable elements must still
    /// be caught by the all-pairs loop, not missed the way the sorted
    /// fast path's own adjacency quirk (just above) can.
    #[test]
    fn uniq_items_falls_back_to_all_pairs_when_a_dict_makes_sorting_impossible() {
        let mut dict = Dict::new();
        dict.insert(key("a"), Value::from(1i64));
        let items = vec![
            Value::Dict(dict.clone()),
            Value::List(vec![Value::from(1i64)]),
            Value::Dict(dict),
        ];
        assert!(!py_uniq(&items));
    }

    /// A `NaN` nested inside a list is a duplicate of another nested
    /// `NaN` -- within *one* decoded instance every `NaN` literal is
    /// CPython's own `json` scanner's shared singleton, so `equal()`'s
    /// own `one is two` identity check (at every recursion level) makes
    /// `[[nan], [nan]]` collide even though `nan == nan` is plain-`==`
    /// false (confirmed directly against Python; issue #498's own
    /// follow-up, NaN uniqueness edges).
    #[test]
    fn uniq_items_dedupes_a_nan_nested_inside_a_list() {
        let items = vec![
            Value::List(vec![Value::Float(f64::NAN)]),
            Value::List(vec![Value::Float(f64::NAN)]),
        ];
        assert!(!py_uniq(&items));
    }

    /// Confirmed directly against Python: a bare top-level `NaN` *can*
    /// still miss a duplicate, same shape as
    /// `uniq_items_misses_a_duplicate_a_tied_but_unequal_element_
    /// separates` above -- `[NaN, 1, NaN]` sorts (every `NaN`
    /// comparison is plain `False`, never a `TypeError`) to leave `1`
    /// sitting between the array's own two `NaN`s, and the adjacency
    /// check never compares them to each other at all. A documented,
    /// narrower residual gap (`DESIGN.md` §13): [`py_uniq`]'s own
    /// hash-bucketed fast path (needed so a 50,000-element array stays
    /// fast, [`uniq_items_stays_fast_at_scale`] above) counts every
    /// `NaN` together regardless of what sits between them, so this
    /// specific case (a bare `NaN` *and* a bare non-`NaN` scalar mixed
    /// at the same top level) still diverges from Python's own result.
    #[test]
    fn uniq_items_bare_nan_separated_by_a_scalar_is_a_documented_divergence() {
        let items = vec![
            Value::Float(f64::NAN),
            Value::from(1i64),
            Value::Float(f64::NAN),
        ];
        assert!(
            !py_uniq(&items),
            "documented divergence: Python itself says unique here"
        );
    }

    /// Finding C1: `[T]::sort_by` can panic (a transitivity-violation
    /// check in Rust's own sort implementation) when fed a comparator
    /// that isn't a genuine total order -- mapping "incomparable" to
    /// `Equal` used to do exactly that. A randomized sweep over mixed
    /// types (including several `dict`s, which make the whole list
    /// unorderable), nested lists, `None`, `bool`s and `NaN`, at sizes
    /// both below and at/above the `n = 64` single-run threshold, must
    /// never panic regardless of what [`py_uniq`] it lands on.
    #[test]
    fn uniq_items_never_panics_on_a_randomized_mix_of_types() {
        let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next_u64 = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _trial in 0..500 {
            let n = (next_u64() % 210) as usize;
            let items: Vec<Value> = (0..n)
                .map(|_| match next_u64() % 8 {
                    0 => Value::from((next_u64() % 5) as i64),
                    1 => Value::Str(format!("s{}", next_u64() % 5)),
                    2 => Value::Bool(next_u64() % 2 == 0),
                    3 => Value::None,
                    4 => Value::Float(f64::NAN),
                    5 => Value::List(vec![Value::from((next_u64() % 3) as i64)]),
                    6 => Value::List(vec![Value::List(vec![Value::Float(f64::NAN)])]),
                    _ => {
                        let mut d = Dict::new();
                        d.insert(key("k"), Value::from((next_u64() % 3) as i64));
                        Value::Dict(d)
                    }
                })
                .collect();
            let _ = py_uniq(&items); // must not panic, whatever the verdict
        }
    }

    /// Issue #498's own follow-up, finding N11: `anyOf`
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
