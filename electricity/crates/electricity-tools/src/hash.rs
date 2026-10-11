//! `plugins/hash.py::HashPlugin`, ported -- every algorithm `hashlib`
//! supports there, `hex`/`base64` output, `from_path` and the error
//! texts.

use crate::fs::{as_bool, encode_text, get, os_error, resolve_codec, value_error};
use crate::{CheckResult, ToolCall, ToolError, ToolPlugin, ToolResult};
use async_trait::async_trait;
use base64::Engine;
use digest::Digest;
use electricity_value::{Dict, Value};

/// `_SUPPORTED_ALGORITHMS`, already in the sorted order `hash:
/// unsupported algorithm ...`'s own message lists them in (Python's
/// `sorted()` over the set).
const SUPPORTED_ALGORITHMS_SORTED: &[&str] = &[
    "blake2b", "blake2s", "md5", "sha1", "sha224", "sha256", "sha384", "sha3_256", "sha3_512",
    "sha512",
];

fn digest_bytes(algorithm: &str, data: &[u8]) -> Vec<u8> {
    match algorithm {
        "md5" => md5::Md5::digest(data).to_vec(),
        "sha1" => sha1::Sha1::digest(data).to_vec(),
        "sha224" => sha2::Sha224::digest(data).to_vec(),
        "sha256" => sha2::Sha256::digest(data).to_vec(),
        "sha384" => sha2::Sha384::digest(data).to_vec(),
        "sha512" => sha2::Sha512::digest(data).to_vec(),
        "sha3_256" => sha3::Sha3_256::digest(data).to_vec(),
        "sha3_512" => sha3::Sha3_512::digest(data).to_vec(),
        "blake2b" => blake2::Blake2b512::digest(data).to_vec(),
        "blake2s" => blake2::Blake2s256::digest(data).to_vec(),
        other => unreachable!("unsupported algorithm reached digest_bytes: {other}"),
    }
}

pub struct HashPlugin;

#[async_trait(?Send)]
impl ToolPlugin for HashPlugin {
    fn name(&self) -> &str {
        "hash"
    }

    async fn execute(&self, params: Value, _call: &ToolCall<'_>) -> Result<ToolResult, ToolError> {
        let algorithm_raw = get(&params, "algorithm").cloned().unwrap_or(Value::None);
        let algorithm = if crate::fs::py_truthy(&algorithm_raw) {
            algorithm_raw.py_str().to_lowercase()
        } else {
            "sha256".to_string()
        };
        if !SUPPORTED_ALGORITHMS_SORTED.contains(&algorithm.as_str()) {
            let supported = SUPPORTED_ALGORITHMS_SORTED
                .iter()
                .map(|a| Value::Str(a.to_string()).py_repr())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(value_error(format!(
                "hash: unsupported algorithm {}. Supported: [{supported}]",
                Value::Str(algorithm).py_repr()
            )));
        }

        let output_format_raw = get(&params, "output_format")
            .cloned()
            .unwrap_or(Value::None);
        let output_format = if crate::fs::py_truthy(&output_format_raw) {
            output_format_raw.py_str().to_lowercase()
        } else {
            "hex".to_string()
        };
        if output_format != "hex" && output_format != "base64" {
            return Err(value_error(
                "hash: output_format must be 'hex' or 'base64'.",
            ));
        }

        let bytes = if as_bool(get(&params, "from_path"), false) {
            let src = match get(&params, "input") {
                Some(Value::Str(s)) if !s.is_empty() => s.clone(),
                _ => {
                    return Err(value_error(
                        "hash: from_path=True requires params['input'] path.",
                    ));
                }
            };
            let fs_path = crate::fs::expand_user_path(&src);
            std::fs::read(&fs_path.buf).map_err(|err| os_error(&err, &fs_path.display))?
        } else {
            let text = match get(&params, "input") {
                Some(Value::Str(s)) => s.clone(),
                _ => {
                    return Err(value_error(
                        "hash: in-memory mode requires params['input'] as str.",
                    ));
                }
            };
            let encoding_raw = get(&params, "encoding").cloned().unwrap_or(Value::None);
            let encoding_name = if crate::fs::py_truthy(&encoding_raw) {
                encoding_raw.py_str()
            } else {
                "utf-8".to_string()
            };
            let codec = resolve_codec(&encoding_name)
                .ok_or_else(|| crate::fs::unknown_encoding(&encoding_name))?;
            encode_text(codec, &text)?
        };

        let digest = digest_bytes(&algorithm, &bytes);
        let value = if output_format == "hex" {
            digest
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        } else {
            base64::engine::general_purpose::STANDARD.encode(&digest)
        };

        let mut raw = Dict::new();
        raw.insert(Value::Str("algorithm".to_string()), Value::Str(algorithm));
        raw.insert(
            Value::Str("output_format".to_string()),
            Value::Str(output_format),
        );
        Ok(ToolResult::new(Value::Str(value), Value::Dict(raw)))
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
        HashPlugin.execute(params, &call).await
    }

    #[tokio::test]
    async fn sha256_of_hello_matches_the_well_known_value() {
        let result = run(dict(vec![
            ("algorithm", Value::from("sha256")),
            ("input", Value::from("hello")),
        ]))
        .await
        .unwrap();
        assert_eq!(
            result.value,
            Value::from("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824")
        );
    }

    #[tokio::test]
    async fn md5_defaults_output_format_to_hex() {
        let result = run(dict(vec![
            ("algorithm", Value::from("md5")),
            ("input", Value::from("hello")),
        ]))
        .await
        .unwrap();
        assert_eq!(
            result.value,
            Value::from("5d41402abc4b2a76b9719d911017c592")
        );
    }

    #[tokio::test]
    async fn base64_output_format_matches_standard_padded_alphabet() {
        let result = run(dict(vec![
            ("algorithm", Value::from("md5")),
            ("input", Value::from("hello")),
            ("output_format", Value::from("base64")),
        ]))
        .await
        .unwrap();
        assert_eq!(result.value, Value::from("XUFAKrxLKna5cZ2REBfFkg=="));
    }

    #[tokio::test]
    async fn blake2b_defaults_to_64_bytes_and_blake2s_to_32() {
        let b = run(dict(vec![
            ("algorithm", Value::from("blake2b")),
            ("input", Value::from("x")),
        ]))
        .await
        .unwrap();
        assert_eq!(b.value.as_str().unwrap().len(), 128);
        let s = run(dict(vec![
            ("algorithm", Value::from("blake2s")),
            ("input", Value::from("x")),
        ]))
        .await
        .unwrap();
        assert_eq!(s.value.as_str().unwrap().len(), 64);
    }

    #[tokio::test]
    async fn unsupported_algorithm_lists_the_sorted_set() {
        let err = run(dict(vec![("algorithm", Value::from("bogus"))]))
            .await
            .unwrap_err();
        assert_eq!(
            err.message,
            "hash: unsupported algorithm 'bogus'. Supported: ['blake2b', 'blake2s', 'md5', \
             'sha1', 'sha224', 'sha256', 'sha384', 'sha3_256', 'sha3_512', 'sha512']"
        );
    }

    #[tokio::test]
    async fn bad_output_format_raises_after_the_algorithm_check() {
        let err = run(dict(vec![
            ("algorithm", Value::from("sha256")),
            ("output_format", Value::from("bogus")),
        ]))
        .await
        .unwrap_err();
        assert_eq!(
            err.message,
            "hash: output_format must be 'hex' or 'base64'."
        );
    }

    #[tokio::test]
    async fn in_memory_mode_requires_str_input() {
        let err = run(dict(vec![
            ("algorithm", Value::from("sha256")),
            ("input", Value::from(1i64)),
        ]))
        .await
        .unwrap_err();
        assert_eq!(
            err.message,
            "hash: in-memory mode requires params['input'] as str."
        );
    }

    #[tokio::test]
    async fn from_path_requires_a_non_empty_path() {
        let err = run(dict(vec![("from_path", Value::from(true))]))
            .await
            .unwrap_err();
        assert_eq!(
            err.message,
            "hash: from_path=True requires params['input'] path."
        );
    }

    #[tokio::test]
    async fn from_path_hashes_the_files_bytes() {
        let dir =
            std::env::temp_dir().join(format!("electricity-hash-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("f.txt");
        std::fs::write(&path, b"hello").unwrap();
        let result = run(dict(vec![
            ("algorithm", Value::from("sha256")),
            ("from_path", Value::from(true)),
            ("input", Value::from(path.to_str().unwrap())),
        ]))
        .await
        .unwrap();
        assert_eq!(
            result.value,
            Value::from("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824")
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
