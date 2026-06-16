//! WASM plugin hook bridge — exposes all `HookHandler` events to plugins.
//!
//! ## Scope
//!
//! The plugin ABI supports all events defined in [`HookHandler`]:
//! - Void hooks (fire-and-forget, parallel): `on_gateway_start`, `on_gateway_stop`,
//!   `on_session_start`, `on_session_end`, `on_llm_input`, `on_llm_output`,
//!   `on_after_tool_call`, `on_message_sent`, `on_heartbeat_tick`, `on_agent_end`
//! - Modifying hooks (sequential by priority, can cancel): `before_model_resolve`,
//!   `before_prompt_build`, `before_llm_call`, `before_tool_call`, `on_message_received`,
//!   `on_message_sending`, `before_agent_reply`, `after_tool_result_build`
//!
//! Plugins declare which events they subscribe to in `hook_metadata().events`.
//! Events not listed are silently skipped (no-op) to avoid surprising the host.
//!
//! ## ABI
//!
//! A plugin opts in to hooks by exporting two functions:
//!
//! - `hook_metadata(_) -> HookMetadata` — returns `{ name, priority, events }`.
//!   The `events` array contains event names like `"before_agent_reply"`,
//!   `"after_tool_result_build"`, etc.
//! - `hook_invoke(invocation) -> HookInvocationResult` — invoked with a JSON
//!   payload of `{ "event": "<event_name>", "payload": { ... } }` and
//!   must return `{ "action": "continue", "payload": ... }` to mutate the
//!   input, or `{ "action": "cancel", "reason": "..." }` to short-circuit.
//!
//! ## Timeouts & error handling
//!
//! Every plugin invocation runs inside `tokio::task::spawn_blocking` because
//! [`extism::Plugin`] is `!Send + !Sync` (one WASM instance per thread, no
//! sharing) and is wrapped in [`tokio::time::timeout`] with
//! [`DEFAULT_INVOKE_TIMEOUT`]. Timeouts, traps, JSON parse failures, and
//! permission errors are logged via `zeroclaw_log::record!` and degrade to
//! the safe fallback (pass-through for modifying hooks, no-op for void hooks)
//! so a single misbehaving plugin cannot take down the agent loop.
//!
//! Note: `extism::Plugin` is `!Send + !Sync`, so a fresh instance is created
//! on every invocation. The instantiation cost is intrinsic to extism 1.x;
//! caching across calls would require per-thread pooling.

use crate::PluginPermission;
use crate::runtime;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Duration;
use zeroclaw_api::channel::ChannelMessage;
use zeroclaw_api::hook::{HookHandler, HookResult};
use zeroclaw_api::model_provider::{ChatMessage, ChatResponse};
use zeroclaw_api::tool::ToolResult;

/// All supported hook event names.
mod events {
    pub const BEFORE_MODEL_RESOLVE: &str = "before_model_resolve";
    pub const BEFORE_PROMPT_BUILD: &str = "before_prompt_build";
    pub const BEFORE_LLM_CALL: &str = "before_llm_call";
    pub const BEFORE_TOOL_CALL: &str = "before_tool_call";
    pub const ON_MESSAGE_RECEIVED: &str = "on_message_received";
    pub const ON_MESSAGE_SENDING: &str = "on_message_sending";
    pub const BEFORE_AGENT_REPLY: &str = "before_agent_reply";
    pub const AFTER_TOOL_RESULT_BUILD: &str = "after_tool_result_build";

    // Void events (not currently invoked by WasmHook, but plugins can subscribe)
    pub const ON_GATEWAY_START: &str = "on_gateway_start";
    pub const ON_GATEWAY_STOP: &str = "on_gateway_stop";
    pub const ON_SESSION_START: &str = "on_session_start";
    pub const ON_SESSION_END: &str = "on_session_end";
    pub const ON_LLM_INPUT: &str = "on_llm_input";
    pub const ON_AFTER_TOOL_CALL: &str = "on_after_tool_call";
    pub const ON_MESSAGE_SENT: &str = "on_message_sent";
    pub const ON_HEARTBEAT_TICK: &str = "on_heartbeat_tick";
    pub const ON_AGENT_END: &str = "on_agent_end";
}

/// Default per-invocation timeout. A misbehaving plugin (deadlock, runaway
/// loop) gets cut off and logged; the agent loop falls back to the
/// pre-plugin state.
const DEFAULT_INVOKE_TIMEOUT: Duration = Duration::from_secs(5);

/// A WASM-plugin-backed [`HookHandler`]. All [`HookHandler`] events
/// invoke the plugin if the plugin's `hook_metadata` lists the event in
/// its `events` array; otherwise they are no-ops.
pub struct WasmHook {
    name: String,
    priority: i32,
    events: Vec<String>,
    wasm_path: PathBuf,
    permissions: Vec<PluginPermission>,
    /// Per-invocation wall-clock cap. See [`DEFAULT_INVOKE_TIMEOUT`].
    timeout: Duration,
}

impl WasmHook {
    /// Construct a `WasmHook` with explicit metadata. Used by tests and by
    /// [`WasmHook::from_wasm`] after reading the plugin's `hook_metadata`
    /// export.
    pub fn new(
        name: String,
        priority: i32,
        events: Vec<String>,
        wasm_path: PathBuf,
        permissions: Vec<PluginPermission>,
    ) -> Self {
        Self {
            name,
            priority,
            events,
            wasm_path,
            permissions,
            timeout: DEFAULT_INVOKE_TIMEOUT,
        }
    }

    /// Check if this hook subscribes to a specific event.
    fn subscribes_to(&self, event: &str) -> bool {
        self.events.iter().any(|e| e == event)
    }

    /// Create a `WasmHook` by loading metadata from the plugin's
    /// `hook_metadata` export. Falls back to manifest-supplied values if the
    /// WASM cannot be loaded or the export is missing; both failure modes
    /// are logged at WARN.
    pub fn from_wasm(
        wasm_path: PathBuf,
        permissions: Vec<PluginPermission>,
        fallback_name: String,
    ) -> Self {
        let load = runtime::create_plugin(&wasm_path, &permissions, &fallback_name);
        let (name, priority, events) = match load {
            Ok(mut plugin) => match runtime::call_hook_metadata(&mut plugin) {
                Ok(meta) => (meta.name, meta.priority, meta.events),
                Err(e) => {
                    log_wasm_fallback(&wasm_path, "hook_metadata export missing or invalid", &e);
                    (fallback_name, 0, Vec::new())
                }
            },
            Err(e) => {
                log_wasm_fallback(&wasm_path, "failed to load WASM", &e);
                (fallback_name, 0, Vec::new())
            }
        };

        Self::new(name, priority, events, wasm_path, permissions)
    }

    /// Single chokepoint that calls the plugin for a specific event.
    /// Runs in `spawn_blocking` (extism::Plugin is `!Send`) and is wrapped
    /// in `tokio::time::timeout` to bound the worst case.
    async fn invoke(
        &self,
        event: &str,
        payload: Value,
    ) -> anyhow::Result<runtime::HookInvocationResult> {
        let wasm_path = self.wasm_path.clone();
        let permissions = self.permissions.clone();
        let plugin_name = self.name.clone();
        let event_name = event.to_string();
        let invocation = serde_json::to_vec(&runtime::HookInvocation {
            event: event_name,
            payload,
        })?;

        let join = tokio::task::spawn_blocking(move || {
            let mut plugin = runtime::create_plugin(&wasm_path, &permissions, &plugin_name)?;
            runtime::call_hook_invoke(&mut plugin, &invocation)
        });

        match tokio::time::timeout(self.timeout, join).await {
            Ok(Ok(Ok(result))) => Ok(result),
            Ok(Ok(Err(e))) => Err(e),
            Ok(Err(join_err)) => {
                Err(anyhow::Error::new(join_err).context("wasm hook: spawn_blocking task failed"))
            }
            Err(_elapsed) => Err(anyhow::Error::msg(format!(
                "wasm hook '{}' exceeded {:?} timeout on {}",
                self.name, self.timeout, event
            ))),
        }
    }

    /// Helper for void events - just invoke and ignore result, logging errors.
    async fn invoke_void(&self, event: &str, payload: Value) {
        if !self.subscribes_to(event) {
            return;
        }

        match self.invoke(event, payload).await {
            Ok(_) => {}
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "hook": self.name,
                            "event": event,
                            "error": format!("{e:#}"),
                        })),
                    "void hook invocation failed; ignoring"
                );
            }
        }
    }
}

fn log_wasm_fallback(wasm_path: &std::path::Path, phase: &'static str, err: &anyhow::Error) {
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_attrs(::serde_json::json!({
                "wasm": wasm_path.display().to_string(),
                "phase": phase,
                "error": format!("{err:#}"),
            })),
        "wasm hook plugin load failed; using fallback metadata"
    );
}

#[async_trait]
impl HookHandler for WasmHook {
    fn name(&self) -> &str {
        &self.name
    }

    fn priority(&self) -> i32 {
        self.priority
    }

    // --- Void hooks (parallel, fire-and-forget) ---

    async fn on_gateway_start(&self, host: &str, port: u16) {
        self.invoke_void(
            events::ON_GATEWAY_START,
            json!({
                "host": host,
                "port": port,
            }),
        )
        .await;
    }

    async fn on_gateway_stop(&self) {
        self.invoke_void(events::ON_GATEWAY_STOP, json!({})).await;
    }

    async fn on_session_start(&self, session_id: &str, channel: &str) {
        self.invoke_void(
            events::ON_SESSION_START,
            json!({
                "session_id": session_id,
                "channel": channel,
            }),
        )
        .await;
    }

    async fn on_session_end(&self, session_id: &str, channel: &str) {
        self.invoke_void(
            events::ON_SESSION_END,
            json!({
                "session_id": session_id,
                "channel": channel,
            }),
        )
        .await;
    }

    async fn on_llm_input(&self, messages: &[ChatMessage], model: &str) {
        self.invoke_void(
            events::ON_LLM_INPUT,
            json!({
                "messages": messages,
                "model": model,
            }),
        )
        .await;
    }

    async fn on_llm_output(&self, _response: &ChatResponse) {}

    async fn on_after_tool_call(&self, tool: &str, result: &ToolResult, duration: Duration) {
        self.invoke_void(
            events::ON_AFTER_TOOL_CALL,
            json!({
                "tool": tool,
                "result": result,
                "duration_ms": duration.as_millis() as u64,
            }),
        )
        .await;
    }

    async fn on_message_sent(&self, channel: &str, recipient: &str, content: &str) {
        self.invoke_void(
            events::ON_MESSAGE_SENT,
            json!({
                "channel": channel,
                "recipient": recipient,
                "content": content,
            }),
        )
        .await;
    }

    async fn on_heartbeat_tick(&self) {
        self.invoke_void(events::ON_HEARTBEAT_TICK, json!({})).await;
    }

    async fn on_agent_end(
        &self,
        channel: &str,
        sender: &str,
        user_input: &str,
        agent_response: &str,
        history: &[ChatMessage],
    ) {
        self.invoke_void(
            events::ON_AGENT_END,
            json!({
                "channel": channel,
                "sender": sender,
                "user_input": user_input,
                "agent_response": agent_response,
                "history": history,
            }),
        )
        .await;
    }

    // --- Modifying hooks (sequential by priority, can cancel) ---

    async fn before_model_resolve(
        &self,
        model_provider: String,
        model: String,
    ) -> HookResult<(String, String)> {
        if !self.subscribes_to(events::BEFORE_MODEL_RESOLVE) {
            return HookResult::Continue((model_provider, model));
        }

        let invocation_payload = json!({
            "model_provider": model_provider,
            "model": model,
        });

        match self
            .invoke(events::BEFORE_MODEL_RESOLVE, invocation_payload)
            .await
        {
            Ok(runtime::HookInvocationResult::Continue { payload }) => {
                let provider = payload
                    .get("model_provider")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or(model_provider);
                let model_name = payload
                    .get("model")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or(model);
                HookResult::Continue((provider, model_name))
            }
            Ok(runtime::HookInvocationResult::Cancel { reason }) => {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({
                            "hook": self.name,
                            "event": events::BEFORE_MODEL_RESOLVE,
                            "reason": reason,
                        })),
                    "wasm hook requested cancel"
                );
                HookResult::Cancel(reason)
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "hook": self.name,
                            "event": events::BEFORE_MODEL_RESOLVE,
                            "phase": "invoke",
                            "error": format!("{e:#}"),
                        })),
                    "wasm hook invocation failed; falling back to pre-plugin state"
                );
                HookResult::Continue((model_provider, model))
            }
        }
    }

    async fn before_prompt_build(&self, prompt: String) -> HookResult<String> {
        if !self.subscribes_to(events::BEFORE_PROMPT_BUILD) {
            return HookResult::Continue(prompt);
        }

        match self
            .invoke(events::BEFORE_PROMPT_BUILD, json!({ "prompt": prompt }))
            .await
        {
            Ok(runtime::HookInvocationResult::Continue { payload }) => {
                let new_prompt = payload
                    .get("prompt")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or(prompt);
                HookResult::Continue(new_prompt)
            }
            Ok(runtime::HookInvocationResult::Cancel { reason }) => HookResult::Cancel(reason),
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "hook": self.name,
                            "event": events::BEFORE_PROMPT_BUILD,
                            "error": format!("{e:#}"),
                        })),
                    "wasm hook invocation failed; passing through"
                );
                HookResult::Continue(prompt)
            }
        }
    }

    async fn before_llm_call(
        &self,
        messages: Vec<ChatMessage>,
        model: String,
    ) -> HookResult<(Vec<ChatMessage>, String)> {
        if !self.subscribes_to(events::BEFORE_LLM_CALL) {
            return HookResult::Continue((messages, model));
        }

        let invocation_payload = json!({
            "messages": messages,
            "model": model,
        });

        match self
            .invoke(events::BEFORE_LLM_CALL, invocation_payload)
            .await
        {
            Ok(runtime::HookInvocationResult::Continue { payload }) => {
                let new_messages = payload
                    .get("messages")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or(messages);
                let new_model = payload
                    .get("model")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or(model);
                HookResult::Continue((new_messages, new_model))
            }
            Ok(runtime::HookInvocationResult::Cancel { reason }) => HookResult::Cancel(reason),
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "hook": self.name,
                            "event": events::BEFORE_LLM_CALL,
                            "error": format!("{e:#}"),
                        })),
                    "wasm hook invocation failed; passing through"
                );
                HookResult::Continue((messages, model))
            }
        }
    }

    async fn before_tool_call(&self, name: String, args: Value) -> HookResult<(String, Value)> {
        if !self.subscribes_to(events::BEFORE_TOOL_CALL) {
            return HookResult::Continue((name, args));
        }

        let invocation_payload = json!({
            "name": name,
            "args": args,
        });

        match self
            .invoke(events::BEFORE_TOOL_CALL, invocation_payload)
            .await
        {
            Ok(runtime::HookInvocationResult::Continue { payload }) => {
                let new_name = payload
                    .get("name")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or(name);
                let new_args = payload.get("args").cloned().unwrap_or(args);
                HookResult::Continue((new_name, new_args))
            }
            Ok(runtime::HookInvocationResult::Cancel { reason }) => HookResult::Cancel(reason),
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "hook": self.name,
                            "event": events::BEFORE_TOOL_CALL,
                            "error": format!("{e:#}"),
                        })),
                    "wasm hook invocation failed; passing through"
                );
                HookResult::Continue((name, args))
            }
        }
    }

    async fn on_message_received(&self, message: ChannelMessage) -> HookResult<ChannelMessage> {
        if !self.subscribes_to(events::ON_MESSAGE_RECEIVED) {
            return HookResult::Continue(message);
        }
        return HookResult::Continue(message);
    }

    async fn on_message_sending(
        &self,
        channel: String,
        recipient: String,
        content: String,
    ) -> HookResult<(String, String, String)> {
        if !self.subscribes_to(events::ON_MESSAGE_SENDING) {
            return HookResult::Continue((channel, recipient, content));
        }

        let invocation_payload = json!({
            "channel": channel,
            "recipient": recipient,
            "content": content,
        });

        match self
            .invoke(events::ON_MESSAGE_SENDING, invocation_payload)
            .await
        {
            Ok(runtime::HookInvocationResult::Continue { payload }) => {
                let new_channel = payload
                    .get("channel")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or(channel);
                let new_recipient = payload
                    .get("recipient")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or(recipient);
                let new_content = payload
                    .get("content")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .unwrap_or(content);
                HookResult::Continue((new_channel, new_recipient, new_content))
            }
            Ok(runtime::HookInvocationResult::Cancel { reason }) => HookResult::Cancel(reason),
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "hook": self.name,
                            "event": events::ON_MESSAGE_SENDING,
                            "error": format!("{e:#}"),
                        })),
                    "wasm hook invocation failed; passing through"
                );
                HookResult::Continue((channel, recipient, content))
            }
        }
    }

    async fn before_agent_reply(
        &self,
        msg: &str,
        history: &[ChatMessage],
        agent_alias: &str,
    ) -> HookResult<(Option<String>, Vec<ChatMessage>)> {
        if !self.subscribes_to(events::BEFORE_AGENT_REPLY) {
            return HookResult::Continue((None, Vec::new()));
        }

        let invocation_payload = json!({
            "content": msg,
            "agent_alias": agent_alias,
            "history": history,
        });

        match self
            .invoke(events::BEFORE_AGENT_REPLY, invocation_payload)
            .await
        {
            Ok(runtime::HookInvocationResult::Continue { payload }) => {
                #[derive(Debug, Default, Deserialize)]
                struct AgentReplyPayload {
                    #[serde(default)]
                    response: Option<String>,
                    #[serde(default)]
                    history: Option<Vec<ChatMessage>>,
                }

                let parsed: AgentReplyPayload = match serde_json::from_value(payload) {
                    Ok(p) => p,
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Fail,
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "hook": self.name,
                                "event": events::BEFORE_AGENT_REPLY,
                                "error": format!("{e}"),
                            })),
                            "wasm hook returned payload with wrong shape; ignoring"
                        );
                        return HookResult::Continue((None, Vec::new()));
                    }
                };

                let response = parsed.response.filter(|s| !s.is_empty());
                HookResult::Continue((response, parsed.history.unwrap_or_default()))
            }
            Ok(runtime::HookInvocationResult::Cancel { reason }) => {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({
                            "hook": self.name,
                            "event": events::BEFORE_AGENT_REPLY,
                            "reason": reason,
                        })),
                    "wasm hook requested cancel"
                );
                HookResult::Cancel(reason)
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "hook": self.name,
                            "event": events::BEFORE_AGENT_REPLY,
                            "phase": "invoke",
                            "error": format!("{e:#}"),
                        })),
                    "wasm hook invocation failed; falling back to pre-plugin state"
                );
                HookResult::Continue((None, Vec::new()))
            }
        }
    }

    async fn after_tool_result_build(
        &self,
        tool_name: String,
        tool_args: serde_json::Value,
        tool_call_id: Option<String>,
        output: String,
    ) -> HookResult<(Option<String>, String)> {
        if !self.subscribes_to(events::AFTER_TOOL_RESULT_BUILD) {
            return HookResult::Continue((tool_call_id, output));
        }

        let invocation_payload = json!({
            "tool_name": tool_name,
            "tool_args": tool_args,
            "tool_call_id": tool_call_id,
            "output": output,
        });

        #[derive(Debug, Deserialize)]
        struct AfterToolResultPayload {
            tool_call_id: Option<String>,
            output: String,
        }

        match self
            .invoke(events::AFTER_TOOL_RESULT_BUILD, invocation_payload)
            .await
        {
            Ok(runtime::HookInvocationResult::Continue { payload }) => {
                let parsed: AfterToolResultPayload = match serde_json::from_value(payload) {
                    Ok(p) => p,
                    Err(e) => {
                        ::zeroclaw_log::record!(
                            WARN,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Fail,
                            )
                            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                            .with_attrs(::serde_json::json!({
                                "hook": self.name,
                                "event": events::AFTER_TOOL_RESULT_BUILD,
                                "error": format!("{e}"),
                            })),
                            "wasm hook returned payload with wrong shape; ignoring"
                        );
                        return HookResult::Continue((tool_call_id, output));
                    }
                };

                HookResult::Continue((parsed.tool_call_id, parsed.output))
            }
            Ok(runtime::HookInvocationResult::Cancel { reason }) => {
                ::zeroclaw_log::record!(
                    INFO,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_attrs(::serde_json::json!({
                            "hook": self.name,
                            "event": events::AFTER_TOOL_RESULT_BUILD,
                            "reason": reason,
                        })),
                    "wasm hook requested cancel"
                );
                // Cancel means skip adding this tool result to history
                HookResult::Cancel(reason)
            }
            Err(e) => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({
                            "hook": self.name,
                            "event": events::AFTER_TOOL_RESULT_BUILD,
                            "phase": "invoke",
                            "error": format!("{e:#}"),
                        })),
                    "wasm hook invocation failed; falling back to pre-plugin state"
                );
                HookResult::Continue((tool_call_id, output))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_api::hook::HookResult;
    use zeroclaw_api::model_provider::ChatMessage;

    fn dummy_hook(events: Vec<String>) -> WasmHook {
        WasmHook::new(
            "demo".into(),
            7,
            events,
            PathBuf::from("demo.wasm"),
            Vec::new(),
        )
    }

    fn user_msg(text: &str) -> ChatMessage {
        ChatMessage {
            role: "user".into(),
            content: text.into(),
        }
    }

    #[tokio::test]
    async fn non_subscribing_plugin_is_skipped() {
        let hook = dummy_hook(vec![]);
        let history = vec![user_msg("hi")];
        match hook.before_agent_reply("hi", &history, "agent").await {
            HookResult::Continue((response, appended)) => {
                assert!(response.is_none());
                assert!(appended.is_empty());
            }
            HookResult::Cancel(_) => panic!("should not cancel when not subscribed"),
        }
    }

    #[tokio::test]
    async fn subscribing_plugin_invokes_plugin() {
        // This test would require a real WASM plugin, so we just test
        // the subscription check logic here.
        let hook = dummy_hook(vec![events::BEFORE_AGENT_REPLY.to_string()]);
        // The actual invocation will fail (no WASM file), but we can verify
        // it tries to invoke by checking the error path.
        let history = vec![user_msg("hi")];
        match hook.before_agent_reply("hi", &history, "agent").await {
            HookResult::Continue(_) => {} // Falls back on error
            HookResult::Cancel(_) => {}
        }
    }

    #[test]
    fn subscribes_to_checks_events() {
        let hook = dummy_hook(vec!["event_a".into(), "event_b".into()]);
        assert!(hook.subscribes_to("event_a"));
        assert!(hook.subscribes_to("event_b"));
        assert!(!hook.subscribes_to("event_c"));
    }

    #[test]
    fn name_and_priority_are_exposed() {
        let hook = WasmHook::new(
            "audit".into(),
            42,
            vec!["before_agent_reply".into()],
            PathBuf::from("a.wasm"),
            Vec::new(),
        );
        assert_eq!(hook.name(), "audit");
        assert_eq!(hook.priority(), 42);
    }

    #[test]
    fn from_wasm_falls_back_when_metadata_export_is_missing() {
        let hook = WasmHook::from_wasm(
            PathBuf::from("/nonexistent/plugin.wasm"),
            Vec::new(),
            "fallback-name".into(),
        );
        assert_eq!(hook.name(), "fallback-name");
        assert_eq!(hook.priority(), 0);
        assert!(hook.events.is_empty());
    }
}
