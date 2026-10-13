//! `adapters/factory.py::ADAPTER_REGISTRY`/`build_adapter`, ported --
//! the crate that implements concrete [`electricity_vm::adapter::Adapter`]s
//! (DESIGN.md §2's own dependency direction: this crate depends on
//! `electricity-vm`, which defines the trait boundary, never the
//! reverse). Not created until lane F1's own first PR per
//! `electricity/docs/spec/vm-lanes.md`'s M1 table -- this file set is
//! the gate's own stub (M1 gate seam round 2, item S9), so lane F1's
//! PR only ever fills a body, never adds a new file or a `pub mod`
//! line to this list.
//!
//! - [`scripted`]: the Rust `scripted` adapter (lane F1) --
//!   deterministic replies from a replies file, keyed by call path,
//!   for the conformance suite (`electricity/DESIGN.md` §12) and for
//!   running an orchestration with no real model behind it.
//! - [`host_claude`]: the `host_claude` registration stub (lane F1) --
//!   a name `build_registry` recognizes, whose builder always fails:
//!   this adapter needs a *request_handler* injected at run time via
//!   `RunRequest::adapter` (`circuitry-mcp`'s own use), never built
//!   from `runtime.adapters.host_claude` config alone.
//! - [`openai`], [`anthropic`], [`openai_compat`], [`curl_text`]: lane
//!   F2's own real network-backed providers and the shared curl
//!   failure-text helper -- stub files created now too (review P2-5
//!   on PR #454's second review, extending item S9 to these), so F2's
//!   own PR only ever fills a body here, same as F1's own files
//!   above; `ollama` has no file of its own in this table yet, so F2
//!   creates it fresh in its own PR -- a brand new file is never a
//!   seam conflict the way editing one of these would be.
//!
//! This file also carries the adapter-core helpers every concrete
//! adapter shares (`adapters/base.py::GenerateOptions.set_fields`/
//! `ignored_options_warning`), and the leftover-replies export (lane
//! F1, issue #478, `electricity/docs/spec/scripted-replies.md` §7):
//! [`write_leftover_export`] is called once, at process exit, from
//! `electricity-cli` (`electricity::write_leftover_export`'s own
//! re-export of it) -- never from this crate itself, which has no
//! notion of "the process is exiting" of its own.

pub mod anthropic;
// `claude_code.rs`/`pi.rs`'s own shared half (issue #520 part B) --
// retry classification, `check()`, and the result builder every
// adapter that runs a coding-agent CLI through `electricity_tools::
// agent_cli` (part A) shares.
pub(crate) mod agent_cli;
pub mod claude_code;
pub(crate) mod config;
pub mod curl_text;
pub mod host_claude;
pub mod ollama;
pub mod openai;
pub mod openai_compat;
pub mod pi;
pub mod replicate;
pub mod scripted;
pub mod transport;
pub mod watsonx;

use electricity_vm::adapter::{AdapterRegistry, GenerateOptions};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// `adapters/factory.py::build_adapter`'s own per-attempt registry --
/// signature only; lane F1 fills the body (registering [`scripted`]/
/// [`host_claude`]'s own builders; lane F2 adds its own providers'
/// builders to the same registry in its own PR, each one line in this
/// function, never a new file).
pub fn build_registry() -> AdapterRegistry {
    let mut registry = AdapterRegistry::new();
    registry.register("scripted", Box::new(scripted::build_scripted));
    registry.register("host_claude", Box::new(host_claude::build_host_claude));
    // Issue #520 part B: the two coding-agent-CLI adapters, registered
    // right after `host_claude` (not at the end) -- the issue's own
    // seam note with #515/`cyberdiner`, to keep that lane's own merge
    // conflict here as small as possible.
    registry.register("claude_code", Box::new(claude_code::build_claude_code));
    registry.register("pi", Box::new(pi::build_pi));
    // Lane F2 (issue #503): native Ollama first, then the
    // OpenAI-compatible family, `openai`/`anthropic`, and
    // `replicate`/`watsonx` last. The unbuildable set after this lane
    // is exactly `claude_code`, `cyberdiner`, `litellm` and `pi`.
    registry.register("ollama", Box::new(ollama::build_ollama));
    registry.register("openai", Box::new(openai::build_openai));
    registry.register("anthropic", Box::new(anthropic::build_anthropic));
    registry.register("ai21", Box::new(openai_compat::build_ai21));
    registry.register("azure-openai", Box::new(openai_compat::build_azure_openai));
    registry.register(
        "cloudflare-workers-ai",
        Box::new(openai_compat::build_cloudflare_workers_ai),
    );
    registry.register("cohere", Box::new(openai_compat::build_cohere));
    registry.register("databricks", Box::new(openai_compat::build_databricks));
    registry.register("deepseek", Box::new(openai_compat::build_deepseek));
    registry.register("fireworks", Box::new(openai_compat::build_fireworks));
    registry.register("gemini", Box::new(openai_compat::build_gemini));
    registry.register("groq", Box::new(openai_compat::build_groq));
    registry.register(
        "huggingface-inference",
        Box::new(openai_compat::build_huggingface_inference),
    );
    registry.register("llamacpp", Box::new(openai_compat::build_llamacpp));
    registry.register("lmstudio", Box::new(openai_compat::build_lmstudio));
    registry.register("mistral", Box::new(openai_compat::build_mistral));
    registry.register("nvidia-nim", Box::new(openai_compat::build_nvidia_nim));
    registry.register("openrouter", Box::new(openai_compat::build_openrouter));
    registry.register("perplexity", Box::new(openai_compat::build_perplexity));
    registry.register(
        "qwen-dashscope",
        Box::new(openai_compat::build_qwen_dashscope),
    );
    registry.register("tgi", Box::new(openai_compat::build_tgi));
    registry.register("together", Box::new(openai_compat::build_together));
    registry.register("vllm", Box::new(openai_compat::build_vllm));
    registry.register("xai", Box::new(openai_compat::build_xai));
    registry.register("replicate", Box::new(replicate::build_replicate));
    registry.register("watsonx", Box::new(watsonx::build_watsonx));
    registry
}

// ---------------------------------------------------------------------
// Adapter core: `adapters/base.py::GenerateOptions.set_fields`/
// `ignored_options_warning`
// ---------------------------------------------------------------------

/// `adapters/base.py::GenerateOptions.set_fields` -- the fields
/// carrying a value, named the way a warning shows them, in Python's
/// own fixed order. `deterministic`/`json_schema` are never listed:
/// the runtime realises `deterministic` as `temperature` before an
/// adapter ever sees it, and every adapter is expected to validate
/// against `json_schema` itself without being warned for ignoring it.
pub fn set_fields(options: &GenerateOptions) -> Vec<String> {
    let mut names = Vec::new();
    if options.temperature.is_some() {
        names.push("temperature".to_string());
    }
    if options.max_tokens.is_some() {
        names.push("max_tokens".to_string());
    }
    if !options.stop.is_empty() {
        names.push("stop".to_string());
    }
    if let Some(params) = options.params.as_dict() {
        if !params.is_empty() {
            let mut keys: Vec<String> = params.keys().map(|key| key.py_str()).collect();
            keys.sort();
            names.push(format!("params ({})", keys.join(", ")));
        }
    }
    if !options.messages.is_empty() {
        names.push("messages (roles flattened into one prompt)".to_string());
    }
    if !options.images.is_empty() {
        names.push("images".to_string());
    }
    names
}

/// `adapters/base.py::ignored_options_warning` -- one warning naming
/// the set fields of *options* outside *used* (bare field names,
/// e.g. `"temperature"`, `"messages"`); `[]` when nothing was ignored.
/// *used* is matched against each set field's own first word
/// (`"params (...)"`/`"messages (...)"` both split on the first
/// space), the same way Python's `name.split(" ", 1)[0]` does.
pub fn ignored_options_warning(
    adapter_name: &str,
    options: &GenerateOptions,
    used: &[&str],
) -> Vec<String> {
    let ignored: Vec<String> = set_fields(options)
        .into_iter()
        .filter(|name| {
            let first_word = name.split(' ').next().unwrap_or(name);
            !used.contains(&first_word)
        })
        .collect();
    if ignored.is_empty() {
        return Vec::new();
    }
    vec![format!(
        "adapter '{adapter_name}' ignored: {}",
        ignored.join("; ")
    )]
}

// ---------------------------------------------------------------------
// The leftover-replies export (scripted-replies.md §7)
// ---------------------------------------------------------------------

/// `adapters/scripted.py::LEFTOVER_REPLIES_EXPORT_ENV_VAR` -- the same
/// name Python reads; `tests/conformance/harness.py` sets it for both
/// engines. Never read when unset.
pub const LEFTOVER_REPLIES_EXPORT_ENV_VAR: &str = "CIRCUITRY_TEST_SCRIPTED_LEFTOVER_REPLIES_FILE";

/// Every path a registered instance's own loaded queues know about,
/// mapped to its *current* remaining reply count (`0` included, for a
/// path it has fully consumed) -- `scripted::ScriptedAdapter`'s own
/// `_remaining_counts` equivalent, read through a plain closure so
/// this module needs no knowledge of `scripted`'s own private `Reply`
/// shape.
type RemainingCountsFn = dyn Fn() -> HashMap<String, usize> + Send + Sync;

fn export_registry() -> &'static Mutex<Vec<Arc<RemainingCountsFn>>> {
    static REGISTRY: OnceLock<Mutex<Vec<Arc<RemainingCountsFn>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(Vec::new()))
}

/// Registers *remaining_counts* (an instance's own, moved the moment
/// it *loads* its replies file -- `scripted::ScriptedAdapter::
/// ensure_loaded`, called by both `check()` and `generate()`) for the
/// leftover-replies export, when the variable naming the export file
/// is actually set. A no-op, and so free of cost, for every ordinary
/// run: an instance calls this itself, under its own load lock, so two
/// calls for the same instance can never register it twice (callers
/// pass a fresh `Arc` each time, but the caller's own "already
/// registered" flag -- not this function -- is what prevents a second
/// call in the first place).
pub(crate) fn register_for_leftover_export(remaining_counts: Arc<RemainingCountsFn>) {
    if std::env::var_os(LEFTOVER_REPLIES_EXPORT_ENV_VAR).is_none() {
        return;
    }
    export_registry().lock().unwrap().push(remaining_counts);
}

/// `adapters/scripted.py::_merge_leftover_replies`'s own merge
/// arithmetic, pulled out of [`merge_leftover_replies`] so it can be
/// tested directly against `generate_scripted_corpus.py`'s own
/// `merge_cases` -- each one's recorded per-instance remaining-count
/// snapshots, fed in here with no registry involved at all. The
/// registry itself (unlike this function) is append-only for the
/// whole process's lifetime, matching Python's own module-level
/// `_export_instances` global (by design: one process is one run), so
/// replaying several independent corpus scenarios against the
/// registry directly, in one test binary, would let an earlier
/// scenario's own stale registrations leak into a later one's result
/// -- this pure form has no such state to leak.
///
/// Merges by taking the **minimum** remaining count across every
/// snapshot that holds a given path (`0` counts; never summed),
/// keeping only the paths whose merged count is nonzero.
pub fn merge_remaining_counts(snapshots: &[HashMap<String, usize>]) -> HashMap<String, usize> {
    let mut merged: HashMap<String, usize> = HashMap::new();
    for counts in snapshots {
        for (path, count) in counts {
            merged
                .entry(path.clone())
                .and_modify(|current| *current = (*current).min(*count))
                .or_insert(*count);
        }
    }
    merged.retain(|_, count| *count > 0);
    merged
}

/// `adapters/scripted.py::_merge_leftover_replies` -- every registered
/// instance's own remaining count at each path, merged by
/// [`merge_remaining_counts`].
fn merge_leftover_replies() -> HashMap<String, usize> {
    let snapshots: Vec<HashMap<String, usize>> = export_registry()
        .lock()
        .unwrap()
        .iter()
        .map(|remaining_counts| remaining_counts())
        .collect();
    merge_remaining_counts(&snapshots)
}

/// A single JSON object, `{"<path>": <count>, ...}`, the same shape
/// `json.dumps` of a plain `dict[str, int]` writes -- this crate never
/// depends on `electricity-json`'s own writer for one map with
/// `usize` values, so this is hand-rolled rather than routed through
/// it; every path is a plain effect-path string with no character
/// `str`'s own JSON escaping would ever need to treat specially
/// (`.`-joined identifiers only), so a minimal escape of `"`/`\`/
/// control characters is enough.
fn leftover_export_json(merged: &HashMap<String, usize>) -> String {
    let mut entries: Vec<(&String, &usize)> = merged.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    let body = entries
        .iter()
        .map(|(path, count)| format!("{}: {count}", json_escape_str(path)))
        .collect::<Vec<_>>()
        .join(", ");
    format!("{{{body}}}")
}

fn json_escape_str(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `adapters/scripted.py::_write_leftover_export` -- written once, at
/// process exit, whether the run succeeded or failed; never read when
/// [`LEFTOVER_REPLIES_EXPORT_ENV_VAR`] is unset. The write is
/// best-effort (an unwritable path, say): a failure here never changes
/// the exit code or the run's own output, so any I/O error is
/// silently dropped.
///
/// Called from `electricity-cli`'s own exit path
/// (`electricity::write_leftover_export`), on every exit matching
/// Python's own `atexit` (normal exit, an error exit, and the first
/// signal's own handled exit) -- never from the hard `std::process::
/// exit` a *second* SIGINT/SIGTERM takes, which skips destructors the
/// same way Python's own `os._exit` skips `atexit`.
pub fn write_leftover_export() {
    let Some(target) = std::env::var_os(LEFTOVER_REPLIES_EXPORT_ENV_VAR) else {
        return;
    };
    if export_registry().lock().unwrap().is_empty() {
        // No instance ever registered: the file stays absent, so a
        // harness reads that as "nothing was consumed" (every reply
        // the case's own replies file configures is left over),
        // rather than mistaking a merged-but-empty `{}` for "nothing
        // left over" (`scripted-replies.md` §7).
        return;
    }
    let merged = merge_leftover_replies();
    let _ = std::fs::write(target, leftover_export_json(&merged));
}

#[cfg(test)]
mod tests {
    use super::*;
    use electricity_value::{Dict, Value};

    #[test]
    fn a_fresh_registry_builds_exactly_the_adapters_lanes_f1_and_f2_add() {
        let registry = build_registry();
        assert_eq!(
            registry.supported_names(),
            vec![
                "ai21",
                "anthropic",
                "azure-openai",
                "claude_code",
                "cloudflare-workers-ai",
                "cohere",
                "databricks",
                "deepseek",
                "fireworks",
                "gemini",
                "groq",
                "host_claude",
                "huggingface-inference",
                "llamacpp",
                "lmstudio",
                "mistral",
                "nvidia-nim",
                "ollama",
                "openai",
                "openrouter",
                "perplexity",
                "pi",
                "qwen-dashscope",
                "replicate",
                "scripted",
                "tgi",
                "together",
                "vllm",
                "watsonx",
                "xai",
            ]
        );
    }

    #[test]
    fn build_registry_builds_scripted_through_the_normalized_lookup() {
        let registry = build_registry();
        let adapter = registry.build("SCRIPTED ", &Value::None).unwrap();
        assert_eq!(adapter.name(), "scripted");
    }

    #[test]
    fn build_registry_still_gives_the_unknown_adapter_text_for_an_unbuilt_name() {
        // `litellm` is in Circuitry's own full registry (so it is
        // named in the message's own "Supported adapters" list), but
        // this crate has no builder for it (a Python-SDK-only adapter,
        // out of scope for lane F2 per issue #503) -- `build_registry`
        // mustn't register it.
        let registry = build_registry();
        let err = match registry.build("litellm", &Value::None) {
            Err(err) => err,
            Ok(_) => panic!("expected an unknown-adapter error"),
        };
        assert!(err.message.starts_with("Unknown adapter: 'litellm'."));
    }

    #[test]
    fn build_registry_fails_host_claude_with_its_exact_text() {
        let registry = build_registry();
        let err = match registry.build("host_claude", &Value::None) {
            Err(err) => err,
            Ok(_) => panic!("expected host_claude to fail"),
        };
        assert_eq!(
            err.message,
            "host_claude cannot be built from config; it requires a request_handler injected \
             at runtime via RunRequest.adapter (see circuitry-mcp). Run `circuitry-mcp` (or \
             `cof mcp`) and drive the orchestration via the MCP tool loop, or supply \
             RunRequest(adapter=HostClaudeAdapter(request_handler=...)) from a programmatic \
             caller."
        );
    }

    #[test]
    fn set_fields_lists_nothing_for_default_options() {
        assert!(set_fields(&GenerateOptions::default()).is_empty());
    }

    #[test]
    fn set_fields_orders_and_names_every_field() {
        let mut params = Dict::new();
        params.insert(Value::Str("b".to_string()), Value::from(1i64));
        params.insert(Value::Str("a".to_string()), Value::from(2i64));
        let options = GenerateOptions {
            temperature: Some(0.5),
            max_tokens: Some(10),
            stop: vec!["END".to_string()],
            params: Value::Dict(params),
            deterministic: true,
            messages: vec![electricity_vm::adapter::ChatMessage {
                role: "user".to_string(),
                content: "hi".to_string(),
            }],
            images: vec![electricity_vm::adapter::ImageInput {
                data: None,
                media_type: None,
                url: Some("http://example/x.png".to_string()),
            }],
            json_schema: Some(Value::Dict(Dict::new())),
        };
        assert_eq!(
            set_fields(&options),
            vec![
                "temperature".to_string(),
                "max_tokens".to_string(),
                "stop".to_string(),
                "params (a, b)".to_string(),
                "messages (roles flattened into one prompt)".to_string(),
                "images".to_string(),
            ]
        );
    }

    #[test]
    fn ignored_options_warning_is_empty_when_nothing_was_ignored() {
        let options = GenerateOptions {
            temperature: Some(0.5),
            ..GenerateOptions::default()
        };
        assert!(ignored_options_warning("x", &options, &["temperature"]).is_empty());
    }

    #[test]
    fn ignored_options_warning_names_every_field_outside_used() {
        let options = GenerateOptions {
            temperature: Some(0.5),
            max_tokens: Some(10),
            ..GenerateOptions::default()
        };
        let warnings = ignored_options_warning("scripted", &options, &[]);
        assert_eq!(
            warnings,
            vec!["adapter 'scripted' ignored: temperature; max_tokens".to_string()]
        );
    }

    #[test]
    fn ignored_options_warning_matches_params_and_messages_by_first_word() {
        let mut params = Dict::new();
        params.insert(Value::Str("seed".to_string()), Value::from(1i64));
        let options = GenerateOptions {
            params: Value::Dict(params),
            ..GenerateOptions::default()
        };
        assert!(ignored_options_warning("x", &options, &["params"]).is_empty());
    }

    #[test]
    fn calling_generate_with_no_set_fields_can_never_warn() {
        // `adapters/base.py::call_generate`'s own shortcut: when
        // `options.is_empty()`, Python never calls `generate` with
        // options at all, so no warning is possible. Every Rust
        // adapter always takes options, so this is just "the warning
        // list is empty when no field is set" -- already pinned by
        // `set_fields_lists_nothing_for_default_options` above; this
        // test additionally pins it through `ignored_options_warning`
        // with an empty `used` set, the strictest case.
        let options = GenerateOptions::default();
        assert!(ignored_options_warning("scripted", &options, &[]).is_empty());
    }

    #[test]
    fn leftover_export_json_renders_a_sorted_plain_object() {
        let mut merged = HashMap::new();
        merged.insert("prime.b".to_string(), 1usize);
        merged.insert("prime.a".to_string(), 2usize);
        assert_eq!(
            leftover_export_json(&merged),
            "{\"prime.a\": 2, \"prime.b\": 1}"
        );
    }

    #[test]
    fn leftover_export_json_is_an_empty_object_when_nothing_is_left_over() {
        assert_eq!(leftover_export_json(&HashMap::new()), "{}");
    }
}
