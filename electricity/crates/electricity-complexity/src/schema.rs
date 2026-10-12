//! `_walk_schema`/`_declared_output_tokens` (`core/complexity.py:539-612`):
//! the `output_schema` and `output_size` signals' JSON-Schema-ish walk.

use crate::{CHARS_PER_TOKEN, ScoreError, TOKENS_PER_ITEM, as_number};
use electricity_value::{Dict, Value};

fn dict_get<'a>(dict: &'a Dict, key: &str) -> Option<&'a Value> {
    dict.get(&Value::Str(key.to_string()))
}

fn walk_any(node: &Value, depth: i64) -> (i64, i64) {
    match node {
        Value::Dict(dict) => walk_dict(dict, depth),
        _ => (0, 0),
    }
}

/// `_walk_schema` (`core/complexity.py:539`): `(max_depth, field_count)`.
fn walk_dict(dict: &Dict, depth: i64) -> (i64, i64) {
    let mut max_depth = depth;
    let mut fields = 0i64;

    if let Some(Value::Dict(properties)) = dict_get(dict, "properties") {
        fields += properties.len() as i64;
        for child in properties.values() {
            let (d, f) = walk_any(child, depth + 1);
            max_depth = max_depth.max(d);
            fields += f;
        }
    }

    match dict_get(dict, "items") {
        Some(items @ Value::Dict(_)) => {
            let (d, f) = walk_any(items, depth + 1);
            max_depth = max_depth.max(d);
            fields += f;
        }
        Some(Value::List(items)) => {
            for child in items {
                let (d, f) = walk_any(child, depth + 1);
                max_depth = max_depth.max(d);
                fields += f;
            }
        }
        _ => {}
    }

    for key in ["oneOf", "anyOf", "allOf"] {
        if let Some(Value::List(branches)) = dict_get(dict, key) {
            for child in branches {
                let (d, f) = walk_any(child, depth + 1);
                max_depth = max_depth.max(d);
                fields += f;
            }
        }
    }

    (max_depth, fields)
}

/// `_walk_schema(schema)` at the top level: `schema: None` or a
/// non-mapping gives `(0, 0)`; a mapping root is depth 1.
pub fn walk_schema(schema: Option<&Dict>) -> (i64, i64) {
    match schema {
        Some(dict) => walk_dict(dict, 1),
        None => (0, 0),
    }
}

/// Python's list `max()`: keep the current best, replacing it only when
/// the next element is strictly greater (`core/complexity.py:612`'s own
/// `max(candidates) if candidates else 0.0`). The candidates this
/// function ever sees are already filtered to `> 0`, so none is `NaN`.
fn py_list_max(values: &[f64]) -> Option<f64> {
    let mut iter = values.iter();
    let mut best = *iter.next()?;
    for &v in iter {
        if v > best {
            best = v;
        }
    }
    Some(best)
}

/// `_declared_output_tokens` (`core/complexity.py:580-612`): the largest
/// declared output size in estimated tokens, plus the *names* of the
/// keys that contributed a candidate (sorted for the note by the
/// caller) -- the candidate values themselves are part of `detail`,
/// which this port drops (see `tags::extract_references`'s own note).
pub fn declared_output_tokens(
    schema: Option<&Dict>,
    params: Option<&Dict>,
) -> Result<(f64, Vec<&'static str>), ScoreError> {
    let mut detail_keys: Vec<&'static str> = Vec::new();
    let mut candidates: Vec<f64> = Vec::new();

    if let Some(params) = params {
        for key in ["max_tokens", "num_predict", "max_output_tokens"] {
            if let Some(raw) = dict_get(params, key) {
                if let Some(value) = as_number(raw)? {
                    if value > 0.0 {
                        detail_keys.push(key);
                        candidates.push(value);
                    }
                }
            }
        }
    }

    if let Some(schema) = schema {
        if let Some(raw) = dict_get(schema, "maxLength") {
            if let Some(value) = as_number(raw)? {
                if value > 0.0 {
                    detail_keys.push("maxLength");
                    candidates.push(value / CHARS_PER_TOKEN as f64);
                }
            }
        }

        if let Some(raw) = dict_get(schema, "maxItems") {
            if let Some(max_items) = as_number(raw)? {
                if max_items > 0.0 {
                    detail_keys.push("maxItems");
                    let per_item = match dict_get(schema, "items") {
                        Some(Value::Dict(items)) => match dict_get(items, "maxLength") {
                            Some(raw_len) => as_number(raw_len)?,
                            None => None,
                        },
                        _ => None,
                    };
                    let item_tokens = match per_item {
                        Some(p) if p > 0.0 => p / CHARS_PER_TOKEN as f64,
                        _ => TOKENS_PER_ITEM as f64,
                    };
                    candidates.push(max_items * item_tokens);
                }
            }
        }

        if let Some(raw) = dict_get(schema, "maxProperties") {
            if let Some(value) = as_number(raw)? {
                if value > 0.0 {
                    detail_keys.push("maxProperties");
                    candidates.push(value * TOKENS_PER_ITEM as f64);
                }
            }
        }
    }

    Ok((py_list_max(&candidates).unwrap_or(0.0), detail_keys))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dict_of(pairs: Vec<(&str, Value)>) -> Dict {
        let mut d = Dict::new();
        for (k, v) in pairs {
            d.insert(Value::Str(k.to_string()), v);
        }
        d
    }

    #[test]
    fn no_schema_is_depth_zero_fields_zero() {
        assert_eq!(walk_schema(None), (0, 0));
    }

    #[test]
    fn empty_schema_is_depth_one_fields_zero() {
        assert_eq!(walk_schema(Some(&Dict::new())), (1, 0));
    }

    #[test]
    fn nested_properties_count_every_key() {
        let inner = dict_of(vec![("type", Value::from("string"))]);
        let props = dict_of(vec![("a", Value::Dict(inner)), ("b", Value::from(1i64))]);
        let schema = dict_of(vec![("properties", Value::Dict(props))]);
        assert_eq!(walk_schema(Some(&schema)), (2, 2));
    }

    #[test]
    fn items_as_mapping_walks_one_level_deeper_with_no_field_of_its_own() {
        let items = dict_of(vec![("type", Value::from("string"))]);
        let schema = dict_of(vec![("items", Value::Dict(items))]);
        assert_eq!(walk_schema(Some(&schema)), (2, 0));
    }

    #[test]
    fn items_as_list_walks_each_element() {
        let a = dict_of(vec![("type", Value::from("string"))]);
        let b = dict_of(vec![("type", Value::from("number"))]);
        let schema = dict_of(vec![(
            "items",
            Value::List(vec![Value::Dict(a), Value::Dict(b)]),
        )]);
        assert_eq!(walk_schema(Some(&schema)), (2, 0));
    }

    #[test]
    fn declared_output_tokens_takes_the_max_of_the_candidates() {
        let params = dict_of(vec![("max_tokens", Value::from(100i64))]);
        let schema = dict_of(vec![("maxLength", Value::from(4000i64))]);
        let (tokens, keys) = declared_output_tokens(Some(&schema), Some(&params)).unwrap();
        assert_eq!(tokens, 1000.0);
        assert_eq!(keys, vec!["max_tokens", "maxLength"]);
    }

    #[test]
    fn nothing_declared_is_zero_with_no_keys() {
        assert_eq!(declared_output_tokens(None, None).unwrap(), (0.0, vec![]));
    }
}
