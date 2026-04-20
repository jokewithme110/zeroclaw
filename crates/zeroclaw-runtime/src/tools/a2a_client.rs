use crate::security::SecurityPolicy;
use anyhow::Result;
use async_trait::async_trait;
use ra2a::client::{CallMeta, Client, JsonRpcTransport, StaticCallMetaInjector, TransportConfig};
use ra2a::types::{Message, MessageSendParams, Part};
use serde::Serialize;
use serde_json::json;
use std::sync::Arc;
use zeroclaw_api::tool::{Tool, ToolResult};

const DEFAULT_TIMEOUT_MS: u64 = 300_000;

pub struct A2aClientTool {
    security: Arc<SecurityPolicy>,
}

impl A2aClientTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self { security }
    }

    fn token_from_args(args: &serde_json::Value) -> Option<String> {
        args.get("token")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToOwned::to_owned)
    }

    fn build_client(a2a_root_url: &str, token: Option<&str>) -> Result<Client> {
        let mut config = TransportConfig::new(a2a_root_url.to_string());
        config.timeout_secs = DEFAULT_TIMEOUT_MS.div_ceil(1000);
        let transport = JsonRpcTransport::new(config)?;
        let mut client = Client::new(Box::new(transport)).with_base_url(a2a_root_url.to_string());
        if let Some(token) = token {
            let mut call_meta = CallMeta::default();
            call_meta.append("authorization", format!("Bearer {token}"));
            client = client.with_interceptor(StaticCallMetaInjector::new(call_meta));
        }
        Ok(client)
    }

    fn to_pretty_json<T: Serialize>(value: &T) -> String {
        serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_string())
    }
}

#[async_trait]
impl Tool for A2aClientTool {
    fn name(&self) -> &str { "a2a_client" }
    fn description(&self) -> &str { "Call a remote A2A agent via message/send." }
    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "agent_url": {"type": "string"},
                "token": {"type": "string"},
                "message": {"type": "string"}
            },
            "required": ["agent_url", "token", "message"]
        })
    }
    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        if !self.security.can_act() {
            return Ok(ToolResult { success: false, output: String::new(), error: Some("Action blocked: autonomy is read-only".into()) });
        }
        if !self.security.record_action() {
            return Ok(ToolResult { success: false, output: String::new(), error: Some("Action blocked: rate limit exceeded".into()) });
        }
        let agent_url_raw = args.get("agent_url").and_then(|v| v.as_str()).unwrap_or_default();
        let agent_url = agent_url_raw.trim().trim_end_matches('/').to_string();
        if agent_url.is_empty() {
            return Ok(ToolResult { success: false, output: String::new(), error: Some("Missing or empty 'agent_url' parameter".into()) });
        }
        let message = args.get("message").and_then(|v| v.as_str()).map(str::trim).unwrap_or("");
        if message.is_empty() {
            return Ok(ToolResult { success: false, output: String::new(), error: Some("Missing or empty 'message' parameter".into()) });
        }
        let token = Self::token_from_args(&args);
        let client = match Self::build_client(&agent_url, token.as_deref()) {
            Ok(client) => client,
            Err(error) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to build ra2a client: {error}")),
                })
            }
        };
        let outbound = Message::user(vec![Part::text(message)]);
        let params = MessageSendParams::new(outbound);
        match client.send_message(&params).await {
            Ok(result) => Ok(ToolResult { success: true, output: Self::to_pretty_json(&result), error: None }),
            Err(error) => Ok(ToolResult { success: false, output: String::new(), error: Some(format!("ra2a message/send failed: {error}")) }),
        }
    }
}
