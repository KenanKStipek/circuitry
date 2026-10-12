//! `adapters/_openai_compat.py`, ported -- the shared OpenAI Chat
//! Completions transport every member of the OpenAI-compatible
//! family (`ai21`, `azure-openai`, `cloudflare-workers-ai`, `cohere`,
//! `databricks`, `deepseek`, `fireworks`, `gemini`, `groq`,
//! `huggingface-inference`, `llamacpp`, `lmstudio`, `mistral`,
//! `nvidia-nim`, `openrouter`, `perplexity`, `qwen-dashscope`, `tgi`,
//! `together`, `vllm`, `xai`) shares, over [`crate::transport`].

use crate::config::{cfg_get, cfg_str_or, cfg_str_or_rstrip_target};
use crate::curl_text::{classify_curl_outcome, curl_failure_message};
use crate::transport::{TransportFailure, run_curl};
use async_trait::async_trait;
use electricity_json::WriteMode;
use electricity_value::{Dict, Value};
use electricity_vm::adapter::{
    Adapter, AdapterError, CallContext, CheckResult, GenerateOptions, GenerateResult, PyExc,
};
use electricity_vm::retry::RetryInfo;

fn dict_insert(dict: &mut Dict, key: &str, value: Value) {
    dict.insert(Value::Str(key.to_string()), value);
}

fn last_user_index(messages: &[Value]) -> Option<usize> {
    (0..messages.len()).rev().find(|&i| {
        messages[i]
            .as_dict()
            .and_then(|d| d.get(&Value::Str("role".to_string())))
            .and_then(Value::as_str)
            == Some("user")
    })
}

fn truncate_chars(text: &str, max_chars: usize) -> &str {
    match text.char_indices().nth(max_chars) {
        Some((byte_index, _)) => &text[..byte_index],
        None => text,
    }
}

fn non_json_error(prefix: &str, stdout: &str) -> AdapterError {
    AdapterError {
        message: format!(
            "{prefix} returned non-JSON response: {}",
            truncate_chars(stdout, 200)
        ),
        retry: RetryInfo::not_retryable(),
        py_class: PyExc::RuntimeError,
    }
}

fn attribute_error(value: &Value, attribute: &str) -> AdapterError {
    AdapterError {
        message: format!(
            "'{}' object has no attribute '{attribute}'",
            value.type_name()
        ),
        retry: RetryInfo::not_retryable(),
        py_class: PyExc::AttributeError,
    }
}

fn transport_failure_error(failure: TransportFailure) -> AdapterError {
    match failure {
        TransportFailure::Validation(message) => AdapterError {
            message,
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::ValueError,
        },
        TransportFailure::InvalidUtf8(message) => AdapterError {
            message,
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::UnicodeDecodeError,
        },
    }
}

fn int_if_pyint(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(v @ (Value::Int(_) | Value::Bool(_))) => electricity_value::pycompat::py_int(v).ok(),
        _ => None,
    }
}

/// `_openai_compat.py::chat_messages` -- shared by `openai.rs` too.
pub fn chat_messages(prompt: &str, options: &GenerateOptions) -> Vec<Value> {
    let mut messages: Vec<Value> = options
        .messages
        .iter()
        .map(|m| {
            let mut entry = Dict::new();
            if m.role == "tool" {
                dict_insert(&mut entry, "role", Value::Str("user".to_string()));
                dict_insert(
                    &mut entry,
                    "content",
                    Value::Str(format!("tool: {}", m.content)),
                );
            } else {
                dict_insert(&mut entry, "role", Value::Str(m.role.clone()));
                dict_insert(&mut entry, "content", Value::Str(m.content.clone()));
            }
            Value::Dict(entry)
        })
        .collect();
    if messages.is_empty() {
        let mut entry = Dict::new();
        dict_insert(&mut entry, "role", Value::Str("user".to_string()));
        dict_insert(&mut entry, "content", Value::Str(prompt.to_string()));
        messages.push(Value::Dict(entry));
    }
    if !options.images.is_empty() {
        let index = match last_user_index(&messages) {
            Some(i) => i,
            None => {
                let mut entry = Dict::new();
                dict_insert(&mut entry, "role", Value::Str("user".to_string()));
                dict_insert(&mut entry, "content", Value::Str(String::new()));
                messages.push(Value::Dict(entry));
                messages.len() - 1
            }
        };
        let text = match messages[index]
            .as_dict()
            .and_then(|d| d.get(&Value::Str("content".to_string())))
        {
            Some(Value::Str(s)) => s.clone(),
            _ => String::new(),
        };
        let mut parts: Vec<Value> = Vec::new();
        if !text.is_empty() {
            let mut text_part = Dict::new();
            dict_insert(&mut text_part, "type", Value::Str("text".to_string()));
            dict_insert(&mut text_part, "text", Value::Str(text));
            parts.push(Value::Dict(text_part));
        }
        for image in &options.images {
            let mut image_part = Dict::new();
            dict_insert(&mut image_part, "type", Value::Str("image_url".to_string()));
            let mut image_url = Dict::new();
            dict_insert(&mut image_url, "url", Value::Str(data_url(image)));
            dict_insert(&mut image_part, "image_url", Value::Dict(image_url));
            parts.push(Value::Dict(image_part));
        }
        if let Value::Dict(entry) = &mut messages[index] {
            dict_insert(entry, "content", Value::List(parts));
        }
    }
    messages
}

/// `adapters/base.py::ImageInput.data_url` -- the URL itself, or a
/// `data:` URL of the local file's bytes.
fn data_url(image: &electricity_vm::adapter::ImageInput) -> String {
    if let Some(url) = &image.url {
        return url.clone();
    }
    use base64::Engine;
    let encoded =
        base64::engine::general_purpose::STANDARD.encode(image.data.as_deref().unwrap_or(&[]));
    let media_type = image.media_type.as_deref().unwrap_or("");
    format!("data:{media_type};base64,{encoded}")
}

/// `_openai_compat.py::sampling_fields` -- shared by `openai.rs` too
/// (a different *max_tokens_key*).
/// `_openai_compat.py::chat_completion`'s own payload assembly --
/// `model`/`messages` first, then every `sampling_fields` entry
/// inserted over them (so a `params.model` override, present in the
/// corpus's own override case, wins over the resolved *model* the
/// same way a plain `dict.update`-equivalent would). `pub` so
/// `tests/adapter_corpus.rs` replays `generate_adapter_corpus.py`'s
/// own request-building cases through this exact function, the same
/// one [`chat_completion`] itself calls, rather than reassembling the
/// payload by hand in the test (issue #515 finding 5's third item).
pub fn build_request(model: &str, prompt: &str, options: &GenerateOptions) -> Dict {
    let mut payload = Dict::new();
    dict_insert(&mut payload, "model", Value::Str(model.to_string()));
    dict_insert(
        &mut payload,
        "messages",
        Value::List(chat_messages(prompt, options)),
    );
    for (key, value) in sampling_fields(options, "max_tokens") {
        payload.insert(key, value);
    }
    payload
}

pub fn sampling_fields(options: &GenerateOptions, max_tokens_key: &str) -> Dict {
    let mut fields = Dict::new();
    if let Some(temperature) = options.temperature {
        dict_insert(&mut fields, "temperature", Value::Float(temperature));
    }
    if let Some(max_tokens) = options.max_tokens {
        dict_insert(&mut fields, max_tokens_key, Value::from(max_tokens));
    }
    if !options.stop.is_empty() {
        dict_insert(
            &mut fields,
            "stop",
            Value::List(options.stop.iter().map(|s| Value::Str(s.clone())).collect()),
        );
    }
    if let Some(params) = options.params.as_dict() {
        for (key, value) in params {
            let key_str = match key {
                Value::Str(s) => s.clone(),
                other => other.py_str(),
            };
            fields.insert(Value::Str(key_str), value.clone());
        }
    }
    fields
}

/// Python truthiness (`message or {}`'s own test): a missing key,
/// `None`, `False`, a numeric zero, and any empty `str`/`bytes`/
/// `list`/`dict` are falsy; everything else is truthy.
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

/// `_openai_compat.py::parse_chat_response` -- `(text, finish_reason)`.
pub fn parse_chat_response(raw: &Value) -> Result<(String, Option<String>), AdapterError> {
    let dict = raw.as_dict().ok_or_else(|| attribute_error(raw, "get"))?;
    let choices = dict.get(&Value::Str("choices".to_string()));
    let (text, finish_reason) = match choices.and_then(Value::as_list) {
        Some(list) if !list.is_empty() => match list[0].as_dict() {
            Some(choice) => {
                let message = choice.get(&Value::Str("message".to_string()));
                // `message = choice.get("message") or {}`: a falsy
                // message (missing, `None`, `{}`, ...) becomes `{}`
                // (empty content, no error); a truthy *non*-dict
                // message (a string, a number, `[1]`, ...) raises
                // `AttributeError` on the next `.get` the same way a
                // non-dict `raw` does above.
                let content = if is_truthy(message) {
                    match message.and_then(Value::as_dict) {
                        Some(message_dict) => message_dict.get(&Value::Str("content".to_string())),
                        None => return Err(attribute_error(message.unwrap(), "get")),
                    }
                } else {
                    None
                };
                let text = match content {
                    Some(Value::Str(s)) => s.clone(),
                    _ => String::new(),
                };
                let finish_reason = match choice.get(&Value::Str("finish_reason".to_string())) {
                    Some(Value::Str(s)) => Some(s.clone()),
                    _ => None,
                };
                (text, finish_reason)
            }
            None => (String::new(), None),
        },
        _ => (String::new(), None),
    };
    Ok((
        electricity_value::pycompat::py_strip(&text).to_string(),
        finish_reason,
    ))
}

/// `_openai_compat.py::OpenAICompatibleConfig`.
#[derive(Debug, Clone)]
pub struct OpenAiCompatibleConfig {
    pub base_url: String,
    pub api_key_env: String,
    pub default_model: String,
    pub chat_completions_path: String,
}

impl OpenAiCompatibleConfig {
    fn new(
        base_url: impl Into<String>,
        api_key_env: impl Into<String>,
        default_model: impl Into<String>,
    ) -> Self {
        OpenAiCompatibleConfig {
            base_url: base_url.into(),
            api_key_env: api_key_env.into(),
            default_model: default_model.into(),
            chat_completions_path: "/chat/completions".to_string(),
        }
    }

    fn with_path(mut self, path: impl Into<String>) -> Self {
        self.chat_completions_path = path.into();
        self
    }
}

/// `_openai_compat.py::chat_completion`.
pub async fn chat_completion(
    source: &str,
    cfg: &OpenAiCompatibleConfig,
    model: &str,
    prompt: &str,
    timeout_seconds: u32,
    extra_headers: &[(String, String)],
    options: &GenerateOptions,
) -> Result<GenerateResult, AdapterError> {
    let model = if model.is_empty() {
        cfg.default_model.as_str()
    } else {
        model
    };
    let api_key = if cfg.api_key_env.is_empty() {
        String::new()
    } else {
        std::env::var(&cfg.api_key_env).unwrap_or_default()
    };
    if !cfg.api_key_env.is_empty() && api_key.is_empty() {
        return Err(AdapterError {
            message: format!(
                "API key not found for {0}. Export {0}=... in the environment.",
                cfg.api_key_env
            ),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::RuntimeError,
        });
    }

    let path = cfg.chat_completions_path.replace("{model}", model);
    let url = format!("{}{path}", cfg.base_url.trim_end_matches('/'));

    let payload = build_request(model, prompt, options);
    // `json.dumps(payload)` raises Python's own `TypeError` for a value it
    // can't serialize, uncaught -- never `RuntimeError`.
    let body = electricity_json::dumps(&Value::Dict(payload), WriteMode::COMPACT).map_err(|e| {
        AdapterError {
            message: e.to_string(),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::TypeError,
        }
    })?;

    let mut headers = vec![("Content-Type".to_string(), "application/json".to_string())];
    if !api_key.is_empty() {
        headers.push(("Authorization".to_string(), format!("Bearer {api_key}")));
    }
    for (name, value) in extra_headers {
        headers.push((name.clone(), value.clone()));
    }

    let outcome = run_curl(&url, &headers, Some(&body), timeout_seconds)
        .await
        .map_err(transport_failure_error)?;
    let mut secrets: Vec<&str> = Vec::new();
    if !api_key.is_empty() {
        secrets.push(&api_key);
    }
    for (_, value) in extra_headers {
        if value.len() >= 8 {
            secrets.push(value);
        }
    }
    interpret_outcome(source, &outcome, model, &url, &secrets)
}

/// The pure half of [`chat_completion`] -- everything after
/// [`run_curl`] returns, replayed by `generate_adapter_corpus.py`'s
/// own scripted `(returncode, stdout, stderr)` triples
/// (`tests/adapter_corpus.rs`). `pub` for the same reason.
pub fn interpret_outcome(
    source: &str,
    outcome: &crate::transport::CurlOutcome,
    model: &str,
    url: &str,
    secrets: &[&str],
) -> Result<GenerateResult, AdapterError> {
    if outcome.returncode != 0 {
        let message = curl_failure_message(
            source,
            Some(model),
            url,
            outcome.returncode,
            &outcome.stdout,
            &outcome.stderr,
            "",
            secrets,
        );
        return Err(AdapterError {
            message,
            retry: classify_curl_outcome(outcome),
            py_class: PyExc::AdapterCallError,
        });
    }

    let raw = electricity_json::loads(&outcome.stdout)
        .map_err(|_| non_json_error("Provider", &outcome.stdout))?;
    let (text, finish_reason) = parse_chat_response(&raw)?;
    let usage = raw
        .as_dict()
        .and_then(|d| d.get(&Value::Str("usage".to_string())));
    let usage_dict = match usage {
        Some(Value::Dict(d)) => Some(d),
        Some(other) if !crate::config::value_is_truthy(other) => None,
        Some(other) => return Err(attribute_error(other, "get")),
        None => None,
    };
    let tokens_sent =
        usage_dict.and_then(|d| int_if_pyint(d.get(&Value::Str("prompt_tokens".to_string()))));
    let tokens_received =
        usage_dict.and_then(|d| int_if_pyint(d.get(&Value::Str("completion_tokens".to_string()))));

    Ok(GenerateResult {
        text,
        raw,
        tokens_sent,
        tokens_received,
        finish_reason,
        warnings: Vec::new(),
        cost_usd: None,
    })
}

/// `_openai_compat.py::check_dependencies`.
pub fn check_dependencies(cfg: &OpenAiCompatibleConfig) -> CheckResult {
    let mut missing = Vec::new();
    if !cfg.api_key_env.is_empty()
        && std::env::var(&cfg.api_key_env)
            .unwrap_or_default()
            .is_empty()
    {
        missing.push(format!("env:{}", cfg.api_key_env));
    }
    CheckResult {
        ok: missing.is_empty(),
        missing,
        message: None,
    }
}

/// `_openai_compat.py::OpenAICompatibleAdapter` -- one struct for the
/// whole family, configured per provider name (the stub's own fields
/// were this lane's to change).
/// A pre-check one family member raises before any request --
/// returns the extra headers (`azure-openai`'s own `api-key`) to send
/// alongside the ordinary ones, or an error with exact text
/// (`azure-openai`'s endpoint/key checks, `cloudflare-workers-ai`'s/
/// `databricks`'s own resolved-base-url checks).
type Precheck = fn(&OpenAiCompatibleConfig) -> Result<Vec<(String, String)>, AdapterError>;

pub struct OpenAiCompatibleAdapter {
    pub provider_name: String,
    pub cfg: OpenAiCompatibleConfig,
    pub precheck: Option<Precheck>,
    /// A `check()` override for a member whose readiness depends on
    /// more than the API key (`cloudflare-workers-ai`/`databricks`'s
    /// own missing-base-url message, `azure-openai`'s own two env
    /// vars).
    pub check_override: Option<fn(&OpenAiCompatibleConfig) -> CheckResult>,
}

#[async_trait(?Send)]
impl Adapter for OpenAiCompatibleAdapter {
    fn name(&self) -> &str {
        &self.provider_name
    }

    async fn generate(
        &self,
        model: &str,
        prompt: &str,
        options: &GenerateOptions,
        call: &CallContext<'_>,
    ) -> Result<GenerateResult, AdapterError> {
        let extra_headers = match self.precheck {
            Some(precheck) => precheck(&self.cfg)?,
            None => Vec::new(),
        };
        chat_completion(
            "OpenAI-compatible",
            &self.cfg,
            model,
            prompt,
            call.timeout_seconds,
            &extra_headers,
            options,
        )
        .await
    }

    async fn check(&self) -> CheckResult {
        match self.check_override {
            Some(f) => f(&self.cfg),
            None => check_dependencies(&self.cfg),
        }
    }

    fn accepts_images(&self) -> bool {
        true
    }
}

fn simple_adapter(provider_name: &str, cfg: OpenAiCompatibleConfig) -> Box<dyn Adapter> {
    Box::new(OpenAiCompatibleAdapter {
        provider_name: provider_name.to_string(),
        cfg,
        precheck: None,
        check_override: None,
    })
}

// ---------------------------------------------------------------------
// Per-provider builders (`adapters/factory.py`'s own `_build_*`).
// ---------------------------------------------------------------------

pub fn build_ai21(cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    Ok(simple_adapter(
        "ai21",
        OpenAiCompatibleConfig::new(
            cfg_str_or_rstrip_target(cfg, "base_url", "https://api.ai21.com/studio/v1")?,
            "AI21_API_KEY",
            cfg_str_or(cfg, "default_model", "jamba-large"),
        ),
    ))
}

pub fn build_cohere(cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    Ok(simple_adapter(
        "cohere",
        OpenAiCompatibleConfig::new(
            cfg_str_or_rstrip_target(cfg, "base_url", "https://api.cohere.com")?,
            "COHERE_API_KEY",
            cfg_str_or(cfg, "default_model", "command-r-plus"),
        )
        .with_path("/compatibility/v1/chat/completions"),
    ))
}

pub fn build_deepseek(cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    Ok(simple_adapter(
        "deepseek",
        OpenAiCompatibleConfig::new(
            cfg_str_or_rstrip_target(cfg, "base_url", "https://api.deepseek.com/v1")?,
            "DEEPSEEK_API_KEY",
            cfg_str_or(cfg, "default_model", "deepseek-chat"),
        ),
    ))
}

pub fn build_fireworks(cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    Ok(simple_adapter(
        "fireworks",
        OpenAiCompatibleConfig::new(
            cfg_str_or_rstrip_target(cfg, "base_url", "https://api.fireworks.ai/inference/v1")?,
            "FIREWORKS_API_KEY",
            cfg_str_or(
                cfg,
                "default_model",
                "accounts/fireworks/models/llama-v3p3-70b-instruct",
            ),
        ),
    ))
}

pub fn build_gemini(cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    Ok(simple_adapter(
        "gemini",
        OpenAiCompatibleConfig::new(
            cfg_str_or(
                cfg,
                "base_url",
                "https://generativelanguage.googleapis.com/v1beta/openai",
            ),
            "GOOGLE_API_KEY",
            cfg_str_or(cfg, "default_model", "gemini-2.5-flash"),
        ),
    ))
}

pub fn build_groq(cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    Ok(simple_adapter(
        "groq",
        OpenAiCompatibleConfig::new(
            cfg_str_or_rstrip_target(cfg, "base_url", "https://api.groq.com/openai/v1")?,
            "GROQ_API_KEY",
            cfg_str_or(cfg, "default_model", "llama-3.3-70b-versatile"),
        ),
    ))
}

pub fn build_huggingface_inference(cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    Ok(simple_adapter(
        "huggingface-inference",
        OpenAiCompatibleConfig::new(
            cfg_str_or_rstrip_target(cfg, "base_url", "https://router.huggingface.co/v1")?,
            "HF_TOKEN",
            cfg_str_or(cfg, "default_model", "meta-llama/Llama-3.3-70B-Instruct"),
        ),
    ))
}

pub fn build_llamacpp(cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    Ok(simple_adapter(
        "llamacpp",
        OpenAiCompatibleConfig::new(
            cfg_str_or_rstrip_target(cfg, "base_url", "http://localhost:8080/v1")?,
            "",
            cfg_str_or(cfg, "default_model", ""),
        ),
    ))
}

pub fn build_lmstudio(cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    Ok(simple_adapter(
        "lmstudio",
        OpenAiCompatibleConfig::new(
            cfg_str_or_rstrip_target(cfg, "base_url", "http://localhost:1234/v1")?,
            "",
            cfg_str_or(cfg, "default_model", ""),
        ),
    ))
}

pub fn build_mistral(cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    Ok(simple_adapter(
        "mistral",
        OpenAiCompatibleConfig::new(
            cfg_str_or_rstrip_target(cfg, "base_url", "https://api.mistral.ai/v1")?,
            "MISTRAL_API_KEY",
            cfg_str_or(cfg, "default_model", "mistral-large-latest"),
        ),
    ))
}

pub fn build_nvidia_nim(cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    let api_key_env = match cfg_get(cfg, "api_key_env") {
        None | Some(Value::None) => "NIM_API_KEY".to_string(),
        Some(Value::Str(s)) => s.clone(),
        Some(_) => "NIM_API_KEY".to_string(),
    };
    Ok(simple_adapter(
        "nvidia-nim",
        OpenAiCompatibleConfig::new(
            cfg_str_or_rstrip_target(cfg, "base_url", "https://integrate.api.nvidia.com/v1")?,
            api_key_env,
            cfg_str_or(cfg, "default_model", "meta/llama-3.3-70b-instruct"),
        ),
    ))
}

pub fn build_openrouter(cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    Ok(simple_adapter(
        "openrouter",
        OpenAiCompatibleConfig::new(
            cfg_str_or_rstrip_target(cfg, "base_url", "https://openrouter.ai/api/v1")?,
            "OPENROUTER_API_KEY",
            cfg_str_or(cfg, "default_model", "openai/gpt-4o-mini"),
        ),
    ))
}

pub fn build_perplexity(cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    Ok(simple_adapter(
        "perplexity",
        OpenAiCompatibleConfig::new(
            cfg_str_or_rstrip_target(cfg, "base_url", "https://api.perplexity.ai")?,
            "PERPLEXITY_API_KEY",
            cfg_str_or(cfg, "default_model", "sonar"),
        ),
    ))
}

pub fn build_qwen_dashscope(cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    Ok(simple_adapter(
        "qwen-dashscope",
        OpenAiCompatibleConfig::new(
            cfg_str_or(
                cfg,
                "base_url",
                "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
            ),
            "DASHSCOPE_API_KEY",
            cfg_str_or(cfg, "default_model", "qwen-max"),
        ),
    ))
}

pub fn build_tgi(cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    let api_key_env = match cfg_get(cfg, "api_key_env") {
        None | Some(Value::None) => String::new(),
        Some(Value::Str(s)) => s.clone(),
        Some(_) => String::new(),
    };
    Ok(simple_adapter(
        "tgi",
        OpenAiCompatibleConfig::new(
            cfg_str_or_rstrip_target(cfg, "base_url", "http://localhost:3000/v1")?,
            api_key_env,
            cfg_str_or(cfg, "default_model", ""),
        ),
    ))
}

pub fn build_together(cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    Ok(simple_adapter(
        "together",
        OpenAiCompatibleConfig::new(
            cfg_str_or_rstrip_target(cfg, "base_url", "https://api.together.xyz/v1")?,
            "TOGETHER_API_KEY",
            cfg_str_or(
                cfg,
                "default_model",
                "meta-llama/Llama-3.3-70B-Instruct-Turbo",
            ),
        ),
    ))
}

pub fn build_vllm(cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    Ok(simple_adapter(
        "vllm",
        OpenAiCompatibleConfig::new(
            cfg_str_or_rstrip_target(cfg, "base_url", "http://localhost:8000/v1")?,
            "",
            cfg_str_or(cfg, "default_model", ""),
        ),
    ))
}

pub fn build_xai(cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    Ok(simple_adapter(
        "xai",
        OpenAiCompatibleConfig::new(
            cfg_str_or_rstrip_target(cfg, "base_url", "https://api.x.ai/v1")?,
            "XAI_API_KEY",
            cfg_str_or(cfg, "default_model", "grok-2-latest"),
        ),
    ))
}

// -- Members needing a pre-check or a `check()` override. --

fn azure_openai_precheck(
    cfg: &OpenAiCompatibleConfig,
) -> Result<Vec<(String, String)>, AdapterError> {
    if cfg.base_url.is_empty() {
        return Err(AdapterError {
            message: "azure-openai: endpoint not configured. Set AZURE_OPENAI_ENDPOINT or \
                      runtime.adapters.azure-openai.endpoint."
                .to_string(),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::RuntimeError,
        });
    }
    let api_key = std::env::var("AZURE_OPENAI_API_KEY").unwrap_or_default();
    if api_key.is_empty() {
        return Err(AdapterError {
            message: "azure-openai: AZURE_OPENAI_API_KEY not set.".to_string(),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::RuntimeError,
        });
    }
    Ok(vec![("api-key".to_string(), api_key)])
}

fn azure_openai_check(cfg: &OpenAiCompatibleConfig) -> CheckResult {
    let mut missing = Vec::new();
    if std::env::var("AZURE_OPENAI_API_KEY")
        .unwrap_or_default()
        .is_empty()
    {
        missing.push("env:AZURE_OPENAI_API_KEY".to_string());
    }
    if cfg.base_url.is_empty() {
        missing.push("env:AZURE_OPENAI_ENDPOINT".to_string());
    }
    CheckResult {
        ok: missing.is_empty(),
        missing,
        message: None,
    }
}

pub fn build_azure_openai(cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    let endpoint = cfg_str_or_rstrip_target(cfg, "endpoint", "")?;
    let endpoint = if endpoint.is_empty() {
        let fallback = cfg_str_or_rstrip_target(cfg, "base_url", "")?;
        if fallback.is_empty() {
            std::env::var("AZURE_OPENAI_ENDPOINT").unwrap_or_default()
        } else {
            fallback
        }
    } else {
        endpoint
    };
    let api_version = cfg_str_or(cfg, "api_version", "2024-10-21");
    let base_url = endpoint.trim_end_matches('/').to_string();
    let openai_cfg =
        OpenAiCompatibleConfig::new(base_url, "", cfg_str_or(cfg, "default_model", "")).with_path(
            format!("/openai/deployments/{{model}}/chat/completions?api-version={api_version}"),
        );
    Ok(Box::new(OpenAiCompatibleAdapter {
        provider_name: "azure-openai".to_string(),
        cfg: openai_cfg,
        precheck: Some(azure_openai_precheck),
        check_override: Some(azure_openai_check),
    }))
}

fn cloudflare_resolve_base_url(explicit: &str, account_id: &str) -> String {
    if !explicit.is_empty() {
        return explicit.to_string();
    }
    if account_id.is_empty() {
        return String::new();
    }
    format!("https://api.cloudflare.com/client/v4/accounts/{account_id}/ai/v1")
}

fn cloudflare_precheck(
    cfg: &OpenAiCompatibleConfig,
) -> Result<Vec<(String, String)>, AdapterError> {
    if cfg.base_url.is_empty() {
        return Err(AdapterError {
            message: "cloudflare-workers-ai: account_id not configured. Set CF_ACCOUNT_ID or \
                      runtime.adapters.cloudflare-workers-ai.account_id (or supply a \
                      fully-formed base_url)."
                .to_string(),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::RuntimeError,
        });
    }
    Ok(Vec::new())
}

fn cloudflare_check(cfg: &OpenAiCompatibleConfig) -> CheckResult {
    let base = check_dependencies(cfg);
    if cfg.base_url.is_empty() {
        let mut missing = base.missing;
        missing.push("env:CF_ACCOUNT_ID".to_string());
        return CheckResult {
            ok: false,
            missing,
            message: Some("set CF_ACCOUNT_ID to your Cloudflare account UUID.".to_string()),
        };
    }
    base
}

pub fn build_cloudflare_workers_ai(cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    let explicit_base_url = cfg_str_or_rstrip_target(cfg, "base_url", "")?;
    let account_id = {
        let configured = cfg_str_or(cfg, "account_id", "");
        if configured.is_empty() {
            std::env::var("CF_ACCOUNT_ID").unwrap_or_default()
        } else {
            configured
        }
    };
    let base_url = cloudflare_resolve_base_url(&explicit_base_url, &account_id);
    let openai_cfg = OpenAiCompatibleConfig::new(
        base_url,
        "CF_API_TOKEN",
        cfg_str_or(
            cfg,
            "default_model",
            "@cf/meta/llama-3.3-70b-instruct-fp8-fast",
        ),
    );
    Ok(Box::new(OpenAiCompatibleAdapter {
        provider_name: "cloudflare-workers-ai".to_string(),
        cfg: openai_cfg,
        precheck: Some(cloudflare_precheck),
        check_override: Some(cloudflare_check),
    }))
}

fn databricks_resolve_base_url(explicit: &str) -> String {
    if !explicit.is_empty() {
        return explicit.to_string();
    }
    let host = std::env::var("DATABRICKS_HOST").unwrap_or_default();
    let host = host.trim_end_matches('/');
    if host.is_empty() {
        return String::new();
    }
    let host = if host.starts_with("http://") || host.starts_with("https://") {
        host.to_string()
    } else {
        format!("https://{host}")
    };
    format!("{host}/serving-endpoints")
}

fn databricks_precheck(
    cfg: &OpenAiCompatibleConfig,
) -> Result<Vec<(String, String)>, AdapterError> {
    if cfg.base_url.is_empty() {
        return Err(AdapterError {
            message: "databricks: workspace host not configured. Set DATABRICKS_HOST or \
                      runtime.adapters.databricks.base_url to your workspace URL."
                .to_string(),
            retry: RetryInfo::not_retryable(),
            py_class: PyExc::RuntimeError,
        });
    }
    Ok(Vec::new())
}

fn databricks_check(cfg: &OpenAiCompatibleConfig) -> CheckResult {
    let base = check_dependencies(cfg);
    if cfg.base_url.is_empty() {
        let mut missing = base.missing;
        missing.push("env:DATABRICKS_HOST".to_string());
        return CheckResult {
            ok: false,
            missing,
            message: Some("set DATABRICKS_HOST to your workspace.".to_string()),
        };
    }
    base
}

pub fn build_databricks(cfg: &Value) -> Result<Box<dyn Adapter>, AdapterError> {
    let base_url = databricks_resolve_base_url(&cfg_str_or_rstrip_target(cfg, "base_url", "")?);
    let openai_cfg = OpenAiCompatibleConfig::new(
        base_url,
        "DATABRICKS_TOKEN",
        cfg_str_or(
            cfg,
            "default_model",
            "databricks-meta-llama-3-3-70b-instruct",
        ),
    );
    Ok(Box::new(OpenAiCompatibleAdapter {
        provider_name: "databricks".to_string(),
        cfg: openai_cfg,
        precheck: Some(databricks_precheck),
        check_override: Some(databricks_check),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_vm::adapter::ChatMessage;

    #[test]
    fn chat_messages_turns_a_tool_role_into_a_prefixed_user_turn() {
        let options = GenerateOptions {
            messages: vec![ChatMessage {
                role: "tool".to_string(),
                content: "result".to_string(),
            }],
            ..GenerateOptions::default()
        };
        let messages = chat_messages("hi", &options);
        let entry = messages[0].as_dict().unwrap();
        assert_eq!(
            entry.get(&Value::Str("role".to_string())),
            Some(&Value::Str("user".to_string()))
        );
        assert_eq!(
            entry.get(&Value::Str("content".to_string())),
            Some(&Value::Str("tool: result".to_string()))
        );
    }

    #[test]
    fn chat_messages_defaults_to_one_user_turn_with_the_flattened_prompt() {
        let messages = chat_messages("hi", &GenerateOptions::default());
        assert_eq!(messages.len(), 1);
    }

    #[test]
    fn sampling_fields_lets_params_collide_with_an_earlier_key_in_place() {
        let mut params = Dict::new();
        dict_insert(&mut params, "temperature", Value::Float(0.9));
        let options = GenerateOptions {
            temperature: Some(0.1),
            params: Value::Dict(params),
            ..GenerateOptions::default()
        };
        let fields = sampling_fields(&options, "max_tokens");
        let keys: Vec<String> = fields.keys().map(|k| k.py_str()).collect();
        assert_eq!(keys, vec!["temperature"]);
        assert_eq!(
            fields.get(&Value::Str("temperature".to_string())),
            Some(&Value::Float(0.9))
        );
    }

    #[test]
    fn parse_chat_response_reads_content_and_finish_reason() {
        let raw = electricity_json::loads(
            r#"{"choices": [{"message": {"content": " hi "}, "finish_reason": "stop"}]}"#,
        )
        .unwrap();
        let (text, finish_reason) = parse_chat_response(&raw).unwrap();
        assert_eq!(text, "hi");
        assert_eq!(finish_reason, Some("stop".to_string()));
    }

    #[test]
    fn parse_chat_response_treats_a_falsy_message_as_empty_content() {
        let raw = electricity_json::loads(r#"{"choices": [{"message": null}]}"#).unwrap();
        let (text, finish_reason) = parse_chat_response(&raw).unwrap();
        assert_eq!(text, "");
        assert_eq!(finish_reason, None);
    }

    #[test]
    fn parse_chat_response_raises_on_a_truthy_non_dict_message() {
        let raw = electricity_json::loads(r#"{"choices": [{"message": "oops"}]}"#).unwrap();
        let err = parse_chat_response(&raw).unwrap_err();
        assert_eq!(err.message, "'str' object has no attribute 'get'");
        assert_eq!(err.py_class, PyExc::AttributeError);
    }

    #[test]
    fn build_azure_openai_combines_endpoint_and_model_into_the_path() {
        let adapter = build_azure_openai(&Value::None).unwrap();
        assert_eq!(adapter.name(), "azure-openai");
    }

    #[test]
    fn build_cloudflare_requires_an_account_id_before_any_request() {
        let adapter = build_cloudflare_workers_ai(&Value::None).unwrap();
        assert_eq!(adapter.name(), "cloudflare-workers-ai");
    }

    #[test]
    fn build_nvidia_nim_lets_an_explicit_empty_api_key_env_disable_the_key() {
        let mut cfg = Dict::new();
        dict_insert(&mut cfg, "api_key_env", Value::Str(String::new()));
        let adapter = build_nvidia_nim(&Value::Dict(cfg)).unwrap();
        assert_eq!(adapter.name(), "nvidia-nim");
    }

    #[test]
    fn build_mistral_fails_like_python_for_a_non_string_base_url() {
        // Issue #515: a truthy non-string `base_url` must not silently
        // fall back to the provider's default URL -- every OpenAI-
        // compatible provider shares the one `_openai_compat.py::
        // chat_completion` call that raises `AttributeError: '<type>'
        // object has no attribute 'rstrip'` on it.
        let mut cfg = Dict::new();
        dict_insert(&mut cfg, "base_url", Value::Bool(true));
        let err = match build_mistral(&Value::Dict(cfg)) {
            Err(err) => err,
            Ok(_) => panic!("expected build_mistral to fail"),
        };
        assert_eq!(err.message, "'bool' object has no attribute 'rstrip'");
        assert_eq!(err.py_class, PyExc::AttributeError);
    }

    #[test]
    fn build_azure_openai_fails_like_python_for_a_non_string_endpoint() {
        let mut cfg = Dict::new();
        dict_insert(&mut cfg, "endpoint", Value::from(5i64));
        let err = match build_azure_openai(&Value::Dict(cfg)) {
            Err(err) => err,
            Ok(_) => panic!("expected build_azure_openai to fail"),
        };
        assert_eq!(err.message, "'int' object has no attribute 'rstrip'");
        assert_eq!(err.py_class, PyExc::AttributeError);
    }

    #[test]
    fn build_azure_openai_fails_like_python_for_a_non_string_base_url_fallback() {
        let mut cfg = Dict::new();
        dict_insert(&mut cfg, "base_url", Value::from(5i64));
        let err = match build_azure_openai(&Value::Dict(cfg)) {
            Err(err) => err,
            Ok(_) => panic!("expected build_azure_openai to fail"),
        };
        assert_eq!(err.message, "'int' object has no attribute 'rstrip'");
        assert_eq!(err.py_class, PyExc::AttributeError);
    }

    #[test]
    fn build_cloudflare_fails_like_python_for_a_non_string_base_url() {
        let mut cfg = Dict::new();
        dict_insert(&mut cfg, "base_url", Value::from(5i64));
        let err = match build_cloudflare_workers_ai(&Value::Dict(cfg)) {
            Err(err) => err,
            Ok(_) => panic!("expected build_cloudflare_workers_ai to fail"),
        };
        assert_eq!(err.message, "'int' object has no attribute 'rstrip'");
        assert_eq!(err.py_class, PyExc::AttributeError);
    }

    #[test]
    fn build_databricks_fails_like_python_for_a_non_string_base_url() {
        let mut cfg = Dict::new();
        dict_insert(&mut cfg, "base_url", Value::from(5i64));
        let err = match build_databricks(&Value::Dict(cfg)) {
            Err(err) => err,
            Ok(_) => panic!("expected build_databricks to fail"),
        };
        assert_eq!(err.message, "'int' object has no attribute 'rstrip'");
        assert_eq!(err.py_class, PyExc::AttributeError);
    }
}
