//! Reference static plugin for ZeroClaw — also a community-facing template.
//!
//! Demonstrates the conventional shape of a third-party static plugin:
//! a single public [`register`] function that adds factories to the caller's
//! [`RegistrySet`].
//!
//! # Usage from a host
//!
//! ```
//! use std::sync::Arc;
//! use zeroclaw_api::plugin::{PluginRegistry, RegistrySet};
//!
//! let registries = Arc::new(RegistrySet::new());
//! zeroclaw_fixture_static::register(&registries);
//! assert!(registries.tools.contains("fixture-echo-static"));
//! ```

use async_trait::async_trait;
use zeroclaw_api::plugin::{PluginRegistry, RegistrySet};
use zeroclaw_api::tool::{Tool, ToolResult};

zeroclaw_api::tool_attribution!(FixtureEchoTool, zeroclaw_api::attribution::ToolKind::Plugin);

/// Tool that echoes its JSON arguments back as text.
///
/// Trivially small on purpose — its only job is to prove that the plugin
/// surface (Tool trait, registration, retrieval, async execute) works
/// end-to-end. Authors of real static plugins should mirror this structure.
pub struct FixtureEchoTool;

#[async_trait]
impl Tool for FixtureEchoTool {
    fn name(&self) -> &str {
        "fixture-echo-static"
    }

    fn description(&self) -> &str {
        "Static plugin fixture: echoes input args as output. Demonstrates the static plugin convention."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "additionalProperties": true,
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        Ok(ToolResult {
            success: true,
            output: args.to_string(),
            error: None,
        })
    }
}

/// Plugin entry point. Hosts call this once at startup, passing their
/// [`RegistrySet`]. The function registers every component this plugin
/// provides.
///
/// Conventional name (`register`) and signature (`fn(&RegistrySet)`) — see
/// `zeroclaw-api`'s `plugin` module documentation.
pub fn register(registries: &RegistrySet) {
    registries
        .tools
        .register("fixture-echo-static", |_cfg| Ok(Box::new(FixtureEchoTool)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_populates_tool_registry() {
        let registries = RegistrySet::new();
        register(&registries);
        assert!(registries.tools.contains("fixture-echo-static"));
        assert_eq!(
            registries.tools.list_registered(),
            vec!["fixture-echo-static".to_string()]
        );
    }

    #[tokio::test]
    async fn echo_tool_round_trips_json() {
        let registries = RegistrySet::new();
        register(&registries);
        let tool = registries
            .tools
            .get("fixture-echo-static", &serde_json::Value::Null)
            .expect("registered above");
        let out = tool
            .execute(serde_json::json!({ "msg": "hello" }))
            .await
            .unwrap();
        assert!(out.success);
        assert!(out.output.contains("hello"));
        assert!(out.error.is_none());
    }
}
