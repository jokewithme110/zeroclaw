use anyhow::{Result, anyhow};
use async_trait::async_trait;
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::header::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, client_async, connect_async};
use uuid::Uuid;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};
use zeroclaw_config::schema::BotServiceConfig;

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
type WsWriter = SplitSink<WsStream, WsMessage>;
type WsReader = SplitStream<WsStream>;

#[derive(Debug, Deserialize)]
struct InboundMessage {
    #[serde(rename = "chatUuid")]
    chat_uuid: String,
    #[serde(rename = "topicId")]
    topic_id: Option<i64>,
    #[serde(rename = "msgId")]
    msg_id: Option<i64>,
    text: String,
}

#[derive(Debug, Serialize)]
struct OutboundMessage {
    bo: OutboundBo,
    code: OutboundCode,
}
#[derive(Debug, Serialize)]
struct OutboundBo {
    #[serde(rename = "chatUuid")]
    chat_uuid: String,
    result: String,
    #[serde(rename = "messageId", skip_serializing_if = "String::is_empty")]
    message_id: String,
    #[serde(rename = "msgType", skip_serializing_if = "Option::is_none")]
    msg_type: Option<String>,
}
#[derive(Debug, Serialize)]
struct OutboundCode {
    code: String,
    msg: String,
    #[serde(rename = "msgId")]
    msg_id: String,
}

fn truncate_for_log(s: &str, n: usize) -> String {
    if n == 0 || s.len() <= n {
        return s.to_string();
    }
    let end = s.char_indices().nth(n).map(|(idx, _)| idx).unwrap_or(s.len());
    format!("{}...", &s[..end])
}

fn sanitize_ws_url_for_log(raw: &str) -> String {
    if let Some(idx) = raw.find("key=") {
        let prefix = &raw[..idx + 4];
        let rest = &raw[idx + 4..];
        let end = rest.find('&').map(|i| i + idx + 4).unwrap_or(raw.len());
        let suffix = &raw[end..];
        format!("{prefix}***{suffix}")
    } else {
        raw.to_string()
    }
}

fn next_backoff(current: Duration, max: Duration, multiplier: f64) -> Duration {
    let base = if current.is_zero() { Duration::from_secs(1) } else { current };
    let next = Duration::from_secs_f64(base.as_secs_f64() * multiplier);
    if next > max { max } else { next }
}

#[derive(Clone)]
pub struct BotServiceChannel {
    cfg: BotServiceConfig,
    conn_write: Arc<Mutex<Option<WsWriter>>>,
    conn_read: Arc<Mutex<Option<WsReader>>>,
    closed: Arc<AtomicBool>,
    last_inbound_id: Arc<Mutex<HashMap<String, String>>>,
    reconnect_initial_delay: Duration,
    reconnect_max_delay: Duration,
    reconnect_multiplier: f64,
    heartbeat_interval: Duration,
}

impl BotServiceChannel {
    pub fn new(cfg: BotServiceConfig) -> Self {
        Self {
            cfg,
            conn_write: Arc::new(Mutex::new(None)),
            conn_read: Arc::new(Mutex::new(None)),
            closed: Arc::new(AtomicBool::new(false)),
            last_inbound_id: Arc::new(Mutex::new(HashMap::new())),
            reconnect_initial_delay: Duration::from_secs(1),
            reconnect_max_delay: Duration::from_secs(60),
            reconnect_multiplier: 2.0,
            heartbeat_interval: Duration::from_secs(30),
        }
    }
    fn build_ws_url(&self) -> Result<String> {
        let raw = self.cfg.ws_url.trim();
        if raw.is_empty() {
            return Err(anyhow!("bot_service ws_url is empty"));
        }
        if !raw.starts_with("ws://") && !raw.starts_with("wss://") {
            return Err(anyhow!("invalid bot_service ws_url scheme: {}", raw));
        }
        if let Some(secret) = self.cfg.secret_key.as_deref().map(str::trim) {
            if !secret.is_empty() && !raw.contains("key=") {
                let sep = if raw.contains('?') { '&' } else { '?' };
                return Ok(format!("{raw}{sep}key={secret}"));
            }
        }
        Ok(raw.to_string())
    }
    fn parse_ws_host_port(ws_url: &str) -> Result<(String, u16)> {
        let without_scheme = ws_url
            .strip_prefix("ws://")
            .or_else(|| ws_url.strip_prefix("wss://"))
            .ok_or_else(|| anyhow!("invalid WebSocket URL scheme: {ws_url}"))?;
        let host_port = without_scheme
            .split('/')
            .next()
            .ok_or_else(|| anyhow!("missing host in WebSocket URL: {ws_url}"))?;
        let mut parts = host_port.splitn(2, ':');
        let host = parts
            .next()
            .filter(|h| !h.is_empty())
            .ok_or_else(|| anyhow!("missing host in WebSocket URL: {ws_url}"))?
            .to_string();
        let port = match parts.next() {
            Some(p) => p
                .parse::<u16>()
                .map_err(|_| anyhow!("invalid port in WebSocket URL: {ws_url}"))?,
            None => {
                if ws_url.starts_with("wss://") {
                    443
                } else {
                    80
                }
            }
        };
        Ok((host, port))
    }
    fn parse_proxy_host_port(proxy: &str) -> Result<(String, u16)> {
        let trimmed = proxy.trim();
        let without_scheme = trimmed
            .strip_prefix("http://")
            .or_else(|| trimmed.strip_prefix("https://"))
            .unwrap_or(trimmed);
        let mut parts = without_scheme.splitn(2, ':');
        let host = parts
            .next()
            .filter(|h| !h.is_empty())
            .ok_or_else(|| anyhow!("invalid http_proxy host: {proxy}"))?
            .to_string();
        let port = match parts.next() {
            Some(p) => p
                .parse::<u16>()
                .map_err(|_| anyhow!("invalid http_proxy port: {proxy}"))?,
            None => 8080,
        };
        Ok((host, port))
    }
    async fn connect(&self) -> Result<()> {
        let ws_url = self.build_ws_url()?;
        let mut request = ws_url.as_str().into_client_request()?;
        if let Some(account_id) = self.cfg.account_id.as_deref().map(str::trim) {
            if !account_id.is_empty() {
                if let Ok(header) = HeaderValue::from_str(account_id) {
                    request.headers_mut().insert("X-Emp-No", header);
                }
            }
        }
        let res = if let Some(proxy) = self.cfg.http_proxy.as_deref().map(str::trim) {
            let proxy = proxy.to_string();
            if ws_url.starts_with("wss://") {
                return Err(anyhow!("bot_service http_proxy only supports ws:// URLs"));
            }
            let (proxy_host, proxy_port) = Self::parse_proxy_host_port(&proxy)?;
            let (target_host, target_port) = Self::parse_ws_host_port(&ws_url)?;
            let proxy_addr = format!("{proxy_host}:{proxy_port}");
            let mut stream = TcpStream::connect(&proxy_addr).await?;
            let connect_req = format!(
                "CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n\r\n",
                host = target_host,
                port = target_port
            );
            stream.write_all(connect_req.as_bytes()).await?;
            let mut buf = Vec::with_capacity(1024);
            let mut tmp = [0u8; 256];
            loop {
                let n = stream.read(&mut tmp).await?;
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
                if buf.len() > 8 * 1024 {
                    break;
                }
            }
            let resp = String::from_utf8_lossy(&buf);
            let status_line = resp.lines().next().unwrap_or_default();
            if !status_line.starts_with("HTTP/1.1 200") && !status_line.starts_with("HTTP/1.0 200")
            {
                return Err(anyhow!("http_proxy CONNECT failed: {}", status_line));
            }
            let tls_stream = MaybeTlsStream::Plain(stream);
            client_async(request, tls_stream).await
        } else {
            connect_async(request).await
        };
        match res {
            Ok((stream, _)) => {
                let (write_half, read_half) = stream.split();
                *self.conn_write.lock().await = Some(write_half);
                *self.conn_read.lock().await = Some(read_half);
                self.closed.store(false, Ordering::SeqCst);
                Ok(())
            }
            Err(err) => {
                tracing::error!(
                    target: "bot_service",
                    "WebSocket dial failed url={} error={}",
                    sanitize_ws_url_for_log(&ws_url),
                    err
                );
                Err(anyhow!(err))
            }
        }
    }
    fn is_sender_allowed(&self, chat_uuid: &str) -> bool {
        if self.cfg.allowed_from.iter().any(|v| v.as_str() == "*") {
            return true;
        }
        self.cfg.allowed_from.iter().any(|v| v == chat_uuid)
    }
    async fn read_loop(&self, mut tx: tokio::sync::mpsc::Sender<ChannelMessage>) {
        let mut backoff = self.reconnect_initial_delay;
        loop {
            if self.closed.load(Ordering::SeqCst) {
                return;
            }
            let has_reader = self.conn_read.lock().await.is_some();
            if !has_reader {
                if let Err(err) = self.connect().await {
                    tracing::warn!(target: "bot_service", "Reconnect failed: {err}");
                    sleep(backoff).await;
                    backoff = next_backoff(
                        backoff,
                        self.reconnect_max_delay,
                        self.reconnect_multiplier,
                    );
                    continue;
                }
                backoff = self.reconnect_initial_delay;
                continue;
            }
            let mut read_guard = self.conn_read.lock().await;
            let result = match read_guard.as_mut() {
                Some(reader) => timeout(Duration::from_secs(300), reader.next()).await,
                None => {
                    drop(read_guard);
                    continue;
                }
            };
            match result {
                Ok(Some(Ok(msg))) => {
                    drop(read_guard);
                    if let Err(err) = self.handle_ws_message(msg, &mut tx).await {
                        tracing::warn!(target: "bot_service", "Inbound parse/handle failed: {err}");
                    }
                }
                Ok(Some(Err(_))) | Ok(None) => {
                    *read_guard = None;
                    *self.conn_write.lock().await = None;
                    drop(read_guard);
                    sleep(backoff).await;
                    backoff = next_backoff(
                        backoff,
                        self.reconnect_max_delay,
                        self.reconnect_multiplier,
                    );
                }
                Err(_) => {
                    drop(read_guard);
                }
            }
        }
    }
    async fn handle_ws_message(
        &self,
        msg: WsMessage,
        tx: &mut tokio::sync::mpsc::Sender<ChannelMessage>,
    ) -> Result<()> {
        let data: String = match msg {
            WsMessage::Text(t) => t.to_string(),
            WsMessage::Binary(b) => String::from_utf8_lossy(&b).to_string(),
            WsMessage::Ping(_) | WsMessage::Pong(_) => return Ok(()),
            WsMessage::Close(_) => {
                self.closed.store(true, Ordering::SeqCst);
                return Ok(());
            }
            WsMessage::Frame(_) => return Ok(()),
        };
        let mut raw = data.trim().to_string();
        if raw.is_empty() {
            return Ok(());
        }
        if let Some(stripped) = raw.strip_prefix("data:") {
            let trimmed = stripped.trim();
            if trimmed == "[DONE]" {
                return Ok(());
            }
            raw = trimmed.to_string();
        }
        let mut inbound: InboundMessage = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(err) => {
                tracing::warn!(
                    target: "bot_service",
                    "Inbound parse failed error={} raw={}",
                    err,
                    truncate_for_log(&raw, 200)
                );
                return Ok(());
            }
        };
        let _ = inbound.topic_id;
        inbound.chat_uuid = inbound.chat_uuid.trim().to_string();
        inbound.text = inbound.text.trim().to_string();
        if inbound.chat_uuid.is_empty() || inbound.text.is_empty() {
            return Ok(());
        }
        if !self.is_sender_allowed(&inbound.chat_uuid) {
            return Ok(());
        }
        if let Some(id) = inbound.msg_id {
            self.last_inbound_id
                .lock()
                .await
                .insert(inbound.chat_uuid.clone(), id.to_string());
        }
        let sender = format!("bot_service:{}", inbound.chat_uuid);
        let msg = ChannelMessage {
            id: Uuid::new_v4().to_string(),
            sender: sender.clone(),
            reply_target: sender.clone(),
            content: inbound.text,
            channel: "bot_service".to_string(),
            timestamp: chrono::Utc::now().timestamp() as u64,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: Vec::new(),
        };
        let _ = tx.send(msg).await;
        Ok(())
    }
    async fn heartbeat_loop(&self) {
        if self.heartbeat_interval.is_zero() {
            return;
        }
        let mut interval = tokio::time::interval(self.heartbeat_interval);
        loop {
            interval.tick().await;
            if self.closed.load(Ordering::SeqCst) {
                return;
            }
            let mut guard = self.conn_write.lock().await;
            if let Some(writer) = guard.as_mut() {
                let _ = writer.send(WsMessage::Ping(b"ping".to_vec().into())).await;
            }
        }
    }
}

#[async_trait]
impl Channel for BotServiceChannel {
    fn name(&self) -> &str {
        "bot_service"
    }
    async fn send(&self, message: &SendMessage) -> Result<()> {
        let mut chat_uuid = message
            .recipient
            .strip_prefix("bot_service:")
            .unwrap_or(&message.recipient)
            .to_string();
        chat_uuid = chat_uuid.trim().to_string();
        if chat_uuid.is_empty() {
            return Err(anyhow!("bot_service: missing chatUuid in recipient"));
        }
        let message_id = self
            .last_inbound_id
            .lock()
            .await
            .get(&chat_uuid)
            .cloned()
            .unwrap_or_default();
        let outbound = OutboundMessage {
            bo: OutboundBo {
                chat_uuid: chat_uuid.clone(),
                result: message.content.clone(),
                message_id,
                msg_type: Some("chat".to_string()),
            },
            code: OutboundCode {
                code: "0000".to_string(),
                msg: "Success".to_string(),
                msg_id: "RetCode.Success".to_string(),
            },
        };
        let payload = serde_json::to_string(&outbound)?;
        let mut guard = self.conn_write.lock().await;
        let writer = match guard.as_mut() {
            Some(w) => w,
            None => return Err(anyhow!("bot_service websocket not connected")),
        };
        writer.send(WsMessage::Text(payload.into())).await?;
        Ok(())
    }
    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> Result<()> {
        self.closed.store(false, Ordering::SeqCst);
        self.connect().await?;
        let read_self = self.clone();
        tokio::spawn(async move { read_self.read_loop(tx).await });
        let hb_self = self.clone();
        tokio::spawn(async move { hb_self.heartbeat_loop().await });
        loop {
            if self.closed.load(Ordering::SeqCst) {
                break;
            }
            sleep(Duration::from_secs(3600)).await;
        }
        Ok(())
    }
    async fn health_check(&self) -> bool {
        self.conn_write.lock().await.is_some()
    }
}
