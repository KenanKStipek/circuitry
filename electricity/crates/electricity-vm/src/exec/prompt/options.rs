//! `core/prompt.py::PromptRuntime._generation_options`/`_load_image`,
//! ported -- `params.temperature`/`max_tokens`/`stop` validation,
//! asset rendering/reading, and image-format sniffing.

use electricity_value::{Dict, IntValue, Value};
use std::io;
use std::path::{Component, Path, PathBuf};

/// The validated, lifted-out half of a prompt's own `params:` --
/// everything else stays in [`BuiltOptions::params`], untouched.
#[derive(Debug)]
pub(crate) struct BuiltOptions {
    pub temperature: Option<f64>,
    pub max_tokens: Option<i64>,
    pub stop: Vec<String>,
    pub params: Value,
}

fn key(name: &str) -> Value {
    Value::Str(name.to_string())
}

fn int_value_as_f64(value: &IntValue) -> f64 {
    match value {
        IntValue::Small(n) => *n as f64,
        IntValue::Big(big) => big.to_string().parse().unwrap_or(f64::INFINITY),
    }
}

/// `_generation_options`'s own `params`/`temperature`/`max_tokens`/
/// `stop` half (the asset/`json_schema` halves are
/// [`process_assets`]/the caller's own one-line `prompt_type` check).
/// *raw_params* is the prompt's own raw `params:` dict (a `Value::Dict`
/// literal, never rendered -- `effects::PromptOp::model_params`'s own
/// doc comment), or `None`/anything else when the effect has none.
pub(crate) fn build_options(
    name: &str,
    raw_params: Option<&Value>,
    deterministic: bool,
) -> Result<BuiltOptions, String> {
    let mut params: Dict = match raw_params {
        Some(Value::Dict(dict)) => dict.clone(),
        _ => Dict::new(),
    };

    let temperature_raw = params.shift_remove(&key("temperature"));
    let max_tokens_raw = params.shift_remove(&key("max_tokens"));
    let stop_raw = params.shift_remove(&key("stop"));

    let mut temperature = match &temperature_raw {
        None | Some(Value::None) => None,
        Some(Value::Bool(_)) => {
            return Err(format!(
                "Prompt '{name}': params.temperature must be a number, got {}",
                temperature_raw.as_ref().unwrap().py_repr()
            ));
        }
        Some(Value::Int(iv)) => Some(int_value_as_f64(iv)),
        Some(Value::Float(f)) => Some(*f),
        Some(other) => {
            return Err(format!(
                "Prompt '{name}': params.temperature must be a number, got {}",
                other.py_repr()
            ));
        }
    };
    if temperature.is_none() && deterministic {
        temperature = Some(0.0);
    }

    let max_tokens = match &max_tokens_raw {
        None | Some(Value::None) => None,
        Some(Value::Bool(_)) => {
            return Err(format!(
                "Prompt '{name}': params.max_tokens must be a positive integer, got {}",
                max_tokens_raw.as_ref().unwrap().py_repr()
            ));
        }
        Some(Value::Int(iv)) => {
            let n = match iv {
                IntValue::Small(n) => *n,
                // A genuinely huge `max_tokens` is vanishingly unlikely
                // in a real document (Python's own `int` has no such
                // ceiling) -- clamped rather than rejected, since the
                // document's own value is still a positive integer by
                // construction.
                IntValue::Big(_) if iv.is_negative() => i64::MIN,
                IntValue::Big(_) => i64::MAX,
            };
            if n < 1 {
                return Err(format!(
                    "Prompt '{name}': params.max_tokens must be a positive integer, got {}",
                    max_tokens_raw.as_ref().unwrap().py_repr()
                ));
            }
            Some(n)
        }
        Some(other) => {
            return Err(format!(
                "Prompt '{name}': params.max_tokens must be a positive integer, got {}",
                other.py_repr()
            ));
        }
    };

    let stop = match &stop_raw {
        None | Some(Value::None) => Vec::new(),
        Some(Value::Str(s)) => vec![s.clone()],
        Some(Value::List(items)) => {
            let mut out = Vec::with_capacity(items.len());
            let mut all_str = true;
            for item in items {
                match item {
                    Value::Str(s) => out.push(s.clone()),
                    _ => {
                        all_str = false;
                        break;
                    }
                }
            }
            if !all_str {
                return Err(format!(
                    "Prompt '{name}': params.stop must be a string or a list of strings, got {}",
                    stop_raw.as_ref().unwrap().py_repr()
                ));
            }
            out
        }
        Some(other) => {
            return Err(format!(
                "Prompt '{name}': params.stop must be a string or a list of strings, got {}",
                other.py_repr()
            ));
        }
    };

    Ok(BuiltOptions {
        temperature,
        max_tokens,
        stop,
        params: Value::Dict(params),
    })
}

// ---------------------------------------------------------------------
// Assets / image sniffing (`_load_image`, `_image_media_type`)
// ---------------------------------------------------------------------

/// One rendered, loaded asset -- an image (ready for
/// [`electricity_vm::adapter::ImageInput`]) or a non-image warning only.
#[derive(Debug)]
pub(crate) enum LoadedAsset {
    Image {
        image: crate::adapter::ImageInput,
        record: Value,
    },
    NonImageWarning(String),
}

/// `_image_media_type`: the four signatures Circuitry itself sniffs for,
/// bytes only -- never the file name.
fn image_media_type(data: &[u8]) -> Option<&'static str> {
    const SIGNATURES: &[(&[u8], &str)] = &[
        (b"\x89PNG\r\n\x1a\n", "image/png"),
        (b"\xff\xd8\xff", "image/jpeg"),
        (b"GIF87a", "image/gif"),
        (b"GIF89a", "image/gif"),
    ];
    for (signature, media_type) in SIGNATURES {
        if data.starts_with(signature) {
            return Some(media_type);
        }
    }
    if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

// `Path(raw).expanduser()` -- a bare leading `~` only (the current
// user's own `$HOME`); a `~username` form is left unchanged, the same
// narrow divergence `electricity-adapters::scripted`'s own copy of
// this documents (never exercised under a hermetic test `$HOME`).
fn expand_user_path(raw: &str) -> PathBuf {
    let path = Path::new(raw);
    let mut components = path.components();
    if let Some(Component::Normal(first)) = components.next() {
        if first == "~" {
            if let Ok(home) = std::env::var("HOME") {
                if !home.is_empty() {
                    let mut buf = PathBuf::from(home);
                    buf.extend(components);
                    return buf;
                }
            }
        }
    }
    path.to_path_buf()
}

fn strerror(err: &io::Error) -> String {
    let text = err.to_string();
    match text.rfind(" (os error ") {
        Some(idx) if text.ends_with(')') => text[..idx].to_string(),
        _ => text,
    }
}

/// CPython's `OSError.__str__`: `[Errno N] <strerror>: '<path>'`.
fn os_error_text(err: &io::Error, display: &str) -> String {
    let errno = err.raw_os_error().unwrap_or(0);
    format!("[Errno {errno}] {}: '{display}'", strerror(err))
}

/// `_load_image`: an `http(s)://` ref passes through as a URL; anything
/// else is read from a path relative to the process's own working
/// directory (never the document directory), `~`-expanded.
fn load_image(reference: &str) -> Result<LoadedAsset, String> {
    if reference.starts_with("http://") || reference.starts_with("https://") {
        let mut record = Dict::new();
        record.insert(key("kind"), Value::Str("image".to_string()));
        record.insert(key("ref"), Value::Str(reference.to_string()));
        return Ok(LoadedAsset::Image {
            image: crate::adapter::ImageInput {
                data: None,
                media_type: None,
                url: Some(reference.to_string()),
            },
            record: Value::Dict(record),
        });
    }
    let path = expand_user_path(reference);
    let data = std::fs::read(&path).map_err(|err| {
        format!(
            "image asset {} could not be read: {}",
            Value::Str(reference.to_string()).py_repr(),
            os_error_text(&err, reference)
        )
    })?;
    let Some(media_type) = image_media_type(&data) else {
        return Err(format!(
            "image asset {} is not a PNG, JPEG, GIF or WebP image",
            Value::Str(reference.to_string()).py_repr()
        ));
    };
    let sha256 = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(&data);
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };
    let mut record = Dict::new();
    record.insert(key("kind"), Value::Str("image".to_string()));
    record.insert(key("ref"), Value::Str(reference.to_string()));
    record.insert(key("media_type"), Value::Str(media_type.to_string()));
    record.insert(key("size"), Value::from(data.len() as i64));
    record.insert(key("sha256"), Value::Str(sha256));
    Ok(LoadedAsset::Image {
        image: crate::adapter::ImageInput {
            data: Some(data),
            media_type: Some(media_type.to_string()),
            url: None,
        },
        record: Value::Dict(record),
    })
}

/// One asset entry, already rendered (`render_template(asset.ref, ctx,
/// label=...).strip()`) and stripped -- the caller does the rendering
/// (it alone has the `ScopeRoot`/composition context), this just loads
/// it or reports the non-image warning.
pub(crate) fn load_asset(kind: &str, rendered_ref: &str) -> Result<LoadedAsset, String> {
    if kind != "image" {
        return Ok(LoadedAsset::NonImageWarning(format!(
            "asset kind {} is not sent to any adapter; only images are ({rendered_ref})",
            Value::Str(kind.to_string()).py_repr()
        )));
    }
    load_image(rendered_ref)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_with_no_temperature_gives_zero() {
        let built = build_options("p", None, true).unwrap();
        assert_eq!(built.temperature, Some(0.0));
    }

    #[test]
    fn an_explicit_temperature_is_kept_over_deterministic() {
        let mut params = Dict::new();
        params.insert(key("temperature"), Value::from(0.7));
        let built = build_options("p", Some(&Value::Dict(params)), true).unwrap();
        assert_eq!(built.temperature, Some(0.7));
    }

    #[test]
    fn a_bool_temperature_is_rejected() {
        let mut params = Dict::new();
        params.insert(key("temperature"), Value::Bool(true));
        let err = build_options("p", Some(&Value::Dict(params)), false).unwrap_err();
        assert_eq!(
            err,
            "Prompt 'p': params.temperature must be a number, got True"
        );
    }

    #[test]
    fn a_non_numeric_temperature_is_rejected() {
        let mut params = Dict::new();
        params.insert(key("temperature"), Value::from("warm"));
        let err = build_options("p", Some(&Value::Dict(params)), false).unwrap_err();
        assert_eq!(
            err,
            "Prompt 'p': params.temperature must be a number, got 'warm'"
        );
    }

    #[test]
    fn max_tokens_must_be_a_positive_integer() {
        let mut params = Dict::new();
        params.insert(key("max_tokens"), Value::from(0i64));
        let err = build_options("p", Some(&Value::Dict(params)), false).unwrap_err();
        assert_eq!(
            err,
            "Prompt 'p': params.max_tokens must be a positive integer, got 0"
        );
    }

    #[test]
    fn a_bool_max_tokens_is_rejected() {
        let mut params = Dict::new();
        params.insert(key("max_tokens"), Value::Bool(false));
        let err = build_options("p", Some(&Value::Dict(params)), false).unwrap_err();
        assert_eq!(
            err,
            "Prompt 'p': params.max_tokens must be a positive integer, got False"
        );
    }

    #[test]
    fn a_string_stop_becomes_a_one_element_list() {
        let mut params = Dict::new();
        params.insert(key("stop"), Value::from("X"));
        let built = build_options("p", Some(&Value::Dict(params)), false).unwrap();
        assert_eq!(built.stop, vec!["X".to_string()]);
    }

    #[test]
    fn a_non_string_list_entry_is_rejected() {
        let mut params = Dict::new();
        params.insert(
            key("stop"),
            Value::List(vec![Value::from("a"), Value::from(1i64)]),
        );
        let err = build_options("p", Some(&Value::Dict(params)), false).unwrap_err();
        assert_eq!(
            err,
            "Prompt 'p': params.stop must be a string or a list of strings, got ['a', 1]"
        );
    }

    #[test]
    fn other_params_pass_through_untouched() {
        let mut params = Dict::new();
        params.insert(key("temperature"), Value::from(0.2));
        params.insert(key("seed"), Value::from(1i64));
        let built = build_options("p", Some(&Value::Dict(params)), false).unwrap();
        let remaining = built.params.as_dict().unwrap();
        assert_eq!(remaining.len(), 1);
        assert!(remaining.contains_key(&key("seed")));
    }

    #[test]
    fn png_signature_is_sniffed() {
        let mut data = vec![0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];
        data.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(image_media_type(&data), Some("image/png"));
    }

    #[test]
    fn webp_signature_is_sniffed() {
        let mut data = b"RIFF".to_vec();
        data.extend_from_slice(&[0, 0, 0, 0]);
        data.extend_from_slice(b"WEBP");
        assert_eq!(image_media_type(&data), Some("image/webp"));
    }

    #[test]
    fn an_unrecognized_signature_is_none() {
        assert_eq!(image_media_type(b"not an image"), None);
    }

    #[test]
    fn a_url_ref_passes_through_without_reading_anything() {
        let outcome = load_asset("image", "https://example.com/pic.png").unwrap();
        let LoadedAsset::Image { image, record } = outcome else {
            panic!("expected an image");
        };
        assert_eq!(image.url, Some("https://example.com/pic.png".to_string()));
        assert_eq!(
            record.as_dict().unwrap().get(&key("ref")),
            Some(&Value::Str("https://example.com/pic.png".to_string()))
        );
    }

    #[test]
    fn a_non_image_kind_warns_and_never_touches_the_filesystem() {
        let outcome = load_asset("file", "/does/not/exist").unwrap();
        let LoadedAsset::NonImageWarning(warning) = outcome else {
            panic!("expected a warning");
        };
        assert_eq!(
            warning,
            "asset kind 'file' is not sent to any adapter; only images are (/does/not/exist)"
        );
    }

    #[test]
    fn an_unreadable_local_image_path_fails_with_the_os_error_text() {
        let err = load_asset("image", "/does/not/exist/pic.png").unwrap_err();
        assert!(err.starts_with("image asset '/does/not/exist/pic.png' could not be read: "));
    }

    #[test]
    fn a_readable_non_image_file_fails_with_the_format_text() {
        let dir =
            std::env::temp_dir().join(format!("electricity-asset-test-{}", std::process::id()));
        std::fs::write(&dir, b"not an image").unwrap();
        let err = load_asset("image", dir.to_str().unwrap()).unwrap_err();
        std::fs::remove_file(&dir).unwrap();
        assert!(err.ends_with("is not a PNG, JPEG, GIF or WebP image"));
    }
}
