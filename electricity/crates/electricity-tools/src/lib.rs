//! electricity-tools: the `ToolPlugin` trait, the per-attempt plugin
//! builder, the process API seam, and the `json` tool (issue #449's
//! gate lane; DESIGN.md §8.1). Lane A ships this crate's final public
//! shape -- [`ToolPlugin`] v2, [`ToolCall`], [`ToolError`], [`PyExc`],
//! [`ErrorCause`], [`ToolResult::validate`], [`registry::build_plugin`]
//! -- plus the [`process`] module's signature-only stub (lane C fills
//! the body) and the `json`/`test-tools` modules updated to the new
//! trait shape.
//!
//! `#[async_trait]` (not a native `async fn` in a public trait) makes
//! [`ToolPlugin`] object-safe: a provider is looked up by name at run
//! time, the same dynamic dispatch Circuitry's own `ToolPlugin`
//! `Protocol` gets for free from Python's duck typing.

use async_trait::async_trait;
use electricity_value::{CancellationToken, Value};
use std::collections::HashMap;
use std::fmt;

pub mod json;
pub mod plugin_names;
pub mod process;
pub mod registry;
#[cfg(feature = "test-tools")]
pub mod test_tools;

pub use registry::build_plugin;

/// `plugins/base.py::ToolResult`, field for field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    pub value: Value,
    /// Always a dict (`core/tool.py`'s own meta.raw convention) -- a
    /// plugin that has nothing to report returns an empty one, never
    /// `Value::None`.
    pub raw: Value,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub exit_code: Option<i32>,
    /// Whether the tool call itself succeeded -- `false` is the one
    /// signal `ToolRuntime` (lane B/C) treats as a failure without an
    /// exception (`plugins/base.py::ToolResult.ok`'s own doc comment).
    pub ok: bool,
}

impl ToolResult {
    /// `ToolResult(value=..., raw=..., ...)`'s own defaults: `stdout`/
    /// `stderr`/`exit_code` all `None`, `ok: true`.
    pub fn new(value: Value, raw: Value) -> Self {
        ToolResult {
            value,
            raw,
            stdout: None,
            stderr: None,
            exit_code: None,
            ok: true,
        }
    }

    /// `plugins/base.py::validate_tool_result`'s own contract check --
    /// an empty result means "satisfies the contract". `stdout`/
    /// `stderr`/`ok`'s own Python `isinstance` checks have no Rust
    /// counterpart here (this struct's own field types already make
    /// them unconditionally true), so only the two checks a `Value`-
    /// typed `raw`/`i32`-typed `exit_code` can actually still fail
    /// remain: `raw` must be a dict, and a non-`None` `exit_code` must
    /// not be negative.
    pub fn validate(&self, plugin_name: &str) -> Vec<String> {
        let mut diagnostics = Vec::new();
        if !matches!(self.raw, Value::Dict(_)) {
            diagnostics.push(format!(
                "{plugin_name}: 'raw' must be dict, got {}",
                self.raw.type_name()
            ));
        }
        if let Some(exit_code) = self.exit_code {
            if exit_code < 0 {
                diagnostics.push(format!(
                    "{plugin_name}: 'exit_code' must be >= 0, got {exit_code}"
                ));
            }
        }
        diagnostics
    }
}

/// A Python exception class name -- carried by [`ToolError`] so #442's
/// multi-failure tree text (`"[i] {type}: {err}"`) can name the real
/// exception a Circuitry plugin would have raised, not a single generic
/// Rust error type. Not exhaustive of every exception class in
/// Circuitry's own plugin catalog -- lane D1/C/E/L widen this enum (a
/// new variant, never a change to an existing one's spelling) the first
/// time a ported plugin actually needs to raise one not listed here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PyExc {
    ValueError,
    RuntimeError,
    TypeError,
    KeyError,
    LookupError,
    PermissionError,
    FileNotFoundError,
    IsADirectoryError,
    NotADirectoryError,
    OSError,
    ConnectionError,
    TimeoutError,
    UnicodeDecodeError,
}

impl PyExc {
    /// The bare class name, e.g. `"ValueError"` -- #442's own `{type}`.
    pub fn name(self) -> &'static str {
        match self {
            PyExc::ValueError => "ValueError",
            PyExc::RuntimeError => "RuntimeError",
            PyExc::TypeError => "TypeError",
            PyExc::KeyError => "KeyError",
            PyExc::LookupError => "LookupError",
            PyExc::PermissionError => "PermissionError",
            PyExc::FileNotFoundError => "FileNotFoundError",
            PyExc::IsADirectoryError => "IsADirectoryError",
            PyExc::NotADirectoryError => "NotADirectoryError",
            PyExc::OSError => "OSError",
            PyExc::ConnectionError => "ConnectionError",
            PyExc::TimeoutError => "TimeoutError",
            PyExc::UnicodeDecodeError => "UnicodeDecodeError",
        }
    }
}

impl fmt::Display for PyExc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name())
    }
}

/// What kind of failure a [`ToolError`] carries, for retry
/// classification (`core/tool.py::ToolRuntime._is_retryable_failure`,
/// `adapters/_retry.py::classify_exception`) -- the same three-way
/// split an adapter dispatch failure gets, shared by every HTTP-family
/// tool (`http`, `web_fetch`, `webhook`, `linear`) once lane E wires
/// `execute_tool`'s own HTTP branch against it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorCause {
    /// No classification -- every non-HTTP-family tool failure (a
    /// process exit, a validation error, ...), always retryable
    /// (`core/tool.py`'s own fallback: "there is no status to classify").
    None,
    /// The request never got a reply at all -- a `URLError`/timeout/
    /// connection failure, matching `RETRYABLE_CURL_EXIT_CODES`'s own
    /// condition. Always retryable.
    NoReply,
    /// An HTTP response was received with this status; *retry_after* is
    /// the provider's own `Retry-After` header value, verbatim, when
    /// present.
    Http {
        status: u16,
        retry_after: Option<String>,
    },
}

/// A tool call's own raised error (as opposed to [`ToolResult::ok`]
/// `false`, a returned failure) -- the text a `meta.error` entry carries
/// when a plugin raises rather than returning an unsuccessful result.
///
/// Typed (issue #449's gate lane, widening the M0-H `ToolError(String)`
/// tuple struct): *py_class* names the Python exception class #442's
/// multi-failure tree text needs; *cause* is what `execute_tool`'s own
/// retry classification (`electricity_vm::retry`) reads once an
/// HTTP-family tool can actually produce something other than
/// [`ErrorCause::None`] (lane E).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError {
    pub py_class: PyExc,
    pub message: String,
    pub cause: ErrorCause,
}

impl ToolError {
    /// A plain message, defaulting to `RuntimeError`/[`ErrorCause::None`]
    /// -- every M0-H plugin error that doesn't yet classify itself more
    /// precisely (the `json` tool's own parse/mode failures, none of
    /// which Python ever raises as anything but a bare `ValueError`/
    /// `RuntimeError` text match) uses this constructor; a plugin that
    /// does know its own class/cause builds [`ToolError`] directly.
    pub fn message(message: impl Into<String>) -> Self {
        ToolError {
            py_class: PyExc::RuntimeError,
            message: message.into(),
            cause: ErrorCause::None,
        }
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for ToolError {}

/// `preflight.py::CheckResult`, field for field -- a tool plugin's own
/// `check()` outcome (dependency/environment readiness), independent of
/// whether it can actually [`ToolPlugin::execute`] anything right now.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CheckResult {
    pub ok: bool,
    /// `"env:VAR_NAME"`/`"binary:name"`/`"library:dotted.path"`/
    /// `"host:url"` -- `preflight.py`'s own small grammar.
    pub missing: Vec<String>,
    pub message: Option<String>,
}

/// What [`ToolPlugin::execute`] needs that isn't *params* itself
/// (issue #449's gate lane, item 1) -- `core/tool.py::ToolRuntime.
/// execute`'s own per-attempt call carries all four of these to the
/// plugin it dispatches to, none of them through *params*:
///
/// - *timeout_seconds*: already resolved by the caller
///   (`ToolRuntime._resolve_timeout_seconds`'s own `timeout_ms`-vs-
///   `runtime.tools.timeout_seconds`-vs-default policy);
/// - *config*: `runtime.plugins.<name>` (read, never written, by a
///   plugin's own builder in [`registry::build_plugin`] -- a plugin's
///   `execute` almost never needs it again once built, but a few
///   (`shell`'s own host pin) read it at dispatch time too);
/// - *token*: this run's own [`CancellationToken`], so a plugin that
///   runs a subprocess ([`process::run_tracked`]) or waits on I/O can
///   stop promptly on cancellation instead of only being dropped from
///   outside;
/// - *armed*: `true` only for a CLI-driven run (`cli/runtime_shim.py`'s
///   own distinction between a `cof run` process and an embedded/
///   library call) -- `process::RunOpts::new_session`'s own policy
///   (`core/cancellation.py::run_tracked`'s own doc comment: a child
///   only ever gets its own process group while the run is armed)
///   reads this, not *token* itself.
pub struct ToolCall<'a> {
    pub timeout_seconds: u32,
    pub config: &'a Value,
    pub token: &'a CancellationToken,
    pub armed: bool,
}

/// DESIGN.md §8.1's shared tool-plugin trait, mirroring `plugins/
/// base.py::ToolPlugin`.
#[async_trait(?Send)]
pub trait ToolPlugin {
    fn name(&self) -> &str;
    async fn execute(&self, params: Value, call: &ToolCall<'_>) -> Result<ToolResult, ToolError>;
    /// Never panics -- a misbehaving `check()` degrades to `ok: false`
    /// (DESIGN.md §8.1's own comment); lane C's own registry, not this
    /// trait, is what actually has to enforce that by catching a panic
    /// at the call site.
    fn check(&self) -> CheckResult;
}

/// A name -> plugin lookup, kept for a caller (`electricity` (lib)'s own
/// `tool_registry()`) that still wants one long-lived instance per
/// provider rather than [`registry::build_plugin`]'s own per-attempt
/// build -- `json` (and, behind `test-tools`, `sleep`/`fail`) have no
/// per-plugin config to go stale between attempts, so a singleton
/// instance is harmless for them; a provider whose builder actually
/// reads `runtime.plugins.<name>` (lane C's `shell`, for one) is built
/// fresh per attempt through [`registry::build_plugin`] instead,
/// **not** registered here.
#[derive(Default)]
pub struct ToolRegistry {
    plugins: HashMap<String, Box<dyn ToolPlugin>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        ToolRegistry::default()
    }

    /// Registers *plugin* under its own [`ToolPlugin::name`], replacing
    /// any plugin already registered under that name.
    pub fn register(&mut self, plugin: Box<dyn ToolPlugin>) {
        self.plugins.insert(plugin.name().to_string(), plugin);
    }

    pub fn get(&self, provider: &str) -> Option<&dyn ToolPlugin> {
        self.plugins.get(provider).map(|boxed| boxed.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EchoTool;

    #[async_trait(?Send)]
    impl ToolPlugin for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }

        async fn execute(
            &self,
            params: Value,
            _call: &ToolCall<'_>,
        ) -> Result<ToolResult, ToolError> {
            Ok(ToolResult::new(params, Value::Dict(Default::default())))
        }

        fn check(&self) -> CheckResult {
            CheckResult {
                ok: true,
                ..Default::default()
            }
        }
    }

    static NONE: Value = Value::None;

    fn call(token: &CancellationToken) -> ToolCall<'_> {
        ToolCall {
            timeout_seconds: 30,
            config: &NONE,
            token,
            armed: false,
        }
    }

    #[test]
    fn registry_looks_up_a_registered_plugin_by_its_own_name() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool));
        assert!(registry.get("echo").is_some());
        assert!(registry.get("missing").is_none());
    }

    #[tokio::test]
    async fn a_registered_plugin_executes_through_the_trait_object() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(EchoTool));
        let plugin = registry.get("echo").unwrap();
        let token = CancellationToken::new();
        let result = plugin
            .execute(Value::Str("hi".to_string()), &call(&token))
            .await
            .unwrap();
        assert_eq!(result.value, Value::Str("hi".to_string()));
        assert!(result.ok);
    }

    #[test]
    fn validate_accepts_a_well_shaped_result() {
        let result = ToolResult::new(Value::None, Value::Dict(Default::default()));
        assert_eq!(result.validate("echo"), Vec::<String>::new());
    }

    #[test]
    fn validate_reports_a_non_dict_raw_by_its_python_type_name() {
        let result = ToolResult::new(Value::None, Value::Str("oops".to_string()));
        assert_eq!(
            result.validate("echo"),
            vec!["echo: 'raw' must be dict, got str".to_string()]
        );
    }

    #[test]
    fn validate_reports_a_negative_exit_code() {
        let mut result = ToolResult::new(Value::None, Value::Dict(Default::default()));
        result.exit_code = Some(-1);
        assert_eq!(
            result.validate("echo"),
            vec!["echo: 'exit_code' must be >= 0, got -1".to_string()]
        );
    }

    #[test]
    fn tool_error_message_defaults_to_runtime_error_with_no_cause() {
        let err = ToolError::message("boom");
        assert_eq!(err.py_class, PyExc::RuntimeError);
        assert_eq!(err.cause, ErrorCause::None);
        assert_eq!(err.to_string(), "boom");
    }
}
