use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use regex::Regex;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
const DINGTALK_BOT_CALLBACK_TOPIC: &str = "/v1.0/im/bot/messages/get";
const DINGTALK_OUTBOUND_MAX_ATTEMPTS: u32 = 4;
const DINGTALK_OUTBOUND_RETRY_DELAY: Duration = Duration::from_millis(500);
const DINGTALK_STREAM_CONNECT_MAX_ATTEMPTS: u32 = 3;
const DINGTALK_STREAM_CONNECT_RETRY_DELAY: Duration = Duration::from_secs(1);
const DINGTALK_STREAM_RECONNECT_DELAY: Duration = Duration::from_secs(1);
const DINGTALK_STREAM_STALL_TIMEOUT: Duration = Duration::from_secs(120);
const DINGTALK_STREAM_STALL_CHECK_INTERVAL: Duration = Duration::from_secs(10);
const DINGTALK_USER_BATCH_SEND_URL: &str =
    "https://api.dingtalk.com/v1.0/robot/oToMessages/batchSend";
const DINGTALK_GROUP_SEND_URL: &str = "https://api.dingtalk.com/v1.0/robot/groupMessages/send";
type DingTalkWsStream = zeroclaw_config::schema::ProxiedWsStream;
type CleanupConfigResolver =
    Arc<dyn Fn() -> zeroclaw_infra::temp_file_manager::TempFileConfig + Send + Sync>;

macro_rules! dingtalk_info {
    ($message:expr) => {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            $message
        )
    };
    ($attrs:expr, $message:expr) => {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs($attrs),
            $message
        )
    };
}

macro_rules! dingtalk_warn {
    ($message:expr) => {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown),
            $message
        )
    };
    ($attrs:expr, $message:expr) => {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs($attrs),
            $message
        )
    };
}

macro_rules! dingtalk_debug {
    ($message:expr) => {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note),
            $message
        )
    };
    ($attrs:expr, $message:expr) => {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs($attrs),
            $message
        )
    };
}

/// DingTalk channel — connects via Stream Mode WebSocket for real-time messages.
/// Replies are sent through per-message session webhook URLs.
pub struct DingTalkChannel {
    client_id: String,
    client_secret: String,
    /// The alias key under `[channels.dingtalk.<alias>]` this handle is
    /// bound to. Used to scope peer-group writes and resolver lookups.
    alias: String,
    /// Resolves inbound external peers from canonical state at message-time.
    /// No cache (see AGENTS.md "ABSOLUTE RULE — SINGLE SOURCE OF TRUTH").
    peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    /// Per-chat session webhooks for sending replies (chatID -> webhook URL).
    /// DingTalk provides a unique webhook URL with each incoming message.
    session_webhooks: Arc<RwLock<HashMap<String, String>>>,
    /// Runtime reply route learned from inbound events so API fallback can
    /// preserve DM vs group delivery semantics after webhook failure.
    reply_targets: Arc<RwLock<HashMap<String, DingTalkReplyTarget>>>,
    /// Per-channel proxy URL override.
    proxy_url: Option<String>,
    /// Workspace directory for saving downloaded images.
    workspace_dir: Option<PathBuf>,
    /// Resolves cleanup config from canonical state at write-time.
    cleanup_config_resolver: Option<CleanupConfigResolver>,
    /// Upload cache: avoids re-uploading the same image within TTL.
    upload_cache: Arc<RwLock<HashMap<String, UploadCacheEntry>>>,
}

/// Cached upload entry to avoid re-uploading the same image.
struct UploadCacheEntry {
    media_id: String,
    photo_url: String,
    expires_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DingTalkReplyTarget {
    User(String),
    Group(String),
}

/// Response from DingTalk gateway connection registration.
#[derive(serde::Deserialize)]
struct GatewayResponse {
    endpoint: String,
    ticket: String,
}

#[derive(Debug, Deserialize)]
struct DingTalkWebhookResponse {
    errcode: Option<i64>,
    errmsg: Option<String>,
}

impl DingTalkChannel {
    pub fn new(
        client_id: String,
        client_secret: String,
        alias: impl Into<String>,
        peer_resolver: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    ) -> Self {
        Self {
            client_id,
            client_secret,
            alias: alias.into(),
            peer_resolver,
            session_webhooks: Arc::new(RwLock::new(HashMap::new())),
            reply_targets: Arc::new(RwLock::new(HashMap::new())),
            proxy_url: None,
            workspace_dir: None,
            cleanup_config_resolver: None,
            upload_cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Return the alias under `[channels.dingtalk.<alias>]` that this
    /// channel handle is bound to.
    pub fn alias(&self) -> &str {
        &self.alias
    }

    /// Set a per-channel proxy URL that overrides the global proxy config.
    pub fn with_proxy_url(mut self, proxy_url: Option<String>) -> Self {
        self.proxy_url = proxy_url;
        self
    }

    /// Resolve cleanup config from canonical state whenever a file is saved.
    pub fn with_cleanup_config_resolver(mut self, resolver: CleanupConfigResolver) -> Self {
        self.cleanup_config_resolver = Some(resolver);
        self
    }

    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_channel_proxy_client(
            "channel.dingtalk",
            self.proxy_url.as_deref(),
        )
    }

    fn robot_code(&self) -> &str {
        &self.client_id
    }

    fn format_api_text_content(text_content: &str, subject: Option<&str>) -> String {
        let text_content = text_content.trim();
        let subject = subject.map(str::trim).filter(|subject| !subject.is_empty());

        match (subject, text_content.is_empty()) {
            (Some(subject), false) => format!("【{subject}】\n{text_content}"),
            (Some(subject), true) => format!("【{subject}】"),
            (None, _) => text_content.to_string(),
        }
    }

    fn is_user_allowed(&self, user_id: &str) -> bool {
        let peers = (self.peer_resolver)();
        crate::allowlist::is_user_allowed(&peers, user_id, crate::allowlist::Match::Sensitive)
    }

    fn parse_stream_data(frame: &serde_json::Value) -> Option<serde_json::Value> {
        match frame.get("data") {
            Some(serde_json::Value::String(raw)) => serde_json::from_str(raw).ok(),
            Some(serde_json::Value::Object(_)) => frame.get("data").cloned(),
            _ => None,
        }
    }

    fn resolve_chat_id(data: &serde_json::Value, sender_id: &str) -> String {
        if Self::is_private_chat(data) {
            sender_id.to_string()
        } else {
            data.get("conversationId")
                .and_then(|c| c.as_str())
                .unwrap_or(sender_id)
                .to_string()
        }
    }

    fn is_private_chat(data: &serde_json::Value) -> bool {
        data.get("conversationType")
            .and_then(|value| {
                value
                    .as_str()
                    .map(|v| v == "1")
                    .or_else(|| value.as_i64().map(|v| v == 1))
            })
            .unwrap_or(true)
    }

    fn resolve_group_target(data: &serde_json::Value, fallback: &str) -> String {
        data.get("openConversationId")
            .and_then(|value| value.as_str())
            .filter(|value| !value.is_empty())
            .or_else(|| {
                data.get("conversationId")
                    .and_then(|value| value.as_str())
                    .filter(|value| !value.is_empty())
            })
            .unwrap_or(fallback)
            .to_string()
    }

    async fn store_reply_routes(
        &self,
        data: &serde_json::Value,
        sender_id: &str,
        chat_id: &str,
        session_webhook: Option<&str>,
    ) {
        let mut reply_targets = self.reply_targets.write().await;
        reply_targets.insert(
            sender_id.to_string(),
            DingTalkReplyTarget::User(sender_id.to_string()),
        );

        if Self::is_private_chat(data) {
            reply_targets.insert(
                chat_id.to_string(),
                DingTalkReplyTarget::User(sender_id.to_string()),
            );
        } else {
            reply_targets.insert(
                chat_id.to_string(),
                DingTalkReplyTarget::Group(Self::resolve_group_target(data, chat_id)),
            );
        }
        drop(reply_targets);

        if let Some(webhook) = session_webhook.filter(|webhook| !webhook.is_empty()) {
            let webhook = webhook.to_string();
            let mut webhooks = self.session_webhooks.write().await;
            webhooks.insert(chat_id.to_string(), webhook.clone());
            if Self::is_private_chat(data) {
                webhooks.insert(sender_id.to_string(), webhook);
            }
        }
    }

    async fn reply_target_for_recipient(&self, recipient: &str) -> Option<DingTalkReplyTarget> {
        let reply_targets = self.reply_targets.read().await;
        reply_targets.get(recipient).cloned()
    }

    async fn clear_session_webhook(&self, recipient: &str) {
        let mut webhooks = self.session_webhooks.write().await;
        webhooks.remove(recipient);
    }

    async fn send_text_via_reply_target(
        &self,
        reply_target: Option<&DingTalkReplyTarget>,
        recipient: &str,
        text_content: &str,
        subject: Option<&str>,
    ) -> anyhow::Result<()> {
        match reply_target {
            Some(DingTalkReplyTarget::User(user_id)) => {
                self.send_text_via_api(user_id, text_content, subject).await
            }
            Some(DingTalkReplyTarget::Group(group_id)) => {
                self.send_text_via_group_api(group_id, text_content, subject)
                    .await
            }
            None => {
                self.send_text_via_api(recipient, text_content, subject)
                    .await
            }
        }
    }

    async fn send_image_via_reply_target(
        &self,
        reply_target: Option<&DingTalkReplyTarget>,
        recipient: &str,
        image_path: &str,
    ) -> anyhow::Result<()> {
        match reply_target {
            Some(DingTalkReplyTarget::User(user_id)) => {
                self.send_image_via_api(user_id, image_path).await
            }
            Some(DingTalkReplyTarget::Group(group_id)) => {
                self.send_image_via_group_api(group_id, image_path).await
            }
            None => self.send_image_via_api(recipient, image_path).await,
        }
    }

    /// Register a connection with DingTalk's gateway to get a WebSocket endpoint.
    async fn register_connection(&self) -> anyhow::Result<GatewayResponse> {
        let body = serde_json::json!({
            "clientId": self.client_id,
            "clientSecret": self.client_secret,
            "subscriptions": [
                {
                    "type": "CALLBACK",
                    "topic": DINGTALK_BOT_CALLBACK_TOPIC,
                }
            ],
        });

        let resp = self
            .http_client()
            .post("https://api.dingtalk.com/v1.0/gateway/connections/open")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("gateway registration failed ({status}): {err}");
        }

        let gw: GatewayResponse = resp.json().await?;
        Ok(gw)
    }

    async fn open_stream_websocket_with_retry(&self) -> anyhow::Result<DingTalkWsStream> {
        let mut last_error = None;

        for attempt in 1..=DINGTALK_STREAM_CONNECT_MAX_ATTEMPTS {
            let connect_result = async {
                dingtalk_info!("DingTalk: registering gateway connection");
                let gw = self.register_connection().await?;
                let ws_url = format!("{}?ticket={}", gw.endpoint, gw.ticket);

                dingtalk_info!("DingTalk: connecting to stream WebSocket");
                let (ws_stream, _) = zeroclaw_config::schema::ws_connect_with_proxy(
                    &ws_url,
                    "channel.dingtalk",
                    self.proxy_url.as_deref(),
                )
                .await?;

                Ok::<_, anyhow::Error>(ws_stream)
            }
            .await;

            match connect_result {
                Ok(ws_stream) => {
                    if attempt > 1 {
                        dingtalk_info!(
                            ::serde_json::json!({
                                "attempt": attempt,
                                "max_attempts": DINGTALK_STREAM_CONNECT_MAX_ATTEMPTS,
                            }),
                            "DingTalk: stream connection recovered after retry"
                        );
                    }
                    return Ok(ws_stream);
                }
                Err(error) => {
                    if attempt >= DINGTALK_STREAM_CONNECT_MAX_ATTEMPTS {
                        return Err(error);
                    }

                    dingtalk_warn!(
                        ::serde_json::json!({
                            "attempt": attempt,
                            "max_attempts": DINGTALK_STREAM_CONNECT_MAX_ATTEMPTS,
                            "retry_delay_ms": DINGTALK_STREAM_CONNECT_RETRY_DELAY.as_millis() as u64,
                            "error": error.to_string(),
                        }),
                        "DingTalk: stream connection failed, retrying"
                    );
                    last_error = Some(error);
                    tokio::time::sleep(DINGTALK_STREAM_CONNECT_RETRY_DELAY).await;
                }
            }
        }

        Err(last_error
            .unwrap_or_else(|| anyhow::Error::msg("DingTalk: stream connection retry exhausted")))
    }

    async fn run_stream_session(
        &self,
        ws_stream: DingTalkWsStream,
        tx: &tokio::sync::mpsc::Sender<ChannelMessage>,
    ) -> anyhow::Result<()> {
        let (mut write, mut read) = ws_stream.split();
        let mut last_recv = Instant::now();
        let mut stall_check = tokio::time::interval(DINGTALK_STREAM_STALL_CHECK_INTERVAL);
        stall_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        stall_check.tick().await;

        dingtalk_info!("DingTalk: connected and listening for messages");

        loop {
            tokio::select! {
                _ = stall_check.tick() => {
                    if last_recv.elapsed() > DINGTALK_STREAM_STALL_TIMEOUT {
                        anyhow::bail!(
                            "DingTalk WebSocket stalled for {}s",
                            last_recv.elapsed().as_secs()
                        );
                    }
                }
                maybe_msg = read.next() => {
                    let msg = match maybe_msg {
                        Some(Ok(msg)) => {
                            last_recv = Instant::now();
                            msg
                        }
                        Some(Err(error)) => anyhow::bail!("DingTalk WebSocket error: {error}"),
                        None => anyhow::bail!("DingTalk WebSocket stream ended"),
                    };

                    let text = match msg {
                        Message::Text(text) => text,
                        Message::Ping(payload) => {
                            if let Err(error) = write.send(Message::Pong(payload)).await {
                                anyhow::bail!("DingTalk: failed to send websocket pong: {error}");
                            }
                            continue;
                        }
                        Message::Pong(_) => continue,
                        Message::Close(frame) => {
                            dingtalk_warn!(
                                ::serde_json::json!({
                                    "close_frame": format!("{frame:?}"),
                                }),
                                "DingTalk WebSocket closed by remote"
                            );
                            anyhow::bail!("DingTalk WebSocket closed by remote");
                        }
                        _ => continue,
                    };

                    let frame: serde_json::Value = match serde_json::from_str(text.as_ref()) {
                        Ok(value) => value,
                        Err(_) => continue,
                    };

                    let frame_type = frame.get("type").and_then(|t| t.as_str()).unwrap_or("");

                    match frame_type {
                        "SYSTEM" => {
                            let message_id = frame
                                .get("headers")
                                .and_then(|h| h.get("messageId"))
                                .and_then(|m| m.as_str())
                                .unwrap_or("");

                            let pong = serde_json::json!({
                                "code": 200,
                                "headers": {
                                    "contentType": "application/json",
                                    "messageId": message_id,
                                },
                                "message": "OK",
                                "data": "",
                            });

                            if let Err(error) = write.send(Message::Text(pong.to_string().into())).await {
                                anyhow::bail!("DingTalk: failed to send system pong: {error}");
                            }
                        }
                        "EVENT" | "CALLBACK" => {
                            let data = match Self::parse_stream_data(&frame) {
                                Some(value) => value,
                                None => {
                                    dingtalk_debug!("DingTalk: frame has no parseable data payload");
                                    continue;
                                }
                            };

                            let sender_id = data
                                .get("senderStaffId")
                                .and_then(|s| s.as_str())
                                .unwrap_or("unknown");

                            if !self.is_user_allowed(sender_id) {
                                dingtalk_warn!(
                                    ::serde_json::json!({
                                        "sender_id": sender_id,
                                    }),
                                    "DingTalk: ignoring message from unauthorized user"
                                );
                                continue;
                            }

                            let chat_id = Self::resolve_chat_id(&data, sender_id);
                            let session_webhook = data.get("sessionWebhook").and_then(|w| w.as_str());
                            self.store_reply_routes(&data, sender_id, &chat_id, session_webhook)
                                .await;

                            let message_id = frame
                                .get("headers")
                                .and_then(|h| h.get("messageId"))
                                .and_then(|m| m.as_str())
                                .unwrap_or("");

                            let ack = serde_json::json!({
                                "code": 200,
                                "headers": {
                                    "contentType": "application/json",
                                    "messageId": message_id,
                                },
                                "message": "OK",
                                "data": "",
                            });
                            if let Err(error) = write.send(Message::Text(ack.to_string().into())).await {
                                anyhow::bail!("DingTalk: failed to send event ack: {error}");
                            }

                            let channel_msg = match self
                                .build_incoming_channel_message(&data, sender_id, chat_id)
                                .await
                            {
                                Some(channel_msg) => channel_msg,
                                None => continue,
                            };

                            if tx.send(channel_msg).await.is_err() {
                                dingtalk_warn!("DingTalk: message channel closed");
                                return Ok(());
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
}

impl ::zeroclaw_api::attribution::Attributable for DingTalkChannel {
    fn role(&self) -> ::zeroclaw_api::attribution::Role {
        ::zeroclaw_api::attribution::Role::Channel(
            ::zeroclaw_api::attribution::ChannelKind::DingTalk,
        )
    }
    fn alias(&self) -> &str {
        &self.alias
    }
}

#[async_trait]
impl Channel for DingTalkChannel {
    fn name(&self) -> &str {
        "dingtalk"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        // Parse [IMAGE:...] markers from content
        let (text_content, image_paths) = Self::parse_image_markers(&message.content);
        let reply_target = self.reply_target_for_recipient(&message.recipient).await;
        let mut webhook_url = {
            let webhooks = self.session_webhooks.read().await;
            webhooks.get(&message.recipient).cloned()
        };

        for image_path in &image_paths {
            if let Some(current_webhook) = webhook_url.as_deref() {
                if let Err(error) = self
                    .send_image_attachment(current_webhook, image_path)
                    .await
                {
                    dingtalk_warn!(
                        ::serde_json::json!({
                            "image_path": image_path,
                            "error": error.to_string(),
                        }),
                        "DingTalk: webhook image reply failed, falling back to proactive API"
                    );
                    self.clear_session_webhook(&message.recipient).await;
                    webhook_url = None;
                    if let Err(error) = self
                        .send_image_via_reply_target(
                            reply_target.as_ref(),
                            &message.recipient,
                            image_path,
                        )
                        .await
                    {
                        dingtalk_warn!(
                            ::serde_json::json!({
                                "image_path": image_path,
                                "error": error.to_string(),
                            }),
                            "DingTalk: failed to send image"
                        );
                    }
                }
            } else if let Err(error) = self
                .send_image_via_reply_target(reply_target.as_ref(), &message.recipient, image_path)
                .await
            {
                dingtalk_warn!(
                    ::serde_json::json!({
                        "image_path": image_path,
                        "error": error.to_string(),
                    }),
                    "DingTalk: failed to send image"
                );
            }
        }

        if !text_content.trim().is_empty() {
            if let Some(current_webhook) = webhook_url.as_deref() {
                if let Err(error) = self
                    .send_markdown_via_webhook(
                        current_webhook,
                        &text_content,
                        message.subject.as_deref(),
                    )
                    .await
                {
                    dingtalk_warn!(
                        ::serde_json::json!({
                            "error": error.to_string(),
                        }),
                        "DingTalk: webhook text reply failed, falling back to proactive API"
                    );
                    self.clear_session_webhook(&message.recipient).await;
                    self.send_text_via_reply_target(
                        reply_target.as_ref(),
                        &message.recipient,
                        &text_content,
                        message.subject.as_deref(),
                    )
                    .await?;
                }
            } else {
                self.send_text_via_reply_target(
                    reply_target.as_ref(),
                    &message.recipient,
                    &text_content,
                    message.subject.as_deref(),
                )
                .await?;
            }
        }

        if image_paths.is_empty() && text_content.trim().is_empty() {
            anyhow::bail!(
                "DingTalk: message for recipient {} has no text or image content to send",
                message.recipient
            );
        }

        Ok(())
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        loop {
            let ws_stream = self.open_stream_websocket_with_retry().await?;

            match self.run_stream_session(ws_stream, &tx).await {
                Ok(()) => return Ok(()),
                Err(_error) if tx.is_closed() => return Ok(()),
                Err(error) => {
                    dingtalk_warn!(
                        ::serde_json::json!({
                            "retry_delay_ms": DINGTALK_STREAM_RECONNECT_DELAY.as_millis() as u64,
                            "error": error.to_string(),
                        }),
                        "DingTalk: stream session ended, reconnecting"
                    );
                    tokio::time::sleep(DINGTALK_STREAM_RECONNECT_DELAY).await;
                }
            }
        }
    }

    async fn health_check(&self) -> bool {
        self.register_connection().await.is_ok()
    }
}

// ============================================================================
// Image Upload/Download Helper Functions
// ============================================================================

impl DingTalkChannel {
    /// Maximum image upload size (10 MB).
    const IMAGE_MAX_BYTES: usize = 10 * 1024 * 1024;

    /// Upload cache TTL (1 hour).
    const UPLOAD_CACHE_TTL: u64 = 3600;

    /// Configure workspace directory for saving downloaded/uploaded images.
    pub fn with_workspace_dir(mut self, dir: PathBuf) -> Self {
        self.workspace_dir = Some(dir);
        self
    }

    fn existing_local_image_path(image_path: &str) -> Option<&Path> {
        let path = Path::new(image_path);
        if !path.exists() {
            dingtalk_warn!(
                ::serde_json::json!({
                    "image_path": image_path,
                }),
                "DingTalk: image file not found"
            );
            return None;
        }

        Some(path)
    }

    fn build_text_msg_param(
        text_content: &str,
        subject: Option<&str>,
    ) -> anyhow::Result<Option<String>> {
        let content = Self::format_api_text_content(text_content, subject);
        if content.is_empty() {
            return Ok(None);
        }

        Ok(Some(serde_json::to_string(&serde_json::json!({
            "content": content,
        }))?))
    }

    async fn build_image_msg_param(&self, image_path: &str) -> anyhow::Result<Option<String>> {
        let Some(path) = Self::existing_local_image_path(image_path) else {
            return Ok(None);
        };

        let (_media_id, photo_url, _token) = self.upload_image(path).await?;
        Ok(Some(serde_json::to_string(&serde_json::json!({
            "photoURL": photo_url,
        }))?))
    }

    /// Download image using downloadCode API (with retry).
    async fn download_image_by_code(&self, download_code: &str, file_name: &str) -> Option<String> {
        const MAX_RETRIES: u32 = 2;
        const RETRY_DELAY: Duration = Duration::from_millis(500);

        // Get access token
        let token = match self.get_access_token().await {
            Ok(t) => t,
            Err(e) => {
                dingtalk_warn!(
                    ::serde_json::json!({
                        "error": e.to_string(),
                    }),
                    "DingTalk: failed to get access token"
                );
                return None;
            }
        };

        // Build request body
        let body = serde_json::json!({
            "downloadCode": download_code,
            "robotCode": self.robot_code()
        });

        // Send POST request with retry
        let url = "https://api.dingtalk.com/v1.0/robot/messageFiles/download";

        for attempt in 0..=MAX_RETRIES {
            let resp = match self
                .http_client()
                .post(url)
                .header("x-acs-dingtalk-access-token", &token)
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    if attempt < MAX_RETRIES {
                        tokio::time::sleep(RETRY_DELAY).await;
                        continue;
                    }
                    dingtalk_warn!(
                        ::serde_json::json!({
                            "error": e.to_string(),
                        }),
                        "DingTalk: download request failed"
                    );
                    return None;
                }
            };

            if !resp.status().is_success() {
                if attempt < MAX_RETRIES {
                    tokio::time::sleep(RETRY_DELAY).await;
                    continue;
                }
                dingtalk_warn!(
                    ::serde_json::json!({
                        "status": resp.status().to_string(),
                    }),
                    "DingTalk: download failed with status"
                );
                return None;
            }

            // Parse response
            let result: serde_json::Value = match resp.json().await {
                Ok(r) => r,
                Err(e) => {
                    if attempt < MAX_RETRIES {
                        tokio::time::sleep(RETRY_DELAY).await;
                        continue;
                    }
                    dingtalk_warn!(
                        ::serde_json::json!({
                            "error": e.to_string(),
                        }),
                        "DingTalk: response parse failed"
                    );
                    return None;
                }
            };

            let download_url = match result.get("downloadUrl").and_then(|v| v.as_str()) {
                Some(u) => u,
                None => {
                    dingtalk_warn!("DingTalk: no downloadUrl in response");
                    return None;
                }
            };

            // Download from URL
            let download_resp = match self.http_client().get(download_url).send().await {
                Ok(r) => r,
                Err(e) => {
                    if attempt < MAX_RETRIES {
                        tokio::time::sleep(RETRY_DELAY).await;
                        continue;
                    }
                    dingtalk_warn!(
                        ::serde_json::json!({
                            "error": e.to_string(),
                        }),
                        "DingTalk: image download failed"
                    );
                    return None;
                }
            };

            if !download_resp.status().is_success() {
                if attempt < MAX_RETRIES {
                    tokio::time::sleep(RETRY_DELAY).await;
                    continue;
                }
                dingtalk_warn!(
                    ::serde_json::json!({
                        "status": download_resp.status().to_string(),
                    }),
                    "DingTalk: image download failed with status"
                );
                return None;
            }

            // Process response
            return self
                .process_image_download_response(download_resp, file_name, download_code)
                .await;
        }

        dingtalk_warn!(
            ::serde_json::json!({
                "max_retries": MAX_RETRIES,
            }),
            "DingTalk: download failed after retries"
        );
        None
    }

    /// Process image download response and save to workspace.
    async fn process_image_download_response(
        &self,
        resp: reqwest::Response,
        file_name: &str,
        _file_key: &str,
    ) -> Option<String> {
        // Read content-type header
        let content_type = resp
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        // Read bytes
        let bytes = match resp.bytes().await {
            Ok(b) => b,
            Err(e) => {
                dingtalk_warn!(
                    ::serde_json::json!({
                        "error": e.to_string(),
                    }),
                    "DingTalk: failed to read image bytes"
                );
                return None;
            }
        };

        // Validate size
        if bytes.is_empty() {
            dingtalk_warn!("DingTalk: downloaded image is empty");
            return None;
        }
        if bytes.len() > Self::IMAGE_MAX_BYTES {
            dingtalk_warn!(
                ::serde_json::json!({
                    "size_bytes": bytes.len(),
                }),
                "DingTalk: downloaded image too large"
            );
            return None;
        }

        // Detect MIME type
        let _mime = dingtalk_detect_image_mime(content_type.as_deref(), &bytes);

        // Save to workspace
        if let Some(ref workspace) = self.workspace_dir {
            let dir = workspace.join("dingtalk_files");

            if tokio::fs::create_dir_all(&dir).await.is_ok() {
                // Generate unique filename
                let stem = std::path::Path::new(file_name)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("image");
                let ext = std::path::Path::new(file_name)
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("jpg");
                let unique = &uuid::Uuid::new_v4().to_string()[..8];
                let safe_filename = format!("{stem}_{unique}.{ext}");
                let path = dir.join(&safe_filename);

                if tokio::fs::write(&path, &bytes).await.is_ok() {
                    dingtalk_info!(
                        ::serde_json::json!({
                            "path": path.display().to_string(),
                        }),
                        "DingTalk: image saved"
                    );
                    if let Some(resolve_cleanup_config) = self.cleanup_config_resolver.as_ref() {
                        let cleanup_config = resolve_cleanup_config();
                        if let Err(error) = zeroclaw_infra::temp_file_manager::TempFileManager::trigger_cleanup_by_path(
                            workspace,
                            &path,
                            &cleanup_config,
                        ) {
                            dingtalk_warn!(
                                ::serde_json::json!({
                                    "path": path.display().to_string(),
                                    "error": error.to_string(),
                                }),
                                "DingTalk: cleanup trigger failed after saving image"
                            );
                        }
                    }
                    return Some(format!("[IMAGE:{}]", path.display()));
                }
            }
        }

        None
    }

    /// Parse [IMAGE:/path] markers from content, return (text, image_paths).
    fn parse_image_markers(content: &str) -> (String, Vec<String>) {
        let mut text = String::new();
        let mut image_paths = Vec::new();
        let mut last_end = 0;

        let re = Regex::new(r"\[IMAGE:([^\]]+)\]").unwrap();

        for cap in re.captures_iter(content) {
            let full_match = cap.get(0).unwrap();
            let path = cap.get(1).unwrap().as_str();

            text.push_str(&content[last_end..full_match.start()]);

            // Only collect local file paths (not data: URLs or placeholders)
            if path.starts_with('/') {
                image_paths.push(path.to_string());
            }

            last_end = full_match.end();
        }

        text.push_str(&content[last_end..]);
        (text, image_paths)
    }

    /// Get access token for DingTalk API calls.
    async fn get_access_token(&self) -> anyhow::Result<String> {
        let mut last_error = None;

        for attempt in 1..=DINGTALK_OUTBOUND_MAX_ATTEMPTS {
            match self.request_access_token_once().await {
                Ok(token) => {
                    if attempt > 1 {
                        dingtalk_info!(
                            ::serde_json::json!({
                                "attempt": attempt,
                            }),
                            "DingTalk: access token request recovered after retry"
                        );
                    }
                    return Ok(token);
                }
                Err(error) => {
                    if attempt >= DINGTALK_OUTBOUND_MAX_ATTEMPTS {
                        return Err(error);
                    }

                    dingtalk_warn!(
                        ::serde_json::json!({
                            "attempt": attempt,
                            "max_attempts": DINGTALK_OUTBOUND_MAX_ATTEMPTS,
                            "error": error.to_string(),
                        }),
                        "DingTalk: access token request failed, retrying"
                    );
                    last_error = Some(error);
                    tokio::time::sleep(DINGTALK_OUTBOUND_RETRY_DELAY).await;
                }
            }
        }

        Err(last_error
            .unwrap_or_else(|| anyhow::Error::msg("DingTalk: access token retry exhausted")))
    }

    async fn request_access_token_once(&self) -> anyhow::Result<String> {
        let body = serde_json::json!({
            "appKey": self.client_id,
            "appSecret": self.client_secret,
        });

        let resp = self
            .http_client()
            .post("https://api.dingtalk.com/v1.0/oauth2/accessToken")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("DingTalk token request failed ({status}): {err}");
        }

        let token_resp: serde_json::Value = resp.json().await?;

        if let Some(access_token) = token_resp.get("accessToken").and_then(|v| v.as_str()) {
            return Ok(access_token.to_string());
        }

        let code = token_resp
            .get("code")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let message = token_resp
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("missing accessToken");
        anyhow::bail!("DingTalk token failed: code={code}, message={message}")
    }

    async fn send_proactive_request(
        &self,
        endpoint: &'static str,
        target_kind: &'static str,
        target_id: &str,
        body: &serde_json::Value,
    ) -> anyhow::Result<()> {
        let mut last_error = None;

        for attempt in 1..=DINGTALK_OUTBOUND_MAX_ATTEMPTS {
            let token = self.get_access_token().await?;
            let request = self
                .http_client()
                .post(endpoint)
                .header("x-acs-dingtalk-access-token", token)
                .header("Content-Type", "application/json")
                .json(body)
                .send()
                .await;

            match request {
                Ok(resp) => {
                    if !resp.status().is_success() {
                        let status = resp.status();
                        let err = resp.text().await.unwrap_or_default();
                        let error = anyhow::Error::msg(format!(
                            "DingTalk proactive {target_kind} send failed ({status}): {err}"
                        ));
                        if attempt >= DINGTALK_OUTBOUND_MAX_ATTEMPTS {
                            return Err(error);
                        }
                        dingtalk_warn!(
                            ::serde_json::json!({
                                "attempt": attempt,
                                "max_attempts": DINGTALK_OUTBOUND_MAX_ATTEMPTS,
                                "target_kind": target_kind,
                                "target_id": target_id,
                                "error": error.to_string(),
                            }),
                            "DingTalk: proactive send failed, retrying"
                        );
                        last_error = Some(error);
                        tokio::time::sleep(DINGTALK_OUTBOUND_RETRY_DELAY).await;
                        continue;
                    }

                    return Ok(());
                }
                Err(error) => {
                    let error: anyhow::Error = error.into();
                    if attempt >= DINGTALK_OUTBOUND_MAX_ATTEMPTS {
                        return Err(error);
                    }
                    dingtalk_warn!(
                        ::serde_json::json!({
                            "attempt": attempt,
                            "max_attempts": DINGTALK_OUTBOUND_MAX_ATTEMPTS,
                            "target_kind": target_kind,
                            "target_id": target_id,
                            "endpoint": endpoint,
                            "error": error.to_string(),
                        }),
                        "DingTalk: proactive send request failed, retrying"
                    );
                    last_error = Some(error);
                    tokio::time::sleep(DINGTALK_OUTBOUND_RETRY_DELAY).await;
                }
            }
        }

        Err(last_error
            .unwrap_or_else(|| anyhow::Error::msg("DingTalk: proactive send retry exhausted")))
    }

    async fn send_webhook_markdown(
        &self,
        webhook_url: &str,
        body: &serde_json::Value,
        payload_kind: &'static str,
    ) -> anyhow::Result<()> {
        let mut last_error = None;

        for attempt in 1..=DINGTALK_OUTBOUND_MAX_ATTEMPTS {
            match self
                .send_webhook_markdown_once(webhook_url, body, payload_kind)
                .await
            {
                Ok(()) => return Ok(()),
                Err(error) => {
                    if attempt >= DINGTALK_OUTBOUND_MAX_ATTEMPTS {
                        return Err(error);
                    }
                    dingtalk_warn!(
                        ::serde_json::json!({
                            "attempt": attempt,
                            "max_attempts": DINGTALK_OUTBOUND_MAX_ATTEMPTS,
                            "payload_kind": payload_kind,
                            "webhook_url": webhook_url,
                            "error": error.to_string(),
                        }),
                        "DingTalk: webhook reply failed, retrying"
                    );
                    last_error = Some(error);
                    tokio::time::sleep(DINGTALK_OUTBOUND_RETRY_DELAY).await;
                }
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow::Error::msg("DingTalk: webhook retry exhausted")))
    }

    async fn send_webhook_markdown_once(
        &self,
        webhook_url: &str,
        body: &serde_json::Value,
        payload_kind: &'static str,
    ) -> anyhow::Result<()> {
        let resp = self
            .http_client()
            .post(webhook_url)
            .json(body)
            .send()
            .await?;

        let status = resp.status();
        let response_body = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!(
                "DingTalk webhook {payload_kind} reply failed ({status}): {response_body}"
            );
        }

        Self::ensure_webhook_response_ok(payload_kind, &response_body)
    }

    fn ensure_webhook_response_ok(
        payload_kind: &'static str,
        response_body: &str,
    ) -> anyhow::Result<()> {
        if response_body.trim().is_empty() {
            return Ok(());
        }

        let response = match serde_json::from_str::<DingTalkWebhookResponse>(response_body) {
            Ok(response) => response,
            Err(_) => return Ok(()),
        };

        match response.errcode.unwrap_or(0) {
            0 => Ok(()),
            code => {
                let message = response.errmsg.unwrap_or_else(|| "unknown".to_string());
                anyhow::bail!(
                    "DingTalk webhook {payload_kind} reply failed: errcode={code}, errmsg={message}"
                )
            }
        }
    }

    /// Upload image to DingTalk and return (media_id, photo_url, access_token).
    async fn upload_image(&self, file_path: &Path) -> anyhow::Result<(String, String, String)> {
        const MAX_RETRIES: u32 = 2;
        const RETRY_DELAY: Duration = Duration::from_millis(500);

        let file_path_str = file_path.display().to_string();
        let file_name = file_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        // Check upload cache
        {
            let cache = self.upload_cache.read().await;
            if let Some(entry) = cache.get(&file_path_str) {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                if now < entry.expires_at {
                    return Ok((
                        entry.media_id.clone(),
                        entry.photo_url.clone(),
                        String::new(),
                    ));
                }
            }
        }

        // Read file bytes
        let file_bytes = match tokio::fs::read(file_path).await {
            Ok(b) => b,
            Err(e) => anyhow::bail!("DingTalk: image file read failed: {}", e),
        };
        if file_bytes.is_empty() {
            anyhow::bail!("DingTalk: image file is empty: {}", file_path.display());
        }
        if file_bytes.len() > Self::IMAGE_MAX_BYTES {
            anyhow::bail!(
                "DingTalk: image file too large: {} bytes exceeds {} bytes limit",
                file_bytes.len(),
                Self::IMAGE_MAX_BYTES
            );
        }

        // Detect MIME type from extension
        let mime = match file_path.extension().and_then(|e| e.to_str()) {
            Some("png") => "image/png",
            Some("gif") => "image/gif",
            Some("webp") => "image/webp",
            Some("bmp") => "image/bmp",
            _ => "image/jpeg",
        };

        // Upload with retry
        let mut last_error = None;
        for attempt in 0..=MAX_RETRIES {
            // Get access token
            let token = match self.get_access_token().await {
                Ok(t) => t,
                Err(e) => {
                    last_error = Some(e);
                    if attempt < MAX_RETRIES {
                        tokio::time::sleep(RETRY_DELAY).await;
                        continue;
                    }
                    return Err(last_error.unwrap());
                }
            };

            // Build multipart form
            let form = reqwest::multipart::Form::new().part(
                "media",
                reqwest::multipart::Part::bytes(file_bytes.clone())
                    .file_name(file_name.clone())
                    .mime_str(mime)?,
            );

            // Upload to DingTalk API
            let url = format!(
                "https://oapi.dingtalk.com/media/upload?access_token={}&type=image",
                token
            );
            let resp = match self.http_client().post(&url).multipart(form).send().await {
                Ok(r) => r,
                Err(e) => {
                    last_error = Some(anyhow::Error::msg(format!("Request failed: {e}")));
                    if attempt < MAX_RETRIES {
                        tokio::time::sleep(RETRY_DELAY).await;
                        continue;
                    }
                    return Err(last_error.unwrap());
                }
            };

            if !resp.status().is_success() {
                let status = resp.status();
                let err = resp.text().await.unwrap_or_default();
                last_error = Some(anyhow::Error::msg(format!(
                    "Upload failed ({status}): {err}"
                )));
                if attempt < MAX_RETRIES {
                    tokio::time::sleep(RETRY_DELAY).await;
                    continue;
                }
                return Err(last_error.unwrap());
            }

            // Parse response
            #[derive(Deserialize)]
            struct UploadResp {
                errcode: i32,
                errmsg: String,
                media_id: Option<String>,
            }

            let upload_resp: UploadResp = match resp.json().await {
                Ok(r) => r,
                Err(e) => {
                    last_error = Some(anyhow::Error::msg(format!("Parse failed: {e}")));
                    if attempt < MAX_RETRIES {
                        tokio::time::sleep(RETRY_DELAY).await;
                        continue;
                    }
                    return Err(last_error.unwrap());
                }
            };

            if upload_resp.errcode != 0 {
                last_error = Some(anyhow::Error::msg(format!(
                    "Upload failed: errcode={}, errmsg={}",
                    upload_resp.errcode, upload_resp.errmsg
                )));
                if attempt < MAX_RETRIES {
                    tokio::time::sleep(RETRY_DELAY).await;
                    continue;
                }
                return Err(last_error.unwrap());
            }

            let media_id = upload_resp
                .media_id
                .ok_or_else(|| anyhow::Error::msg("DingTalk: no media_id in upload response"))?;

            // Build photo URL
            let photo_url = format!(
                "https://oapi.dingtalk.com/media/downloadFile?access_token={}&media_id={}",
                token, media_id
            );

            // Cache the result
            {
                let mut cache = self.upload_cache.write().await;
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                cache.insert(
                    file_path_str.clone(),
                    UploadCacheEntry {
                        media_id: media_id.clone(),
                        photo_url: photo_url.clone(),
                        expires_at: now + Self::UPLOAD_CACHE_TTL,
                    },
                );
            }

            dingtalk_info!(
                ::serde_json::json!({
                    "media_id": media_id,
                }),
                "DingTalk: image uploaded successfully"
            );
            return Ok((media_id, photo_url, token));
        }

        Err(last_error
            .unwrap_or_else(|| anyhow::Error::msg("DingTalk: upload failed after retries")))
    }

    /// Send image via DingTalk webhook using markdown with image URL.
    async fn send_image_via_webhook(
        &self,
        webhook_url: &str,
        _media_id: &str,
        photo_url: &str,
    ) -> anyhow::Result<()> {
        // Send as markdown with image URL
        let body = serde_json::json!({
            "msgtype": "markdown",
            "markdown": {
                "title": "ZeroClaw",
                "text": format!("![image]({})", photo_url)
            }
        });

        self.send_webhook_markdown(webhook_url, &body, "image")
            .await
    }

    /// Send a single image attachment: upload and send via webhook.
    async fn send_image_attachment(
        &self,
        webhook_url: &str,
        image_path: &str,
    ) -> anyhow::Result<()> {
        let Some(path) = Self::existing_local_image_path(image_path) else {
            return Ok(());
        };

        // Upload image to DingTalk
        let (media_id, photo_url, _token) = self.upload_image(path).await?;

        // Send image via webhook
        self.send_image_via_webhook(webhook_url, &media_id, &photo_url)
            .await
    }

    /// Send text via DingTalk's proactive robot API.
    async fn send_text_via_api(
        &self,
        recipient: &str,
        text_content: &str,
        subject: Option<&str>,
    ) -> anyhow::Result<()> {
        let Some(msg_param) = Self::build_text_msg_param(text_content, subject)? else {
            anyhow::bail!(
                "DingTalk: recipient {} has no session webhook and proactive API send only supports text content",
                recipient
            );
        };
        let body = serde_json::json!({
            "robotCode": self.robot_code(),
            "userIds": [recipient],
            "msgKey": "sampleText",
            "msgParam": msg_param,
        });

        self.send_proactive_request(DINGTALK_USER_BATCH_SEND_URL, "text", recipient, &body)
            .await
    }

    async fn send_text_via_group_api(
        &self,
        group_id: &str,
        text_content: &str,
        subject: Option<&str>,
    ) -> anyhow::Result<()> {
        let Some(msg_param) = Self::build_text_msg_param(text_content, subject)? else {
            return Ok(());
        };
        let body = serde_json::json!({
            "robotCode": self.robot_code(),
            "openConversationId": group_id,
            "conversationId": group_id,
            "msgKey": "sampleText",
            "msgParam": msg_param,
        });

        self.send_proactive_request(DINGTALK_GROUP_SEND_URL, "group_text", group_id, &body)
            .await
    }

    /// Send a single image via DingTalk's proactive robot API.
    async fn send_image_via_api(&self, recipient: &str, image_path: &str) -> anyhow::Result<()> {
        let Some(msg_param) = self.build_image_msg_param(image_path).await? else {
            return Ok(());
        };
        let body = serde_json::json!({
            "robotCode": self.robot_code(),
            "userIds": [recipient],
            "msgKey": "sampleImageMsg",
            "msgParam": msg_param,
        });

        self.send_proactive_request(DINGTALK_USER_BATCH_SEND_URL, "image", recipient, &body)
            .await
    }

    async fn send_image_via_group_api(
        &self,
        group_id: &str,
        image_path: &str,
    ) -> anyhow::Result<()> {
        let Some(msg_param) = self.build_image_msg_param(image_path).await? else {
            return Ok(());
        };
        let body = serde_json::json!({
            "robotCode": self.robot_code(),
            "openConversationId": group_id,
            "conversationId": group_id,
            "msgKey": "sampleImageMsg",
            "msgParam": msg_param,
        });

        self.send_proactive_request(DINGTALK_GROUP_SEND_URL, "group_image", group_id, &body)
            .await
    }

    /// Send text message via DingTalk webhook.
    #[allow(dead_code)]
    async fn send_text_message(
        &self,
        recipient: &str,
        text_content: &str,
        subject: Option<&str>,
    ) -> anyhow::Result<()> {
        let webhook_url = {
            let webhooks = self.session_webhooks.read().await;
            webhooks.get(recipient).cloned()
        }
        .ok_or_else(|| {
            anyhow::Error::msg(format!(
                "No session webhook found for chat {}. \
                 The user must send a message first to establish a session.",
                recipient
            ))
        })?;

        self.send_markdown_via_webhook(&webhook_url, text_content, subject)
            .await
    }

    async fn send_markdown_via_webhook(
        &self,
        webhook_url: &str,
        text_content: &str,
        subject: Option<&str>,
    ) -> anyhow::Result<()> {
        let title = subject.unwrap_or("ZeroClaw");
        let body = serde_json::json!({
            "msgtype": "markdown",
            "markdown": {
                "title": title,
                "text": text_content,
            }
        });

        self.send_webhook_markdown(webhook_url, &body, "text").await
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Detect image MIME type from Content-Type header or magic bytes.
#[allow(dead_code)]
fn dingtalk_detect_image_mime(content_type: Option<&str>, bytes: &[u8]) -> String {
    // First try Content-Type header
    if let Some(ct) = content_type
        && ct.starts_with("image/")
    {
        return ct.split(';').next().unwrap_or(ct).trim().to_string();
    }

    // Fallback to magic bytes detection
    if bytes.len() >= 4 {
        // PNG: 89 50 4E 47
        if bytes[0] == 0x89 && bytes[1] == 0x50 && bytes[2] == 0x4E && bytes[3] == 0x47 {
            return "image/png".to_string();
        }
        // GIF: 47 49 46 38
        if bytes[0] == 0x47 && bytes[1] == 0x49 && bytes[2] == 0x46 && bytes[3] == 0x38 {
            return "image/gif".to_string();
        }
        // WEBP: 52 49 46 46 ... 57 45 42 50
        if bytes.len() >= 12
            && bytes[0] == 0x52
            && bytes[1] == 0x49
            && bytes[2] == 0x46
            && bytes[3] == 0x46
            && bytes[8] == 0x57
            && bytes[9] == 0x45
            && bytes[10] == 0x42
            && bytes[11] == 0x50
        {
            return "image/webp".to_string();
        }
        // BMP: 42 4D
        if bytes[0] == 0x42 && bytes[1] == 0x4D {
            return "image/bmp".to_string();
        }
    }
    // Default to JPEG
    "image/jpeg".to_string()
}

// ── Incoming content helpers ───────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
struct DingTalkIncomingImageRef {
    download_code: String,
    file_name: String,
}

impl DingTalkIncomingImageRef {
    const DEFAULT_FILE_NAME: &str = "image.jpg";

    fn from_download_fields(download_code: Option<&str>, file_name: Option<&str>) -> Option<Self> {
        let download_code = download_code?.trim();
        if download_code.is_empty() {
            return None;
        }

        let file_name = file_name
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(Self::DEFAULT_FILE_NAME);

        Some(Self {
            download_code: download_code.to_string(),
            file_name: file_name.to_string(),
        })
    }

    fn from_content_map(map: &serde_json::Map<String, serde_json::Value>) -> Option<Self> {
        Self::from_download_fields(
            map.get("downloadCode")
                .and_then(|value| value.as_str())
                .or_else(|| {
                    map.get("pictureDownloadCode")
                        .and_then(|value| value.as_str())
                }),
            map.get("fileName").and_then(|value| value.as_str()),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DingTalkIncomingPart {
    Text(String),
    Image(DingTalkIncomingImageRef),
}

impl DingTalkChannel {
    const IMAGE_DOWNLOAD_FAILURE_MARKER: &str = "[IMAGE:download failed]";

    fn normalize_embedded_json(value: &serde_json::Value) -> serde_json::Value {
        if let Some(raw) = value.as_str()
            && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(raw)
        {
            return parsed;
        }
        value.clone()
    }

    fn normalized_content(data: &serde_json::Value) -> Option<serde_json::Value> {
        data.get("content").map(Self::normalize_embedded_json)
    }

    fn push_text_segment(parts: &mut Vec<DingTalkIncomingPart>, raw: &str) {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return;
        }
        parts.push(DingTalkIncomingPart::Text(trimmed.to_string()));
    }

    fn collect_rich_text_parts(value: &serde_json::Value, out: &mut Vec<DingTalkIncomingPart>) {
        match value {
            serde_json::Value::Object(map) => {
                if let Some(text) = map.get("text").and_then(|v| v.as_str()) {
                    Self::push_text_segment(out, text);
                }

                if let Some(image_ref) = DingTalkIncomingImageRef::from_content_map(map) {
                    out.push(DingTalkIncomingPart::Image(image_ref));
                }

                if let Some(rich_text) = map.get("richText") {
                    let normalized = Self::normalize_embedded_json(rich_text);
                    Self::collect_rich_text_parts(&normalized, out);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    Self::collect_rich_text_parts(item, out);
                }
            }
            serde_json::Value::String(raw) => {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(raw) {
                    Self::collect_rich_text_parts(&parsed, out);
                }
            }
            _ => {}
        }
    }

    fn extract_plain_text_content(data: &serde_json::Value) -> Option<String> {
        data.get("text")
            .and_then(|t| t.get("content"))
            .and_then(|c| c.as_str())
            .map(str::trim)
            .filter(|content| !content.is_empty())
            .map(ToString::to_string)
    }

    fn extract_picture_reference(data: &serde_json::Value) -> Option<DingTalkIncomingImageRef> {
        let content = Self::normalized_content(data)?;
        content
            .as_object()
            .and_then(DingTalkIncomingImageRef::from_content_map)
    }

    fn extract_rich_text_parts(data: &serde_json::Value) -> Vec<DingTalkIncomingPart> {
        let mut parts = Vec::new();
        if let Some(normalized) = Self::normalized_content(data)
            && let Some(rich_text) = normalized.get("richText")
        {
            let normalized_rich_text = Self::normalize_embedded_json(rich_text);
            Self::collect_rich_text_parts(&normalized_rich_text, &mut parts);
        }
        parts
    }

    fn image_download_failure_marker() -> String {
        Self::IMAGE_DOWNLOAD_FAILURE_MARKER.to_string()
    }

    async fn download_incoming_image_marker(
        &self,
        image_ref: &DingTalkIncomingImageRef,
        source: &'static str,
    ) -> Option<String> {
        match self
            .download_image_by_code(&image_ref.download_code, &image_ref.file_name)
            .await
        {
            Some(marker) => Some(marker),
            None => {
                dingtalk_warn!(
                    ::serde_json::json!({
                        "download_code": image_ref.download_code,
                        "source": source,
                    }),
                    "DingTalk: failed to download incoming image"
                );
                None
            }
        }
    }

    /// Handle incoming picture message: download and return [IMAGE:path] marker.
    async fn handle_picture_message(&self, data: &serde_json::Value) -> Option<String> {
        let Some(image_ref) = Self::extract_picture_reference(data) else {
            dingtalk_warn!("DingTalk: picture message missing downloadCode");
            return Some(Self::image_download_failure_marker());
        };

        Some(
            self.download_incoming_image_marker(&image_ref, "picture")
                .await
                .unwrap_or_else(Self::image_download_failure_marker),
        )
    }

    async fn extract_incoming_message_content(
        &self,
        msg_type: &str,
        data: &serde_json::Value,
    ) -> Option<String> {
        match msg_type {
            value if value.eq_ignore_ascii_case("picture") => {
                self.handle_picture_message(data).await
            }
            value if value.eq_ignore_ascii_case("richText") => {
                let parts = Self::extract_rich_text_parts(data);
                self.render_incoming_parts(parts).await
            }
            _ => Self::extract_plain_text_content(data),
        }
    }

    async fn build_incoming_channel_message(
        &self,
        data: &serde_json::Value,
        sender_id: &str,
        reply_target: String,
    ) -> Option<ChannelMessage> {
        let msg_type = data
            .get("msgtype")
            .and_then(|t| t.as_str())
            .unwrap_or("text");
        let content = self
            .extract_incoming_message_content(msg_type, data)
            .await?;

        Some(ChannelMessage {
            channel_alias: Some(self.alias.clone()),
            ..ChannelMessage::new(
                Uuid::new_v4().to_string(),
                sender_id.to_string(),
                reply_target,
                content,
                "dingtalk",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            )
        })
    }

    async fn render_incoming_parts(&self, parts: Vec<DingTalkIncomingPart>) -> Option<String> {
        let mut rendered_parts = Vec::new();
        for part in parts {
            match part {
                DingTalkIncomingPart::Text(text) => rendered_parts.push(text),
                DingTalkIncomingPart::Image(image_ref) => {
                    let marker = self
                        .download_incoming_image_marker(&image_ref, "rich_text")
                        .await
                        .unwrap_or_else(|| {
                            format!("[IMAGE:{} | download failed]", image_ref.download_code)
                        });
                    rendered_parts.push(marker);
                }
            }
        }

        let result = rendered_parts.join("\n");
        let trimmed = result.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_name() {
        let ch = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            "dingtalk_test_alias",
            Arc::new(Vec::new),
        );
        assert_eq!(ch.name(), "dingtalk");
    }

    #[test]
    fn test_user_allowed_wildcard() {
        let ch = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            "dingtalk_test_alias",
            Arc::new(|| vec!["*".into()]),
        );
        assert!(ch.is_user_allowed("anyone"));
    }

    #[test]
    fn test_user_allowed_specific() {
        let ch = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            "dingtalk_test_alias",
            Arc::new(|| vec!["user123".into()]),
        );
        assert!(ch.is_user_allowed("user123"));
        assert!(!ch.is_user_allowed("other"));
    }

    #[test]
    fn test_user_denied_empty() {
        let ch = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            "dingtalk_test_alias",
            Arc::new(Vec::new),
        );
        assert!(!ch.is_user_allowed("anyone"));
    }

    #[test]
    fn v2_allowed_users_fold_into_peer_groups() {
        // V2 `[channels.dingtalk].allowed_users` migrates into a synthesized
        // `[peer_groups.dingtalk_default]` block in V3. The wildcard sentinel
        // is filtered out during synthesis so only concrete usernames survive
        // as external peers.
        let v2_toml = r#"
schema_version = 2

[channels.dingtalk]
enabled = true
client_id = "app_id_123"
client_secret = "secret_456"
allowed_users = ["user1", "*"]
"#;
        let cfg = zeroclaw_config::migration::migrate_to_current(v2_toml)
            .expect("V2 dingtalk config migrates to V3");
        let dingtalk = cfg
            .channels
            .dingtalk
            .get("default")
            .expect("V2 dingtalk folds under alias `default`");
        assert_eq!(dingtalk.client_id, "app_id_123");
        assert_eq!(dingtalk.client_secret, "secret_456");

        let group = cfg
            .peer_groups
            .get("dingtalk_default")
            .expect("dingtalk allow-list synthesizes [peer_groups.dingtalk_default]");
        assert_eq!(group.channel, "dingtalk");
        let peers: Vec<&str> = group.external_peers.iter().map(|p| p.as_str()).collect();
        assert_eq!(peers, vec!["user1"]);
    }

    #[test]
    fn v2_no_allowed_users_synthesizes_no_peer_group() {
        // V2 dingtalk without `allowed_users` must not synthesize a peer group;
        // V3 leaves `peer_groups` empty rather than emitting an empty block.
        let v2_toml = r#"
schema_version = 2

[channels.dingtalk]
enabled = true
client_id = "id"
client_secret = "secret"
"#;
        let cfg = zeroclaw_config::migration::migrate_to_current(v2_toml)
            .expect("V2 dingtalk config without allowed_users migrates");
        assert!(
            !cfg.peer_groups.contains_key("dingtalk_default"),
            "no peer group synthesized when allowed_users is absent"
        );
    }

    #[test]
    fn robot_code_equals_client_id() {
        let ch = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            "dingtalk_test_alias",
            Arc::new(Vec::new),
        );
        assert_eq!(ch.robot_code(), "id");
    }

    #[test]
    fn format_api_text_content_includes_subject() {
        assert_eq!(
            DingTalkChannel::format_api_text_content("CPU > 95%", Some("告警")),
            "【告警】\nCPU > 95%"
        );
    }

    #[test]
    fn format_api_text_content_trims_empty_subject() {
        assert_eq!(
            DingTalkChannel::format_api_text_content("  hello  ", Some("  ")),
            "hello"
        );
    }

    #[test]
    fn parse_stream_data_supports_string_payload() {
        let frame = serde_json::json!({
            "data": "{\"text\":{\"content\":\"hello\"}}"
        });
        let parsed = DingTalkChannel::parse_stream_data(&frame).unwrap();
        assert_eq!(
            parsed.get("text").and_then(|v| v.get("content")),
            Some(&serde_json::json!("hello"))
        );
    }

    #[test]
    fn parse_stream_data_supports_object_payload() {
        let frame = serde_json::json!({
            "data": {"text": {"content": "hello"}}
        });
        let parsed = DingTalkChannel::parse_stream_data(&frame).unwrap();
        assert_eq!(
            parsed.get("text").and_then(|v| v.get("content")),
            Some(&serde_json::json!("hello"))
        );
    }

    #[test]
    fn resolve_chat_id_handles_numeric_group_conversation_type() {
        let data = serde_json::json!({
            "conversationType": 2,
            "conversationId": "cid-group",
        });
        let chat_id = DingTalkChannel::resolve_chat_id(&data, "staff-1");
        assert_eq!(chat_id, "cid-group");
    }

    #[test]
    fn extract_plain_text_content_trims_and_keeps_text() {
        let data = serde_json::json!({
            "text": { "content": "  hello DingTalk!  " }
        });

        assert_eq!(
            DingTalkChannel::extract_plain_text_content(&data),
            Some("hello DingTalk!".to_string())
        );
    }

    #[test]
    fn extract_picture_reference_supports_picture_download_code() {
        let data = serde_json::json!({
            "content": {
                "pictureDownloadCode": "abc123",
                "downloadCode": "def456",
                "fileName": "sample.png"
            }
        });

        let image_ref = DingTalkChannel::extract_picture_reference(&data).unwrap();
        assert_eq!(image_ref.download_code, "def456");
        assert_eq!(image_ref.file_name, "sample.png");
    }

    #[test]
    fn extract_rich_text_parts_preserves_array_order() {
        let data = serde_json::json!({
            "content": {
                "richText": [
                    { "text": "first line" },
                    {
                        "type": "picture",
                        "pictureDownloadCode": "pic-001",
                        "downloadCode": "img-001",
                        "fileName": "photo.jpg"
                    },
                    { "text": "last line" }
                ]
            }
        });

        let parts = DingTalkChannel::extract_rich_text_parts(&data);
        assert_eq!(
            parts,
            vec![
                DingTalkIncomingPart::Text("first line".to_string()),
                DingTalkIncomingPart::Image(DingTalkIncomingImageRef {
                    download_code: "img-001".to_string(),
                    file_name: "photo.jpg".to_string(),
                }),
                DingTalkIncomingPart::Text("last line".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn extract_incoming_message_content_uses_plain_text_for_text_messages() {
        let ch = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            "dingtalk_test_alias",
            Arc::new(Vec::new),
        );
        let data = serde_json::json!({
            "text": { "content": "  one line text  " }
        });

        assert_eq!(
            ch.extract_incoming_message_content("text", &data).await,
            Some("one line text".to_string())
        );
    }

    #[tokio::test]
    async fn extract_incoming_message_content_returns_failure_marker_for_picture_without_code() {
        let ch = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            "dingtalk_test_alias",
            Arc::new(Vec::new),
        );
        let data = serde_json::json!({
            "content": {}
        });

        assert_eq!(
            ch.extract_incoming_message_content("picture", &data).await,
            Some(DingTalkChannel::image_download_failure_marker())
        );
    }
}
