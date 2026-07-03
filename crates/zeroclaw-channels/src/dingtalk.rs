use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use regex::Regex;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, RwLock};
use tokio_tungstenite::tungstenite::Message;
use uuid::Uuid;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_config::schema::StreamMode;
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
    /// Runtime hook invoked after the channel persists a media
    /// file. Wired by the orchestrator at construction time.
    file_persisted_hook: Option<zeroclaw_api::channel::FilePersistedHook>,
    /// Upload cache: avoids re-uploading the same image within TTL.
    upload_cache: Arc<RwLock<HashMap<String, UploadCacheEntry>>>,
    /// Streaming mode for AI card responses (off/partial).
    stream_mode: StreamMode,
    /// Minimum interval between streamingUpdate calls in milliseconds.
    streaming_update_interval_ms: u64,
    /// Per-card timestamp of the last streamingUpdate PUT. Paired with
    /// `pending_streaming_text` to implement a "cache-and-flush" throttle
    /// identical to Lark's behavior: text deltas inside the throttle
    /// window overwrite the cached buffer; the next call outside the
    /// window sends the latest accumulated buffer in one PUT.
    last_streaming_edit: Arc<Mutex<HashMap<String, Instant>>>,
    /// Per-card buffer of the latest accumulated text waiting to be
    /// flushed by the next throttle window. Overwritten (not appended)
    /// on every drop — we only ever care about the freshest full text
    /// because `isFull: true` means the receiver replaces the body
    /// wholesale. Cleared on flush and on `finalize_draft` /
    /// `cancel_draft` to avoid stale data leaking into the next card.
    pending_streaming_text: Arc<Mutex<HashMap<String, String>>>,
    /// Cache of active AI card instances (cardInstanceId -> recipient).
    /// Used to track which cards are being streamed to which users.
    card_instances: Arc<RwLock<HashMap<String, DingTalkCardInstance>>>,
    /// AI Card Template ID for streaming responses.
    /// Must be created in DingTalk developer console first.
    ai_card_template_id: Option<String>,
}

/// AI card instance information for tracking active streaming sessions.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct DingTalkCardInstance {
    card_instance_id: String,
    created_at: Instant,
    recipient: String,
}

/// Cached upload entry to avoid re-uploading the same image.
struct UploadCacheEntry {
    media_id: String,
    photo_url: String,
    expires_at: u64,
}

/// Token cache entry with expiration time.
/// DingTalk access tokens are valid for 7200 seconds (2 hours).
/// We refresh 60 seconds early to avoid boundary issues.
#[derive(Clone)]
struct TokenCacheEntry {
    token: String,
    expires_at: Instant,
}

impl TokenCacheEntry {
    fn new(token: String) -> Self {
        // Token valid for 7200 seconds, refresh 60 seconds early
        Self {
            token,
            expires_at: Instant::now() + Duration::from_secs(7200 - 60),
        }
    }

    fn is_expired(&self) -> bool {
        Instant::now() >= self.expires_at
    }
}

/// Lightweight async handle for making DingTalk API calls.
/// Contains only the minimal state needed for HTTP requests,
/// used for async API calls without blocking the main channel.
#[derive(Clone)]
pub struct DingTalkChannelAsync {
    client_id: String,
    client_secret: String,
    proxy_url: Option<String>,
    /// Access token cache (2 hour validity)
    token_cache: Arc<RwLock<Option<TokenCacheEntry>>>,
}

impl DingTalkChannelAsync {
    /// Build an HTTP client with the same proxy configuration as the main channel.
    fn http_client(&self) -> reqwest::Client {
        zeroclaw_config::schema::build_channel_proxy_client(
            "channel.dingtalk.async",
            self.proxy_url.as_deref(),
        )
    }

    /// Get access token for API calls with caching.
    /// Tokens are cached for 2 hours (7200 seconds) per DingTalk's specification.
    /// Uses double-checked locking to handle concurrent requests efficiently.
    async fn get_access_token(&self) -> anyhow::Result<String> {
        // Fast path: check cache with read lock
        {
            let cache = self.token_cache.read().await;
            if let Some(entry) = cache.as_ref() {
                if !entry.is_expired() {
                    let remaining_secs = entry.expires_at.duration_since(Instant::now()).as_secs();
                    dingtalk_debug!(
                        ::serde_json::json!({
                            "remaining_secs": remaining_secs,
                        }),
                        "DingTalk: using cached access token"
                    );
                    return Ok(entry.token.clone());
                }
            }
        }

        // Slow path: acquire write lock and fetch new token
        // Use double-checked locking to avoid duplicate requests
        {
            // Re-check cache after acquiring write lock
            let cache = self.token_cache.read().await;
            if let Some(entry) = cache.as_ref() {
                if !entry.is_expired() {
                    let remaining_secs = entry.expires_at.duration_since(Instant::now()).as_secs();
                    dingtalk_debug!(
                        ::serde_json::json!({
                            "remaining_secs": remaining_secs,
                        }),
                        "DingTalk: using cached access token (after re-check)"
                    );
                    return Ok(entry.token.clone());
                }
            }
        }

        // Cache miss or expired: fetch new token
        dingtalk_info!("DingTalk: fetching new access token");

        let mut last_error = None;
        for attempt in 1..=DINGTALK_OUTBOUND_MAX_ATTEMPTS {
            match self.request_access_token_once().await {
                Ok(token) => {
                    // Update cache
                    {
                        let mut cache = self.token_cache.write().await;
                        // Final check to avoid overwriting valid token from concurrent request
                        if let Some(entry) = cache.as_ref() {
                            if !entry.is_expired() {
                                return Ok(entry.token.clone());
                            }
                        }
                        *cache = Some(TokenCacheEntry::new(token.clone()));
                    }

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

    /// Request access token once (single attempt).
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

        let status = resp.status();
        if !status.is_success() {
            return Err(anyhow::anyhow!(
                "DingTalk access token request failed with status {}: {}",
                status,
                resp.text().await.unwrap_or_default()
            ));
        }

        #[derive(Deserialize)]
        struct TokenResponse {
            #[serde(rename = "accessToken")]
            access_token: String,
        }

        let token_resp: TokenResponse = resp.json().await?;
        Ok(token_resp.access_token)
    }

    /// Update an AI card via streamingUpdate API.
    /// This is the async version used for background updates.
    async fn streaming_update_card(
        &self,
        card_instance_id: &str,
        content: &str,
        is_final: bool,
    ) -> anyhow::Result<()> {
        let token = self.get_access_token().await?;

        // Each update carries a unique GUID per the official SDK.
        let guid = Uuid::new_v4().to_string();

        let body = serde_json::json!({
            "outTrackId": card_instance_id,
            "guid": guid,
            "key": "content",
            "content": content,
            "isFull": true,
            "isFinalize": is_final,
            "isError": false,
        });

        let resp = self
            .http_client()
            .put("https://api.dingtalk.com/v1.0/card/streaming")
            .header("x-acs-dingtalk-access-token", &token)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let error_text = resp.text().await.unwrap_or_default();
            return Err(anyhow::anyhow!(
                "DingTalk streamingUpdate failed with status {}: {}",
                status,
                error_text
            ));
        }

        dingtalk_debug!(
            ::serde_json::json!({
                "out_track_id": card_instance_id,
                "content_bytes": content.len(),
                "is_final": is_final,
            }),
            "DingTalk: streamingUpdate card success"
        );

        Ok(())
    }
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
            file_persisted_hook: None,
            upload_cache: Arc::new(RwLock::new(HashMap::new())),
            stream_mode: StreamMode::Off,
            streaming_update_interval_ms: 1000,
            last_streaming_edit: Arc::new(Mutex::new(HashMap::new())),
            pending_streaming_text: Arc::new(Mutex::new(HashMap::new())),
            card_instances: Arc::new(RwLock::new(HashMap::new())),
            ai_card_template_id: None,
        }
    }

    /// Set the AI card template ID for streaming responses.
    /// The template must be created in DingTalk developer console first.
    pub fn with_ai_card_template(mut self, template_id: String) -> Self {
        self.ai_card_template_id = Some(template_id);
        self
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

    /// Configure progressive AI card streaming. `stream_mode = Off`
    /// (default) routes every response through `send()`; `partial` creates
    /// an AI card and updates it incrementally via `streamingUpdate` API.
    /// `multi_message` is rejected for DingTalk (falls back to `off` with
    /// a warning).
    ///
    /// `update_interval_ms` controls the minimum delay between consecutive
    /// `streamingUpdate` calls. Default: 500ms (tuned to DingTalk's rate limits).
    pub fn with_streaming(mut self, stream_mode: StreamMode, update_interval_ms: u64) -> Self {
        let effective_stream_mode = match stream_mode {
            StreamMode::MultiMessage => {
                dingtalk_warn!(
                    ::serde_json::json!({
                        "requested_mode": "multi_message",
                    }),
                    "DingTalk: stream_mode=multi_message is not supported; falling back to off (no AI card streaming)"
                );
                StreamMode::Off
            }
            mode => mode,
        };
        self.stream_mode = effective_stream_mode;
        // No hard floor: orchestrator-side coalescing + DingTalk streamingUpdate
        // quota (~30 PUT/s/card) are the real guardrails. A user-configured
        // 50ms still leaves 6x headroom.
        self.streaming_update_interval_ms = update_interval_ms;
        self
    }

    /// Create a lightweight clone for async API calls.
    /// This clones only the necessary fields for making HTTP requests,
    /// avoiding the overhead of cloning the entire channel state.
    fn clone_for_async_call(&self) -> DingTalkChannelAsync {
        DingTalkChannelAsync {
            client_id: self.client_id.clone(),
            client_secret: self.client_secret.clone(),
            proxy_url: self.proxy_url.clone(),
            token_cache: Arc::new(RwLock::new(None)),
        }
    }

    /// Check if streaming mode is enabled.
    fn supports_streaming(&self) -> bool {
        // Streaming requires both Partial mode AND a configured AI card template ID
        matches!(self.stream_mode, StreamMode::Partial) && self.ai_card_template_id.is_some()
    }

    /// Clean up expired card instances from the cache.
    /// Cards are considered expired after 30 minutes (typical conversation timeout).
    /// This method is intended to be called periodically by a background cleanup task.
    #[allow(dead_code)]
    async fn cleanup_expired_card_instances(&self) {
        let now = Instant::now();
        let expiry_duration = Duration::from_secs(1800); // 30 minutes

        let mut instances = self.card_instances.write().await;
        instances.retain(|_, instance| now.duration_since(instance.created_at) < expiry_duration);

        if !instances.is_empty() {
            dingtalk_debug!(
                ::serde_json::json!({
                    "active_instances": instances.len(),
                }),
                "DingTalk: cleaned up expired card instances"
            );
        }
    }

    /// Create an AI card instance for streaming responses and return its
    /// `outTrackId`. Subsequent `streamingUpdate` calls reference this same
    /// id.
    ///
    /// Aligned with the official `dingtalk-stream` Python SDK
    /// (`CardReplier.create_and_send_card`) — the card body uses
    /// `cardData.cardParamMap` and the space model is selected by whether
    /// the recipient is a single chat or a group.
    /// Docs: https://open.dingtalk.com/document/orgapp/interface-for-creating-a-card-instance
    async fn send_ai_card(&self, recipient: &str, initial_content: &str) -> anyhow::Result<String> {
        let template_id = self.ai_card_template_id.as_ref().ok_or_else(|| {
            anyhow::Error::msg(
                "AI card template ID not configured. Use with_ai_card_template() to set it.",
            )
        })?;

        let token = self.get_access_token().await?;

        // Per Python SDK: outTrackId is the caller's tracking identifier and is
        // echoed back as the card's primary reference. Use a UUID to avoid
        // collisions when the same recipient re-sends within the same second.
        let out_track_id = Uuid::new_v4().to_string();

        // Pick the correct open-space model by looking up the cached reply
        // target learned from the inbound event. Fall back to the single-chat
        // model when the target has not been recorded yet — the official SDK
        // only sends one of the two, never both.
        let is_group = matches!(
            self.reply_target_for_recipient(recipient).await,
            Some(DingTalkReplyTarget::Group(_))
        );

        let mut create_body = serde_json::json!({
            "cardTemplateId": template_id,
            "outTrackId": out_track_id,
            "callbackType": "STREAM",
            "cardData": {
                "cardParamMap": {
                    "content": initial_content,
                    "status": "thinking",
                },
            },
        });
        let obj = create_body
            .as_object_mut()
            .ok_or_else(|| anyhow::Error::msg("DingTalk: create_body must be a JSON object"))?;
        if is_group {
            obj.insert(
                "imGroupOpenSpaceModel".into(),
                serde_json::json!({
                    "supportForward": true,
                    "openSpaceId": recipient,
                    "robotCode": self.client_id,
                }),
            );
        } else {
            obj.insert(
                "imRobotOpenSpaceModel".into(),
                serde_json::json!({
                    "supportForward": true,
                    "robotCode": self.client_id,
                }),
            );
        }

        dingtalk_info!(
            ::serde_json::json!({
                "template_id": template_id,
                "out_track_id": out_track_id,
                "is_group": is_group,
                "callback_type": "STREAM",
            }),
            "DingTalk: Creating AI card with STREAM callback"
        );

        let resp = self
            .http_client()
            .post("https://api.dingtalk.com/v1.0/card/instances")
            .header("x-acs-dingtalk-access-token", &token)
            .header("Content-Type", "application/json")
            .json(&create_body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("DingTalk card instance create failed ({status}): {err}");
        }

        // The official SDK treats the caller-supplied outTrackId as the
        // card's identifier; we do the same rather than re-parsing the
        // response body for a server-side id.
        let card_id = out_track_id;

        // Cache the card instance for tracking
        {
            let mut instances = self.card_instances.write().await;
            instances.insert(
                card_id.clone(),
                DingTalkCardInstance {
                    card_instance_id: card_id.clone(),
                    created_at: Instant::now(),
                    recipient: recipient.to_string(),
                },
            );
        }

        dingtalk_info!(
            ::serde_json::json!({
                "card_id": card_id,
            }),
            "DingTalk: AI card created successfully"
        );

        // Two-step flow per the official `dingtalk-stream` Python SDK:
        //   1) create instance   (POST /v1.0/card/instances)
        //   2) deliver to user   (POST /v1.0/card/instances/deliver)
        // Without the second call, the card exists server-side but is
        // never pushed to the recipient's DingTalk client, so the user
        // sees nothing in the chat. This is why every "successful"
        // finalize PUT in the logs was a no-op on the user side.
        if let Err(error) = self.deliver_ai_card(&card_id, recipient).await {
            dingtalk_warn!(
                ::serde_json::json!({
                    "out_track_id": card_id,
                    "recipient": recipient,
                    "error": error.to_string(),
                }),
                "DingTalk: AI card deliver failed (card created but not delivered)"
            );
            return Err(error);
        }

        Ok(card_id)
    }

    /// Deliver an AI card to the recipient's chat.
    ///
    /// Second half of the official `CardReplier.create_and_send_card`
    /// flow. Without this call, the created card instance is never
    /// surfaced to the DingTalk client and the recipient receives
    /// nothing — every streamingUpdate afterwards is silently dropped.
    ///
    /// Aligned with the official Python SDK:
    /// `POST https://api.dingtalk.com/v1.0/card/instances/deliver`
    /// Docs: https://open.dingtalk.com/document/orgapp/delivery-card-interface
    async fn deliver_ai_card(&self, out_track_id: &str, recipient: &str) -> anyhow::Result<()> {
        let token = self.get_access_token().await?;

        let is_group = matches!(
            self.reply_target_for_recipient(recipient).await,
            Some(DingTalkReplyTarget::Group(_))
        );

        // `openSpaceId` is a magic string the DingTalk card platform
        // recognizes for routing the deliver to a 1:1 IM chat (IM_ROBOT)
        // or a group (IM_GROUP). The spaceId is the sender_staff_id for
        // single chat and the conversation_id for group chat. We resolve
        // both from the cached reply target learned from the inbound
        // event, falling back to the raw `recipient` for the 1:1 case
        // (most inbound staffId-only sessions store it there).
        let (open_space_id, deliver_model) = if is_group {
            let open_space_id = format!("dtv1.card//IM_GROUP.{recipient}");
            let model = serde_json::json!({
                "robotCode": self.client_id,
            });
            (open_space_id, model)
        } else {
            let open_space_id = format!("dtv1.card//IM_ROBOT.{recipient}");
            let model = serde_json::json!({
                "spaceType": "IM_ROBOT",
            });
            (open_space_id, model)
        };

        let body = serde_json::json!({
            "outTrackId": out_track_id,
            "userIdType": 1,
            "openSpaceId": open_space_id,
            "imGroupOpenDeliverModel": deliver_model,
            "imRobotOpenDeliverModel": deliver_model,
        });

        dingtalk_info!(
            ::serde_json::json!({
                "out_track_id": out_track_id,
                "is_group": is_group,
                "open_space_id": open_space_id,
            }),
            "DingTalk: Delivering AI card"
        );

        let resp = self
            .http_client()
            .post("https://api.dingtalk.com/v1.0/card/instances/deliver")
            .header("x-acs-dingtalk-access-token", &token)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("DingTalk card deliver failed ({status}): {err}");
        }

        dingtalk_info!(
            ::serde_json::json!({
                "out_track_id": out_track_id,
            }),
            "DingTalk: AI card delivered"
        );

        Ok(())
    }

    /// Push a streaming update to a previously created AI card.
    ///
    /// Aligned with the official `dingtalk-stream` Python SDK
    /// (`AICardReplier.async_streaming`):
    /// - `PUT https://api.dingtalk.com/v1.0/card/streaming`
    /// - `content` is a plain string, not a nested params object
    /// - `isFull=true` means the body is the full accumulated text (not a
    ///   delta). The orchestrator always sends the full accumulated
    ///   buffer; we never use incremental updates because LLM tokens
    ///   can interleave with tool calls in non-monotonic ways.
    /// - `isFinalize=true` closes the card (triggers the "Done" reaction).
    ///   Docs: https://open.dingtalk.com/document/development/api-streamingupdate
    async fn streaming_update_card(
        &self,
        card_instance_id: &str,
        content: &str,
        is_final: bool,
    ) -> anyhow::Result<()> {
        let token = self.get_access_token().await?;

        // Each update carries a unique GUID per the official SDK.
        let guid = Uuid::new_v4().to_string();

        let body = serde_json::json!({
            "outTrackId": card_instance_id,
            "guid": guid,
            "key": "content",
            "content": content,
            "isFull": true,
            "isFinalize": is_final,
            "isError": false,
        });

        dingtalk_debug!(
            ::serde_json::json!({
                "out_track_id": card_instance_id,
                "guid": guid,
                "is_finalize": is_final,
                "content_bytes": content.len(),
            }),
            "DingTalk: Streaming card update"
        );

        let resp = self
            .http_client()
            .put("https://api.dingtalk.com/v1.0/card/streaming")
            .header("x-acs-dingtalk-access-token", &token)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("DingTalk streaming update failed ({status}): {err}");
        }

        dingtalk_debug!(
            ::serde_json::json!({
                "out_track_id": card_instance_id,
                "is_finalize": is_final,
            }),
            "DingTalk: Card update successful"
        );

        if is_final {
            let mut instances = self.card_instances.write().await;
            instances.remove(card_instance_id);
        }

        Ok(())
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
    fn on_file_persisted(&self, path: &std::path::Path) {
        if let Some(hook) = &self.file_persisted_hook {
            hook(path);
        }
    }

    fn name(&self) -> &str {
        "dingtalk"
    }

    /// True when both Partial mode is enabled and a template id is set.
    /// When false, the orchestrator falls back to the non-streaming
    /// `send()` path automatically.
    fn supports_draft_updates(&self) -> bool {
        self.supports_streaming()
    }

    /// Open a streaming AI card for the recipient and return its
    /// `outTrackId` as the platform-specific message id used by
    /// subsequent `update_draft` / `finalize_draft` calls.
    /// Returns `Ok(None)` when streaming is unsupported or the card
    /// create call fails — the orchestrator will then route through
    /// the non-streaming `send()` path.
    async fn send_draft(&self, message: &SendMessage) -> anyhow::Result<Option<String>> {
        if !self.supports_streaming() {
            return Ok(None);
        }
        match self.send_ai_card(&message.recipient, "正在思考中…").await {
            Ok(card_id) => {
                dingtalk_info!(
                    ::serde_json::json!({
                        "recipient": message.recipient,
                        "card_id": card_id,
                    }),
                    "DingTalk: send_draft opened streaming card"
                );
                Ok(Some(card_id))
            }
            Err(error) => {
                dingtalk_warn!(
                    ::serde_json::json!({
                        "recipient": message.recipient,
                        "error": error.to_string(),
                    }),
                    "DingTalk: send_draft failed, falling back to non-streaming send()"
                );
                Ok(None)
            }
        }
    }

    /// Push an incremental AI card update with the latest accumulated
    /// text. The orchestrator emits a `text` argument that is the full
    /// accumulated buffer (see the `accumulated.push_str` site in the
    /// orchestrator), so we never need to stitch deltas here — we just
    /// honor the throttle window.
    ///
    /// **Throttle model: cache-and-flush** (matches Lark).
    /// - Inside the throttle window: overwrite the cached buffer for
    ///   this card with the freshest text and return without sending.
    /// - At or past the window: flush the cached buffer (which is the
    ///   freshest accumulated text the orchestrator has produced) in
    ///   one streamingUpdate PUT, reset the timer, and clear the cache.
    ///
    /// Because DingTalk's `isFull: true` PUT replaces the card body
    /// wholesale, the receiver always sees the freshest full content.
    /// We never lose data: every incoming delta overwrites the cache,
    /// and the next flush sends it. The only data we drop is
    /// intermediate `text_bytes` from a burst, which is exactly what
    /// Feishu's 5-QPS PATCH coalescing drops.
    async fn update_draft(
        &self,
        _recipient: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        if message_id.is_empty() || !self.supports_streaming() {
            return Ok(());
        }
        let interval_ms = self.streaming_update_interval_ms;

        // Determine whether we are inside the throttle window.
        let now = Instant::now();
        let elapsed_ms = {
            let last_guard = self.last_streaming_edit.lock().await;
            last_guard.get(message_id).map(|last| {
                u64::try_from(now.duration_since(*last).as_millis()).unwrap_or(u64::MAX)
            })
        };
        let inside_window = elapsed_ms.is_some_and(|e| e < interval_ms);

        if inside_window {
            // Cache the freshest buffer; flush on the next call outside
            // the window. Overwrite (not append) — the orchestrator
            // already passes the full accumulated text.
            let mut cache = self.pending_streaming_text.lock().await;
            cache.insert(message_id.to_string(), text.to_string());
            return Ok(());
        }

        // Outside the window: build the payload from the cached buffer
        // (or the just-arrived `text` if no cache exists) and flush.
        let to_send = {
            let mut cache = self.pending_streaming_text.lock().await;
            cache.remove(message_id).unwrap_or_else(|| text.to_string())
        };

        // Reset the throttle timer BEFORE making the API call.
        // This is critical: we want the next call to be allowed after
        // `interval_ms` from NOW, not after the API response returns.
        {
            let mut last_guard = self.last_streaming_edit.lock().await;
            last_guard.insert(message_id.to_string(), Instant::now());
        }

        dingtalk_debug!(
            ::serde_json::json!({
                "card_id": message_id,
                "text_bytes": to_send.len(),
            }),
            "DingTalk: update_draft flush"
        );

        // Spawn the API call asynchronously to avoid blocking the caller.
        // This allows the orchestrator to continue processing LLM tokens
        // without waiting for the DingTalk API response (~350ms).
        let self_arc = Arc::new(self.clone_for_async_call());
        let message_id = message_id.to_string();
        zeroclaw_spawn::spawn!(async move {
            if let Err(error) = self_arc
                .streaming_update_card(&message_id, &to_send, false)
                .await
            {
                dingtalk_warn!(
                    ::serde_json::json!({
                        "out_track_id": message_id,
                        "error": error.to_string(),
                    }),
                    "DingTalk: update_draft streaming call failed (async)"
                );
            }
        });

        Ok(())
    }

    /// Close the AI card with the final accumulated text. Triggers
    /// the "Done" reaction on the DingTalk client.
    async fn finalize_draft(
        &self,
        _recipient: &str,
        message_id: &str,
        text: &str,
    ) -> anyhow::Result<()> {
        if message_id.is_empty() || !self.supports_streaming() {
            return Ok(());
        }
        // Drop the per-card throttle slot AND the pending buffer so the
        // next message on this handle starts from a clean state. The
        // final text is supplied by the orchestrator (`text` arg) and
        // already supersedes anything still in the cache, so we discard
        // the cache rather than flush-then-PUT, which would be two round
        // trips.
        self.last_streaming_edit.lock().await.remove(message_id);
        self.pending_streaming_text.lock().await.remove(message_id);
        dingtalk_info!(
            ::serde_json::json!({
                "card_id": message_id,
                "text_bytes": text.len(),
            }),
            "DingTalk: finalize_draft streaming card"
        );
        if let Err(error) = self.streaming_update_card(message_id, text, true).await {
            dingtalk_warn!(
                ::serde_json::json!({
                    "out_track_id": message_id,
                    "error": error.to_string(),
                }),
                "DingTalk: finalize_draft streaming call failed"
            );
        }
        Ok(())
    }

    /// Best-effort cancel: send a final update with a short notice so
    /// the card doesn't stay in "thinking" state. We do not have a
    /// dedicated delete API; DingTalk clients will replace the body
    /// with the notice.
    async fn cancel_draft(&self, _recipient: &str, message_id: &str) -> anyhow::Result<()> {
        if message_id.is_empty() || !self.supports_streaming() {
            return Ok(());
        }
        let result = self
            .streaming_update_card(message_id, "[回答已取消]", true)
            .await;
        // Evict the throttle slot AND the pending buffer so a future
        // card reuses the same id without inheriting stale rate-limit
        // or cache state.
        self.last_streaming_edit.lock().await.remove(message_id);
        self.pending_streaming_text.lock().await.remove(message_id);
        if let Err(error) = result {
            dingtalk_warn!(
                ::serde_json::json!({
                    "out_track_id": message_id,
                    "error": error.to_string(),
                }),
                "DingTalk: cancel_draft streaming call failed"
            );
        }
        Ok(())
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

    /// Install the runtime hook that the channel will invoke after
    /// persisting a media file. Wired by the orchestrator.
    pub fn with_file_persisted_hook(
        mut self,
        hook: zeroclaw_api::channel::FilePersistedHook,
    ) -> Self {
        self.file_persisted_hook = Some(hook);
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
                    self.on_file_persisted(&path);
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
                "title": zeroclaw_api::branding::product_name(),
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
        let title = subject
            .map(String::from)
            .unwrap_or_else(zeroclaw_api::branding::product_name);
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

    #[test]
    fn test_streaming_support_detection() {
        // Partial mode + template ID enables streaming
        let ch_partial = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            "dingtalk_test_alias",
            Arc::new(Vec::new),
        )
        .with_streaming(StreamMode::Partial, 500)
        .with_ai_card_template("tpl-001".into());
        assert!(ch_partial.supports_streaming());

        // Partial mode without template ID -> no streaming
        let ch_no_tpl = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            "dingtalk_test_alias",
            Arc::new(Vec::new),
        )
        .with_streaming(StreamMode::Partial, 500);
        assert!(!ch_no_tpl.supports_streaming());

        // MultiMessage mode falls back to Off
        let ch_multi = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            "dingtalk_test_alias",
            Arc::new(Vec::new),
        )
        .with_streaming(StreamMode::MultiMessage, 500);
        assert!(!ch_multi.supports_streaming());
    }

    #[test]
    fn test_with_streaming_configuration() {
        // Test Partial mode with custom interval
        let ch_streaming = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            "dingtalk_test_alias",
            Arc::new(Vec::new),
        )
        .with_streaming(StreamMode::Partial, 1000);
        assert_eq!(ch_streaming.streaming_update_interval_ms, 1000);

        // Test that small intervals are accepted (no hard floor)
        let ch_fast = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            "dingtalk_test_alias",
            Arc::new(Vec::new),
        )
        .with_streaming(StreamMode::Partial, 50);
        assert_eq!(ch_fast.streaming_update_interval_ms, 50);
    }

    #[tokio::test]
    async fn test_card_instance_cleanup() {
        let ch = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            "dingtalk_test_alias",
            Arc::new(Vec::new),
        );

        // Manually insert a card instance for testing
        {
            let mut instances = ch.card_instances.write().await;
            instances.insert(
                "test_card_1".to_string(),
                DingTalkCardInstance {
                    card_instance_id: "test_card_1".to_string(),
                    created_at: Instant::now(),
                    recipient: "user1".to_string(),
                },
            );
        }

        // Cleanup should not remove recent instances
        ch.cleanup_expired_card_instances().await;
        {
            let instances = ch.card_instances.read().await;
            assert_eq!(instances.len(), 1);
        }

        // Manually insert an old instance (simulate by modifying created_at)
        {
            let mut instances = ch.card_instances.write().await;
            if let Some(instance) = instances.get_mut("test_card_1") {
                // Set created_at to 31 minutes ago
                instance.created_at = Instant::now() - Duration::from_secs(1860);
            }
        }

        // Cleanup should remove expired instances
        ch.cleanup_expired_card_instances().await;
        {
            let instances = ch.card_instances.read().await;
            assert_eq!(instances.len(), 0);
        }
    }

    #[test]
    fn test_stream_mode_multi_message_fallback() {
        let ch = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            "dingtalk_test_alias",
            Arc::new(Vec::new),
        );

        // MultiMessage should fall back to Off with a warning
        let ch_multi = ch.with_streaming(StreamMode::MultiMessage, 500);
        assert_eq!(ch_multi.stream_mode, StreamMode::Off);
    }

    // --- Draft streaming trait method tests ----------------------------------

    #[test]
    fn test_draft_support_reflects_streaming() {
        // Off mode + no template -> no draft streaming
        let ch = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            "dingtalk_test_alias",
            Arc::new(Vec::new),
        );
        assert!(!ch.supports_draft_updates());

        // Partial mode but no template id -> still no draft streaming
        let ch_partial_no_tpl = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            "dingtalk_test_alias",
            Arc::new(Vec::new),
        )
        .with_streaming(StreamMode::Partial, 500);
        assert!(!ch_partial_no_tpl.supports_draft_updates());

        // Partial mode + template id -> draft streaming enabled
        let ch_full = ch_partial_no_tpl.with_ai_card_template("tpl-001".into());
        assert!(ch_full.supports_draft_updates());
    }

    #[tokio::test]
    async fn test_send_draft_returns_none_when_streaming_disabled() {
        // No template id -> send_draft must return Ok(None) so the
        // orchestrator falls back to non-streaming send().
        let ch = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            "dingtalk_test_alias",
            Arc::new(Vec::new),
        )
        .with_streaming(StreamMode::Partial, 500);
        let msg = SendMessage::new("...", "user1");
        let result = ch.send_draft(&msg).await.expect("send_draft ok");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_update_draft_is_noop_when_disabled() {
        let ch = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            "dingtalk_test_alias",
            Arc::new(Vec::new),
        );
        // Even with a card id, the call must not panic and must not
        // attempt to issue an HTTP request (no token cache, would
        // fail loudly). supports_streaming() is false, so we short-
        // circuit before any HTTP work.
        ch.update_draft("user1", "card-1", "hello")
            .await
            .expect("ok");
        ch.finalize_draft("user1", "card-1", "hello")
            .await
            .expect("ok");
        ch.cancel_draft("user1", "card-1").await.expect("ok");
    }

    #[tokio::test]
    async fn test_update_draft_ignores_empty_message_id() {
        // With streaming enabled but empty card id, the call must
        // silently succeed without any HTTP traffic.
        let ch = DingTalkChannel::new(
            "id".into(),
            "secret".into(),
            "dingtalk_test_alias",
            Arc::new(Vec::new),
        )
        .with_streaming(StreamMode::Partial, 500)
        .with_ai_card_template("tpl-001".into());
        ch.update_draft("user1", "", "hello").await.expect("ok");
        ch.finalize_draft("user1", "", "hello").await.expect("ok");
        ch.cancel_draft("user1", "").await.expect("ok");
    }
}
