//! Multi-component dynamic plugin fixture for ZeroClaw.
//!
//! Demonstrates a single cdylib registering multiple components (two tools and
//! one provider) via the `#[plugin_module]` attribute.

use async_trait::async_trait;
use zeroclaw_api::model_provider::{ModelProvider, ProviderCapabilities};
use zeroclaw_api::tool::{Tool, ToolResult};
use zeroclaw_macros::plugin_module;

zeroclaw_api::tool_attribution!(EchoTool, zeroclaw_api::attribution::ToolKind::Plugin);
zeroclaw_api::tool_attribution!(ReverseTool, zeroclaw_api::attribution::ToolKind::Plugin);

// ── Tool 1: echo ─────────────────────────────────────────────────────────────

pub struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "multi-echo-tool"
    }

    fn description(&self) -> &str {
        "Multi-component fixture: echoes input args as output."
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

// ── Tool 2: reverse ──────────────────────────────────────────────────────────

pub struct ReverseTool;

#[async_trait]
impl Tool for ReverseTool {
    fn name(&self) -> &str {
        "multi-reverse-tool"
    }

    fn description(&self) -> &str {
        "Multi-component fixture: reverses the input string."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "text": { "type": "string" }
            },
            "required": ["text"],
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let text = args.get("text").and_then(|v| v.as_str()).unwrap_or("");
        Ok(ToolResult {
            success: true,
            output: text.chars().rev().collect(),
            error: None,
        })
    }
}

// ── Provider: dummy ──────────────────────────────────────────────────────────

pub struct DummyProvider;

impl zeroclaw_api::attribution::Attributable for DummyProvider {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        zeroclaw_api::attribution::Role::Provider(zeroclaw_api::attribution::ProviderKind::Model(
            zeroclaw_api::attribution::ModelProviderKind::Plugin,
        ))
    }
    fn alias(&self) -> &str {
        "multi-dummy-provider"
    }
}

#[async_trait]
impl ModelProvider for DummyProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            native_tool_calling: false,
            vision: false,
            prompt_caching: false,
            extended_thinking: false,
        }
    }

    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        message: &str,
        _model: &str,
        _temperature: Option<f64>,
    ) -> anyhow::Result<String> {
        Ok(format!("dummy-provider-reply: {message}"))
    }
}

// ── Plugin module: aggregates all components ─────────────────────────────────

#[plugin_module]
mod registry {
    use super::*;

    #[plugin_entry(tool = "multi-echo-tool")]
    fn make_echo_tool() -> Box<dyn Tool> {
        Box::new(EchoTool)
    }

    #[plugin_entry(tool = "multi-reverse-tool")]
    fn make_reverse_tool() -> Box<dyn Tool> {
        Box::new(ReverseTool)
    }

    #[plugin_entry(provider = "multi-dummy-provider")]
    fn make_dummy_provider() -> Box<dyn ModelProvider> {
        Box::new(DummyProvider)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_export_matches_api_constant() {
        assert_eq!(
            crate::registry::zc_api_version(),
            zeroclaw_api::version::API_VERSION_U32
        );
    }

    #[tokio::test]
    async fn echo_tool_round_trips_json() {
        let tool = EchoTool;
        let out = tool
            .execute(serde_json::json!({ "msg": "hello" }))
            .await
            .unwrap();
        assert!(out.success);
        assert!(out.output.contains("hello"));
    }

    #[tokio::test]
    async fn reverse_tool_reverses_text() {
        let tool = ReverseTool;
        let out = tool
            .execute(serde_json::json!({ "text": "hello" }))
            .await
            .unwrap();
        assert!(out.success);
        assert_eq!(out.output, "olleh");
    }

    #[tokio::test]
    async fn dummy_provider_replies() {
        let provider = DummyProvider;
        let reply = provider
            .chat_with_system(None, "ping", "any-model", None)
            .await
            .unwrap();
        assert!(reply.contains("dummy-provider-reply"));
        assert!(reply.contains("ping"));
    }
}
