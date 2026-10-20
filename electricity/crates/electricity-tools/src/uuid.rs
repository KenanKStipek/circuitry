//! `plugins/uuid.py::UuidPlugin`, ported -- v1/v4/v5, `count`, `hex`,
//! the named namespaces and a custom namespace parsed exactly as
//! `uuid.UUID(str)` does.

use crate::fs::{as_bool, get, value_error};
use crate::{CheckResult, ToolCall, ToolError, ToolPlugin, ToolResult};
use async_trait::async_trait;
use electricity_value::pycompat::py_int;
use electricity_value::{Dict, Value};

fn named_namespace(name: &str) -> Option<uuid::Uuid> {
    match name.to_ascii_lowercase().as_str() {
        "dns" => Some(uuid::Uuid::NAMESPACE_DNS),
        "url" => Some(uuid::Uuid::NAMESPACE_URL),
        "oid" => Some(uuid::Uuid::NAMESPACE_OID),
        "x500" => Some(uuid::Uuid::NAMESPACE_X500),
        _ => None,
    }
}

/// `uuid.UUID(hex)`'s own parse, exactly: strip `urn:`/`uuid:`
/// (literal substring removal, not prefix-only -- `str.replace`),
/// strip leading/trailing `{`/`}`, strip every `-`, require length 32,
/// then `int(hex, 16)`.
fn parse_python_uuid(raw: &str) -> Result<[u8; 16], String> {
    let without_urn = raw.replace("urn:", "").replace("uuid:", "");
    let without_braces = without_urn.trim_matches(|c| c == '{' || c == '}');
    let hex: String = without_braces.chars().filter(|&c| c != '-').collect();
    if hex.len() != 32 {
        return Err("badly formed hexadecimal UUID string".to_string());
    }
    let value = u128::from_str_radix(&hex, 16).map_err(|_| {
        format!(
            "invalid literal for int() with base 16: {}",
            Value::Str(hex.clone()).py_repr()
        )
    })?;
    Ok(value.to_be_bytes())
}

/// `uuid.uuid1()` -- compared by shape only (issue #460's own test
/// plan), so a fresh random node id with the multicast bit set
/// (Python's own `getnode()` fallback when no real MAC is available)
/// is as faithful as this plugin needs to be.
fn uuid1() -> uuid::Uuid {
    let mut node = [0u8; 6];
    let random = uuid::Uuid::new_v4().into_bytes();
    node.copy_from_slice(&random[..6]);
    node[0] |= 0x01;
    let context = uuid::ContextV1::new_random();
    let timestamp = uuid::Timestamp::now(&context);
    uuid::Uuid::new_v1(timestamp, &node)
}

fn uuid5(params: &Value) -> Result<uuid::Uuid, ToolError> {
    let name = match get(params, "name") {
        Some(Value::Str(s)) if !s.is_empty() => s.clone(),
        _ => return Err(value_error("uuid: v5 requires params['name'].")),
    };
    let ns_value = get(params, "namespace").cloned().unwrap_or(Value::None);
    let namespace = match &ns_value {
        Value::Str(s) if named_namespace(s).is_some() => named_namespace(s).unwrap(),
        Value::Str(s) if !s.is_empty() => {
            let bytes = parse_python_uuid(s).map_err(|exc| {
                value_error(format!(
                    "uuid: invalid v5 namespace {}: {exc}",
                    ns_value.py_repr()
                ))
            })?;
            uuid::Uuid::from_bytes(bytes)
        }
        _ => return Err(value_error("uuid: v5 requires params['namespace'].")),
    };
    Ok(uuid::Uuid::new_v5(&namespace, name.as_bytes()))
}

pub struct UuidPlugin;

#[async_trait(?Send)]
impl ToolPlugin for UuidPlugin {
    fn name(&self) -> &str {
        "uuid"
    }

    async fn execute(&self, params: Value, _call: &ToolCall<'_>) -> Result<ToolResult, ToolError> {
        // `version = int(params.get("version") or 4)` -- the `or 4`
        // short-circuits *before* `int()` ever runs: a falsy value
        // (absent, `null`, `0`, `""`, ...) becomes the literal `4`,
        // never passed through `int()`'s own coercion/error path.
        let version_raw = get(&params, "version").cloned().unwrap_or(Value::None);
        let version = if crate::fs::py_truthy(&version_raw) {
            py_int(&version_raw).map_err(|err| ToolError {
                py_class: err.py_class,
                message: err.message,
                cause: crate::ErrorCause::None,
            })?
        } else {
            4
        };
        if version != 1 && version != 4 && version != 5 {
            return Err(value_error(format!(
                "uuid: unsupported version {version}; expected 1|4|5."
            )));
        }

        let count_raw = get(&params, "count").cloned().unwrap_or(Value::None);
        let count = if crate::fs::py_truthy(&count_raw) {
            py_int(&count_raw).map_err(|err| ToolError {
                py_class: err.py_class,
                message: err.message,
                cause: crate::ErrorCause::None,
            })?
        } else {
            1
        };
        if count < 1 {
            return Err(value_error("uuid: count must be >= 1."));
        }

        let as_hex = as_bool(get(&params, "hex"), false);

        // Never `Vec::with_capacity(count as usize)` -- a huge *count*
        // would abort the process on that allocation alone, before a
        // v5 call's own per-iteration `uuid5` ever gets a chance to
        // raise on its first iteration the way CPython's loop does
        // (issue #460's own fix pass). An unbounded `Vec`
        // still does the same real work CPython's own loop would for a
        // huge *count* of v1/v4 ids -- that's parity, not a bug; the
        // bug was paying for the whole allocation up front regardless
        // of whether the loop would ever get that far.
        let mut ids: Vec<String> = Vec::new();
        for _ in 0..count {
            let generated = match version {
                1 => uuid1(),
                5 => uuid5(&params)?,
                _ => uuid::Uuid::new_v4(),
            };
            ids.push(if as_hex {
                generated.simple().to_string()
            } else {
                generated.hyphenated().to_string()
            });
        }

        let value = if count > 1 {
            Value::List(ids.into_iter().map(Value::Str).collect())
        } else {
            Value::Str(ids.into_iter().next().expect("count >= 1"))
        };

        let mut raw = Dict::new();
        raw.insert(Value::Str("version".to_string()), Value::from(version));
        raw.insert(Value::Str("count".to_string()), Value::from(count));
        raw.insert(Value::Str("hex".to_string()), Value::from(as_hex));
        Ok(ToolResult::new(value, Value::Dict(raw)))
    }

    fn check(&self) -> CheckResult {
        CheckResult {
            ok: true,
            missing: Vec::new(),
            message: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::CancellationToken;

    fn dict(pairs: Vec<(&str, Value)>) -> Value {
        let mut d = Dict::new();
        for (k, v) in pairs {
            d.insert(Value::Str(k.to_string()), v);
        }
        Value::Dict(d)
    }

    async fn run(params: Value) -> Result<ToolResult, ToolError> {
        let token = CancellationToken::new();
        let call = ToolCall {
            timeout_seconds: 30,
            config: &Value::None,
            token: &token,
            armed: false,
        };
        UuidPlugin.execute(params, &call).await
    }

    #[tokio::test]
    async fn v5_with_a_named_namespace_matches_pythons_uuid5() {
        let result = run(dict(vec![
            ("version", Value::from(5i64)),
            ("namespace", Value::from("dns")),
            ("name", Value::from("example.com")),
        ]))
        .await
        .unwrap();
        assert_eq!(
            result.value,
            Value::from("cfbff0d1-9375-5685-968c-48ce8b15ae17")
        );
    }

    #[tokio::test]
    async fn v5_with_a_named_namespace_is_case_insensitive() {
        let result = run(dict(vec![
            ("version", Value::from(5i64)),
            ("namespace", Value::from("URL")),
            ("name", Value::from("example.com")),
        ]))
        .await
        .unwrap();
        assert_eq!(
            result.value,
            Value::from("a5cf6e8e-4cfa-5f31-a804-6de6d1245e26")
        );
    }

    #[tokio::test]
    async fn v5_with_a_custom_namespace_string_every_accepted_spelling() {
        for spelling in [
            "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
            "urn:uuid:6ba7b810-9dad-11d1-80b4-00c04fd430c8",
            "{6ba7b810-9dad-11d1-80b4-00c04fd430c8}",
            "6ba7b8109dad11d180b400c04fd430c8",
        ] {
            let result = run(dict(vec![
                ("version", Value::from(5i64)),
                ("namespace", Value::from(spelling)),
                ("name", Value::from("x")),
            ]))
            .await
            .unwrap();
            assert_eq!(
                result.value,
                Value::from("05b16a01-46c6-56dd-bd6e-c6dfb4a1427a"),
                "spelling {spelling}"
            );
        }
    }

    #[tokio::test]
    async fn v5_requires_name_before_namespace() {
        let err = run(dict(vec![("version", Value::from(5i64))]))
            .await
            .unwrap_err();
        assert_eq!(err.message, "uuid: v5 requires params['name'].");
    }

    #[tokio::test]
    async fn v5_requires_a_namespace() {
        let err = run(dict(vec![
            ("version", Value::from(5i64)),
            ("name", Value::from("x")),
        ]))
        .await
        .unwrap_err();
        assert_eq!(err.message, "uuid: v5 requires params['namespace'].");
    }

    #[tokio::test]
    async fn v5_with_a_badly_formed_namespace_reports_cpythons_own_text() {
        let err = run(dict(vec![
            ("version", Value::from(5i64)),
            ("name", Value::from("x")),
            ("namespace", Value::from("not-a-uuid")),
        ]))
        .await
        .unwrap_err();
        assert_eq!(
            err.message,
            "uuid: invalid v5 namespace 'not-a-uuid': badly formed hexadecimal UUID string"
        );
    }

    #[tokio::test]
    async fn unsupported_version_names_itself() {
        let err = run(dict(vec![("version", Value::from(2i64))]))
            .await
            .unwrap_err();
        assert_eq!(err.message, "uuid: unsupported version 2; expected 1|4|5.");
    }

    #[tokio::test]
    async fn version_zero_and_empty_string_both_default_to_4() {
        for version in [Value::from(0i64), Value::from("")] {
            let result = run(dict(vec![("version", version)])).await.unwrap();
            let raw = result.raw.as_dict().unwrap();
            assert_eq!(
                raw.get(&Value::Str("version".to_string())),
                Some(&Value::from(4i64))
            );
        }
    }

    #[tokio::test]
    async fn count_zero_defaults_to_one_but_negative_fails() {
        let result = run(dict(vec![("count", Value::from(0i64))])).await.unwrap();
        assert!(matches!(result.value, Value::Str(_)));
        let err = run(dict(vec![("count", Value::from(-1i64))]))
            .await
            .unwrap_err();
        assert_eq!(err.message, "uuid: count must be >= 1.");
    }

    #[tokio::test]
    async fn a_huge_count_with_an_invalid_v5_call_fails_fast_without_preallocating() {
        // Pre-#460-fix-pass, `Vec::with_capacity(count as usize)` ran
        // *before* the loop that would otherwise raise on its very
        // first iteration -- a huge `count` aborted the process before
        // `uuid5` ever got a chance to reject the missing `name`.
        let err = run(dict(vec![
            ("version", Value::from(5i64)),
            ("count", Value::from(i64::MAX)),
        ]))
        .await
        .unwrap_err();
        assert_eq!(err.message, "uuid: v5 requires params['name'].");
    }

    #[tokio::test]
    async fn count_greater_than_one_gives_a_list() {
        let result = run(dict(vec![
            ("version", Value::from(5i64)),
            ("namespace", Value::from("dns")),
            ("name", Value::from("example.com")),
            ("count", Value::from(3i64)),
        ]))
        .await
        .unwrap();
        match &result.value {
            Value::List(items) => assert_eq!(items.len(), 3),
            other => panic!("expected a list, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn hex_true_gives_32_chars_without_dashes() {
        let result = run(dict(vec![
            ("version", Value::from(5i64)),
            ("namespace", Value::from("dns")),
            ("name", Value::from("example.com")),
            ("hex", Value::from(true)),
        ]))
        .await
        .unwrap();
        let s = result.value.as_str().unwrap();
        assert_eq!(s.len(), 32);
        assert!(!s.contains('-'));
    }

    #[tokio::test]
    async fn v4_and_v1_are_well_formed_uuid_shapes() {
        // tools §7 case 9: v1/v4 output is host/time-dependent and
        // never byte-compared -- only shape (and, for v1, a
        // monotonically increasing timestamp component across calls).
        let v4 = run(dict(vec![("version", Value::from(4i64))]))
            .await
            .unwrap();
        let v4_str = v4.value.as_str().unwrap();
        assert_eq!(v4_str.chars().nth(14), Some('4'));

        let v1_first = run(dict(vec![("version", Value::from(1i64))]))
            .await
            .unwrap();
        let v1_second = run(dict(vec![("version", Value::from(1i64))]))
            .await
            .unwrap();
        for v1 in [&v1_first, &v1_second] {
            assert_eq!(v1.value.as_str().unwrap().chars().nth(14), Some('1'));
        }
        let first_uuid = uuid::Uuid::parse_str(v1_first.value.as_str().unwrap()).unwrap();
        let second_uuid = uuid::Uuid::parse_str(v1_second.value.as_str().unwrap()).unwrap();
        assert!(
            second_uuid.get_timestamp().unwrap().to_gregorian().0
                >= first_uuid.get_timestamp().unwrap().to_gregorian().0
        );
    }

    #[test]
    fn parse_python_uuid_matches_cpythons_own_algorithm() {
        assert_eq!(
            parse_python_uuid("urn:uuid:6ba7b810-9dad-11d1-80b4-00c04fd430c8"),
            parse_python_uuid("6ba7b8109dad11d180b400c04fd430c8")
        );
        assert_eq!(
            parse_python_uuid("too-short"),
            Err("badly formed hexadecimal UUID string".to_string())
        );
    }
}
