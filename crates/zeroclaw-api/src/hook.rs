use async_trait::async_trait;
use serde_json::Value;
use std::time::Duration;

use crate::channel::ChannelMessage;
use crate::model_provider::{ChatMessage, ChatResponse};
use crate::tool::ToolResult;

/// Result of a modifying hook — continue with (possibly modified) data, or cancel.
#[derive(Debug, Clone)]
pub enum HookResult<T> {
    Continue(T),
    Cancel(String),
}

impl<T> HookResult<T> {
    pub fn is_cancel(&self) -> bool {
        matches!(self, HookResult::Cancel(_))
    }
}

/// Trait for hook handlers. All methods have default no-op implementations.
/// Implement only the events you care about.
#[async_trait]
pub trait HookHandler: Send + Sync {
    fn name(&self) -> &str;
    fn priority(&self) -> i32 {
        0
    }

    // --- Void hooks (parallel, fire-and-forget) ---
    async fn on_gateway_start(&self, _host: &str, _port: u16) {}
    async fn on_gateway_stop(&self) {}
    async fn on_session_start(&self, _session_id: &str, _channel: &str) {}
    async fn on_session_end(&self, _session_id: &str, _channel: &str) {}
    async fn on_llm_input(&self, _messages: &[ChatMessage], _model: &str) {}
    async fn on_llm_output(&self, _response: &ChatResponse) {}
    async fn on_after_tool_call(&self, _tool: &str, _result: &ToolResult, _duration: Duration) {}
    async fn on_message_sent(&self, _channel: &str, _recipient: &str, _content: &str) {}
    async fn on_heartbeat_tick(&self) {}
    async fn on_agent_end(
        &self,
        _channel: &str,
        _sender: &str,
        _user_input: &str,
        _agent_response: &str,
        _history: &[ChatMessage],
    ) {
    }

    // --- Modifying hooks (sequential by priority, can cancel) ---
    async fn before_model_resolve(
        &self,
        model_provider: String,
        model: String,
    ) -> HookResult<(String, String)> {
        HookResult::Continue((model_provider, model))
    }

    async fn before_prompt_build(&self, prompt: String) -> HookResult<String> {
        HookResult::Continue(prompt)
    }

    async fn before_llm_call(
        &self,
        messages: Vec<ChatMessage>,
        model: String,
    ) -> HookResult<(Vec<ChatMessage>, String)> {
        HookResult::Continue((messages, model))
    }

    async fn before_tool_call(&self, name: String, args: Value) -> HookResult<(String, Value)> {
        HookResult::Continue((name, args))
    }

    async fn on_message_received(&self, message: ChannelMessage) -> HookResult<ChannelMessage> {
        HookResult::Continue(message)
    }

    async fn on_message_sending(
        &self,
        channel: String,
        recipient: String,
        content: String,
    ) -> HookResult<(String, String, String)> {
        HookResult::Continue((channel, recipient, content))
    }

    // ------------- xydt custom hook -----------
    /// Called before agent reply executes.
    ///
    /// Returns a tuple of `(short_circuit_response, messages_to_append)`:
    /// - `Some(response)` in the first slot: skip the agent loop and return the
    ///   response directly. The append list is still merged into history by
    ///   the dispatcher before short-circuiting.
    /// - `None` in the first slot: continue with the normal agent loop, after
    ///   the dispatcher has appended the second-slot messages to history.
    /// - `Cancel(reason)`: abort the request.
    ///
    /// `history` is passed by shared reference so the dispatcher can keep
    /// ownership and avoid cloning the whole conversation per hook. The hook
    /// only needs to inspect history to decide what to append.
    ///
    /// Example use cases:
    /// - Keyword-based short-circuit responses (e.g., "/help" → show help)
    /// - Simple agent fallback with limited tools for short messages
    /// - Custom routing logic based on message content
    /// - Injecting extra context messages before the agent loop
    /// - Dynamic tool exclusion based on context
    async fn before_agent_reply(
        &self,
        _msg: &str,
        _history: &[ChatMessage],
        _agent_alias: &str,
    ) -> HookResult<(Option<String>, Vec<ChatMessage>)> {
        HookResult::Continue((None, Vec::new()))
    }
    /// Called after a tool result is built but before it's added to history.
    /// Allows hooks to modify or filter tool output.
    ///
    /// Parameters:
    /// - tool_name: Name of the tool that was executed
    /// - tool_args: Arguments passed to the tool (as JSON)
    /// - tool_call_id: Unique identifier for this tool call (if available)
    /// - output: The tool's output string (not yet wrapped in <tool_result> tags)
    ///
    /// Returns:
    /// - Continue((tool_call_id, modified_output)): Use the (possibly modified) result
    /// - Cancel(reason): Skip adding this tool result to history
    async fn after_tool_result_build(
        &self,
        _tool_name: String,
        _tool_args: serde_json::Value,
        tool_call_id: Option<String>,
        output: String,
    ) -> HookResult<(Option<String>, String)> {
        HookResult::Continue((tool_call_id, output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestHook {
        name: String,
        priority: i32,
    }

    impl TestHook {
        fn new(name: &str, priority: i32) -> Self {
            Self {
                name: name.to_string(),
                priority,
            }
        }
    }

    #[async_trait]
    impl HookHandler for TestHook {
        fn name(&self) -> &str {
            &self.name
        }
        fn priority(&self) -> i32 {
            self.priority
        }
    }

    #[test]
    fn hook_result_is_cancel() {
        let ok: HookResult<String> = HookResult::Continue("hi".into());
        assert!(!ok.is_cancel());
        let cancel: HookResult<String> = HookResult::Cancel("blocked".into());
        assert!(cancel.is_cancel());
    }

    #[test]
    fn default_priority_is_zero() {
        struct MinimalHook;
        #[async_trait]
        impl HookHandler for MinimalHook {
            fn name(&self) -> &str {
                "minimal"
            }
        }
        assert_eq!(MinimalHook.priority(), 0);
    }

    #[tokio::test]
    async fn default_modifying_hooks_pass_through() {
        let hook = TestHook::new("test", 0);
        match hook
            .before_tool_call("shell".into(), serde_json::json!({"cmd": "ls"}))
            .await
        {
            HookResult::Continue((name, _args)) => assert_eq!(name, "shell"),
            HookResult::Cancel(_) => panic!("should not cancel"),
        }
    }
}
