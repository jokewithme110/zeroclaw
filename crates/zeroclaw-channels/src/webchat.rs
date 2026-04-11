use anyhow::{Result, bail};
use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, header},
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
    routing::post,
};
use serde::{Deserialize, Serialize};
use std::{collections::HashMap, convert::Infallible, sync::Arc};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_stream::{StreamExt, wrappers::ReceiverStream};
use uuid::Uuid;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_config::schema::build_runtime_proxy_client;
use zeroclaw_runtime::security::pairing::PairingGuard;

#[derive(Debug)]
pub struct WebchatChannel {
    listen_port: u16,
    listen_path: String,
    callback_url: Option<String>,
    callback_auth_header: Option<String>,
    support_reasoning: bool,
    pairing: PairingGuard,
    sessions: Arc<Mutex<HashMap<String, SessionEntry>>>,
}

#[derive(Debug)]
struct SessionEntry {
    sender: String,
    model_label: String,
    completion_id: String,
    created: i64,
    wants_stream: bool,
    first_chunk_sent: bool,
    mode: SessionMode,
}

#[derive(Debug)]
enum SessionMode {
    Stream(mpsc::Sender<StreamFrame>),
    AwaitFinalize(oneshot::Sender<String>),
    CallbackOnly,
}

#[derive(Debug, Clone)]
enum StreamFrame {
    Chunk(serde_json::Value),
    Done,
}

/// Request schema aligned with `dt_nodes_registry::response::HttpChatRequest`.
#[derive(Debug, Deserialize)]
struct HttpChatRequest {
    model: Option<String>,
    messages: Vec<OpenAiChatMessage>,
    #[serde(default)]
    stream: bool,
    session_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAiChatMessage {
    role: String,
    #[serde(default)]
    content: String,
}

#[derive(Debug, Serialize)]
struct AcceptedResponse {
    accepted: bool,
    session_id: String,
}

impl WebchatChannel {
    pub fn new(
        listen_port: u16,
        listen_path: Option<String>,
        callback_url: Option<String>,
        callback_auth_header: Option<String>,
        support_reasoning: bool,
        require_pairing: bool,
        paired_tokens: Vec<String>,
    ) -> Self {
        let path = listen_path.unwrap_or_else(|| "/webchat".to_string());
        let listen_path = if path.starts_with('/') {
            path
        } else {
            format!("/{path}")
        };
        Self {
            listen_port,
            listen_path,
            callback_url,
            callback_auth_header,
            support_reasoning,
            pairing: PairingGuard::new(require_pairing, &paired_tokens),
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn has_callback(&self) -> bool {
        self.callback_url.is_some()
    }

    fn http_client(&self) -> reqwest::Client {
        build_runtime_proxy_client("channel.webchat")
    }
    async fn send_callback_payload(
        &self,
        _session_id: &str,
        _sender: &str,
        payload: serde_json::Value,
    ) -> Result<()> {
        let Some(ref url) = self.callback_url else {
            bail!("webchat callback_url is not configured");
        };
        let client = self.http_client();
        let mut req = client.post(url).json(&payload);
        if let Some(ref auth) = self.callback_auth_header {
            req = req.header("Authorization", auth);
        }
        let resp = req.send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp
                .text()
                .await
                .unwrap_or_else(|e| format!("<failed to read response: {e}>"));
            bail!("webchat callback failed ({status}): {body}");
        }
        Ok(())
    }

    fn user_content_from_messages(messages: &[OpenAiChatMessage]) -> Option<String> {
        messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .map(|m| m.content.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    fn extract_bearer_token(headers: &HeaderMap) -> Option<&str> {
        headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|auth| {
                auth.strip_prefix("Bearer ")
                    .or_else(|| auth.strip_prefix("bearer "))
            })
            .map(str::trim)
            .filter(|token| !token.is_empty())
    }

    fn is_request_authorized(&self, headers: &HeaderMap) -> bool {
        if !self.pairing.require_pairing() {
            return true;
        }
        let token = Self::extract_bearer_token(headers).unwrap_or("");
        self.pairing.is_authenticated(token)
    }

    fn completion_json(id: &str, created: i64, model: &str, content: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "object": "chat.completion",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": content,
                },
                "finish_reason": "stop",
            }],
        })
    }
    fn stream_chunk_json(
        id: &str,
        created: i64,
        model: &str,
        delta: serde_json::Value,
        finish_reason: Option<&str>,
        is_thinking: bool,
    ) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": finish_reason,
            }],
            "is_thinking": is_thinking,
        })
    }
}

impl Clone for WebchatChannel {
    fn clone(&self) -> Self {
        Self {
            listen_port: self.listen_port,
            listen_path: self.listen_path.clone(),
            callback_url: self.callback_url.clone(),
            callback_auth_header: self.callback_auth_header.clone(),
            support_reasoning: self.support_reasoning,
            pairing: self.pairing.clone(),
            sessions: Arc::clone(&self.sessions),
        }
    }
}

#[async_trait]
impl Channel for WebchatChannel {
    fn name(&self) -> &str {
        "webchat"
    }

    /// Whether this channel supports progressive message updates via draft edits.
    fn supports_draft_updates(&self) -> bool {
        true
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        self.finalize_draft(&message.recipient, "", &message.content)
            .await
    }
    async fn send_draft(&self, message: &SendMessage) -> Result<Option<String>> {
        let session_id = message.recipient.clone();
        let _ = message;
        Ok(Some(session_id))
    }

    /// Stream assistant **answer** text (`delta.content`). `text` is the full accumulated
    /// body from the agent loop (same contract as Telegram/Slack draft updates).
    async fn update_draft(&self, recipient: &str, _message_id: &str, text: &str) -> Result<()> {
        if text.is_empty() || !self.support_reasoning {
            return Ok(());
        }
        let dispatch = {
            let mut sessions = self.sessions.lock().await;
            let Some(entry) = sessions.get_mut(recipient) else {
                return Ok(());
            };
            if !entry.wants_stream {
                return Ok(());
            }
            let delta = if entry.first_chunk_sent {
                serde_json::json!({ "content": text })
            } else {
                entry.first_chunk_sent = true;
                serde_json::json!({ "role": "assistant", "content": text })
            };
            let payload = Self::stream_chunk_json(
                &entry.completion_id,
                entry.created,
                &entry.model_label,
                delta,
                None,
                false,
            );
            match &entry.mode {
                SessionMode::Stream(tx) => {
                    Some((entry.sender.clone(), payload, Some(tx.clone()), false))
                }
                SessionMode::CallbackOnly => Some((entry.sender.clone(), payload, None, true)),
                SessionMode::AwaitFinalize(_) => None,
            }
        };

        let Some((sender, payload, tx, callback_only)) = dispatch else {
            return Ok(());
        };

        if callback_only {
            let _ = self
                .send_callback_payload(recipient, &sender, payload)
                .await;
            return Ok(());
        }

        if let Some(tx) = tx {
            let _ = tx.send(StreamFrame::Chunk(payload)).await;
        }
        Ok(())
    }

    async fn update_draft_progress(
        &self,
        recipient: &str,
        _message_id: &str,
        text: &str,
    ) -> Result<()> {
        let dispatch = {
            let mut sessions = self.sessions.lock().await;
            let Some(entry) = sessions.get_mut(recipient) else {
                return Ok(());
            };
            if !entry.wants_stream {
                return Ok(());
            }
            let delta = if entry.first_chunk_sent {
                serde_json::json!({ "content": text })
            } else {
                entry.first_chunk_sent = true;
                serde_json::json!({ "role": "assistant", "content": text })
            };
            let payload = Self::stream_chunk_json(
                &entry.completion_id,
                entry.created,
                &entry.model_label,
                delta,
                None,
                true,
            );
            match &entry.mode {
                SessionMode::Stream(tx) => {
                    Some((entry.sender.clone(), payload, Some(tx.clone()), false))
                }
                SessionMode::CallbackOnly => Some((entry.sender.clone(), payload, None, true)),
                SessionMode::AwaitFinalize(_) => None,
            }
        };

        let Some((sender, payload, tx, callback_only)) = dispatch else {
            return Ok(());
        };

        if callback_only {
            let _ = self
                .send_callback_payload(recipient, &sender, payload)
                .await;
            return Ok(());
        }

        if let Some(tx) = tx {
            let _ = tx.send(StreamFrame::Chunk(payload)).await;
        }
        Ok(())
    }

    // 思考内容流式返回
    async fn update_draft_reasoning(
        &self,
        recipient: &str,
        _message_id: &str,
        reasoning: &str,
    ) -> Result<()> {
        if !self.support_reasoning {
            return Ok(());
        }
        let dispatch = {
            let mut sessions = self.sessions.lock().await;
            let Some(entry) = sessions.get_mut(recipient) else {
                return Ok(());
            };
            if !entry.wants_stream {
                return Ok(());
            }
            let delta = if entry.first_chunk_sent {
                serde_json::json!({ "reasoning_content": reasoning })
            } else {
                entry.first_chunk_sent = true;
                serde_json::json!({
                    "role": "assistant",
                    "reasoning_content": reasoning,
                })
            };
            let payload = Self::stream_chunk_json(
                &entry.completion_id,
                entry.created,
                &entry.model_label,
                delta,
                None,
                true,
            );
            match &entry.mode {
                SessionMode::Stream(tx) => {
                    Some((entry.sender.clone(), payload, Some(tx.clone()), false))
                }
                SessionMode::CallbackOnly => Some((entry.sender.clone(), payload, None, true)),
                SessionMode::AwaitFinalize(_) => None,
            }
        };

        let Some((sender, payload, tx, callback_only)) = dispatch else {
            return Ok(());
        };

        if callback_only {
            let _ = self
                .send_callback_payload(recipient, &sender, payload)
                .await;
            return Ok(());
        }

        if let Some(tx) = tx {
            let _ = tx.send(StreamFrame::Chunk(payload)).await;
        }
        Ok(())
    }

    // 最终的结果非流式返回
    async fn finalize_draft(&self, recipient: &str, _message_id: &str, text: &str) -> Result<()> {
        let entry = {
            let mut sessions = self.sessions.lock().await;
            sessions.remove(recipient)
        };
        let Some(entry) = entry else {
            return Ok(());
        };

        match entry.mode {
            SessionMode::Stream(tx) => {
                if !text.is_empty() && !self.support_reasoning {
                    let delta = if entry.first_chunk_sent {
                        serde_json::json!({ "content": text })
                    } else {
                        serde_json::json!({ "role": "assistant", "content": text })
                    };
                    let payload = Self::stream_chunk_json(
                        &entry.completion_id,
                        entry.created,
                        &entry.model_label,
                        delta,
                        None,
                        false,
                    );
                    let _ = tx.send(StreamFrame::Chunk(payload)).await;
                }
                let stop_payload = Self::stream_chunk_json(
                    &entry.completion_id,
                    entry.created,
                    &entry.model_label,
                    serde_json::json!({}),
                    Some("stop"),
                    false,
                );
                let _ = tx.send(StreamFrame::Chunk(stop_payload)).await;
                let _ = tx.send(StreamFrame::Done).await;
            }
            SessionMode::AwaitFinalize(done_tx) => {
                let _ = done_tx.send(text.to_string());
            }
            SessionMode::CallbackOnly => {
                if entry.wants_stream {
                    if !text.is_empty() {
                        let delta = if entry.first_chunk_sent {
                            serde_json::json!({ "content": text })
                        } else {
                            serde_json::json!({ "role": "assistant", "content": text })
                        };
                        let payload = Self::stream_chunk_json(
                            &entry.completion_id,
                            entry.created,
                            &entry.model_label,
                            delta,
                            None,
                            false,
                        );
                        let _ = self
                            .send_callback_payload(recipient, &entry.sender, payload)
                            .await;
                    }
                    let stop_payload = Self::stream_chunk_json(
                        &entry.completion_id,
                        entry.created,
                        &entry.model_label,
                        serde_json::json!({}),
                        Some("stop"),
                        false,
                    );
                    let _ = self
                        .send_callback_payload(recipient, &entry.sender, stop_payload)
                        .await;
                } else {
                    let payload = Self::completion_json(
                        &entry.completion_id,
                        entry.created,
                        &entry.model_label,
                        text,
                    );
                    self.send_callback_payload(recipient, &entry.sender, payload)
                        .await?;
                }
            }
        }
        Ok(())
    }

    async fn cancel_draft(&self, recipient: &str, _message_id: &str) -> Result<()> {
        let mut sessions = self.sessions.lock().await;
        sessions.remove(recipient);
        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        #[derive(Clone)]
        struct AppState {
            channel: Arc<WebchatChannel>,
            tx: tokio::sync::mpsc::Sender<ChannelMessage>,
        }
        async fn handle_incoming(
            State(state): State<AppState>,
            headers: HeaderMap,
            Json(body): Json<HttpChatRequest>,
        ) -> axum::response::Response {
            if !state.channel.is_request_authorized(&headers) {
                return (
                    axum::http::StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({
                        "error": "Unauthorized — provide Authorization: Bearer <token>"
                    })),
                )
                    .into_response();
            }

            if body.messages.is_empty() {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": "messages must not be empty" })),
                )
                    .into_response();
            }

            let Some(user_content) = WebchatChannel::user_content_from_messages(&body.messages)
            else {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": "last user message content must not be empty" })),
                )
                    .into_response();
            };

            let session_id = body
                .session_id
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "agent_default_session".to_string());
            let sender = session_id.clone();
            let has_callback = state.channel.has_callback();
            let wants_stream = body.stream;
            let model_label = body
                .model
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("agemt::main")
                .to_string();
            let completion_id = format!("chatcmpl-{}", Uuid::new_v4().simple());
            let created = chrono::Utc::now().timestamp();

            let (mode, sse, wait_rx) = if has_callback {
                (SessionMode::CallbackOnly, None, None)
            } else if wants_stream {
                let (evt_tx, evt_rx) = mpsc::channel::<StreamFrame>(128);
                let stream = ReceiverStream::new(evt_rx).map(|evt| {
                    let data = match evt {
                        StreamFrame::Chunk(payload) => payload.to_string(),
                        StreamFrame::Done => "[DONE]".to_string(),
                    };
                    Ok::<Event, Infallible>(Event::default().data(data))
                });
                (
                    SessionMode::Stream(evt_tx),
                    Some(
                        Sse::new(stream)
                            .keep_alive(KeepAlive::default())
                            .into_response(),
                    ),
                    None,
                )
            } else {
                let (done_tx, done_rx) = oneshot::channel::<String>();
                (SessionMode::AwaitFinalize(done_tx), None, Some(done_rx))
            };

            {
                let mut sessions = state.channel.sessions.lock().await;
                sessions.insert(
                    session_id.clone(),
                    SessionEntry {
                        sender: sender.clone(),
                        model_label: model_label.clone(),
                        completion_id: completion_id.clone(),
                        created,
                        wants_stream,
                        first_chunk_sent: false,
                        mode,
                    },
                );
            }

            let msg = ChannelMessage {
                id: format!("webchat_{}", Uuid::new_v4()),
                sender,
                reply_target: session_id.clone(),
                content: user_content,
                channel: "webchat".to_string(),
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                thread_ts: None,
                interruption_scope_id: None,
                attachments: Vec::new(),
            };

            if state.tx.send(msg).await.is_err() {
                return (
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({ "error": "agent channel closed" })),
                )
                    .into_response();
            }

            if has_callback {
                return (
                    axum::http::StatusCode::ACCEPTED,
                    Json(AcceptedResponse {
                        accepted: true,
                        session_id,
                    }),
                )
                    .into_response();
            }

            if let Some(resp) = sse {
                return resp;
            }

            if let Some(done_rx) = wait_rx {
                match done_rx.await {
                    Ok(text) => {
                        return (
                            axum::http::StatusCode::OK,
                            Json(WebchatChannel::completion_json(
                                &completion_id,
                                created,
                                &model_label,
                                &text,
                            )),
                        )
                            .into_response();
                    }
                    Err(_) => {
                        return (
                            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                            Json(serde_json::json!({ "error": "final response channel closed" })),
                        )
                            .into_response();
                    }
                }
            }

            (
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "unexpected webchat state",
            )
                .into_response()
        }
        let state = AppState {
            channel: Arc::new(self.clone()),
            tx,
        };

        let app = Router::new()
            .route(&self.listen_path, post(handle_incoming))
            .with_state(state);

        let addr = std::net::SocketAddr::from(([0, 0, 0, 0], self.listen_port));
        tracing::info!(
            "Webchat channel listening on http://0.0.0.0:{}{} ...",
            self.listen_port,
            self.listen_path
        );

        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app)
            .await
            .map_err(|e| anyhow::anyhow!("webchat server error: {e}"))?;
        Ok(())
    }
}
