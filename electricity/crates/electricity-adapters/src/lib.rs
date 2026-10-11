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
pub mod curl_text;
pub mod host_claude;
pub mod openai;
pub mod openai_compat;
pub mod scripted;

use electricity_vm::adapter::{Adapter, AdapterError, AdapterRegistry, GenerateOptions, PyExc};
use electricity_vm::retry::RetryInfo;
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
    registry
}

/// A builder stub shared by every not-yet-implemented adapter in this
/// crate -- lane F1/F2 replaces each call site with a real builder,
/// never widening [`electricity_vm::adapter::AdapterBuilder`]'s own
/// signature.
pub(crate) fn not_implemented_builder(
    adapter_name: &str,
    lane: &str,
) -> Result<Box<dyn Adapter>, AdapterError> {
    Err(AdapterError {
        message: format!("electricity_adapters::{adapter_name}: not implemented yet ({lane})"),
        retry: RetryInfo::not_retryable(),
        py_class: PyExc::RuntimeError,
    })
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

/// `adapters/scripted.py::_merge_leftover_replies` -- every registered
/// instance's own remaining count at each path, merged by taking the
/// **minimum** remaining count across every instance that holds that
/// path (`0` counts; never summed), keeping only the paths whose
/// merged count is nonzero.
fn merge_leftover_replies() -> HashMap<String, usize> {
    let mut merged: HashMap<String, usize> = HashMap::new();
    for remaining_counts in export_registry().lock().unwrap().iter() {
        for (path, count) in remaining_counts() {
            merged
                .entry(path)
                .and_modify(|current| *current = (*current).min(count))
                .or_insert(count);
        }
    }
    merged.retain(|_, count| *count > 0);
    merged
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
    fn a_fresh_registry_builds_exactly_scripted_and_host_claude() {
        let registry = build_registry();
        assert_eq!(registry.supported_names(), vec!["host_claude", "scripted"]);
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
