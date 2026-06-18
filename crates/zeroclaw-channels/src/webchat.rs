use anyhow::{Error, Result};
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
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use std::{collections::HashMap, convert::Infallible, fs, path::PathBuf, sync::Arc};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_stream::{StreamExt, wrappers::ReceiverStream};
use uuid::Uuid;
use zeroclaw_api::attribution::{Attributable, Role, ToolKind};
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
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
    workspace_dir: PathBuf,
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
    /// Optional node request metadata for event emission
    #[serde(default)]
    node_req: Option<NodeRequest>,
}

/// Node request metadata for event emission
#[derive(Debug, Deserialize, Clone)]
pub struct NodeRequest {
    /// Event type (e.g., "alert.cpu")
    pub event: String,
    /// Channel ID (e.g., "qq", "wechat")
    pub channel_id: String,
    /// Recipient identifier (e.g., "user:123")
    pub recipient: String,
    /// Original message content
    pub message: String,
    /// Optional image data (base64 encoded)
    #[serde(default)]
    pub images: Option<Vec<ImageData>>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ImageData {
    pub filename: String,
    pub base64: String,
}

#[derive(Debug, Deserialize)]
struct OpenAiChatMessage {
    role: String,
    #[serde(default)]
    content: String,
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
        workspace_dir: PathBuf,
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
            workspace_dir,
        }
    }

    fn user_content_from_messages(messages: &[OpenAiChatMessage]) -> Option<String> {
        messages
            .iter()
            .rev()
            .find(|m| m.role == "user")
            .map(|m| m.content.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    /// 保存 base64 图片到本地并返回文件路径
    fn save_image_from_base64(&self, base64_data: &str, filename: &str) -> Result<String, String> {
        // 使用 workspace 目录下的 media 文件夹
        let image_dir = self.workspace_dir.join("media");

        fs::create_dir_all(&image_dir)
            .map_err(|e| format!("Failed to create image directory: {}", e))?;

        // 解析 base64 数据（移除 data:image/xxx;base64, 前缀）
        let base64_str = base64_data.split(',').next_back().unwrap_or(base64_data);

        let image_bytes = STANDARD
            .decode(base64_str)
            .map_err(|e| format!("Failed to decode base64: {}", e))?;

        let file_path = image_dir.join(filename);

        fs::write(&file_path, &image_bytes)
            .map_err(|e| format!("Failed to write image file: {}", e))?;

        Ok(file_path.to_string_lossy().to_string())
    }

    /// 从请求中提取用户消息内容和发送者标识
    ///
    /// 如果存在 node_req，使用 node_req 中的 message 和 recipient；
    /// 如果有图片，保存到本地并在 message 后追加 [IMAGE:<path>] 占位符。
    fn extract_user_content_and_sender(
        &self,
        body: &HttpChatRequest,
        session_id: &str,
    ) -> Result<(String, String), (axum::http::StatusCode, Json<serde_json::Value>)> {
        if let Some(node_req) = &body.node_req {
            let mut content = node_req.message.clone();

            // 处理图片
            if let Some(images) = &node_req.images {
                for (i, image) in images.iter().enumerate() {
                    match self.save_image_from_base64(&image.base64, &image.filename) {
                        Ok(path) => {
                            content.push_str(&format!(" [IMAGE:{}]", path));
                        }
                        Err(e) => {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(
                                    module_path!(),
                                    ::zeroclaw_log::Action::Write
                                )
                                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                .with_attrs(::serde_json::json!({
                                    "filename": image.filename.as_str(),
                                    "error": e,
                                })),
                                "Failed to save webchat image"
                            );
                            content.push_str(&format!(" [IMAGE:{}:failed_to_save]", i));
                        }
                    }
                }
            }

            Ok((content, node_req.recipient.clone()))
        } else {
            // 普通聊天模式
            if body.messages.is_empty() {
                return Err((
                    axum::http::StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": "messages must not be empty" })),
                ));
            }

            let Some(user_content) = Self::user_content_from_messages(&body.messages) else {
                return Err((
                    axum::http::StatusCode::BAD_REQUEST,
                    Json(
                        serde_json::json!({ "error": "last user message content must not be empty" }),
                    ),
                ));
            };
            Ok((user_content, session_id.to_string()))
        }
    }

    /// 构建 ChannelMessage 用于发送到代理通道
    /// 返回 (ChannelMessage, is_immediate)
    fn build_channel_message(
        body: &HttpChatRequest,
        sender: &str,
        session_id: &str,
        user_content: &str,
    ) -> (ChannelMessage, bool) {
        let channel_name = body
            .node_req
            .as_ref()
            .map(|n| n.channel_id.as_str())
            .unwrap_or("webchat");
        let reply_target = body
            .node_req
            .as_ref()
            .map(|n| n.recipient.as_str())
            .unwrap_or(session_id);
        let is_immediate = body
            .node_req
            .as_ref()
            .map(|n| !n.event.is_empty())
            .unwrap_or(false);
        let msg_id = if is_immediate {
            "[Immediately Message]".to_string()
        } else {
            format!("webchat_{}", Uuid::new_v4())
        };

        let msg = ChannelMessage {
            id: msg_id,
            sender: sender.to_string(),
            reply_target: reply_target.to_string(),
            content: user_content.to_string(),
            channel: channel_name.to_string(),
            channel_alias: None,
            subject: None,
            timestamp: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            thread_ts: None,
            interruption_scope_id: None,
            attachments: Vec::new(),
        };
        (msg, is_immediate)
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
            workspace_dir: self.workspace_dir.clone(),
        }
    }
}

impl Attributable for WebchatChannel {
    fn role(&self) -> Role {
        Role::Tool(ToolKind::Plugin)
    }
    fn alias(&self) -> &str {
        <Self as Channel>::name(self)
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
        let message_id = if message.subject.as_deref() == Some("[webchat message]") {
            "[webchat message]".to_string()
        } else {
            "".to_string()
        };
        self.finalize_draft(&message.recipient, &message_id, &message.content)
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
                SessionMode::Stream(tx) => Some((entry.sender.clone(), payload, tx.clone())),
                SessionMode::AwaitFinalize(_) => None,
            }
        };

        let Some((_, payload, tx)) = dispatch else {
            return Ok(());
        };

        let _ = tx.send(StreamFrame::Chunk(payload)).await;
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
                SessionMode::Stream(tx) => Some((entry.sender.clone(), payload, tx.clone())),
                SessionMode::AwaitFinalize(_) => None,
            }
        };

        let Some((_, payload, tx)) = dispatch else {
            return Ok(());
        };

        let _ = tx.send(StreamFrame::Chunk(payload)).await;
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
                SessionMode::Stream(tx) => Some((entry.sender.clone(), payload, tx.clone())),
                SessionMode::AwaitFinalize(_) => None,
            }
        };

        let Some((_, payload, tx)) = dispatch else {
            return Ok(());
        };

        let _ = tx.send(StreamFrame::Chunk(payload)).await;
        Ok(())
    }

    // 最终的结果非流式返回
    async fn finalize_draft(&self, recipient: &str, message_id: &str, text: &str) -> Result<()> {
        let entry = {
            let mut sessions = self.sessions.lock().await;
            sessions.remove(recipient)
        };
        let Some(entry) = entry else {
            return Ok(());
        };

        match entry.mode {
            SessionMode::Stream(tx) => {
                if (!text.is_empty() && !self.support_reasoning)
                    || message_id == "[webchat message]"
                {
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

            let session_id = body
                .session_id
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "agent_default_session".to_string());

            let (user_content, sender) = match state
                .channel
                .extract_user_content_and_sender(&body, &session_id)
            {
                Ok(v) => v,
                Err(e) => return e.into_response(),
            };

            let wants_stream = body.stream;
            let model_label = body
                .model
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("agent::main")
                .to_string();
            let completion_id = format!("chatcmpl-{}", Uuid::new_v4().simple());
            let created = chrono::Utc::now().timestamp();

            let (msg, is_immediate) =
                WebchatChannel::build_channel_message(&body, &sender, &session_id, &user_content);

            // 如果是立即响应场景，不注册 session，直接返回响应断开连接
            if is_immediate {
                if state.tx.send(msg).await.is_err() {
                    return (
                        axum::http::StatusCode::SERVICE_UNAVAILABLE,
                        Json(serde_json::json!({ "error": "agent channel closed" })),
                    )
                        .into_response();
                }
                return (
                    axum::http::StatusCode::OK,
                    Json(serde_json::json!({
                        "id": completion_id,
                        "object": "chat.completion",
                        "created": created,
                        "model": model_label,
                        "choices": [{
                            "index": 0,
                            "message": { "role": "assistant", "content": "事件已发送" },
                            "finish_reason": "stop",
                        }],
                    })),
                )
                    .into_response();
            }

            // 普通场景：注册 session 并等待响应
            let (mode, sse, wait_rx) = if wants_stream {
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

            if state.tx.send(msg).await.is_err() {
                return (
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    Json(serde_json::json!({ "error": "agent channel closed" })),
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

        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app).await.map_err(|e| {
            zeroclaw_log::record!(
                ERROR,
                zeroclaw_log::Event::new("webchat", zeroclaw_log::Action::Fail)
                    .with_category(zeroclaw_log::EventCategory::Channel)
                    .with_outcome(zeroclaw_log::EventOutcome::Failure)
                    .with_attrs(serde_json::json!({
                        "listen_port": self.listen_port,
                        "error": e.to_string(),
                    })),
                "Webchat server error",
            );
            Error::msg(format!("webchat server error: {e}"))
        })?;
        Ok(())
    }
}
