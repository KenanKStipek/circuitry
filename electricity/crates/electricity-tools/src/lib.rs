//! electricity-tools: the `ToolPlugin` trait, a registry, and the `json`
//! tool (issue #431's Scope section, lane C; DESIGN.md §8.1). Lane A
//! ships this crate's final public shape -- [`ToolPlugin`], [`ToolResult`],
//! [`ToolError`], [`ToolRegistry`] -- plus the `json`/`test-tools` module
//! skeletons; the [`json`] module's own plugin is a stub (parse/
//! stringify/extract are lane C's own port of `plugins/json.py`).
//!
//! `#[async_trait]` (not a native `async fn` in a public trait) makes
//! [`ToolPlugin`] object-safe: [`ToolRegistry`] holds
//! `Box<dyn ToolPlugin>`, so a provider is looked up by name at run time,
//! the same dynamic dispatch Circuitry's own `ToolPlugin` `Protocol`
//! gets for free from Python's duck typing.

use async_trait::async_trait;
use electricity_value::Value;
use std::collections::HashMap;
use std::fmt;

pub mod json;
#[cfg(feature = "test-tools")]
pub mod test_tools;

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
}

/// A tool call's own raised error (as opposed to [`ToolResult::ok`]
/// `false`, a returned failure) -- the text a `meta.error` entry carries
/// when a plugin raises rather than returning an unsuccessful result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolError(pub String);

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
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

/// DESIGN.md §8.1's shared tool-plugin trait, mirroring `plugins/
/// base.py::ToolPlugin`.
#[async_trait(?Send)]
pub trait ToolPlugin {
    fn name(&self) -> &str;
    async fn execute(&self, params: Value, timeout_seconds: u32) -> Result<ToolResult, ToolError>;
    /// Never panics -- a misbehaving `check()` degrades to `ok: false`
    /// (DESIGN.md §8.1's own comment); lane C's own registry, not this
    /// trait, is what actually has to enforce that by catching a panic
    /// at the call site.
    fn check(&self) -> CheckResult;
}

/// A name -> plugin lookup -- `electricity_vm::run_tool`'s own provider
/// dispatch (issue #431's "Seams" section 5), and the one place lane C
/// registers `json` (and, behind `test-tools`, `sleep`/`fail`) without
/// `electricity-vm` ever needing to know their concrete types.
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
            _timeout_seconds: u32,
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
        let result = plugin
            .execute(Value::Str("hi".to_string()), 30)
            .await
            .unwrap();
        assert_eq!(result.value, Value::Str("hi".to_string()));
        assert!(result.ok);
    }
}
